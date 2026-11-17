//! `Judge` over the shared LiteLLM client, pinned to the cheap alias.

use crate::llm::{BoxFuture, LiteLlmClient, LlmError};

use super::{CandidatePair, Judge, PairJudgement, build_judge_prompt, parse_judgements};

/// LiteLLM alias judging calls, and only judging calls (ticket T10). The same
/// alias as tagging: judging a handful of tag pairs is the same amount of work
/// as tagging a batch of notes.
pub const JUDGE_MODEL_ALIAS: &str = "strands.cheap";

pub struct LiteLlmJudge {
    client: LiteLlmClient,
    model_alias: String,
}

impl LiteLlmJudge {
    pub fn new(client: LiteLlmClient) -> Self {
        Self {
            client,
            model_alias: JUDGE_MODEL_ALIAS.to_owned(),
        }
    }

    /// Build from the environment, failing at boot rather than on first use.
    pub fn from_env() -> Result<Self, std::env::VarError> {
        Ok(Self::new(LiteLlmClient::from_env()?))
    }
}

impl Judge for LiteLlmJudge {
    fn model_alias(&self) -> &str {
        &self.model_alias
    }

    fn judge<'a>(
        &'a self,
        pairs: &'a [CandidatePair],
    ) -> BoxFuture<'a, Result<Vec<PairJudgement>, LlmError>> {
        Box::pin(async move {
            let prompt = build_judge_prompt(pairs);
            let reply = self.client.complete(&self.model_alias, &prompt).await?;
            parse_judgements(&reply, pairs.len())
        })
    }
}
