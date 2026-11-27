//! Refreshing the Google Photos access token (ticket B3).
//!
//! Google issues an access token that lives about an hour and a refresh token
//! that outlives it. Every Photos request reads its token through
//! [`access_token`], so the rules B3 asks for live in one place:
//!
//! * a token that has expired is refreshed once and the answer is re-sealed
//!   before it is used, so the request that needed it goes through and the next
//!   request finds a live token;
//! * a grant Google no longer honours is not retried. The connector is marked
//!   `error` with a message that says what to do, and the caller gets a `400`
//!   saying the same thing — a `503` would read as "try again later", which is
//!   never true for a revoked grant;
//! * a connector already in `error` is answered from the row alone: no token is
//!   opened and no refresh is attempted, because the grant behind it is the one
//!   Google refused. Only a reconnect replaces that grant, so retrying here
//!   would spend a request on Google for every pick and still answer `400`.
//!
//! Both the read and the write are keyed on the owning `user_id`: the token is
//! sealed under that account's key, and a query that did not carry the id could
//! hand one account's token to another.

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::photos_connect::{CLIENT_ID_ENV, RefreshError};
use crate::app::AppState;
use crate::error::AppError;
use crate::oauth::{self, ConnectorTokens};
use crate::photos::CONNECTOR_KIND;
use crate::secrets::{self, SecretError, SecretKey};

/// Refresh a little before Google says the token dies, so a token that expires
/// while a request is in flight is not used anyway.
const EXPIRY_SKEW: Duration = Duration::seconds(60);

/// What the person is told when only they can fix the connection.
pub const RECONNECT_REQUIRED: &str =
    "the Google Photos connection has expired; reconnect it from Settings";

/// The caller's stored access token, refreshed if it has expired.
///
/// Fails when there is no connection to use: `400` for a connector the person
/// has to fix, `500` for a server-side gap.
pub async fn access_token(state: &AppState, user_id: Uuid) -> Result<String, AppError> {
    let key = state.secrets.as_ref().ok_or_else(|| {
        AppError::internal(
            "photos token refresh",
            sqlx::Error::Protocol(format!(
                "{} is unset, so connector tokens cannot be opened",
                secrets::KEY_ENV
            )),
        )
    })?;

    refresh_if_expired(
        &state.pool,
        key,
        state.photos_oauth.as_deref(),
        user_id,
        CONNECTOR_KIND,
    )
    .await
}

/// The refresh path itself, with the OAuth client passed in so the token
/// endpoint can be a stub in tests and Google in production.
async fn refresh_if_expired(
    pool: &PgPool,
    key: &SecretKey,
    oauth: Option<&super::photos_connect::PhotosOAuth>,
    user_id: Uuid,
    kind: &str,
) -> Result<String, AppError> {
    let Some(row) = stored(pool, user_id, kind).await? else {
        return Err(not_connected());
    };

    // A row already reported as broken is not retried: a refresh token Google
    // refused does not come back by asking again, so the answer is the same
    // `400` the refresh that broke it would have produced, with no token read
    // and no call to Google. A reconnect is what clears this state.
    match row.status.as_str() {
        "connected" => {}
        "error" => return Err(AppError::BadRequest(RECONNECT_REQUIRED.to_owned())),
        _ => return Err(not_connected()),
    }

    let access_token = open_text(key, user_id, row.key_version, &row.ciphertext)?;

    if !expired(row.expires_at) {
        return Ok(access_token);
    }

    let oauth = oauth.ok_or_else(|| {
        AppError::internal(
            "photos token refresh",
            sqlx::Error::Protocol(format!(
                "{CLIENT_ID_ENV} is unset, so an expired token cannot be refreshed"
            )),
        )
    })?;

    let Some(sealed) = row.refresh_token_encrypted.as_deref() else {
        // A grant made before offline access was asked for carries no refresh
        // token, so an expired access token ends the connection.
        mark_error(pool, user_id, row.connector_id, RECONNECT_REQUIRED).await?;
        return Err(AppError::BadRequest(RECONNECT_REQUIRED.to_owned()));
    };
    let refresh_token = open_text(key, user_id, row.key_version, sealed)?;

    match oauth.refresh(&refresh_token).await {
        Ok(tokens) => {
            // Google rotates the refresh token only sometimes, so an answer
            // without one means "keep the one you have" — dropping it here
            // would strand the account on the next expiry.
            let kept = tokens.refresh_token.unwrap_or(refresh_token);
            let stored = ConnectorTokens {
                access_token: tokens.access_token,
                refresh_token: Some(kept),
                expires_at: tokens.expires_at,
            };
            oauth::store_tokens(pool, key, row.connector_id, user_id, &stored).await?;
            // The row was `connected` to get this far and stays that way: the
            // `error` gate above is the only thing that reads a broken row, so
            // there is no earlier failure here to clear.
            Ok(stored.access_token)
        }
        Err(RefreshError::Revoked) => {
            mark_error(pool, user_id, row.connector_id, RECONNECT_REQUIRED).await?;
            Err(AppError::BadRequest(RECONNECT_REQUIRED.to_owned()))
        }
        // A network failure says nothing about the grant, so the row is left
        // alone and the next request tries again.
        Err(error) => Err(AppError::from(error)),
    }
}

