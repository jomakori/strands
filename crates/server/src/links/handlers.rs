//! HTTP surface for links (ticket T10).
//!
//! `GET /api/v1/links` is the constellation's read: every judged pair, with
//! both tag names. `POST /api/v1/links/judge` re-runs judging over the current
//! evidence and reports what it changed. Both are user-scoped through
//! [`CurrentUser`]; the client never supplies a user id.

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::{get, post};
use chrono::Utc;
use strands_api_types::{JudgeLinksResponse, LinkSummary};

use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;

use super::{find_candidates, judge_batches, list_links, upsert_judgements};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/links", get(links))
        .route("/api/v1/links/judge", post(judge_links))
}

/// The caller's constellation: every judged pair, strongest first.
pub async fn links(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Vec<LinkSummary>>, AppError> {
    let rows = list_links(&state.pool, user.id).await?;

    Ok(Json(
        rows.into_iter()
            .map(|link| LinkSummary {
                id: link.id,
                a_tag_id: link.a_tag_id,
                a_tag_name: link.a_tag_name,
                b_tag_id: link.b_tag_id,
                b_tag_name: link.b_tag_name,
                score: link.score,
                rationale: link.rationale,
                judged_at: link.judged_at,
            })
            .collect(),
    ))
}

/// Judge every current candidate pair, then persist in one transaction.
///
/// No candidates is a successful empty run (`judged: 0`), not an error: a user
/// with one tag has nothing to compare, and the constellation renders its
/// empty state. A model failure writes nothing and answers 500.
pub async fn judge_links(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<JudgeLinksResponse>, AppError> {
    let candidates = find_candidates(&state.pool, user.id).await?;
    if candidates.is_empty() {
        return Ok(Json(JudgeLinksResponse {
            judged: 0,
            created: 0,
            updated: 0,
        }));
    }

    let judgements = judge_batches(state.judge.as_ref(), &candidates).await?;

    let mut tx = state.pool.begin().await?;
    let outcome = upsert_judgements(&mut tx, user.id, &candidates, &judgements, Utc::now()).await?;
    tx.commit().await?;

    Ok(Json(JudgeLinksResponse {
        judged: candidates.len() as u32,
        created: outcome.created as u32,
        updated: outcome.updated as u32,
    }))
}
