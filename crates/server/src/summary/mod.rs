//! Summary generation (ticket T7): guided generation, regeneration, and a
//! pruned version history, behind a `Summarizer` trait so tests never touch
//! the network — LiteLLM is reachable only through this seam, mirroring how
//! `auth::identity::IdentityProvider` keeps Google out of the handlers.

pub mod handlers;
mod litellm;
mod prompt;
pub mod store;

use std::future::Future;
use std::pin::Pin;

use sha2::{Digest, Sha256};
use strands_api_types::SummaryGuide;
use strands_core::SummaryPreset;

pub use litellm::LiteLlmSummarizer;
pub use prompt::build_prompt;

/// Boxed future so [`Summarizer`] stays object-safe (`Arc<dyn Summarizer>`).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Failure talking to the LLM backend; mapped to a 500 by `AppError`.
#[derive(Debug, thiserror::Error)]
pub enum SummarizeError {
    #[error("litellm request failed: {0}")]
    Request(String),
    #[error("litellm response was not usable: {0}")]
    InvalidResponse(String),
}

/// Everything a handler needs to turn a prompt into summary text.
pub trait Summarizer: Send + Sync {
    /// The LiteLLM alias recorded on every version this summarizer produces
    /// (spec acceptance: "history lists versions with the model alias").
    fn model_alias(&self) -> &str;

    fn summarize<'a>(&'a self, prompt: &'a str) -> BoxFuture<'a, Result<String, SummarizeError>>;
}

/// Split a guide into the `(preset, guide)` columns `summaries` stores it as:
/// a fixed preset is the enum column, freeform text is the text column — the
/// two never both hold a value.
pub fn guide_columns(guide: &SummaryGuide) -> (Option<SummaryPreset>, Option<String>) {
    match guide {
        SummaryGuide::Shorter => (Some(SummaryPreset::Shorter), None),
        SummaryGuide::Takeaways => (Some(SummaryPreset::Takeaways), None),
        SummaryGuide::Detail => (Some(SummaryPreset::Detail), None),
        SummaryGuide::Dates => (Some(SummaryPreset::Dates), None),
        SummaryGuide::Freeform { text } => (None, Some(text.clone())),
    }
}

/// The inverse of [`guide_columns`]: reconstruct the guide a version was
/// generated with from its stored columns.
pub fn guide_from_columns(
    preset: Option<SummaryPreset>,
    guide_text: Option<String>,
) -> SummaryGuide {
    match preset {
        Some(SummaryPreset::Shorter) => SummaryGuide::Shorter,
        Some(SummaryPreset::Takeaways) => SummaryGuide::Takeaways,
        Some(SummaryPreset::Detail) => SummaryGuide::Detail,
        Some(SummaryPreset::Dates) => SummaryGuide::Dates,
        None => SummaryGuide::Freeform {
            text: guide_text.unwrap_or_default(),
        },
    }
}

/// Deterministic fingerprint of what a version was generated from: the guide
/// and the notes' bodies. Two generations with the same guide and the same
/// notes hash identically, which is what lets `generate` skip the LLM call
/// entirely (spec acceptance: "a second open with unchanged notes makes zero
/// LLM calls").
pub fn compute_source_hash(guide: &SummaryGuide, notes: &[String]) -> String {
    let mut hasher = Sha256::new();
    // `SummaryGuide` is a plain enum, so this cannot fail; fall back to its
    // debug form rather than panicking if it ever does.
    let guide_json = serde_json::to_string(guide).unwrap_or_else(|_| format!("{guide:?}"));
    hasher.update(guide_json.as_bytes());
    hasher.update([0u8]);
    for note in notes {
        hasher.update(note.as_bytes());
        hasher.update([0x1eu8]); // record separator between notes
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_guide_and_notes_hash_identically() {
        let notes = vec!["a".to_owned(), "b".to_owned()];
        let left = compute_source_hash(&SummaryGuide::Shorter, &notes);
        let right = compute_source_hash(&SummaryGuide::Shorter, &notes);
        assert_eq!(left, right);
    }

    #[test]
    fn a_different_guide_changes_the_hash() {
        let notes = vec!["a".to_owned()];
        let shorter = compute_source_hash(&SummaryGuide::Shorter, &notes);
        let detail = compute_source_hash(&SummaryGuide::Detail, &notes);
        assert_ne!(shorter, detail);
    }

    #[test]
    fn different_notes_change_the_hash() {
        let a = compute_source_hash(&SummaryGuide::Shorter, &["a".to_owned()]);
        let b = compute_source_hash(&SummaryGuide::Shorter, &["b".to_owned()]);
        assert_ne!(a, b);
    }

    #[test]
    fn guide_columns_round_trip() {
        for guide in [
            SummaryGuide::Shorter,
            SummaryGuide::Takeaways,
            SummaryGuide::Detail,
            SummaryGuide::Dates,
            SummaryGuide::Freeform {
                text: "focus on dates".to_owned(),
            },
        ] {
            let (preset, text) = guide_columns(&guide);
            assert_eq!(guide_from_columns(preset, text), guide);
        }
    }
}
