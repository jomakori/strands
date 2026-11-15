//! DTO for `GET /api/v1/search` (ticket T11, spec §7.5 search).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One row of `GET /api/v1/search` — a thread plus the tag it lives under, so
/// the search sheet can render a result without a second round trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: Uuid,
    pub title: String,
    /// Plain-text excerpt of whichever field matched (the thread's own title,
    /// or a note filed under it); the title itself when this row is a
    /// "recent" fallback rather than a real match. Never carries markup — the
    /// server's `ts_headline` markers are stripped in SQL so a note's own body
    /// text cannot inject HTML into the client.
    pub snippet: String,
    pub tag_id: Uuid,
    pub tag_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_result_round_trips_through_json() {
        let result = SearchResult {
            id: Uuid::nil(),
            title: "A night that starts at 2 a.m.".to_owned(),
            snippet: "…the night starts at 2 a.m. on the canyon rim…".to_owned(),
            tag_id: Uuid::nil(),
            tag_name: "Travel".to_owned(),
        };
        let json = serde_json::to_string(&result).expect("serialize");
        let back: SearchResult = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(result, back);
    }
}
