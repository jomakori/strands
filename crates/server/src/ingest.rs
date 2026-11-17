//! `POST /api/v1/ingest` (ticket T9): batch import with AI tag assignment.
//!
//! Every model call finishes before the first row is written, and the writes
//! all share one transaction. A model failure therefore answers 500 with no
//! tags or notes created — there is no such thing as a half-applied batch.
//! Names are matched after normalization, so re-ingesting the same items
//! attaches the tags that already exist instead of creating duplicates.

use std::collections::HashMap;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::routing::post;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, Postgres, Transaction};
use strands_api_types::{IngestItem, IngestRequest, IngestResult, TagSummary};
use strands_core::{Note, NoteKind, Source};
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::tagging::{self, MAX_NEW_TAGS_PER_INGEST, TagItem, clean_tag_name, normalize_tag_name};

/// Items one request may carry: five model calls, enough for a real import and
/// small enough that a single ingest stays bounded.
pub const MAX_ITEMS_PER_INGEST: usize = 200;

/// Model-supplied name used when nothing usable survived, and the fallback
/// once an ingest has already created [`MAX_NEW_TAGS_PER_INGEST`] tags.
const FALLBACK_TAG_NAME: &str = "Inbox";

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/ingest", post(ingest))
}

pub async fn ingest(
    State(state): State<AppState>,
    user: CurrentUser,
    payload: Result<Json<IngestRequest>, JsonRejection>,
) -> Result<Json<IngestResult>, AppError> {
    let Json(request) = payload.map_err(|_| AppError::BadRequest("invalid ingest body".into()))?;
    validate(&request)?;

    // Fail before spending a model call if there is nowhere to file a new tag.
    let category_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM categories WHERE user_id = $1 ORDER BY position, name LIMIT 1",
    )
    .bind(user.id)
    .fetch_optional(&state.pool)
    .await?;
    let category_id = category_id
        .ok_or_else(|| AppError::BadRequest("no category exists to file a new tag under".into()))?;

    let existing = load_tag_names(&state.pool, user.id).await?;
    let items: Vec<TagItem> = request
        .items
        .iter()
        .map(|item| TagItem {
            body: item.body.clone(),
        })
        .collect();

    // Every batch must succeed before anything is written.
    let names = tagging::assign_batches(state.tagger.as_ref(), &existing, &items).await?;

    let (tag_ids, notes_created) =
        apply(&state.pool, user.id, category_id, &request.items, &names).await?;

    Ok(Json(IngestResult {
        tags: load_tag_summaries(&state.pool, user.id, &tag_ids).await?,
        notes_created,
    }))
}

fn validate(request: &IngestRequest) -> Result<(), AppError> {
    if request.items.is_empty() {
        return Err(AppError::BadRequest(
            "ingest needs at least one item".into(),
        ));
    }
    if request.items.len() > MAX_ITEMS_PER_INGEST {
        return Err(AppError::BadRequest(format!(
            "ingest accepts at most {MAX_ITEMS_PER_INGEST} items per request"
        )));
    }
    for item in &request.items {
        if item.body.trim().is_empty() {
            return Err(AppError::BadRequest("ingest items need a body".into()));
        }
        if item.body.chars().count() > strands_core::MAX_BODY_CHARS {
            return Err(AppError::BadRequest(format!(
                "ingest bodies are limited to {} characters",
                strands_core::MAX_BODY_CHARS
            )));
        }
    }
    Ok(())
}

async fn load_tag_names(pool: &sqlx::PgPool, user_id: Uuid) -> Result<Vec<String>, AppError> {
    let names: Vec<String> =
        sqlx::query_scalar("SELECT name FROM tags WHERE user_id = $1 ORDER BY created_at")
            .bind(user_id)
            .fetch_all(pool)
            .await?;
    Ok(names)
}

/// Apply one name per item in a single transaction; returns the tags touched.
async fn apply(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    category_id: Uuid,
    items: &[IngestItem],
    names: &[String],
) -> Result<(Vec<Uuid>, usize), AppError> {
    let mut tx = pool.begin().await?;

    let existing: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, name FROM tags WHERE user_id = $1")
            .bind(user_id)
            .fetch_all(&mut *tx)
            .await?;
    let mut index = TagIndex {
        by_key: existing
            .into_iter()
            .map(|(id, name)| (normalize_tag_name(&name), id))
            .collect(),
        category_id,
        created: 0,
    };

    let mut touched: Vec<Uuid> = Vec::new();
    let mut notes_created = 0usize;
    let now = Utc::now();

    for (item, name) in items.iter().zip(names) {
        let tag_id = index.resolve(&mut tx, user_id, name).await?;
        if !touched.contains(&tag_id) {
            touched.push(tag_id);
        }

        if insert_note(&mut tx, user_id, tag_id, item, now).await? {
            notes_created += 1;
        }
    }

    tx.commit().await?;
    Ok((touched, notes_created))
}

/// Insert the note unless this exact item was ingested before, and give it a
/// thread only in that case — a repeat run creates neither note nor thread.
async fn insert_note(
    conn: &mut PgConnection,
    user_id: Uuid,
    tag_id: Uuid,
    item: &IngestItem,
    now: DateTime<Utc>,
) -> Result<bool, AppError> {
    let note = Note::new(item.body.clone(), Source::Manual)?;
    let key = dedupe_key(item);

    let note_id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO notes \
            (id, user_id, tag_id, kind, body, occurred_at, source, external_id, dedupe_key) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (user_id, dedupe_key) DO NOTHING \
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(tag_id)
    .bind(NoteKind::Text)
    .bind(note.body())
    .bind(now)
    .bind(Source::Manual)
    .bind(item.external_id.as_deref())
    .bind(&key)
    .fetch_optional(&mut *conn)
    .await?;

    let Some(note_id) = note_id else {
        return Ok(false);
    };

    let thread_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO threads (id, user_id, tag_id, title, source_count, updated_at) \
         VALUES ($1, $2, $3, $4, 1, $5)",
    )
    .bind(thread_id)
    .bind(user_id)
    .bind(tag_id)
    .bind(note.title())
    .bind(now)
    .execute(&mut *conn)
    .await?;

    sqlx::query("UPDATE notes SET thread_id = $1 WHERE id = $2 AND user_id = $3")
        .bind(thread_id)
        .bind(note_id)
        .bind(user_id)
        .execute(&mut *conn)
        .await?;

    Ok(true)
}

