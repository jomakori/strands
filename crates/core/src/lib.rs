//! Strands domain types — no IO, no framework dependencies (see architecture §6).

pub mod connector;
mod db;
mod error;
mod ids;
mod note;
mod source;
mod tag;
mod timestamp;

pub use connector::{Connector, ConnectorError, ConnectorManifest, Registry};
pub use db::{
    ConnectorStatus, ImportRunState, JobKind, JobState, NoteKind, SummaryPreset, SummaryState,
};
pub use error::ValidationError;
pub use ids::{
    CategoryId, ConnectorId, ImportRunId, JobId, LinkId, ListId, NoteId, PlaceId, SessionId,
    SummaryId, TagId, ThreadId, UserId,
};
pub use note::{MAX_BODY_CHARS, Note};
pub use source::Source;
pub use tag::Tag;
pub use timestamp::Timestamp;
