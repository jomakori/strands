//! The real Picker API client: `photospicker.googleapis.com/v1`.

use chrono::{DateTime, Utc};
use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Response, StatusCode};
use serde::Deserialize;

use crate::picker::{
    MAX_THUMBNAIL_BYTES, PhotosError, PickedItem, PickerApi, PickerFuture, PickerSession,
    SessionStatus, Thumbnail, thumbnail_url,
};

const API_BASE: &str = "https://photospicker.googleapis.com/v1";

/// A picker session is created with an empty body; Google fills the rest in.
const EMPTY_SESSION_BODY: &str = "{}";

/// Calls the Picker API over HTTPS.
#[derive(Debug, Clone)]
pub struct GooglePicker {
    client: Client,
    api_base: String,
}

impl GooglePicker {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: Client::builder().build()?,
            api_base: API_BASE.to_owned(),
        })
    }

    async fn session_body(&self, access_token: &str) -> Result<SessionBody, PhotosError> {
        let response = self
            .client
            .post(format!("{}/sessions", self.api_base))
            .bearer_auth(access_token)
            .header(CONTENT_TYPE, "application/json")
            .body(EMPTY_SESSION_BODY)
            .send()
            .await
            .map_err(|error| transport("creating a picker session", &error))?;
        let response = checked(response).await?;
        response
            .json()
            .await
            .map_err(|error| transport("reading a picker session", &error))
    }

    async fn session_status_body(
        &self,
        access_token: &str,
        session_id: &str,
    ) -> Result<SessionBody, PhotosError> {
        let response = self
            .client
            .get(format!("{}/sessions/{session_id}", self.api_base))
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|error| transport("polling a picker session", &error))?;
        let response = checked(response).await?;
        response
            .json()
            .await
            .map_err(|error| transport("reading a picker session", &error))
    }

    async fn page(
        &self,
        access_token: &str,
        session_id: &str,
        page_token: Option<&str>,
    ) -> Result<ListBody, PhotosError> {
        let mut request = self
            .client
            .get(format!("{}/mediaItems", self.api_base))
            .bearer_auth(access_token)
            .query(&[("sessionId", session_id)]);
        if let Some(page_token) = page_token {
            request = request.query(&[("pageToken", page_token)]);
        }
        let response = request
            .send()
            .await
            .map_err(|error| transport("listing picked items", &error))?;
        let response = checked(response).await?;
        response
            .json()
            .await
            .map_err(|error| transport("reading picked items", &error))
    }
}

impl PickerApi for GooglePicker {
    fn create_session<'a>(
        &'a self,
        access_token: &'a str,
    ) -> PickerFuture<'a, Result<PickerSession, PhotosError>> {
        Box::pin(async move {
            let body = self.session_body(access_token).await?;
            Ok(PickerSession {
                id: body.id,
                picker_uri: body.picker_uri,
                expires_at: body.expire_time,
            })
        })
    }

    fn session_status<'a>(
        &'a self,
        access_token: &'a str,
        session_id: &'a str,
    ) -> PickerFuture<'a, Result<SessionStatus, PhotosError>> {
        Box::pin(async move {
            let body = self.session_status_body(access_token, session_id).await?;
            Ok(SessionStatus {
                media_items_set: body.media_items_set,
            })
        })
    }

    fn list_items<'a>(
        &'a self,
        access_token: &'a str,
        session_id: &'a str,
    ) -> PickerFuture<'a, Result<Vec<PickedItem>, PhotosError>> {
        Box::pin(async move {
            let mut items = Vec::new();
            let mut page_token: Option<String> = None;
            loop {
                let body = self
                    .page(access_token, session_id, page_token.as_deref())
                    .await?;
                items.extend(
                    body.media_items
                        .into_iter()
                        .filter_map(ItemBody::into_picked),
                );
                match body.next_page_token {
                    Some(token) if !token.is_empty() => page_token = Some(token),
                    _ => return Ok(items),
                }
            }
        })
    }

    fn thumbnail<'a>(
        &'a self,
        base_url: &'a str,
    ) -> PickerFuture<'a, Result<Thumbnail, PhotosError>> {
        Box::pin(async move {
            let response = self
                .client
                .get(thumbnail_url(base_url))
                .send()
                .await
                .map_err(|_| PhotosError::Transport("downloading a thumbnail".to_owned()))?;
            let response = match response.status() {
                StatusCode::OK => response,
                // The 60-minute window lapsed, or the item is gone: both need a
                // new session rather than a retry against the same URL.
                StatusCode::UNAUTHORIZED
                | StatusCode::FORBIDDEN
                | StatusCode::NOT_FOUND
                | StatusCode::GONE => return Err(PhotosError::Expired),
                status => return Err(refused(status, "downloading a thumbnail")),
            };
            if response.content_length().unwrap_or_default() > MAX_THUMBNAIL_BYTES as u64 {
                return Err(PhotosError::ThumbnailTooLarge);
            }
            let content_type = image_content_type(&response);
            let bytes = response
                .bytes()
                .await
                .map_err(|_| PhotosError::Transport("downloading a thumbnail".to_owned()))?;
            if bytes.len() > MAX_THUMBNAIL_BYTES {
                return Err(PhotosError::ThumbnailTooLarge);
            }
            Ok(Thumbnail {
                content_type,
                bytes: bytes.to_vec(),
            })
        })
    }
}

