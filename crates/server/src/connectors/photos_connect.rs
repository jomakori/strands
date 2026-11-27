//! Google Photos OAuth connect: `GET .../google_photos/connect` and `.../callback` (ticket B2).
//!
//! The flow is the login flow's shape with one difference: the grant belongs to
//! a signed-in account, so every row it touches is scoped to that account. The
//! `state` row written by [`crate::oauth::begin_connect`] carries the user, the
//! connector kind and the PKCE verifier; the callback refuses a state issued for
//! anyone else, and the token is sealed under the same account's key.
//!
//! Endpoints are a field, not a constant, so tests point the token exchange at a
//! stub and never need a Google client.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use chrono::{Duration, Utc};
use reqwest::Client;
use serde::Deserialize;
use sqlx::PgPool;
use strands_core::UserId;
use uuid::Uuid;

use super::photos::SCOPES;
use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::auth::token::{percent_encode, pkce_challenge, random_token};
use crate::error::AppError;
use crate::oauth::{self, ConnectorTokens};
use crate::photos::CONNECTOR_KIND;
use crate::secrets;

/// Google's OAuth client id for this deploy. Absent means no connect route.
pub const CLIENT_ID_ENV: &str = "GOOGLE_CLIENT_ID";
/// Google's OAuth client secret, sent to the token endpoint beside the verifier.
pub const CLIENT_SECRET_ENV: &str = "GOOGLE_CLIENT_SECRET";
/// Overrides the redirect URI derived from the public base URL.
pub const REDIRECT_URI_ENV: &str = "GOOGLE_OAUTH_REDIRECT_URI";

/// Where Settings lives, so the callback lands somewhere that shows the result.
const SETTINGS_PATH: &str = "/settings";

/// The two URLs the flow talks to. Google's by default, a stub in tests.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub authorization_url: String,
    pub token_url: String,
}

impl Endpoints {
    pub fn google() -> Self {
        Self {
            authorization_url: "https://accounts.google.com/o/oauth2/v2/auth".to_owned(),
            token_url: "https://oauth2.googleapis.com/token".to_owned(),
        }
    }
}

/// One deploy's Google OAuth client, plus where the browser comes back to.
pub struct PhotosOAuth {
    client_id: String,
    client_secret: String,
    redirect_uri: String,
    endpoints: Endpoints,
    http: Client,
}

impl PhotosOAuth {
    pub fn new(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        redirect_uri: impl Into<String>,
        endpoints: Endpoints,
    ) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            redirect_uri: redirect_uri.into(),
            endpoints,
            http: Client::new(),
        }
    }

    /// `None` when this deploy has no Google client, which is the state every
    /// deploy was in before B2: the connect route then says so instead of
    /// redirecting to a client id that does not exist.
    pub fn from_env(public_base_url: &str) -> Option<Self> {
        let client_id = env_value(CLIENT_ID_ENV)?;
        let client_secret = env_value(CLIENT_SECRET_ENV)?;
        let redirect_uri = env_value(REDIRECT_URI_ENV).unwrap_or_else(|| {
            format!(
                "{}/api/v1/connectors/google_photos/callback",
                public_base_url.trim_end_matches('/')
            )
        });
        Some(Self::new(
            client_id,
            client_secret,
            redirect_uri,
            Endpoints::google(),
        ))
    }

    /// Use a different set of URLs. Tests point this at a stub server.
    pub fn with_endpoints(mut self, endpoints: Endpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    /// The consent URL: state, S256 challenge, and the Picker scope only.
    fn authorization_url(&self, state: &str, challenge: &str) -> String {
        let scope = SCOPES.join(" ");
        format!(
            "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}\
             &code_challenge={}&code_challenge_method=S256&access_type=offline&prompt=consent",
            self.endpoints.authorization_url,
            percent_encode(&self.client_id),
            percent_encode(&self.redirect_uri),
            percent_encode(&scope),
            percent_encode(state),
            percent_encode(challenge),
        )
    }

    /// Redeem the code with the verifier the `state` row kept.
    async fn exchange(&self, code: &str, verifier: &str) -> Result<ConnectorTokens, ConnectError> {
        let response = self
            .http
            .post(&self.endpoints.token_url)
            .form(&[
                ("code", code),
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                // Must equal the value sent on the consent redirect, so it comes
                // from the same place that built that URL.
                ("redirect_uri", self.redirect_uri.as_str()),
                ("grant_type", "authorization_code"),
                ("code_verifier", verifier),
            ])
            .send()
            .await
            .map_err(|error| ConnectError::Transport(error.to_string()))?;

        if !response.status().is_success() {
            // Google answers 400 `invalid_grant` for a spent, expired or
            // mismatched code: starting over is the fix, not a retry.
            return Err(ConnectError::RefusedCode);
        }

        let grant: TokenResponse = response.json().await.map_err(|error| {
            ConnectError::Transport(format!("token response was not JSON: {error}"))
        })?;
        let access_token = grant.access_token.ok_or(ConnectError::NoToken)?;
        Ok(ConnectorTokens {
            access_token,
            refresh_token: grant.refresh_token,
            // Google reports a lifetime in seconds; an absent one means the
            // token has no expiry, which stays `None`.
            expires_at: grant
                .expires_in
                .map(|seconds| Utc::now() + Duration::seconds(seconds.max(0))),
        })
    }
}

