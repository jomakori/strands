//! The top row every screen opens on: brand, search trigger, and the two tools.

use dioxus::prelude::*;
use strands_keymap::Action;

use crate::keys::Shortcuts;
use crate::routes::Route;

const LOGO: Asset = asset!("/assets/logo.svg");

/// Brand left, search pill centred, settings and shortcuts right.
#[component]
pub fn TopRow() -> Element {
    let shortcuts = use_context::<Shortcuts>();

    rsx! {
        header { class: "top-row",
            Link { class: "brand", to: Route::HomeView {}, "aria-label": "Strands home",
                img { class: "brand-mark", src: LOGO, width: "44", height: "44", alt: "" }
                h1 { class: "brand-word",
                    b { "Strands" }
                    span { class: "strand", "aria-hidden": "true",
                        svg { view_box: "0 0 100 6", preserve_aspect_ratio: "none",
                            path { d: "M0 3C10 .5 18 5.5 30 3S52 .5 64 3S88 5.5 100 3" }
                        }
                    }
                }
            }
            button {
                class: "search-trigger",
                r#type: "button",
                "aria-label": "Search tags and threads",
                onclick: move |_| shortcuts.raise(Action::OpenSearch),
                svg {
                    width: "16",
                    height: "16",
                    view_box: "0 0 24 24",
                    fill: "none",
                    stroke: "currentColor",
                    stroke_width: "1.9",
                    stroke_linecap: "round",
                    "aria-hidden": "true",
                    path { d: "m21 21-4.3-4.3M17 11a6 6 0 1 1-12 0 6 6 0 0 1 12 0" }
                }
                span { class: "lbl", "Search tags and threads" }
                kbd { "/" }
            }
            div { class: "top-tools",
                button {
                    class: "icon-btn",
                    r#type: "button",
                    "aria-label": "Settings",
                    title: "Settings (,)",
                    onclick: move |_| shortcuts.raise(Action::OpenSettings),
                    svg {
                        width: "20",
                        height: "20",
                        view_box: "0 0 24 24",
                        fill: "none",
                        stroke: "currentColor",
                        stroke_width: "1.7",
                        stroke_linecap: "round",
                        stroke_linejoin: "round",
                        "aria-hidden": "true",
                        path { d: "M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" }
                        circle { cx: "12", cy: "12", r: "3" }
                    }
                }
                button {
                    class: "icon-btn help-btn",
                    r#type: "button",
                    "aria-label": "Keyboard shortcuts",
                    title: "Keyboard shortcuts (?)",
                    onclick: move |_| shortcuts.raise(Action::OpenHelp),
                    svg {
                        width: "20",
                        height: "20",
                        view_box: "0 0 24 24",
                        fill: "none",
                        stroke: "currentColor",
                        stroke_width: "1.8",
                        stroke_linecap: "round",
                        stroke_linejoin: "round",
                        "aria-hidden": "true",
                        rect { x: "2.5", y: "6", width: "19", height: "12", rx: "3" }
                        path { d: "M6.5 10h.01M10 10h.01M13.5 10h.01M17 10h.01M7 14h10" }
                    }
                }
            }
        }
    }
}