/// Turn a non-2xx answer into the error the caller should act on.
async fn checked(response: Response) -> Result<Response, PhotosError> {
    match response.status() {
        status if status.is_success() => Ok(response),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(PhotosError::Refused(
            "the access token was rejected".to_owned(),
        )),
        StatusCode::NOT_FOUND => Err(PhotosError::Refused("the session is unknown".to_owned())),
        status => Err(refused(status, "calling the picker api")),
    }
}

fn refused(status: StatusCode, what: &str) -> PhotosError {
    PhotosError::Transport(format!("google photos answered {status} while {what}"))
}

fn transport(what: &str, error: &reqwest::Error) -> PhotosError {
    let reason = if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "could not connect"
    } else {
        "failed"
    };
    PhotosError::Transport(format!("{what} {reason}"))
}

fn image_content_type(response: &Response) -> String {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.starts_with("image/"))
        .unwrap_or("image/jpeg")
        .to_owned()
}

#[derive(Debug, Deserialize)]
struct SessionBody {
    id: String,
    #[serde(rename = "pickerUri")]
    picker_uri: String,
    #[serde(rename = "expireTime")]
    expire_time: DateTime<Utc>,
    #[serde(default, rename = "mediaItemsSet")]
    media_items_set: bool,
}

#[derive(Debug, Deserialize)]
struct ListBody {
    #[serde(default, rename = "mediaItems")]
    media_items: Vec<ItemBody>,
    #[serde(default, rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ItemBody {
    #[serde(default)]
    id: String,
    #[serde(default)]
    caption: Option<String>,
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default, rename = "createTime")]
    create_time: Option<DateTime<Utc>>,
    #[serde(default, rename = "mediaFile")]
    media_file: Option<FileBody>,
}

#[derive(Debug, Deserialize)]
struct FileBody {
    #[serde(default, rename = "baseUrl")]
    base_url: String,
    #[serde(default, rename = "mimeType")]
    mime_type: String,
    #[serde(default, rename = "mediaFileMetadata")]
    metadata: Option<MetadataBody>,
}

#[derive(Debug, Deserialize)]
struct MetadataBody {
    #[serde(default)]
    width: i64,
    #[serde(default)]
    height: i64,
}

impl ItemBody {
    /// Only items with a live download URL can become a note; videos carry none.
    fn into_picked(self) -> Option<PickedItem> {
        if self.r#type.as_deref() == Some("VIDEO") {
            return None;
        }
        let file = self.media_file?;
        if file.base_url.is_empty() {
            return None;
        }
        let metadata = file.metadata.unwrap_or(MetadataBody {
            width: 0,
            height: 0,
        });
        Some(PickedItem {
            id: self.id,
            caption: self.caption,
            create_time: self.create_time,
            base_url: file.base_url,
            mime_type: file.mime_type,
            width: metadata.width,
            height: metadata.height,
        })
    }
}
