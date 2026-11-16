//! Search sheet (T11): the one overlay the whole app shares.
//!
//! It is mounted once by the [`crate::routes::Shell`] layout rather than per page, so it
//! works from every route and opening the sheet never costs a remount of the view behind
//! it. The keys that open and close it come from the app's single listener (T19) and the
//! map in `strands-keymap`; the sheet itself binds nothing and a backdrop tap closes it.
//!
//! Results are fetched on submit rather than per keystroke: the server ranks a whole note
//! body, so a partial query would spend a request to answer something the user has not
//! finished typing.

use dioxus::prelude::*;
use strands_api_types::SearchResult;

use crate::api;
use crate::keys::{self, Shortcuts};
use crate::routes::Route;

/// The sheet itself; renders nothing at all while closed.
#[component]
pub fn SearchSheet() -> Element {
    let shortcuts = use_context::<Shortcuts>();
    // `open` is the shell's, so the keyboard and the pointer drive the same state.
    let mut open = shortcuts.search_open;
    let mut query = use_signal(String::new);
    // `None` means "no search submitted yet" — that is the recents state.
    let mut submitted = use_signal(|| None::<String>);

    // The overlay is keyboard-first: opening it must land the caret in the input.
    use_effect(move || {
        if open() {
            keys::focus("#search-input");
        }
    });

    // Re-runs whenever the sheet opens or a query is submitted; an empty query asks the
    // endpoint for recents, so both states are one code path.
    let results = use_resource(move || {
        let open = open();
        let query = submitted();
        async move {
            if !open {
                return Vec::new();
            }
            api::search(query.as_deref().unwrap_or_default())
                .await
                .unwrap_or_default()
        }
    });

    let rows = results().unwrap_or_default();
    let loading = results().is_none();
    let typed = !query().trim().is_empty();

    rsx! {
        if open() {
            div {
                id: "search-sheet",
                class: "sheet",
                role: "dialog",
                "aria-modal": "true",
                "aria-label": "Search",
                onclick: move |_| open.set(false),
                div {
                    class: "sheet__panel",
                    // The panel is inside the dismiss target, so keep its own taps local.
                    onclick: move |event| event.stop_propagation(),
                    form {
                        class: "sheet__form",
                        onsubmit: move |event| {
                            event.prevent_default();
                            submitted.set(Some(query().trim().to_owned()));
                        },
                        input {
                            id: "search-input",
                            class: "sheet__input",
                            r#type: "search",
                            placeholder: "Search threads and notes",
                            autocomplete: "off",
                            value: "{query}",
                            oninput: move |event| {
                                let value = event.value();
                                // Emptying the field returns to recents without a submit.
                                if value.trim().is_empty() {
                                    submitted.set(None);
                                }
                                query.set(value);
                            },
                        }
                        button { r#type: "submit", class: "sheet__submit", "Search" }
                    }
                    div { class: "sheet__results", "aria-live": "polite",
                        if typed {
                            h2 { class: "sheet__heading", "Results" }
                        } else {
                            h2 { class: "sheet__heading", "Recent" }
                        }
                        if loading {
                            p { class: "sheet__empty", "Searching…" }
                        } else if rows.is_empty() && typed {
                            p { class: "sheet__empty", "Nothing matches “{query}”." }
                        } else if rows.is_empty() {
                            p { class: "sheet__empty", "No notes yet." }
                        } else {
                            for row in rows {
                                SearchRow { row, open }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One result: the tag it lives under, the thread title, and the matching excerpt.
#[component]
fn SearchRow(row: SearchResult, open: Signal<bool>) -> Element {
    let mut open = open;
    rsx! {
        Link {
            class: "sheet__row",
            to: Route::TagDetail { tag_id: row.tag_id.to_string() },
            onclick: move |_| open.set(false),
            span { class: "sheet__tag", "{row.tag_name}" }
            span { class: "sheet__title", "{row.title}" }
            if row.snippet != row.title {
                span { class: "sheet__snippet", "{row.snippet}" }
            }
        }
    }
}
