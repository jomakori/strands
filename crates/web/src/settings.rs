//! Connector settings screen (T13): one row per registered connector, each with its
//! scopes, a status chip, and — when the connector's own status check failed — its
//! error text. A failing connector's row renders inline; it never blocks the others
//! or the page itself (ticket acceptance).
//!
//! WhatsApp (T28) is the one row with a live lifecycle: its panel polls the link
//! status while it is mounted, so pairing moves the chip to Connected without a
//! page reload, and the QR it renders is the one the phone scans. The disclosure
//! that the bridge is unofficial is part of the row, not a footnote elsewhere.
//!
//! Reached at `/settings`. Loads through [`crate::api::list_connectors`] with
//! `use_resource` (the same fetch-on-mount shape `use_resource`'s own docs show),
//! so the list re-fetches whenever the component remounts and needs no manual
//! wiring for the pending/ready states.

use dioxus::prelude::*;
use strands_api_types::{
    ConnectorStatus, ConnectorSummary, WHATSAPP_DISCLOSURE, WhatsAppLinkState, WhatsAppStatus,
};

use crate::api;
use crate::routes::Route;
use crate::state::{self, EmptyNotice, RetryNotice, Skeleton};

/// How often the WhatsApp panel asks the server for the link state. Two seconds:
/// fast enough that scanning a QR feels immediate, slow enough that an idle
/// settings tab is not a request stream.
const WHATSAPP_POLL: u32 = 2_000;

/// The whole settings screen: a back link, then the connector list.
#[component]
pub fn Settings() -> Element {
    // Bumping `attempt` is what a retry does: the resource re-runs when it changes.
    let mut attempt = use_signal(|| 0_u32);
    let connectors = use_resource(move || async move {
        let _ = attempt();
        api::list_connectors().await
    });

    rsx! {
        div { class: "app",
            header { class: "top-row",
                Link { class: "back-link", to: Route::HomeView {}, "aria-label": "Back to tags",
                    "← Tags"
                }
            }
            main { class: "feed", "aria-label": "Connector settings",
                div { class: "feed-inner",
                    h1 { class: "name", id: "settings-title", "Connectors" }
                    match &*connectors.read() {
                        None => rsx! { Skeleton { lines: 3, label: "Loading connectors…" } },
                        Some(Ok(summaries)) => rsx! { ConnectorList { summaries: summaries.clone() } },
                        Some(Err(error)) => rsx! {
                            RetryNotice {
                                message: format!("Could not load connectors: {}", state::describe(error)),
                                onretry: move |_| attempt += 1,
                            }
                        },
                    }
                }
            }
        }
    }
}

/// The row list, or a short empty state when the registry has nothing registered.
#[component]
fn ConnectorList(summaries: Vec<ConnectorSummary>) -> Element {
    if summaries.is_empty() {
        return rsx! {
            EmptyNotice {
                title: "No connectors registered.".to_owned(),
                hint: "Connectors appear here once the server has one configured.".to_owned(),
            }
        };
    }

    rsx! {
        div { class: "connector-list", id: "connector-list", role: "list", "aria-label": "Connectors",
            for summary in summaries.iter() {
                if summary.id == "whatsapp" {
                    // The WhatsApp row drives its own status from the link
                    // endpoint: the registry summary is a snapshot, and pairing
                    // happens after it was taken.
                    WhatsAppRow { key: "{summary.id}", summary: summary.clone() }
                } else {
                    ConnectorRow { key: "{summary.id}", summary: summary.clone() }
                }
            }
        }
    }
}

/// One connector's row: label, scopes, a status chip, and its error text if it has one.
/// Rendered purely from its own [`ConnectorSummary`] — a neighbour's failure cannot
/// reach in and change this row's markup.
#[component]
fn ConnectorRow(summary: ConnectorSummary) -> Element {
    rsx! {
        article { class: "connector-row", "data-connector-id": "{summary.id}", role: "listitem",
            div { class: "connector-head",
                span { class: "connector-label", "{summary.label}" }
                StatusChip { status: summary.status, has_error: summary.error.is_some() }
            }
            if !summary.scopes.is_empty() {
                ul { class: "connector-scopes", "aria-label": "Scopes",
                    for scope in summary.scopes.iter() {
                        li { class: "connector-scope", "{scope}" }
                    }
                }
            }
            if let Some(error) = &summary.error {
                p { class: "connector-error", "data-connector-error": "true", "{error}" }
            }
        }
    }
}

