//! Connector import wizard (T32): container picker → gallery/review → import N.
//!
//! Reached from a connector's row on `/settings` (every row except WhatsApp, which
//! already has its own Sync control). Three states this screen must answer for on
//! its own: the preview read failed (retry), the connector has nothing to import, or
//! everything it can see is already imported (`ConnectorPreviewItem::already_imported`
//! — a straight read of the ledger `preview` already applied server-side, T33c).
//!
//! Known seam: `ConnectorPreviewItem` (merged in T33c) carries `item_key`, `label`,
//! `timestamp` and `already_imported` only — no container or kind field yet. Until a
//! connector ships that metadata for real, this screen derives both from an assumed
//! `"{kind}/{container}/…"` item_key shape (see [`classify`]); an item_key that does
//! not follow the convention still renders — it just lands under kind `text` and
//! container `"Items"` rather than losing the row. The same honesty applies to the
//! import hand-off: T35 (the job view) is not built, so a successful import stays on
//! this screen and shows its own counts plus a link to this connector's history,
//! instead of routing to a page that does not exist.

use std::collections::{BTreeMap, HashSet};

use dioxus::prelude::*;
use strands_api_types::{ConnectorHistory, WhatsAppLinkState};

use crate::api::{self, ApiError};
use crate::photos_pick;
use crate::routes::Route;
use crate::state::{self, EmptyNotice, RetryNotice, Skeleton};

/// The five kinds the gallery chips and filters know about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Photo,
    Voice,
    Link,
    Place,
}

impl Kind {
    const ALL: [Kind; 5] = [
        Kind::Text,
        Kind::Photo,
        Kind::Voice,
        Kind::Link,
        Kind::Place,
    ];

    /// The `data-kind` / `data-filter` value — stable for the e2e specs.
    fn key(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Photo => "photo",
            Kind::Voice => "voice",
            Kind::Link => "link",
            Kind::Place => "place",
        }
    }

    /// The card's kind chip.
    fn chip_label(self) -> &'static str {
        match self {
            Kind::Text => "Text",
            Kind::Photo => "Photo",
            Kind::Voice => "Voice",
            Kind::Link => "Link",
            Kind::Place => "Place",
        }
    }

    /// The filter row's own label (ticket: "all · text · photos · voice · links · places").
    fn filter_label(self) -> &'static str {
        match self {
            Kind::Text => "Text",
            Kind::Photo => "Photos",
            Kind::Voice => "Voice",
            Kind::Link => "Links",
            Kind::Place => "Places",
        }
    }

    /// The footer's kind summary noun ("3 photos · 2 places").
    fn plural(self) -> &'static str {
        match self {
            Kind::Text => "text notes",
            Kind::Photo => "photos",
            Kind::Voice => "voice notes",
            Kind::Link => "links",
            Kind::Place => "places",
        }
    }
}

/// The filter row's current selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KindFilter {
    All,
    Only(Kind),
}

/// A `preview` item reshaped for this screen: its kind and container derived
/// (see the module doc), everything else carried straight through.
#[derive(Debug, Clone, PartialEq)]
struct Candidate {
    item_key: String,
    label: String,
    kind: Kind,
    container: String,
    already_imported: bool,
}

/// Split `item_key` into its assumed `kind` and `container` segments. See the
/// module doc: this is a stand-in for metadata no connector reports yet, so an
/// item_key that does not match the convention still gets a row — kind `text`,
/// container `"Items"` — rather than being dropped.
fn classify(item_key: &str) -> (Kind, String) {
    let mut parts = item_key.splitn(3, '/');
    let kind = match parts.next().unwrap_or("") {
        "photo" => Kind::Photo,
        "voice" => Kind::Voice,
        "link" => Kind::Link,
        "place" => Kind::Place,
        _ => Kind::Text,
    };
    let container = parts
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or("Items")
        .to_owned();
    (kind, container)
}

/// Which step of the wizard is showing. Carried in the parent so a refreshed
/// preview (after a successful import) does not knock the visitor back to
/// the container list.
#[derive(Debug, Clone)]
enum Step {
    Containers,
    Review { container: Option<String> },
}

