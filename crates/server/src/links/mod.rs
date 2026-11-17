//! AI link judging on tagged evidence (ticket T10).
//!
//! Two tags are a *candidate* when the user's own data puts them on the same
//! piece of evidence: notes of both tags in one thread, or places of both tags
//! in one list. Candidates are capped per tag ([`MAX_CANDIDATES_PER_TAG`]) so a
//! single rich tag cannot pair with everything the user owns and blow up both
//! the query and the model bill.
//!
//! Candidates are judged in batches ([`JUDGE_BATCH_SIZE`]) by a [`Judge`]. The
//! production judge is LiteLLM on the cheap alias; tests use a fake, so no test
//! in this crate touches the network.
//!
//! A judged pair is persisted in **canonical order** ([`canonical_pair`], which
//! matches the `links_canonical_pair_order` check) and written with an upsert on
//! `(a_tag_id, b_tag_id)`. Re-judging therefore refreshes `score`, `rationale`
//! and `judged_at` on the one existing row instead of adding a second.

mod candidates;
mod litellm;
mod prompt;
mod store;

pub mod handlers;

pub use candidates::{MAX_CANDIDATES, MAX_CANDIDATES_PER_TAG, cap_candidates, find_candidates};
pub use litellm::{JUDGE_MODEL_ALIAS, LiteLlmJudge};
pub use prompt::{build_judge_prompt, parse_judgements};
pub use store::{JudgedLink, UpsertOutcome, list_links, upsert_judgements};

use strands_core::TagId;

use crate::llm::{BoxFuture, LlmError};

/// Most pairs one model call may see. Small enough that the reply stays a
/// short JSON array the parser can trust, large enough that a normal user's
/// whole constellation costs one or two calls.
pub const JUDGE_BATCH_SIZE: usize = 20;

/// One pairing offered to the judge: both names plus a sample of each tag's
/// own text, so the model judges topics rather than two bare labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidatePair {
    pub a_tag_id: TagId,
    pub a_tag_name: String,
    pub b_tag_id: TagId,
    pub b_tag_name: String,
    pub a_context: String,
    pub b_context: String,
}

impl CandidatePair {
    /// The pair's identity, independent of which end is `a`: the storage key.
    pub fn key(&self) -> (TagId, TagId) {
        canonical_pair(self.a_tag_id, self.b_tag_id)
    }
}

/// The judge's answer for one candidate pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairJudgement {
    pub score: u8,
    pub rationale: String,
}

/// Scores candidate pairs. `dyn` so production can use LiteLLM and tests a
/// fake that never hits the network.
pub trait Judge: Send + Sync {
    /// The LiteLLM alias this judge calls (spec: `strands.cheap`).
    fn model_alias(&self) -> &str;

    /// One call's worth of pairs in, one judgement per pair out, same order.
    fn judge<'a>(
        &'a self,
        pairs: &'a [CandidatePair],
    ) -> BoxFuture<'a, Result<Vec<PairJudgement>, LlmError>>;
}

/// Judge every pair, one model call per [`JUDGE_BATCH_SIZE`] chunk.
///
/// Fails the whole run if any batch fails or answers with the wrong count:
/// callers persist only after every batch succeeded, so a late failure leaves
/// no half-written constellation.
pub async fn judge_batches(
    judge: &dyn Judge,
    pairs: &[CandidatePair],
) -> Result<Vec<PairJudgement>, LlmError> {
    let mut judged = Vec::with_capacity(pairs.len());
    for batch in pairs.chunks(JUDGE_BATCH_SIZE) {
        let mut answers = judge.judge(batch).await?;
        if answers.len() != batch.len() {
            return Err(LlmError::InvalidResponse(format!(
                "judge answered {} pairs for {} candidates",
                answers.len(),
                batch.len()
            )));
        }
        judged.append(&mut answers);
    }
    Ok(judged)
}

/// The pair in the order the schema stores it: smaller tag id first.
pub fn canonical_pair(a: TagId, b: TagId) -> (TagId, TagId) {
    if a <= b { (a, b) } else { (b, a) }
}