/// Stable key for "the same item again": the caller's id when given, otherwise
/// a hash of the body.
fn dedupe_key(item: &IngestItem) -> String {
    match item
        .external_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(id) => format!("import:ext:{id}"),
        None => {
            let digest = Sha256::digest(item.body.as_bytes());
            let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
            format!("import:body:{hex}")
        }
    }
}

pub(crate) async fn load_tag_summaries(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    tag_ids: &[Uuid],
) -> Result<Vec<TagSummary>, AppError> {
    if tag_ids.is_empty() {
        return Ok(Vec::new());
    }

    let rows: Vec<(Uuid, String, Uuid, DateTime<Utc>, i64)> = sqlx::query_as(
        "SELECT t.id, t.name, t.category_id, t.created_at, COUNT(th.id) \
         FROM tags t \
         LEFT JOIN threads th ON th.tag_id = t.id AND th.user_id = t.user_id \
         WHERE t.user_id = $1 AND t.id = ANY($2) \
         GROUP BY t.id, t.name, t.category_id, t.created_at \
         ORDER BY t.created_at DESC",
    )
    .bind(user_id)
    .bind(tag_ids)
    .fetch_all(pool)
    .await?;

    Ok(rows
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
        .collect())
}

/// The caller's tags keyed by normalized name, plus the new-tag budget.
pub(crate) struct TagIndex {
    by_key: HashMap<String, Uuid>,
    category_id: Uuid,
    created: usize,
}

impl TagIndex {
    /// Build an index from the caller's existing tags.
    ///
    /// The text-ingest path fills this as it goes; importers that file notes
    /// under a place-derived tag hint (ticket T16) need the same tag rows the
    /// user already has, so they start from a load rather than an empty map.
    pub(crate) async fn load(
        conn: &mut Transaction<'_, Postgres>,
        user_id: Uuid,
        category_id: Uuid,
    ) -> Result<Self, AppError> {
        let rows: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT id, name FROM tags WHERE user_id = $1")
                .bind(user_id)
                .fetch_all(&mut **conn)
                .await?;
        let by_key = rows
            .into_iter()
            .map(|(id, name)| (normalize_tag_name(&name), id))
            .collect();
        Ok(Self {
            by_key,
            category_id,
            created: 0,
        })
    }

    /// The id for `raw`, creating the tag only when the name is genuinely new
    /// and the per-ingest budget allows it; otherwise file under Inbox.
    pub(crate) async fn resolve(
        &mut self,
        conn: &mut Transaction<'_, Postgres>,
        user_id: Uuid,
        raw: &str,
    ) -> Result<Uuid, AppError> {
        let mut cleaned = clean_tag_name(raw).unwrap_or_else(|| FALLBACK_TAG_NAME.to_owned());
        let mut key = normalize_tag_name(&cleaned);

        if !self.by_key.contains_key(&key) && self.created >= MAX_NEW_TAGS_PER_INGEST {
            cleaned = FALLBACK_TAG_NAME.to_owned();
            key = normalize_tag_name(&cleaned);
        }

        if let Some(id) = self.by_key.get(&key) {
            return Ok(*id);
        }

        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO tags (id, user_id, category_id, name) VALUES ($1, $2, $3, $4)")
            .bind(id)
            .bind(user_id)
            .bind(self.category_id)
            .bind(&cleaned)
            .execute(&mut **conn)
            .await?;

        self.created += 1;
        self.by_key.insert(key, id);
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(body: &str, external_id: Option<&str>) -> IngestItem {
        IngestItem {
            body: body.to_owned(),
            external_id: external_id.map(str::to_owned),
        }
    }

    fn bad_request_message(error: AppError) -> String {
        match error {
            AppError::BadRequest(message) => message,
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_request_is_rejected() {
        let error = validate(&IngestRequest { items: Vec::new() }).expect_err("empty");
        assert!(bad_request_message(error).contains("at least one"));
    }

    #[test]
    fn more_than_the_cap_is_rejected() {
        let items = vec![item("note", None); MAX_ITEMS_PER_INGEST + 1];
        let error = validate(&IngestRequest { items }).expect_err("too many");
        assert!(bad_request_message(error).contains("at most"));
    }

    #[test]
    fn a_blank_body_is_rejected() {
        let error = validate(&IngestRequest {
            items: vec![item("   \n", None)],
        })
        .expect_err("blank");
        assert!(bad_request_message(error).contains("body"));
    }

    #[test]
    fn the_same_external_id_hashes_to_the_same_dedupe_key() {
        let a = dedupe_key(&item("one body", Some("wa-1")));
        let b = dedupe_key(&item("different body", Some("wa-1")));
        assert_eq!(a, b);
    }

    #[test]
    fn identical_bodies_without_ids_share_a_dedupe_key() {
        assert_eq!(
            dedupe_key(&item("same", None)),
            dedupe_key(&item("same", None))
        );
    }

    #[test]
    fn different_bodies_without_ids_do_not() {
        assert_ne!(
            dedupe_key(&item("one", None)),
            dedupe_key(&item("two", None))
        );
    }
}