/// Why a callback could not finish. Named variants because the test asserts on
/// the distinct answers: a spent state and a refused code are not the same.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConnectError {
    #[error("this connect link has already been used or has expired; start again from Settings")]
    SpentState,
    #[error("this connect link was issued for a different account")]
    WrongUser,
    #[error("this connect link was issued for a different connector")]
    WrongKind,
    #[error("Google Photos sent no authorization code")]
    MissingCode,
    #[error("Google Photos reported an error: {0}")]
    Denied(String),
    #[error("Google Photos refused the authorization code; start again from Settings")]
    RefusedCode,
    #[error("the token response carried no access token")]
    NoToken,
    #[error("the token endpoint could not be reached: {0}")]
    Transport(String),
}

impl From<ConnectError> for AppError {
    fn from(error: ConnectError) -> Self {
        match error {
            ConnectError::Transport(detail) => AppError::internal(
                "Google Photos token endpoint",
                sqlx::Error::Protocol(detail),
            ),
            // Everything else is a condition the person can act on by
            // connecting again, so the message reaches them intact.
            other => AppError::BadRequest(other.to_string()),
        }
    }
}

/// What Google appends to the redirect URI.
#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/connectors/google_photos/connect", get(connect))
        .route("/api/v1/connectors/google_photos/callback", get(callback))
}

