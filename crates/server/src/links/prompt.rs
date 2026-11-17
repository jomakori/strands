//! Prompt and response decoding for link judging (ticket T10).
//!
//! One model call sees one batch of candidate pairs. Each pair is numbered and
//! carries both tag names plus a short sample of each tag's latest note, so the
//! model judges topical relatedness rather than string similarity. The reply is
//! a JSON array of `{"score", "rationale"}` objects, one per pair, in order.

use crate::llm::LlmError;

use super::{CandidatePair, PairJudgement};

/// Characters of a tag's context shown to the model; the query already limits
/// the stored snippet, this keeps a caller-supplied one from inflating the
/// prompt.
const CONTEXT_PREVIEW_CHARS: usize = 240;

/// Compose the batch prompt.
pub fn build_judge_prompt(pairs: &[CandidatePair]) -> String {
    let mut prompt = String::from(
        "Two tags are related when they cover the same subject, event, trip or \
         project. For each numbered pair, score how strongly the two tags belong \
         together from 0 (unrelated) to 100 (the same thing), and give one short \
         sentence as the rationale. Be sparing with high scores: sharing one \
         generic word is not a relationship.\n\n\
         Reply with only a JSON array, one object per pair in order, shaped like \
         [{\"score\": 82, \"rationale\": \"Both strands describe the Kyoto trip.\"}].\n\n",
    );

    prompt.push_str("Pairs:\n");
    for (index, pair) in pairs.iter().enumerate() {
        prompt.push_str(&format!(
            "{}. {} / {}\n",
            index + 1,
            pair.a_tag_name,
            pair.b_tag_name
        ));
        push_context(&mut prompt, &pair.a_tag_name, &pair.a_context);
        push_context(&mut prompt, &pair.b_tag_name, &pair.b_context);
    }
    prompt
}

fn push_context(prompt: &mut String, name: &str, context: &str) {
    let snippet: String = context.chars().take(CONTEXT_PREVIEW_CHARS).collect();
    if !snippet.trim().is_empty() {
        prompt.push_str(&format!("   {name}: {}\n", snippet.replace('\n', " ")));
    }
}

#[derive(serde::Deserialize)]
struct RawJudgement {
    score: i64,
    rationale: String,
}

/// Decode the model's reply: a JSON array of judgements, tolerating a fenced
/// code block. A count mismatch is an invalid response, not a partial apply.
/// Scores are clamped to 0-100 and a blank rationale is rejected, because the
/// constellation shows the rationale and an empty label is a rendering bug.
pub fn parse_judgements(raw: &str, expected: usize) -> Result<Vec<PairJudgement>, LlmError> {
    let start = raw
        .find('[')
        .ok_or_else(|| LlmError::InvalidResponse("no JSON array in response".into()))?;
    let end = raw
        .rfind(']')
        .filter(|end| *end > start)
        .ok_or_else(|| LlmError::InvalidResponse("unterminated JSON array in response".into()))?;

    let judgements: Vec<RawJudgement> = serde_json::from_str(&raw[start..=end])
        .map_err(|error| LlmError::InvalidResponse(error.to_string()))?;

    if judgements.len() != expected {
        return Err(LlmError::InvalidResponse(format!(
            "expected {expected} judgements, got {}",
            judgements.len()
        )));
    }

    judgements
        .into_iter()
        .map(|judgement| {
            let rationale = judgement.rationale.trim().to_owned();
            if rationale.is_empty() {
                return Err(LlmError::InvalidResponse(
                    "judge answered a pair with no rationale".into(),
                ));
            }
            Ok(PairJudgement {
                score: judgement.score.clamp(0, 100) as u8,
                rationale,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn pair(a_name: &str, a_context: &str, b_name: &str, b_context: &str) -> CandidatePair {
        CandidatePair {
            a_tag_id: Uuid::from_u128(1),
            a_tag_name: a_name.to_owned(),
            b_tag_id: Uuid::from_u128(2),
            b_tag_name: b_name.to_owned(),
            a_context: a_context.to_owned(),
            b_context: b_context.to_owned(),
        }
    }

    #[test]
    fn the_prompt_numbers_pairs_and_shows_both_contexts() {
        let prompt = build_judge_prompt(&[pair("Food", "ramen in Osaka", "Kyoto", "temple walk")]);

        assert!(prompt.contains("1. Food / Kyoto"));
        assert!(prompt.contains("Food: ramen in Osaka"));
        assert!(prompt.contains("Kyoto: temple walk"));
    }

    #[test]
    fn a_second_pair_is_numbered_too() {
        let prompt = build_judge_prompt(&[
            pair("Food", "ramen", "Kyoto", "temple"),
            pair("Flights", "ANA 12", "Hotels", "ryokan"),
        ]);

        assert!(prompt.contains("1. Food / Kyoto"));
        assert!(prompt.contains("2. Flights / Hotels"));
    }

    #[test]
    fn empty_context_is_left_out_instead_of_rendering_a_blank_label() {
        let prompt = build_judge_prompt(&[pair("Food", "  ", "Kyoto", "")]);
        assert!(!prompt.contains("Food: \n"));
    }

    #[test]
    fn long_context_is_cut_to_the_preview_width() {
        let long = "x".repeat(CONTEXT_PREVIEW_CHARS + 50);
        let prompt = build_judge_prompt(&[pair("Food", &long, "Kyoto", "")]);
        assert!(!prompt.contains(&long));
    }

    #[test]
    fn a_plain_json_array_parses() {
        let judgements = parse_judgements(
            r#"[{"score": 82, "rationale": "Both describe the same trip."}]"#,
            1,
        )
        .expect("parse");
        assert_eq!(judgements[0].score, 82);
        assert_eq!(judgements[0].rationale, "Both describe the same trip.");
    }

    #[test]
    fn a_fenced_json_array_parses() {
        let judgements = parse_judgements(
            "```json\n[{\"score\": 40, \"rationale\": \"Mild overlap.\"}]\n```",
            1,
        )
        .expect("parse fenced reply");
        assert_eq!(judgements[0].score, 40);
    }

    #[test]
    fn out_of_range_scores_are_clamped() {
        let judgements = parse_judgements(
            r#"[{"score": 250, "rationale": "too sure"}, {"score": -3, "rationale": "negative"}]"#,
            2,
        )
        .expect("parse");

        assert_eq!(judgements[0].score, 100);
        assert_eq!(judgements[1].score, 0);
    }

    #[test]
    fn a_blank_rationale_is_an_error() {
        let error = parse_judgements(r#"[{"score": 50, "rationale": "   "}]"#, 1)
            .expect_err("blank rationale must fail");
        assert!(error.to_string().contains("no rationale"));
    }

    #[test]
    fn a_count_mismatch_is_an_error() {
        let error = parse_judgements(r#"[{"score": 50, "rationale": "one"}]"#, 2)
            .expect_err("mismatch must fail");
        assert!(error.to_string().contains("expected 2"));
    }

    #[test]
    fn a_non_json_reply_is_an_error() {
        assert!(parse_judgements("sure, here you go", 1).is_err());
    }
}
