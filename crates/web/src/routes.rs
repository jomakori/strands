//! The route table (T6, T12): the home grid, a tag's detail view, and quick capture.
//!
//! Selecting a tag from the grid pushes `/tags/:tag_id`; the grid's primary action pushes
//! `/capture`. The browser's own back action (and dioxus-router's history) return to `/`. No
//! server routing is involved — these are client-side pushes, so the bundle's static file
//! server never needs to know these paths.

use dioxus::prelude::*;

use crate::capture::Capture;
use crate::detail::TagDetail;
use crate::home::HomeView;
use crate::search::SearchSheet;
use crate::settings::Settings;

#[derive(Clone, Debug, PartialEq, Routable)]
pub enum Route {
    // Every route renders inside the one shell layout, which is what lets the search
    // sheet and its `'/'` binding exist once for the whole app.
    #[layout(Shell)]
    #[route("/")]
    HomeView {},

    #[route("/tags/:tag_id")]
    TagDetail { tag_id: String },

    #[route("/capture")]
    Capture {},

    #[route("/settings")]
    Settings {},
}

/// App shell: the active route, plus the search overlay every route shares.
#[component]
pub fn Shell() -> Element {
    rsx! {
        Outlet::<Route> {}
        SearchSheet {}
    }
}
