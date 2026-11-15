//! The route table (T6): the home grid, and a tag's detail view.
//!
//! Selecting a tag from the grid pushes `/tags/:tag_id`; the browser's own back action
//! (and dioxus-router's history) return to `/`. No server routing is involved — this is a
//! client-side push, so the bundle's static file server never needs to know these paths.

use dioxus::prelude::*;

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