/// The connector id whose items arrive by sync rather than by preview.
const WHATSAPP_CONNECTOR: &str = "whatsapp";

/// Why the WhatsApp picker has nothing to show, read off the link state (A3).
///
/// Zero chats alone does not mean "wait for the sync": a phone with no link, a
/// pairing still open, or a bridge that died is empty for a reason the user can
/// act on, and telling them history is on its way would be a lie.
#[derive(Debug, Clone, PartialEq)]
enum PhoneGate {
    /// The probe has not answered yet.
    Loading,
    /// The link snapshot itself could not be read; the message is the reason.
    Unreadable(String),
    /// Linked, but the phone has pushed no chats yet.
    HistoryPending,
    /// No session: never paired, or unlinked from the phone.
    Unlinked,
    /// A pairing attempt is live but unfinished.
    AwaitingScan,
    /// The bridge went down; the server's own reason travels with it.
    Failed(String),
}

/// `/connectors/:id/import`.
///
/// Photos has no preview and nothing to tick: its preview and import routes
/// answer 503 by design, because a pick is the only way photos arrive. So a
/// direct visit here answers with the pick instead of asking for a preview,
/// which is what keeps that 503 pair unreachable from the UI — the wizard below
/// is the only caller of `connector_preview`/`connector_import`, and it never
/// mounts for photos. Every other connector gets it unchanged.
#[component]
pub fn ImportWizard(id: String) -> Element {
    if id == photos_pick::PHOTOS_CONNECTOR_ID {
        return rsx! { PhotosPickOnly {} };
    }
    rsx! { PreviewWizard { id } }
}

/// The Photos stand-in for the wizard: there is no preview to read, and the way
/// in is the pick that lives on the connector's own screen.
#[component]
fn PhotosPickOnly() -> Element {
    rsx! {
        div { class: "app",
            header { class: "top-row",
                Link { class: "back-link", to: Route::Settings {}, "aria-label": "Back to connectors",
                    "← Connectors"
                }
            }
            main { class: "feed", "aria-label": "Import from photos",
                div { class: "feed-inner", "data-testid": "photos-import-pick-only",
                    h1 { class: "name", "Google Photos imports by picking" }
                    EmptyNotice {
                        title: "There is no preview to import.".to_owned(),
                        hint: "Photos arrive through a pick: you choose them in Google's window, and each one is filed as a note when you are done.".to_owned(),
                    }
                    footer { class: "import-footer",
                        Link {
                            class: "button button-primary",
                            to: Route::ConnectorScreen { id: photos_pick::PHOTOS_CONNECTOR_ID.to_string() },
                            "data-testid": "photos-import-start-pick",
                            "Start a pick"
                        }
                    }
                }
            }
        }
    }
}

