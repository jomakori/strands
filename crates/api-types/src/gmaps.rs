//! Google Maps ingest and place DTOs (ticket T16).
//!
//! Four endpoints share these shapes: `POST /api/v1/ingest/gmaps`,
//! `GET /api/v1/lists`, `GET /api/v1/lists/{id}/places`, and the KML export
//! (`GET /api/v1/places/kml`, which has no JSON body — its unresolved count
//! travels in a header, see the server route).
//!
//! Kept free of `sqlx`/`axum` so `strands-web` can render a list without
//! pulling a database driver into the wasm bundle.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::TagSummary;

/// Which file shape the upload turned out to be.
///
/// The server decides by content, never by the file name: a Takeout export
/// renamed `Timeline.json` must still import as a list rather than fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GmapsFormat {
    /// `Timeline.json`: `semanticSegments` (2024+) or the older `locations`.
    Timeline,
    /// Takeout "Saved" CSV: `Title,Note,URL,Comment`.
    SavedPlacesCsv,
    /// Takeout "Saved" JSON (the same rows as the CSV, one object per place).
    SavedPlacesJson,
}

/// Body of `POST /api/v1/ingest/gmaps`.
///
/// The counts are the import's receipt: `notes_created` is 0 on a re-upload,
/// and `places_unresolved` is how many rows were kept without coordinates
/// because no address (or no usable lookup) backed them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GmapsIngestResult {
    pub format: GmapsFormat,
    /// Visit entries the file carried, including any dropped as unusable.
    pub visits_seen: usize,
    /// Visit entries skipped because they had no timestamp or no place name.
    pub visits_skipped: usize,
    /// Visit notes written; 0 when the same file was uploaded before.
    pub notes_created: usize,
    /// Saved-place rows the file carried, blank rows already removed.
    pub places_seen: usize,
    /// New `places` rows; a repeat upload updates in place and adds none.
    pub places_created: usize,
    /// Rows left without coordinates, marked `unresolved` rather than dropped.
    pub places_unresolved: usize,
    /// Lists created by this upload; 0 when the list already existed.
    pub lists_created: usize,
    /// The tags the notes were filed under, ready for the grid.
    pub tags: Vec<TagSummary>,
}

/// One row of `GET /api/v1/lists`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaceListSummary {
    pub id: Uuid,
    pub name: String,
    /// Where the list came from (`google_maps_takeout` today).
    pub source: String,
    pub place_count: i64,
    pub created_at: DateTime<Utc>,
}

/// Where a place stands in the enrichment pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaceEnrichment {
    /// Imported before enrichment ran (or the lookup was never attempted).
    Pending,
    /// The lookup found the place: coordinates and type are real.
    Resolved,
    /// No address to look up, or the lookup found nothing. The row is kept.
    Unresolved,
}

impl PlaceEnrichment {
    /// SQL literal form, matching the `places.enrichment` check constraint.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Resolved => "resolved",
            Self::Unresolved => "unresolved",
        }
    }

    /// Map a stored value back; an unknown value reads as `pending` rather
    /// than failing the whole response.
    pub fn from_stored(value: &str) -> Self {
        match value {
            "resolved" => Self::Resolved,
            "unresolved" => Self::Unresolved,
            _ => Self::Pending,
        }
    }
}

/// One row of `GET /api/v1/lists/{id}/places`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaceSummary {
    pub id: Uuid,
    pub list_id: Option<Uuid>,
    pub name: String,
    pub address: Option<String>,
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    pub place_id: Option<String>,
    pub primary_type: Option<String>,
    pub types: Vec<String>,
    pub metro: Option<String>,
    pub locality: Option<String>,
    pub country: Option<String>,
    pub notes: Option<String>,
    pub source: String,
    pub enrichment: PlaceEnrichment,
    pub enriched_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_enrichment_serialize_to_the_stored_literals() {
        assert_eq!(
            serde_json::to_string(&GmapsFormat::SavedPlacesCsv).expect("serialize"),
            "\"saved_places_csv\""
        );
        assert_eq!(PlaceEnrichment::Unresolved.as_str(), "unresolved");
        assert_eq!(
            serde_json::to_string(&PlaceEnrichment::Pending).expect("serialize"),
            "\"pending\""
        );
    }

    #[test]
    fn stored_values_round_trip() {
        for value in [
            PlaceEnrichment::Pending,
            PlaceEnrichment::Resolved,
            PlaceEnrichment::Unresolved,
        ] {
            assert_eq!(PlaceEnrichment::from_stored(value.as_str()), value);
        }
        assert_eq!(
            PlaceEnrichment::from_stored("something-else"),
            PlaceEnrichment::Pending
        );
    }

    #[test]
    fn an_ingest_result_round_trips_through_json() {
        let result = GmapsIngestResult {
            format: GmapsFormat::Timeline,
            visits_seen: 40,
            visits_skipped: 0,
            notes_created: 40,
            places_seen: 0,
            places_created: 0,
            places_unresolved: 0,
            lists_created: 0,
            tags: Vec::new(),
        };
        let json = serde_json::to_string(&result).expect("serialize");
        let back: GmapsIngestResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(result, back);
    }
}
