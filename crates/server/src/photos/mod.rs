//! Google Photos Picker routes (ticket T14).
//!
//! The Picker is a three-request handoff: create a session, wait for the user
//! to pick in Google's UI, then read the picked items out. That spans requests,
//! so the session lives in `photo_picker_sessions` and every poll re-enters it
//! by Google's session id.
//!
//! Two failure modes decide the shape of this module:
//!
//! * **A lapsed `baseUrl`.** Google hands out download URLs that live for 60
//!   minutes ([`BASE_URL_WINDOW_MINUTES`]); past that they are dead and no retry
//!   revives them. The only recovery is a *new* session, so an expiry is not an
//!   error here — it is a state the poll reports, carrying the replacement
//!   session the client should switch to.
//! * **A re-polled session.** Importing the same pick twice must not duplicate
//!   anything, so notes dedupe on `(user_id, dedupe_key)` where the key is the
//!   picker's media item id. Each item is written in its own transaction, so a
//!   rotation mid-import keeps whatever already landed and the retry skips it.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::{DateTime, Duration, Utc};
use sqlx::{Postgres, Transaction};
use strands_api_types::{PhotoPickResult, PhotoPickState, PhotoSession};
use strands_connector_photos::picker::BASE_URL_WINDOW_MINUTES;
use strands_connector_photos::{
    PhotosError, PickedItem, PickerApi, PickerSession, Thumbnail, dedupe_key, note_body, tag_hint,
};
use strands_core::{Note, NoteKind, Source};
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::ingest::{TagIndex, load_tag_summaries};
use crate::secrets;

/// `connectors.kind` for this lane; also the `source` label stored in meta.
const CONNECTOR_KIND: &str = "google_photos";

/// Appended to `notes.meta` so a note can be traced back to its pick.
const IMPORT_LABEL: &str = "google_photos_picker";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/connectors/photos/sessions", post(create_session))
        .route(
            "/api/v1/connectors/photos/sessions/{session_id}",
            get(poll_session),
        )
        // The bytes are stored, not referenced: this is the only way back to
        // them, and it obeys the same ownership rule as the note itself.
        .route("/api/v1/notes/{note_id}/thumbnail", get(note_thumbnail))
}

/// One session row, as the poll needs it.
#[derive(sqlx::FromRow)]
struct SessionRow {
    session_id: String,
    expires_at: DateTime<Utc>,
    state: String,
    notes_created: i32,
    notes_skipped: i32,
    created_at: DateTime<Utc>,
}