/// The whole wizard: `/connectors/:id/import`.
#[component]
fn PreviewWizard(id: String) -> Element {
    let mut attempt = use_signal(|| 0_u32);
    let connector_id = id.clone();
    let preview = use_resource(move || {
        let connector_id = connector_id.clone();
        async move {
            let _ = attempt();
            api::connector_preview(&connector_id).await
        }
    });

    // A3: WhatsApp previews nothing — its candidates are whatever the phone has
    // already pushed — so with no chats the picker has an honest answer of its
    // own instead of a preview failure. The probe reads the link snapshot along
    // with the chat list, and it asks for WhatsApp only, so every other connector
    // keeps the preview path below untouched, and so does WhatsApp as soon as
    // chats exist.
    let probe_id = id.clone();
    let phone = use_resource(move || {
        let connector_id = probe_id.clone();
        async move {
            let _ = attempt();
            if connector_id != WHATSAPP_CONNECTOR {
                return None;
            }
            Some(api::whatsapp_status().await)
        }
    });

    let mut step = use_signal(|| Step::Containers);

    // The probe above is only an answer for WhatsApp: for every other connector
    // it resolves to `None` and the gate stays absent, so the preview path runs
    // exactly as before.
    let gate = if id == WHATSAPP_CONNECTOR {
        match &*phone.read() {
            // `None` is the probe still running; `Some(None)` is the probe
            // declining to ask (another connector), which cannot happen here.
            None | Some(None) => Some(PhoneGate::Loading),
            Some(Some(Err(failure))) => Some(PhoneGate::Unreadable(format!(
                "Could not read the WhatsApp link: {}",
                state::describe(failure)
            ))),
            Some(Some(Ok(status))) => match &status.state {
                // Only a live link can be waiting for a sync: the rest are
                // connection problems, and their copy says so.
                WhatsAppLinkState::Linked if status.chats.is_empty() => {
                    Some(PhoneGate::HistoryPending)
                }
                WhatsAppLinkState::Linked => None,
                WhatsAppLinkState::Unlinked => Some(PhoneGate::Unlinked),
                WhatsAppLinkState::AwaitingScan { .. } => Some(PhoneGate::AwaitingScan),
                WhatsAppLinkState::Failed { reason } => Some(PhoneGate::Failed(reason.clone())),
            },
        }
    } else {
        None
    };

    rsx! {
        div { class: "app",
            header { class: "top-row",
                Link { class: "back-link", to: Route::Settings {}, "aria-label": "Back to connectors",
                    "← Connectors"
                }
            }
            main { class: "feed", "aria-label": "Import from {id}",
                div { class: "feed-inner",
                    h1 { class: "name", id: "import-title", "Import from {id}" }
                    match gate {
                        Some(PhoneGate::Loading) => rsx! {
                            Skeleton { lines: 4, label: "Loading candidates…" }
                        },
                        Some(PhoneGate::Unreadable(message)) => rsx! {
                            RetryNotice {
                                message,
                                onretry: move |_| attempt += 1,
                            }
                        },
                        Some(PhoneGate::HistoryPending) => rsx! {
                            PhonePicker {
                                gate: "linked-empty",
                                title: state::WHATSAPP_ZERO_CHAT_TITLE.to_owned(),
                                hint: state::WHATSAPP_ZERO_CHAT_HINT.to_owned(),
                                fixable: false,
                            }
                        },
                        Some(PhoneGate::Unlinked) => rsx! {
                            PhonePicker {
                                gate: "unlinked",
                                title: state::WHATSAPP_UNLINKED_TITLE.to_owned(),
                                hint: state::WHATSAPP_UNLINKED_HINT.to_owned(),
                                fixable: true,
                            }
                        },
                        Some(PhoneGate::AwaitingScan) => rsx! {
                            PhonePicker {
                                gate: "awaiting-scan",
                                title: state::WHATSAPP_PAIRING_TITLE.to_owned(),
                                hint: state::WHATSAPP_PAIRING_HINT.to_owned(),
                                fixable: true,
                            }
                        },
                        Some(PhoneGate::Failed(reason)) => rsx! {
                            PhonePicker {
                                gate: "failed",
                                title: state::WHATSAPP_FAILED_TITLE.to_owned(),
                                hint: format!(
                                    "The bridge stopped: {reason}. {}",
                                    state::WHATSAPP_FAILED_HINT
                                ),
                                fixable: true,
                            }
                        },
                        None => rsx! {
                        match &*preview.read() {
                            None => rsx! { Skeleton { lines: 4, label: "Loading candidates…" } },
                            Some(Err(error)) => rsx! {
                                RetryNotice {
                                    message: format!("Could not load candidates: {}", state::describe(error)),
                                    onretry: move |_| attempt += 1,
                                }
                            },
                            Some(Ok(preview)) => {
                                let candidates: Vec<Candidate> = preview
                                    .items
                                    .iter()
                                    .map(|item| {
                                        let (kind, container) = classify(&item.item_key);
                                        Candidate {
                                            item_key: item.item_key.clone(),
                                            label: item.label.clone(),
                                            kind,
                                            container,
                                            already_imported: item.already_imported,
                                        }
                                    })
                                    .collect();

                                if candidates.is_empty() {
                                    rsx! {
                                        EmptyNotice {
                                            title: "No candidates to import.".to_owned(),
                                            hint: "This connector has nothing new to bring in right now.".to_owned(),
                                        }
                                    }
                                } else if candidates.iter().all(|candidate| candidate.already_imported) {
                                    rsx! {
                                        EmptyNotice {
                                            title: "Everything here is already imported.".to_owned(),
                                            hint: "Check back once this connector sees something new.".to_owned(),
                                        }
                                    }
                                } else {
                                    match step() {
                                        Step::Containers => rsx! {
                                            ContainerStep {
                                                candidates: candidates.clone(),
                                                onselect: move |container: Option<String>| step.set(Step::Review { container }),
                                            }
                                        },
                                        Step::Review { container } => rsx! {
                                            ReviewStep {
                                                connector_id: id.clone(),
                                                all_candidates: candidates.clone(),
                                                container,
                                                onback: move |_| step.set(Step::Containers),
                                                onimported: move |_| attempt += 1,
                                            }
                                        },
                                    }
                                }
                            }
                        }
                        }
                    }
                }
            }
        }
    }
}

