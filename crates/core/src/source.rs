//! Origin of a note: manual capture or an import connector (see spec §6 D10).

use serde::{Deserialize, Serialize};
use sqlx::Type;

/// Where a note's content came from. Mirrors the `note_source` Postgres enum
/// in `migrations/0001_init.sql` (spec §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "note_source", rename_all = "snake_case")]
pub enum Source {
    /// Typed directly by the user (quick capture, D9).
    Manual,
    /// Imported via the Google Photos Picker connector.
    GooglePhotos,
    /// Imported via the WhatsApp bridge connector.
    #[sqlx(rename = "whatsapp")]
    WhatsApp,
    /// Imported via the Google Maps file-ingest connector.
    GoogleMaps,
}

impl Source {
    /// Stable string form stored in `notes.source` (spec §6 data model).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::GooglePhotos => "google_photos",
            Self::WhatsApp => "whatsapp",
            Self::GoogleMaps => "google_maps",
        }
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_round_trips_through_serde_json() {
        for source in [
            Source::Manual,
            Source::GooglePhotos,
            Source::WhatsApp,
            Source::GoogleMaps,
        ] {
            let json = serde_json::to_string(&source).expect("serialize");
            let back: Source = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(source, back);
        }
    }

    #[test]
    fn manual_as_str_is_stable() {
        assert_eq!(Source::Manual.as_str(), "manual");
    }

    #[test]
    fn display_matches_as_str() {
        assert_eq!(Source::WhatsApp.to_string(), "whatsapp");
    }
}
