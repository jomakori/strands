//! The three auth endpoints (ticket T3).
//!
//! ```text
//! GET  /auth/google            → 303 to the provider's consent screen
//! GET  /auth/google/callback   → exchange the code, set the session cookie, 303 back to the
//!                                route the visitor asked for ("\/" when they did not ask)
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
use crate::auth::session::{
    after_sign_in_cookie, after_sign_in_path_from_headers, cleared_after_sign_in_cookie,
    cleared_session_cookie, safe_return_path, session_cookie, session_id_from_headers,
};
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
pub async fn start(
    State(state): State<AppState>,
    Query(query): Query<StartQuery>,
) -> Result<Response, AppError> {
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

    // A signed-out visitor who followed a deep link carries that route in `next`; remember it
    // in a cookie scoped to `/auth` so the callback can send them back to it. An unusable value
    // is dropped rather than rejected — a bad link should still let somebody sign in.
    let response = match query.next.as_deref().and_then(safe_return_path) {
        Some(return_to) => (
            [(
                header::SET_COOKIE,
                after_sign_in_cookie(&return_to, state.auth.cookie_secure),
            )],
            Redirect::to(&consent_url),
        )
            .into_response(),
        None => Redirect::to(&consent_url).into_response(),
    };
    Ok(response)
}

/// `?next=` on the sign-in link: the route the visitor was trying to reach.
#[derive(Debug, Deserialize)]
pub struct StartQuery {
    next: Option<String>,
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
    headers: HeaderMap,
) -> Result<Response, AppError> {
    // Read before anything consumes the request: where this visitor was headed when they hit
    // the gate, if they came from a deep link.
    let return_to = after_sign_in_path_from_headers(&headers).unwrap_or_else(|| "/".to_owned());

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

    // 303 so the browser re-issues the follow-up as a GET without the `code`, and back to the
    // route they asked for — `/` for everyone who just pressed the button.
    Ok((
        [
            (header::SET_COOKIE, cookie),
            (
                header::SET_COOKIE,
                cleared_after_sign_in_cookie(state.auth.cookie_secure),
            ),
        ],
        Redirect::to(&return_to),
    )
        .into_response())
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