/// Screen 0: the picker for a connector whose items arrive by sync, when there
/// is nothing to pick yet (A3).
///
/// The reason goes on screen verbatim and, where the picker still owns an
/// import action, that action stays visibly unavailable rather than failing when
/// pressed. When the reason is the connection itself, the footer points at
/// Settings instead of offering an import that cannot run.
#[component]
fn PhonePicker(gate: &'static str, title: String, hint: String, fixable: bool) -> Element {
    rsx! {
        div {
            class: "import-zero-chats",
            "data-testid": "import-zero-chats",
            "data-whatsapp-gate": gate,
            EmptyNotice { title: title, hint: hint }
            footer { class: "import-footer",
                if fixable {
                    Link {
                        class: "button button-primary",
                        to: Route::Settings {},
                        "data-testid": "import-open-settings",
                        "Open Settings"
                    }
                } else {
                    p { class: "import-summary", id: "import-summary", "aria-live": "polite",
                        "Importing 0 of 0 strands"
                    }
                    button {
                        class: "button button-primary",
                        r#type: "button",
                        "data-testid": "import-submit",
                        disabled: true,
                        "Import"
                    }
                }
            }
        }
    }
}

/// Screen 1: every container this connector can see, with its item count and
/// how many of those are already imported.
#[component]
fn ContainerStep(candidates: Vec<Candidate>, onselect: EventHandler<Option<String>>) -> Element {
    // BTreeMap keeps the row order stable (and alphabetical) across renders.
    let mut groups: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for candidate in &candidates {
        let entry = groups.entry(candidate.container.clone()).or_insert((0, 0));
        entry.0 += 1;
        if candidate.already_imported {
            entry.1 += 1;
        }
    }
    let total = candidates.len();

    rsx! {
        div { class: "import-containers",
            button {
                class: "container-row container-row-all",
                r#type: "button",
                "data-testid": "import-container-all",
                onclick: move |_| onselect.call(None),
                span { class: "container-name", "View all items" }
                span { class: "container-count", "{total} items" }
            }
            div { class: "container-list", role: "list", "aria-label": "Containers",
                for (name , (count , imported)) in groups.into_iter() {
                    button {
                        key: "{name}",
                        class: "container-row",
                        r#type: "button",
                        role: "listitem",
                        "data-testid": "import-container",
                        "data-container": "{name}",
                        onclick: {
                            let name = name.clone();
                            move |_| onselect.call(Some(name.clone()))
                        },
                        span { class: "container-name", "{name}" }
                        span { class: "container-count", "{count} items · {imported} already imported" }
                    }
                }
            }
        }
    }
}

