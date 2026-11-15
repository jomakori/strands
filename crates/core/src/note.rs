//! A single piece of captured or imported content (spec §6 `notes` table).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Source, ValidationError};

/// A note: manual capture or normalized connector output, always owned by a tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    id: Uuid,
    body: String,
    source: Source,
}

impl Note {
    /// Build a note, rejecting an empty body (spec §7.6 capture requires text).
    pub fn new(body: impl Into<String>, source: Source) -> Result<Self, ValidationError> {
        let body = body.into();
        if body.trim().is_empty() {
            return Err(ValidationError::EmptyBody);
        }
        Ok(Self {
            id: Uuid::new_v4(),
            body,
            source,
        })
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn source(&self) -> Source {
        self.source
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_accepts_non_empty_body() {
        let note = Note::new("hello", Source::Manual).expect("valid body");
        assert_eq!(note.body(), "hello");
        assert_eq!(note.source(), Source::Manual);
    }

    #[test]
    fn new_rejects_empty_body() {
        assert_eq!(
            Note::new("", Source::Manual),
            Err(ValidationError::EmptyBody)
        );
    }

    #[test]
    fn new_rejects_whitespace_only_body() {
        assert_eq!(
            Note::new("   \n\t", Source::Manual),
            Err(ValidationError::EmptyBody)
        );
    }

    #[test]
    fn each_note_gets_a_distinct_id() {
        let a = Note::new("a", Source::Manual).unwrap();
        let b = Note::new("b", Source::Manual).unwrap();
        assert_ne!(a.id(), b.id());
    }
}
