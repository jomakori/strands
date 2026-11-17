//! Google Maps ingest, place lists and KML export (ticket T16).
//!
//! Two Takeout exports feed this module:
//!
//! * `Timeline.json` — visits become `visit` notes with a `geo:` hook in the
//!   body and a tag hinted from the place type.
//! * the saved-places export (CSV or JSON) — one list per file, one `places`
//!   row per saved place, geocoded when there is an address to geocode.
//!
//! Idempotency is the database's job, not the importer's: notes dedupe on
//! `(user_id, dedupe_key)`, lists on `(user_id, name, source)`, and places on
//! `(user_id, place_key)` where `place_key` is a generated identity column. The
//! import never reads-then-writes to decide whether a row exists; two uploads of
//! the same file racing each other still cannot double anything.

pub mod kml;
pub mod parse;
pub mod places;

use axum::Json;
use axum::Router;
use axum::extract::Multipart;
use axum::extract::Path;
use axum::extract::State;
use axum::extract::multipart::MultipartRejection;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use strands_api_types::{GmapsIngestResult, PlaceEnrichment, PlaceListSummary, PlaceSummary};
use strands_core::{Note, NoteKind, Source};
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::gmaps::kml::KmlPlace;
use crate::gmaps::parse::{Document, SavedPlace, TimelineDocument, Visit};
use crate::gmaps::places::{PlaceQuery, PlaceResolver, ResolvedPlace};
use crate::ingest::{TagIndex, load_tag_summaries};

/// One Takeout file, in memory. A Month of timeline JSON is a few MB; this cap
/// keeps a pathological upload from becoming the process's problem.
pub const MAX_UPLOAD_BYTES: usize = 32 * 1024 * 1024;

/// Tag used for visits whose file carries no place type to hint from.
const VISIT_FALLBACK_TAG: &str = "Places";

/// Stored in `lists.source` and `places.source` for everything this module
/// writes.
const SOURCE: &str = "google_maps_takeout";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/ingest/gmaps", post(ingest_gmaps))
        .route("/api/v1/lists", get(list_lists))
        .route("/api/v1/lists/{list_id}/places", get(list_places))
        .route("/api/v1/places/kml", get(places_kml))
}

/// `POST /api/v1/ingest/gmaps` — multipart upload of one Takeout file.
///
/// The file field is `file`; an optional `name` field overrides the list title.
/// The format is detected from the content, so a mislabelled upload still
/// imports.
pub async fn ingest_gmaps(
    State(state): State<AppState>,
    user: CurrentUser,
    multipart: Result<Multipart, MultipartRejection>,
) -> Result<Json<GmapsIngestResult>, AppError> {
    let upload = read_upload(
        multipart.map_err(|_| AppError::BadRequest("expected a multipart upload".into()))?,
    )
    .await?;

    let document = parse::parse_upload(&upload.text)
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    let format = document.format();
    // Read the multipart parts' metadata before the document takes ownership.
    let fallback_list_name = upload.list_name();
    let explicit_name = upload.name;

    let category_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM categories WHERE user_id = $1 ORDER BY position, name LIMIT 1",
    )
    .bind(user.id)
    .fetch_optional(&state.pool)
    .await?;
    let category_id = category_id
        .ok_or_else(|| AppError::BadRequest("no category exists to file a new tag under".into()))?;

    match document {
        Document::Timeline(timeline) => {
            let visits_seen = timeline.visits.len() + timeline.skipped;
            let skipped = timeline.skipped;
            let (tag_ids, notes_created) =
                write_visits(&state.pool, user.id, category_id, &timeline).await?;
            Ok(Json(GmapsIngestResult {
                format,
                visits_seen,
                visits_skipped: skipped,
                notes_created,
                places_seen: 0,
                places_created: 0,
                places_unresolved: 0,
                lists_created: 0,
                tags: load_tag_summaries(&state.pool, user.id, &tag_ids).await?,
            }))
        }
        Document::SavedPlaces(saved) => {
            let list_name = explicit_name.or(saved.name).unwrap_or(fallback_list_name);
            let resolutions = resolve_all(state.geocoder.as_ref(), &saved.places).await;
            let written = write_saved_places(
                &state.pool,
                user.id,
                &list_name,
                &saved.places,
                &resolutions,
            )
            .await?;
            Ok(Json(GmapsIngestResult {
                format,
                visits_seen: 0,
                visits_skipped: 0,
                notes_created: 0,
                places_seen: saved.places.len(),
                places_created: written.places_created,
                places_unresolved: written.unresolved,
                lists_created: written.lists_created,
                tags: Vec::new(),
            }))
        }
    }
}