/// `GET /api/v1/connectors/google_photos/connect` — start the grant.
pub async fn connect(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Response, AppError> {
    let oauth = configured(&state)?;
    // The verifier stays on the server: the state row carries it and the browser
    // only ever sees its SHA-256.
    let verifier = random_token();
    let challenge = pkce_challenge(&verifier);
    let state_token =
        oauth::begin_connect(&state.pool, user.id, CONNECTOR_KIND, &verifier, Utc::now()).await?;
    let url = oauth.authorization_url(&state_token, &challenge);
    // 302: the spec pins the code, and a browser navigation is what this is.
    let location = HeaderValue::from_str(&url).map_err(|error| {
        AppError::internal(
            "Google Photos consent URL",
            sqlx::Error::Protocol(error.to_string()),
        )
    })?;
    let mut response = StatusCode::FOUND.into_response();
    response.headers_mut().insert(header::LOCATION, location);
    Ok(response)
}

/// `GET /api/v1/connectors/google_photos/callback` — redeem and store.
pub async fn callback(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    let oauth = configured(&state)?;
    let key = state.secrets.as_ref().ok_or_else(|| {
        AppError::internal(
            "Google Photos connector",
            sqlx::Error::Protocol(format!(
                "{} is unset, so the token cannot be sealed",
                secrets::KEY_ENV
            )),
        )
    })?;

    let state_token = query.state.ok_or(ConnectError::SpentState)?;
    // Claim first: the row is single use, so a second delivery of the same state
    // (back button, refresh, a leaked URL) finds nothing and is refused.
    let pending = oauth::take_connect(&state.pool, &state_token)
        .await?
        .ok_or(ConnectError::SpentState)?;
    if pending.kind != CONNECTOR_KIND {
        return Err(ConnectError::WrongKind.into());
    }
    // The state was issued for one account; a session that does not match it
    // must not inherit the grant.
    if pending.user_id != user.id {
        return Err(ConnectError::WrongUser.into());
    }

    // Consent refused: Google sends `error=` instead of a code.
    if let Some(error) = query.error {
        return Err(ConnectError::Denied(error).into());
    }
    let code = query.code.ok_or(ConnectError::MissingCode)?;

    let tokens = oauth.exchange(&code, &pending.pkce_verifier).await?;
    let connector_id = connected_row(&state.pool, user.id).await?;
    oauth::store_tokens(&state.pool, key.as_ref(), connector_id, user.id, &tokens).await?;

    let settings = format!(
        "{}{SETTINGS_PATH}",
        state.auth.public_base_url.trim_end_matches('/')
    );
    Ok(Redirect::to(&settings).into_response())
}

/// The deploy's Google client, or the reason there is no connect route.
fn configured(state: &AppState) -> Result<&PhotosOAuth, AppError> {
    state.photos_oauth.as_deref().ok_or_else(|| {
        AppError::internal(
            "Google Photos connector",
            sqlx::Error::Protocol(format!(
                "{} and {} are unset, so there is no client to connect with",
                CLIENT_ID_ENV, CLIENT_SECRET_ENV
            )),
        )
    })
}

/// The caller's `connectors` row for this kind, marked connected.
///
/// Upserted on `(user_id, kind)` so a reconnect reuses the row the token hangs
/// off instead of colliding with the unique constraint. The scopes written here
/// are the ones this grant asked for, so the row states what the token allows.
async fn connected_row(pool: &PgPool, user_id: UserId) -> Result<Uuid, AppError> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO connectors (id, user_id, kind, status, scopes) \
         VALUES ($1, $2, $3, 'connected', $4) \
         ON CONFLICT (user_id, kind) DO UPDATE SET \
             status = 'connected', scopes = EXCLUDED.scopes, last_error = NULL \
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(CONNECTOR_KIND)
    .bind(SCOPES)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// What the token endpoint answers. `token_type` and `scope` are unread.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration as StdDuration;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use sqlx::PgPool;
    use sqlx::postgres::PgPoolOptions;
    use strands_core::ConnectorStatus;
    use strands_core::connector::Connector;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tower::ServiceExt;

    use super::*;
    use crate::app::router;
    use crate::auth::identity::BoxFuture;
    use crate::auth::{AuthError, ConsentRequest, Identity, IdentityProvider};
    use crate::config::AuthConfig;
    use crate::connectors::{PhotosConnector, build_registry};
    use crate::llm::LiteLlmClient;
    use crate::secrets::{CURRENT_KEY_VERSION, SecretKey};
    use crate::summary::LiteLlmSummarizer;

    const CONNECT: &str = "/api/v1/connectors/google_photos/connect";
    const CALLBACK: &str = "/api/v1/connectors/google_photos/callback";

    struct TestUser {
        id: UserId,
        cookie: String,
    }

    struct UnusedIdentity;

    impl IdentityProvider for UnusedIdentity {
        fn consent_url(&self, _request: &ConsentRequest<'_>) -> String {
            "https://identity.example/consent".to_owned()
        }

        fn exchange_code<'a>(
            &'a self,
            _code: &'a str,
            _code_verifier: &'a str,
            _redirect_uri: &'a str,
        ) -> BoxFuture<'a, Result<Identity, AuthError>> {
            Box::pin(async { Err(AuthError::InvalidCode) })
        }
    }

    /// Same skip-or-fail contract as `tests/common`: a laptop without Postgres
    /// skips loudly, CI (`STRANDS_TEST_REQUIRE_DB=1`) fails instead of skipping.
    async fn pool_or_skip(test_name: &str) -> Option<PgPool> {
        let url = std::env::var("DATABASE_URL")
            .ok()
            .filter(|url| !url.trim().is_empty());
        let Some(url) = url else {
            eprintln!(
                "skipping {test_name}: DATABASE_URL is not set (no live Postgres on this host)"
            );
            return None;
        };

        let required = std::env::var("STRANDS_TEST_REQUIRE_DB").is_ok_and(|value| value == "1");

        let pool = match PgPoolOptions::new()
            .acquire_timeout(StdDuration::from_secs(3))
            .connect(&url)
            .await
        {
            Ok(pool) => pool,
            Err(error) if required => {
                panic!("STRANDS_TEST_REQUIRE_DB is set but Postgres is unreachable: {error}")
            }
            Err(error) => {
                eprintln!("skipping {test_name}: cannot reach DATABASE_URL ({error})");
                return None;
            }
        };

        crate::db::MIGRATOR
            .run(&pool)
            .await
            .expect("apply migrations");
        Some(pool)
    }

    fn key() -> Arc<SecretKey> {
        Arc::new(SecretKey::from_bytes(&[7u8; 32]).expect("a 32-byte key"))
    }

    async fn account(pool: &PgPool) -> TestUser {
        let id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, google_sub, email, name) VALUES ($1, $2, $3, $4)")
            .bind(id)
            .bind(format!("connect-sub-{id}"))
            .bind(format!("connect-{id}@example.test"))
            .bind("Connect Test")
            .execute(pool)
            .await
            .expect("insert user");
        sqlx::query("INSERT INTO sessions (id, user_id, expires_at) VALUES ($1, $2, $3)")
            .bind(session_id)
            .bind(id)
            .bind(Utc::now() + Duration::hours(1))
            .execute(pool)
            .await
            .expect("insert session");
        TestUser {
            id,
            cookie: format!("strands_session={session_id}"),
        }
    }

    /// Both URLs pointed at `origin`, which is what makes the flow testable.
    fn endpoints(origin: &str) -> Endpoints {
        Endpoints {
            authorization_url: format!("{origin}/authorize"),
            token_url: format!("{origin}/token"),
        }
    }

    fn oauth(origin: &str) -> PhotosOAuth {
        PhotosOAuth::new(
            "stub-client-id",
            "stub-client-secret",
            format!("https://test.example{CALLBACK}"),
            endpoints(origin),
        )
    }

    fn app(pool: &PgPool, key: Arc<SecretKey>, oauth: PhotosOAuth) -> Router {
        let state = AppState::new(
            pool.clone(),
            AuthConfig {
                client_id: "unused-client-id".into(),
                issuer: "https://identity.example".into(),
                public_base_url: "https://test.example".into(),
                cookie_secure: true,
                session_ttl_days: 30,
            },
            Arc::new(UnusedIdentity),
            Arc::new(build_registry(
                pool.clone(),
                Some(Arc::clone(&key)),
                None,
                false,
            )),
            Arc::new(LiteLlmSummarizer::new(LiteLlmClient::new(
                "http://unused.invalid",
                "unused",
            ))),
        )
        .with_secrets(key)
        .with_photos_oauth(Arc::new(oauth));
        router(state)
    }

    fn request(uri: &str, user: &TestUser) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .header(axum::http::header::COOKIE, &user.cookie)
            .body(Body::empty())
            .expect("build request")
    }

    async fn get(app: &Router, uri: &str, user: &TestUser) -> axum::http::Response<Body> {
        app.clone()
            .oneshot(request(uri, user))
            .await
            .expect("router response")
    }

    async fn body_text(response: axum::http::Response<Body>) -> String {
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("read body")
            .to_bytes();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// One query parameter out of the consent URL, still percent-encoded.
    fn param(url: &str, name: &str) -> Option<String> {
        let query = url.split_once('?')?.1;
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
            .map(str::to_owned)
    }

    /// Start a grant and hand back the `state` the browser would carry.
    async fn started_state(app: &Router, pool: &PgPool, user: &TestUser) -> String {
        let response = get(app, CONNECT, user).await;
        assert_eq!(
            response.status(),
            StatusCode::FOUND,
            "connect must redirect"
        );
        let location = response
            .headers()
            .get(header::LOCATION)
            .expect("a redirect has a location")
            .to_str()
            .expect("location is ASCII");
        let state = param(location, "state").expect("the redirect carries a state");
        let stored: i64 =
            sqlx::query_scalar("SELECT count(*) FROM connector_oauth_states WHERE state = $1")
                .bind(&state)
                .fetch_one(pool)
                .await
                .expect("count state rows");
        assert_eq!(stored, 1, "connect writes exactly one state row");
        state
    }

    /// A stub token endpoint that answers once and reports the form it received.
    async fn token_stub(body: &'static str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
        let origin = format!(
            "http://{}",
            listener.local_addr().expect("stub local address")
        );
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept one call");
            let request = read_request(&mut socket).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write response");
            request
        });
        (origin, handle)
    }

    /// Read one HTTP request: headers, then `content-length` bytes of body.
    async fn read_request(socket: &mut TcpStream) -> String {
        let mut buffer = Vec::new();
        loop {
            let mut chunk = [0u8; 1024];
            let read = socket.read(&mut chunk).await.expect("read request");
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            let text = String::from_utf8_lossy(&buffer).into_owned();
            let Some((headers, body)) = text.split_once("\r\n\r\n") else {
                continue;
            };
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if body.len() >= length {
                break;
            }
        }
        String::from_utf8_lossy(&buffer).into_owned()
    }

    /// B2 acceptance: connect answers 302 to the consent page with a state the
    /// server remembers, a PKCE challenge it can verify, and no other scope.
    #[tokio::test]
    async fn connect_redirects_with_state_and_pkce() {
        let Some(pool) = pool_or_skip(
            "connectors::photos_connect::tests::connect_redirects_with_state_and_pkce",
        )
        .await
        else {
            return;
        };
        let user = account(&pool).await;
        let app = app(&pool, key(), oauth("https://stub.example"));

        let response = get(&app, CONNECT, &user).await;
        assert_eq!(
            response.status(),
            StatusCode::FOUND,
            "the spec asks for a plain 302 to Google"
        );
        let location = response
            .headers()
            .get(header::LOCATION)
            .expect("a redirect has a location")
            .to_str()
            .expect("location is ASCII")
            .to_owned();
        assert!(
            location.starts_with("https://stub.example/authorize?"),
            "the endpoint override is what the redirect uses: {location}"
        );
        assert_eq!(param(&location, "response_type").as_deref(), Some("code"));
        assert_eq!(
            param(&location, "code_challenge_method").as_deref(),
            Some("S256"),
            "PKCE must be S256: {location}"
        );
        assert_eq!(
            param(&location, "redirect_uri").as_deref(),
            Some("https%3A%2F%2Ftest.example%2Fapi%2Fv1%2Fconnectors%2Fgoogle_photos%2Fcallback")
        );

        let state = param(&location, "state").expect("the redirect carries a state");
        let challenge =
            param(&location, "code_challenge").expect("the redirect carries a challenge");
        let (owner, kind, verifier): (UserId, String, String) = sqlx::query_as(
            "SELECT user_id, kind, pkce_verifier FROM connector_oauth_states WHERE state = $1",
        )
        .bind(&state)
        .fetch_one(&pool)
        .await
        .expect("connect wrote the state row");
        assert_eq!(owner, user.id, "the state belongs to the signed-in account");
        assert_eq!(kind, CONNECTOR_KIND);
        assert_eq!(
            pkce_challenge(&verifier),
            challenge,
            "the challenge is SHA-256 over the verifier the server kept"
        );
        assert!(
            !location.contains(&verifier),
            "the verifier must never reach the browser: {location}"
        );

        // Picker only: the same scope string `photos.rs` registers, nothing else.
        let scope = param(&location, "scope").expect("the redirect carries a scope");
        assert_eq!(scope, percent_encode(&SCOPES.join(" ")));
        assert!(
            !location.contains("photoslibrary"),
            "no library-wide scope may be requested: {location}"
        );
        assert_eq!(
            location.matches("photospicker.mediaitems.readonly").count(),
            1,
            "one picker scope: {location}"
        );
    }

    /// B2 acceptance: the callback redeems the code, seals the token against the
    /// caller, and leaves the connector reporting Connected.
    #[tokio::test]
    async fn callback_writes_token() {
        let Some(pool) =
            pool_or_skip("connectors::photos_connect::tests::callback_writes_token").await
        else {
            return;
        };
        let user = account(&pool).await;
        let key = key();
        let (origin, recorded) = token_stub(
            r#"{"access_token":"ya29.stub-access","refresh_token":"1//stub-refresh","expires_in":3599,"token_type":"Bearer"}"#,
        )
        .await;
        let app = app(&pool, Arc::clone(&key), oauth(&origin));

        let state = started_state(&app, &pool, &user).await;
        let verifier: String =
            sqlx::query_scalar("SELECT pkce_verifier FROM connector_oauth_states WHERE state = $1")
                .bind(&state)
                .fetch_one(&pool)
                .await
                .expect("the state row still carries the verifier");

        let response = get(
            &app,
            &format!("{CALLBACK}?state={state}&code=stub-code"),
            &user,
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::SEE_OTHER,
            "the callback hands the browser back to Settings"
        );
        let location = response
            .headers()
            .get(header::LOCATION)
            .expect("a redirect has a location")
            .to_str()
            .expect("location is ASCII")
            .to_owned();
        assert_eq!(location, "https://test.example/settings");

        // The exchange itself: the code, the verifier it was issued with, and a
        // redirect URI identical to the one the consent URL carried.
        let form = recorded.await.expect("the stub token endpoint was called");
        assert!(
            form.contains("code=stub-code"),
            "the code is redeemed: {form}"
        );
        assert!(
            form.contains("grant_type=authorization_code"),
            "an authorization code grant: {form}"
        );
        assert!(
            form.contains(&format!("code_verifier={verifier}")),
            "the verifier from the state row is sent: {form}"
        );
        assert!(
            form.contains("client_secret=stub-client-secret"),
            "the client secret is used: {form}"
        );

        // The token row: the caller's, sealed at the current key version, not
        // readable in place.
        let (connector_id, ciphertext, version): (Uuid, Vec<u8>, i16) = sqlx::query_as(
            "SELECT c.id, s.ciphertext, s.key_version FROM connectors c \
             JOIN connector_secrets s ON s.connector_id = c.id \
             WHERE c.user_id = $1 AND c.kind = $2",
        )
        .bind(user.id)
        .bind(CONNECTOR_KIND)
        .fetch_one(&pool)
        .await
        .expect("the callback wrote a sealed token row");
        assert_eq!(version, CURRENT_KEY_VERSION);
        assert!(
            !ciphertext
                .windows(b"ya29.stub-access".len())
                .any(|window| window == b"ya29.stub-access"),
            "the access token must not be stored in the clear"
        );

        let stored = crate::oauth::load_tokens(&pool, &key, connector_id, user.id)
            .await
            .expect("load the stored token")
            .expect("the row exists");
        assert_eq!(stored.access_token, "ya29.stub-access");
        assert_eq!(stored.refresh_token.as_deref(), Some("1//stub-refresh"));
        assert!(
            stored.expires_at.is_some(),
            "Google reported a lifetime, so the row carries an expiry"
        );

        // Settings reads this: the row says connected, and the connector's own
        // status agrees because the sealed token opens.
        let status: String = sqlx::query_scalar(
            "SELECT status::text FROM connectors WHERE user_id = $1 AND kind = $2",
        )
        .bind(user.id)
        .bind(CONNECTOR_KIND)
        .fetch_one(&pool)
        .await
        .expect("the connector row exists");
        assert_eq!(status, "connected");
        let reported = PhotosConnector::new(pool.clone(), Some(Arc::clone(&key)))
            .status(user.id)
            .await
            .expect("read the connector status");
        assert_eq!(
            reported,
            ConnectorStatus::Connected,
            "Settings shows Connected once the token opens (got {reported:?})"
        );
    }

    /// B2 acceptance: a `state` that was already spent is refused, and the token
    /// it produced is left alone.
    #[tokio::test]
    async fn replayed_state_is_refused() {
        let Some(pool) =
            pool_or_skip("connectors::photos_connect::tests::replayed_state_is_refused").await
        else {
            return;
        };
        let user = account(&pool).await;
        let key = key();
        let (origin, recorded) = token_stub(
            r#"{"access_token":"ya29.first-access","refresh_token":"1//first-refresh","expires_in":3599}"#,
        )
        .await;
        let app = app(&pool, Arc::clone(&key), oauth(&origin));

        let state = started_state(&app, &pool, &user).await;
        let callback = format!("{CALLBACK}?state={state}&code=first-code");
        let first = get(&app, &callback, &user).await;
        assert_eq!(first.status(), StatusCode::SEE_OTHER);
        recorded.await.expect("the first exchange reached the stub");

        let replay = get(&app, &callback, &user).await;
        assert_eq!(
            replay.status(),
            StatusCode::BAD_REQUEST,
            "a spent state is a 400, not a second connection"
        );
        let text = body_text(replay).await;
        assert!(
            text.contains("already been used"),
            "the refusal names the condition: {text}"
        );

        // Nothing changed: one connector row, one token row, the first token.
        let connectors: i64 =
            sqlx::query_scalar("SELECT count(*) FROM connectors WHERE user_id = $1")
                .bind(user.id)
                .fetch_one(&pool)
                .await
                .expect("count connector rows");
        assert_eq!(connectors, 1);
        let secrets: i64 =
            sqlx::query_scalar("SELECT count(*) FROM connector_secrets WHERE user_id = $1")
                .bind(user.id)
                .fetch_one(&pool)
                .await
                .expect("count token rows");
        assert_eq!(secrets, 1);
        let connector_id: Uuid =
            sqlx::query_scalar("SELECT id FROM connectors WHERE user_id = $1 AND kind = $2")
                .bind(user.id)
                .bind(CONNECTOR_KIND)
                .fetch_one(&pool)
                .await
                .expect("the connector row exists");
        let stored = crate::oauth::load_tokens(&pool, &key, connector_id, user.id)
            .await
            .expect("load the stored token")
            .expect("the row exists");
        assert_eq!(stored.access_token, "ya29.first-access");
    }

    /// A state issued for one account cannot be redeemed by another session.
    #[tokio::test]
    async fn callback_refuses_another_users_state() {
        let Some(pool) =
            pool_or_skip("connectors::photos_connect::tests::callback_refuses_another_users_state")
                .await
        else {
            return;
        };
        let owner = account(&pool).await;
        let stranger = account(&pool).await;
        let app = app(&pool, key(), oauth("https://stub.example"));

        let state = started_state(&app, &pool, &owner).await;
        let response = get(
            &app,
            &format!("{CALLBACK}?state={state}&code=someone-elses-code"),
            &stranger,
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "a state minted for another account must not connect this one"
        );
        let text = body_text(response).await;
        assert!(
            text.contains("different account"),
            "the refusal says why: {text}"
        );

        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM connectors WHERE user_id = $1")
            .bind(stranger.id)
            .fetch_one(&pool)
            .await
            .expect("count the stranger's connectors");
        assert_eq!(rows, 0, "the stranger got no connector and no token");
    }
}
