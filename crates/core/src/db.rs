//! Postgres enum types defined in `migrations/0001_init.sql` (spec §6), each
//! mirrored by a Rust enum with a runtime-checked `sqlx::Type` derive (no
//! compile-time `query!`/`query_as!` macros, so building needs no
//! `DATABASE_URL`). `note_source` lives in `source.rs` as `Source`.

use serde::{Deserialize, Serialize};
use sqlx::Type;

/// Mirrors the Postgres `note_kind` enum (spec §6 `notes.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "note_kind", rename_all = "snake_case")]
pub enum NoteKind {
    Text,
    Photo,
    Visit,
}

/// Mirrors the Postgres `summary_state` enum (spec §6 `summaries.state`, D8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "summary_state", rename_all = "snake_case")]
pub enum SummaryState {
    Proposed,
    Approved,
    Discarded,
    Edited,
    Failed,
}

/// Mirrors the Postgres `summary_preset` enum (spec §6 `summaries.preset`, §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "summary_preset", rename_all = "snake_case")]
pub enum SummaryPreset {
    Shorter,
    Takeaways,
    Detail,
    Dates,
}

/// Mirrors the Postgres `connector_status` enum (spec §6 `connectors.status`, §7.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "connector_status", rename_all = "snake_case")]
pub enum ConnectorStatus {
    Disconnected,
    NeedsSignIn,
    Connected,
    Error,
}

/// Mirrors the Postgres `import_run_state` enum (spec §6 `import_runs.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "import_run_state", rename_all = "snake_case")]
pub enum ImportRunState {
    Running,
    Completed,
    Failed,
}

/// Mirrors the Postgres `job_kind` enum (spec §6 `jobs.kind`, D13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "job_kind", rename_all = "snake_case")]
pub enum JobKind {
    Import,
    Summarize,
    JudgeLinks,
    EnrichPlaces,
    ExportKml,
}

/// Mirrors the Postgres `job_state` enum (spec §6 `jobs.state`, D13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "job_state", rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Completed,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_kind_serializes_to_the_schema_literal() {
        let json = serde_json::to_string(&JobKind::JudgeLinks).expect("serialize");
        assert_eq!(json, "\"judge_links\"");
    }

    #[test]
    fn connector_status_serializes_to_the_schema_literal() {
        let json = serde_json::to_string(&ConnectorStatus::NeedsSignIn).expect("serialize");
        assert_eq!(json, "\"needs_sign_in\"");
    }
}
