//! `Summarizer` over LiteLLM's OpenAI-compatible `/chat/completions` endpoint
//! (ticket T7). This is the only place that reaches the network for a
//! summary: handlers depend on the `Summarizer` trait, tests wire in a fake.

use serde::{Deserialize, Serialize};

use super::{BoxFuture, SummarizeError, Summarizer};

/// LiteLLM alias every version generated this way is recorded with.
pub const SUMMARY_MODEL_ALIAS: &str = "strands.summary";

/// Talks to a LiteLLM deployment configured by `LITELLM_BASE_URL` and
/// `LITELLM_MASTER_KEY` (spec: secrets come from the environment, never code).
pub struct LiteLlmSummarizer {
    client: reqwest::Client,
    base_url: String,
    master_key: String,
    model_alias: String,
}

impl LiteLlmSummarizer {
    /// Build from `LITELLM_BASE_URL` and `LITELLM_MASTER_KEY`. `base_url` has
    /// no trailing slash requirement — it is stripped before use.
    pub fn from_env() -> Result<Self, std::env::VarError> {
        let base_url = std::env::var("LITELLM_BASE_URL")?;
        let master_key = std::env::var("LITELLM_MASTER_KEY")?;
        Ok(Self::new(base_url, master_key))
    }

    pub fn new(base_url: impl Into<String>, master_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            master_key: master_key.into(),
            model_alias: SUMMARY_MODEL_ALIAS.to_owned(),
        }
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage<'a>],
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: Option<String>,
}

impl Summarizer for LiteLlmSummarizer {
    fn model_alias(&self) -> &str {
        &self.model_alias
    }

    fn summarize<'a>(&'a self, prompt: &'a str) -> BoxFuture<'a, Result<String, SummarizeError>> {
        Box::pin(async move {
            let body = ChatRequest {
                model: &self.model_alias,
                messages: &[ChatMessage {
                    role: "user",
                    content: prompt,
                }],
            };

            let response = self
                .client
                .post(format!("{}/chat/completions", self.base_url))
                .bearer_auth(&self.master_key)
                .json(&body)
                .send()
                .await
                .map_err(|error| SummarizeError::Request(error.to_string()))?;

            if !response.status().is_success() {
                return Err(SummarizeError::Request(format!(
                    "litellm answered {}",
                    response.status()
                )));
            }

            let parsed: ChatResponse = response
                .json()
                .await
                .map_err(|error| SummarizeError::InvalidResponse(error.to_string()))?;

            let content = parsed
                .choices
                .into_iter()
                .next()
                .and_then(|choice| choice.message.content)
                .filter(|content| !content.trim().is_empty())
                .ok_or_else(|| {
                    SummarizeError::InvalidResponse("no message content in response".into())
                })?;

            Ok(content)
        })
    }
}
