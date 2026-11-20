//! New strand (T31): the grid's one create affordance, and the sheet behind it.
//!
//! The `+` button sits over the feed and opens a single sheet that takes everything a strand
//! needs: a tag (an existing one, picked from a live filter, or a new name typed in), a title
//! and a note. The tag is filed before the note — the thread API only takes a tag id — so a
//! failure between the two says exactly where it stopped and the retry then saves the note
//! only, instead of reporting half a write as a whole one (see [`NewStrandSheet::submit`]).
//!
//! The thread API takes a body and derives the title from its first line, so the sheet's title
//! is the body's first line and the note is the paragraph under it.

use dioxus::prelude::*;
use strands_api_types::CategorySummary;
use uuid::Uuid;

use crate::api;
use crate::data::{self, Category};
use crate::focus_trap::use_focus_trap;
use crate::home::{CatGlyph, Grid};
use crate::keys::Shortcuts;
use crate::state::describe;

/// Body limit, mirroring `strands_core::note::MAX_BODY_CHARS` (`migrations/0005_notes_body_limit.sql`).
/// The server still re-checks it; this keeps the sheet from accepting text that could never save.
const MAX_BODY_CHARS: usize = 5000;

/// Title cut, mirroring `strands_core::note::TITLE_CHARS`: the title is the body's first line,
/// so a longer one would be stored cut and the sheet would be telling a story the server does
/// not.
const TITLE_CHARS: usize = 96;

/// The `+` button. Home only, and the control focus returns to when the sheet closes.
#[component]
pub fn NewStrandFab() -> Element {
    let shortcuts = use_context::<Shortcuts>();

    rsx! {
        button {
            class: "fab",
            id: "new-strand-fab",
            "data-testid": "new-strand-fab",
            r#type: "button",
            "aria-label": "New strand",
            "aria-haspopup": "dialog",
            "aria-expanded": if (shortcuts.compose_open)() { "true" } else { "false" },
            onclick: move |_| {
                let mut open = shortcuts.compose_open;
                open.set(true);
            },
            svg {
                class: "fab__glyph",
                view_box: "0 0 24 24",
                width: "24",
                height: "24",
                "aria-hidden": "true",
                path { d: "M12 5v14M5 12h14", fill: "none", stroke: "currentColor", stroke_width: "2", stroke_linecap: "round" }
            }
        }
    }
}

