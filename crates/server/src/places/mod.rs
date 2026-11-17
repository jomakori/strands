//! Place tagging lane (ticket T21): geocode cache, export backfill, and the
//! tap-only Nearby Search surface.
//!
//! The cache is the load-bearing part. Reverse geocoding costs money and a
//! Timeline export repeats the same coordinate many times, so every lookup is
//! keyed on the coordinate rounded to four decimals (~11 m) and persisted per
//! user. A repeat request is answered from `geocode_cache`; [`BackfillResult`]
//! reports `cache_hits`/`cache_misses` so "served from cache" is observable in
//! the response instead of inferred from the bill.
//!
//! Nearby Search never writes a note. It stages at most ten candidates in
//! `place_candidates`; a candidate becomes a note only when the user accepts it.

use std::collections::HashMap;

use axum::Json;
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::routing::post;
use sqlx::PgPool;
use strands_api_types::{
    AcceptedPlace, BackfillItem, BackfillPlace, BackfillRequest, BackfillResult,
    MAX_BACKFILL_ITEMS, MAX_NEARBY_CANDIDATES, NearbyCandidate, NearbyRequest, NearbyResult,
};
use strands_core::{NoteKind, PoiTag, Source, metro_tag};
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::gmaps::places::{PlaceError, ResolvedPlace};
use crate::ingest::TagIndex;

/// Latitude/longitude are rounded to four decimals before they become a cache
/// key: `0.0001°` is roughly 11 m, close enough that two GPS fixes from the
/// same doorway share one provider call, and coarse enough that the cache does
/// not grow one row per metre.
pub const COORD_DECIMALS: u32 = 4;

/// Default Nearby Search radius, matching the map surface's first ring.
const DEFAULT_NEARBY_RADIUS_M: u32 = 800;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/places/backfill", post(backfill))
        .route("/api/v1/places/nearby", post(nearby))
        .route(
            "/api/v1/places/candidates/{candidate_id}/accept",
            post(accept_candidate),
        )
}

/// Round a coordinate to [`COORD_DECIMALS`] decimals.
pub fn rounded(value: f64) -> f64 {
    let factor = 10f64.powi(COORD_DECIMALS as i32);
    (value * factor).round() / factor
}

/// Stable cache key for a coordinate: the rounded pair, fixed-width so `-0.0`
/// and `0.0` cannot become two rows.
pub fn coord_key(lat: f64, lng: f64) -> String {
    format!("{:.4},{:.4}", rounded(lat), rounded(lng))
}

/// Reject `NaN`, infinities and coordinates outside the globe. A bad export row
/// is counted as skipped rather than geocoded into nonsense.
pub fn valid_coord(lat: f64, lng: f64) -> bool {
    lat.is_finite()
        && lng.is_finite()
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lng)
}

/// `geocode_cache` columns read back in one shot.
type CacheRow = (
    bool,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// Candidate columns needed to turn a staged tap into a note.
type CandidateRow = (
    String,
    Option<String>,
    Option<String>,
    Option<f64>,
    Option<f64>,
    Option<Uuid>,
);

/// One cached administrative answer. `found == false` is a cached negative:
/// the provider was asked and knew nothing, which is still worth remembering.
#[derive(Debug, Clone)]
struct CachedAdministrative {
    found: bool,
    locality: Option<String>,
    county: Option<String>,
    state: Option<String>,
    country: Option<String>,
}

impl CachedAdministrative {
    fn into_place(self, lat: f64, lng: f64) -> Option<ResolvedPlace> {
        if !self.found {
            return None;
        }
        Some(ResolvedPlace {
            name: String::new(),
            address: None,
            lat,
            lng,
            place_id: None,
            primary_type: None,
            types: Vec::new(),
            metro: self.county,
            locality: self.locality,
            state: self.state,
            country: self.country,
        })
    }
}

async fn cache_lookup(
    pool: &PgPool,
    user_id: Uuid,
    key: &str,
) -> Result<Option<CachedAdministrative>, AppError> {
    let row: Option<CacheRow> = sqlx::query_as(
        "SELECT found, locality, county, state, country \
             FROM geocode_cache WHERE user_id = $1 AND coord_key = $2",
    )
    .bind(user_id)
    .bind(key)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(
        |(found, locality, county, state, country)| CachedAdministrative {
            found,
            locality,
            county,
            state,
            country,
        },
    ))
}

