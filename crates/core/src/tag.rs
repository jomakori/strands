//! A tag ("strand"): the unit notes are grouped and summarized under (spec §6 `tags`).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ValidationError;

/// A tag groups notes into threads and carries an AI-generated summary (D6, D8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    id: Uuid,
    name: String,
}

impl Tag {
    /// Build a tag, rejecting an empty name.
    pub fn new(name: impl Into<String>) -> Result<Self, ValidationError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(ValidationError::EmptyName);
        }
        Ok(Self {
            id: Uuid::new_v4(),
            name,
        })
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_accepts_non_empty_name() {
        let tag = Tag::new("Travel").expect("valid name");
        assert_eq!(tag.name(), "Travel");
    }

    #[test]
    fn new_rejects_empty_name() {
        assert_eq!(Tag::new(""), Err(ValidationError::EmptyName));
    }

    #[test]
    fn new_rejects_whitespace_only_name() {
        assert_eq!(Tag::new("   "), Err(ValidationError::EmptyName));
    }

    #[test]
    fn each_tag_gets_a_distinct_id() {
        let a = Tag::new("a").unwrap();
        let b = Tag::new("b").unwrap();
        assert_ne!(a.id(), b.id());
    }
}
