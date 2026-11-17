//! Data-control DTOs (ticket T17): `GET /api/v1/account/export` returns the
//! caller's whole dataset, and `DELETE /api/v1/account/data` erases it.
//!
//! The export is a file format, not a view model: it has its own types so the
//! on-disk shape can stay put while the screens' DTOs keep moving. The one
//! exception is [`SummaryVersion`], which is already exactly a stored version
//! row with no view-only fields, so re-using it is honest rather than lazy.
//!
//! `format_version` is the migration lever: a consumer that reads
//! [`EXPORT_FORMAT_VERSION`]` = 1` knows the field names below. Bumping it is
//! how the envelope changes shape without silently breaking yesterday's import.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::me::Profile;
use crate::summary::SummaryVersion;

/// The only export shape this build writes. See the module note.
pub const EXPORT_FORMAT_VERSION: u32 = 1;

/// Everything the caller owns, in one document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountExport {
    pub format_version: u32,
    pub exported_at: DateTime<Utc>,
    /// The identity the rows belong to, so the file explains itself without a
    /// second request.
    pub profile: Profile,
    pub tags: Vec<ExportTag>,
    pub threads: Vec<ExportThread>,
    pub notes: Vec<ExportNote>,
    /// The summaries currently in force, one per tag that has one.
    pub summaries: Vec<ExportSummary>,
    /// Every version ever generated, including the discarded ones.
    pub summary_versions: Vec<SummaryVersion>,
    pub links: Vec<ExportLink>,
    pub lists: Vec<ExportList>,
    pub places: Vec<ExportPlace>,
}

/// One `tags` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportTag {
    pub id: Uuid,
    pub name: String,
    pub category_id: Uuid,
    pub created_at: DateTime<Utc>,
}

/// One `threads` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportThread {
    pub id: Uuid,
    pub tag_id: Uuid,
    pub title: String,
    pub source_count: i32,
    pub updated_at: DateTime<Utc>,
}

/// One `notes` row. The body is the actual content of a capture, which is why
/// the export carries notes and not only the threads that group them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportNote {
    pub id: Uuid,
    pub tag_id: Uuid,
    pub thread_id: Option<Uuid>,
    /// `text` | `photo` | `visit`.
    pub kind: String,
    pub body: String,
    pub occurred_at: DateTime<Utc>,
    /// `manual` | `google_photos` | `whatsapp` | `google_maps`.
    pub source: String,
    pub created_at: DateTime<Utc>,
}

/// A tag's summary: the version a person approved for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportSummary {
    pub tag_id: Uuid,
    pub version_id: Uuid,
}

/// A judged tag-to-tag similarity. It is in the export because the account
/// paid a model for it, and a restore should not have to judge again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportLink {
    pub id: Uuid,
    pub a_tag_id: Uuid,
    pub b_tag_id: Uuid,
    pub score: i32,
    pub rationale: String,
    pub judged_at: DateTime<Utc>,
}

/// An imported list: the container a batch of places arrived in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportList {
    pub id: Uuid,
    pub name: String,
    pub source: String,
    pub import_run_id: Option<Uuid>,
}

/// One row of a list, enriched with what the place lookup knows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportPlace {
    pub id: Uuid,
    pub list_id: Option<Uuid>,
    pub tag_id: Option<Uuid>,
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
    pub enriched_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn export() -> AccountExport {
        let tag_id = Uuid::nil();
        let version_id = Uuid::from_u128(7);
        AccountExport {
            format_version: EXPORT_FORMAT_VERSION,
            exported_at: Utc::now(),
            profile: Profile {
                id: Uuid::nil(),
                email: "alice@example.com".to_owned(),
                name: "Alice".to_owned(),
                avatar_url: None,
                provider: "google".to_owned(),
            },
            tags: vec![ExportTag {
                id: tag_id,
                name: "Tokyo".to_owned(),
                category_id: Uuid::from_u128(1),
                created_at: Utc::now(),
            }],
            threads: vec![ExportThread {
                id: Uuid::from_u128(2),
                tag_id,
                title: "The 6 a.m. train".to_owned(),
                source_count: 1,
                updated_at: Utc::now(),
            }],
            notes: vec![ExportNote {
                id: Uuid::from_u128(3),
                tag_id,
                thread_id: None,
                kind: "text".to_owned(),
                body: "It leaves on time.".to_owned(),
                occurred_at: Utc::now(),
                source: "manual".to_owned(),
                created_at: Utc::now(),
            }],
            summaries: vec![ExportSummary { tag_id, version_id }],
            summary_versions: vec![SummaryVersion {
                id: version_id,
                tag_id,
                version: 1,
                body: "Trains, mostly.".to_owned(),
                guide: crate::summary::SummaryGuide::Shorter,
                model_alias: "strands.summary".to_owned(),
                state: crate::summary::SummaryState::Approved,
                created_at: Utc::now(),
            }],
            links: vec![ExportLink {
                id: Uuid::from_u128(4),
                a_tag_id: tag_id,
                b_tag_id: Uuid::from_u128(5),
                score: 88,
                rationale: "same trip".to_owned(),
                judged_at: Utc::now(),
            }],
            lists: vec![ExportList {
                id: Uuid::from_u128(6),
                name: "Saved 2024".to_owned(),
                source: "import".to_owned(),
                import_run_id: None,
            }],
            places: vec![ExportPlace {
                id: Uuid::from_u128(8),
                list_id: None,
                tag_id: Some(tag_id),
                name: "Nishiki Market".to_owned(),
                address: Some("Kyoto".to_owned()),
                lat: Some(35.005),
                lng: Some(135.764),
                place_id: None,
                primary_type: Some("market".to_owned()),
                types: vec!["market".to_owned()],
                metro: None,
                locality: Some("Kyoto".to_owned()),
                country: Some("Japan".to_owned()),
                notes: None,
                source: "import".to_owned(),
                enriched_at: None,
            }],
        }
    }

    #[test]
    fn the_envelope_round_trips_through_json() {
        let export = export();
        let json = serde_json::to_string(&export).expect("serialize");
        let back: AccountExport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(export, back);
    }

    #[test]
    fn the_envelope_names_its_format_version_and_time() {
        let json = serde_json::to_value(export()).expect("serialize");
        assert_eq!(json["format_version"], serde_json::json!(1));
        assert!(json["exported_at"].is_string());
    }

    #[test]
    fn every_collection_is_present_even_when_it_is_empty() {
        // A consumer must not have to distinguish "absent" from "no rows".
        let mut export = export();
        export.tags.clear();
        export.threads.clear();
        export.notes.clear();
        export.summaries.clear();
        export.summary_versions.clear();
        let json = serde_json::to_value(&export).expect("serialize");
        for key in ["tags", "threads", "notes", "summaries", "summary_versions"] {
            assert_eq!(json[key], serde_json::json!([]), "{key}");
        }
    }
}
