//! The three auth endpoints (ticket T3).
//!
//! ```text
//! GET  /auth/google            → 303 to the provider's consent screen
//! GET  /auth/google/callback   → exchange the code, set the session cookie, 303 to "/"
//! POST /auth/logout            → 204, session deleted and cookie cleared
//! ```
//!
//! The callback is the only handler that creates a session, and it does so only
//! after the provider has vouched for an identity. Nothing here reads a user id
//! from the request.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::{Duration, Utc};
use serde::Deserialize;

use crate::app::AppState;
use crate::auth::AuthError;
use crate::auth::identity::ConsentRequest;
use crate::auth::session::{cleared_session_cookie, session_cookie, session_id_from_headers};
use crate::auth::store;
use crate::auth::token::{pkce_challenge, random_token};
use crate::error::AppError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/google", get(start))
        .route("/auth/google/callback", get(callback))
        .route("/auth/logout", post(logout))
}

/// Begin the flow: mint state + PKCE verifier, remember them server-side, then
/// hand the browser to the provider.
///
/// The verifier never leaves the server — not in the URL, not in a cookie —
/// which is the whole point of storing it in `oauth_requests`. The nonce is
/// minted on the same draw and travels to the issuer; see the `identity` module
/// note for why nothing compares it.
pub async fn start(State(state): State<AppState>) -> Result<Response, AppError> {
    let state_token = random_token();
    let code_verifier = random_token();
    let nonce = random_token();
    let code_challenge = pkce_challenge(&code_verifier);
    let redirect_uri = state.auth.redirect_uri();

    store::store_oauth_request(
        &state.pool,
        &state_token,
        &code_verifier,
        &redirect_uri,
        Utc::now(),
    )
    .await?;

    let consent_url = state.identity.consent_url(&ConsentRequest {
        state: &state_token,
        nonce: &nonce,
        code_challenge: &code_challenge,
        redirect_uri: &redirect_uri,
    });

    Ok(Redirect::to(&consent_url).into_response())
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// Finish the flow.
///
/// `state` is claimed by deleting its row, so a replayed callback — the classic
/// login-CSRF probe — finds nothing and gets a 400. The stored `redirect_uri`
/// is replayed to the token endpoint rather than re-derived from config, so the
/// two values cannot drift apart mid-flow.
pub async fn callback(
    State(state): State<AppState>,
    Query(query): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    if let Some(error) = query.error.filter(|error| !error.is_empty()) {
        return Err(AuthError::Denied(error).into());
    }

    let code = query
        .code
        .filter(|code| !code.is_empty())
        .ok_or_else(|| AppError::BadRequest("missing `code`".into()))?;
    let state_token = query
        .state
        .filter(|state| !state.is_empty())
        .ok_or_else(|| AppError::BadRequest("missing `state`".into()))?;

    let pending = store::take_oauth_request(&state.pool, &state_token)
        .await?
        .ok_or(AuthError::UnknownState)?;

    let identity = state
        .identity
        .exchange_code(&code, &pending.pkce_verifier, &pending.redirect_uri)
        .await?;

    let user_id = store::upsert_user(&state.pool, &identity).await?;
    // Spec §6: a user starts with five categories. Doing it here — inside the
    // only handler that can create a user — keeps "first login" in one place.
    store::seed_default_categories(&state.pool, user_id).await?;
    let ttl = Duration::days(state.auth.session_ttl_days);
    let session_id = store::create_session(&state.pool, user_id, ttl).await?;
    let cookie = session_cookie(session_id, ttl, state.auth.cookie_secure, Utc::now());

    // 303 so the browser re-issues the follow-up as a GET without the `code`.
    Ok(([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response())
}

/// Revoke the session and clear the cookie.
///
/// Deliberately not `CurrentUser`-gated: an expired session still has a cookie
/// to clear, and logging out a session that is already gone is a no-op, not an
/// error. No user id is read or needed here.
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if let Some(session_id) = session_id_from_headers(&headers) {
        store::delete_session(&state.pool, session_id).await?;
    }

    Ok((
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            cleared_session_cookie(state.auth.cookie_secure),
        )],
    )
        .into_response())
}
