//! Every SQL statement data controls run (ticket T17).
//!
//! Two shapes only: read everything the caller owns, or delete it. Both take
//! the user id as a parameter — there is no ambient "current user" — so every
//! statement is scoped by construction and an export can never widen to a
//! second account.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use strands_api_types::{
    AccountExport, EXPORT_FORMAT_VERSION, ExportLink, ExportList, ExportNote, ExportPlace,
    ExportSummary, ExportTag, ExportThread, SummaryVersion,
};
use strands_core::UserId;
use uuid::Uuid;

use crate::auth::store as auth_store;
use crate::summary::store::SummaryVersionRow;
use strands_core::{SummaryPreset, SummaryState};

/// The caller's whole dataset, or `None` when the session points at a user row
/// that no longer exists (a deleted account still holding a stale cookie).
pub async fn export(pool: &PgPool, user_id: UserId) -> Result<Option<AccountExport>, sqlx::Error> {
    let Some(profile) = auth_store::find_profile(pool, user_id).await? else {
        return Ok(None);
    };

    Ok(Some(AccountExport {
        format_version: EXPORT_FORMAT_VERSION,
        exported_at: Utc::now(),
        profile: strands_api_types::Profile {
            id: profile.id,
            email: profile.email,
            name: profile.name,
            avatar_url: profile.avatar_url,
            provider: profile.provider,
        },
        tags: tags(pool, user_id).await?,
        threads: threads(pool, user_id).await?,
        notes: notes(pool, user_id).await?,
        summaries: summaries(pool, user_id).await?,
        summary_versions: summary_versions(pool, user_id).await?,
        links: links(pool, user_id).await?,
        lists: lists(pool, user_id).await?,
        places: places(pool, user_id).await?,
    }))
}

async fn tags(pool: &PgPool, user_id: UserId) -> Result<Vec<ExportTag>, sqlx::Error> {
    let rows: Vec<(Uuid, String, Uuid, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, name, category_id, created_at FROM tags WHERE user_id = $1 \
         ORDER BY created_at, id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, name, category_id, created_at)| ExportTag {
            id,
            name,
            category_id,
            created_at,
        })
        .collect())
}

async fn threads(pool: &PgPool, user_id: UserId) -> Result<Vec<ExportThread>, sqlx::Error> {
    let rows: Vec<(Uuid, Uuid, String, i32, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, tag_id, title, source_count, updated_at FROM threads WHERE user_id = $1 \
         ORDER BY updated_at DESC, id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(
            |(id, tag_id, title, source_count, updated_at)| ExportThread {
                id,
                tag_id,
                title,
                source_count,
                updated_at,
            },
        )
        .collect())
}

async fn notes(pool: &PgPool, user_id: UserId) -> Result<Vec<ExportNote>, sqlx::Error> {
    /// `(id, tag_id, thread_id, kind, body, occurred_at, source, created_at)`.
    type NoteRow = (
        Uuid,
        Uuid,
        Option<Uuid>,
        String,
        String,
        DateTime<Utc>,
        String,
        DateTime<Utc>,
    );

    // The two PG enums are cast to text: the export is a document, so it should
    // carry the values a reader sees, not a type name this build happens to map.
    let rows: Vec<NoteRow> = sqlx::query_as(
        "SELECT id, tag_id, thread_id, kind::text, body, occurred_at, source::text, created_at \
         FROM notes WHERE user_id = $1 ORDER BY occurred_at, id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(
            |(id, tag_id, thread_id, kind, body, occurred_at, source, created_at)| ExportNote {
                id,
                tag_id,
                thread_id,
                kind,
                body,
                occurred_at,
                source,
                created_at,
            },
        )
        .collect())
}

/// One entry per tag that has an approved summary — "the summary in force".
async fn summaries(pool: &PgPool, user_id: UserId) -> Result<Vec<ExportSummary>, sqlx::Error> {
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, summary_approved_id FROM tags \
         WHERE user_id = $1 AND summary_approved_id IS NOT NULL ORDER BY created_at, id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(tag_id, version_id)| ExportSummary { tag_id, version_id })
        .collect())
}

async fn summary_versions(
    pool: &PgPool,
    user_id: UserId,
) -> Result<Vec<SummaryVersion>, sqlx::Error> {
    type Row = (
        Uuid,
        Uuid,
        i32,
        String,
        String,
        Option<SummaryPreset>,
        Option<String>,
        String,
        SummaryState,
        DateTime<Utc>,
    );

    // Same column order as the tag-scoped history query, minus the WHERE on a
    // single tag: the export spans every tag the caller owns.
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, tag_id, version, body, model_alias, preset, guide, source_hash, state, \
                created_at \
         FROM summaries WHERE user_id = $1 ORDER BY tag_id, version",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                tag_id,
                version,
                body,
                model_alias,
                preset,
                guide_text,
                source_hash,
                state,
                created_at,
            )| {
                SummaryVersion::from(SummaryVersionRow {
                    id,
                    tag_id,
                    version,
                    body,
                    model_alias,
                    preset,
                    guide_text,
                    source_hash,
                    state,
                    created_at,
                })
            },
        )
        .collect())
}

