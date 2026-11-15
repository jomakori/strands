//! HTTP handlers for guided summary generation and history (ticket T7).
//!
//! Both routes scope by `WHERE id = $1 AND user_id = $2` through
//! `store::tag_exists_for_user`, so a foreign tag 404s exactly like every
//! other handler in this server (see `api::tag_detail`).

use axum::Json;
use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use strands_api_types::{GenerateSummaryRequest, SummaryHistory, SummaryVersion};
use uuid::Uuid;

use super::store;
use super::{build_prompt, compute_source_hash};
use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/tags/{tag_id}/summary", post(generate_summary))
        .route(
            "/api/v1/tags/{tag_id}/summary/history",
            get(summary_history),
        )
}

/// `POST /api/v1/tags/{id}/summary`: generate a summary version.
///
/// Short-circuits with zero LLM calls when `force` is false and the newest
/// version's `source_hash` already matches the notes and guide — the
/// "generate" action is then a no-op that returns what already exists.
/// `force: true` is the "regenerate" action: it always calls the LLM, even
/// when the source is unchanged.
pub async fn generate_summary(
    State(state): State<AppState>,
    user: CurrentUser,
    path: Result<Path<Uuid>, PathRejection>,
    Json(request): Json<GenerateSummaryRequest>,
) -> Result<Json<SummaryVersion>, AppError> {
    let Path(tag_id) = path.map_err(|_| AppError::BadRequest("invalid tag id".into()))?;

    if !store::tag_exists_for_user(&state.pool, tag_id, user.id).await? {
        return Err(AppError::NotFound);
    }

    let notes = store::fetch_note_bodies(&state.pool, tag_id, user.id).await?;
    let source_hash = compute_source_hash(&request.guide, &notes);

    if !request.force
        && let Some(existing) = store::newest_version(&state.pool, tag_id, user.id).await?
        && existing.source_hash == source_hash
    {
        return Ok(Json(existing.into()));
    }

    let prompt = build_prompt(&request.guide, &notes);
    let body = state.summarizer.summarize(&prompt).await?;

    let version = store::insert_version(
        &state.pool,
        store::NewVersion {
            tag_id,
            user_id: user.id,
            guide: &request.guide,
            body: &body,
            model_alias: state.summarizer.model_alias(),
            source_hash: &source_hash,
        },
    )
    .await?;

    Ok(Json(version.into()))
}

/// `GET /api/v1/tags/{id}/summary/history`: every version, newest first.
pub async fn summary_history(
    State(state): State<AppState>,
    user: CurrentUser,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<SummaryHistory>, AppError> {
    let Path(tag_id) = path.map_err(|_| AppError::BadRequest("invalid tag id".into()))?;

    if !store::tag_exists_for_user(&state.pool, tag_id, user.id).await? {
        return Err(AppError::NotFound);
    }

    let versions = store::history(&state.pool, tag_id, user.id)
        .await?
        .into_iter()
        .map(SummaryVersion::from)
        .collect();

    Ok(Json(SummaryHistory { versions }))
}
