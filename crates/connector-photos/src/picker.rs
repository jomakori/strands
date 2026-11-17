//! Picker API contracts and the rules that turn a picked photo into a note.
//!
//! The flow is fixed by Google: create a session, the user picks in Google's UI,
//! poll until `mediaItemsSet`, list what was picked, then download each item from
//! its `baseUrl` while that URL is still live. There is no way to read a library
//! directly, and asking for the wrong scope gets a refusal rather than an empty
//! library.

use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, Timelike, Utc};

/// The one scope the Picker API grants: items the user picked, nothing else.
pub const PHOTOS_SCOPE: &str = "https://www.googleapis.com/auth/photospicker.mediaitems.readonly";

/// Google keeps a picked item's `baseUrl` valid for 60 minutes from issue.
pub const BASE_URL_WINDOW_MINUTES: i64 = 60;

/// Largest thumbnail we store: a rendered preview, not the original.
pub const MAX_THUMBNAIL_BYTES: usize = 512 * 1024;

/// The size directive appended to a `baseUrl` to ask for a preview.
const THUMBNAIL_REQUEST: &str = "w512-h512";

/// A picker session: Google's id, the URL the user opens, and when it lapses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerSession {
    pub id: String,
    pub picker_uri: String,
    pub expires_at: DateTime<Utc>,
}

/// Poll answer: `media_items_set` turns true once the user finishes picking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionStatus {
    pub media_items_set: bool,
}

/// One picked photo, as the Picker API describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickedItem {
    pub id: String,
    pub caption: Option<String>,
    pub create_time: Option<DateTime<Utc>>,
    pub base_url: String,
    pub mime_type: String,
    pub width: i64,
    pub height: i64,
}

/// Thumbnail bytes plus the media type to serve them back as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// Every way a Picker call can fail.
///
/// None of the variants carry a URL or a token: the picker's `baseUrl` is a
/// bearer-ish capability and the access token is a credential, so neither may
/// reach a log line.
#[derive(Debug, thiserror::Error)]
pub enum PhotosError {
    /// Google refused the request itself: a bad token, or an unknown session.
    #[error("google photos refused the request: {0}")]
    Refused(String),
    /// The download URL has lapsed; only a fresh session hands out live ones.
    #[error("the picked item's download window has passed")]
    Expired,
    /// Bytes we will not store, whatever the remote claimed its size was.
    #[error("the thumbnail exceeds the {MAX_THUMBNAIL_BYTES}-byte cap")]
    ThumbnailTooLarge,
    /// Network failure, a timeout, or an unexpected status.
    #[error("google photos request failed: {0}")]
    Transport(String),
}

pub type PickerFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The four Picker calls the connector needs.
///
/// A trait so the server can be handed the real client in production and a
/// scripted one in tests, without either side knowing the difference.
pub trait PickerApi: Send + Sync {
    fn create_session<'a>(
        &'a self,
        access_token: &'a str,
    ) -> PickerFuture<'a, Result<PickerSession, PhotosError>>;

    fn session_status<'a>(
        &'a self,
        access_token: &'a str,
        session_id: &'a str,
    ) -> PickerFuture<'a, Result<SessionStatus, PhotosError>>;

    fn list_items<'a>(
        &'a self,
        access_token: &'a str,
        session_id: &'a str,
    ) -> PickerFuture<'a, Result<Vec<PickedItem>, PhotosError>>;

    fn thumbnail<'a>(
        &'a self,
        base_url: &'a str,
    ) -> PickerFuture<'a, Result<Thumbnail, PhotosError>>;
}

/// The picker that is wired when no client exists: it refuses rather than
/// pretending a session was made.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnconfiguredPicker;

impl PickerApi for UnconfiguredPicker {
    fn create_session<'a>(
        &'a self,
        _access_token: &'a str,
    ) -> PickerFuture<'a, Result<PickerSession, PhotosError>> {
        Box::pin(async { Err(unconfigured()) })
    }

    fn session_status<'a>(
        &'a self,
        _access_token: &'a str,
        _session_id: &'a str,
    ) -> PickerFuture<'a, Result<SessionStatus, PhotosError>> {
        Box::pin(async { Err(unconfigured()) })
    }

    fn list_items<'a>(
        &'a self,
        _access_token: &'a str,
        _session_id: &'a str,
    ) -> PickerFuture<'a, Result<Vec<PickedItem>, PhotosError>> {
        Box::pin(async { Err(unconfigured()) })
    }

    fn thumbnail<'a>(
        &'a self,
        _base_url: &'a str,
    ) -> PickerFuture<'a, Result<Thumbnail, PhotosError>> {
        Box::pin(async { Err(unconfigured()) })
    }
}

