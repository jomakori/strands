//! Account screen (T17): who you are signed in as, a way to take your data out,
//! and a way to erase it.
//!
//! Reached at `/account`. Reads [`crate::api::me`] with `use_resource` (the same
//! fetch-on-mount shape the settings screen uses), so the identity arrives
//! without any manual pending/ready wiring.
//!
//! The destructive action is two-step on purpose: the first press only reveals
//! the consequence and the real button, so a mis-tap cannot reach the server.
//! Export is a plain anchor (`download`) rather than a `fetch` — the browser
//! saves the file with the name the server sets, and no bytes pass through JS.

use dioxus::prelude::*;

use crate::api::{self, ApiError};
use crate::home::Grid;
use crate::routes::Route;

/// The whole account screen: identity, export, then the destructive action.
#[component]
pub fn Account() -> Element {
    let profile = use_resource(api::me);
    // Whether the confirm step is showing. Nothing is deleted until this is true
    // *and* the second press lands.
    let mut confirming = use_signal(|| false);
    let mut deleting = use_signal(|| false);
    let mut deleted = use_signal(|| false);
    let mut failure = use_signal(|| None::<String>);
    let mut grid = use_context::<Grid>().0;

    let confirm_and_delete = move || {
        spawn(async move {
            deleting.set(true);
            failure.set(None);
            match api::delete_my_data().await {
                Ok(()) => {
                    // The grid is the app's shared cache of what exists; it is
                    // empty now, and saying so beats a re-fetch that would also
                    // be empty.
                    grid.set(Vec::new());
                    confirming.set(false);
                    deleted.set(true);
                }
                Err(error) => {
                    failure.set(Some(format_error(&error)));
                    confirming.set(false);
                }
            }
            deleting.set(false);
        });
    };

    rsx! {
        div { class: "app",
            header { class: "top-row",
                Link { class: "back-link", to: Route::HomeView {}, "aria-label": "Back to tags",
                    "← Tags"
                }
            }
            main { class: "feed", "aria-label": "Account", id: "account",
                div { class: "feed-inner",
                    h1 { class: "name", id: "account-title", "Account" }
                    match &*profile.read() {
                        None => rsx! { p { class: "sub", "Loading your account…" } },
                        Some(Err(error)) => rsx! {
                            p { class: "sub", "Could not load your account: {format_error(error)}" }
                        },
                        Some(Ok(identity)) => rsx! {
                            dl { class: "account-identity", id: "account-identity",
                                dt { "Name" }
                                dd { id: "account-name", "{identity.name}" }
                                dt { "Email" }
                                dd { id: "account-email", "{identity.email}" }
                                dt { "Signed in with" }
                                dd { id: "account-provider", "{identity.provider}" }
                            }

                            section { class: "account-section", "aria-labelledby": "account-export-heading",
                                h2 { class: "account-heading", id: "account-export-heading", "Your data" }
                                p { class: "sub",
                                    "Download every tag, thread, note and summary you have written as one JSON file."
                                }
                                a {
                                    class: "button account-action",
                                    id: "account-export",
                                    href: api::EXPORT_PATH,
                                    download: "strands-export.json",
                                    role: "button",
                                    "Export my data"
                                }
                                p { class: "sub", id: "account-kml-hint",
                                    "Saved Google Maps places download as KML: open it in Google Earth or \
                                     My Maps. Places without coordinates are left out of the file."
                                }
                                a {
                                    class: "button account-action",
                                    id: "account-kml",
                                    href: api::KML_PATH,
                                    download: "strands-places.kml",
                                    role: "button",
                                    "Download places as KML"
                                }
                            }

                            section { class: "account-section", "aria-labelledby": "account-delete-heading",
                                h2 { class: "account-heading", id: "account-delete-heading", "Delete my data" }
                                if deleted() {
                                    p { class: "sub", id: "account-deleted", role: "status",
                                        "Your data is deleted. Your account is still here — capture something new whenever you like."
                                    }
                                } else if confirming() {
                                    p { class: "sub account-warning", id: "account-confirm-text", role: "alert",
                                        "This deletes every tag, thread, note and summary you own. It cannot be undone. Your account stays."
                                    }
                                    div { class: "account-confirm-row",
                                        button {
                                            class: "button button-danger",
                                            id: "account-delete-confirm",
                                            disabled: deleting(),
                                            onclick: move |_| confirm_and_delete(),
                                            if deleting() { "Deleting…" } else { "Yes, delete everything" }
                                        }
                                        button {
                                            class: "button account-action",
                                            id: "account-delete-cancel",
                                            onclick: move |_| confirming.set(false),
                                            "Keep my data"
                                        }
                                    }
                                } else {
                                    p { class: "sub", "Erase everything you have captured. Your account stays signed in." }
                                    button {
                                        class: "button account-action",
                                        id: "account-delete",
                                        onclick: move |_| confirming.set(true),
                                        "Delete my data"
                                    }
                                }
                                if let Some(message) = failure() {
                                    p { class: "sub", id: "account-delete-error", role: "alert",
                                        "Could not delete your data: {message}"
                                    }
                                }
                            }
                        },
                    }
                }
            }
        }
    }
}

/// Short, user-facing text for a failed call (matches the settings screen).
fn format_error(error: &ApiError) -> String {
    match error {
        ApiError::Network(_) => "network error".to_owned(),
        ApiError::Status(401) => "you are signed out".to_owned(),
        ApiError::Status(status) => format!("server returned {status}"),
    }
}
