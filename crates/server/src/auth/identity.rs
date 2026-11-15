//! The identity-provider seam (ticket T3).
//!
//! Handlers never speak OAuth themselves: they call an
//! [`IdentityProvider`]. Production wires in [`GoogleIdentityProvider`];
//! `tests/auth_isolation.rs` wires in a fake that hands back a fixed identity,
//! so the whole login flow — state, PKCE, session, cookie — runs with no
//! network and no Google credentials.
//!
//! ## Why the id_token signature is not checked
//!
//! The token arrives as the direct response of our own POST to Google's token
//! endpoint over TLS, authenticated with the client secret. OpenID Connect Core
//! §3.1.3.7 allows exactly that: TLS server validation stands in for signature
//! validation when the token comes straight from the token endpoint. The claims
//! that matter an attacker cannot forge are still validated below — issuer,
//! audience, expiry, subject — so a token minted for another client is refused.

use std::future::Future;
use std::pin::Pin;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use serde::Deserialize;

use super::AuthError;
use crate::auth::token::percent_encode;
use crate::config::AuthConfig;

/// Boxed future so [`IdentityProvider`] stays object-safe (`Arc<dyn …>`).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Scopes needed for identity only: `openid` for the id_token, `email` and
/// `profile` for the claims the `users` row stores.
const SCOPES: &str = "openid email profile";

/// Google uses both spellings depending on the endpoint generation.
const GOOGLE_ISSUERS: &[&str] = &["accounts.google.com", "https://accounts.google.com"];

/// A verified signed-in person, normalised away from any provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// Stable per-provider subject (`sub`). This is the join key, not the email:
    /// an email can change hands.
    pub subject: String,
    pub email: String,
    pub name: String,
    pub avatar_url: Option<String>,
}

/// The parameters of a consent redirect.
pub struct ConsentRequest<'a> {
    pub state: &'a str,
    pub code_challenge: &'a str,
    pub redirect_uri: &'a str,
}

/// Everything the auth handlers need from an identity provider.
pub trait IdentityProvider: Send + Sync {
    /// Where to send the browser to start the flow.
    fn consent_url(&self, request: &ConsentRequest<'_>) -> String;

    /// Redeem an authorization code for a verified identity.
    fn exchange_code<'a>(
        &'a self,
        code: &'a str,
        code_verifier: &'a str,
        redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<Identity, AuthError>>;
}

/// Google, authorization-code flow with PKCE.
pub struct GoogleIdentityProvider {
    client: reqwest::Client,
    client_id: String,
    client_secret: String,
    authorization_endpoint: String,
    token_endpoint: String,
}

impl GoogleIdentityProvider {
    pub fn from_config(config: &AuthConfig) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("strands-server/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(15))
            .build()?;

        Ok(Self {
            client,
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
            authorization_endpoint: config.authorization_endpoint.clone(),
            token_endpoint: config.token_endpoint.clone(),
        })
    }
}

impl IdentityProvider for GoogleIdentityProvider {
    fn consent_url(&self, request: &ConsentRequest<'_>) -> String {
        format!(
            "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}\
             &code_challenge={}&code_challenge_method=S256&access_type=online\
             &prompt=select_account",
            self.authorization_endpoint,
            percent_encode(&self.client_id),
            percent_encode(request.redirect_uri),
            percent_encode(SCOPES),
            percent_encode(request.state),
            percent_encode(request.code_challenge),
        )
    }

    fn exchange_code<'a>(
        &'a self,
        code: &'a str,
        code_verifier: &'a str,
        redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<Identity, AuthError>> {
        Box::pin(async move {
            let response = self
                .client
                .post(&self.token_endpoint)
                .form(&[
                    ("code", code),
                    ("client_id", self.client_id.as_str()),
                    ("client_secret", self.client_secret.as_str()),
                    // Must equal the value used on the consent redirect; the
                    // stored row is what we replay here, not a re-derived one.
                    ("redirect_uri", redirect_uri),
                    ("grant_type", "authorization_code"),
                    ("code_verifier", code_verifier),
                ])
                .send()
                .await
                .map_err(|error| AuthError::Provider(error.to_string()))?;

            if !response.status().is_success() {
                // Google answers 400 `invalid_grant` for a replayed, expired or
                // mismatched code — the client's error, not a server fault.
                return Err(AuthError::InvalidCode);
            }

            let tokens: TokenResponse = response.json().await.map_err(|error| {
                AuthError::Provider(format!("token response was not JSON: {error}"))
            })?;
            let id_token = tokens.id_token.ok_or_else(|| {
                AuthError::InvalidToken("token response carried no id_token".into())
            })?;

            identity_from_id_token(&id_token, &self.client_id)
        })
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IdTokenClaims {
    sub: String,
    iss: String,
    aud: Audience,
    exp: i64,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    picture: Option<String>,
}

/// `aud` is a string for a single audience and an array for several; both are
/// legal JWT, so accept both rather than failing on a shape Google happens not
/// to send today.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn contains(&self, client_id: &str) -> bool {
        match self {
            Self::One(audience) => audience == client_id,
            Self::Many(audiences) => audiences.iter().any(|audience| audience == client_id),
        }
    }
}

/// Decode the claims of an id_token and turn them into an [`Identity`].
///
/// Split out and public so the claim rules are testable without a network.
pub fn identity_from_id_token(id_token: &str, client_id: &str) -> Result<Identity, AuthError> {
    let claims = decode_claims(id_token)?;

    if !GOOGLE_ISSUERS.contains(&claims.iss.as_str()) {
        return Err(AuthError::InvalidToken(format!(
            "unexpected issuer {}",
            claims.iss
        )));
    }

    if !claims.aud.contains(client_id) {
        return Err(AuthError::InvalidToken(
            "id_token was issued for another client".into(),
        ));
    }

    if claims.exp <= Utc::now().timestamp() {
        return Err(AuthError::InvalidToken("id_token has expired".into()));
    }

    if claims.sub.trim().is_empty() {
        return Err(AuthError::InvalidToken("id_token has no subject".into()));
    }

    // `email` is only guaranteed because the `email` scope was requested; a
    // provider that omits it cannot be turned into a user row.
    let email = claims
        .email
        .filter(|email| !email.trim().is_empty())
        .ok_or_else(|| AuthError::InvalidToken("id_token has no email claim".into()))?;

    let name = claims
        .name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| email.split('@').next().unwrap_or("strands user").to_owned());

