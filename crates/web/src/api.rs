//! Typed HTTP client over the T4 API surface (architecture §6: the wire
//! types arrive with the client, then get mapped into `data`'s view types
//! at the call site — the DTOs here stay the wire shape, nothing more).
//!
//! One function per endpoint, each a bare GET decoded straight into the
//! shared `strands-api-types` DTO. Same-origin `fetch` sends the session
//! cookie automatically, so there is no credentials wiring to do here.

use gloo_net::http::Request;
use serde::de::DeserializeOwned;
use strands_api_types::{
    ConnectorSummary, Profile, TagDetail, TagSummary, ThreadDetail, ThreadSummary,
};
use uuid::Uuid;

/// Failure modes a caller must handle: the request/parse itself failed, or
/// the server answered but rejected it.
#[derive(Debug)]
pub enum ApiError {
    /// `fetch` failed, or the body was not valid JSON for the target type.
    Network(gloo_net::Error),
    /// The server answered with a non-2xx status.
    Status(u16),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(error) => write!(f, "network error: {error}"),
            Self::Status(status) => write!(f, "server returned {status}"),
        }
    }
}

impl From<gloo_net::Error> for ApiError {
    fn from(error: gloo_net::Error) -> Self {
        Self::Network(error)
    }
}

/// `GET /api/v1/me` — the signed-in user, or [`ApiError::Status`]`(401)`.
pub async fn me() -> Result<Profile, ApiError> {
    get_json("/api/v1/me").await
}

/// `GET /api/v1/tags` — every tag the caller owns.
pub async fn list_tags() -> Result<Vec<TagSummary>, ApiError> {
    get_json("/api/v1/tags").await
}

/// `GET /api/v1/tags/{id}` — a foreign id answers [`ApiError::Status`]`(404)`.
pub async fn tag_detail(id: Uuid) -> Result<TagDetail, ApiError> {
    get_json(&format!("/api/v1/tags/{id}")).await
}

/// `GET /api/v1/tags/{id}/threads` — threads under one tag.
pub async fn list_threads(tag_id: Uuid) -> Result<Vec<ThreadSummary>, ApiError> {
    get_json(&format!("/api/v1/tags/{tag_id}/threads")).await
}

/// `GET /api/v1/threads/{id}`.
pub async fn thread_detail(id: Uuid) -> Result<ThreadDetail, ApiError> {
    get_json(&format!("/api/v1/threads/{id}")).await
}

/// `GET /api/v1/connectors` — every registered connector's settings row.
pub async fn list_connectors() -> Result<Vec<ConnectorSummary>, ApiError> {
    get_json("/api/v1/connectors").await
}

/// Shared GET-and-decode: every endpoint above is a plain GET with a JSON
/// body, so the per-call code is just the path and the target type.
async fn get_json<T: DeserializeOwned>(path: &str) -> Result<T, ApiError> {
    let response = Request::get(path).send().await?;
    if !response.ok() {
        return Err(ApiError::Status(response.status()));
    }
    Ok(response.json().await?)
}
