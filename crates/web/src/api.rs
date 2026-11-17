//! Typed HTTP client over the T4 API surface (architecture §6: the wire
//! types arrive with the client, then get mapped into `data`'s view types
//! at the call site — the DTOs here stay the wire shape, nothing more).
//!
//! One function per endpoint, each a bare GET decoded straight into the
//! shared `strands-api-types` DTO. Same-origin `fetch` sends the session
//! cookie automatically, so there is no credentials wiring to do here.

// The API-backed screens land one at a time, so some client functions have no
// caller in the bundle yet; that is expected rather than dead weight.
#![allow(dead_code)]

use gloo_net::http::{Request, RequestBuilder};
use serde::Serialize;
use serde::de::DeserializeOwned;
use strands_api_types::{
    CategorySummary, ConnectorSummary, EditSummaryRequest, GenerateSummaryRequest, NewTag,
    NewThread, Profile, SearchResult, SummaryHistory, SummaryVersion, TagDetail, TagSummary,
    ThreadDetail, ThreadSummary,
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

/// `POST /api/v1/threads/{id}/archive` — the thread leaves its tag's list. Success is a
/// `204` with no body, so there is nothing to decode and only the status to check.
pub async fn archive_thread(id: Uuid) -> Result<(), ApiError> {
    let response = Request::post(&format!("/api/v1/threads/{id}/archive"))
        .send()
        .await?;
    if response.ok() {
        return Ok(());
    }
    Err(ApiError::Status(response.status()))
}

/// `GET /api/v1/connectors` — every registered connector's settings row.
pub async fn list_connectors() -> Result<Vec<ConnectorSummary>, ApiError> {
    get_json("/api/v1/connectors").await
}

/// `GET /api/v1/search?q=` — the caller's threads matching `query` by title or note
/// body, best match first. A blank `query` asks for the recent threads instead, which
/// is the same endpoint the sheet uses before anything has been typed.
pub async fn search(query: &str) -> Result<Vec<SearchResult>, ApiError> {
    send_json(Request::get("/api/v1/search").query([("q", query)])).await
}

/// `POST /api/v1/tags/{id}/summary` — generate a summary; `force: true` in the
/// request is the "regenerate" action, which always mints a new version.
pub async fn generate_summary(
    tag_id: Uuid,
    request: &GenerateSummaryRequest,
) -> Result<SummaryVersion, ApiError> {
    post_json(&format!("/api/v1/tags/{tag_id}/summary"), request).await
}

/// `GET /api/v1/tags/{id}/summary/history` — every version, newest first, plus
/// the version in force.
pub async fn summary_history(tag_id: Uuid) -> Result<SummaryHistory, ApiError> {
    get_json(&format!("/api/v1/tags/{tag_id}/summary/history")).await
}

/// `POST /api/v1/tags/{id}/summary/{version}/approve` — commit a proposal. The
/// answer is the tag's fresh review state, so one call updates the whole card.
pub async fn approve_summary(tag_id: Uuid, summary_id: Uuid) -> Result<SummaryHistory, ApiError> {
    send_json(Request::post(&review_path(tag_id, summary_id, "approve"))).await
}

/// `POST /api/v1/tags/{id}/summary/{version}/discard` — drop a proposal without
/// applying it.
pub async fn discard_summary(tag_id: Uuid, summary_id: Uuid) -> Result<SummaryHistory, ApiError> {
    send_json(Request::post(&review_path(tag_id, summary_id, "discard"))).await
}

/// `POST /api/v1/tags/{id}/summary/{version}/edit` — record a rewrite as a new
/// `edited` version; the original proposal is left untouched.
pub async fn edit_summary(
    tag_id: Uuid,
    summary_id: Uuid,
    body: &str,
) -> Result<SummaryHistory, ApiError> {
    let request = EditSummaryRequest {
        body: body.to_owned(),
    };
    post_json(&review_path(tag_id, summary_id, "edit"), &request).await
}

/// The three review actions share a URL shape; only the last segment differs.
fn review_path(tag_id: Uuid, summary_id: Uuid, action: &str) -> String {
    format!("/api/v1/tags/{tag_id}/summary/{summary_id}/{action}")
}

/// `GET /api/v1/categories` — the caller's own categories, in seed order. Quick capture
/// needs a `category_id` before it can file a brand-new tag.
pub async fn list_categories() -> Result<Vec<CategorySummary>, ApiError> {
    get_json("/api/v1/categories").await
}

/// `POST /api/v1/tags` — create a tag under one of the caller's categories. A foreign or
/// unknown category answers [`ApiError::Status`]`(404)`.
pub async fn create_tag(name: &str, category_id: Uuid) -> Result<TagSummary, ApiError> {
    let body = NewTag {
        name: name.to_owned(),
        category_id,
    };
    post_json("/api/v1/tags", &body).await
}

/// `POST /api/v1/threads` — capture one note. `tag_id: None` files it in the caller's inbox;
/// a foreign tag id answers [`ApiError::Status`]`(404)`.
pub async fn create_thread(body: &str, tag_id: Option<Uuid>) -> Result<ThreadDetail, ApiError> {
    let body = NewThread {
        body: body.to_owned(),
        tag_id,
    };
    post_json("/api/v1/threads", &body).await
}

/// `DELETE /api/v1/account/data` — erase every note, tag, thread and summary the
/// caller owns. The account itself stays, so the caller is still signed in after.
pub async fn delete_my_data() -> Result<(), ApiError> {
    let response = Request::delete("/api/v1/account/data").send().await?;
    if !response.ok() {
        return Err(ApiError::Status(response.status()));
    }
    Ok(())
}

/// Where the export download points. An anchor with `download` lets the browser
/// own the save dialog and send the session cookie, so there is no second code
/// path that could disagree with `GET /api/v1/account/export`.
pub const EXPORT_PATH: &str = "/api/v1/account/export";

/// Where the KML download points. Same trick as [`EXPORT_PATH`]: an anchor with
/// `download` keeps the session cookie and the server's own
/// `application/vnd.google-earth.kml+xml` type, and leaves the unresolved count
/// on the response header rather than inventing a second code path.
pub const KML_PATH: &str = "/api/v1/places/kml";

/// Shared GET-and-decode: every endpoint above is a plain GET with a JSON
/// body, so the per-call code is just the path and the target type.
async fn get_json<T: DeserializeOwned>(path: &str) -> Result<T, ApiError> {
    send_json(Request::get(path)).await
}

/// Shared POST-and-decode: send the DTO as the JSON body and decode the answer.
async fn post_json<T: DeserializeOwned>(path: &str, body: &impl Serialize) -> Result<T, ApiError> {
    decode(Request::post(path).json(body)?.send().await?).await
}

/// Send one prepared request and decode its JSON body.
async fn send_json<T: DeserializeOwned>(request: RequestBuilder) -> Result<T, ApiError> {
    decode(request.send().await?).await
}

/// One response body into `T`, mapping a non-2xx answer to `Status`.
async fn decode<T: DeserializeOwned>(response: gloo_net::http::Response) -> Result<T, ApiError> {
    if !response.ok() {
        return Err(ApiError::Status(response.status()));
    }
    Ok(response.json().await?)
}