/// `GET /api/v1/lists` — the caller's place lists, newest first.
pub async fn list_lists(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Vec<PlaceListSummary>>, AppError> {
    let lists: Vec<(Uuid, String, String, DateTime<Utc>, i64)> = sqlx::query_as(
        "SELECT l.id, l.name, l.source, l.created_at, count(p.id) \
         FROM lists l \
         LEFT JOIN places p ON p.list_id = l.id AND p.user_id = l.user_id \
         WHERE l.user_id = $1 \
         GROUP BY l.id \
         ORDER BY l.created_at DESC, l.name",
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(
        lists
            .into_iter()
            .map(
                |(id, name, source, created_at, place_count)| PlaceListSummary {
                    id,
                    name,
                    source,
                    place_count,
                    created_at,
                },
            )
            .collect(),
    ))
}

/// `GET /api/v1/lists/{id}/places` — one list's places.
///
/// A list that is not the caller's is reported as missing rather than forbidden:
/// the ids are unguessable, and 404 does not confirm someone else's list exists.
pub async fn list_places(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(list_id): Path<Uuid>,
) -> Result<Json<Vec<PlaceSummary>>, AppError> {
    let exists: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM lists WHERE id = $1 AND user_id = $2")
            .bind(list_id)
            .bind(user.id)
            .fetch_optional(&state.pool)
            .await?;
    if exists.is_none() {
        return Err(AppError::NotFound);
    }

    let rows: Vec<PlaceRow> = sqlx::query_as(PLACE_SELECT_BY_LIST)
        .bind(list_id)
        .bind(user.id)
        .fetch_all(&state.pool)
        .await?;

    Ok(Json(rows.into_iter().map(PlaceRow::into_summary).collect()))
}

/// `GET /api/v1/places/kml` — every resolved place as one KML document.
///
/// Unresolved rows cannot be plotted; their number travels in
/// `X-Strands-Unresolved-Places` and inside the document description so a
/// download is not silently incomplete.
pub async fn places_kml(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<impl IntoResponse, AppError> {
    let rows: Vec<PlaceRow> = sqlx::query_as(PLACE_SELECT_BY_USER)
        .bind(user.id)
        .fetch_all(&state.pool)
        .await?;

    let unresolved: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM places \
         WHERE user_id = $1 AND (lat IS NULL OR lng IS NULL OR enrichment <> 'resolved')",
    )
    .bind(user.id)
    .fetch_one(&state.pool)
    .await?;

    let points: Vec<KmlPlace> = rows
        .into_iter()
        .filter_map(|row| match (row.lat, row.lng) {
            (Some(lat), Some(lng)) => Some(KmlPlace {
                name: row.name,
                address: row.address,
                lat,
                lng,
                place_id: row.place_id,
                primary_type: row.primary_type,
                notes: row.notes,
            }),
            _ => None,
        })
        .collect();

    let body = kml::render(&points, unresolved.max(0) as usize);
    Ok((
        [
            (
                header::CONTENT_TYPE,
                "application/vnd.google-earth.kml+xml".to_owned(),
            ),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"strands-places.kml\"".to_owned(),
            ),
            (
                header::HeaderName::from_static("x-strands-unresolved-places"),
                unresolved.max(0).to_string(),
            ),
        ],
        body,
    ))
}

/// The parts of a multipart upload this importer uses.
struct Upload {
    text: String,
    /// Explicit `name` field: overrides everything else.
    name: Option<String>,
    /// File name from the multipart part, extension included.
    file_name: Option<String>,
}

impl Upload {
    /// List title derived from the file name, so the same file reuploaded finds
    /// the list it created last time.
    fn list_name(&self) -> String {
        let stem = self
            .file_name
            .as_deref()
            .map(|name| {
                name.rsplit('/')
                    .next()
                    .unwrap_or(name)
                    .rsplit_once('.')
                    .map_or(name, |(stem, _)| stem)
                    .to_owned()
            })
            .map(|stem| stem.trim().to_owned())
            .filter(|stem| !stem.is_empty());
        stem.unwrap_or_else(|| "Saved places".to_owned())
    }
}

