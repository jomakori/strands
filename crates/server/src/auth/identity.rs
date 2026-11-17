//! The identity-provider seam (ticket T3, reworked for the Dex broker by T20c).
//!
//! Handlers never speak OAuth themselves: they call an [`IdentityProvider`].
//! Production wires in [`OidcIdentityProvider`], a **public** client of the
//! cluster's Dex issuer; `tests/auth_isolation.rs` wires in a fake that hands
//! back a fixed identity, so the whole login flow — state, PKCE, session, cookie
//! — runs with no network at all.
//!
//! ## Why Dex, and why there is no secret
//!
//! Google has no API for creating OAuth clients, so the cluster registers one
//! Google client at Dex and every app is a public client of Dex: client id in
//! the URL bar, no client secret anywhere. A public client cannot prove
//! possession with a credential, so it proves it with PKCE (RFC 7636): the
//! verifier stays in `oauth_requests`, keyed by the single-use `state`, and
//! never leaves the server.
//!
//! ## How identity is established, and what makes it trustworthy
//!
//! Identity is read from the issuer's `/userinfo` endpoint with the access token
//! — **not** from the `id_token`. Two facts make that call meaningful:
//!
//! 1. the access token was handed to *us* by the token endpoint, in the direct
//!    response to our own server-side POST over TLS, never through the browser;
//! 2. the code that produced it was bound to this browser by PKCE and by the
//!    single-use `state` we minted.
//!
//! So the userinfo response is an answer to a request only this process could
//! make. Nothing is read out of the `id_token`, which is why its signature is
//! not verified and no JWT/JWKS machinery is pulled in: an unverified claim is
//! worth nothing, and pretending otherwise would be worse than not looking.
//!
//! `nonce` is still sent on the authorize request. It is the OIDC login-binding
//! value, and the honest statement of its status here is that **nothing checks
//! it**, because the only thing a nonce is compared against is the `id_token` we
//! deliberately do not consume. Sending it keeps the request a conforming OIDC
//! authentication request; it is not what stops a replayed login. `state`
//! (single use, server-stored, 10 min TTL) is that control.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::Deserialize;

use super::AuthError;
use crate::auth::token::percent_encode;
use crate::config::AuthConfig;

/// Boxed future so [`IdentityProvider`] stays object-safe (`Arc<dyn …>`).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Scopes needed for identity only: `openid` for a conforming OIDC request,
/// `email` and `profile` for the claims the `users` row stores.
const SCOPES: &str = "openid email profile";

/// The endpoints we use, taken from the issuer's discovery document rather than
/// hard-coded, so moving Dex (or pointing a test at a stub) needs no rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    pub authorization: String,
    pub token: String,
    pub userinfo: String,
}

/// A verified signed-in person, normalised away from any provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// Stable per-issuer subject (`sub`). This is the join key, not the email: an
    /// email can change hands.
    pub subject: String,
    pub email: String,
    pub name: String,
    pub avatar_url: Option<String>,
}

/// The parameters of a consent redirect.
pub struct ConsentRequest<'a> {
    pub state: &'a str,
    /// See the module note: sent, not checked.
    pub nonce: &'a str,
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

/// Why the endpoint list could not be resolved. Reported at boot, so every
/// variant names the URL that failed.
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("could not build the HTTP client: {0}")]
    Client(#[from] reqwest::Error),
    #[error("could not fetch {url}: {source}")]
    Fetch { url: String, source: reqwest::Error },
    #[error("{url} did not answer with a JSON discovery document: {source}")]
    NotJson { url: String, source: reqwest::Error },
    #[error("{url} does not describe a usable issuer: {detail}")]
    Unusable { url: String, detail: String },
}

/// The subset of the discovery document we need (OIDC Discovery 1.0 §3).
///
/// Every field is read as optional so a document that is merely *wrong* gets a
/// message naming what is missing, instead of serde's "missing field".
#[derive(Debug, Deserialize)]
struct DiscoveryDocument {
    #[serde(default)]
    issuer: Option<String>,
    #[serde(default)]
    authorization_endpoint: Option<String>,
    #[serde(default)]
    token_endpoint: Option<String>,
    #[serde(default)]
    userinfo_endpoint: Option<String>,
    #[serde(default)]
    code_challenge_methods_supported: Vec<String>,
}

