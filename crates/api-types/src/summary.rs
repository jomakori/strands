//! Summary DTOs: `POST /api/v1/tags/{id}/summary` (generate/regenerate),
//! `GET /api/v1/tags/{id}/summary/history` (ticket T7), and the review actions
//! approve / discard / edit (ticket T8).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Longest edited summary body, in characters. Mirrors the notes limit so an
/// edit cannot smuggle an unbounded blob into the versions list.
pub const MAX_SUMMARY_BODY_CHARS: usize = 5000;

/// What has happened to one version. A generated version is a proposal
/// (`proposed`) that nobody has committed to yet; approving it makes it the
/// tag's summary, discarding drops it, and `edited` marks a proposal a person
/// rewrote (T8: an edit is a new version, never a mutation of the old one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryState {
    Proposed,
    Approved,
    Discarded,
    Edited,
    Failed,
}

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
    pub state: SummaryState,
    pub created_at: DateTime<Utc>,
}

/// Body of `GET /api/v1/tags/{id}/summary/history`, newest version first, plus
/// which version is in force. `approved_version_id` is the tag's summary: `None`
/// means nothing has been approved yet and the card is still showing a proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryHistory {
    pub versions: Vec<SummaryVersion>,
    pub approved_version_id: Option<Uuid>,
}

/// Body of `POST /api/v1/tags/{id}/summary/{version}/edit` — the rewritten body.
/// The server records it as a new version; the version being edited keeps its
/// own body and state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditSummaryRequest {
    pub body: String,
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
            state: SummaryState::Edited,
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&version).expect("serialize");
        let back: SummaryVersion = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(version, back);
    }

    #[test]
    fn states_are_snake_case_on_the_wire() {
        for (state, wire) in [
            (SummaryState::Proposed, "\"proposed\""),
            (SummaryState::Approved, "\"approved\""),
            (SummaryState::Discarded, "\"discarded\""),
            (SummaryState::Edited, "\"edited\""),
        ] {
            assert_eq!(serde_json::to_string(&state).expect("serialize"), wire);
        }
    }

    #[test]
    fn history_names_the_approved_version_and_allows_none() {
        let history = SummaryHistory {
            versions: Vec::new(),
            approved_version_id: None,
        };
        let json = serde_json::to_string(&history).expect("serialize");
        assert_eq!(json, r#"{"versions":[],"approved_version_id":null}"#);
    }
}
