//! Candidate discovery: which tag pairs share evidence (ticket T10).
//!
//! Evidence is only ever the user's own rows:
//!
//! * **thread** — two notes in the same thread, filed under different tags;
//! * **place** — two places in the same list (or the same provider place),
//!   filed under different tags.
//!
//! Both joins are filtered by `user_id` first, and the pairing is grouped and
//! ranked inside the database: `row_number()` per tag keeps the strongest
//! [`MAX_CANDIDATES_PER_TAG`] partners and the outer `LIMIT` bounds the row
//! count, so the work is capped in SQL rather than after shipping rows to Rust.
//! [`cap_candidates`] repeats the cap in Rust as a cheap guard against a query
//! regression — the model bill is the thing that breaks if it slips.

use sqlx::PgPool;
use strands_core::{TagId, UserId};

use super::CandidatePair;

/// How many partners one tag may be paired with in a single judge run. Twelve
/// keeps a busy tag legible in the constellation and stops one hub tag from
/// pulling the whole grid into the model call.
pub const MAX_CANDIDATES_PER_TAG: usize = 12;

/// Absolute ceiling on candidates returned for one run, so a user with many
/// busy tags still costs a predictable number of judge batches.
pub const MAX_CANDIDATES: usize = MAX_CANDIDATES_PER_TAG * 10;

/// Characters of one tag's latest note kept as judging context.
const CONTEXT_CHARS: i64 = 240;

/// One candidate row: two tag ids and names, plus a short note from each tag as
/// judging context. The context is optional — a place-only tag may have no note.
type CandidateRow = (TagId, String, TagId, String, Option<String>, Option<String>);

/// Candidate pairs for one user, strongest evidence first.
pub async fn find_candidates(
    pool: &PgPool,
    user_id: UserId,
) -> Result<Vec<CandidatePair>, sqlx::Error> {
    let rows: Vec<CandidateRow> = sqlx::query_as(
        "WITH thread_pairs AS ( \
             SELECT n1.tag_id AS a_tag_id, n2.tag_id AS b_tag_id, COUNT(*) AS weight \
             FROM notes n1 \
             JOIN notes n2 ON n2.user_id = n1.user_id \
                 AND n2.thread_id = n1.thread_id \
                 AND n2.tag_id <> n1.tag_id \
             WHERE n1.user_id = $1 AND n1.thread_id IS NOT NULL \
             GROUP BY 1, 2 \
         ), \
         place_pairs AS ( \
             SELECT p1.tag_id AS a_tag_id, p2.tag_id AS b_tag_id, COUNT(*) AS weight \
             FROM places p1 \
             JOIN places p2 ON p2.user_id = p1.user_id \
                 AND p2.tag_id <> p1.tag_id \
                 AND ( \
                     (p1.list_id IS NOT NULL AND p1.list_id = p2.list_id) \
                     OR (p1.place_id IS NOT NULL AND p1.place_id = p2.place_id) \
                 ) \
             WHERE p1.user_id = $1 AND p1.tag_id IS NOT NULL AND p2.tag_id IS NOT NULL \
             GROUP BY 1, 2 \
         ), \
         shared AS ( \
             SELECT a_tag_id, b_tag_id, SUM(weight) AS weight \
             FROM (SELECT * FROM thread_pairs UNION ALL SELECT * FROM place_pairs) evidence \
             GROUP BY 1, 2 \
         ), \
         canonical AS ( \
             SELECT LEAST(a_tag_id, b_tag_id) AS a_tag_id, \
                    GREATEST(a_tag_id, b_tag_id) AS b_tag_id, \
                    SUM(weight) AS weight \
             FROM shared \
             GROUP BY 1, 2 \
         ), \
         ranked AS ( \
             SELECT a_tag_id, b_tag_id, weight, \
                    ROW_NUMBER() OVER (PARTITION BY a_tag_id ORDER BY weight DESC, b_tag_id) AS a_rank, \
                    ROW_NUMBER() OVER (PARTITION BY b_tag_id ORDER BY weight DESC, a_tag_id) AS b_rank \
             FROM canonical \
         ) \
         SELECT r.a_tag_id, ta.name, r.b_tag_id, tb.name, \
                a_note.body, b_note.body \
         FROM ranked r \
         JOIN tags ta ON ta.id = r.a_tag_id AND ta.user_id = $1 \
         JOIN tags tb ON tb.id = r.b_tag_id AND tb.user_id = $1 \
         LEFT JOIN LATERAL ( \
             SELECT LEFT(n.body, $4::int) AS body FROM notes n \
             WHERE n.user_id = $1 AND n.tag_id = r.a_tag_id \
             ORDER BY n.created_at DESC LIMIT 1 \
         ) a_note ON TRUE \
         LEFT JOIN LATERAL ( \
             SELECT LEFT(n.body, $4::int) AS body FROM notes n \
             WHERE n.user_id = $1 AND n.tag_id = r.b_tag_id \
             ORDER BY n.created_at DESC LIMIT 1 \
         ) b_note ON TRUE \
         WHERE r.a_rank <= $2 AND r.b_rank <= $2 \
         ORDER BY r.weight DESC, r.a_tag_id, r.b_tag_id \
         LIMIT $3",
    )
    .bind(user_id)
    .bind(MAX_CANDIDATES_PER_TAG as i64)
    .bind(MAX_CANDIDATES as i64)
    .bind(CONTEXT_CHARS)
    .fetch_all(pool)
    .await?;

    Ok(cap_candidates(rows.into_iter().map(
        |(a_tag_id, a_tag_name, b_tag_id, b_tag_name, a_context, b_context)| CandidatePair {
            a_tag_id,
            a_tag_name,
            b_tag_id,
            b_tag_name,
            a_context: a_context.unwrap_or_default(),
            b_context: b_context.unwrap_or_default(),
        },
    )))
}