async fn read_upload(mut multipart: Multipart) -> Result<Upload, AppError> {
    let mut text: Option<String> = None;
    let mut name: Option<String> = None;
    let mut file_name: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| AppError::BadRequest(format!("unreadable multipart body: {error}")))?
    {
        let field_name = field.name().unwrap_or_default().to_owned();
        match field_name.as_str() {
            "file" => {
                file_name = field.file_name().map(str::to_owned);
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|error| AppError::BadRequest(format!("unreadable upload: {error}")))?;
                if bytes.len() > MAX_UPLOAD_BYTES {
                    return Err(AppError::BadRequest(format!(
                        "the upload is larger than {} MB",
                        MAX_UPLOAD_BYTES / (1024 * 1024)
                    )));
                }
                let body = String::from_utf8(bytes.to_vec())
                    .map_err(|_| AppError::BadRequest("the upload is not UTF-8 text".into()))?;
                text = Some(body);
            }
            "name" => {
                let value = field
                    .text()
                    .await
                    .map_err(|error| AppError::BadRequest(format!("unreadable name: {error}")))?;
                let value = value.trim().to_owned();
                if !value.is_empty() {
                    name = Some(value);
                }
            }
            _ => {}
        }
    }

    let text = text.ok_or_else(|| AppError::BadRequest("the upload has no `file` part".into()))?;
    Ok(Upload {
        text,
        name,
        file_name,
    })
}

/// Write one visit note per timeline entry; returns the tags touched and how
/// many notes were actually new.
async fn write_visits(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    category_id: Uuid,
    timeline: &TimelineDocument,
) -> Result<(Vec<Uuid>, usize), AppError> {
    let mut tx = pool.begin().await?;
    let mut tags = TagIndex::load(&mut tx, user_id, category_id).await?;

    let mut touched: Vec<Uuid> = Vec::new();
    let mut notes_created = 0usize;

    for visit in &timeline.visits {
        let tag_id = tags.resolve(&mut tx, user_id, &tag_hint(visit)).await?;
        if !touched.contains(&tag_id) {
            touched.push(tag_id);
        }
        if insert_visit_note(&mut tx, user_id, tag_id, visit).await? {
            notes_created += 1;
        }
    }

    tx.commit().await?;
    Ok((touched, notes_created))
}

