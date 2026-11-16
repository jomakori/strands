//! Prompt and response decoding for AI tag assignment (ticket T9).
//!
//! One model call sees one batch: the user's existing tag names (so the model
//! can reuse them) and the items to classify. The reply is a JSON array of tag
//! names, one per item, in input order.

use crate::llm::LlmError;

use super::TagItem;

/// Cap on an item body shown to the model; longer notes still get tagged from
/// their opening, and the prompt stays a predictable size.
const PREVIEW_CHARS: usize = 500;

/// Compose the batch prompt. Existing names are listed so reuse is the easy
/// path; the instruction also permits a new name when nothing fits.
pub fn build_tag_prompt(existing: &[String], items: &[TagItem]) -> String {
    let mut prompt = String::from(
        "Assign one tag to each item below. Prefer an existing tag when it fits; \
         otherwise propose a short new tag name (1-3 words, no punctuation). \
         Reply with only a JSON array of strings, one per item, in order.\n\n",
    );

    prompt.push_str("Existing tags: ");
    if existing.is_empty() {
        prompt.push_str("(none)\n");
    } else {
        prompt.push_str(&existing.join(", "));
        prompt.push('\n');
    }

    prompt.push_str("\nItems:\n");
    for (index, item) in items.iter().enumerate() {
        let preview: String = item.body.chars().take(PREVIEW_CHARS).collect();
        prompt.push_str(&format!("{}. {}\n", index + 1, preview));
    }
    prompt
}

/// Decode the model's reply: a JSON array of tag names, tolerating a fenced
/// code block. A count mismatch is an invalid response, not a partial apply:
/// every item in the batch must get exactly one name.
pub fn parse_tag_names(raw: &str, expected: usize) -> Result<Vec<String>, LlmError> {
    let start = raw
        .find('[')
        .ok_or_else(|| LlmError::InvalidResponse("no JSON array in response".into()))?;
    let end = raw
        .rfind(']')
        .filter(|end| *end > start)
        .ok_or_else(|| LlmError::InvalidResponse("unterminated JSON array in response".into()))?;

    let names: Vec<String> = serde_json::from_str(&raw[start..=end])
        .map_err(|error| LlmError::InvalidResponse(error.to_string()))?;

    if names.len() != expected {
        return Err(LlmError::InvalidResponse(format!(
            "expected {expected} tag names, got {}",
            names.len()
        )));
    }

    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(bodies: &[&str]) -> Vec<TagItem> {
        bodies
            .iter()
            .map(|body| TagItem {
                body: (*body).to_owned(),
            })
            .collect()
    }

    #[test]
    fn the_prompt_lists_existing_tags_and_every_item() {
        let prompt = build_tag_prompt(
            &["Travel".to_owned(), "Food".to_owned()],
            &items(&["flight to Kyoto", "ramen in Osaka"]),
        );
        assert!(prompt.contains("Travel, Food"));
        assert!(prompt.contains("flight to Kyoto"));
        assert!(prompt.contains("ramen in Osaka"));
    }

    #[test]
    fn no_existing_tags_says_none() {
        let prompt = build_tag_prompt(&[], &items(&["a note"]));
        assert!(prompt.contains("(none)"));
    }

    #[test]
    fn a_plain_json_array_parses() {
        let names = parse_tag_names(r#"["Travel","Food"]"#, 2).expect("parse");
        assert_eq!(names, vec!["Travel".to_owned(), "Food".to_owned()]);
    }

    #[test]
    fn a_fenced_json_array_parses() {
        let names = parse_tag_names("```json\n[\"Travel\"]\n```", 1).expect("parse fenced reply");
        assert_eq!(names, vec!["Travel".to_owned()]);
    }

    #[test]
    fn a_count_mismatch_is_an_error() {
        let error = parse_tag_names(r#"["Travel"]"#, 2).expect_err("mismatch must fail");
        assert!(error.to_string().contains("expected 2"));
    }

    #[test]
    fn a_non_json_reply_is_an_error() {
        assert!(parse_tag_names("sure, here you go", 1).is_err());
    }
}
