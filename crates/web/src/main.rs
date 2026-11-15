//! Strands web SPA entrypoint — Dioxus 0.7, web-only build, no SSR (spec D1).

use dioxus::prelude::*;

fn main() {
    dioxus::launch(App);
}

/// Placeholder home route; a real router/typed client lands with T4.
#[component]
fn App() -> Element {
    rsx! {
        div { id: "home",
            h1 { "Strands" }
            p { "Coming soon." }
        }
    }
}
