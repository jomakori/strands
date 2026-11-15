//! User-scoped API surface (tickets T3, T4, T12).
//!
//! T3 landed `/api/v1/me` and the tag-detail probe that proves the isolation
//! rule. T4 grows the surface the home grid and detail view need (tag list,
//! tag detail, thread list, thread detail), keeping the same rule throughout:
//! the user id comes from [`CurrentUser`] and every query binds it, so "not
//! mine" takes the exact same path as "does not exist" — a 404, never a 403.
//! T12 adds the two writes quick capture makes and the category list it files a
//! brand-new tag under; those writes take the user id from the same extractor,
//! so a capture can only ever land in the caller's own rows.

use axum::Json;
use axum::Router;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use strands_api_types::{
    CategorySummary, NewTag, NewThread, Profile, TagDetail, TagSummary, ThreadDetail, ThreadSummary,
};
use strands_core::{Note, NoteKind, Source, Tag};
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::auth::store;
use crate::error::AppError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/me", get(me))
        .route("/api/v1/categories", get(list_categories))
        .route("/api/v1/tags", get(list_tags).post(create_tag))
        .route("/api/v1/tags/{tag_id}", get(tag_detail))
        .route("/api/v1/tags/{tag_id}/threads", get(list_threads))
        .route("/api/v1/threads", post(create_thread))
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

/// Every category the caller owns, in the order the seed laid them down.
///
/// Quick capture needs the ids to file a brand-new tag; nothing else reads
/// this yet, so it stays a plain list rather than a nested tag tree.
pub async fn list_categories(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Vec<CategorySummary>>, AppError> {
    let rows: Vec<(Uuid, String, String, i32)> = sqlx::query_as(
        "SELECT id, name, color, position FROM categories WHERE user_id = $1 ORDER BY position",
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;

    let categories = rows
        .into_iter()
        .map(|(id, name, color, position)| CategorySummary {
            id,
            name,
            color,
            position,
        })
        .collect();

    Ok(Json(categories))
}

/// Create a tag the caller just typed in quick capture.
///
/// The category is checked with `AND user_id = $2` before the insert, so a
/// foreign category id answers 404 instead of writing a tag that would point
/// at somebody else's row.
pub async fn create_tag(
    State(state): State<AppState>,
    user: CurrentUser,
    payload: Result<Json<NewTag>, JsonRejection>,
) -> Result<(StatusCode, Json<TagSummary>), AppError> {
    let Json(request) = payload.map_err(|error| AppError::BadRequest(error.body_text()))?;
    // Same validation the rest of the domain uses: a blank name is a 400.
    let tag = Tag::new(request.name)?;

    let owned: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM categories WHERE id = $1 AND user_id = $2")
            .bind(request.category_id)
            .bind(user.id)
            .fetch_optional(&state.pool)
            .await?;
    owned.ok_or(AppError::NotFound)?;

    let created_at: DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO tags (id, user_id, name, category_id) VALUES ($1, $2, $3, $4) \
         RETURNING created_at",
    )
    .bind(tag.id())
    .bind(user.id)
    .bind(tag.name())
    .bind(request.category_id)
    .fetch_one(&state.pool)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(TagSummary {
            id: tag.id(),
            name: tag.name().to_owned(),
            category_id: request.category_id,
            thread_count: 0,
            created_at,
        }),
    ))
}

/// Capture one note: a thread and its first note, written in one transaction.
///
/// The title is the note's own opening line, so an empty title is impossible.
/// Without a `tag_id` the note lands in the caller's `Inbox` (created on first
/// use) — a capture never waits on the user choosing a tag.
pub async fn create_thread(
    State(state): State<AppState>,
    user: CurrentUser,
    payload: Result<Json<NewThread>, JsonRejection>,
) -> Result<(StatusCode, Json<ThreadDetail>), AppError> {
    let Json(request) = payload.map_err(|error| AppError::BadRequest(error.body_text()))?;
    let note = Note::capture(request.body)?;

    let mut tx = state.pool.begin().await?;

    let tag_id = match request.tag_id {
        Some(tag_id) => {
            let owned: Option<Uuid> =
                sqlx::query_scalar("SELECT id FROM tags WHERE id = $1 AND user_id = $2")
                    .bind(tag_id)
                    .bind(user.id)
                    .fetch_optional(&mut *tx)
                    .await?;
            owned.ok_or(AppError::NotFound)?
        }
        None => inbox_tag(&mut tx, user.id).await?,
    };

    let thread_id = Uuid::new_v4();
    let title = note.title();
    let updated_at: DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO threads (id, user_id, tag_id, title, source_count) \
         VALUES ($1, $2, $3, $4, 1) RETURNING updated_at",
    )
    .bind(thread_id)
    .bind(user.id)
    .bind(tag_id)
    .bind(&title)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO notes \
         (id, user_id, tag_id, thread_id, kind, source, body, occurred_at, dedupe_key) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, now(), $8)",
    )
    .bind(note.id())
    .bind(user.id)
    .bind(tag_id)
    .bind(thread_id)
    .bind(NoteKind::Text)
    .bind(Source::Manual)
    .bind(note.body())
    // A captured note has no upstream identity; its own id is the dedupe key.
    .bind(note.id().to_string())
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(ThreadDetail {
            id: thread_id,
            tag_id,
            title,
            source_count: 1,
            updated_at,
        }),
    ))
}

/// The caller's `Inbox` tag, created in their first category the first time a
/// capture arrives without one.
async fn inbox_tag(conn: &mut PgConnection, user_id: Uuid) -> Result<Uuid, AppError> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM tags WHERE user_id = $1 AND name = 'Inbox' ORDER BY created_at LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(id) = existing {
        return Ok(id);
    }

    let category: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM categories WHERE user_id = $1 ORDER BY position LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(&mut *conn)
    .await?;
    // Every user is seeded with categories at sign-in (T3), so this only fires
    // for an account whose categories were deleted by hand.
    let category_id = category
        .ok_or_else(|| AppError::BadRequest("no category to file a new tag under".into()))?;

    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO tags (id, user_id, name, category_id) VALUES ($1, $2, 'Inbox', $3)")
        .bind(id)
        .bind(user_id)
        .bind(category_id)
        .execute(&mut *conn)
        .await?;

    Ok(id)
}