impl DiscoveryDocument {
    /// Check the document against the issuer we asked about, and turn it into
    /// [`Endpoints`].
    fn into_endpoints(self, issuer: &str, url: &str) -> Result<Endpoints, DiscoveryError> {
        let unusable = |detail: &str| DiscoveryError::Unusable {
            url: url.to_owned(),
            detail: detail.to_owned(),
        };

        // OIDC Discovery §4.3: the document's `issuer` must equal the one we
        // requested, otherwise a document fetched from one issuer could name
        // another's endpoints. Comparing normalized trailing slashes only.
        match self.issuer.as_deref() {
            Some(documented) if documented.trim_end_matches('/') == issuer => {}
            Some(documented) => {
                return Err(unusable(&format!("issuer is {documented}, not {issuer}")));
            }
            None => return Err(unusable("no issuer")),
        }

        // PKCE S256 is how a public client proves possession; an issuer that does
        // not advertise it cannot complete our flow, and saying so at boot beats
        // sending a challenge nobody will check.
        if !self
            .code_challenge_methods_supported
            .iter()
            .any(|method| method.eq_ignore_ascii_case("S256"))
        {
            return Err(unusable(
                "code_challenge_methods_supported does not list S256",
            ));
        }

        let field = |value: Option<String>, name: &str| -> Result<String, DiscoveryError> {
            value
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| unusable(&format!("no {name}")))
        };

        Ok(Endpoints {
            authorization: field(self.authorization_endpoint, "authorization_endpoint")?,
            token: field(self.token_endpoint, "token_endpoint")?,
            // Required here even though OIDC makes it optional per client: it is
            // where our identity comes from.
            userinfo: field(self.userinfo_endpoint, "userinfo_endpoint")?,
        })
    }
}

/// The cluster's Dex issuer, authorization-code flow with PKCE and no secret.
pub struct OidcIdentityProvider {
    client: reqwest::Client,
    client_id: String,
    endpoints: Endpoints,
}

impl OidcIdentityProvider {
    fn http_client() -> Result<reqwest::Client, reqwest::Error> {
        reqwest::Client::builder()
            .user_agent(concat!("strands-server/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(15))
            .build()
    }

    /// Resolve the endpoints from `{issuer}/.well-known/openid-configuration`.
    ///
    /// Discovery runs **once, at boot**, not on first use. The endpoint list is
    /// static for the life of an issuer, so a cache with an invalidation policy
    /// would be a cache with nothing to invalidate; and doing it at boot turns a
    /// wrong `OIDC_ISSUER` or an unreachable Dex into a crash loop visible in
    /// `kubectl get pods` instead of a 500 on the first login. The cost is that
    /// this pod will not start while Dex is down — which is the same failure the
    /// pod would have anyway, one login earlier.
    pub async fn discover(config: &AuthConfig) -> Result<Self, DiscoveryError> {
        let client = Self::http_client()?;
        let url = config.discovery_url();

        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|source| DiscoveryError::Fetch {
                url: url.clone(),
                source,
            })?
            .error_for_status()
            .map_err(|source| DiscoveryError::Fetch {
                url: url.clone(),
                source,
            })?;

        let document: DiscoveryDocument =
            response
                .json()
                .await
                .map_err(|source| DiscoveryError::NotJson {
                    url: url.clone(),
                    source,
                })?;

        let endpoints = document.into_endpoints(&config.issuer, &url)?;

        Ok(Self {
            client,
            client_id: config.client_id.clone(),
            endpoints,
        })
    }

    /// Build a provider from a known endpoint set. This is the seam the tests
    /// use: a real provider, no discovery round trip.
    pub fn with_endpoints(
        config: &AuthConfig,
        endpoints: Endpoints,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: Self::http_client()?,
            client_id: config.client_id.clone(),
            endpoints,
        })
    }

    /// The endpoint list, for logging at boot and for assertions in tests.
    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }
}