/// Erase everything the caller captured, inside one transaction.
///
/// Order is children-first so no statement depends on cascade for a row it can
/// name itself — a truncated order here is a foreign-key error, not silent data
/// loss. Table by table:
///
/// * `summary_notes`, `links`, `notes` — leaves that point at tags/summaries;
/// * `summaries` next, because `tags.summary_approved_id` references them (the
///   FK nulls out, which is why tags cannot go first);
/// * `threads` before `tags` (threads point at tags);
/// * `places` and `lists` are place-list imports, not tag content;
/// * `jobs` so no queued job wakes up holding a deleted id;
/// * `import_runs` last of the connector bookkeeping (lists pointed at it);
/// * `tags` closes the graph.
///
/// Kept: the `users` row and its sessions (the caller stays signed in and can
/// start over), the seeded `categories` (configuration, not content), and the
/// `connectors` rows (which provider accounts are linked, not what was imported
/// from them).
async fn links(pool: &PgPool, user_id: UserId) -> Result<Vec<ExportLink>, sqlx::Error> {
    let rows: Vec<(Uuid, Uuid, Uuid, i32, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, a_tag_id, b_tag_id, score, rationale, judged_at FROM links \
         WHERE user_id = $1 ORDER BY judged_at, id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(
            |(id, a_tag_id, b_tag_id, score, rationale, judged_at)| ExportLink {
                id,
                a_tag_id,
                b_tag_id,
                score,
                rationale,
                judged_at,
            },
        )
        .collect())
}

async fn lists(pool: &PgPool, user_id: UserId) -> Result<Vec<ExportList>, sqlx::Error> {
    let rows: Vec<(Uuid, String, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT id, name, source, import_run_id FROM lists WHERE user_id = $1 \
         ORDER BY name, id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, name, source, import_run_id)| ExportList {
            id,
            name,
            source,
            import_run_id,
        })
        .collect())
}

async fn places(pool: &PgPool, user_id: UserId) -> Result<Vec<ExportPlace>, sqlx::Error> {
    let rows: Vec<PlaceRow> = sqlx::query_as(
        "SELECT id, list_id, tag_id, name, address, lat, lng, place_id, primary_type, \
         types, metro, locality, country, notes, source, enriched_at FROM places \
         WHERE user_id = $1 ORDER BY name, id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| ExportPlace {
            id: row.id,
            list_id: row.list_id,
            tag_id: row.tag_id,
            name: row.name,
            address: row.address,
            lat: row.lat,
            lng: row.lng,
            place_id: row.place_id,
            primary_type: row.primary_type,
            types: row.types,
            metro: row.metro,
            locality: row.locality,
            country: row.country,
            notes: row.notes,
            source: row.source,
            enriched_at: row.enriched_at,
        })
        .collect())
}

/// The places columns as they come back, so the sixteen of them stay readable
/// at the call site instead of as a tuple nobody can count.
#[derive(sqlx::FromRow)]
struct PlaceRow {
    id: Uuid,
    list_id: Option<Uuid>,
    tag_id: Option<Uuid>,
    name: String,
    address: Option<String>,
    lat: Option<f64>,
    lng: Option<f64>,
    place_id: Option<String>,
    primary_type: Option<String>,
    types: Vec<String>,
    metro: Option<String>,
    locality: Option<String>,
    country: Option<String>,
    notes: Option<String>,
    source: String,
    enriched_at: Option<DateTime<Utc>>,
}

pub async fn delete_all(pool: &PgPool, user_id: UserId) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;

    for statement in [
        "DELETE FROM connector_secrets WHERE user_id = $1",
        "DELETE FROM connectors WHERE user_id = $1",
        "DELETE FROM summary_notes WHERE user_id = $1",
        "DELETE FROM links WHERE user_id = $1",
        "DELETE FROM notes WHERE user_id = $1",
        "DELETE FROM summaries WHERE user_id = $1",
        "DELETE FROM threads WHERE user_id = $1",
        "DELETE FROM places WHERE user_id = $1",
        "DELETE FROM lists WHERE user_id = $1",
        "DELETE FROM jobs WHERE user_id = $1",
        "DELETE FROM import_runs WHERE user_id = $1",
        "DELETE FROM tags WHERE user_id = $1",
    ] {
        sqlx::query(statement)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
    }

    // A tag that is no longer there cannot be the one the grid remembered as
    // approved; nothing else in `tags` survives, so this is the whole table.
    tx.commit().await
}