/// `POST /api/v1/connectors/photos/sessions` — open a pick.
///
/// The caller gets back the URL to send the user to; the poll id is Google's
/// session id, which is the only handle the client ever holds.
pub async fn create_session(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<PhotoSession>, AppError> {
    let token = access_token(&state, user.id).await?;
    Ok(Json(start_session(&state, user.id, &token).await?))
}

/// `GET /api/v1/connectors/photos/sessions/{session_id}` — where the pick
/// stands, and the import when it is ready.
///
/// Scoping is `session_id` + `user_id` in the same predicate and a missing row
/// is `404`, so another user's session is indistinguishable from one that never
/// existed. `403` would confirm the session is real.
pub async fn poll_session(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(session_id): Path<String>,
) -> Result<Json<PhotoPickResult>, AppError> {
    let row: SessionRow = sqlx::query_as(
        "SELECT session_id, expires_at, state, notes_created, notes_skipped, created_at \
           FROM photo_picker_sessions \
          WHERE session_id = $1 AND user_id = $2",
    )
    .bind(&session_id)
    .bind(user.id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    let created = row.notes_created.max(0) as usize;
    let skipped = row.notes_skipped.max(0) as usize;

    match row.state.as_str() {
        // Importing is a one-shot: the counts are the receipt, and the pick
        // cannot be re-read because the baseUrls died with it.
        "imported" => {
            return Ok(Json(PhotoPickResult {
                state: PhotoPickState::Imported,
                session: None,
                notes_created: 0,
                notes_skipped: created + skipped,
                tags: Vec::new(),
            }));
        }
        // Already rotated once. Answering with *the same* successor keeps a
        // client that polls an expired session from minting a session per poll.
        "expired" => {
            let replacement = successor_or_new(&state, user.id, row.created_at).await?;
            return Ok(Json(PhotoPickResult {
                state: PhotoPickState::Expired,
                session: Some(replacement),
                notes_created: 0,
                notes_skipped: created + skipped,
                tags: Vec::new(),
            }));
        }
        _ => {}
    }

    let token = access_token(&state, user.id).await?;

    // The session itself has a lifetime of its own; a poll after it lapses can
    // never see items even if the user picked.
    if Utc::now() >= row.expires_at {
        return Ok(Json(
            expire_and_replace(
                &state,
                user.id,
                &row.session_id,
                row.created_at,
                created,
                skipped,
            )
            .await?,
        ));
    }

    match state.photos.session_status(&token, &row.session_id).await {
        Ok(status) if !status.media_items_set => {
            return Ok(Json(PhotoPickResult {
                state: PhotoPickState::Pending,
                session: None,
                notes_created: 0,
                notes_skipped: 0,
                tags: Vec::new(),
            }));
        }
        Ok(_) => {}
        // Google forgot the session (it is older than its retention), which no
        // retry fixes but a new session does.
        Err(PhotosError::Expired) => {
            return Ok(Json(
                expire_and_replace(
                    &state,
                    user.id,
                    &row.session_id,
                    row.created_at,
                    created,
                    skipped,
                )
                .await?,
            ));
        }
        Err(error) => return Err(picker_error(error, "polling the picker session")),
    }

    let items = match state.photos.list_items(&token, &row.session_id).await {
        Ok(items) => items,
        Err(PhotosError::Expired) => {
            return Ok(Json(
                expire_and_replace(
                    &state,
                    user.id,
                    &row.session_id,
                    row.created_at,
                    created,
                    skipped,
                )
                .await?,
            ));
        }
        Err(error) => return Err(picker_error(error, "listing the picked items")),
    };

    if items.is_empty() {
        mark(&state.pool, &row.session_id, user.id, "imported", 0, 0).await?;
        return Ok(Json(PhotoPickResult {
            state: PhotoPickState::Empty,
            session: None,
            notes_created: 0,
            notes_skipped: 0,
            tags: Vec::new(),
        }));
    }

    match import_items(&state, user.id, &items).await? {
        Import::Written {
            created,
            skipped,
            tags,
        } => {
            mark(
                &state.pool,
                &row.session_id,
                user.id,
                "imported",
                created,
                skipped,
            )
            .await?;
            Ok(Json(PhotoPickResult {
                state: PhotoPickState::Imported,
                session: None,
                notes_created: created,
                notes_skipped: skipped,
                tags: load_tag_summaries(&state.pool, user.id, &tags).await?,
            }))
        }
        // Whatever landed before the download window closed stays; the retry
        // under the new session skips it by media item id.
        Import::Expired { created, skipped } => Ok(Json(
            expire_and_replace(
                &state,
                user.id,
                &row.session_id,
                row.created_at,
                created,
                skipped,
            )
            .await?,
        )),
    }
}

/// How one import run ended.
enum Import {
    Written {
        created: usize,
        skipped: usize,
        tags: Vec<Uuid>,
    },
    Expired {
        created: usize,
        skipped: usize,
    },
}

/// Download each picked item's preview and file it as a note.
///
/// One transaction per item rather than one for the run: a `baseUrl` that lapses
/// halfway must leave the earlier notes committed, or the user loses the part of
/// the pick that did work.
async fn import_items(
    state: &AppState,
    user_id: Uuid,
    items: &[PickedItem],
) -> Result<Import, AppError> {
    let category_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM categories WHERE user_id = $1 ORDER BY position, name LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await?;
    let category_id = category_id
        .ok_or_else(|| AppError::BadRequest("no category exists to file a new tag under".into()))?;

    // The baseUrls were issued by the list call just now, so the window opens
    // here: checking the clock before each download turns "Google says 403" into
    // "we knew this would fail, rotate instead".
    let download_until = Utc::now() + Duration::minutes(BASE_URL_WINDOW_MINUTES);

    let mut created = 0usize;
    let mut skipped = 0usize;
    let mut tag_ids: Vec<Uuid> = Vec::new();
    let mut index: Option<TagIndex> = None;

    for item in items {
        if Utc::now() >= download_until {
            return Ok(Import::Expired { created, skipped });
        }

        let thumbnail = match state.photos.thumbnail(&item.base_url).await {
            Ok(thumbnail) => thumbnail,
            Err(PhotosError::Expired) => return Ok(Import::Expired { created, skipped }),
            // A video still has a baseUrl but no still image at the preview
            // size; skipping it keeps one odd item from failing the pick.
            Err(PhotosError::ThumbnailTooLarge) => {
                eprintln!(
                    "strands: skipping picked item {}: the preview exceeds the size cap",
                    item.id
                );
                skipped += 1;
                continue;
            }
            Err(error) => return Err(picker_error(error, "downloading a photo preview")),
        };

        let mut tx = state.pool.begin().await?;
        let index = match index.as_mut() {
            Some(index) => index,
            None => index.insert(TagIndex::load(&mut tx, user_id, category_id).await?),
        };
        let tag_id = index.resolve(&mut tx, user_id, &tag_hint(item)).await?;
        if !tag_ids.contains(&tag_id) {
            tag_ids.push(tag_id);
        }
        if write_item(&mut tx, user_id, tag_id, item, &thumbnail).await? {
            created += 1;
        } else {
            skipped += 1;
        }
        tx.commit().await?;
    }

    Ok(Import::Written {
        created,
        skipped,
        tags: tag_ids,
    })
}

/// Insert one photo note, its thread and its preview; `false` when the item was
/// already imported (the conflict is the idempotency, not a code path).
async fn write_item(
    conn: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    tag_id: Uuid,
    item: &PickedItem,
    thumbnail: &Thumbnail,
) -> Result<bool, AppError> {
    let note = Note::new(note_body(item), Source::GooglePhotos)?;
    // A pick can predate its download, so the note is anchored to when the photo
    // was taken; without a capture time the import moment is the honest one.
    let occurred_at = item.create_time.unwrap_or_else(Utc::now);

    let meta = serde_json::json!({
        "captured_at": item.create_time,
        "mime_type": item.mime_type,
        "width": item.width,
        "height": item.height,
        "google_media_item_id": item.id,
        "import": IMPORT_LABEL,
    });

    let note_id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO notes \
            (id, user_id, tag_id, kind, body, occurred_at, source, external_id, dedupe_key, meta) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::jsonb) \
         ON CONFLICT (user_id, dedupe_key) DO NOTHING \
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(tag_id)
    .bind(NoteKind::Photo)
    .bind(note.body())
    .bind(occurred_at)
    .bind(Source::GooglePhotos)
    .bind(&item.id)
    .bind(dedupe_key(item))
    .bind(meta.to_string())
    .fetch_optional(&mut **conn)
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
    .bind(occurred_at)
    .execute(&mut **conn)
    .await?;

    sqlx::query("UPDATE notes SET thread_id = $1 WHERE id = $2 AND user_id = $3")
        .bind(thread_id)
        .bind(note_id)
        .bind(user_id)
        .execute(&mut **conn)
        .await?;

    sqlx::query(
        "INSERT INTO note_thumbnails (note_id, user_id, content_type, bytes, width, height) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(note_id)
    .bind(user_id)
    .bind(&thumbnail.content_type)
    .bind(&thumbnail.bytes)
    .bind(to_i32(item.width))
    .bind(to_i32(item.height))
    .execute(&mut **conn)
    .await?;

    Ok(true)
}

/// `GET /api/v1/notes/{note_id}/thumbnail` — the stored preview bytes.
pub async fn note_thumbnail(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(note_id): Path<Uuid>,
) -> Result<Response, AppError> {
    let stored: Option<(String, Vec<u8>)> = sqlx::query_as(
        "SELECT t.content_type, t.bytes \
           FROM note_thumbnails t \
           JOIN notes n ON n.id = t.note_id \
          WHERE t.note_id = $1 AND t.user_id = $2 AND n.user_id = $2",
    )
    .bind(note_id)
    .bind(user.id)
    .fetch_optional(&state.pool)
    .await?;

    let (content_type, bytes) = stored.ok_or(AppError::NotFound)?;
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            // The bytes for a given note never change, and the response is
            // user-scoped, so a private cache is safe.
            (header::CACHE_CONTROL, "private, max-age=86400".to_owned()),
        ],
        bytes,
    )
        .into_response())
}

