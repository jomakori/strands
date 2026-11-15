//! `GET /api/v1/search` (ticket T11, spec §7.1 search): full-text search over
//! a thread's own title and the notes filed under it, ranked by `ts_rank` and
//! scoped to the caller at query time — the generated `tsvector` columns
//! themselves carry no user id (migrations 0001, 0003), so every query here
//! binds `user_id` rather than relying on the index to isolate rows.
//!
//! A blank `q` skips full-text matching entirely and answers the caller's 5
//! most recently updated threads instead (the search sheet's "recents" list),
//! because `websearch_to_tsquery('')` matches nothing and would otherwise
//! look identical to a real zero-result search.

use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::routing::get;
use serde::Deserialize;
use sqlx::PgPool;
use strands_api_types::SearchResult;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;

/// Ticket acceptance: results are capped at 20.
const RESULT_LIMIT: i64 = 20;
/// Ticket acceptance: an empty query shows the last 5 recents.
const RECENT_LIMIT: i64 = 5;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/search", get(search))
}

#[derive(Debug, Deserialize)]
pub struct SearchParams {
    #[serde(default)]
    q: String,
}

pub async fn search(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(params): Query<SearchParams>,
) -> Result<Json<Vec<SearchResult>>, AppError> {
    let query = params.q.trim();
    let rows = if query.is_empty() {
        recent_threads(&state.pool, user.id).await?
    } else {
        search_threads(&state.pool, user.id, query).await?
    };

    Ok(Json(rows))
}

/// The caller's own threads, most recently updated first.
async fn recent_threads(pool: &PgPool, user_id: Uuid) -> Result<Vec<SearchResult>, AppError> {
    let rows: Vec<(Uuid, String, Uuid, String)> = sqlx::query_as(
        "SELECT t.id, t.title, t.tag_id, tg.name \
         FROM threads t \
         JOIN tags tg ON tg.id = t.tag_id AND tg.user_id = $1 \
         WHERE t.user_id = $1 \
         ORDER BY t.updated_at DESC \
         LIMIT $2",
    )
    .bind(user_id)
    .bind(RECENT_LIMIT)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, title, tag_id, tag_name)| SearchResult {
            id,
            snippet: title.clone(),
            title,
            tag_id,
            tag_name,
        })
        .collect())
}

/// Matches a thread by its own title or by a note filed under it, keeping
/// only the best-ranked row per thread so a thread with several matching
/// notes still surfaces once.
async fn search_threads(
    pool: &PgPool,
    user_id: Uuid,
    query: &str,
) -> Result<Vec<SearchResult>, AppError> {
    let rows: Vec<(Uuid, String, String, Uuid, String)> = sqlx::query_as(
        "WITH q AS ( \
            SELECT websearch_to_tsquery('english', $2) AS tsq \
         ), \
         matches AS ( \
            SELECT t.id AS thread_id, t.tag_id, t.title, t.title AS snippet, \
                   ts_rank(t.title_tsv, q.tsq) AS rank \
            FROM threads t \
            CROSS JOIN q \
            WHERE t.user_id = $1 AND t.title_tsv @@ q.tsq \
            UNION ALL \
            SELECT t.id AS thread_id, t.tag_id, t.title, \
                   -- ts_headline marks hits with <b>…</b>; the sheet renders plain text, so
                   -- strip the markers instead of passing an empty StartSel/StopSel, which
                   -- PostgreSQL does not accept (it swallows the next option name).
                   regexp_replace( \
                       ts_headline('english', n.body, q.tsq, \
                           'MaxFragments=1,MaxWords=24,MinWords=12'), \
                       '</?b>', '', 'g') AS snippet, \
                   ts_rank(n.body_tsv, q.tsq) AS rank \
            FROM notes n \
            JOIN threads t ON t.id = n.thread_id AND t.user_id = n.user_id \
            CROSS JOIN q \
            WHERE n.user_id = $1 AND n.body_tsv @@ q.tsq \
         ), \
         best AS ( \
            SELECT DISTINCT ON (thread_id) thread_id, tag_id, title, snippet, rank \
            FROM matches \
            ORDER BY thread_id, rank DESC \
         ) \
         SELECT b.thread_id, b.title, b.snippet, b.tag_id, tg.name \
         FROM best b \
         JOIN tags tg ON tg.id = b.tag_id AND tg.user_id = $1 \
         ORDER BY b.rank DESC \
         LIMIT $3",
    )
    .bind(user_id)
    .bind(query)
    .bind(RESULT_LIMIT)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, title, snippet, tag_id, tag_name)| SearchResult {
            id,
            title,
            snippet,
            tag_id,
            tag_name,
        })
        .collect())
}