impl IdentityProvider for OidcIdentityProvider {
    fn consent_url(&self, request: &ConsentRequest<'_>) -> String {
        // No `client_secret` parameter exists for a public client — there is no
        // secret to send. `access_type`/`prompt` were Google-specific and Dex
        // ignores them; Dex's own login page is the account chooser.
        format!(
            "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}\
             &nonce={}&code_challenge={}&code_challenge_method=S256",
            self.endpoints.authorization,
            percent_encode(&self.client_id),
            percent_encode(request.redirect_uri),
            percent_encode(SCOPES),
            percent_encode(request.state),
            percent_encode(request.nonce),
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
                .post(&self.endpoints.token)
                .form(&[
                    ("code", code),
                    ("client_id", self.client_id.as_str()),
                    // Must equal the value used on the consent redirect; the
                    // stored row is what we replay here, not a re-derived one.
                    ("redirect_uri", redirect_uri),
                    ("grant_type", "authorization_code"),
                    // The whole of our proof of possession.
                    ("code_verifier", code_verifier),
                ])
                .send()
                .await
                .map_err(|error| AuthError::Provider(error.to_string()))?;

            if !response.status().is_success() {
                // Dex answers 400 `invalid_grant` for a replayed, expired or
                // mismatched code, and `invalid_request` for a wrong verifier —
                // the client's error, not a server fault.
                return Err(AuthError::InvalidCode);
            }

            let tokens: TokenResponse = response.json().await.map_err(|error| {
                AuthError::Provider(format!("token response was not JSON: {error}"))
            })?;
            let access_token = tokens.access_token.ok_or_else(|| {
                AuthError::InvalidIdentity("token response carried no access_token".into())
            })?;

            let response = self
                .client
                .get(&self.endpoints.userinfo)
                .bearer_auth(&access_token)
                .send()
                .await
                .map_err(|error| AuthError::Provider(error.to_string()))?;

            if !response.status().is_success() {
                // A token the issuer just minted for us is being refused by the
                // issuer's own userinfo endpoint: that is our problem (or the
                // issuer's), not the person logging in.
                return Err(AuthError::Provider(format!(
                    "userinfo answered {}",
                    response.status()
                )));
            }

            let claims: UserInfo = response
                .json()
                .await
                .map_err(|error| AuthError::Provider(format!("userinfo was not JSON: {error}")))?;

            identity_from_userinfo(claims)
        })
    }
}

/// What the token endpoint gives us. `id_token` is deliberately unread — see the
/// module note — but the field is kept so a response carrying one is not an
/// error.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    #[serde(rename = "token_type")]
    _token_type: Option<String>,
}

/// The userinfo claims we use. Dex fills these from the upstream connector
/// (`email`, `email_verified`) and leaves `name`/`picture` absent for a Google
/// login that does not request them, so both are optional here.
#[derive(Debug, Default, Deserialize)]
struct UserInfo {
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    email_verified: Option<bool>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    picture: Option<String>,
}