/// Screen 2–4: the gallery, its filters, the already-imported skip state, and
/// the import footer — one component, because the footer's counts and the
/// gallery's selection are the same piece of state.
#[component]
fn ReviewStep(
    connector_id: String,
    all_candidates: Vec<Candidate>,
    container: Option<String>,
    onback: EventHandler<()>,
    onimported: EventHandler<()>,
) -> Element {
    let mut selected = use_signal(HashSet::<String>::new);
    let mut kind_filter = use_signal(|| KindFilter::All);
    let mut importing = use_signal(|| false);
    let mut import_error = use_signal(|| None::<String>);
    let mut result = use_signal(|| None::<strands_api_types::ConnectorImportResult>);
    let mut history_open = use_signal(|| false);
    let mut history = use_signal(|| None::<Result<ConnectorHistory, ApiError>>);

    let scoped: Vec<Candidate> = all_candidates
        .iter()
        .filter(|candidate| {
            container
                .as_deref()
                .is_none_or(|name| name == candidate.container)
        })
        .cloned()
        .collect();
    let visible: Vec<Candidate> = scoped
        .iter()
        .filter(|candidate| match kind_filter() {
            KindFilter::All => true,
            KindFilter::Only(kind) => candidate.kind == kind,
        })
        .cloned()
        .collect();

    let selected_now = selected();
    let total = scoped.len();
    let ticked = scoped
        .iter()
        .filter(|candidate| selected_now.contains(&candidate.item_key))
        .count();
    let kind_summary: Vec<String> = Kind::ALL
        .iter()
        .map(|&kind| {
            let count = scoped
                .iter()
                .filter(|candidate| {
                    candidate.kind == kind && selected_now.contains(&candidate.item_key)
                })
                .count();
            (kind, count)
        })
        .filter(|&(_, count)| count > 0)
        .map(|(kind, count)| format!("{count} {}", kind.plural()))
        .collect();

    let do_import = use_callback({
        let connector_id = connector_id.clone();
        move |_: ()| {
            if importing() {
                return;
            }
            let item_keys: Vec<String> = selected().into_iter().collect();
            if item_keys.is_empty() {
                return;
            }
            importing.set(true);
            import_error.set(None);
            let connector_id = connector_id.clone();
            spawn(async move {
                match api::connector_import(&connector_id, item_keys).await {
                    Ok(report) => {
                        result.set(Some(report));
                        selected.set(HashSet::new());
                        importing.set(false);
                        onimported.call(());
                    }
                    Err(error) => {
                        import_error.set(Some(format!(
                            "Import failed ({}).",
                            state::describe(&error)
                        )));
                        importing.set(false);
                    }
                }
            });
        }
    });

    rsx! {
        div { class: "import-review",
            button {
                class: "back-link",
                r#type: "button",
                "data-testid": "import-back",
                onclick: move |_| onback.call(()),
                "← Containers"
            }

            div { class: "filter-row", role: "group", "aria-label": "Filter by kind",
                button {
                    class: if kind_filter() == KindFilter::All { "filter-chip on" } else { "filter-chip" },
                    r#type: "button",
                    "data-testid": "import-filter",
                    "data-filter": "all",
                    "aria-pressed": if kind_filter() == KindFilter::All { "true" } else { "false" },
                    onclick: move |_| kind_filter.set(KindFilter::All),
                    "All"
                }
                for kind in Kind::ALL {
                    button {
                        key: "{kind.key()}",
                        class: if kind_filter() == KindFilter::Only(kind) { "filter-chip on" } else { "filter-chip" },
                        r#type: "button",
                        "data-testid": "import-filter",
                        "data-filter": kind.key(),
                        "aria-pressed": if kind_filter() == KindFilter::Only(kind) { "true" } else { "false" },
                        onclick: move |_| kind_filter.set(KindFilter::Only(kind)),
                        "{kind.filter_label()}"
                    }
                }
            }

            div { class: "gallery-actions",
                button {
                    class: "button",
                    r#type: "button",
                    "data-testid": "import-select-all",
                    onclick: {
                        let scoped = scoped.clone();
                        move |_| {
                            let keys: HashSet<String> = scoped
                                .iter()
                                .filter(|candidate| !candidate.already_imported)
                                .map(|candidate| candidate.item_key.clone())
                                .collect();
                            selected.set(keys);
                        }
                    },
                    "Select all"
                }
                button {
                    class: "button",
                    r#type: "button",
                    "data-testid": "import-clear",
                    onclick: move |_| selected.set(HashSet::new()),
                    "Clear"
                }
            }

            if visible.is_empty() {
                EmptyNotice {
                    title: "Nothing matches this filter.".to_owned(),
                    hint: "Try a different kind, or clear the filter.".to_owned(),
                }
            } else {
                div { class: "gallery", role: "group", "aria-label": "Candidates",
                    for candidate in visible.iter().cloned() {
                        {
                            let checked = selected_now.contains(&candidate.item_key);
                            let skipped = candidate.already_imported;
                            let toggle_key = candidate.item_key.clone();
                            let toggle_key2 = candidate.item_key.clone();
                            rsx! {
                                div {
                                    key: "{candidate.item_key}",
                                    class: if skipped { "candidate-card skipped" } else if checked { "candidate-card checked" } else { "candidate-card" },
                                    role: "checkbox",
                                    "aria-checked": if checked { "true" } else { "false" },
                                    "aria-disabled": if skipped { "true" } else { "false" },
                                    tabindex: if skipped { "-1" } else { "0" },
                                    "data-testid": "import-candidate",
                                    "data-key": "{candidate.item_key}",
                                    "data-kind": candidate.kind.key(),
                                    "data-skipped": if skipped { "true" } else { "false" },
                                    onclick: move |_| {
                                        if skipped {
                                            return;
                                        }
                                        let mut set = selected();
                                        if set.contains(&toggle_key) {
                                            set.remove(&toggle_key);
                                        } else {
                                            set.insert(toggle_key.clone());
                                        }
                                        selected.set(set);
                                    },
                                    onkeydown: move |event| {
                                        if skipped {
                                            return;
                                        }
                                        let key = event.key().to_string();
                                        if key == "Enter" || key == " " {
                                            event.prevent_default();
                                            let mut set = selected();
                                            if set.contains(&toggle_key2) {
                                                set.remove(&toggle_key2);
                                            } else {
                                                set.insert(toggle_key2.clone());
                                            }
                                            selected.set(set);
                                        }
                                    },
                                    span { class: "kind-chip", "data-kind": candidate.kind.key(), "{candidate.kind.chip_label()}" }
                                    span { class: "candidate-label", "{candidate.label}" }
                                    if skipped {
                                        span { class: "skipped-badge", "data-testid": "import-skipped-badge", "Already imported" }
                                    } else {
                                        span { class: "check-mark", "aria-hidden": "true", if checked { "✓" } else { "" } }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if let Some(message) = import_error() {
                RetryNotice {
                    message,
                    onretry: move |_| do_import(()),
                }
            }

            if let Some(report) = result() {
                div { class: "import-result", role: "status", "aria-live": "polite", "data-testid": "import-result",
                    p {
                        "Imported {report.imported} · skipped {report.skipped} · failed {report.failed}."
                    }
                    button {
                        class: "button",
                        r#type: "button",
                        "data-testid": "import-history-toggle",
                        onclick: {
                            let connector_id = connector_id.clone();
                            move |_| {
                                let opening = !history_open();
                                history_open.set(opening);
                                if opening && history.read().is_none() {
                                    let connector_id = connector_id.clone();
                                    spawn(async move {
                                        history.set(Some(api::connector_history(&connector_id).await));
                                    });
                                }
                            }
                        },
                        if history_open() { "Hide import history" } else { "View import history" }
                    }
                    if history_open() {
                        match &*history.read() {
                            None => rsx! { Skeleton { lines: 2, label: "Loading history…" } },
                            Some(Err(error)) => rsx! {
                                p { class: "load-error-text", "Could not load history ({state::describe(error)})." }
                            },
                            Some(Ok(history)) => rsx! {
                                if history.runs.is_empty() {
                                    p { class: "sub", "No previous runs." }
                                } else {
                                    ul { class: "import-history-list", "data-testid": "import-history-list",
                                        for run in history.runs.iter() {
                                            {
                                                let when = run.imported_at.format("%Y-%m-%d %H:%M").to_string();
                                                let imported = run.imported;
                                                rsx! {
                                                    li {
                                                        key: "{run.imported_at}",
                                                        class: "import-history-row",
                                                        "{when} — {imported} imported"
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            },
                        }
                    }
                }
            }

            footer { class: "import-footer",
                p { class: "import-summary", id: "import-summary", "aria-live": "polite", "Importing {ticked} of {total} strands" }
                if !kind_summary.is_empty() {
                    p { class: "import-kind-summary", "{kind_summary.join(\" · \")}" }
                }
                button {
                    class: "button button-primary",
                    r#type: "button",
                    "data-testid": "import-submit",
                    disabled: ticked == 0 || importing(),
                    onclick: move |_| do_import(()),
                    if importing() { "Importing…" } else { "Import" }
                }
            }
        }
    }
}
