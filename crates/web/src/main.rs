//! Strands web SPA entrypoint — Dioxus 0.7, web-only build, no SSR (spec D1).

mod account;
mod api;
mod capture;
mod constellation;
mod data;
mod detail;
mod help;
mod home;
mod keys;
mod routes;
mod search;
mod settings;

use dioxus::prelude::*;

use home::Grid;
use routes::Route;

/// Styles ship inside the bundle: one artifact, no extra request, no external host. The
/// stylesheet stays a real file under `assets/` so it is still lintable and reviewable.
const STYLES: &str = include_str!("../assets/main.css");

fn main() {
    dioxus::launch(App);
}

/// App shell: document head, then the router. Selecting a tag from the grid pushes
/// `/tags/:id` (T6); the browser's back action returns to the grid.
///
/// The grid signal is provided here, above the router, so it survives route changes: a note
/// captured at `/capture` is already in the list when the user lands back on `/`.
#[component]
fn App() -> Element {
    let grid = use_signal(data::load_home);
    use_context_provider(|| Grid(grid));

    rsx! {
        document::Meta {
            name: "viewport",
            content: "width=device-width, initial-scale=1, viewport-fit=cover",
        }
        document::Meta { name: "color-scheme", content: "dark" }
        document::Title { "Strands" }
        document::Style { {STYLES} }
        Router::<Route> {}
    }
}
