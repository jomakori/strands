//! Thread DTOs: `GET /api/v1/tags/{id}/threads` (list) and
//! `GET /api/v1/threads/{id}` (detail).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One row of `GET /api/v1/tags/{id}/threads`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub id: Uuid,
    pub tag_id: Uuid,
    pub title: String,
    pub source_count: i32,
    pub updated_at: DateTime<Utc>,
}

/// Body of `GET /api/v1/threads/{id}`, scoped to the caller (spec: a foreign
/// id answers 404, never 403 or 200).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadDetail {
    pub id: Uuid,
    pub tag_id: Uuid,
    pub title: String,
    pub source_count: i32,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_summary_round_trips_through_json() {
        let summary = ThreadSummary {
            id: Uuid::nil(),
            tag_id: Uuid::nil(),
            title: "A night that starts at 2 a.m.".to_owned(),
            source_count: 2,
            updated_at: Utc::now(),
        };
        let json = serde_json::to_string(&summary).expect("serialize");
        let back: ThreadSummary = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(summary, back);
    }
}
