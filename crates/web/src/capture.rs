//! Quick capture (T12): one screen, one textarea, one tag field, one primary action.
//!
//! The note enters the grid before the server answers — the insert happens while the POST is
//! in flight, so the grid already shows the row when the user lands back on it. The server's
//! answer then rewrites that row in place ([`data::reconcile_capture`]); a refusal removes it
//! again ([`data::rollback_capture`]) and says why, rather than leaving a lie on screen.
//!
//! The tag field is a type-ahead over the caller's tags and doubles as the new-tag name: an
//! existing name files the note under it, an unknown one creates the tag first and files the
//! note under that, so capture never leaves this screen to make a tag.

use dioxus::prelude::*;
use strands_api_types::{CategorySummary, TagSummary};
use uuid::Uuid;

use crate::api;
use crate::data::{self, Category};
use crate::home::{CatGlyph, Grid};
use crate::routes::Route;

/// Body limit, mirroring `strands_core::note::MAX_BODY_CHARS`. The server is still the
/// authority — it re-checks and refuses 5001 — this only keeps the textarea from accepting
/// text that could never be saved.
const MAX_BODY_CHARS: usize = 5000;

/// Title cut, mirroring `strands_core::note::TITLE_CHARS`: the optimistic preview fills the
/// same width as the title the server derives, so reconciliation changes no pixel.
const TITLE_CHARS: usize = 96;

