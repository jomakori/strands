//! Preview/import/history DTOs for the connector select-then-import flow
//! (ticket T33c): `GET .../preview`, `POST .../import`, `GET .../history`.
//!
//! These sit alongside `connector.rs`'s list/status DTOs (T13) but are their
//! own module: T13 answers "what connectors exist and are they connected?",
//! this answers "what can this connector import right now, and what has it
//! already imported?" — two different screens, two different tickets, so
//! kept as separate wire contracts rather than growing one struct to cover
//! both.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// `GET /api/v1/connectors/{connector}/preview`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorPreview {
    pub connector: String,
    pub items: Vec<ConnectorPreviewItem>,
}

/// One item `preview` can currently see, flagged with whether an earlier
/// `import` already landed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorPreviewItem {
    pub item_key: String,
    pub label: String,
    pub timestamp: DateTime<Utc>,
    pub already_imported: bool,
}

/// `POST /api/v1/connectors/{connector}/import` request body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorImportRequest {
    pub item_keys: Vec<String>,
}

/// What `import` did with one requested key, as the nested-status convention
/// in `whatsapp.rs` does: the enum self-tags under the same field name the
/// outer struct stores it under (`outcome`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ConnectorImportOutcome {
    Imported,
    SkippedAlreadyImported,
    Failed { reason: String },
}

/// One requested key's outcome, in request order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorImportItemResult {
    pub item_key: String,
    pub outcome: ConnectorImportOutcome,
}

/// `POST /api/v1/connectors/{connector}/import` response: the per-item
/// results plus the counts a client would otherwise have to tally itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorImportResult {
    pub results: Vec<ConnectorImportItemResult>,
    pub imported: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// One import run in `GET /api/v1/connectors/{connector}/history`.
///
/// `connector_items` (T33b) has no run id column, so a "run" is every ledger
/// row sharing the same `imported_at` truncated to the second — rows written
/// by one `import` call land in the same transaction and therefore the same
/// wall-clock second. `imported` is that group's row count.
///
/// `skipped` is always `0`: a skip (an item already in the ledger) writes no
/// row at all — `record_import`'s `ON CONFLICT DO NOTHING` is exactly what
/// makes a repeat import a no-op — so there is nothing in `connector_items`
/// to count a historical skip from. The field stays in the shape so a client
/// does not need a schema change if a future ticket adds a run id and can
/// then populate it for real; today it is always `0`, never a true count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorHistoryRun {
    pub imported_at: DateTime<Utc>,
    pub imported: usize,
    pub skipped: usize,
}

/// `GET /api/v1/connectors/{connector}/history`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorHistory {
    pub connector: String,
    pub runs: Vec<ConnectorHistoryRun>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_outcome_serializes_with_the_nested_status_convention() {
        let json = serde_json::to_string(&ConnectorImportOutcome::SkippedAlreadyImported)
            .expect("serialize");
        assert_eq!(json, "{\"outcome\":\"skipped_already_imported\"}");

        let failed = ConnectorImportOutcome::Failed {
            reason: "boom".to_owned(),
        };
        let json = serde_json::to_string(&failed).expect("serialize");
        assert_eq!(json, "{\"outcome\":\"failed\",\"reason\":\"boom\"}");
    }

    #[test]
    fn import_result_round_trips_through_json() {
        let result = ConnectorImportResult {
            results: vec![ConnectorImportItemResult {
                item_key: "a".to_owned(),
                outcome: ConnectorImportOutcome::Imported,
            }],
            imported: 1,
            skipped: 0,
            failed: 0,
        };
        let json = serde_json::to_string(&result).expect("serialize");
        let back: ConnectorImportResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(result, back);
    }

    #[test]
    fn history_round_trips_through_json() {
        let history = ConnectorHistory {
            connector: "stub".to_owned(),
            runs: vec![ConnectorHistoryRun {
                imported_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
                imported: 2,
                skipped: 0,
            }],
        };
        let json = serde_json::to_string(&history).expect("serialize");
        let back: ConnectorHistory = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(history, back);
    }
}
