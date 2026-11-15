//! Category DTO: `GET /api/v1/categories` — the chips quick capture files a
//! brand-new tag under.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One category the caller owns, in the order the seed laid it down.
///
/// The capture screen needs the id to create a tag; the name and colour ride
/// along because the same rows will back the grid's category chrome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategorySummary {
    pub id: Uuid,
    pub name: String,
    pub color: String,
    pub position: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_summary_round_trips_through_json() {
        let category = CategorySummary {
            id: Uuid::nil(),
            name: "Travel".to_owned(),
            color: "#2F6F4F".to_owned(),
            position: 0,
        };
        let json = serde_json::to_string(&category).expect("serialize");
        let back: CategorySummary = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(category, back);
    }
}
