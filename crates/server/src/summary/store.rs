//! Every SQL statement summary generation runs (ticket T7), mirroring how
//! `auth::store` is the only place that touches its own tables.
//!
//! Runtime-checked `sqlx` calls only, so building this crate needs no
//! `DATABASE_URL`. The user id is always a parameter, never assumed, so every
//! query here is auditable the same way `auth::store`'s are.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use strands_api_types::SummaryVersion;
use strands_core::{SummaryPreset, TagId, UserId};
use uuid::Uuid;

use super::{guide_columns, guide_from_columns};
use strands_api_types::SummaryGuide;

/// How many versions a tag keeps; older ones are pruned on every insert.
const MAX_VERSIONS_PER_TAG: i64 = 20;

/// One `summaries` row, before it is turned into the wire [`SummaryVersion`].
/// Carries `source_hash`, which the DTO deliberately does not expose.
pub struct SummaryVersionRow {
    pub id: Uuid,
    pub tag_id: TagId,
    pub version: i32,
    pub body: String,
    pub model_alias: String,
    pub preset: Option<SummaryPreset>,
    pub guide_text: Option<String>,
    pub source_hash: String,
    pub created_at: DateTime<Utc>,
}

impl From<SummaryVersionRow> for SummaryVersion {
    fn from(row: SummaryVersionRow) -> Self {
        SummaryVersion {
            id: row.id,
            tag_id: row.tag_id,
            version: row.version,
            body: row.body,
            guide: guide_from_columns(row.preset, row.guide_text),
            model_alias: row.model_alias,
            created_at: row.created_at,
        }
    }
}

/// The column tuple a `summaries` SELECT returns, in the order [`row_from_tuple`]
/// unpacks it.
type SummaryRowTuple = (
    Uuid,
    Uuid,
    i32,
    String,
    String,
    Option<SummaryPreset>,
    Option<String>,
    String,
    DateTime<Utc>,
);

/// What [`insert_version`] needs to write one new row.
pub struct NewVersion<'a> {
    pub tag_id: TagId,
    pub user_id: UserId,
    pub guide: &'a SummaryGuide,
    pub body: &'a str,
    pub model_alias: &'a str,
    pub source_hash: &'a str,
}

/// True if and only if `tag_id` belongs to `user_id` — the ownership check a
/// handler runs before touching anything else under that tag (spec: "not
/// yours" and "does not exist" must take the same 404 path).
pub async fn tag_exists_for_user(
    pool: &PgPool,
    tag_id: TagId,
    user_id: UserId,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM tags WHERE id = $1 AND user_id = $2)")
        .bind(tag_id)
        .bind(user_id)
        .fetch_one(pool)
        .await
}

/// Every note body under a tag, in a stable order so the same notes always
/// hash identically (the `id` tiebreaker makes the order deterministic even
/// when several notes share an `occurred_at`).
pub async fn fetch_note_bodies(
    pool: &PgPool,
    tag_id: TagId,
    user_id: UserId,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT body FROM notes WHERE tag_id = $1 AND user_id = $2 \
         ORDER BY occurred_at ASC, id ASC",
    )
    .bind(tag_id)
    .bind(user_id)
    .fetch_all(pool)
    .await
}

/// The highest-numbered version for a tag, if any exists yet.
pub async fn newest_version(
    pool: &PgPool,
    tag_id: TagId,
    user_id: UserId,
) -> Result<Option<SummaryVersionRow>, sqlx::Error> {
    let row: Option<SummaryRowTuple> = sqlx::query_as(
        "SELECT id, tag_id, version, body, model_alias, preset, guide, source_hash, created_at \
         FROM summaries WHERE tag_id = $1 AND user_id = $2 \
         ORDER BY version DESC LIMIT 1",
    )
    .bind(tag_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(row_from_tuple))
}

/// Every version for a tag, newest first (spec acceptance: "history lists
/// versions newest first").
pub async fn history(
    pool: &PgPool,
    tag_id: TagId,
    user_id: UserId,
) -> Result<Vec<SummaryVersionRow>, sqlx::Error> {
    let rows: Vec<SummaryRowTuple> = sqlx::query_as(
        "SELECT id, tag_id, version, body, model_alias, preset, guide, source_hash, created_at \
         FROM summaries WHERE tag_id = $1 AND user_id = $2 \
         ORDER BY version DESC",
    )
    .bind(tag_id)
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_from_tuple).collect())
}

/// Insert the next version for a tag and prune anything past the newest
/// [`MAX_VERSIONS_PER_TAG`], all in one transaction so a reader never sees a
/// tag with more than the kept history, even momentarily.
pub async fn insert_version(
    pool: &PgPool,
    new: NewVersion<'_>,
) -> Result<SummaryVersionRow, sqlx::Error> {
    let mut tx = pool.begin().await?;

    let (preset, guide_text) = guide_columns(new.guide);

    let next_version: i32 =
        sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) + 1 FROM summaries WHERE tag_id = $1")
            .bind(new.tag_id)
            .fetch_one(&mut *tx)
            .await?;

    let id = Uuid::new_v4();
    let (created_at,): (DateTime<Utc>,) = sqlx::query_as(
        "INSERT INTO summaries \
            (id, user_id, tag_id, version, body, model_alias, guide, preset, source_hash) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         RETURNING created_at",
    )
    .bind(id)
    .bind(new.user_id)
    .bind(new.tag_id)
    .bind(next_version)
    .bind(new.body)
    .bind(new.model_alias)
    .bind(&guide_text)
    .bind(preset)
    .bind(new.source_hash)
    .fetch_one(&mut *tx)
    .await?;

    // Keep only the newest MAX_VERSIONS_PER_TAG rows for this tag.
    sqlx::query(
        "DELETE FROM summaries WHERE tag_id = $1 AND id NOT IN ( \
            SELECT id FROM summaries WHERE tag_id = $1 ORDER BY version DESC LIMIT $2 \
         )",
    )
    .bind(new.tag_id)
    .bind(MAX_VERSIONS_PER_TAG)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(SummaryVersionRow {
        id,
        tag_id: new.tag_id,
        version: next_version,
        body: new.body.to_owned(),
        model_alias: new.model_alias.to_owned(),
        preset,
        guide_text,
        source_hash: new.source_hash.to_owned(),
        created_at,
    })
}

fn row_from_tuple(row: SummaryRowTuple) -> SummaryVersionRow {
    let (id, tag_id, version, body, model_alias, preset, guide_text, source_hash, created_at) = row;
    SummaryVersionRow {
        id,
        tag_id,
        version,
        body,
        model_alias,
        preset,
        guide_text,
        source_hash,
        created_at,
    }
}
