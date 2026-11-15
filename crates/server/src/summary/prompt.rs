//! Builds the one prompt sent to the LLM from a tag's notes and the chosen
//! guide (ticket T7).

use strands_api_types::SummaryGuide;

/// Compose the prompt for `guide` over `notes` (a tag's note bodies, oldest
/// first). Each preset carries its own instruction line; a freeform guide
/// uses the caller's own instructions verbatim instead of one of ours.
pub fn build_prompt(guide: &SummaryGuide, notes: &[String]) -> String {
    let instruction = match guide {
        SummaryGuide::Shorter => "Summarize the notes below as concisely as possible.",
        SummaryGuide::Takeaways => {
            "Summarize the notes below as a short bulleted list of key takeaways."
        }
        SummaryGuide::Detail => {
            "Summarize the notes below with as much relevant detail as possible."
        }
        SummaryGuide::Dates => {
            "Summarize the notes below, calling out every date and chronological detail."
        }
        SummaryGuide::Freeform { text } => text.as_str(),
    };

    let mut prompt = String::from(instruction);
    prompt.push_str("\n\nNotes:\n");
    if notes.is_empty() {
        prompt.push_str("(no notes yet)");
    } else {
        for note in notes {
            prompt.push_str("- ");
            prompt.push_str(note);
            prompt.push('\n');
        }
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_preset_carries_its_own_instruction() {
        let notes = vec!["a".to_owned()];
        assert!(build_prompt(&SummaryGuide::Shorter, &notes).contains("concisely"));
        assert!(build_prompt(&SummaryGuide::Takeaways, &notes).contains("takeaways"));
        assert!(build_prompt(&SummaryGuide::Detail, &notes).contains("detail"));
        assert!(build_prompt(&SummaryGuide::Dates, &notes).contains("date"));
    }

    #[test]
    fn freeform_uses_the_callers_text_verbatim() {
        let prompt = build_prompt(
            &SummaryGuide::Freeform {
                text: "focus on dates and places".to_owned(),
            },
            &["a".to_owned()],
        );
        assert!(prompt.starts_with("focus on dates and places"));
    }

    #[test]
    fn every_note_appears_in_the_prompt() {
        let notes = vec!["first note".to_owned(), "second note".to_owned()];
        let prompt = build_prompt(&SummaryGuide::Shorter, &notes);
        assert!(prompt.contains("first note"));
        assert!(prompt.contains("second note"));
    }

    #[test]
    fn no_notes_says_so_rather_than_an_empty_section() {
        let prompt = build_prompt(&SummaryGuide::Shorter, &[]);
        assert!(prompt.contains("no notes yet"));
    }
}
