//! Google Photos Picker DTOs (ticket T14).
//!
//! Two endpoints share these shapes: `POST /api/v1/connectors/photos/sessions`
//! answers a [`PhotoSession`], and polling that session
//! (`GET /api/v1/connectors/photos/sessions/{id}`) answers a [`PhotoPickResult`].
//!
//! A pick is a three-step handoff — create, wait for the user, import — so the
//! poll body has to say what happened, not just return data: `pending` means
//! keep waiting, `imported` carries the receipt, `empty` means the user picked
//! nothing, and `expired` carries a replacement session because a lapsed
//! `baseUrl` cannot be retried.
//!
//! Kept free of `sqlx`/`axum` so `strands-web` can poll without a database
//! driver in the wasm bundle.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::TagSummary;

/// Body of `POST /api/v1/connectors/photos/sessions`: one picker session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhotoSession {
    /// Google's session id, to poll with.
    pub session_id: String,
    /// The URL the user opens to pick photos.
    pub picker_uri: String,
    /// When the session itself lapses.
    pub expires_at: DateTime<Utc>,
}

/// Where a pick stands when the client polls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhotoPickState {
    /// The user has not finished picking yet; poll again.
    Pending,
    /// The picked items became notes.
    Imported,
    /// The user finished without picking anything; nothing was written.
    Empty,
    /// The items' download window lapsed mid-import; `session` is the new one.
    Expired,
}

/// One picked photo, as the note it became.
///
/// Google only reveals what was picked when the user finishes, so this list is
/// the client's first and only per-item view of a pick: the note that was
/// written (or already existed), its title, and where it was filed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickedPhoto {
    /// The note this item wrote, or the note it deduped against.
    pub note_id: Uuid,
    /// The note's title — the caption when it had one, else a plain description.
    pub title: String,
    /// The tag the note was filed under.
    pub tag_id: Uuid,
    /// `false` when the media item was already imported and nothing was written.
    pub created: bool,
}

/// Body of `GET /api/v1/connectors/photos/sessions/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhotoPickResult {
    pub state: PhotoPickState,
    /// The replacement session, set only when `state` is `expired`.
    pub session: Option<PhotoSession>,
    /// Notes written by this pick; 0 again on a repeat poll.
    pub notes_created: usize,
    /// Items already noted, skipped by the media item id.
    pub notes_skipped: usize,
    /// The tags the new notes were filed under, ready for the grid.
    pub tags: Vec<TagSummary>,
    /// One entry per picked photo, in pick order. Empty while `pending`, when
    /// the pick was `empty`, and on a repeat poll of an `imported` session.
    #[serde(default)]
    pub items: Vec<PickedPhoto>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pick_state_serializes_to_the_schema_literal() {
        let json = serde_json::to_string(&PhotoPickState::Expired).expect("serialize");
        assert_eq!(json, "\"expired\"");
    }

    #[test]
    fn an_expired_pick_carries_its_replacement_session() {
        let result = PhotoPickResult {
            state: PhotoPickState::Expired,
            session: Some(PhotoSession {
                session_id: "session-2".to_owned(),
                picker_uri: "https://photos.example/pick/2".to_owned(),
                expires_at: Utc::now(),
            }),
            notes_created: 0,
            notes_skipped: 0,
            tags: Vec::new(),
            items: Vec::new(),
        };
        let json = serde_json::to_string(&result).expect("serialize");
        let back: PhotoPickResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(result, back);
    }

    #[test]
    fn a_pick_lists_one_entry_per_photo_and_says_which_were_new() {
        let result = PhotoPickResult {
            state: PhotoPickState::Imported,
            session: None,
            notes_created: 1,
            notes_skipped: 1,
            tags: Vec::new(),
            items: vec![
                PickedPhoto {
                    note_id: Uuid::nil(),
                    title: "Sunrise over the bay".to_owned(),
                    tag_id: Uuid::nil(),
                    created: true,
                },
                PickedPhoto {
                    note_id: Uuid::nil(),
                    title: "Breakfast at the market".to_owned(),
                    tag_id: Uuid::nil(),
                    created: false,
                },
            ],
        };
        let json = serde_json::to_string(&result).expect("serialize");
        assert!(
            json.contains("\"items\":["),
            "the items are on the wire, not only the counts: {json}"
        );
        let back: PhotoPickResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.items.len(), 2);
        assert!(!back.items[1].created, "a deduped photo is still listed");
    }
}