async fn cache_store(
    pool: &PgPool,
    user_id: Uuid,
    key: &str,
    lat: f64,
    lng: f64,
    place: Option<&ResolvedPlace>,
) -> Result<(), AppError> {
    let found = place.is_some();
    let locality = place.and_then(|place| place.locality.clone());
    let county = place.and_then(|place| place.metro.clone());
    let state = place.and_then(|place| place.state.clone());
    let country = place.and_then(|place| place.country.clone());

    sqlx::query(
        "INSERT INTO geocode_cache \
            (user_id, coord_key, lat, lng, found, locality, county, state, country) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (user_id, coord_key) DO UPDATE SET \
            found = EXCLUDED.found, \
            locality = COALESCE(EXCLUDED.locality, geocode_cache.locality), \
            county = COALESCE(EXCLUDED.county, geocode_cache.county), \
            state = COALESCE(EXCLUDED.state, geocode_cache.state), \
            country = COALESCE(EXCLUDED.country, geocode_cache.country)",
    )
    .bind(user_id)
    .bind(key)
    .bind(rounded(lat))
    .bind(rounded(lng))
    .bind(found)
    .bind(locality)
    .bind(county)
    .bind(state)
    .bind(country)
    .execute(pool)
    .await?;

    Ok(())
}

/// Reverse-geocode with the cache in front of the provider.
///
/// Returns the place (or `None` for a cached/known-empty answer) and whether
/// the cache answered. The provider is called only on a miss, and the miss is
/// recorded so the caller can report `cache_hits`/`cache_misses`.
async fn cached_reverse(
    state: &AppState,
    user: &CurrentUser,
    lat: f64,
    lng: f64,
    key: &str,
) -> Result<(Option<ResolvedPlace>, bool), AppError> {
    if let Some(cached) = cache_lookup(&state.pool, user.id, key).await? {
        let place = cached.into_place(rounded(lat), rounded(lng));
        return Ok((place, true));
    }

    let place = state
        .geocoder
        .reverse(lat, lng)
        .await
        .map_err(|error: PlaceError| AppError::Unavailable(error.to_string()))?;

    cache_store(&state.pool, user.id, key, lat, lng, place.as_ref()).await?;
    Ok((place, false))
}

/// `POST /api/v1/places/backfill` — EXIF GPS and Timeline batches.
///
/// Duplicate keys inside one batch are geocoded once: the second occurrence is
/// counted as skipped, not sent to the provider again.
pub async fn backfill(
    State(state): State<AppState>,
    user: CurrentUser,
    payload: Result<Json<BackfillRequest>, JsonRejection>,
) -> Result<Json<BackfillResult>, AppError> {
    let Json(request) =
        payload.map_err(|_| AppError::BadRequest("invalid backfill body".into()))?;
    if request.items.is_empty() {
        return Err(AppError::BadRequest(
            "backfill needs at least one item".into(),
        ));
    }
    if request.items.len() > MAX_BACKFILL_ITEMS {
        return Err(AppError::BadRequest(format!(
            "backfill accepts at most {MAX_BACKFILL_ITEMS} items per request"
        )));
    }

    // Keyed by rounded coordinate so the second copy of a place in the same
    // export never spends a second provider call.
    let mut batch: HashMap<String, Option<ResolvedPlace>> = HashMap::new();
    let mut result = BackfillResult {
        seen: 0,
        resolved: 0,
        skipped: 0,
        cache_hits: 0,
        cache_misses: 0,
        places: Vec::new(),
    };

    for BackfillItem {
        lat, lng, label, ..
    } in request.items
    {
        result.seen += 1;

        let (Some(lat), Some(lng)) = (lat, lng) else {
            result.skipped += 1;
            continue;
        };
        if !valid_coord(lat, lng) {
            result.skipped += 1;
            continue;
        }

        let key = coord_key(lat, lng);
        if batch.contains_key(&key) {
            result.skipped += 1;
            continue;
        }

        let (place, hit) = cached_reverse(&state, &user, lat, lng, &key).await?;
        if hit {
            result.cache_hits += 1;
        } else {
            result.cache_misses += 1;
        }

        if let Some(place) = &place {
            result.resolved += 1;
            result.places.push(BackfillPlace {
                coord_key: key.clone(),
                lat: rounded(lat),
                lng: rounded(lng),
                label,
                locality: place.locality.clone(),
                county: place.metro.clone(),
                state: place.state.clone(),
                country: place.country.clone(),
                metro_tag: metro_tag(place.locality.as_deref()),
            });
        } else {
            result.skipped += 1;
        }

        batch.insert(key, place);
    }

    Ok(Json(result))
}

/// Stable identity for a staged card: Google's place id when present, otherwise
/// the rounded coordinate plus the normalized name.
fn candidate_key(place: &ResolvedPlace) -> String {
    match place
        .place_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(place_id) => format!("place:{place_id}"),
        None => {
            let name = place
                .name
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_lowercase();
            format!("geo:{}:{}", coord_key(place.lat, place.lng), name)
        }
    }
}

