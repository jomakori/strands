//! Authentication and per-user sessions (ticket T3, spec D4 / §6 "Auth flow").
//!
//! The shape is deliberately small:
//!
//! * [`identity`] is the only place that talks to an identity provider, behind
//!   a trait so tests can sign two users in with no network.
//! * [`store`] is the only place that touches `users`, `sessions` and
//!   `oauth_requests`.
//! * [`session`] owns the cookie: one opaque server-side session id, never a
//!   provider token (spec: "never store raw tokens in a cookie").
//! * [`extractor::CurrentUser`] is the *only* way a handler learns who is
//!   calling. There is no second path — no user id from a path, query or body.

pub mod extractor;
pub mod identity;
pub mod routes;
pub mod session;
pub mod store;
pub mod token;

pub use extractor::CurrentUser;
pub use identity::{ConsentRequest, Identity, IdentityProvider};
pub use session::{
    SESSION_COOKIE_NAME, cleared_session_cookie, session_cookie, session_id_from_headers,
};

/// Failures specific to the login flow. Mapped to HTTP by
/// [`crate::error::AppError`].
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The authorization code was rejected by the provider, or the token
    /// endpoint answered non-2xx.
    #[error("the authorization code was rejected by the identity provider")]
    InvalidCode,
    /// The `/userinfo` answer is missing `sub`/`email`, or says the address is
    /// unverified — an identity we cannot build a `users` row from.
    #[error("invalid identity from userinfo: {0}")]
    InvalidIdentity(String),
    /// `state` was unknown, already used, or expired (single use, 10 min TTL).
    #[error("unknown or expired authorization state")]
    UnknownState,
    /// The provider redirected back with `error=…` (user denied consent).
    #[error("the identity provider reported an error: {0}")]
    Denied(String),
    /// Transport or provider-side failure — our fault, surfaced as a 500.
    #[error("identity provider request failed: {0}")]
    Provider(String),
}