/// Turn a userinfo response into an [`Identity`].
///
/// Pure and split out from the request so the rules are testable without a
/// network.
fn identity_from_userinfo(claims: UserInfo) -> Result<Identity, AuthError> {
    let subject = claims
        .sub
        .filter(|sub| !sub.trim().is_empty())
        .ok_or_else(|| AuthError::InvalidIdentity("userinfo has no subject".into()))?;

    let email = claims
        .email
        .filter(|email| !email.trim().is_empty())
        .ok_or_else(|| AuthError::InvalidIdentity("userinfo has no email".into()))?;

    // The `users` row is keyed on the address, so an address the issuer itself
    // will not vouch for must not become an account. Absent is treated as
    // "not stated" and allowed; an explicit `false` is not.
    if claims.email_verified == Some(false) {
        return Err(AuthError::InvalidIdentity(
            "userinfo says the email is not verified".into(),
        ));
    }

    let name = claims
        .name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| email.split('@').next().unwrap_or("strands user").to_owned());

    Ok(Identity {
        subject,
        email,
        name,
        avatar_url: claims.picture.filter(|url| !url.trim().is_empty()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> AuthConfig {
        AuthConfig {
            issuer: "https://dex.example".into(),
            client_id: "strands".into(),
            public_base_url: "https://strands.example".into(),
            cookie_secure: true,
            session_ttl_days: 30,
        }
    }

    fn endpoints() -> Endpoints {
        Endpoints {
            authorization: "https://dex.example/auth".into(),
            token: "https://dex.example/token".into(),
            userinfo: "https://dex.example/userinfo".into(),
        }
    }

    fn provider() -> OidcIdentityProvider {
        OidcIdentityProvider::with_endpoints(&config(), endpoints()).unwrap()
    }

    fn userinfo(sub: &str, email: Option<&str>) -> UserInfo {
        UserInfo {
            sub: Some(sub.into()),
            email: email.map(str::to_owned),
            ..UserInfo::default()
        }
    }

    /// The ticket's headline assertion: an authorize URL with PKCE S256, state,
    /// nonce — and no secret anywhere in it.
    #[test]
    fn the_consent_url_carries_pkce_state_and_nonce_and_no_secret() {
        let config = config();
        let url = provider().consent_url(&ConsentRequest {
            state: "state-value",
            nonce: "nonce-value",
            code_challenge: "challenge-value",
            redirect_uri: &config.redirect_uri(),
        });

        assert!(url.starts_with("https://dex.example/auth?"), "{url}");
        assert!(url.contains("client_id=strands"), "{url}");
        assert!(url.contains("response_type=code"), "{url}");
        assert!(url.contains("scope=openid%20email%20profile"), "{url}");
        assert!(url.contains("state=state-value"), "{url}");
        assert!(url.contains("nonce=nonce-value"), "{url}");
        assert!(url.contains("code_challenge=challenge-value"), "{url}");
        assert!(url.contains("code_challenge_method=S256"), "{url}");
        assert!(
            url.contains("redirect_uri=https%3A%2F%2Fstrands.example%2Fauth%2Fgoogle%2Fcallback"),
            "{url}"
        );
        assert!(
            !url.contains("client_secret") && !url.to_ascii_lowercase().contains("secret"),
            "a public client has no secret to leak: {url}"
        );
    }

    #[test]
    fn a_userinfo_response_becomes_an_identity() {
        let identity = identity_from_userinfo(UserInfo {
            sub: Some("dex-subject".into()),
            email: Some("alice@example.com".into()),
            email_verified: Some(true),
            name: Some("Alice".into()),
            picture: Some("https://example.com/alice.png".into()),
        })
        .unwrap();

        assert_eq!(
            identity,
            Identity {
                subject: "dex-subject".into(),
                email: "alice@example.com".into(),
                name: "Alice".into(),
                avatar_url: Some("https://example.com/alice.png".into()),
            }
        );
    }

    /// Dex does not always carry a display name for a Google login; the address
    /// is a better fallback than an empty card.
    #[test]
    fn a_missing_name_falls_back_to_the_local_part() {
        let identity = identity_from_userinfo(userinfo("sub-1", Some("bob@example.com"))).unwrap();
        assert_eq!(identity.name, "bob");
    }

    #[test]
    fn a_missing_email_is_refused() {
        let error = identity_from_userinfo(userinfo("sub-1", None)).unwrap_err();
        assert!(matches!(error, AuthError::InvalidIdentity(_)), "{error}");
        assert!(error.to_string().contains("email"), "{error}");
    }

    #[test]
    fn a_missing_subject_is_refused() {
        let error = identity_from_userinfo(userinfo("   ", Some("alice@example.com"))).unwrap_err();
        assert!(matches!(error, AuthError::InvalidIdentity(_)), "{error}");
    }

    /// An address the issuer will not vouch for must not become an account.
    #[test]
    fn an_unverified_email_is_refused() {
        let mut claims = userinfo("sub-1", Some("alice@example.com"));
        claims.email_verified = Some(false);
        let error = identity_from_userinfo(claims).unwrap_err();
        assert!(error.to_string().contains("not verified"), "{error}");
    }

    /// A discovery document that describes a different issuer is refused, so a
    /// misdirected fetch cannot silently point the flow somewhere else.
    #[test]
    fn a_discovery_document_naming_another_issuer_is_refused() {
        let document = DiscoveryDocument {
            issuer: Some("https://evil.example".into()),
            authorization_endpoint: Some("https://evil.example/auth".into()),
            token_endpoint: Some("https://evil.example/token".into()),
            userinfo_endpoint: Some("https://evil.example/userinfo".into()),
            code_challenge_methods_supported: vec!["S256".into()],
        };

        let error = document
            .into_endpoints("https://dex.example", "https://dex.example/.well-known/x")
            .unwrap_err();
        assert!(
            error.to_string().contains("not https://dex.example"),
            "{error}"
        );
    }

    #[test]
    fn a_discovery_document_without_userinfo_is_refused() {
        let document = DiscoveryDocument {
            issuer: Some("https://dex.example".into()),
            authorization_endpoint: Some("https://dex.example/auth".into()),
            token_endpoint: Some("https://dex.example/token".into()),
            userinfo_endpoint: None,
            code_challenge_methods_supported: vec!["S256".into()],
        };

        let error = document
            .into_endpoints("https://dex.example", "https://dex.example/.well-known/x")
            .unwrap_err();
        assert!(error.to_string().contains("userinfo_endpoint"), "{error}");
    }

    /// No S256 advertised means PKCE cannot be relied on, which for a public
    /// client is the whole story.
    #[test]
    fn a_discovery_document_without_s256_is_refused() {
        let document = DiscoveryDocument {
            issuer: Some("https://dex.example".into()),
            authorization_endpoint: Some("https://dex.example/auth".into()),
            token_endpoint: Some("https://dex.example/token".into()),
            userinfo_endpoint: Some("https://dex.example/userinfo".into()),
            code_challenge_methods_supported: vec!["plain".into()],
        };

        assert!(
            document
                .into_endpoints("https://dex.example", "https://dex.example/.well-known/x")
                .is_err()
        );
    }
}
