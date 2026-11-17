//! Persistence for judged links (ticket T10).
//!
//! Writes are keyed on the ordered pair `(a_tag_id, b_tag_id)` in canonical
//! order, which is exactly the unique constraint the schema already carries.
//! The upsert refreshes `score`, `rationale` and `judged_at`, so judging the
//! same pair again corrects the row instead of adding a second one.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use strands_core::{TagId, UserId};
use uuid::Uuid;

use super::{CandidatePair, PairJudgement, canonical_pair};

/// One row of `GET /api/v1/links`, with both tag names for rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgedLink {
    pub id: Uuid,
    pub a_tag_id: TagId,
    pub a_tag_name: String,
    pub b_tag_id: TagId,
    pub b_tag_name: String,
    pub score: u8,
    pub rationale: String,
    pub judged_at: DateTime<Utc>,
}

/// What one judge run did to the table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UpsertOutcome {
    pub created: usize,
    pub updated: usize,
}

/// One joined row from [`list_links`]: the link, both tag names, and both ids.
type LinkRow = (
    Uuid,
    TagId,
    String,
    TagId,
    String,
    i32,
    String,
    DateTime<Utc>,
);

/// Every judged link for one user, strongest first.
pub async fn list_links(pool: &PgPool, user_id: UserId) -> Result<Vec<JudgedLink>, sqlx::Error> {
    let rows: Vec<LinkRow> = sqlx::query_as(
        "SELECT l.id, l.a_tag_id, ta.name, l.b_tag_id, tb.name, l.score, l.rationale, l.judged_at \
         FROM links l \
         JOIN tags ta ON ta.id = l.a_tag_id AND ta.user_id = l.user_id \
         JOIN tags tb ON tb.id = l.b_tag_id AND tb.user_id = l.user_id \
         WHERE l.user_id = $1 \
         ORDER BY l.score DESC, l.judged_at DESC, l.id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(
            |(id, a_tag_id, a_tag_name, b_tag_id, b_tag_name, score, rationale, judged_at)| {
                JudgedLink {
                    id,
                    a_tag_id,
                    a_tag_name,
                    b_tag_id,
                    b_tag_name,
                    score: score.clamp(0, 100) as u8,
                    rationale,
                    judged_at,
                }
            },
        )
        .collect())
}

/// Write one judgement per candidate, inside the caller's transaction so a
/// failed run leaves the previous constellation intact.
///
/// Pairs are re-canonicalized here rather than trusted from the query: the
/// `links_canonical_pair_order` check would reject a reversed pair, and the
/// unique constraint would not fire on it either.
pub async fn upsert_judgements(
    tx: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    candidates: &[CandidatePair],
    judgements: &[PairJudgement],
    judged_at: DateTime<Utc>,
) -> Result<UpsertOutcome, sqlx::Error> {
    let mut outcome = UpsertOutcome::default();

    for (candidate, judgement) in candidates.iter().zip(judgements) {
        let (a_tag_id, b_tag_id) = canonical_pair(candidate.a_tag_id, candidate.b_tag_id);

        let (inserted,): (bool,) = sqlx::query_as(
            "INSERT INTO links (id, user_id, a_tag_id, b_tag_id, score, rationale, judged_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (a_tag_id, b_tag_id) DO UPDATE \
             SET score = EXCLUDED.score, \
                 rationale = EXCLUDED.rationale, \
                 judged_at = EXCLUDED.judged_at \
             RETURNING (xmax = 0) AS inserted",
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .bind(a_tag_id)
        .bind(b_tag_id)
        .bind(i32::from(judgement.score))
        .bind(&judgement.rationale)
        .bind(judged_at)
        .fetch_one(&mut **tx)
        .await?;

        if inserted {
            outcome.created += 1;
        } else {
            outcome.updated += 1;
        }
    }

    Ok(outcome)
}
