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
mod ingest;
mod me;
mod search;
mod summary;
mod tag;
mod thread;

pub use account::{
    AccountExport, EXPORT_FORMAT_VERSION, ExportLink, ExportList, ExportNote, ExportPlace,
    ExportSummary, ExportTag, ExportThread,
};
pub use category::CategorySummary;
pub use connector::{ConnectorStatus, ConnectorSummary};
pub use ingest::{IngestItem, IngestRequest, IngestResult};
pub use me::Profile;
pub use search::SearchResult;
pub use summary::{
    EditSummaryRequest, GenerateSummaryRequest, MAX_SUMMARY_BODY_CHARS, SummaryGuide,
    SummaryHistory, SummaryState, SummaryVersion,
};
pub use tag::{NewTag, TagDetail, TagSummary};
pub use thread::{NewThread, ThreadDetail, ThreadSummary};
