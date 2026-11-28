//! Wire DTOs for the T4 API surface — pure `serde` types with no `axum` or
//! `dioxus` dependency, so `strands-server` and `strands-web` can share one
//! definition of each response shape instead of hand-syncing two.
//!
//! Each DTO is named for the exact endpoint it answers (list vs detail), even
//! where the fields happen to coincide today: the two contracts are free to
//! diverge later without a rename ripping through both crates.

mod account;
mod category;
mod connector;
mod connector_items;
mod gmaps;
mod ingest;
mod link;
mod me;
mod photos;
mod places;
mod search;
mod summary;
mod tag;
mod thread;
mod whatsapp;

pub use account::{
    AccountExport, EXPORT_FORMAT_VERSION, ExportLink, ExportList, ExportNote, ExportPlace,
    ExportSummary, ExportTag, ExportThread,
};
pub use category::CategorySummary;
pub use connector::{ConnectorStatus, ConnectorSummary};
pub use connector_items::{
    ConnectorHistory, ConnectorHistoryRun, ConnectorImportItemResult, ConnectorImportOutcome,
    ConnectorImportRequest, ConnectorImportResult, ConnectorPreview, ConnectorPreviewItem,
};
pub use gmaps::{GmapsFormat, GmapsIngestResult, PlaceEnrichment, PlaceListSummary, PlaceSummary};
pub use ingest::{IngestItem, IngestRequest, IngestResult};
pub use link::{JudgeLinksResponse, LinkSummary};
pub use me::Profile;
pub use photos::{PhotoPickResult, PhotoPickState, PhotoSession, PickedPhoto};
pub use places::{
    AcceptedPlace, BackfillItem, BackfillPlace, BackfillRequest, BackfillResult, BackfillSource,
    MAX_BACKFILL_ITEMS, MAX_NEARBY_CANDIDATES, NearbyCandidate, NearbyRequest, NearbyResult,
};
pub use search::SearchResult;
pub use summary::{
    EditSummaryRequest, GenerateSummaryRequest, MAX_SUMMARY_BODY_CHARS, SummaryGuide,
    SummaryHistory, SummaryState, SummaryVersion,
};
pub use tag::{NewTag, TagDetail, TagSummary};
pub use thread::{NewThread, ThreadDetail, ThreadSummary};
pub use whatsapp::{
    WHATSAPP_DISCLOSURE, WhatsAppAllowlist, WhatsAppChat, WhatsAppImportReport, WhatsAppLinkState,
    WhatsAppStatus,
};
