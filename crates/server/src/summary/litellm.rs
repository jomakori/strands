//! `Summarizer` over LiteLLM's OpenAI-compatible `/chat/completions` endpoint
//! (ticket T7). Transport is shared with tagging through [`LiteLlmClient`];
//! this type only pins the summary model alias.

use crate::llm::LiteLlmClient;

use super::{BoxFuture, SummarizeError, Summarizer};

/// LiteLLM alias every version generated this way is recorded with.
pub const SUMMARY_MODEL_ALIAS: &str = "strands.summary";

/// Talks to a LiteLLM deployment configured by `LITELLM_BASE_URL` and
/// `LITELLM_MASTER_KEY` (spec: secrets come from the environment, never code).
pub struct LiteLlmSummarizer {
    client: LiteLlmClient,
    model_alias: String,
}

impl LiteLlmSummarizer {
    pub fn new(client: LiteLlmClient) -> Self {
        Self {
            client,
            model_alias: SUMMARY_MODEL_ALIAS.to_owned(),
        }
    }

    /// Build from the environment, failing at boot rather than on first use.
    pub fn from_env() -> Result<Self, std::env::VarError> {
        Ok(Self::new(LiteLlmClient::from_env()?))
    }
}

impl Summarizer for LiteLlmSummarizer {
    fn model_alias(&self) -> &str {
        &self.model_alias
    }

    fn summarize<'a>(&'a self, prompt: &'a str) -> BoxFuture<'a, Result<String, SummarizeError>> {
        Box::pin(async move { self.client.complete(&self.model_alias, prompt).await })
    }
}
