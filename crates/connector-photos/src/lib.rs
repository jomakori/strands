//! Google Photos Picker connector (ticket T14).
//!
//! Photos has no library read any more: the only way in is the Picker API, where
//! the user picks in Google's own UI and we get a short-lived `baseUrl` per item
//! to download from. This crate holds the pieces that are not about storage:
//! the wire contracts, the tag/caption rules, and the HTTP client.
//!
//! The server keeps the session rows, the downloaded bytes and the notes; it
//! takes [`PickerApi`] from here so a test can drive a fake instead of Google.

pub mod google;
pub mod picker;

pub use google::GooglePicker;
pub use picker::{
    MAX_THUMBNAIL_BYTES, PHOTOS_SCOPE, PhotosError, PickedItem, PickerApi, PickerFuture,
    PickerSession, SessionStatus, Thumbnail, UnconfiguredPicker, dedupe_key, note_body, tag_hint,
    thumbnail_url,
};
