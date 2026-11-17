//! Connector settings screen (T13): one row per registered connector, each with its
//! scopes, a status chip, and — when the connector's own status check failed — its
//! error text. A failing connector's row renders inline; it never blocks the others
//! or the page itself (ticket acceptance).
//!
//! Reached at `/settings`. Loads through [`crate::api::list_connectors`] with
//! `use_resource` (the same fetch-on-mount shape `use_resource`'s own docs show),
//! so the list re-fetches whenever the component remounts and needs no manual
//! wiring for the pending/ready states.

use dioxus::prelude::*;
use strands_api_types::{ConnectorStatus, ConnectorSummary};

use crate::api;
use crate::routes::Route;
use crate::state::{self, EmptyNotice, RetryNotice, Skeleton};

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
                ConnectorRow { key: "{summary.id}", summary: summary.clone() }
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