/// One connector's sealed token material, for the owning account only.
#[derive(sqlx::FromRow)]
struct StoredRow {
    connector_id: Uuid,
    ciphertext: Vec<u8>,
    key_version: i16,
    refresh_token_encrypted: Option<Vec<u8>>,
    expires_at: Option<DateTime<Utc>>,
    status: String,
}

async fn stored(pool: &PgPool, user_id: Uuid, kind: &str) -> Result<Option<StoredRow>, AppError> {
    Ok(sqlx::query_as(
        "SELECT s.connector_id, s.ciphertext, s.key_version, s.refresh_token_encrypted, \
                s.expires_at, c.status::text AS status \
           FROM connectors c \
           JOIN connector_secrets s \
             ON s.connector_id = c.id AND s.user_id = c.user_id \
          WHERE c.user_id = $1 AND c.kind = $2",
    )
    .bind(user_id)
    .bind(kind)
    .fetch_optional(pool)
    .await?)
}

/// True when the token must be replaced before it is used again. `None` means
/// the grant did not say, which is the legacy shape and is taken at its word.
fn expired(expires_at: Option<DateTime<Utc>>) -> bool {
    expires_at.is_some_and(|at| at <= Utc::now() + EXPIRY_SKEW)
}

async fn mark_error(
    pool: &PgPool,
    user_id: Uuid,
    connector_id: Uuid,
    message: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE connectors SET status = 'error', last_error = $3 \
          WHERE id = $1 AND user_id = $2",
    )
    .bind(connector_id)
    .bind(user_id)
    .bind(message)
    .execute(pool)
    .await?;
    Ok(())
}

fn not_connected() -> AppError {
    AppError::BadRequest(
        "the Google Photos connector is not connected; connect it first".to_owned(),
    )
}

fn open_text(
    key: &SecretKey,
    user_id: Uuid,
    key_version: i16,
    blob: &[u8],
) -> Result<String, AppError> {
    let plaintext = key
        .open(user_id, key_version, blob)
        .map_err(|error| AppError::internal("connector secret", protocol(error)))?;
    String::from_utf8(plaintext)
        .map_err(|_| AppError::internal("connector secret", protocol(SecretError::Truncated)))
}

