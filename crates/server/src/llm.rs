//! Shared LiteLLM chat client (tickets T7, T9): the only place the server
//! reaches the network for a completion. Feature modules keep their own model
//! alias and prompt; transport lives here so summaries and tags cannot drift
//! into two HTTP clients.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

/// Boxed future so the model traits stay object-safe (`Arc<dyn …>`).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Failure talking to the LLM backend; mapped to a 500 by `AppError`.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("litellm request failed: {0}")]
    Request(String),
    #[error("litellm response was not usable: {0}")]
    InvalidResponse(String),
}

/// `reqwest` client for LiteLLM's OpenAI-compatible `/chat/completions`.
/// Cloneable because `reqwest::Client` is: one connection pool per process.
#[derive(Clone)]
pub struct LiteLlmClient {
    client: reqwest::Client,
    base_url: String,
    master_key: String,
}

impl LiteLlmClient {
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
        }
    }

    /// One user message in, one assistant message out.
    pub async fn complete(&self, model: &str, prompt: &str) -> Result<String, LlmError> {
        let body = ChatRequest {
            model,
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
            .map_err(|error| LlmError::Request(error.to_string()))?;

        if !response.status().is_success() {
            return Err(LlmError::Request(format!(
                "litellm answered {}",
                response.status()
            )));
        }

        let parsed: ChatResponse = response
            .json()
            .await
            .map_err(|error| LlmError::InvalidResponse(error.to_string()))?;

        parsed
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .filter(|content| !content.trim().is_empty())
            .ok_or_else(|| LlmError::InvalidResponse("no message content in response".into()))
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
