//! Tag detail view (T6): a summary card plus a collapsible path stack of threads.
//!
//! Reached from the grid at `/tags/:tag_id`. The card mirrors the grid's eyebrow/name
//! pattern; the path stack below it lists every thread with its breadcrumb of path
//! segments (category → tag → thread), each entry collapsible independently.

use dioxus::prelude::*;
use strands_api_types::{
    GenerateSummaryRequest, MAX_SUMMARY_BODY_CHARS, SummaryGuide, SummaryState, SummaryVersion,
};
use strands_keymap::Action;
use uuid::Uuid;

use crate::api;
use crate::data::{self, Tag, ThreadPreview};
use crate::home::{CatGlyph, Grid};
use crate::keys::Shortcuts;
use crate::routes::Route;
use crate::state::{EmptyNotice, RetryNotice, Skeleton};

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
    let shortcuts = use_context::<Shortcuts>();
    let grid = use_context::<Grid>().0;
    let notice = use_signal(|| None::<String>);
    let tag_id = tag.id.clone();
    let thread_label = data::thread_plural(tag.thread_count);
    let latest = tag
        .threads
        .first()
        .map(|thread| data::relative(thread.age_minutes));

    // The rows can archive; the card owns the line that says what happened.
    use_context_provider(|| DetailNotice(notice));

    // `X` archives the row that has focus. The map owns the key, the focused row names
    // the target, so the keyboard archives exactly what the pointer would.
    use_effect(move || {
        if shortcuts
            .take_screen_action(&[Action::ArchiveThread])
            .is_some()
            && let Some(thread_id) = focused_thread_id()
        {
            archive(grid, &tag_id, &thread_id, notice);
        }
    });

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
        if let Some(text) = notice() {
            p { class: "notice", id: "detail-notice", role: "status", "{text}" }
        }
        SummaryCard { tag_id: tag.id.to_string() }
        div { class: "path-stack", id: "path-stack", role: "list", "aria-label": "Threads",
            for thread in tag.threads.iter() {
                PathEntry { key: "{thread.id}", tag: tag.clone(), thread: thread.clone() }
            }
        }
    }
}

/// The card's one-line feedback; the rows write to it so an archive is never silent.
#[derive(Clone, Copy)]
struct DetailNotice(Signal<Option<String>>);

/// Drop a thread from its tag and hand back its title, when it was still there to drop.
fn archive_thread(mut grid: Signal<Vec<Tag>>, tag_id: &str, thread_id: &str) -> Option<String> {
    let mut tags = grid.write();
    data::archive_thread(&mut tags, tag_id, thread_id)
}

/// Say what was archived; a row that had already gone stays quiet.
fn announce(notice: &mut Signal<Option<String>>, archived: Option<String>) {
    if let Some(title) = archived {
        notice.set(Some(format!("Archived “{title}”.")));
    }
}

/// Archive a thread: the server is asked first and the row only leaves the grid once it
/// has accepted, so a refused request cannot hide a thread that is still live.
fn archive(
    grid: Signal<Vec<Tag>>,
    tag_id: &str,
    thread_id: &str,
    mut notice: Signal<Option<String>>,
) {
    let tag_id = tag_id.to_owned();
    let thread_id = thread_id.to_owned();
    spawn(async move {
        let Ok(id) = Uuid::parse_str(&thread_id) else {
            notice.set(Some("That thread cannot be archived.".to_owned()));
            return;
        };
        match api::archive_thread(id).await {
            Ok(()) => {
                let archived = archive_thread(grid, &tag_id, &thread_id);
                announce(&mut notice, archived);
            }
            Err(error) => notice.set(Some(format!("Could not archive: {error}"))),
        }
    });
}

/// The thread whose row holds focus, when a keyboard user is standing on one.
fn focused_thread_id() -> Option<String> {
    let entry = web_sys::window()?
        .document()?
        .active_element()?
        .closest("[data-path-entry]")
        .ok()??;
    entry.get_attribute("data-thread-id")
}

