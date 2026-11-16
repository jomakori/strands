//! The route table (T6, T12): the home grid, a tag's detail view, and quick capture.
//!
//! Selecting a tag from the grid pushes `/tags/:tag_id`; the grid's primary action pushes
//! `/capture`. The browser's own back action (and dioxus-router's history) return to `/`. No
//! server routing is involved — these are client-side pushes, so the bundle's static file
//! server never needs to know these paths.

use dioxus::prelude::*;
use strands_keymap::Action;

use crate::account::Account;
use crate::capture::Capture;
use crate::detail::TagDetail;
use crate::help::ShortcutMap;
use crate::home::HomeView;
use crate::keys::{self, Shortcuts};
use crate::search::SearchSheet;
use crate::settings::Settings;

#[derive(Clone, Debug, PartialEq, Routable)]
pub enum Route {
    // Every route renders inside the one shell layout, which is what lets the keyboard
    // listener, the search sheet and the `?` map exist once for the whole app.
    #[layout(Shell)]
    #[route("/")]
    HomeView {},

    #[route("/tags/:tag_id")]
    TagDetail { tag_id: String },

    #[route("/capture")]
    Capture {},

    #[route("/settings")]
    Settings {},

    #[route("/account")]
    Account {},
}

/// App shell: the active route, plus the overlays and the one listener every route shares.
#[component]
pub fn Shell() -> Element {
    let shortcuts = use_context_provider(Shortcuts::new);
    let navigator = use_navigator();

    keys::use_listener(shortcuts);

    // The listener only raises an action; routing and sheet state change here, inside the
    // reactive runtime. Actions that belong to a screen are parked for that screen instead.
    use_effect(move || {
        let Some(action) = shortcuts.take_pending() else {
            return;
        };
        let mut state = shortcuts;
        match action {
            Action::OpenSearch => state.search_open.set(true),
            Action::CloseSearch => state.search_open.set(false),
            Action::OpenHelp => state.help_open.set(true),
            Action::CloseHelp => state.help_open.set(false),
            Action::OpenCapture => {
                navigator.push(Route::Capture {});
            }
            Action::OpenSettings => {
                navigator.push(Route::Settings {});
            }
            // Back replaces rather than pushes, so `Esc` out of a tag does not pile up
            // history entries the user then has to walk back through.
            Action::Back => {
                navigator.replace(Route::HomeView {});
            }
            Action::ApproveSummary
            | Action::DiscardSummary
            | Action::ToggleHistory
            | Action::ArchiveThread => state.park(action),
        }
    });

    rsx! {
        Outlet::<Route> {}
        SearchSheet {}
        ShortcutMap {}
    }
}