/// Insert one visit note unless an identical visit was imported before.
async fn insert_visit_note(
    conn: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    tag_id: Uuid,
    visit: &Visit,
) -> Result<bool, AppError> {
    let note = Note::new(visit_body(visit), Source::GoogleMaps)
        .map_err(|error| AppError::BadRequest(error.to_string()))?;

    let meta = serde_json::json!({
        "lat": visit.lat,
        "lng": visit.lng,
        "address": visit.address,
        "place_id": visit.place_id,
        "primary_type": visit.primary_type,
        "visited_at": visit.occurred_at,
        "import": SOURCE,
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
    .bind(NoteKind::Visit)
    .bind(note.body())
    .bind(visit.occurred_at)
    .bind(Source::GoogleMaps)
    .bind(visit.place_id.as_deref())
    .bind(visit_dedupe_key(visit))
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
    .bind(visit.occurred_at)
    .execute(&mut **conn)
    .await?;

    sqlx::query("UPDATE notes SET thread_id = $1 WHERE id = $2 AND user_id = $3")
        .bind(thread_id)
        .bind(note_id)
        .bind(user_id)
        .execute(&mut **conn)
        .await?;

    Ok(true)
}

/// Tag a visit by its place type (`Cafe`, `Museum`); without a type the file
/// says nothing usable about the place, so it files under [`VISIT_FALLBACK_TAG`]
/// instead of inventing a tag per business name.
fn tag_hint(visit: &Visit) -> String {
    match visit.primary_type.as_deref() {
        Some(kind) if !kind.trim().is_empty() => parse::title_case(kind.trim()),
        _ => VISIT_FALLBACK_TAG.to_owned(),
    }
}

/// Note body: the human line first, then the coordinates as a `geo:` URI
/// (RFC 5870) so anything downstream can map the visit without parsing prose.
fn visit_body(visit: &Visit) -> String {
    let mut body = format!("Visited {}", visit.name);
    if let Some(address) = visit.address.as_deref().filter(|value| !value.is_empty()) {
        body.push('\n');
        body.push_str(address);
    }
    if let (Some(lat), Some(lng)) = (visit.lat, visit.lng) {
        body.push_str(&format!("\ngeo:{lat:.5},{lng:.5}"));
    }
    body.push_str(&format!(
        "\nGoogle Maps Timeline · {}",
        visit.occurred_at.format("%Y-%m-%d %H:%M UTC")
    ));
    body
}

/// Identity of a visit for dedupe: the place (or its coordinates) plus the
/// moment. Re-uploading the same export produces the same keys; a second visit
/// to the same place an hour later does not collide with the first.
fn visit_dedupe_key(visit: &Visit) -> String {
    let identity = visit
        .place_id
        .clone()
        .unwrap_or_else(|| match (visit.lat, visit.lng) {
            (Some(lat), Some(lng)) => format!("{lat:.5},{lng:.5}"),
            _ => visit.name.clone(),
        });
    let mut hasher = Sha256::new();
    hasher.update(b"gmaps-visit\n");
    hasher.update(identity.as_bytes());
    hasher.update(b"\n");
    hasher.update(visit.occurred_at.timestamp().to_string().as_bytes());
    format!("gmaps:visit:{:x}", hasher.finalize())
}

/// Resolve every saved place that can be resolved, before the transaction
/// opens: the lookup is network I/O and must not hold a connection open.
async fn resolve_all(resolver: &dyn PlaceResolver, places: &[SavedPlace]) -> Vec<Outcome> {
    let mut outcomes = Vec::with_capacity(places.len());
    for place in places {
        let address = place.address.as_deref().unwrap_or_default();
        let query = PlaceQuery {
            name: &place.title,
            address,
        };
        if !query.is_resolvable() {
            outcomes.push(Outcome::Unresolved);
            continue;
        }
        match resolver.resolve(query).await {
            Ok(Some(resolved)) => outcomes.push(Outcome::Resolved(Box::new(resolved))),
            Ok(None) => outcomes.push(Outcome::Unresolved),
            Err(error) => {
                // The row is still imported, just unresolved: an enrichment
                // outage must not lose the user's saved place.
                eprintln!(
                    "strands: place enrichment failed for {:?}: {error}",
                    place.title
                );
                outcomes.push(Outcome::Unresolved);
            }
        }
    }
    outcomes
}

/// What enrichment made of one saved place.
enum Outcome {
    /// Boxed: a resolved place is much larger than nothing, and most rows in a
    /// blank-address import take the other branch.
    Resolved(Box<ResolvedPlace>),
    Unresolved,
}

struct SavedPlacesWritten {
    lists_created: usize,
    places_created: usize,
    unresolved: usize,
}

/// Write the list and its places in one transaction.
async fn write_saved_places(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    list_name: &str,
    places: &[SavedPlace],
    outcomes: &[Outcome],
) -> Result<SavedPlacesWritten, AppError> {
    let mut tx = pool.begin().await?;

    let (list_id, list_created): (Uuid, bool) = sqlx::query_as(
        "INSERT INTO lists (id, user_id, name, source) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (user_id, name, source) DO UPDATE SET name = lists.name \
         RETURNING id, (xmax = 0) AS created",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(list_name)
    .bind(SOURCE)
    .fetch_one(&mut *tx)
    .await?;

    let mut places_created = 0usize;
    let mut unresolved = 0usize;

    for (place, outcome) in places.iter().zip(outcomes) {
        let row = PlaceWrite::new(user_id, list_id, place, outcome);
        if row.enrichment == PlaceEnrichment::Unresolved {
            unresolved += 1;
        }
        let (_, created) = row.upsert(&mut tx).await?;
        if created {
            places_created += 1;
        }
    }

    tx.commit().await?;
    Ok(SavedPlacesWritten {
        lists_created: usize::from(list_created),
        places_created,
        unresolved,
    })
}

/// One `places` row, merged from what the file said and what the lookup found.
struct PlaceWrite {
    user_id: Uuid,
    list_id: Uuid,
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
    enrichment: PlaceEnrichment,
    enriched_at: Option<DateTime<Utc>>,
}

impl PlaceWrite {
    fn new(user_id: Uuid, list_id: Uuid, place: &SavedPlace, outcome: &Outcome) -> Self {
        let notes = [
            place.note.as_deref(),
            place.comment.as_deref(),
            place.url.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

        let mut write = Self {
            user_id,
            list_id,
            name: place.title.clone(),
            address: place.address.clone(),
            lat: None,
            lng: None,
            place_id: None,
            primary_type: None,
            types: Vec::new(),
            metro: None,
            locality: None,
            country: None,
            notes: (!notes.is_empty()).then_some(notes),
            enrichment: PlaceEnrichment::Unresolved,
            enriched_at: None,
        };

        if let Outcome::Resolved(resolved) = outcome {
            if !resolved.name.trim().is_empty() {
                write.name = resolved.name.clone();
            }
            write.address = resolved.address.clone().or(write.address);
            write.lat = Some(resolved.lat);
            write.lng = Some(resolved.lng);
            write.place_id = resolved.place_id.clone();
            write.primary_type = resolved.primary_type.clone();
            write.types = resolved.types.clone();
            write.metro = resolved.metro.clone();
            write.locality = resolved.locality.clone();
            write.country = resolved.country.clone();
            write.enrichment = PlaceEnrichment::Resolved;
            write.enriched_at = Some(Utc::now());
        }
        write
    }

    /// Insert, or merge into the row this place already is.
    ///
    /// The conflict target is the unique index on `(user_id, place_key)`, where
    /// `place_key` is generated by Postgres — so dedupe holds even if two
    /// uploads of the same file run at once. A re-upload of an unresolved row
    /// keeps whatever the first import learned rather than clearing it.
    async fn upsert(&self, tx: &mut Transaction<'_, Postgres>) -> Result<(Uuid, bool), AppError> {
        let row: (Uuid, bool) = sqlx::query_as(
            "INSERT INTO places \
                (id, user_id, list_id, name, address, lat, lng, place_id, primary_type, types, \
                 metro, locality, country, notes, source, enrichment, enriched_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17) \
             ON CONFLICT (user_id, place_key) DO UPDATE SET \
                list_id = coalesce(EXCLUDED.list_id, places.list_id), \
                address = coalesce(EXCLUDED.address, places.address), \
                lat = coalesce(EXCLUDED.lat, places.lat), \
                lng = coalesce(EXCLUDED.lng, places.lng), \
                place_id = coalesce(EXCLUDED.place_id, places.place_id), \
                primary_type = coalesce(EXCLUDED.primary_type, places.primary_type), \
                types = CASE WHEN cardinality(EXCLUDED.types) > 0 \
                             THEN EXCLUDED.types ELSE places.types END, \
                metro = coalesce(EXCLUDED.metro, places.metro), \
                locality = coalesce(EXCLUDED.locality, places.locality), \
                country = coalesce(EXCLUDED.country, places.country), \
                notes = coalesce(EXCLUDED.notes, places.notes), \
                enriched_at = coalesce(EXCLUDED.enriched_at, places.enriched_at), \
                enrichment = CASE WHEN EXCLUDED.enrichment = 'resolved' \
                                  THEN 'resolved' ELSE places.enrichment END \
             RETURNING id, (xmax = 0) AS created",
        )
        .bind(Uuid::new_v4())
        .bind(self.user_id)
        .bind(self.list_id)
        .bind(&self.name)
        .bind(&self.address)
        .bind(self.lat)
        .bind(self.lng)
        .bind(&self.place_id)
        .bind(&self.primary_type)
        .bind(&self.types)
        .bind(&self.metro)
        .bind(&self.locality)
        .bind(&self.country)
        .bind(&self.notes)
        .bind(SOURCE)
        .bind(self.enrichment.as_str())
        .bind(self.enriched_at)
        .fetch_one(&mut **tx)
        .await?;
        Ok(row)
    }
}

/// Columns every `PlaceSummary` needs, in the order [`PlaceRow`] reads them.
const PLACE_SELECT_BY_LIST: &str = "SELECT id, list_id, name, address, lat, lng, place_id, \
     primary_type, types, metro, locality, country, notes, source, enrichment, enriched_at \
     FROM places WHERE list_id = $1 AND user_id = $2 ORDER BY created_at, name";

const PLACE_SELECT_BY_USER: &str = "SELECT id, list_id, name, address, lat, lng, place_id, \
     primary_type, types, metro, locality, country, notes, source, enrichment, enriched_at \
     FROM places WHERE user_id = $1 AND lat IS NOT NULL AND lng IS NOT NULL \
     ORDER BY name";

#[derive(Debug, sqlx::FromRow)]
struct PlaceRow {
    id: Uuid,
    list_id: Option<Uuid>,
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
    enrichment: String,
    enriched_at: Option<DateTime<Utc>>,
}

impl PlaceRow {
    fn into_summary(self) -> PlaceSummary {
        PlaceSummary {
            id: self.id,
            list_id: self.list_id,
            name: self.name,
            address: self.address,
            lat: self.lat,
            lng: self.lng,
            place_id: self.place_id,
            primary_type: self.primary_type,
            types: self.types,
            metro: self.metro,
            locality: self.locality,
            country: self.country,
            notes: self.notes,
            source: self.source,
            enrichment: PlaceEnrichment::from_stored(&self.enrichment),
            enriched_at: self.enriched_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn visit(place_id: Option<&str>, name: &str, kind: Option<&str>) -> Visit {
        Visit {
            name: name.to_owned(),
            address: None,
            lat: Some(35.005),
            lng: Some(135.764),
            place_id: place_id.map(str::to_owned),
            primary_type: kind.map(str::to_owned),
            types: kind.map(|kind| vec![kind.to_owned()]).unwrap_or_default(),
            occurred_at: Utc.with_ymd_and_hms(2024, 3, 1, 10, 0, 0).unwrap(),
        }
    }

    #[test]
    fn a_visit_note_carries_a_geo_hook_and_a_human_line() {
        let body = visit_body(&visit(Some("ChIJabc"), "Nishiki Market", Some("market")));
        assert!(body.starts_with("Visited Nishiki Market"), "{body}");
        assert!(body.contains("geo:35.00500,135.76400"), "{body}");
        assert!(body.contains("2024-03-01 10:00 UTC"), "{body}");
    }

    #[test]
    fn the_tag_hint_comes_from_the_place_type_not_the_business_name() {
        assert_eq!(
            tag_hint(&visit(Some("ChIJabc"), "Nishiki Market", Some("market"))),
            "Market"
        );
        assert_eq!(
            tag_hint(&visit(None, "Kyoto Station", None)),
            VISIT_FALLBACK_TAG
        );
    }

    #[test]
    fn the_same_visit_always_produces_the_same_dedupe_key() {
        let first = visit(Some("ChIJabc"), "Nishiki Market", Some("market"));
        let mut second = first.clone();
        second.name = "NISHIKI MARKET".to_owned();
        assert_eq!(visit_dedupe_key(&first), visit_dedupe_key(&second));

        let mut later = first.clone();
        later.occurred_at = Utc.with_ymd_and_hms(2024, 3, 1, 12, 0, 0).unwrap();
        assert_ne!(visit_dedupe_key(&first), visit_dedupe_key(&later));
    }

    #[test]
    fn a_file_named_list_keeps_its_stem_so_a_reupload_finds_it() {
        let upload = Upload {
            text: String::new(),
            name: None,
            file_name: Some("Saved Places.csv".to_owned()),
        };
        assert_eq!(upload.list_name(), "Saved Places");

        let unnamed = Upload {
            text: String::new(),
            name: None,
            file_name: None,
        };
        assert_eq!(unnamed.list_name(), "Saved places");
    }

    #[test]
    fn an_unresolved_row_is_marked_and_keeps_everything_the_file_said() {
        let place = SavedPlace {
            title: "Blue Bottle".to_owned(),
            note: Some("go early".to_owned()),
            comment: Some("cash only".to_owned()),
            url: Some("https://maps.app.goo.gl/x".to_owned()),
            address: None,
        };
        let write = PlaceWrite::new(Uuid::nil(), Uuid::nil(), &place, &Outcome::Unresolved);
        assert_eq!(write.enrichment, PlaceEnrichment::Unresolved);
        assert_eq!(write.lat, None);
        assert_eq!(write.name, "Blue Bottle");
        assert_eq!(
            write.notes.as_deref(),
            Some("go early\ncash only\nhttps://maps.app.goo.gl/x")
        );
    }

    #[test]
    fn a_resolved_row_takes_the_looked_up_name_and_coordinates() {
        let place = SavedPlace {
            title: "katz deli".to_owned(),
            note: None,
            comment: None,
            url: None,
            address: Some("205 E Houston St".to_owned()),
        };
        let outcome = Outcome::Resolved(Box::new(ResolvedPlace {
            name: "Katz's Delicatessen".to_owned(),
            address: Some("205 E Houston St, New York, NY 10002".to_owned()),
            lat: 40.7223,
            lng: -73.9874,
            place_id: Some("ChIJkatz".to_owned()),
            primary_type: Some("deli".to_owned()),
            types: vec!["deli".to_owned()],
            metro: Some("New York County".to_owned()),
            locality: Some("New York".to_owned()),
            state: Some("New York".to_owned()),
            country: Some("United States".to_owned()),
        }));
        let write = PlaceWrite::new(Uuid::nil(), Uuid::nil(), &place, &outcome);
        assert_eq!(write.name, "Katz's Delicatessen");
        assert_eq!(write.lat, Some(40.7223));
        assert_eq!(write.enrichment, PlaceEnrichment::Resolved);
        assert_eq!(write.country.as_deref(), Some("United States"));
    }
}
