//! Library surface of the strands server binary. Split out so integration
//! tests (e.g. `tests/schema_shape.rs`, `tests/auth_isolation.rs`) can build the
//! real router and reach the migration machinery without binding a socket or
//! going through `main()`.

pub mod account;
pub mod api;
pub mod app;
pub mod auth;
pub mod config;
pub mod connectors;
pub mod db;
pub mod error;
pub mod gmaps;
pub mod ingest;
pub mod links;
pub mod llm;
pub mod search;
pub mod summary;
pub mod tagging;