/// Record how a session ended, so a later poll does not re-run the import.
async fn mark(
    pool: &sqlx::PgPool,
    session_id: &str,
    user_id: Uuid,
    state: &str,
    created: usize,
    skipped: usize,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE photo_picker_sessions \
            SET state = $3, notes_created = $4, notes_skipped = $5 \
          WHERE session_id = $1 AND user_id = $2",
    )
    .bind(session_id)
    .bind(user_id)
    .bind(state)
    .bind(created as i32)
    .bind(skipped as i32)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark `session_id` expired and hand back the session to use instead.
async fn expire_and_replace(
    state: &AppState,
    user_id: Uuid,
    session_id: &str,
    created_at: DateTime<Utc>,
    created: usize,
    skipped: usize,
) -> Result<PhotoPickResult, AppError> {
    mark(
        &state.pool,
        session_id,
        user_id,
        "expired",
        created,
        skipped,
    )
    .await?;
    let replacement = successor_or_new(state, user_id, created_at).await?;
    Ok(PhotoPickResult {
        state: PhotoPickState::Expired,
        session: Some(replacement),
        // What this run managed before the URLs died; the replacement session
        // imports the rest, and its already-noted items are skipped.
        notes_created: created,
        notes_skipped: skipped,
        tags: Vec::new(),
    })
}

