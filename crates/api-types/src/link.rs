//! Link DTOs for T10: `GET /api/v1/links` and `POST /api/v1/links/judge`.
//!
//! A link is one judged pair of tags. The pair is stored canonically — the
//! lexicographically smaller tag id is always `a_tag_id` — so the same two
//! tags can never appear twice, whatever order the judge returned them in.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One judged pair, enough to draw an edge and label it.
///
/// Both names ship with the edge: the constellation draws nodes and edges from
/// the same payload, so a tag that only ever appears as an endpoint of an edge
/// still has a name to render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkSummary {
    pub id: Uuid,
    pub a_tag_id: Uuid,
    pub a_tag_name: String,
    pub b_tag_id: Uuid,
    pub b_tag_name: String,
    /// Judge's confidence, 0-100. Higher means the two strands are more
    /// clearly about the same thing.
    pub score: u8,
    /// One sentence from the judge explaining the score.
    pub rationale: String,
    pub judged_at: DateTime<Utc>,
}

/// Body of `POST /api/v1/links/judge`.
///
/// `judged` counts every candidate pair sent to the model, `created` and
/// `updated` split that count by what happened in the database. On a second
/// run over unchanged evidence `created` is 0 and `updated` equals `judged`,
/// which is the observable form of "judging twice leaves one row per pair".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgeLinksResponse {
    pub judged: u32,
    pub created: u32,
    pub updated: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_summary_round_trips_through_json() {
        let link = LinkSummary {
            id: Uuid::nil(),
            a_tag_id: Uuid::nil(),
            a_tag_name: "Food".to_owned(),
            b_tag_id: Uuid::max(),
            b_tag_name: "Kyoto".to_owned(),
            score: 91,
            rationale: "Both strands come from the same Kyoto trip.".to_owned(),
            judged_at: Utc::now(),
        };
        let json = serde_json::to_string(&link).expect("serialize");
        let back: LinkSummary = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(link, back);
    }

    #[test]
    fn judge_response_round_trips_through_json() {
        let response = JudgeLinksResponse {
            judged: 7,
            created: 5,
            updated: 2,
        };
        let json = serde_json::to_string(&response).expect("serialize");
        let back: JudgeLinksResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(response, back);
    }
}
