//! Minimal user-scoped API surface (ticket T3).
//!
//! Only what T3 needs to make the isolation rules observable: who am I, and one
//! object lookup that proves a foreign id is a 404 rather than a 403 or, worse,
//! a 200. T4 grows this into the real tag/note surface; the pattern it must
//! keep is the one here — the user id comes from [`CurrentUser`] and every
//! query binds it, so the "not mine" case is simply the empty result set.

use axum::Json;
use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::routing::get;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::auth::store;
use crate::error::AppError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/me", get(me))
        .route("/api/v1/tags/{tag_id}", get(tag))
}

/// The signed-in user's own profile.
pub async fn me(State(state): State<AppState>, user: CurrentUser) -> Result<Json<Value>, AppError> {
    let profile = store::find_profile(&state.pool, user.id)
        .await?
        // The session points at a user that is gone (deleted between the
        // extractor and this query): treat it as signed out.
        .ok_or(AppError::Unauthenticated)?;

    Ok(Json(json!({
        "id": profile.id,
        "email": profile.email,
        "name": profile.name,
        "avatar_url": profile.avatar_url,
    })))
}

/// One tag, if and only if it belongs to the caller.
///
/// `WHERE id = $1 AND user_id = $2` — not "fetch then compare" — so another
/// user's id and a non-existent id take the exact same path to 404. Answering
/// 403 for "exists but not yours" would confirm the id is real.
pub async fn tag(
    State(state): State<AppState>,
    user: CurrentUser,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Value>, AppError> {
    let Path(tag_id) = path.map_err(|_| AppError::BadRequest("invalid tag id".into()))?;

    let row: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id, name FROM tags WHERE id = $1 AND user_id = $2")
            .bind(tag_id)
            .bind(user.id)
            .fetch_optional(&state.pool)
            .await?;

    let (id, name) = row.ok_or(AppError::NotFound)?;

    Ok(Json(json!({ "id": id, "name": name })))
}
