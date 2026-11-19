//! Strands web SPA entrypoint — Dioxus 0.7, web-only build, no SSR (spec D1).

mod account;
mod api;
mod capture;
mod constellation;
mod data;
mod detail;
mod focus_trap;
mod help;
mod home;
mod keys;
mod routes;
mod search;
mod session;
mod settings;
mod signin;
mod state;

use dioxus::prelude::*;

use home::Grid;
use routes::Route;
use session::Session;

/// Styles ship inside the bundle: one artifact, no extra request, no external host. The
/// stylesheet stays a real file under `assets/` so it is still lintable and reviewable.
const STYLES: &str = include_str!("../assets/main.css");

fn main() {
    dioxus::launch(App);
}

/// App shell: document head, then the gate or the router.
///
/// The visitor's identity is the first question, and it is asked of the API, not of the
/// document: `GET /api/v1/me` is the only thing that decides whether the app exists for this
/// browser (T24). Until it answers, neither side renders — see [`signin::Probing`].
///
/// The grid signal is provided here, above the gate, so it survives route changes: a note
/// captured at `/capture` is already in the list when the user lands back on `/`. It is also
/// why the gate cannot leak grid content — the screen, not the state, is what gets replaced.
#[component]
fn App() -> Element {
    let grid = use_signal(data::load_home);
    use_context_provider(|| Grid(grid));

    let mut session = use_signal(|| Session::Probing);
    // `api` reports a refused call through this handle, which is how an in-app 401 gets the
    // visitor back to the gate without every screen having to know about sign-in.
    use_hook(move || session::install(session));
    use_future(move || async move {
        let resolved = match api::me().await {
            Ok(profile) => Session::SignedIn(profile),
            // Any failure keeps the app shut; a gate we cannot verify is a gate we do not open.
            Err(_) => Session::SignedOut,
        };
        session.set(resolved);
    });

    let current = session();
    rsx! {
        document::Meta {
            name: "viewport",
            content: "width=device-width, initial-scale=1, viewport-fit=cover",
        }
        document::Meta { name: "color-scheme", content: "dark" }
        document::Title { "Strands" }
        document::Style { {STYLES} }
        {match current {
            Session::Probing => rsx! { signin::Probing {} },
            Session::SignedOut => rsx! { signin::SignIn { next: current_path() } },
            Session::SignedIn(_) => rsx! { Router::<Route> {} },
        }}
    }
}

/// The path the browser is on, so the gate can carry a deep link through sign-in and drop the
/// visitor back on it afterwards. Read from the document rather than the router because the
/// gate renders above it — the router is exactly what is not mounted yet.
fn current_path() -> String {
    web_sys::window()
        .and_then(|window| window.location().pathname().ok())
        .unwrap_or_else(|| "/".to_owned())
}