/// The WhatsApp row (T28): disclosure, live link state, the QR/pair code while a
/// pairing is open, and the four actions the bridge supports.
///
/// The panel polls [`api::whatsapp_status`] for as long as it is mounted, so the
/// chip follows the link without the user reloading anything: scan the code and
/// the next poll says Connected. Nothing else on the screen polls, and a failed
/// link is a message in this row, not an error that replaces the list.
#[component]
fn WhatsAppRow(summary: ConnectorSummary) -> Element {
    let mut link = use_signal(|| None::<WhatsAppStatus>);
    let mut error = use_signal(|| summary.error.clone());
    let mut notice = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);

    use_future(move || async move {
        loop {
            match api::whatsapp_status().await {
                Ok(status) => {
                    // The server's own failure text (a dropped bridge, a failed
                    // unlink) is the honest one to show; clear it when it goes.
                    error.set(status.last_error.clone());
                    link.set(Some(status));
                }
                Err(failure) => error.set(Some(format!("Could not read the link: {failure}"))),
            }
            gloo_timers::future::TimeoutFuture::new(WHATSAPP_POLL).await;
        }
    });

    let snapshot = link();
    let state = snapshot.as_ref().map(|status| status.state.clone());
    let disclosure = snapshot
        .as_ref()
        .map(|status| status.disclosure.clone())
        .unwrap_or_else(|| WHATSAPP_DISCLOSURE.to_owned());

    // Formatting the timestamp here keeps the attribute-free rsx string a plain
    // interpolation: the formatter does not take a closure with a format string.
    let last_sync = snapshot
        .as_ref()
        .and_then(|status| status.last_sync_at)
        .map(|at| at.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "never".to_owned());

    let (chip_key, chip_text) = match (&state, error().is_some()) {
        (Some(WhatsAppLinkState::Linked), _) => ("connected", "Connected"),
        (Some(WhatsAppLinkState::AwaitingScan { .. }), _) => ("needs-sign-in", "Scan the code"),
        (Some(WhatsAppLinkState::Failed { .. }), _) => ("error", "Bridge down"),
        (Some(WhatsAppLinkState::Unlinked), true) => ("error", "Link failed"),
        (Some(WhatsAppLinkState::Unlinked), false) => ("disconnected", "Disconnected"),
        (None, true) => ("error", "Error"),
        (None, false) => match summary.status {
            Some(ConnectorStatus::Connected) => ("connected", "Connected"),
            Some(ConnectorStatus::NeedsSignIn) => ("needs-sign-in", "Needs sign-in"),
            Some(ConnectorStatus::Error) => ("error", "Error"),
            _ => ("disconnected", "Disconnected"),
        },
    };

    rsx! {
        article { class: "connector-row connector-whatsapp", "data-connector-id": "whatsapp",
            role: "listitem",
            div { class: "connector-head",
                span { class: "connector-label", "{summary.label}" }
                span { class: "status-chip", "data-status": "{chip_key}", "{chip_text}" }
            }

            // Always on the row, before any button: this is the risk the user is
            // taking, and it does not disappear once the phone is linked.
            p { class: "whatsapp-disclosure", "data-unofficial": "true", "{disclosure}" }

            match &state {
                Some(WhatsAppLinkState::AwaitingScan { qr, pair_code, expires_in_seconds }) => rsx! {
                    div { class: "whatsapp-pair",
                        if let Some(payload) = qr {
                            img {
                                class: "whatsapp-qr",
                                src: api::whatsapp_qr_url(&payload),
                                alt: "WhatsApp pairing QR code",
                                width: "220",
                                height: "220",
                            }
                            p { class: "whatsapp-hint",
                                "On the phone: WhatsApp → Settings → Linked devices → Link a device, then point the camera at this code."
                            }
                        }
                        if let Some(code) = pair_code {
                            p { class: "whatsapp-pair-code",
                                "Or type this on the phone: " strong { "{code}" }
                            }
                        }
                        p { class: "whatsapp-hint whatsapp-muted",
                            "The code expires in about {*expires_in_seconds / 60} minutes; this row refreshes it on its own."
                        }
                    }
                },
                Some(WhatsAppLinkState::Linked)
                | Some(WhatsAppLinkState::Unlinked)
                | Some(WhatsAppLinkState::Failed { .. })
                | None => rsx! {},
            }

            if let Some(status) = snapshot.as_ref() {
                p { class: "whatsapp-meta whatsapp-muted",
                    "{status.chats.len()} chats on the phone · {status.allowlist.len()} allowed · last sync: {last_sync}"
                }
            }

            if let Some(message) = error() {
                p { class: "connector-error", "data-connector-error": "true", "{message}" }
            }
            if let Some(message) = notice() {
                p { class: "whatsapp-notice", "data-whatsapp-notice": "true", "{message}" }
            }

            div { class: "whatsapp-actions",
                match state {
                    Some(WhatsAppLinkState::Linked) => rsx! {
                        button {
                            class: "whatsapp-action",
                            r#type: "button",
                            disabled: busy(),
                            onclick: move |_| {
                                busy.set(true);
                                notice.set(None);
                                spawn(async move {
                                    match api::whatsapp_sync().await {
                                        Ok(report) => {
                                            error.set(None);
                                            let mut line = format!(
                                                "Imported {} notes from {} chats ({} chats not allowed).",
                                                report.notes_created, report.chats_imported, report.chats_skipped
                                            );
                                            if report.media_skipped > 0 {
                                                line.push_str(&format!(
                                                    " {} attachments had no caption and were skipped.",
                                                    report.media_skipped
                                                ));
                                            }
                                            notice.set(Some(line));
                                        }
                                        Err(failure) => error.set(Some(format!("Sync failed: {failure}"))),
                                    }
                                    busy.set(false);
                                });
                            },
                            "Sync now"
                        }
                        button {
                            class: "whatsapp-action whatsapp-danger",
                            r#type: "button",
                            disabled: busy(),
                            onclick: move |_| {
                                busy.set(true);
                                notice.set(None);
                                spawn(async move {
                                    match api::whatsapp_disconnect().await {
                                        Ok(status) => {
                                            error.set(status.last_error.clone());
                                            link.set(Some(status));
                                            notice.set(Some("The phone is unlinked and the stored session is purged.".to_owned()));
                                        }
                                        Err(failure) => error.set(Some(format!("Disconnect failed: {failure}"))),
                                    }
                                    busy.set(false);
                                });
                            },
                            "Disconnect"
                        }
                    },
                    _ => rsx! {
                        button {
                            class: "whatsapp-action",
                            r#type: "button",
                            disabled: busy(),
                            onclick: move |_| {
                                busy.set(true);
                                notice.set(None);
                                spawn(async move {
                                    match api::whatsapp_pair().await {
                                        Ok(status) => {
                                            error.set(status.last_error.clone());
                                            link.set(Some(status));
                                        }
                                        Err(failure) => error.set(Some(format!("Could not start pairing: {failure}"))),
                                    }
                                    busy.set(false);
                                });
                            },
                            if error().is_some() { "Retry pairing" } else { "Connect" }
                        }
                    },
                }
            }
        }
    }
}

/// The chip: one of the four known statuses, or "Error" when the connector's own
/// status check failed (its message lives in [`ConnectorRow`]'s error text, not here).
#[component]
fn StatusChip(status: Option<ConnectorStatus>, has_error: bool) -> Element {
    let (key, text) = match status {
        Some(ConnectorStatus::Connected) => ("connected", "Connected"),
        Some(ConnectorStatus::NeedsSignIn) => ("needs-sign-in", "Needs sign-in"),
        Some(ConnectorStatus::Disconnected) => ("disconnected", "Disconnected"),
        Some(ConnectorStatus::Error) => ("error", "Error"),
        None if has_error => ("error", "Error"),
        None => ("disconnected", "Disconnected"),
    };

    rsx! {
        span { class: "status-chip", "data-status": "{key}", "{text}" }
    }
}
