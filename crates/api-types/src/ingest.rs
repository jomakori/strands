//! Request and response bodies for `POST /api/v1/ingest` (ticket T9).
//!
//! Ingest is the batch door: items arrive, the cheap model assigns each a tag,
//! and the server files them in one transaction with no review step.

use serde::{Deserialize, Serialize};

use crate::TagSummary;

/// Body of `POST /api/v1/ingest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestRequest {
    pub items: Vec<IngestItem>,
}

/// One item offered for ingest.
///
/// `external_id` is the caller's stable key for the item (a connector's message
/// id, say). Re-sending the same item — with or without an `external_id` —
/// never creates a second note or tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestItem {
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
}

/// Body of the ingest response: the tags the run touched, ready for the grid,
/// plus how many new notes landed (0 on a repeat run over the same items).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestResult {
    pub tags: Vec<TagSummary>,
    pub notes_created: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_request_round_trips_through_json() {
        let request = IngestRequest {
            items: vec![
                IngestItem {
                    body: "coffee with Ana".to_owned(),
                    external_id: Some("wa-1".to_owned()),
                },
                IngestItem {
                    body: "flight to Kyoto".to_owned(),
                    external_id: None,
                },
            ],
        };
        let json = serde_json::to_string(&request).expect("serialize");
        let back: IngestRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(request, back);
    }

    #[test]
    fn a_missing_external_id_is_none() {
        let item: IngestItem = serde_json::from_str(r#"{"body":"note"}"#).expect("deserialize");
        assert_eq!(item.external_id, None);
    }
}