/// One collapsible thread entry: a summary row that toggles its path segments.
/// Starts expanded so the stack's segments are visible without interaction, and stays
/// collapsible so a crowded stack can be narrowed by hand.
#[component]
fn PathEntry(tag: Tag, thread: ThreadPreview) -> Element {
    let mut open = use_signal(|| true);
    let grid = use_context::<Grid>().0;
    let notice = use_context::<DetailNotice>().0;
    let tag_id = tag.id.clone();
    let thread_id = thread.id.clone();
    let age = data::relative(thread.age_minutes);
    let segments = data::thread_path(&tag, &thread);

    rsx! {
        // `role="listitem"` because the stack is a `role="list"`: a list whose children
        // are bare `div`s announces nothing, so each entry names itself as one item.
        div {
            class: "path-entry",
            role: "listitem",
            "data-path-entry": "true",
            "data-thread-id": "{thread.id}",
            div { class: "path-head",
                button {
                    class: "path-summary",
                    r#type: "button",
                    "aria-expanded": if open() { "true" } else { "false" },
                    onclick: move |_| open.set(!open()),
                    span { class: "path-title", "{thread.title}" }
                    span { class: "path-age", "{age}" }
                }
                button {
                    class: "path-archive",
                    r#type: "button",
                    "aria-label": "Archive “{thread.title}”",
                    onclick: move |_| archive(grid, &tag_id, &thread_id, notice),
                    "Archive"
                }
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
    // The request a failed notice came from, kept so the notice can offer the same call
    // again rather than only describing it.
    let mut retry = use_signal(|| None::<RetryOp>);
    let mut history_open = use_signal(|| true);
    let mut editing = use_signal(|| None::<Uuid>);
    let mut draft = use_signal(String::new);
    let shortcuts = use_context::<Shortcuts>();

    // Load the history whenever the card mounts or a retry bumps the attempt. A
    // resource, not a future: a future spawns once, so the attempt bump would never
    // re-issue the call and the retry button would be decoration.
    let mut history_attempt = use_signal(|| 0_u32);
    let mut history_error = use_signal(|| None::<String>);
    let mut history_loading = use_signal(|| tag_uuid.is_some());
    let _history = use_resource(move || async move {
        let Some(id) = tag_uuid else {
            return;
        };
        let _ = history_attempt();
        history_loading.set(true);
        match api::summary_history(id).await {
            Ok(history) => {
                history_error.set(None);
                history_loading.set(false);
                versions.set(history.versions);
                approved.set(history.approved_version_id);
            }
            Err(error) => {
                history_loading.set(false);
                history_error.set(Some(format!(
                    "Summary history unavailable: {}",
                    crate::state::describe(&error)
                )));
            }
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

    let run = Callback::new(move |force: bool| {
        if busy() {
            return;
        }
        let Some(id) = tag_uuid else {
            notice.set(Some("Summaries need a saved tag.".to_owned()));
            retry.set(None);
            return;
        };
        let guide = selected().to_guide(&freeform.read());
        if selected() == GuideChoice::Freeform && freeform.read().trim().is_empty() {
            notice.set(Some(
                "Write an instruction for the freeform guide first.".to_owned(),
            ));
            retry.set(None);
            return;
        }

        busy.set(true);
        notice.set(None);
        retry.set(None);
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
                        retry.set(Some(RetryOp::Generate { force }));
                    }
                },
                Err(error) => {
                    notice.set(Some(format!("Summary failed: {error}")));
                    retry.set(Some(RetryOp::Generate { force }));
                }
            }
            busy.set(false);
        });
    });

    // One transition, one request: the server answers with the card's whole review
    // state, so nothing has to be spliced locally.
    let review = Callback::new(move |action: ReviewAction| {
        if busy() {
            return;
        }
        let Some(id) = tag_uuid else {
            notice.set(Some("Summaries need a saved tag.".to_owned()));
            retry.set(None);
            return;
        };
        busy.set(true);
        notice.set(None);
        retry.set(None);
        spawn(apply_review(
            id, action, versions, approved, busy, notice, retry,
        ));
    });

    let mut start_edit = move |id: Uuid| {
        editing.set(Some(id));
        draft.set(body_of(&versions.read(), id));
    };
    let cancel_edit = move |_| {
        editing.set(None);
        draft.set(String::new());
    };
    let save_edit = Callback::new(move |_: ()| {
        let (Some(id), Some(version_id)) = (tag_uuid, editing()) else {
            return;
        };
        if busy() {
            return;
        }
        let body = draft();
        if body.trim().is_empty() {
            notice.set(Some("The summary cannot be empty.".to_owned()));
            retry.set(None);
            return;
        }

        busy.set(true);
        notice.set(None);
        retry.set(None);
        spawn(async move {
            match api::edit_summary(id, version_id, &body).await {
                Ok(history) => {
                    versions.set(history.versions);
                    approved.set(history.approved_version_id);
                    editing.set(None);
                    draft.set(String::new());
                }
                Err(error) => {
                    notice.set(Some(format!("Edit failed: {error}")));
                    retry.set(Some(RetryOp::Edit));
                }
            }
            busy.set(false);
        });
    });

    // A / D / H are the card's own bindings: the map resolves them and the shell parks
    // the action here, so the card installs no listener of its own.
    use_effect(move || {
        let Some(action) = shortcuts.take_screen_action(&REVIEW_ACTIONS) else {
            return;
        };
        if busy() {
            return;
        }
        match action {
            Action::ToggleHistory => history_open.toggle(),
            Action::ApproveSummary => {
                if let Some(id) = pending_of(&versions.read(), approved()) {
                    review.call(ReviewAction::Approve(id));
                }
            }
            Action::DiscardSummary => {
                if let Some(id) = pending_of(&versions.read(), approved()) {
                    review.call(ReviewAction::Discard(id));
                }
            }
            _ => {}
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
                        label { class: "sr-only", r#for: "summary-edit", "Summary text" }
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
                                onclick: move |_| save_edit.call(()),
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
                                        review.call(ReviewAction::Approve(id));
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
                                        review.call(ReviewAction::Discard(id));
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
                label { class: "sr-only", r#for: "summary-instruction", "Summary focus" }
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
                    onclick: move |_| run.call(false),
                    if busy() { "Working…" } else { "Generate" }
                }
                button {
                    class: "summary-regenerate",
                    id: "summary-regenerate",
                    r#type: "button",
                    disabled: busy(),
                    onclick: move |_| run.call(true),
                    "Regenerate"
                }
            }
            if let Some(message) = notice() {
                if let Some(op) = retry() {
                    RetryNotice {
                        message,
                        onretry: move |_| match op {
                            RetryOp::Generate { force } => run.call(force),
                            RetryOp::Review(action) => review.call(action),
                            RetryOp::Edit => save_edit.call(()),
                        },
                    }
                } else {
                    p { class: "sub summary-notice", role: "alert", "{message}" }
                }
            }
            if let Some(message) = history_error() {
                RetryNotice {
                    message,
                    onretry: move |_| history_attempt += 1,
                }
            }
            if history_loading() {
                Skeleton { lines: 2, label: "Loading summary history…" }
            }
            if !history_loading() && history_error().is_none() && rows.is_empty() {
                EmptyNotice {
                    title: "No summaries yet.".to_owned(),
                    hint: "Generate one from the transcript to start a version history.".to_owned(),
                }
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

/// The review keys this card answers to; the shell parks any of them for the card.
const REVIEW_ACTIONS: [Action; 3] = [
    Action::ApproveSummary,
    Action::DiscardSummary,
    Action::ToggleHistory,
];

/// The review transitions a card can run.
#[derive(Clone, Copy)]
enum ReviewAction {
    Approve(Uuid),
    Discard(Uuid),
}

/// The request behind a failed notice, so its retry re-issues exactly that call.
#[derive(Clone, Copy)]
enum RetryOp {
    Generate { force: bool },
    Review(ReviewAction),
    Edit,
}

/// Run one transition and fold the server's answer back into the card.
async fn apply_review(
    tag_id: Uuid,
    action: ReviewAction,
    mut versions: Signal<Vec<SummaryVersion>>,
    mut approved: Signal<Option<Uuid>>,
    mut busy: Signal<bool>,
    mut notice: Signal<Option<String>>,
    mut retry: Signal<Option<RetryOp>>,
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
        Err(error) => {
            notice.set(Some(format!("Review action failed: {error}")));
            retry.set(Some(RetryOp::Review(action)));
        }
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
