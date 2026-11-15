//! Tag DTOs: `GET /api/v1/tags` (list) and `GET /api/v1/tags/{id}` (detail).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One row of `GET /api/v1/tags` — enough to render a grid card, not the full
/// detail payload (no need to ship every field twice over the wire).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagSummary {
    pub id: Uuid,
    pub name: String,
    pub category_id: Uuid,
    /// Threads currently filed under this tag, computed server-side.
    pub thread_count: i64,
    pub created_at: DateTime<Utc>,
}

/// Body of `GET /api/v1/tags/{id}`, scoped to the caller (spec: a foreign id
/// answers 404, never 403 or 200).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagDetail {
    pub id: Uuid,
    pub name: String,
    pub category_id: Uuid,
    pub created_at: DateTime<Utc>,
}

/// Body of `POST /api/v1/tags` — a tag created from quick capture (T12).
///
/// `category_id` must be one of the caller's own categories; a foreign or
/// unknown id answers 404, the same as an unknown tag id does elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewTag {
    pub name: String,
    pub category_id: Uuid,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tag_round_trips_through_json() {
        let request = NewTag {
            name: "Kyoto".to_owned(),
            category_id: Uuid::nil(),
        };
        let json = serde_json::to_string(&request).expect("serialize");
        let back: NewTag = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(request, back);
    }

    #[test]
    fn tag_summary_round_trips_through_json() {
        let summary = TagSummary {
            id: Uuid::nil(),
            name: "Travel".to_owned(),
            category_id: Uuid::nil(),
            thread_count: 3,
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&summary).expect("serialize");
        let back: TagSummary = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(summary, back);
    }
}
