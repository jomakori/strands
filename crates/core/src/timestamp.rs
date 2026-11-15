//! The Rust representation of every `TIMESTAMPTZ` column in
//! `migrations/0001_init.sql` (spec §6 data model).

/// A UTC instant, matching Postgres `TIMESTAMPTZ` on the wire.
pub type Timestamp = chrono::DateTime<chrono::Utc>;