/// Keep at most [`MAX_CANDIDATES_PER_TAG`] pairs per tag end, preserving input
/// order (strongest evidence first). A pair is dropped only when one of its
/// tags is already full, so no tag exceeds the cap and no pair is duplicated.
pub fn cap_candidates(pairs: impl IntoIterator<Item = CandidatePair>) -> Vec<CandidatePair> {
    let mut per_tag: std::collections::HashMap<TagId, usize> = std::collections::HashMap::new();
    let mut kept = Vec::new();

    for pair in pairs {
        if kept.len() == MAX_CANDIDATES {
            break;
        }
        let a_count = per_tag.get(&pair.a_tag_id).copied().unwrap_or(0);
        let b_count = per_tag.get(&pair.b_tag_id).copied().unwrap_or(0);
        if a_count >= MAX_CANDIDATES_PER_TAG || b_count >= MAX_CANDIDATES_PER_TAG {
            continue;
        }
        *per_tag.entry(pair.a_tag_id).or_insert(0) += 1;
        *per_tag.entry(pair.b_tag_id).or_insert(0) += 1;
        kept.push(pair);
    }

    kept
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use uuid::Uuid;

    use super::*;

    fn pair(a: TagId, b: TagId) -> CandidatePair {
        CandidatePair {
            a_tag_id: a,
            a_tag_name: "a".to_owned(),
            b_tag_id: b,
            b_tag_name: "b".to_owned(),
            a_context: String::new(),
            b_context: String::new(),
        }
    }

    fn tag(index: u128) -> TagId {
        Uuid::from_u128(index + 1)
    }

    #[test]
    fn a_hub_tag_keeps_at_most_twelve_partners() {
        let hub = tag(0);
        let pairs: Vec<_> = (1..=30).map(|index| pair(hub, tag(index))).collect();

        let kept = cap_candidates(pairs);
        let hub_partners = kept.iter().filter(|pair| pair.a_tag_id == hub).count();

        assert_eq!(hub_partners, MAX_CANDIDATES_PER_TAG);
        assert_eq!(kept.len(), MAX_CANDIDATES_PER_TAG);
    }

    #[test]
    fn the_strongest_evidence_survives_the_cap() {
        let hub = tag(0);
        let pairs: Vec<_> = (1..=20).map(|index| pair(hub, tag(index))).collect();

        let kept = cap_candidates(pairs);

        // Input order is strength order, so the first twelve partners stay.
        assert_eq!(kept[0].b_tag_id, tag(1));
        assert_eq!(
            kept[MAX_CANDIDATES_PER_TAG - 1].b_tag_id,
            tag(MAX_CANDIDATES_PER_TAG as u128)
        );
        assert!(!kept.iter().any(|pair| pair.b_tag_id == tag(20)));
    }

    #[test]
    fn no_tag_exceeds_the_cap_and_nothing_is_duplicated() {
        let mut pairs = Vec::new();
        for a in 0..8u128 {
            for b in (a + 1)..12u128 {
                pairs.push(pair(tag(a), tag(b)));
            }
        }

        let kept = cap_candidates(pairs);

        let mut per_tag: HashMap<TagId, usize> = HashMap::new();
        for pair in &kept {
            *per_tag.entry(pair.a_tag_id).or_insert(0) += 1;
            *per_tag.entry(pair.b_tag_id).or_insert(0) += 1;
        }
        assert!(
            per_tag
                .values()
                .all(|count| *count <= MAX_CANDIDATES_PER_TAG),
            "a tag kept more partners than the cap allows"
        );
        let unique: std::collections::HashSet<_> = kept.iter().map(|pair| pair.key()).collect();
        assert_eq!(unique.len(), kept.len());
    }

    #[test]
    fn no_evidence_is_no_candidates() {
        assert!(cap_candidates(Vec::new()).is_empty());
    }
}
