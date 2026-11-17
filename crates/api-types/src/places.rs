//! Place cache, backfill and Nearby Search DTOs (ticket T21).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Upper bound on one backfill request: a Timeline export can hold years of
/// visits, but a single request should be one bounded batch the caller can
/// retry, not an unbounded job.
pub const MAX_BACKFILL_ITEMS: usize = 500;

/// Nearby Search is tap-only and never grows the note list by itself; ten cards
/// is the whole point of the surface, so the cap is part of the contract.
pub const MAX_NEARBY_CANDIDATES: usize = 10;

/// Which export a backfill row came from. Kept in the request so the response
/// and logs can say which lane resolved what; both lanes use the same cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackfillSource {
    /// EXIF GPS from an imported photo.
    Exif,
    /// A visit row from a Google Timeline export.
    Timeline,
}

/// One coordinate to reverse-geocode. `lat`/`lng` are optional so a malformed
/// export row is counted as skipped instead of rejecting the whole batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackfillItem {
    pub source: BackfillSource,
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    /// Optional human label (photo file name, Timeline place name), echoed in
    /// the resolved place so the caller can line results up with its input.
    #[serde(default)]
    pub label: Option<String>,
    /// When the visit or photo happened, if the export carried it.
    #[serde(default)]
    pub occurred_at: Option<DateTime<Utc>>,
}

/// A batch of coordinates from one or both export lanes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackfillRequest {
    pub items: Vec<BackfillItem>,
}

/// The administrative answer for one rounded coordinate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackfillPlace {
    /// The cache key: the coordinate rounded to four decimals (~11 m).
    pub coord_key: String,
    pub lat: f64,
    pub lng: f64,
    pub label: Option<String>,
    pub locality: Option<String>,
    pub county: Option<String>,
    pub state: Option<String>,
    pub country: Option<String>,
    /// `Metro: <locality>`, when the provider returned a locality component.
    pub metro_tag: Option<String>,
}

/// Counts for one batch, including cache traffic so a caller can see that a
/// repeat run was served from Postgres rather than from the provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackfillResult {
    /// Items in the request, including malformed and duplicate ones.
    pub seen: usize,
    /// Items that produced a cached or freshly resolved place.
    pub resolved: usize,
    /// Items not resolved: bad coordinates, a duplicate key inside the batch,
    /// or the provider knowing nothing about the point.
    pub skipped: usize,
    /// Cache lookups answered by a row already in Postgres (no provider call).
    pub cache_hits: usize,
    /// Cache lookups that had to ask the provider.
    pub cache_misses: usize,
    /// One entry per distinct rounded key that resolved.
    pub places: Vec<BackfillPlace>,
}

/// Nearby Search around a point. `radius_m` defaults to 800 m and is clamped by
/// the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NearbyRequest {
    pub lat: f64,
    pub lng: f64,
    #[serde(default)]
    pub radius_m: Option<u32>,
}

/// One staged result. `state` is `pending`, `accepted` or `dismissed`; a
/// candidate only becomes a note through the accept route.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NearbyCandidate {
    pub id: Uuid,
    pub name: String,
    pub address: Option<String>,
    pub lat: f64,
    pub lng: f64,
    pub place_id: Option<String>,
    pub primary_type: Option<String>,
    pub poi_tag: String,
    pub metro_tag: Option<String>,
    pub state: String,
}

/// At most [`MAX_NEARBY_CANDIDATES`] staged candidates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NearbyResult {
    pub candidates: Vec<NearbyCandidate>,
}

/// What a tap produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptedPlace {
    pub candidate_id: Uuid,
    pub note_id: Uuid,
    pub tag_id: Uuid,
    pub name: String,
}
