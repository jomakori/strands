//! User-scoped API surface (tickets T3, T4).
//!
//! T3 landed `/api/v1/me` and the tag-detail probe that proves the isolation
//! rule. T4 grows the surface the home grid and detail view need (tag list,
//! tag detail, thread list, thread detail), keeping the same rule throughout:
//! the user id comes from [`CurrentUser`] and every query binds it, so "not
//! mine" takes the exact same path as "does not exist" — a 404, never a 403.

use axum::Json;
use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::routing::get;
use chrono::{DateTime, Utc};
use strands_api_types::{Profile, TagDetail, TagSummary, ThreadDetail, ThreadSummary};
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::auth::store;
use crate::error::AppError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/me", get(me))
        .route("/api/v1/tags", get(list_tags))
        .route("/api/v1/tags/{tag_id}", get(tag_detail))
        .route("/api/v1/tags/{tag_id}/threads", get(list_threads))
        .route("/api/v1/threads/{thread_id}", get(thread_detail))
}

/// The signed-in user's own profile.
pub async fn me(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Profile>, AppError> {
    let profile = store::find_profile(&state.pool, user.id)
        .await?
        // The session points at a user that is gone (deleted between the
        // extractor and this query): treat it as signed out.
        .ok_or(AppError::Unauthenticated)?;

    Ok(Json(Profile {
        id: profile.id,
        email: profile.email,
        name: profile.name,
        avatar_url: profile.avatar_url,
    }))
}

/// Every tag the caller owns, most recently created first.
///
/// `thread_count` is computed with a `LEFT JOIN`, not stored, so it can never
/// drift from the rows it is counting.
pub async fn list_tags(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Vec<TagSummary>>, AppError> {
    let rows: Vec<(Uuid, String, Uuid, DateTime<Utc>, i64)> = sqlx::query_as(
        "SELECT t.id, t.name, t.category_id, t.created_at, COUNT(th.id) \
         FROM tags t \
         LEFT JOIN threads th ON th.tag_id = t.id AND th.user_id = t.user_id \
         WHERE t.user_id = $1 \
         GROUP BY t.id, t.name, t.category_id, t.created_at \
         ORDER BY t.created_at DESC",
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;

    let tags = rows
        .into_iter()
        .map(
            |(id, name, category_id, created_at, thread_count)| TagSummary {
                id,
                name,
                category_id,
                thread_count,
                created_at,
            },
        )
        .collect();

    Ok(Json(tags))
}

/// One tag, if and only if it belongs to the caller.
///
/// `WHERE id = $1 AND user_id = $2` — not "fetch then compare" — so another
/// user's id and a non-existent id take the exact same path to 404. Answering
/// 403 for "exists but not yours" would confirm the id is real.
pub async fn tag_detail(
    State(state): State<AppState>,
    user: CurrentUser,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<TagDetail>, AppError> {
    let Path(tag_id) = path.map_err(|_| AppError::BadRequest("invalid tag id".into()))?;

    let row: Option<(Uuid, String, Uuid, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, name, category_id, created_at FROM tags WHERE id = $1 AND user_id = $2",
    )
    .bind(tag_id)
    .bind(user.id)
    .fetch_optional(&state.pool)
    .await?;

    let (id, name, category_id, created_at) = row.ok_or(AppError::NotFound)?;

    Ok(Json(TagDetail {
        id,
        name,
        category_id,
        created_at,
    }))
}

/// Threads under one tag, if and only if the tag belongs to the caller.
///
/// The ownership check runs before the list query: a foreign `tag_id` must
/// answer 404, not an empty list — an empty list would let a caller fish for
/// which ids exist by checking `[]` versus 404.
pub async fn list_threads(
    State(state): State<AppState>,
    user: CurrentUser,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Vec<ThreadSummary>>, AppError> {
    let Path(tag_id) = path.map_err(|_| AppError::BadRequest("invalid tag id".into()))?;

    let owned: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM tags WHERE id = $1 AND user_id = $2")
            .bind(tag_id)
            .bind(user.id)
            .fetch_optional(&state.pool)
            .await?;
    owned.ok_or(AppError::NotFound)?;

    let rows: Vec<(Uuid, Uuid, String, i32, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, tag_id, title, source_count, updated_at \
         FROM threads WHERE tag_id = $1 AND user_id = $2 \
         ORDER BY updated_at DESC",
    )
    .bind(tag_id)
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;

    let threads = rows
        .into_iter()
        .map(
            |(id, tag_id, title, source_count, updated_at)| ThreadSummary {
                id,
                tag_id,
                title,
                source_count,
                updated_at,
            },
        )
        .collect();

    Ok(Json(threads))
}

/// One thread, if and only if it belongs to the caller.
pub async fn thread_detail(
    State(state): State<AppState>,
    user: CurrentUser,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<ThreadDetail>, AppError> {
    let Path(thread_id) = path.map_err(|_| AppError::BadRequest("invalid thread id".into()))?;

    let row: Option<(Uuid, Uuid, String, i32, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, tag_id, title, source_count, updated_at \
         FROM threads WHERE id = $1 AND user_id = $2",
    )
    .bind(thread_id)
    .bind(user.id)
    .fetch_optional(&state.pool)
    .await?;

    let (id, tag_id, title, source_count, updated_at) = row.ok_or(AppError::NotFound)?;

    Ok(Json(ThreadDetail {
        id,
        tag_id,
        title,
        source_count,
        updated_at,
    }))
}
