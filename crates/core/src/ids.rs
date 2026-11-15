//! Typed aliases for the UUID primary/foreign keys defined by
//! `migrations/0001_init.sql` (spec §6 data model). IDs are application-supplied
//! (see `Note::new`/`Tag::new`), so these are plain aliases rather than newtypes
//! with their own constructors — the wrapping buys no extra invariant here.

use uuid::Uuid;

pub type UserId = Uuid;
pub type SessionId = Uuid;
pub type CategoryId = Uuid;
pub type TagId = Uuid;
pub type ThreadId = Uuid;
pub type NoteId = Uuid;
pub type SummaryId = Uuid;
pub type LinkId = Uuid;
pub type ConnectorId = Uuid;
pub type ListId = Uuid;
pub type PlaceId = Uuid;
pub type ImportRunId = Uuid;
pub type JobId = Uuid;