    Ok(Identity {
        subject: claims.sub,
        email,
        name,
        avatar_url: claims.picture.filter(|url| !url.trim().is_empty()),
    })
}

fn decode_claims(id_token: &str) -> Result<IdTokenClaims, AuthError> {
    let payload = id_token
        .split('.')
        .nth(1)
        .ok_or_else(|| AuthError::InvalidToken("id_token is not a JWT".into()))?;

    let bytes = URL_SAFE_NO_PAD.decode(payload).map_err(|error| {
        AuthError::InvalidToken(format!("id_token payload is not base64url: {error}"))
    })?;

    serde_json::from_slice(&bytes)
        .map_err(|error| AuthError::InvalidToken(format!("id_token payload is not JSON: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn id_token(claims: serde_json::Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(json!({ "alg": "RS256", "typ": "JWT" }).to_string());
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
        // Signature bytes are irrelevant: see the module note on OIDC Core
        // §3.1.3.7 — the token is trusted for arriving over TLS from Google.
        format!("{header}.{payload}.signature-not-checked")
    }

    fn valid_claims() -> serde_json::Value {
        json!({
            "sub": "google-sub-alice",
            "iss": "https://accounts.google.com",
            "aud": "client-id.apps.googleusercontent.com",
            "exp": Utc::now().timestamp() + 600,
            "email": "alice@example.com",
            "name": "Alice",
            "picture": "https://example.com/alice.png",
        })
    }

    #[test]
    fn a_valid_id_token_becomes_an_identity() {
        let identity = identity_from_id_token(
            &id_token(valid_claims()),
            "client-id.apps.googleusercontent.com",
        )
        .unwrap();

        assert_eq!(
            identity,
            Identity {
                subject: "google-sub-alice".into(),
                email: "alice@example.com".into(),
                name: "Alice".into(),
                avatar_url: Some("https://example.com/alice.png".into()),
            }
        );
    }

    #[test]
    fn a_token_issued_for_another_client_is_refused() {
        let error = identity_from_id_token(&id_token(valid_claims()), "someone-else").unwrap_err();
        assert!(matches!(error, AuthError::InvalidToken(_)), "{error}");
    }

    #[test]
    fn an_expired_token_is_refused() {
        let mut claims = valid_claims();
        claims["exp"] = json!(Utc::now().timestamp() - 1);
        let error =
            identity_from_id_token(&id_token(claims), "client-id.apps.googleusercontent.com")
                .unwrap_err();
        assert!(error.to_string().contains("expired"), "{error}");
    }

    #[test]
    fn a_foreign_issuer_is_refused() {
        let mut claims = valid_claims();
        claims["iss"] = json!("https://evil.example");
        assert!(
            identity_from_id_token(&id_token(claims), "client-id.apps.googleusercontent.com")
                .is_err()
        );
    }

    #[test]
    fn a_missing_email_is_refused() {
        let mut claims = valid_claims();
        claims.as_object_mut().unwrap().remove("email");
        assert!(
            identity_from_id_token(&id_token(claims), "client-id.apps.googleusercontent.com")
                .is_err()
        );
    }

    #[test]
    fn the_consent_url_carries_state_pkce_and_the_encoded_redirect() {
        let config = AuthConfig {
            client_id: "client-id".into(),
            client_secret: "secret".into(),
            public_base_url: "https://strands.example".into(),
            cookie_secure: true,
            session_ttl_days: 30,
            authorization_endpoint: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_endpoint: "https://oauth2.googleapis.com/token".into(),
        };
        let provider = GoogleIdentityProvider::from_config(&config).unwrap();

        let url = provider.consent_url(&ConsentRequest {
            state: "state-value",
            code_challenge: "challenge-value",
            redirect_uri: &config.redirect_uri(),
        });

        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("state=state-value"));
        assert!(url.contains("code_challenge=challenge-value"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(
            url.contains("redirect_uri=https%3A%2F%2Fstrands.example%2Fauth%2Fgoogle%2Fcallback")
        );
        assert!(url.contains("response_type=code"));
        assert!(url.contains("scope=openid%20email%20profile"));
    }
}
