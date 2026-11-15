//! A single piece of captured or imported content (spec §6 `notes` table).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Source, ValidationError};

/// Longest body a person can type in one capture (T12: 5000 characters).
///
/// Mirrors the `notes_manual_body_length` constraint added in
/// `migrations/0005_notes_body_limit.sql`: the handler answers 400, the
/// constraint makes that the only thing the table can hold.
pub const MAX_BODY_CHARS: usize = 5000;

/// Characters of the opening line kept as a thread title — the grid's preview.
pub const TITLE_CHARS: usize = 96;

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

    /// A note the user typed, bounded by [`MAX_BODY_CHARS`].
    ///
    /// Connector imports go through [`Note::new`] instead: their bodies are
    /// normalized output, not something a person typed into a textarea, so the
    /// capture limit does not apply to them.
    pub fn capture(body: impl Into<String>) -> Result<Self, ValidationError> {
        let note = Self::new(body, Source::Manual)?;
        if note.body.chars().count() > MAX_BODY_CHARS {
            return Err(ValidationError::BodyTooLong {
                max: MAX_BODY_CHARS,
            });
        }
        Ok(note)
    }

    /// One-line title for this note: its first line with text, runs of
    /// whitespace collapsed, cut at [`TITLE_CHARS`] with an ellipsis.
    pub fn title(&self) -> String {
        let line = self
            .body
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default();
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.chars().count() <= TITLE_CHARS {
            return collapsed;
        }
        let mut kept: String = collapsed.chars().take(TITLE_CHARS).collect();
        kept.push('…');
        kept
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

    #[test]
    fn capture_accepts_exactly_the_limit() {
        let body = "a".repeat(MAX_BODY_CHARS);
        let note = Note::capture(body.clone()).expect("5000 characters is allowed");
        assert_eq!(note.body(), body);
    }

    #[test]
    fn capture_rejects_one_character_past_the_limit() {
        let body = "a".repeat(MAX_BODY_CHARS + 1);
        assert_eq!(
            Note::capture(body),
            Err(ValidationError::BodyTooLong {
                max: MAX_BODY_CHARS
            })
        );
    }

    #[test]
    fn capture_counts_characters_not_bytes() {
        // 5000 three-byte characters: over the byte budget, exactly at the
        // character budget the textarea enforces.
        let body = "の".repeat(MAX_BODY_CHARS);
        assert!(Note::capture(body).is_ok());
    }

    #[test]
    fn capture_rejects_an_empty_body() {
        assert_eq!(Note::capture(""), Err(ValidationError::EmptyBody));
    }

    #[test]
    fn title_is_the_first_line_with_text() {
        let note = Note::capture("\n\n  A night that starts   at 2 a.m. \nthe rest").unwrap();
        assert_eq!(note.title(), "A night that starts at 2 a.m.");
    }

    #[test]
    fn title_truncates_a_long_body() {
        let note = Note::capture("word ".repeat(1000)).unwrap();
        let title = note.title();
        assert_eq!(title.chars().count(), TITLE_CHARS + 1);
        assert!(title.ends_with('…'));
    }
}