/// `POST /api/v1/places/nearby` — stage at most ten candidates, add no notes.
pub async fn nearby(
    State(state): State<AppState>,
    user: CurrentUser,
    payload: Result<Json<NearbyRequest>, JsonRejection>,
) -> Result<Json<NearbyResult>, AppError> {
    let Json(request) = payload.map_err(|_| AppError::BadRequest("invalid nearby body".into()))?;
    if !valid_coord(request.lat, request.lng) {
        return Err(AppError::BadRequest("nearby needs a valid lat/lng".into()));
    }
    let radius_m = request
        .radius_m
        .unwrap_or(DEFAULT_NEARBY_RADIUS_M)
        .clamp(1, 50_000);

    let places = state
        .geocoder
        .nearby(request.lat, request.lng, radius_m, MAX_NEARBY_CANDIDATES)
        .await
        .map_err(|error: PlaceError| AppError::Unavailable(error.to_string()))?;

    let mut seen = std::collections::HashSet::new();
    let mut candidates = Vec::new();

    for place in places {
        if candidates.len() >= MAX_NEARBY_CANDIDATES {
            break;
        }
        let Some(lat) = Some(place.lat).filter(|value| valid_coord(*value, place.lng)) else {
            continue;
        };
        let key = candidate_key(&place);
        if !seen.insert(key.clone()) {
            continue;
        }

        let poi_tag = PoiTag::from_primary_type(place.primary_type.as_deref());
        let metro = metro_tag(place.locality.as_deref());
        let id = Uuid::new_v4();

        // Re-tapping Nearby refreshes the staged card but never un-accepts one:
        // `state` is deliberately not in the UPDATE list.
        let (id, status): (Uuid, String) = sqlx::query_as(
            "INSERT INTO place_candidates \
                (id, user_id, candidate_key, name, address, lat, lng, place_id, \
                 primary_type, types, poi_tag, metro) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::jsonb, $11, $12) \
             ON CONFLICT (user_id, candidate_key) DO UPDATE SET \
                name = EXCLUDED.name, \
                address = EXCLUDED.address, \
                lat = EXCLUDED.lat, \
                lng = EXCLUDED.lng, \
                place_id = EXCLUDED.place_id, \
                primary_type = EXCLUDED.primary_type, \
                types = EXCLUDED.types, \
                poi_tag = EXCLUDED.poi_tag, \
                metro = EXCLUDED.metro, \
                found_at = now() \
             RETURNING id, state",
        )
        .bind(id)
        .bind(user.id)
        .bind(&key)
        .bind(&place.name)
        .bind(place.address.as_deref())
        .bind(lat)
        .bind(place.lng)
        .bind(place.place_id.as_deref())
        .bind(place.primary_type.as_deref())
        .bind(serde_json::to_string(&place.types).unwrap_or_else(|_| "[]".into()))
        .bind(poi_tag.as_str())
        .bind(metro.as_deref())
        .fetch_one(&state.pool)
        .await?;

        candidates.push(NearbyCandidate {
            id,
            name: place.name,
            address: place.address,
            lat,
            lng: place.lng,
            place_id: place.place_id,
            primary_type: place.primary_type,
            poi_tag: poi_tag.as_str().to_owned(),
            metro_tag: metro,
            state: status,
        });
    }

    Ok(Json(NearbyResult { candidates }))
}

