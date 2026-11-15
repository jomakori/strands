//! Strands web SPA entrypoint — Dioxus 0.7, web-only build, no SSR (spec D1).

mod data;
mod home;

use dioxus::prelude::*;

/// Styles ship inside the bundle: one artifact, no extra request, no external host. The
/// stylesheet stays a real file under `assets/` so it is still lintable and reviewable.
const STYLES: &str = include_str!("../assets/main.css");

fn main() {
    dioxus::launch(App);
}

/// App shell: document head, then the home surface. A router lands with the detail view
/// (T6); the home grid is the whole surface for now.
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
        home::HomeView {}
    }
}
