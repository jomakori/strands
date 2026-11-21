//! The sign-in screen the gate puts in front of the app (T24).
//!
//! Everything the app proper shows — the grid, the chips, the counts, the settings and account
//! links — lives below the router, so swapping the router out for this screen removes all of it.
//! There is exactly one control here, and it leaves the app for the server's OIDC start.

use dioxus::prelude::*;

use crate::session;

/// The signed-out screen: product name, one line of what it is for, one way in.
///
/// `next` is the route the visitor was trying to reach, so signing in returns them there
/// instead of dumping everybody on the grid.
#[component]
pub fn SignIn(next: String) -> Element {
    let href = session::sign_in_href(&next);
    rsx! {
        main { class: "gate", id: "gate",
            div { class: "gate-card",
                h1 { class: "gate-title", "Strands" }
                p { class: "gate-line", "A note for every tag." }
                a { class: "gate-cta", id: "sign-in", href: "{href}", "Sign in with Google" }
            }
        }
    }
}

/// The probe is still in flight: brand only.
///
/// This is not decoration. Rendering the gate first and swapping it out when `/api/v1/me`
/// answers would make every signed-in reload flash a sign-in screen, and rendering the app
/// first would leak the shell to visitors who have no session. Neither is acceptable, so the
/// only thing on screen while we ask is the wordmark.
#[component]
pub fn Probing() -> Element {
    rsx! {
        div { class: "gate gate-probing", id: "gate-probing",
            span { class: "gate-title", "Strands" }
        }
    }
}
