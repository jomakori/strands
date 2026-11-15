//! Tag detail view (T6): a summary card plus a collapsible path stack of threads.
//!
//! Reached from the grid at `/tags/:tag_id`. The card mirrors the grid's eyebrow/name
//! pattern; the path stack below it lists every thread with its breadcrumb of path
//! segments (category → tag → thread), each entry collapsible independently.

use dioxus::prelude::*;

use crate::data::{self, Tag, ThreadPreview};
use crate::home::CatGlyph;
use crate::routes::Route;

/// The whole detail screen: a back link, the tag card, and the path stack.
#[component]
pub fn TagDetail(tag_id: String) -> Element {
    let tag = data::find_tag(&tag_id);

    rsx! {
        div { class: "app",
            header { class: "top-row",
                Link { class: "back-link", to: Route::HomeView {}, "aria-label": "Back to tags",
                    "← Tags"
                }
            }
            main { class: "feed", "aria-label": "Tag detail",
                div { class: "feed-inner",
                    match tag {
                        Some(tag) => rsx! { DetailCard { tag } },
                        None => rsx! { p { class: "sub", "Tag not found." } },
                    }
                }
            }
        }
    }
}

/// The card (name, category, thread count, last-activity) plus its path stack.
#[component]
fn DetailCard(tag: Tag) -> Element {
    let thread_label = data::thread_plural(tag.thread_count);
    let latest = tag
        .threads
        .first()
        .map(|thread| data::relative(thread.age_minutes));

    rsx! {
        article {
            class: "detail-card",
            id: "tag-detail",
            style: "--cat: {tag.category.accent()};",
            "data-tag-id": tag.id,
            div { class: "cat-row",
                CatGlyph { category: tag.category, size: 20 }
                span { class: "cat-name", "{tag.category.label()}" }
            }
            h1 { class: "name", id: "detail-name", "{tag.name}" }
            div { class: "detail-meta", id: "detail-meta",
                span { class: "detail-category", "{tag.category.label()}" }
                span { class: "detail-count", "{thread_label}" }
                if let Some(age) = latest {
                    span { class: "detail-age", "Active {age}" }
                }
            }
        }
        div { class: "path-stack", id: "path-stack", role: "list", "aria-label": "Threads",
            for thread in tag.threads.iter() {
                PathEntry { key: "{thread.title}", tag, thread: *thread }
            }
        }
    }
}

/// One collapsible thread entry: a summary row that toggles its path segments.
/// Starts expanded so the stack's segments are visible without interaction, and stays
/// collapsible so a crowded stack can be narrowed by hand.
#[component]
fn PathEntry(tag: Tag, thread: ThreadPreview) -> Element {
    let mut open = use_signal(|| true);
    let age = data::relative(thread.age_minutes);
    let segments = data::thread_path(&tag, &thread);

    rsx! {
        div { class: "path-entry", "data-path-entry": "true",
            button {
                class: "path-summary",
                r#type: "button",
                "aria-expanded": if open() { "true" } else { "false" },
                onclick: move |_| open.set(!open()),
                span { class: "path-title", "{thread.title}" }
                span { class: "path-age", "{age}" }
            }
            if open() {
                ol { class: "path-segments", "data-path-segments": "true",
                    for (index, segment) in segments.iter().enumerate() {
                        li { key: "{index}", class: "path-segment", "{segment}" }
                    }
                }
            }
        }
    }
}
