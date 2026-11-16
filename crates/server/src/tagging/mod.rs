//! AI tag assignment on ingest (ticket T9).
//!
//! Items are batched (at most [`BATCH_SIZE`] per model call) and the cheap
//! model returns one tag name per item. Names are compared case-insensitively
//! after whitespace normalization, so a second run over the same items attaches
//! the tags that already exist instead of creating look-alikes.

mod litellm;
mod prompt;

pub use litellm::LiteLlmTagger;
pub use prompt::{build_tag_prompt, parse_tag_names};

use crate::llm::{BoxFuture, LlmError};

/// Most items one model call may see (ticket T9: at most 40 per call).
pub const BATCH_SIZE: usize = 40;

/// New tags one ingest may create. Beyond this, further novel names fall back
/// to Inbox: a runaway model cannot flood the grid with tags.
pub const MAX_NEW_TAGS_PER_INGEST: usize = 10;

/// Longest tag name kept after cleaning; the grid has to render it.
pub const MAX_TAG_NAME_CHARS: usize = 60;

/// One item offered for tagging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagItem {
    pub body: String,
}

/// Assigns tag names to a batch of items, given the user's existing names.
pub trait Tagger: Send + Sync {
    /// The LiteLLM alias this tagger calls (spec: `strands.cheap`).
    fn model_alias(&self) -> &str;

    /// One call's worth of items in, one tag name per item out, same order.
    fn assign<'a>(
        &'a self,
        existing: &'a [String],
        items: &'a [TagItem],
    ) -> BoxFuture<'a, Result<Vec<String>, LlmError>>;
}

/// Assign tags for every item, one model call per [`BATCH_SIZE`] chunk.
///
/// Fails the whole run if any batch fails or answers with the wrong count:
/// callers apply the result after every batch succeeded, so a late failure
/// leaves no half-applied run.
pub async fn assign_batches(
    tagger: &dyn Tagger,
    existing: &[String],
    items: &[TagItem],
) -> Result<Vec<String>, LlmError> {
    let mut names = Vec::with_capacity(items.len());
    for batch in items.chunks(BATCH_SIZE) {
        let mut assigned = tagger.assign(existing, batch).await?;
        if assigned.len() != batch.len() {
            return Err(LlmError::InvalidResponse(format!(
                "tagger answered {} names for {} items",
                assigned.len(),
                batch.len()
            )));
        }
        names.append(&mut assigned);
    }
    Ok(names)
}

/// Comparison key for a tag name: trimmed, runs of whitespace collapsed, and
/// case-folded. Two names with the same key are the same tag.
pub fn normalize_tag_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The name to store for a model-supplied tag, or `None` when nothing usable
/// survived (empty, whitespace, or control characters only).
pub fn clean_tag_name(raw: &str) -> Option<String> {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed.chars().take(MAX_TAG_NAME_CHARS).collect())
}

/// Placeholder for an `AppState` built without a tagger: tests that never hit
/// ingest use it, and a misconfigured deploy fails loudly on first ingest.
pub struct UnconfiguredTagger;

impl Tagger for UnconfiguredTagger {
    fn model_alias(&self) -> &str {
        litellm::TAGGING_MODEL_ALIAS
    }

    fn assign<'a>(
        &'a self,
        _existing: &'a [String],
        _items: &'a [TagItem],
    ) -> BoxFuture<'a, Result<Vec<String>, LlmError>> {
        Box::pin(async { Err(LlmError::Request("no tagger configured".into())) })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Records each batch's size and answers every item with "Travel".
    struct RecordingTagger {
        batch_sizes: Mutex<Vec<usize>>,
    }

    impl RecordingTagger {
        fn new() -> Self {
            Self {
                batch_sizes: Mutex::new(Vec::new()),
            }
        }
    }

    impl Tagger for RecordingTagger {
        fn model_alias(&self) -> &str {
            "test/cheap"
        }

        fn assign<'a>(
            &'a self,
            _existing: &'a [String],
            items: &'a [TagItem],
        ) -> BoxFuture<'a, Result<Vec<String>, LlmError>> {
            Box::pin(async move {
                self.batch_sizes
                    .lock()
                    .expect("batch sizes lock")
                    .push(items.len());
                Ok(vec!["Travel".to_owned(); items.len()])
            })
        }
    }

    fn items(count: usize) -> Vec<TagItem> {
        (0..count)
            .map(|index| TagItem {
                body: format!("item {index}"),
            })
            .collect()
    }

    #[tokio::test]
    async fn forty_items_are_one_batch() {
        let tagger = RecordingTagger::new();
        let names = assign_batches(&tagger, &[], &items(40))
            .await
            .expect("assign");
        assert_eq!(names.len(), 40);
        assert_eq!(
            *tagger.batch_sizes.lock().expect("batch sizes lock"),
            vec![40]
        );
    }

    #[tokio::test]
    async fn forty_one_items_split_into_forty_and_one() {
        let tagger = RecordingTagger::new();
        let names = assign_batches(&tagger, &[], &items(41))
            .await
            .expect("assign");
        assert_eq!(names.len(), 41);
        assert_eq!(
            *tagger.batch_sizes.lock().expect("batch sizes lock"),
            vec![40, 1]
        );
    }

    #[test]
    fn normalization_trims_and_case_folds() {
        assert_eq!(normalize_tag_name("  Travel "), "travel");
        assert_eq!(normalize_tag_name("TRAVEL"), "travel");
        assert_eq!(normalize_tag_name("  New   York\t"), "new york");
        assert_eq!(normalize_tag_name("Travél"), "travél");
    }

    #[test]
    fn cleaning_keeps_a_readable_single_line() {
        assert_eq!(
            clean_tag_name("  New\n York  ").as_deref(),
            Some("New York")
        );
        assert_eq!(clean_tag_name("   "), None);
        assert_eq!(clean_tag_name(""), None);
    }

    #[test]
    fn cleaning_caps_the_stored_name() {
        let long = "a".repeat(MAX_TAG_NAME_CHARS + 20);
        let cleaned = clean_tag_name(&long).expect("non-empty");
        assert_eq!(cleaned.chars().count(), MAX_TAG_NAME_CHARS);
    }
}