fn unconfigured() -> PhotosError {
    PhotosError::Transport("the Google Photos picker is not configured".to_owned())
}

/// The preview URL for a picked item, derived from its `baseUrl`.
pub fn thumbnail_url(base_url: &str) -> String {
    format!("{base_url}={THUMBNAIL_REQUEST}")
}

/// Dedupe key: the picker's media item id, so re-picking a photo adds no note.
pub fn dedupe_key(item: &PickedItem) -> String {
    format!("photo:item:{}", item.id)
}

/// Tag hint: the caption when there is one, else the time of day it was taken.
pub fn tag_hint(item: &PickedItem) -> String {
    match caption(item) {
        Some(caption) => caption.to_owned(),
        None => time_of_day(item.create_time).to_owned(),
    }
}

/// Note body: the caption, or a plain description, plus where it came from.
pub fn note_body(item: &PickedItem) -> String {
    let at = item
        .create_time
        .map(|at| at.format("%Y-%m-%d %H:%M UTC").to_string());
    match (caption(item), at) {
        (Some(caption), Some(at)) => format!("{caption}\nGoogle Photos · {at}"),
        (Some(caption), None) => caption.to_owned(),
        (None, Some(at)) => format!("Photo, {at}"),
        (None, None) => "Photo".to_owned(),
    }
}

fn caption(item: &PickedItem) -> Option<&str> {
    item.caption
        .as_deref()
        .map(str::trim)
        .filter(|caption| !caption.is_empty())
}

fn time_of_day(at: Option<DateTime<Utc>>) -> &'static str {
    match at.map(|moment| moment.hour()) {
        Some(5..=11) => "Morning",
        Some(12..=16) => "Afternoon",
        Some(17..=21) => "Evening",
        Some(_) => "Night",
        None => "Photos",
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn item(caption: Option<&str>, hour: Option<u32>) -> PickedItem {
        PickedItem {
            id: "item-1".to_owned(),
            caption: caption.map(str::to_owned),
            create_time: hour.map(|hour| {
                Utc.with_ymd_and_hms(2024, 6, 2, hour, 30, 0)
                    .single()
                    .expect("valid moment")
            }),
            base_url: "https://photos.example/media/abc".to_owned(),
            mime_type: "image/jpeg".to_owned(),
            width: 4032,
            height: 3024,
        }
    }

    #[test]
    fn caption_wins_the_tag_hint() {
        assert_eq!(
            tag_hint(&item(Some("Lisbon tram"), Some(14))),
            "Lisbon tram"
        );
    }

    #[test]
    fn a_blank_caption_falls_back_to_the_hour() {
        assert_eq!(tag_hint(&item(Some("   "), Some(14))), "Afternoon");
        assert_eq!(tag_hint(&item(None, Some(7))), "Morning");
        assert_eq!(tag_hint(&item(None, Some(2))), "Night");
        assert_eq!(tag_hint(&item(None, None)), "Photos");
    }

    #[test]
    fn the_note_body_carries_the_capture_time() {
        let body = note_body(&item(Some("Lisbon tram"), Some(14)));
        assert!(body.starts_with("Lisbon tram"));
        assert!(body.contains("2024-06-02 14:30 UTC"), "{body}");
    }

    #[test]
    fn a_captionless_photo_still_reads_as_a_note() {
        assert_eq!(note_body(&item(None, None)), "Photo");
        assert_eq!(
            note_body(&item(None, Some(9))),
            "Photo, 2024-06-02 09:30 UTC"
        );
    }

    #[test]
    fn the_dedupe_key_is_the_media_item_id() {
        assert_eq!(dedupe_key(&item(None, None)), "photo:item:item-1");
    }

    #[test]
    fn the_preview_url_asks_for_a_bounded_size() {
        assert_eq!(
            thumbnail_url("https://photos.example/media/abc"),
            "https://photos.example/media/abc=w512-h512"
        );
    }
}
