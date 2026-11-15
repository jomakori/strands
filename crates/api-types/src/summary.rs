//! Summary DTOs: `POST /api/v1/tags/{id}/summary` (generate/regenerate) and
//! `GET /api/v1/tags/{id}/summary/history` (ticket T7).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What guided a generation: one of four fixed presets, or the caller's own
/// instructions. Internally tagged so the wire shape is `{"kind": "shorter"}`
/// or `{"kind": "freeform", "text": "..."}` — a client never needs a second
/// field to know which preset was picked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SummaryGuide {
    Shorter,
    Takeaways,
    Detail,
    Dates,
    Freeform { text: String },
}

/// Body of `POST /api/v1/tags/{id}/summary`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerateSummaryRequest {
    pub guide: SummaryGuide,
    /// Force a new version even when the notes and guide are unchanged from
    /// the newest one — the "regenerate" action. Defaults to `false`, the
    /// "generate" action, which short-circuits on an unchanged source.
    #[serde(default)]
    pub force: bool,
}

/// One version of a tag's summary, as returned by generate and by history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryVersion {
    pub id: Uuid,
    pub tag_id: Uuid,
    /// 1-based, increasing per tag; the newest version has the highest number.
    pub version: i32,
    pub body: String,
    pub guide: SummaryGuide,
    /// The LiteLLM alias this version was generated under (spec acceptance:
    /// "history lists versions with the model alias").
    pub model_alias: String,
    pub created_at: DateTime<Utc>,
}

/// Body of `GET /api/v1/tags/{id}/summary/history`, newest version first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryHistory {
    pub versions: Vec<SummaryVersion>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preset_guide_round_trips_through_json() {
        let request = GenerateSummaryRequest {
            guide: SummaryGuide::Takeaways,
            force: false,
        };
        let json = serde_json::to_string(&request).expect("serialize");
        assert_eq!(json, r#"{"guide":{"kind":"takeaways"},"force":false}"#);
        let back: GenerateSummaryRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(request, back);
    }

    #[test]
    fn a_freeform_guide_carries_its_text() {
        let request = GenerateSummaryRequest {
            guide: SummaryGuide::Freeform {
                text: "focus on dates and places".to_owned(),
            },
            force: true,
        };
        let json = serde_json::to_string(&request).expect("serialize");
        let back: GenerateSummaryRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(request, back);
    }

    #[test]
    fn force_defaults_to_false_when_omitted() {
        let request: GenerateSummaryRequest =
            serde_json::from_str(r#"{"guide":{"kind":"dates"}}"#).expect("deserialize");
        assert!(!request.force);
    }

    #[test]
    fn summary_version_round_trips_through_json() {
        let version = SummaryVersion {
            id: Uuid::nil(),
            tag_id: Uuid::nil(),
            version: 3,
            body: "Three trips to Tokyo, mostly about trains.".to_owned(),
            guide: SummaryGuide::Shorter,
            model_alias: "strands.summary".to_owned(),
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&version).expect("serialize");
        let back: SummaryVersion = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(version, back);
    }
}
