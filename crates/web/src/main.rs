//! Strands web SPA entrypoint — Dioxus 0.7, web-only build, no SSR (spec D1).

mod api;
mod data;
mod detail;
mod home;
mod routes;
mod settings;

use dioxus::prelude::*;

use routes::Route;

/// Styles ship inside the bundle: one artifact, no extra request, no external host. The
/// stylesheet stays a real file under `assets/` so it is still lintable and reviewable.
const STYLES: &str = include_str!("../assets/main.css");

fn main() {
    dioxus::launch(App);
}

/// App shell: document head, then the router. Selecting a tag from the grid pushes
/// `/tags/:id` (T6); the browser's back action returns to the grid.
#[component]
fn App() -> Element {
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
