//! Strands domain types — no IO, no framework dependencies (see architecture §6).

mod error;
mod note;
mod source;
mod tag;

pub use error::ValidationError;
pub use note::Note;
pub use source::Source;
pub use tag::Tag;