/// `POST /api/v1/places/candidates/{id}/accept` — the tap that makes a note.
///
/// Selecting by `(id, user_id)` is the whole authorization story: another
/// user's candidate is simply not found, and the answer is 404, not 403.
pub async fn accept_candidate(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(candidate_id): Path<Uuid>,
) -> Result<Json<AcceptedPlace>, AppError> {
    let mut tx = state.pool.begin().await?;

    let row: Option<CandidateRow> = sqlx::query_as(
        "SELECT name, address, poi_tag, lat, lng, note_id \
         FROM place_candidates WHERE id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(candidate_id)
    .bind(user.id)
    .fetch_optional(&mut *tx)
    .await?;

    let (name, address, poi_tag, lat, lng, existing_note) = row.ok_or(AppError::NotFound)?;

    // A double tap returns the note the first tap made instead of writing a
    // second one.
    if let Some(note_id) = existing_note {
        let tag_id: Option<Uuid> =
            sqlx::query_scalar("SELECT tag_id FROM notes WHERE id = $1 AND user_id = $2")
                .bind(note_id)
                .bind(user.id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(tag_id) = tag_id {
            tx.commit().await?;
            return Ok(Json(AcceptedPlace {
                candidate_id,
                note_id,
                tag_id,
                name,
            }));
        }
    }

    let category_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM categories WHERE user_id = $1 ORDER BY position, name LIMIT 1",
    )
    .bind(user.id)
    .fetch_optional(&mut *tx)
    .await?;
    let category_id = category_id
        .ok_or_else(|| AppError::BadRequest("no category exists to file a new tag under".into()))?;

    let poi_tag = poi_tag.unwrap_or_else(|| PoiTag::Other.as_str().to_owned());
    let mut index = TagIndex::load(&mut tx, user.id, category_id).await?;
    let tag_id = index.resolve(&mut tx, user.id, &poi_tag).await?;

    let metro: Option<String> =
        sqlx::query_scalar("SELECT metro FROM place_candidates WHERE id = $1 AND user_id = $2")
            .bind(candidate_id)
            .bind(user.id)
            .fetch_optional(&mut *tx)
            .await?
            .flatten();
    let metro_tag_id = match metro.as_deref() {
        Some(name) if !name.trim().is_empty() => Some(index.resolve(&mut tx, user.id, name).await?),
        _ => None,
    };

    let note_id = Uuid::new_v4();
    let coordinates = match (lat, lng) {
        (Some(lat), Some(lng)) => format!("geo:{lat:.4},{lng:.4}"),
        _ => String::new(),
    };
    let body = [
        name.as_str(),
        address.as_deref().unwrap_or(""),
        coordinates.as_str(),
    ]
    .iter()
    .filter(|line| !line.trim().is_empty())
    .copied()
    .collect::<Vec<_>>()
    .join("\n");

    let note_id: Uuid = sqlx::query_scalar(
        "INSERT INTO notes \
            (id, user_id, tag_id, kind, body, occurred_at, source, dedupe_key, meta) \
         VALUES ($1, $2, $3, $4, $5, now(), $6, $7, $8::jsonb) \
         ON CONFLICT (user_id, dedupe_key) DO UPDATE SET body = notes.body \
         RETURNING id",
    )
    .bind(note_id)
    .bind(user.id)
    .bind(tag_id)
    .bind(NoteKind::Visit)
    .bind(&body)
    .bind(Source::GoogleMaps)
    .bind(format!("place-candidate:{candidate_id}"))
    .bind(
        serde_json::json!({
            "place_id": null,
            "lat": lat,
            "lng": lng,
            "accepted_from": "nearby",
        })
        .to_string(),
    )
    .fetch_one(&mut *tx)
    .await?;

    if let Some(metro_tag_id) = metro_tag_id {
        sqlx::query(
            "INSERT INTO note_tags (note_id, tag_id, user_id) VALUES ($1, $2, $3) \
             ON CONFLICT DO NOTHING",
        )
        .bind(note_id)
        .bind(metro_tag_id)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query(
        "UPDATE place_candidates SET state = 'accepted', note_id = $3 \
         WHERE id = $1 AND user_id = $2",
    )
    .bind(candidate_id)
    .bind(user.id)
    .bind(note_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Json(AcceptedPlace {
        candidate_id,
        note_id,
        tag_id,
        name,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_decimals_is_the_cache_granularity() {
        assert_eq!(coord_key(37.77491, -122.41941), "37.7749,-122.4194");
        // Two fixes 5 m apart round to the same key, so they share one call.
        assert_eq!(
            coord_key(37.77491, -122.41941),
            coord_key(37.77486, -122.41944)
        );
        // ~22 m apart is a different key.
        assert_ne!(
            coord_key(37.77491, -122.41941),
            coord_key(37.77511, -122.41941)
        );
    }

    #[test]
    fn invalid_coordinates_are_not_geocoded() {
        assert!(!valid_coord(f64::NAN, 0.0));
        assert!(!valid_coord(91.0, 0.0));
        assert!(!valid_coord(0.0, 181.0));
        assert!(valid_coord(0.0, 0.0));
    }

    #[test]
    fn candidate_identity_prefers_place_id() {
        let mut place = ResolvedPlace {
            name: "Blue Bottle".into(),
            address: None,
            lat: 37.7749,
            lng: -122.4194,
            place_id: Some("ChIJ123".into()),
            primary_type: Some("cafe".into()),
            types: vec![],
            metro: None,
            locality: None,
            state: None,
            country: None,
        };
        assert_eq!(candidate_key(&place), "place:ChIJ123");
        place.place_id = None;
        assert_eq!(candidate_key(&place), "geo:37.7749,-122.4194:blue bottle");
    }
}