fn protocol(error: SecretError) -> sqlx::Error {
    sqlx::Error::Protocol(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration as StdDuration;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use http_body_util::BodyExt;
    use sqlx::postgres::PgPoolOptions;
    use strands_connector_photos::{
        PHOTOS_SCOPE, PhotosError, PickedItem, PickerApi, PickerFuture, PickerSession,
        SessionStatus, Thumbnail,
    };
    use strands_core::UserId;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tower::ServiceExt;

    use super::*;
    use crate::app::router;
    use crate::auth::identity::BoxFuture;
    use crate::auth::{AuthError, ConsentRequest, Identity, IdentityProvider};
    use crate::config::AuthConfig;
    use crate::connectors::build_registry;
    use crate::connectors::photos_connect::{Endpoints, PhotosOAuth};
    use crate::llm::LiteLlmClient;
    use crate::secrets::CURRENT_KEY_VERSION;
    use crate::summary::LiteLlmSummarizer;

    const SESSIONS: &str = "/api/v1/connectors/photos/sessions";

    struct TestUser {
        id: UserId,
        cookie: String,
    }

    /// A pick that records the token it was handed and nothing else.
    struct RecordingPicker {
        tokens: Mutex<Vec<String>>,
    }

    impl RecordingPicker {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                tokens: Mutex::new(Vec::new()),
            })
        }

        fn tokens(&self) -> Vec<String> {
            self.tokens.lock().expect("picker log").clone()
        }
    }

    impl PickerApi for RecordingPicker {
        fn create_session<'a>(
            &'a self,
            access_token: &'a str,
        ) -> PickerFuture<'a, Result<PickerSession, PhotosError>> {
            Box::pin(async move {
                self.tokens
                    .lock()
                    .expect("picker log")
                    .push(access_token.to_owned());
                Ok(PickerSession {
                    id: "stub-session".to_owned(),
                    picker_uri: "https://photos.example/pick".to_owned(),
                    expires_at: Utc::now() + Duration::minutes(15),
                })
            })
        }

        fn session_status<'a>(
            &'a self,
            _access_token: &'a str,
            _session_id: &'a str,
        ) -> PickerFuture<'a, Result<SessionStatus, PhotosError>> {
            Box::pin(async { Err(PhotosError::Refused("not used".to_owned())) })
        }

        fn list_items<'a>(
            &'a self,
            _access_token: &'a str,
            _session_id: &'a str,
        ) -> PickerFuture<'a, Result<Vec<PickedItem>, PhotosError>> {
            Box::pin(async { Err(PhotosError::Refused("not used".to_owned())) })
        }

        fn thumbnail<'a>(
            &'a self,
            _base_url: &'a str,
        ) -> PickerFuture<'a, Result<Thumbnail, PhotosError>> {
            Box::pin(async { Err(PhotosError::Refused("not used".to_owned())) })
        }
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
            .bind(format!("refresh-sub-{id}"))
            .bind(format!("refresh-{id}@example.test"))
            .bind("Refresh Test")
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

    /// Both URLs pointed at `origin`, which is what makes the exchange testable.
    fn oauth(origin: &str) -> PhotosOAuth {
        PhotosOAuth::new(
            "stub-client-id",
            "stub-client-secret",
            "https://test.example/api/v1/connectors/google_photos/callback",
            Endpoints {
                authorization_url: format!("{origin}/authorize"),
                token_url: format!("{origin}/token"),
            },
        )
    }

    fn app(
        pool: &PgPool,
        key: Arc<SecretKey>,
        oauth: PhotosOAuth,
        picker: Arc<dyn PickerApi>,
    ) -> Router {
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
        .with_photos_oauth(Arc::new(oauth))
        .with_photos(picker);
        router(state)
    }

    async fn post(app: &Router, uri: &str, user: &TestUser) -> axum::http::Response<Body> {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::COOKIE, &user.cookie)
            .body(Body::empty())
            .expect("build request");
        app.clone().oneshot(request).await.expect("router response")
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

    /// A connected Photos row whose access token lapsed an hour ago — the shape
    /// B2 leaves behind once time passes.
    async fn stale_connection(pool: &PgPool, key: &SecretKey, user: UserId) -> Uuid {
        let connector_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO connectors (id, user_id, kind, status, scopes) \
             VALUES ($1, $2, $3, 'connected', $4)",
        )
        .bind(connector_id)
        .bind(user)
        .bind(CONNECTOR_KIND)
        .bind(vec![PHOTOS_SCOPE.to_owned()])
        .execute(pool)
        .await
        .expect("insert connector");

        let access = key.seal(user, b"ya29.stale-access").expect("seal access");
        let refresh = key.seal(user, b"stub-refresh").expect("seal refresh");
        sqlx::query(
            "INSERT INTO connector_secrets \
                 (connector_id, user_id, ciphertext, refresh_token_encrypted, expires_at, key_version) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(connector_id)
        .bind(user)
        .bind(access)
        .bind(refresh)
        .bind(Utc::now() - Duration::hours(1))
        .bind(CURRENT_KEY_VERSION)
        .execute(pool)
        .await
        .expect("insert secret");
        connector_id
    }

    /// The message an earlier refusal left on the row, quoted back by the test to
    /// prove a retry does not rewrite it.
    const EARLIER_FAILURE: &str = "the Google Photos grant was revoked; reconnect it from Settings";

    /// A connector the refresh path already gave up on: the row B3 leaves behind
    /// after Google refused the grant. The token row is still the stale one and
    /// still carries a refresh token, so a retry would find everything it needs.
    async fn broken_connection(pool: &PgPool, key: &SecretKey, user: UserId) -> Uuid {
        let connector_id = stale_connection(pool, key, user).await;
        sqlx::query("UPDATE connectors SET status = 'error', last_error = $2 WHERE id = $1")
            .bind(connector_id)
            .bind(EARLIER_FAILURE)
            .execute(pool)
            .await
            .expect("mark the connector error");
        connector_id
    }

    /// A stub token endpoint that answers once and reports the form it received.
    async fn token_stub(
        status: &'static str,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
        let origin = format!(
            "http://{}",
            listener.local_addr().expect("stub local address")
        );
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept one call");
            let request = read_request(&mut socket).await;
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
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

    /// B3 acceptance, first half: an expired token triggers one refresh, the
    /// answer is re-sealed, and the request that needed it goes through.
    #[tokio::test]
    async fn expired_token_refreshes_once() {
        let Some(pool) =
            pool_or_skip("connectors::photos_refresh::tests::expired_token_refreshes_once").await
        else {
            return;
        };
        let user = account(&pool).await;
        let key = key();
        let (origin, captured) = token_stub(
            "200 OK",
            r#"{"access_token":"ya29.refreshed","expires_in":3600,"token_type":"Bearer"}"#,
        )
        .await;
        let picker = RecordingPicker::new();
        let app = app(&pool, Arc::clone(&key), oauth(&origin), picker.clone());
        let connector_id = stale_connection(&pool, &key, user.id).await;

        let response = post(&app, SESSIONS, &user).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the pick that needed the token must succeed: {}",
            body_text(response).await
        );

        let request = captured.await.expect("the stub answered its one call");
        assert!(
            request.contains("grant_type=refresh_token"),
            "the token endpoint must be asked for a refresh: {request}"
        );
        assert!(
            request.contains("refresh_token=stub-refresh"),
            "the stored refresh token is what gets redeemed: {request}"
        );

        // The rewrite is on disk and sealed: the row opens to the fresh token
        // and its expiry moved forward.
        let stored = oauth::load_tokens(&pool, &key, connector_id, user.id)
            .await
            .expect("read tokens")
            .expect("the row still exists");
        assert_eq!(stored.access_token, "ya29.refreshed");
        assert_eq!(
            stored.refresh_token.as_deref(),
            Some("stub-refresh"),
            "a response without a refresh token keeps the stored one"
        );
        assert!(stored.expires_at.expect("a fresh expiry") > Utc::now() + Duration::minutes(50));

        // One refresh, not one per call: the stub accepted a single connection,
        // so a second attempt would have failed rather than answer with the live
        // token already stored.
        let again = post(&app, SESSIONS, &user).await;
        assert_eq!(
            again.status(),
            StatusCode::OK,
            "the second pick reuses the refreshed token: {}",
            body_text(again).await
        );
        assert_eq!(
            picker.tokens(),
            vec!["ya29.refreshed".to_owned(); 2],
            "both picks used the refreshed token"
        );
    }

    /// B3 acceptance, second half: a grant Google no longer honours is not a
    /// `503`. It is a `400` that asks for a reconnect, and the connector is
    /// marked `error` carrying the same message.
    #[tokio::test]
    async fn revoked_refresh_asks_to_reconnect() {
        let Some(pool) =
            pool_or_skip("connectors::photos_refresh::tests::revoked_refresh_asks_to_reconnect")
                .await
        else {
            return;
        };
        let user = account(&pool).await;
        let key = key();
        let (origin, _captured) = token_stub(
            "400 Bad Request",
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
        )
        .await;
        let picker = RecordingPicker::new();
        let app = app(&pool, Arc::clone(&key), oauth(&origin), picker.clone());
        let connector_id = stale_connection(&pool, &key, user.id).await;

        let response = post(&app, SESSIONS, &user).await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "a revoked grant is the person's to fix, not a server error"
        );
        let body = body_text(response).await;
        assert!(
            body.to_lowercase().contains("reconnect"),
            "the answer must say what to do: {body}"
        );
        assert!(
            picker.tokens().is_empty(),
            "no pick may start on a token that could not be refreshed"
        );

        let (status, last_error): (String, Option<String>) =
            sqlx::query_as("SELECT status::text, last_error FROM connectors WHERE id = $1")
                .bind(connector_id)
                .fetch_one(&pool)
                .await
                .expect("read connector");
        assert_eq!(status, "error", "a refused grant marks the connector");
        assert!(
            last_error
                .unwrap_or_default()
                .to_lowercase()
                .contains("reconnect"),
            "the connector carries the same instruction the client got"
        );

        // Nothing stale was written over a secret only a reconnect can replace.
        let stored = oauth::load_tokens(&pool, &key, connector_id, user.id)
            .await
            .expect("read tokens")
            .expect("the row still exists");
        assert_eq!(stored.access_token, "ya29.stale-access");
    }

    /// B3's error state: a connector already marked `error` is answered from the
    /// row alone. The same `400` comes back, Google's token endpoint is not
    /// asked again, and nothing the earlier failure wrote is overwritten.
    #[tokio::test]
    async fn error_connection_is_not_retried() {
        let Some(pool) =
            pool_or_skip("connectors::photos_refresh::tests::error_connection_is_not_retried")
                .await
        else {
            return;
        };
        let user = account(&pool).await;
        let key = key();

        // A token endpoint that would answer a refresh happily. It is reachable
        // and correct, so a `400` below can only come from not asking it.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
        let origin = format!(
            "http://{}",
            listener.local_addr().expect("stub local address")
        );
        let asked = Arc::new(AtomicBool::new(false));
        let answered = Arc::clone(&asked);
        let stub = tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            // Set before the answer is written, so the caller returning from the
            // request proves this flag is already visible: no timer needed.
            answered.store(true, Ordering::SeqCst);
            // Drain the request first: closing a socket with unread bytes in it
            // resets the connection and the client would never see the answer.
            let _ = read_request(&mut socket).await;
            let body = r#"{"access_token":"ya29.should-never-be-minted","expires_in":3600,"token_type":"Bearer"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });

        let picker = RecordingPicker::new();
        let app = app(&pool, Arc::clone(&key), oauth(&origin), picker.clone());
        let connector_id = broken_connection(&pool, &key, user.id).await;

        let response = post(&app, SESSIONS, &user).await;
        let status = response.status();
        let body = body_text(response).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a broken connector is still the person's to fix: {body}"
        );
        assert!(
            body.to_lowercase().contains("reconnect"),
            "the answer must say what to do: {body}"
        );

        assert!(
            picker.tokens().is_empty(),
            "no pick may start on a token that cannot be refreshed"
        );
        assert!(
            !asked.load(Ordering::SeqCst),
            "a connector already marked error must not reach Google's token endpoint"
        );

        // The row keeps the failure the refresh recorded: nothing cleared it and
        // nothing replaced the token a reconnect is meant to replace.
        let (status, last_error): (String, Option<String>) =
            sqlx::query_as("SELECT status::text, last_error FROM connectors WHERE id = $1")
                .bind(connector_id)
                .fetch_one(&pool)
                .await
                .expect("read connector");
        assert_eq!(
            status, "error",
            "the connector stays broken until a reconnect"
        );
        assert_eq!(
            last_error.as_deref(),
            Some(EARLIER_FAILURE),
            "the recorded failure is left exactly as the refusal wrote it"
        );
        let stored = oauth::load_tokens(&pool, &key, connector_id, user.id)
            .await
            .expect("read tokens")
            .expect("the row still exists");
        assert_eq!(stored.access_token, "ya29.stale-access");

        stub.abort();
    }
}
