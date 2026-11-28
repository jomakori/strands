//! The Google Photos pick (ticket B5): the one way photos come in.
//!
//! Photos has no library read any more. The user picks in Google's own UI, we
//! get short-lived per-item URLs back, and the server turns them into notes — so
//! there is no preview to browse and no import list to tick. That is why this
//! panel replaces the generic `Import…` entry on the Photos row: the generic
//! path's preview/import routes answer 503 for this connector by design
//! (`crates/server/src/connectors/photos.rs`), and a link into them is a dead
//! end the UI should never offer.
//!
//! The flow is one round trip the user cannot see: `POST …/photos/sessions`
//! opens the pick, this panel shows the `picker_uri` to open, and a timer polls
//! `GET …/photos/sessions/{id}` until the pick reports `imported`, `empty`, or
//! `expired`. A lapsed session is replaced by the server, so `expired` keeps
//! polling the replacement instead of offering a retry that would 400.
//!
//! The panel is mounted twice — on the Settings row and on the connector screen
//! — because both are places a user looks for it. Each mount owns its own
//! session; nothing is shared, so closing one screen cannot strand the other.

use dioxus::prelude::*;
use strands_api_types::{ConnectorStatus, PhotoPickResult, PhotoPickState, PhotoSession};

use crate::api;
use crate::routes::Route;

/// How often an open pick is polled. Faster than the WhatsApp row's two
/// seconds: the user is actively waiting on this one, and the server answers
/// `pending` with no upstream call of its own until the pick is done.
const PHOTOS_POLL: u32 = 1_500;

/// The Photos connector's registry id (`connector-photos`'s manifest). The
/// Settings row and the connector screen both key the panel off this, because
/// the rows are rendered from the registry rather than from a list the client
/// keeps.
pub const PHOTOS_CONNECTOR_ID: &str = "photos";

/// The receipt line for a finished pick.
///
/// It has to read correctly on a *repeat* pick too — the same photos picked
/// again write nothing and skip by media item id — so the counts are named
/// rather than assumed: "nothing new" is a different sentence from "nothing
/// picked".
fn receipt_line(result: &PhotoPickResult) -> String {
    match (result.notes_created, result.notes_skipped) {
        (0, 0) => "Nothing was written: the pick came back empty.".to_owned(),
        (created, 0) => format!("Imported {created} photos."),
        (0, skipped) => format!("Nothing new: all {skipped} picked photos were already here."),
        (created, skipped) => format!("Imported {created} photos ({skipped} already here)."),
    }
}

/// The picker panel: start a pick, show the window to pick in, then the receipt.
#[component]
pub fn PhotoPicker(status: Option<ConnectorStatus>) -> Element {
    let mut session = use_signal(|| None::<PhotoSession>);
    let mut picked = use_signal(|| None::<PhotoPickResult>);
    let mut failure = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);

    // One timer for the panel's life, the same shape the WhatsApp row uses: it
    // reads the open session each tick, so starting a pick needs no new future
    // and finishing one needs no cancellation.
    use_future(move || async move {
        loop {
            if let Some(open) = session() {
                match api::photos_pick(&open.session_id).await {
                    Ok(result) => match result.state {
                        PhotoPickState::Pending => {}
                        PhotoPickState::Imported | PhotoPickState::Empty => {
                            session.set(None);
                            failure.set(None);
                            picked.set(Some(result));
                        }
                        // The download window lapsed mid-import. The server has
                        // already opened the replacement, so the wait continues
                        // against it — a retry button here would only 400.
                        PhotoPickState::Expired => {
                            session.set(result.session.clone());
                            picked.set(None);
                            failure.set(Some(
                                "That pick lapsed before the photos came back. A new one is open below; pick again."
                                    .to_owned(),
                            ));
                        }
                    },
                    Err(error) => {
                        session.set(None);
                        picked.set(None);
                        failure.set(Some(format!("Could not read the pick: {error}")));
                    }
                }
            }
            gloo_timers::future::TimeoutFuture::new(PHOTOS_POLL).await;
        }
    });

    let connected = matches!(status, Some(ConnectorStatus::Connected));
    let open = session();
    let receipt = picked();

    rsx! {
        section { class: "photo-picker", "data-testid": "photo-picker",
            if let Some(open) = open.as_ref() {
                div { class: "photo-pick-waiting",
                    a {
                        class: "button button-primary photo-pick-open",
                        "data-testid": "photos-picker-link",
                        href: "{open.picker_uri}",
                        target: "_blank",
                        rel: "noreferrer",
                        "Open Google Photos picker"
                    }
                    p { class: "photo-pick-hint", "aria-live": "polite",
                        "Pick the photos in that tab. This page fills in on its own when you are done."
                    }
                }
            } else if let Some(result) = receipt.as_ref() {
                div { class: "photo-pick-receipt",
                    p { class: "photo-pick-line", "data-testid": "photos-receipt",
                        "data-pick-state": "{pick_state_key(result.state)}",
                        "{receipt_line(result)}"
                    }
                    // The picks land as notes filed under the tag each photo was
                    // filed by (`connector-photos`'s tag rules), so this is the
                    // list of what actually arrived, ready to open.
                    if !result.tags.is_empty() {
                        ul { class: "photo-pick-items", "data-testid": "photos-picked-items",
                            for tag in result.tags.iter() {
                                li { class: "photo-pick-item", "data-testid": "photos-picked-item",
                                    Link {
                                        class: "photo-pick-tag",
                                        to: Route::TagDetail { tag_id: tag.id.to_string() },
                                        "{tag.name}"
                                    }
                                    span { class: "photo-pick-count", "data-thread-count": "{tag.thread_count}",
                                        "{tag.thread_count} notes"
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if let Some(message) = failure() {
                p { class: "connector-error", "data-testid": "photos-pick-error", "{message}" }
            }

            if !connected {
                // A pick needs a stored grant. Offering the button without one
                // would only earn a 400 from `create_session`, so the row sends
                // the user to the grant instead.
                p { class: "photo-pick-hint",
                    "A pick needs a Google Photos grant first — connect it here, then start a pick."
                }
                a {
                    class: "button photo-connect",
                    "data-testid": "photos-connect",
                    href: api::PHOTOS_CONNECT_PATH,
                    "Connect Google Photos"
                }
            } else {
                button {
                    class: "button photo-pick-start",
                    "data-testid": "photos-start-pick",
                    r#type: "button",
                    disabled: busy() || open.is_some(),
                    onclick: move |_| {
                        busy.set(true);
                        failure.set(None);
                        picked.set(None);
                        spawn(async move {
                            match api::photos_start_pick().await {
                                Ok(started) => {
                                    session.set(Some(started));
                                }
                                Err(error) => failure.set(Some(format!(
                                    "Could not start a pick: {error}"
                                ))),
                            }
                            busy.set(false);
                        });
                    },
                    if open.is_some() {
                        "Pick in progress…"
                    } else if receipt.is_some() || failure().is_some() {
                        "Start another pick"
                    } else {
                        "Start a pick"
                    }
                }
            }
        }
    }
}

/// The `data-pick-state` key for a finished pick, so a test reads the state the
/// server reported rather than parsing prose.
fn pick_state_key(state: PhotoPickState) -> &'static str {
    match state {
        PhotoPickState::Pending => "pending",
        PhotoPickState::Imported => "imported",
        PhotoPickState::Empty => "empty",
        PhotoPickState::Expired => "expired",
    }
}
