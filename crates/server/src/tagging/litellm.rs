//! `Tagger` over the shared LiteLLM client, pinned to the cheap alias.

use crate::llm::{BoxFuture, LiteLlmClient, LlmError};

use super::{TagItem, Tagger, build_tag_prompt, parse_tag_names};

/// LiteLLM alias tagging calls, and only tagging calls (ticket T9).
pub const TAGGING_MODEL_ALIAS: &str = "strands.cheap";

pub struct LiteLlmTagger {
    client: LiteLlmClient,
    model_alias: String,
}

impl LiteLlmTagger {
    pub fn new(client: LiteLlmClient) -> Self {
        Self {
            client,
            model_alias: TAGGING_MODEL_ALIAS.to_owned(),
        }
    }

    /// Build from the environment, failing at boot rather than on first use.
    pub fn from_env() -> Result<Self, std::env::VarError> {
        Ok(Self::new(LiteLlmClient::from_env()?))
    }
}

impl Tagger for LiteLlmTagger {
    fn model_alias(&self) -> &str {
        &self.model_alias
    }

    fn assign<'a>(
        &'a self,
        existing: &'a [String],
        items: &'a [TagItem],
    ) -> BoxFuture<'a, Result<Vec<String>, LlmError>> {
        Box::pin(async move {
            let prompt = build_tag_prompt(existing, items);
            let reply = self.client.complete(&self.model_alias, &prompt).await?;
            parse_tag_names(&reply, items.len())
        })
    }
}
