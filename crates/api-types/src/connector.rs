//! Connector DTOs: `GET /api/v1/connectors` (list) and
//! `GET /api/v1/connectors/{id}/status` (single row), spec §7.7 / T13.
//!
//! `ConnectorStatus` mirrors `strands_core::ConnectorStatus`'s wire shape
//! rather than reusing it: `strands-core` carries a `sqlx` dependency that
//! `strands-web`'s wasm build must never see, so the wire enum is kept as a
//! plain, independent `serde` type and the server maps between the two.

use serde::{Deserialize, Serialize};

/// A connector's connection state, as shown on its settings row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorStatus {
    Disconnected,
    NeedsSignIn,
    Connected,
    Error,
}

/// One row of `GET /api/v1/connectors`: a connector's static manifest plus
/// its freshly queried status, or the error querying it hit instead. Exactly
/// one of `status`/`error` is set — a connector whose own status check fails
/// still gets a full row, with its own error, rather than being dropped from
/// the list or taking the whole response down (T13 acceptance).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorSummary {
    pub id: String,
    pub label: String,
    pub scopes: Vec<String>,
    pub status: Option<ConnectorStatus>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connector_status_serializes_to_the_schema_literal() {
        let json = serde_json::to_string(&ConnectorStatus::NeedsSignIn).expect("serialize");
        assert_eq!(json, "\"needs_sign_in\"");
    }

    #[test]
    fn connector_summary_round_trips_through_json() {
        let summary = ConnectorSummary {
            id: "stub".to_owned(),
            label: "Stub".to_owned(),
            scopes: vec!["demo:read".to_owned()],
            status: Some(ConnectorStatus::Connected),
            error: None,
        };
        let json = serde_json::to_string(&summary).expect("serialize");
        let back: ConnectorSummary = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(summary, back);
    }

    #[test]
    fn a_failing_connector_s_row_carries_an_error_not_a_status() {
        let summary = ConnectorSummary {
            id: "failing".to_owned(),
            label: "Failing Demo".to_owned(),
            scopes: vec!["demo:read".to_owned()],
            status: None,
            error: Some("upstream service is unreachable".to_owned()),
        };
        let json = serde_json::to_string(&summary).expect("serialize");
        let back: ConnectorSummary = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(summary, back);
    }
}