/// The sheet: tag, title, note, and the two writes it can make.
///
/// `open` and `announce` are the grid's, because the sheet is not the only thing that needs
/// them: `Esc` sets the first through the app's key map, and the polite announcement outlives
/// the sheet the way a toast does.
#[component]
pub fn NewStrandSheet(open: Signal<bool>, announce: Signal<String>) -> Element {
    use_focus_trap(open, "new-strand-sheet");

    let mut open = open;
    let mut announce = announce;
    let mut grid = use_context::<Grid>().0;

    let mut tag_field = use_signal(String::new);
    let mut title = use_signal(String::new);
    let mut note = use_signal(String::new);
    let mut category = use_signal(|| Category::Travel);
    let mut categories = use_signal(Vec::<CategorySummary>::new);
    let mut filed = use_signal(|| None::<(Uuid, String)>);
    let mut saving = use_signal(|| false);
    let mut failure = use_signal(|| None::<String>);
    let attempt = use_signal(|| 0_u32);
    let mut reading = use_signal(|| None::<String>);

    // The categories are the one thing the grid does not carry: a card knows its category's
    // name, a new tag needs that category's id. Read on first open rather than on mount, so an
    // untouched home screen asks the server for nothing extra.
    let _categories = use_resource(move || async move {
        let _ = attempt();
        if !open() {
            return;
        }
        reading.set(None);
        match api::list_categories().await {
            Ok(rows) => categories.set(rows),
            Err(error) => reading.set(Some(format!(
                "Could not load your categories ({}). A new tag cannot be filed yet.",
                describe(&error)
            ))),
        }
    });

    let typed = tag_field();
    let held = filed();
    // One snapshot per render: the combobox, the match test and the save path all read the same
    // grid, so a keystroke cannot be answered against a list that changed underneath it.
    let tags = grid();
    let matches: Vec<String> = tags
        .iter()
        .filter(|tag| typed.is_empty() || tag.name.to_lowercase().contains(&typed.to_lowercase()))
        .filter(|tag| !tag.name.eq_ignore_ascii_case(&typed))
        .map(|tag| tag.name.clone())
        .take(6)
        .collect();
    // Cloned, not borrowed: the save callback outlives this render.
    let exact = tags
        .iter()
        .find(|tag| !typed.is_empty() && tag.name.eq_ignore_ascii_case(&typed))
        .cloned();
    let creating = !typed.trim().is_empty() && exact.is_none();

    let typed_title = title();
    let typed_note = note();
    let used = typed_title.chars().count()
        + if typed_note.is_empty() {
            0
        } else {
            2 + typed_note.chars().count()
        };
    let left = MAX_BODY_CHARS.saturating_sub(used);
    let ready = !typed.trim().is_empty() && !typed_title.trim().is_empty() && !saving() && left > 0;

    let submit = Callback::new(move |_: ()| {
        if saving() {
            return;
        }
        let tag_name = tag_field().trim().to_owned();
        let title_text = title().trim().to_owned();
        if tag_name.is_empty() || title_text.is_empty() {
            return;
        }
        let note_text = note().trim().to_owned();
        let body = if note_text.is_empty() {
            title_text.clone()
        } else {
            format!("{title_text}\n\n{note_text}")
        };
        // A card that is already on the grid knows its own colour: filing a note into it must not
        // repaint it with whatever category the picker happened to be showing.
        let chosen = match exact.as_ref() {
            Some(tag) => tag.category,
            None => category(),
        };
        let category_id = categories()
            .iter()
            .find(|row| row.name.eq_ignore_ascii_case(chosen.label()))
            .map(|row| row.id);
        // The card knows its tag's id already when the tag is on the grid — that is the list the
        // combobox filters, so an existing tag never needs a second read.
        let existing = exact.clone().and_then(|tag| Uuid::parse_str(&tag.id).ok());
        let unanswered = exact.is_some() && existing.is_none();
        let made = filed();

        saving.set(true);
        failure.set(None);

        spawn(async move {
            if unanswered {
                saving.set(false);
                failure.set(Some(
                    "That tag is still saving. Try again in a moment.".to_owned(),
                ));
                return;
            }

            // The tag is filed once. A retry after a failed note reuses it, so the sheet never
            // leaves a second tag behind.
            let tag_id = match made {
                Some((id, _)) => Some(id),
                None => match existing {
                    Some(id) => Some(id),
                    None => {
                        let Some(category_id) = category_id else {
                            saving.set(false);
                            failure.set(Some(format!(
                                "Could not file “{tag_name}”: no category is available."
                            )));
                            return;
                        };
                        match api::create_tag(&tag_name, category_id).await {
                            Ok(row) => {
                                filed.set(Some((row.id, row.name.clone())));
                                Some(row.id)
                            }
                            Err(error) => {
                                saving.set(false);
                                failure.set(Some(format!(
                                    "Could not create the tag “{tag_name}” ({}).",
                                    describe(&error)
                                )));
                                return;
                            }
                        }
                    }
                },
            };
            let Some(tag_id) = tag_id else {
                saving.set(false);
                return;
            };
            let label = filed()
                .map(|(_, name)| name)
                .unwrap_or_else(|| tag_name.clone());
            // Whether this attempt is the one that brought the tag into existence: the failure
            // message has to say so, because that half of the work is already done and a retry
            // must not do it twice.
            let created_now = made.is_none() && existing.is_none() && !unanswered;

            match api::create_thread(&body, Some(tag_id)).await {
                Ok(thread) => {
                    data::add_strand(
                        &mut grid.write(),
                        &data::Strand {
                            tag_id: tag_id.to_string(),
                            tag_name: label.clone(),
                            category: chosen,
                            thread_id: thread.id.to_string(),
                            title: thread.title.clone(),
                        },
                    );
                    announce.set(format!("Saved “{}” under “{label}”.", thread.title));
                    saving.set(false);
                    failure.set(None);
                    tag_field.set(String::new());
                    title.set(String::new());
                    note.set(String::new());
                    filed.set(None);
                    open.set(false);
                }
                Err(error) => {
                    saving.set(false);
                    let why = describe(&error);
                    failure.set(Some(if created_now {
                        format!(
                            "The tag “{label}” was created, but the note did not save ({why}). \
                             Retry saves the note only."
                        )
                    } else {
                        format!("Could not save the note under “{label}” ({why}).")
                    }));
                }
            }
        });
    });

    // Closed: nothing in the tree, so the grid keeps its layout and the trap releases focus.
    if !open() {
        return rsx! {};
    }

    rsx! {
        div {
            class: "sheet",
            id: "new-strand-sheet",
            "data-testid": "new-strand-sheet",
            role: "dialog",
            "aria-modal": "true",
            "aria-labelledby": "new-strand-heading",
            onclick: move |_| open.set(false),
            div {
                class: "sheet__panel composer",
                onclick: move |event| event.stop_propagation(),
                h2 { class: "composer__heading", id: "new-strand-heading", "New strand" }
                form {
                    class: "composer__form",
                    id: "new-strand-form",
                    onsubmit: move |event| {
                        event.prevent_default();
                        submit.call(());
                    },
                    label { class: "capture-label", r#for: "new-strand-tag", "Tag" }
                    if let Some((_, name)) = held {
                        // The tag exists now: the retry is about the note, so the tag is not
                        // editable any more. Saying so is the point — no silent second write.
                        p {
                            class: "composer__filed",
                            id: "new-strand-filed",
                            "data-testid": "new-strand-filed",
                            "Tag “{name}” is created."
                        }
                    } else {
                        input {
                            class: "capture-tag",
                            id: "new-strand-tag",
                            "data-testid": "new-strand-tag",
                            r#type: "text",
                            autocomplete: "off",
                            role: "combobox",
                            "aria-autocomplete": "list",
                            "aria-controls": "new-strand-options",
                            "aria-expanded": if matches.is_empty() && !creating { "false" } else { "true" },
                            placeholder: "Pick a tag, or type a new one",
                            value: "{typed}",
                            oninput: move |event| tag_field.set(event.value()),
                        }
                        div {
                            class: "typeahead",
                            id: "new-strand-options",
                            role: "listbox",
                            "aria-label": "Tags",
                            for name in matches.iter() {
                                button {
                                    key: "{name}",
                                    class: "typeahead-row",
                                    r#type: "button",
                                    role: "option",
                                    "data-testid": "new-strand-option",
                                    "data-name": "{name}",
                                    onclick: {
                                        let name = name.clone();
                                        move |_| tag_field.set(name.clone())
                                    },
                                    span { class: "typeahead-name", "{name}" }
                                }
                            }
                            if creating {
                                button {
                                    class: "typeahead-row create",
                                    id: "new-strand-create",
                                    r#type: "button",
                                    role: "option",
                                    "data-testid": "new-strand-create",
                                    onclick: {
                                        let name = typed.trim().to_owned();
                                        move |_| tag_field.set(name.clone())
                                    },
                                    span { class: "typeahead-name", "Create “{typed.trim()}”" }
                                }
                            }
                        }
                        if creating {
                            div {
                                class: "typeahead",
                                id: "new-strand-cats-panel",
                                p { class: "capture-create-hint", "Create “{typed.trim()}” under" }
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
                    }
                    label { class: "capture-label", r#for: "new-strand-title", "Title" }
                    input {
                        class: "capture-tag",
                        id: "new-strand-title",
                        "data-testid": "new-strand-title",
                        r#type: "text",
                        autocomplete: "off",
                        maxlength: "{TITLE_CHARS}",
                        placeholder: "What is this strand about?",
                        value: "{typed_title}",
                        oninput: move |event| {
                            let text: String = event.value().chars().take(TITLE_CHARS).collect();
                            title.set(text);
                        },
                    }
                    label { class: "capture-label", r#for: "new-strand-note", "Note" }
                    textarea {
                        class: "capture-body composer__note",
                        id: "new-strand-note",
                        "data-testid": "new-strand-note",
                        rows: "4",
                        placeholder: "What did you notice?",
                        value: "{typed_note}",
                        oninput: move |event| {
                            let budget = MAX_BODY_CHARS
                                .saturating_sub(TITLE_CHARS.min(title().chars().count()) + 2);
                            let text: String = event.value().chars().take(budget).collect();
                            note.set(text);
                        },
                    }
                    p {
                        class: if left == 0 { "capture-count at-limit" } else { "capture-count" },
                        id: "new-strand-count",
                        "data-testid": "new-strand-count",
                        "{left} left"
                    }
                    if let Some(message) = reading() {
                        p { class: "composer__hint", id: "new-strand-reading", "{message}" }
                    }
                    if let Some(message) = failure() {
                        div { class: "capture-error", id: "new-strand-error", role: "alert",
                            p { class: "load-error-text", "{message}" }
                            button {
                                class: "button retry-button",
                                id: "new-strand-retry",
                                r#type: "button",
                                "data-retry": "true",
                                onclick: move |_| submit.call(()),
                                "Try again"
                            }
                        }
                    }
                    div { class: "capture-actions",
                        button {
                            class: "button",
                            id: "new-strand-cancel",
                            "data-testid": "new-strand-cancel",
                            r#type: "button",
                            onclick: move |_| open.set(false),
                            "Cancel"
                        }
                        button {
                            class: "capture-save",
                            id: "new-strand-save",
                            "data-testid": "new-strand-save",
                            r#type: "submit",
                            disabled: !ready,
                            if saving() { "Saving…" } else { "Save strand" }
                        }
                    }
                }
            }
        }
    }
}
