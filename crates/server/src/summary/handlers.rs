//! HTTP handlers for guided summary generation, history, and review (tickets
//! T7 and T8): a generated version is a proposal until it is approved or
//! discarded, and rewriting it records a new `edited` version.
//!
//! Both routes scope by `WHERE id = $1 AND user_id = $2` through
//! `store::tag_exists_for_user`, so a foreign tag 404s exactly like every
//! other handler in this server (see `api::tag_detail`).

use axum::Json;
use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use strands_api_types::{
    EditSummaryRequest, GenerateSummaryRequest, MAX_SUMMARY_BODY_CHARS, SummaryHistory,
    SummaryVersion,
};
use strands_core::{SummaryState, TagId, UserId};
use uuid::Uuid;

use super::store;
use super::{build_prompt, compute_source_hash, guide_from_columns};
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
        .route(
            "/api/v1/tags/{tag_id}/summary/{summary_id}/approve",
            post(approve_summary),
        )
        .route(
            "/api/v1/tags/{tag_id}/summary/{summary_id}/discard",
            post(discard_summary),
        )
        .route(
            "/api/v1/tags/{tag_id}/summary/{summary_id}/edit",
            post(edit_summary),
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
            state: SummaryState::Proposed,
        },
    )
    .await?;

    Ok(Json(version.into()))
}

/// `GET /api/v1/tags/{id}/summary/history`: every version, newest first, plus
/// the version currently in force.
pub async fn summary_history(
    State(state): State<AppState>,
    user: CurrentUser,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<SummaryHistory>, AppError> {
    let Path(tag_id) = path.map_err(|_| AppError::BadRequest("invalid tag id".into()))?;

    if !store::tag_exists_for_user(&state.pool, tag_id, user.id).await? {
        return Err(AppError::NotFound);
    }

    Ok(Json(review_state(&state, tag_id, user.id).await?))
}

/// `POST /api/v1/tags/{id}/summary/{version}/approve`: commit a proposal. The
/// version becomes the tag's summary and survives reload; a second approval is
/// a no-op rather than a conflict.
pub async fn approve_summary(
    State(state): State<AppState>,
    user: CurrentUser,
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> Result<Json<SummaryHistory>, AppError> {
    let (tag_id, version) = owned_version(&state, user.id, path).await?;

    if version.state == SummaryState::Discarded {
        return Err(AppError::BadRequest(
            "a discarded version cannot be approved".into(),
        ));
    }

    if !store::approve(&state.pool, tag_id, user.id, version.id).await? {
        return Err(AppError::NotFound);
    }

    Ok(Json(review_state(&state, tag_id, user.id).await?))
}

/// `POST /api/v1/tags/{id}/summary/{version}/discard`: drop a proposal. The
/// version stays in history marked `discarded`; if it was the approved one the
/// tag is left with no summary, so the card falls back to "nothing approved".
pub async fn discard_summary(
    State(state): State<AppState>,
    user: CurrentUser,
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> Result<Json<SummaryHistory>, AppError> {
    let (tag_id, version) = owned_version(&state, user.id, path).await?;

    if !store::discard(&state.pool, tag_id, user.id, version.id).await? {
        return Err(AppError::NotFound);
    }

    Ok(Json(review_state(&state, tag_id, user.id).await?))
}

/// `POST /api/v1/tags/{id}/summary/{version}/edit`: rewrite a proposal. The
/// rewrite is a *new* version of kind `edited`; the version it was based on
/// keeps its own body and state, which is what makes the history auditable.
pub async fn edit_summary(
    State(state): State<AppState>,
    user: CurrentUser,
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
    Json(request): Json<EditSummaryRequest>,
) -> Result<Json<SummaryHistory>, AppError> {
    let (tag_id, version) = owned_version(&state, user.id, path).await?;

    let body = request.body.trim();
    if body.is_empty() {
        return Err(AppError::BadRequest("summary body cannot be empty".into()));
    }
    if body.chars().count() > MAX_SUMMARY_BODY_CHARS {
        return Err(AppError::BadRequest(format!(
            "summary body cannot exceed {MAX_SUMMARY_BODY_CHARS} characters"
        )));
    }

    let guide = guide_from_columns(version.preset, version.guide_text.clone());

    store::insert_version(
        &state.pool,
        store::NewVersion {
            tag_id,
            user_id: user.id,
            guide: &guide,
            body,
            // An edit keeps the alias of the version it rewrote: that is where
            // the text came from, and the history line stays truthful about it.
            model_alias: &version.model_alias,
            source_hash: &version.source_hash,
            state: SummaryState::Edited,
        },
    )
    .await?;

    Ok(Json(review_state(&state, tag_id, user.id).await?))
}

/// The current history plus the version in force — the card's whole review
/// surface, so each action can answer with the state it just produced.
async fn review_state(
    state: &AppState,
    tag_id: TagId,
    user_id: UserId,
) -> Result<SummaryHistory, AppError> {
    let versions = store::history(&state.pool, tag_id, user_id)
        .await?
        .into_iter()
        .map(SummaryVersion::from)
        .collect();
    let approved_version_id = store::approved_version_id(&state.pool, tag_id, user_id).await?;

    Ok(SummaryHistory {
        versions,
        approved_version_id,
    })
}

/// Parse `(tag_id, summary_id)` from the path and load the version, scoped to
/// the caller. A foreign tag, a foreign version, and a version under a
/// different tag all answer 404 — the same answer, so nothing leaks.
async fn owned_version(
    state: &AppState,
    user_id: UserId,
    path: Result<Path<(Uuid, Uuid)>, PathRejection>,
) -> Result<(TagId, store::SummaryVersionRow), AppError> {
    let Path((tag_id, summary_id)) =
        path.map_err(|_| AppError::BadRequest("invalid summary id".into()))?;

    store::version_for_tag(&state.pool, tag_id, user_id, summary_id)
        .await?
        .map(|version| (tag_id, version))
        .ok_or(AppError::NotFound)
}