/// The capture screen.
#[component]
pub fn Capture() -> Element {
    let mut grid = use_context::<Grid>().0;
    let navigator = use_navigator();

    let mut body = use_signal(String::new);
    let mut tag_field = use_signal(String::new);
    let mut category = use_signal(|| Category::Travel);
    let mut categories = use_signal(Vec::<CategorySummary>::new);
    let mut known_tags = use_signal(Vec::<TagSummary>::new);
    let mut saving = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);

    // The caller's tags and categories, once, on mount. A failure is not fatal: the grid's
    // own tags still drive the type-ahead and a new tag simply cannot be filed yet.
    use_future(move || async move {
        if let Ok(rows) = api::list_tags().await {
            known_tags.set(rows);
        }
        if let Ok(rows) = api::list_categories().await {
            categories.set(rows);
        }
    });

    let typed = tag_field();
    let matches: Vec<String> = grid()
        .iter()
        .filter(|tag| typed.is_empty() || tag.name.to_lowercase().contains(&typed.to_lowercase()))
        .filter(|tag| !tag.name.eq_ignore_ascii_case(&typed))
        .map(|tag| tag.name.clone())
        .take(4)
        .collect();
    let exact = grid()
        .iter()
        .any(|tag| tag.name.eq_ignore_ascii_case(&typed));
    let creating = !typed.is_empty() && !exact;

    let chars = body().chars().count();
    let ready = chars > 0 && !typed.trim().is_empty() && !saving();

    let mut submit = move |_| {
        let text = body().trim().to_owned();
        let tag_name = tag_field().trim().to_owned();
        if text.is_empty() || tag_name.is_empty() || saving() {
            return;
        }

        let target = known_tags()
            .iter()
            .find(|tag| tag.name.eq_ignore_ascii_case(&tag_name))
            .map(|tag| tag.id);
        let category_id = categories()
            .iter()
            .find(|row| row.name.eq_ignore_ascii_case(category().label()))
            .map(|row| row.id);
        let chosen_category = category();

        let local_id = format!("capture-{}", local_seq());
        data::insert_capture(
            &mut grid.write(),
            &data::Capture {
                local_id: local_id.clone(),
                tag_name: tag_name.clone(),
                category: chosen_category,
                title: preview(&text),
            },
        );

        saving.set(true);
        error.set(None);

        let local_id = local_id.clone();
        spawn(async move {
            let written = write(text, target, &tag_name, category_id).await;
            saving.set(false);
            match written {
                Ok((tag_id, thread_id, title)) => {
                    data::reconcile_capture(
                        &mut grid.write(),
                        &local_id,
                        &tag_id,
                        &thread_id,
                        &title,
                    );
                    navigator.push(Route::HomeView {});
                }
                Err(message) => {
                    data::rollback_capture(&mut grid.write(), &local_id);
                    error.set(Some(message));
                }
            }
        });
    };

    rsx! {
        div { class: "app capture",
            header { class: "top-row",
                Link { class: "back-link", to: Route::HomeView {}, "aria-label": "Back to tags",
                    "← Tags"
                }
            }
            form {
                class: "capture-form",
                id: "capture-form",
                onsubmit: move |event| {
                    event.prevent_default();
                    submit(());
                },
                main { class: "feed", "aria-label": "Quick capture",
                    div { class: "feed-inner",
                        h1 { class: "capture-title", "New note" }
                        textarea {
                            class: "capture-body",
                            id: "capture-body",
                            "data-testid": "capture-body",
                            rows: "6",
                            maxlength: "{MAX_BODY_CHARS}",
                            placeholder: "What did you notice?",
                            "aria-label": "Note",
                            value: "{body()}",
                            oninput: move |event| {
                                let typed: String =
                                    event.value().chars().take(MAX_BODY_CHARS).collect();
                                body.set(typed);
                            },
                        }
                        p {
                            class: if chars >= MAX_BODY_CHARS { "capture-count at-limit" } else { "capture-count" },
                            id: "capture-count",
                            "{chars} / {MAX_BODY_CHARS}"
                        }
                        label { class: "capture-label", r#for: "capture-tag", "Tag" }
                        input {
                            class: "capture-tag",
                            id: "capture-tag",
                            "data-testid": "capture-tag",
                            r#type: "text",
                            autocomplete: "off",
                            placeholder: "Type a tag, or a new one",
                            value: "{typed}",
                            oninput: move |event| tag_field.set(event.value()),
                        }
                        if !matches.is_empty() {
                            div { class: "typeahead", id: "typeahead", role: "listbox",
                                "aria-label": "Matching tags",
                                for name in matches.iter() {
                                    button {
                                        key: "{name}",
                                        class: "typeahead-row",
                                        r#type: "button",
                                        role: "option",
                                        "data-testid": "typeahead-row",
                                        onclick: {
                                            let name = name.clone();
                                            move |_| tag_field.set(name.clone())
                                        },
                                        span { class: "typeahead-name", "{name}" }
                                    }
                                }
                            }
                        }
                        if creating {
                            div { class: "typeahead", id: "capture-create",
                                p { class: "capture-create-hint", "data-testid": "capture-create",
                                    "Create “{typed.trim()}” under"
                                }
                                div { class: "capture-cats", role: "group", "aria-label": "New tag category",
                                    for option in Category::ALL {
                                        button {
                                            key: "{option.key()}",
                                            class: if category() == option { "capture-cat on" } else { "capture-cat" },
                                            r#type: "button",
                                            "data-cat": option.key(),
                                            onclick: move |_| category.set(option),
                                            CatGlyph { category: option, size: 16 }
                                            span { "{option.label()}" }
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(message) = error() {
                            p { class: "capture-error", id: "capture-error", role: "alert",
                                "{message}"
                            }
                        }
                    }
                }
                div { class: "capture-actions",
                    button {
                        class: "capture-save",
                        id: "capture-save",
                        "data-testid": "capture-save",
                        r#type: "submit",
                        disabled: !ready,
                        if saving() { "Saving…" } else { "Save note" }
                    }
                }
            }
        }
    }
}

/// Write one capture: file the tag (creating it when it is new), then the note under it.
async fn write(
    body: String,
    known_tag: Option<Uuid>,
    tag_name: &str,
    category_id: Option<Uuid>,
) -> Result<(String, String, String), String> {
    let tag_id = match known_tag {
        Some(id) => id,
        None => {
            let category_id = category_id.ok_or_else(|| {
                "That tag is new and no category was available to file it under.".to_owned()
            })?;
            api::create_tag(tag_name, category_id)
                .await
                .map_err(|error| format!("Could not create “{tag_name}”: {error}"))?
                .id
        }
    };

    let thread = api::create_thread(&body, Some(tag_id))
        .await
        .map_err(|error| format!("Could not save the note: {error}"))?;

    Ok((tag_id.to_string(), thread.id.to_string(), thread.title))
}

/// The optimistic preview of a body, derived exactly as `Note::title` does server-side:
/// first line with text, runs of whitespace collapsed, cut at [`TITLE_CHARS`] plus an ellipsis.
fn preview(body: &str) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= TITLE_CHARS {
        return collapsed;
    }
    let mut kept: String = collapsed.chars().take(TITLE_CHARS).collect();
    kept.push('…');
    kept
}

/// A process-local counter for optimistic thread ids: unique within the session, which is all
/// the grid needs to find the row again when the server answers.
fn local_seq() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_takes_the_first_line_with_text() {
        assert_eq!(
            preview("\n\n  a   quick\tnote \nsecond line"),
            "a quick note"
        );
    }

    #[test]
    fn preview_cuts_at_the_title_width() {
        let body = "a".repeat(TITLE_CHARS + 20);
        let title = preview(&body);
        assert_eq!(title.chars().count(), TITLE_CHARS + 1);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn body_limit_matches_the_server() {
        assert_eq!(MAX_BODY_CHARS, 5000);
    }
}