/// The newest still-usable session opened after `after`, or a brand new one.
///
/// Reusing the successor is what makes an expired session's poll idempotent: a
/// client that retries gets the same replacement instead of another session.
async fn successor_or_new(
    state: &AppState,
    user_id: Uuid,
    after: DateTime<Utc>,
) -> Result<PhotoSession, AppError> {
    let successor: Option<(String, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT session_id, picker_uri, expires_at \
           FROM photo_picker_sessions \
          WHERE user_id = $1 AND state = 'pending' AND created_at > $2 \
          ORDER BY created_at DESC \
          LIMIT 1",
    )
    .bind(user_id)
    .bind(after)
    .fetch_optional(&state.pool)
    .await?;

    if let Some((session_id, picker_uri, expires_at)) = successor {
        return Ok(PhotoSession {
            session_id,
            picker_uri,
            expires_at,
        });
    }

    let token = access_token(state, user_id).await?;
    start_session(state, user_id, &token).await
}

/// Ask Google for a session and remember it.
async fn start_session(
    state: &AppState,
    user_id: Uuid,
    token: &str,
) -> Result<PhotoSession, AppError> {
    let PickerSession {
        id,
        picker_uri,
        expires_at,
    } = state
        .photos
        .create_session(token)
        .await
        .map_err(|error| picker_error(error, "starting a picker session"))?;

    sqlx::query(
        "INSERT INTO photo_picker_sessions (id, user_id, session_id, picker_uri, expires_at) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (user_id, session_id) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(&id)
    .bind(&picker_uri)
    .bind(expires_at)
    .execute(&state.pool)
    .await?;

    Ok(PhotoSession {
        session_id: id,
        picker_uri,
        expires_at,
    })
}

/// The caller's stored Google Photos access token, or the reason there is none.
async fn access_token(state: &AppState, user_id: Uuid) -> Result<String, AppError> {
    let key = state.secrets.as_ref().ok_or_else(|| {
        AppError::internal(
            "photos connector",
            sqlx::Error::Protocol(format!(
                "{} is unset, so connector tokens cannot be opened",
                secrets::KEY_ENV
            )),
        )
    })?;

    secrets::access_token(&state.pool, key.as_ref(), user_id, CONNECTOR_KIND)
        .await?
        .ok_or_else(|| {
            AppError::BadRequest(
                "the Google Photos connector is not connected; connect it first".to_owned(),
            )
        })
}

/// Map a picker failure onto the answer the client can act on.
///
/// A refused token and an oversized preview are both the caller's problem to
/// resolve (reconnect, pick a different item), so they are `400` with a message
/// rather than a `500` that hides the cause. Transport failures stay ours.
fn picker_error(error: PhotosError, what: &str) -> AppError {
    match error {
        PhotosError::Refused(reason) => AppError::BadRequest(format!(
            "Google rejected the stored access token ({reason}); reconnect Google Photos"
        )),
        PhotosError::Expired => {
            AppError::BadRequest("the picker session has lapsed; start a new one".to_owned())
        }
        PhotosError::ThumbnailTooLarge => {
            AppError::BadRequest("the photo preview is larger than the size cap".to_owned())
        }
        PhotosError::Transport(reason) => AppError::internal(
            "google photos",
            sqlx::Error::Protocol(format!("{what}: {reason}")),
        ),
    }
}

/// Google's dimensions are `i64`; the column is `INTEGER`. Nothing real is
/// anywhere near the boundary, so clamp instead of wrapping.
fn to_i32(value: i64) -> i32 {
    value.clamp(0, i32::MAX as i64) as i32
}

/// Picker state kept next to the pool by [`AppState`].
pub type PickerHandle = Arc<dyn PickerApi>;