/// Placeholder for an `AppState` built without a judge: tests that never judge
/// use it, and a misconfigured deploy fails loudly on first judge instead of
/// silently returning an empty constellation.
pub struct UnconfiguredJudge;

impl Judge for UnconfiguredJudge {
    fn model_alias(&self) -> &str {
        litellm::JUDGE_MODEL_ALIAS
    }

    fn judge<'a>(
        &'a self,
        _pairs: &'a [CandidatePair],
    ) -> BoxFuture<'a, Result<Vec<PairJudgement>, LlmError>> {
        Box::pin(async { Err(LlmError::Request("no judge configured".into())) })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use uuid::Uuid;

    use super::*;

    /// Records each batch's size and scores every pair 80.
    struct RecordingJudge {
        batch_sizes: Mutex<Vec<usize>>,
    }

    impl RecordingJudge {
        fn new() -> Self {
            Self {
                batch_sizes: Mutex::new(Vec::new()),
            }
        }
    }

    impl Judge for RecordingJudge {
        fn model_alias(&self) -> &str {
            "test/cheap"
        }

        fn judge<'a>(
            &'a self,
            pairs: &'a [CandidatePair],
        ) -> BoxFuture<'a, Result<Vec<PairJudgement>, LlmError>> {
            Box::pin(async move {
                self.batch_sizes
                    .lock()
                    .expect("batch sizes lock")
                    .push(pairs.len());
                Ok(vec![
                    PairJudgement {
                        score: 80,
                        rationale: "same trip".to_owned(),
                    };
                    pairs.len()
                ])
            })
        }
    }

    /// Answers one judgement too few, whatever the batch size.
    struct ShortJudge;

    impl Judge for ShortJudge {
        fn model_alias(&self) -> &str {
            "test/short"
        }

        fn judge<'a>(
            &'a self,
            _pairs: &'a [CandidatePair],
        ) -> BoxFuture<'a, Result<Vec<PairJudgement>, LlmError>> {
            Box::pin(async {
                Ok(vec![PairJudgement {
                    score: 10,
                    rationale: "one answer for two pairs".to_owned(),
                }])
            })
        }
    }

    fn pairs(count: usize) -> Vec<CandidatePair> {
        (0..count)
            .map(|index| CandidatePair {
                a_tag_id: Uuid::from_u128(index as u128 + 1),
                a_tag_name: format!("a{index}"),
                b_tag_id: Uuid::from_u128(index as u128 + 1_000),
                b_tag_name: format!("b{index}"),
                a_context: String::new(),
                b_context: String::new(),
            })
            .collect()
    }

    #[tokio::test]
    async fn a_batch_is_capped_and_the_remainder_goes_alone() {
        let judge = RecordingJudge::new();
        let judged = judge_batches(&judge, &pairs(JUDGE_BATCH_SIZE + 1))
            .await
            .expect("judge");
        assert_eq!(judged.len(), JUDGE_BATCH_SIZE + 1);
        assert_eq!(
            *judge.batch_sizes.lock().expect("batch sizes lock"),
            vec![JUDGE_BATCH_SIZE, 1]
        );
    }

    #[tokio::test]
    async fn a_short_answer_fails_the_run() {
        let judged = judge_batches(&ShortJudge, &pairs(2)).await;
        assert!(judged.is_err());
    }

    #[test]
    fn canonical_order_puts_the_smaller_id_first() {
        let low = Uuid::from_u128(1);
        let high = Uuid::from_u128(2);
        assert_eq!(canonical_pair(low, high), (low, high));
        assert_eq!(canonical_pair(high, low), (low, high));
        assert_eq!(canonical_pair(low, low), (low, low));
    }

    #[test]
    fn a_pairs_key_ignores_which_end_is_a() {
        let low = Uuid::from_u128(1);
        let high = Uuid::from_u128(2);
        let mut forward = pairs(1).remove(0);
        forward.a_tag_id = low;
        forward.b_tag_id = high;
        let mut backward = forward.clone();
        backward.a_tag_id = high;
        backward.b_tag_id = low;
        assert_eq!(forward.key(), backward.key());
    }
}
