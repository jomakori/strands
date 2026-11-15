//! Tag detail view (T6): a summary card plus a collapsible path stack of threads.
//!
//! Reached from the grid at `/tags/:tag_id`. The card mirrors the grid's eyebrow/name
//! pattern; the path stack below it lists every thread with its breadcrumb of path
//! segments (category → tag → thread), each entry collapsible independently.

use dioxus::prelude::*;
use strands_api_types::{GenerateSummaryRequest, SummaryGuide, SummaryVersion};
use uuid::Uuid;

use crate::api;
use crate::data::{self, Tag, ThreadPreview};
use crate::home::{CatGlyph, Grid};
use crate::routes::Route;

/// The whole detail screen: a back link, the tag card, and the path stack.
#[component]
pub fn TagDetail(tag_id: String) -> Element {
    // Same grid the home surface renders, so a just-captured note is already in its tag.
    let grid = use_context::<Grid>().0;
    let tag = data::find_tag(&grid(), &tag_id);

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
        SummaryCard { tag_id: tag.id.to_string() }
        div { class: "path-stack", id: "path-stack", role: "list", "aria-label": "Threads",
            for thread in tag.threads.iter() {
                PathEntry { key: "{thread.id}", tag: tag.clone(), thread: thread.clone() }
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

/// Which guide the chips have selected. Mirrors [`SummaryGuide`] but carries no
/// text of its own, so the selection stays `Copy` — the freeform instruction
/// lives in its own input signal.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GuideChoice {
    Shorter,
    Takeaways,
    Detail,
    Dates,
    Freeform,
}

impl GuideChoice {
    const ALL: [Self; 5] = [
        Self::Shorter,
        Self::Takeaways,
        Self::Detail,
        Self::Dates,
        Self::Freeform,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Shorter => "Shorter",
            Self::Takeaways => "Takeaways",
            Self::Detail => "Detail",
            Self::Dates => "Dates",
            Self::Freeform => "Freeform",
        }
    }

    /// The wire guide, with the typed instruction folded in for freeform.
    fn to_guide(self, text: &str) -> SummaryGuide {
        match self {
            Self::Shorter => SummaryGuide::Shorter,
            Self::Takeaways => SummaryGuide::Takeaways,
            Self::Detail => SummaryGuide::Detail,
            Self::Dates => SummaryGuide::Dates,
            Self::Freeform => SummaryGuide::Freeform {
                text: text.trim().to_owned(),
            },
        }
    }
}

/// A recorded version, flattened for rendering — the list is built before the
/// `rsx!` body because the macro forbids `let` bindings inside it.
struct VersionRow {
    number: i32,
    guide: String,
    model_alias: String,
}

/// The newest version, pre-rendered for the card head.
struct VersionHead {
    badge: String,
    body: String,
}

/// One version's guide as a chip label.
fn guide_label(guide: &SummaryGuide) -> String {
    match guide {
        SummaryGuide::Shorter => "Shorter".to_owned(),
        SummaryGuide::Takeaways => "Takeaways".to_owned(),
        SummaryGuide::Detail => "Detail".to_owned(),
        SummaryGuide::Dates => "Dates".to_owned(),
        SummaryGuide::Freeform { text } if text.trim().is_empty() => "Freeform".to_owned(),
        SummaryGuide::Freeform { text } => format!("Freeform: {text}"),
    }
}

/// Summary card (T7): pick a guide, generate or regenerate, and read the
/// version history with the guide and model alias each version was made with.
#[component]
fn SummaryCard(tag_id: String) -> Element {
    // The fixture-driven grid carries placeholder ids ("t1"); only a real UUID
    // tag can be summarised, so the controls stay inert on the demo pages.
    let tag_uuid = Uuid::parse_str(&tag_id).ok();

    let mut versions = use_signal(Vec::<SummaryVersion>::new);
    let mut selected = use_signal(|| GuideChoice::Shorter);
    let mut freeform = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut notice = use_signal(|| None::<String>);

    // Load the history once the card mounts.
    use_future(move || async move {
        let Some(id) = tag_uuid else {
            return;
        };
        match api::summary_history(id).await {
            Ok(history) => versions.set(history.versions),
            Err(error) => notice.set(Some(format!("Summary history unavailable: {error}"))),
        }
    });

    let latest = versions.read().first().map(|version| VersionHead {
        badge: format!("v{} · {}", version.version, version.model_alias),
        body: version.body.clone(),
    });
    let rows: Vec<VersionRow> = versions
        .read()
        .iter()
        .map(|version| VersionRow {
            number: version.version,
            guide: guide_label(&version.guide),
            model_alias: version.model_alias.clone(),
        })
        .collect();

    let mut run = move |force: bool| {
        if busy() {
            return;
        }
        let Some(id) = tag_uuid else {
            notice.set(Some("Summaries need a saved tag.".to_owned()));
            return;
        };
        let guide = selected().to_guide(&freeform.read());
        if selected() == GuideChoice::Freeform && freeform.read().trim().is_empty() {
            notice.set(Some(
                "Write an instruction for the freeform guide first.".to_owned(),
            ));
            return;
        }

        busy.set(true);
        notice.set(None);
        spawn(async move {
            let request = GenerateSummaryRequest { guide, force };
            match api::generate_summary(id, &request).await {
                // Refetch rather than splice: a short-circuited generate returns
                // the existing version, and the server owns the ordering.
                Ok(version) => match api::summary_history(id).await {
                    Ok(history) => versions.set(history.versions),
                    Err(error) => {
                        let mut list = versions.write();
                        list.retain(|existing| existing.id != version.id);
                        list.insert(0, version);
                        drop(list);
                        notice.set(Some(format!("Version list may be stale: {error}")));
                    }
                },
                Err(error) => notice.set(Some(format!("Summary failed: {error}"))),
            }
            busy.set(false);
        });
    };

    rsx! {
        section { class: "summary-card", id: "summary-card", "aria-label": "Summary",
            div { class: "summary-head",
                h2 { class: "summary-title", "Summary" }
                if let Some(head) = latest.as_ref() {
                    span { class: "summary-badge", "{head.badge}" }
                }
            }
            if let Some(head) = latest.as_ref() {
                p { class: "summary-body", id: "summary-body", "{head.body}" }
            } else {
                p { class: "sub", "No summary yet." }
            }
            div { class: "summary-guides", role: "group", "aria-label": "Summary guide",
                for option in GuideChoice::ALL {
                    button {
                        key: "{option.label()}",
                        class: if selected() == option { "guide-chip on" } else { "guide-chip" },
                        r#type: "button",
                        "aria-pressed": if selected() == option { "true" } else { "false" },
                        onclick: move |_| selected.set(option),
                        "{option.label()}"
                    }
                }
            }
            if selected() == GuideChoice::Freeform {
                input {
                    class: "summary-input",
                    id: "summary-instruction",
                    r#type: "text",
                    placeholder: "What should the summary focus on?",
                    value: freeform(),
                    oninput: move |event| freeform.set(event.value()),
                }
            }
            div { class: "summary-actions",
                button {
                    class: "summary-generate",
                    id: "summary-generate",
                    r#type: "button",
                    disabled: busy(),
                    onclick: move |_| run(false),
                    if busy() { "Working…" } else { "Generate" }
                }
                button {
                    class: "summary-regenerate",
                    id: "summary-regenerate",
                    r#type: "button",
                    disabled: busy(),
                    onclick: move |_| run(true),
                    "Regenerate"
                }
            }
            if let Some(message) = notice() {
                p { class: "sub summary-notice", "{message}" }
            }
            if !rows.is_empty() {
                h3 { class: "summary-history-title", "Versions" }
                ol { class: "summary-versions", id: "summary-versions",
                    for row in rows.iter() {
                        li { key: "{row.number}", class: "summary-version",
                            span { class: "summary-version-number", "v{row.number}" }
                            span { class: "summary-version-guide", "{row.guide}" }
                            span { class: "summary-version-alias", "{row.model_alias}" }
                        }
                    }
                }
            }
        }
    }
}
