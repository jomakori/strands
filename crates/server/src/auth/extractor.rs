//! `CurrentUser` — the single door for authentication (ticket T3, spec §6
//! closing line: "a single require_user extractor, no raw IDs from the client
//! without ownership check").
//!
//! Handlers that need to know who is calling take `CurrentUser` as an argument.
//! A handler that does not take it cannot see a user id at all, so "did we
//! forget to scope this route?" is answered by reading the signature. Every
//! rejection is a 401 — no cookie, malformed cookie, unknown session id, and
//! expired session are deliberately indistinguishable from the outside.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use strands_core::{SessionId, UserId};

use crate::app::AppState;
use crate::auth::session::session_id_from_headers;
use crate::auth::store;
use crate::error::AppError;

/// The authenticated caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrentUser {
    pub id: UserId,
    /// Kept so a handler can revoke *this* session without guessing which one.
    pub session_id: SessionId,
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // Staging demo mode (ticket C3): the process was started with
        // STRANDS_DEMO_MODE=1, so every request is the demo account. Production
        // cannot reach this branch — the config refuses to boot there — and the
        // check comes before the cookie so a demo visitor needs no login at all.
        if let Some(demo) = state.demo {
            return Ok(Self {
                id: demo.user_id,
                session_id: demo.session_id,
            });
        }

        let session_id =
            session_id_from_headers(&parts.headers).ok_or(AppError::Unauthenticated)?;

        let user_id = store::find_session_user(&state.pool, session_id)
            .await?
            .ok_or(AppError::Unauthenticated)?;

        Ok(Self {
            id: user_id,
            session_id,
        })
    }
}
