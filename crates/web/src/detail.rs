//! Tag detail view (T6): a summary card plus a collapsible path stack of threads.
//!
//! Reached from the grid at `/tags/:tag_id`. The card mirrors the grid's eyebrow/name
//! pattern; the path stack below it lists every thread with its breadcrumb of path
//! segments (category → tag → thread), each entry collapsible independently.

use dioxus::prelude::*;
use strands_api_types::{
    GenerateSummaryRequest, MAX_SUMMARY_BODY_CHARS, SummaryGuide, SummaryState, SummaryVersion,
};
use uuid::Uuid;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::KeyboardEvent;

use crate::api;
use crate::data::{self, Tag, ThreadPreview};
use crate::home::{CatGlyph, Grid};
use crate::routes::Route;
use crate::search::is_typing;

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
    state: &'static str,
}

/// A version's badge plus its body, pre-rendered for the card.
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

/// Summary card (T7 review gate, T8 approve / discard / edit): the approved body
/// is the committed summary; a generated version is only a proposal until it is
/// approved, and rewriting a proposal records a new `edited` version. A / D / H
/// work on this card alone.
#[component]
fn SummaryCard(tag_id: String) -> Element {
    // The fixture-driven grid carries placeholder ids ("t1"), which are mapped to a
    // stable synthetic UUID so the card's review flow stays exercisable without a
    // server (the e2e spec stubs the endpoints it calls).
    let tag_uuid = Uuid::parse_str(&tag_id)
        .ok()
        .or_else(|| Some(Uuid::new_v5(&Uuid::NAMESPACE_OID, tag_id.as_bytes())));

    let mut versions = use_signal(Vec::<SummaryVersion>::new);
    let mut approved = use_signal(|| None::<Uuid>);
    let mut selected = use_signal(|| GuideChoice::Shorter);
    let mut freeform = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut notice = use_signal(|| None::<String>);
    let mut history_open = use_signal(|| true);
    let mut editing = use_signal(|| None::<Uuid>);
    let mut draft = use_signal(String::new);

    // Load the history once the card mounts.
    use_future(move || async move {
        let Some(id) = tag_uuid else {
            return;
        };
        match api::summary_history(id).await {
            Ok(history) => {
                versions.set(history.versions);
                approved.set(history.approved_version_id);
            }
            Err(error) => notice.set(Some(format!("Summary history unavailable: {error}"))),
        }
    });

    // Derived view state, computed before `rsx!` because the macro forbids `let`.
    let approved_head = approved().and_then(|id| {
        versions
            .read()
            .iter()
            .find(|version| version.id == id)
            .map(|version| VersionHead {
                badge: format!("v{} · {} · approved", version.version, version.model_alias),
                body: version.body.clone(),
            })
    });
    let pending_id = versions
        .read()
        .iter()
        .find(|version| is_pending(version, approved()))
        .map(|version| version.id);
    let pending_head = pending_id.and_then(|id| {
        versions
            .read()
            .iter()
            .find(|version| version.id == id)
            .map(|version| VersionHead {
                badge: format!("v{} · {}", version.version, version.model_alias),
                body: version.body.clone(),
            })
    });
    let editing_pending = pending_id.is_some() && editing() == pending_id;
    let rows: Vec<VersionRow> = versions
        .read()
        .iter()
        .map(|version| VersionRow {
            number: version.version,
            guide: guide_label(&version.guide),
            model_alias: version.model_alias.clone(),
            state: state_label(version.state),
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
                    Ok(history) => {
                        versions.set(history.versions);
                        approved.set(history.approved_version_id);
                    }
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

    // One transition, one request: the server answers with the card's whole review
    // state, so nothing has to be spliced locally.
    let mut review = move |action: ReviewAction| {
        if busy() {
            return;
        }
        let Some(id) = tag_uuid else {
            notice.set(Some("Summaries need a saved tag.".to_owned()));
            return;
        };
        busy.set(true);
        notice.set(None);
        spawn(apply_review(id, action, versions, approved, busy, notice));
    };

    let mut start_edit = move |id: Uuid| {
        editing.set(Some(id));
        draft.set(body_of(&versions.read(), id));
    };
    let cancel_edit = move |_| {
        editing.set(None);
        draft.set(String::new());
    };
    let mut save_edit = move || {
        let (Some(id), Some(version_id)) = (tag_uuid, editing()) else {
            return;
        };
        if busy() {
            return;
        }
        let body = draft();
        if body.trim().is_empty() {
            notice.set(Some("The summary cannot be empty.".to_owned()));
            return;
        }

        busy.set(true);
        notice.set(None);
        spawn(async move {
            match api::edit_summary(id, version_id, &body).await {
                Ok(history) => {
                    versions.set(history.versions);
                    approved.set(history.approved_version_id);
                    editing.set(None);
                    draft.set(String::new());
                }
                Err(error) => notice.set(Some(format!("Edit failed: {error}"))),
            }
            busy.set(false);
        });
    };

    // A / D / H are the card's own bindings. The DOM listener is installed with the
    // card and pulled back out when it unmounts, so they never fire on another
    // screen; every key is handed to a coroutine because a raw DOM callback runs
    // outside Dioxus' runtime and must not touch the card's signals directly.
    let mut shortcuts = use_coroutine(move |mut rx: UnboundedReceiver<Shortcut>| async move {
        while let Some(shortcut) = rx.recv().await {
            if busy() || !card_is_mounted() {
                continue;
            }
            match shortcut {
                Shortcut::History => history_open.toggle(),
                Shortcut::Approve => {
                    if let Some(id) = pending_of(&versions.read(), approved()) {
                        review(ReviewAction::Approve(id));
                    }
                }
                Shortcut::Discard => {
                    if let Some(id) = pending_of(&versions.read(), approved()) {
                        review(ReviewAction::Discard(id));
                    }
                }
            }
        }
    });
    use_card_shortcuts(move |event: KeyboardEvent| {
        if is_typing(&event)
            || event.meta_key()
            || event.ctrl_key()
            || event.alt_key()
            || !card_is_mounted()
        {
            return;
        }

        let key = event.key();
        if key.eq_ignore_ascii_case("a") {
            shortcuts.send(Shortcut::Approve);
        } else if key.eq_ignore_ascii_case("d") {
            shortcuts.send(Shortcut::Discard);
        } else if key.eq_ignore_ascii_case("h") {
            shortcuts.send(Shortcut::History);
        }
    });

    rsx! {
        section { class: "summary-card", id: "summary-card", "aria-label": "Summary",
            div { class: "summary-head",
                h2 { class: "summary-title", "Summary" }
                if let Some(head) = approved_head.as_ref() {
                    span { class: "summary-badge", id: "summary-approved-badge", "{head.badge}" }
                }
            }
            if let Some(head) = approved_head.as_ref() {
                p { class: "summary-body", id: "summary-body", "{head.body}" }
            } else {
                p {
                    class: "sub summary-empty",
                    id: "summary-body",
                    "data-state": "empty",
                    "Nothing approved yet — approve a proposal to commit it."
                }
            }
            if let Some(head) = pending_head.as_ref() {
                div { class: "summary-pending", id: "summary-pending",
                    div { class: "summary-pending-head",
                        span { class: "summary-pending-chip", "Pending approval" }
                        span { class: "summary-badge", "{head.badge}" }
                    }
                    if editing_pending {
                        textarea {
                            class: "summary-edit",
                            id: "summary-edit",
                            rows: "4",
                            maxlength: "{MAX_SUMMARY_BODY_CHARS}",
                            value: "{draft}",
                            oninput: move |event| draft.set(event.value()),
                        }
                        div { class: "summary-actions",
                            button {
                                class: "summary-approve",
                                id: "summary-edit-save",
                                r#type: "button",
                                disabled: busy(),
                                onclick: move |_| save_edit(),
                                "Save as new version"
                            }
                            button {
                                class: "summary-regenerate",
                                id: "summary-edit-cancel",
                                r#type: "button",
                                onclick: cancel_edit,
                                "Cancel"
                            }
                        }
                    } else {
                        p { class: "summary-body", id: "summary-pending-body", "{head.body}" }
                        div { class: "summary-actions",
                            button {
                                class: "summary-approve",
                                id: "summary-approve",
                                r#type: "button",
                                disabled: busy(),
                                title: "Approve (A)",
                                onclick: move |_| {
                                    if let Some(id) = pending_id {
                                        review(ReviewAction::Approve(id));
                                    }
                                },
                                "Approve"
                            }
                            button {
                                class: "summary-discard",
                                id: "summary-discard",
                                r#type: "button",
                                disabled: busy(),
                                title: "Discard (D)",
                                onclick: move |_| {
                                    if let Some(id) = pending_id {
                                        review(ReviewAction::Discard(id));
                                    }
                                },
                                "Discard"
                            }
                            button {
                                class: "summary-regenerate",
                                id: "summary-edit-start",
                                r#type: "button",
                                disabled: busy(),
                                onclick: move |_| {
                                    if let Some(id) = pending_id {
                                        start_edit(id);
                                    }
                                },
                                "Edit"
                            }
                        }
                    }
                }
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
                div { class: "summary-history-head",
                    h3 { class: "summary-history-title", "Versions" }
                    button {
                        class: "summary-history-toggle",
                        id: "summary-history-toggle",
                        r#type: "button",
                        "aria-expanded": if history_open() { "true" } else { "false" },
                        onclick: move |_| history_open.toggle(),
                        if history_open() { "Hide (H)" } else { "History (H)" }
                    }
                }
                if history_open() {
                    ol { class: "summary-versions", id: "summary-versions",
                        for row in rows.iter() {
                            li { key: "{row.number}", class: "summary-version",
                                span { class: "summary-version-number", "v{row.number}" }
                                span { class: "summary-version-guide", "{row.guide}" }
                                span { class: "summary-version-state", "data-state": "{row.state}", "{row.state}" }
                                span { class: "summary-version-alias", "{row.model_alias}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The review transitions a card can run.
#[derive(Clone, Copy)]
/// A key the card answers to, decoded from the DOM before it reaches the signals.
#[derive(Clone, Copy)]
enum Shortcut {
    Approve,
    Discard,
    History,
}

enum ReviewAction {
    Approve(Uuid),
    Discard(Uuid),
}

/// Run one transition and fold the server's answer back into the card.
async fn apply_review(
    tag_id: Uuid,
    action: ReviewAction,
    mut versions: Signal<Vec<SummaryVersion>>,
    mut approved: Signal<Option<Uuid>>,
    mut busy: Signal<bool>,
    mut notice: Signal<Option<String>>,
) {
    let result = match action {
        ReviewAction::Approve(id) => api::approve_summary(tag_id, id).await,
        ReviewAction::Discard(id) => api::discard_summary(tag_id, id).await,
    };

    match result {
        Ok(history) => {
            versions.set(history.versions);
            approved.set(history.approved_version_id);
        }
        Err(error) => notice.set(Some(format!("Review action failed: {error}"))),
    }
    busy.set(false);
}

/// True when a version is a proposal waiting for review: generated or rewritten,
/// and not the version already in force.
fn is_pending(version: &SummaryVersion, approved: Option<Uuid>) -> bool {
    matches!(version.state, SummaryState::Proposed | SummaryState::Edited)
        && Some(version.id) != approved
}

/// The id of the newest proposal, which is what A and D act on.
fn pending_of(versions: &[SummaryVersion], approved: Option<Uuid>) -> Option<Uuid> {
    versions
        .iter()
        .find(|version| is_pending(version, approved))
        .map(|version| version.id)
}

/// One version's body, for prefilling the editor.
fn body_of(versions: &[SummaryVersion], id: Uuid) -> String {
    versions
        .iter()
        .find(|version| version.id == id)
        .map(|version| version.body.clone())
        .unwrap_or_default()
}

/// The state of a version as the history list shows it.
fn state_label(state: SummaryState) -> &'static str {
    match state {
        SummaryState::Proposed => "proposed",
        SummaryState::Approved => "approved",
        SummaryState::Discarded => "discarded",
        SummaryState::Edited => "edited",
        SummaryState::Failed => "failed",
    }
}

/// True while this card is on the page. The bindings are removed on unmount; this
/// keeps a callback that somehow outlived the card from acting on the next screen.
fn card_is_mounted() -> bool {
    web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.query_selector("#summary-card").ok().flatten())
        .is_some()
}

/// The card's document-level key listener, plus the means to take it back down.
#[derive(Clone)]
struct CardShortcuts {
    document: web_sys::Document,
    listener: std::rc::Rc<Closure<dyn FnMut(KeyboardEvent)>>,
}

impl CardShortcuts {
    fn install(handler: impl FnMut(KeyboardEvent) + 'static) -> Option<Self> {
        let document = web_sys::window()?.document()?;
        let listener = std::rc::Rc::new(Closure::<dyn FnMut(KeyboardEvent)>::new(handler));
        document
            .add_event_listener_with_callback("keydown", listener.as_ref().as_ref().unchecked_ref())
            .ok()?;

        Some(Self { document, listener })
    }

    /// Unregisters the listener so A / D / H stop firing once the card is gone.
    fn remove(&self) {
        let _ = self.document.remove_event_listener_with_callback(
            "keydown",
            self.listener.as_ref().as_ref().unchecked_ref(),
        );
    }
}

/// Install the card's key listener for the card's lifetime and take it back down
/// on unmount — the bindings belong to this card, not to the document forever.
fn use_card_shortcuts(handler: impl FnMut(KeyboardEvent) + 'static) {
    let shortcuts = use_hook(move || CardShortcuts::install(handler));
    use_drop(move || {
        if let Some(shortcuts) = shortcuts {
            shortcuts.remove();
        }
    });
}
