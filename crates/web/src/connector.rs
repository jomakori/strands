//! One connector's screen: the summary the settings row drills into, and its import entry.

use dioxus::prelude::*;
use strands_api_types::ConnectorSummary;

use crate::api;
use crate::nav::ScreenHead;
use crate::routes::Route;
use crate::settings::StatusChip;
use crate::state::{self, EmptyNotice, RetryNotice, Skeleton};

/// `GET /api/v1/connectors` narrowed to the `id` this route names.
#[component]
pub fn ConnectorScreen(id: String) -> Element {
    let mut attempt = use_signal(|| 0_u32);
    let connectors = use_resource(move || async move {
        let _ = attempt();
        api::list_connectors().await
    });

    let summary: Option<ConnectorSummary> = connectors
        .read()
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .and_then(|list| list.iter().find(|connector| connector.id == id).cloned());
    let label = summary
        .as_ref()
        .map(|summary| summary.label.clone())
        .unwrap_or_else(|| "Connector".to_string());

    rsx! {
        div { class: "app",
            ScreenHead {
                title: label,
                title_id: Some("connector-title".to_string()),
                back_label: Some("Connectors".to_string()),
                back_to: Some(Route::Settings {}),
                close_to: Route::Settings {},
            }
            main { class: "feed", "aria-label": "Connector detail",
                div { class: "feed-inner",
                    match &*connectors.read() {
                        None => rsx! { Skeleton { lines: 3, label: "Reading this connector…" } },
                        Some(Err(error)) => rsx! {
                            RetryNotice {
                                message: format!("Could not load this connector: {}", state::describe(error)),
                                onretry: move |_| attempt += 1,
                            }
                        },
                        Some(Ok(_)) => match summary {
                            Some(summary) => rsx! { ConnectorPanel { summary } },
                            None => rsx! {
                                EmptyNotice {
                                    title: "Connector not found.".to_owned(),
                                    hint: "It is not registered on this server.".to_owned(),
                                }
                            },
                        },
                    }
                }
            }
        }
    }
}

/// Status, scopes, the error the last check reported, and the way into an import.
#[component]
fn ConnectorPanel(summary: ConnectorSummary) -> Element {
    rsx! {
        section { class: "connector-panel", "data-connector-id": "{summary.id}",
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
            Link {
                class: "button connector-import-link",
                "data-testid": "connector-import-link",
                to: Route::ImportWizard { id: summary.id.clone() },
                "Import…"
            }
        }
    }
}
