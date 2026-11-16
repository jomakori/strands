//! The `?` map (T19): the keyboard reference, rendered from the table the resolver uses.
//!
//! Every row comes from `strands_keymap::BINDINGS`, so the sheet cannot drift from the
//! keys that actually fire. It is an overlay rather than a route: the map is a reference,
//! and closing it must not cost the user the screen they were on.

use dioxus::prelude::*;
use strands_keymap::{BINDINGS, Section};

use crate::keys::{self, Shortcuts};

/// The shortcut map; renders nothing at all while closed.
#[component]
pub fn ShortcutMap() -> Element {
    let mut shortcuts = use_context::<Shortcuts>();

    // Opening the map lands focus on its close button, so Tab starts inside the sheet and
    // walks the list; nothing traps the focus, so Tab on past the end leaves normally.
    use_effect(move || {
        if (shortcuts.help_open)() {
            keys::focus("#shortcut-close");
        }
    });

    if !(shortcuts.help_open)() {
        return rsx! {};
    }

    rsx! {
        div {
            id: "shortcut-map",
            class: "sheet help",
            role: "dialog",
            "aria-modal": "true",
            "aria-label": "Keyboard shortcuts",
            div { class: "sheet__panel help__panel",
                div { class: "help__head",
                    h2 { class: "sheet__heading", "Keyboard" }
                    button {
                        id: "shortcut-close",
                        class: "help__close",
                        r#type: "button",
                        onclick: move |_| shortcuts.help_open.set(false),
                        "Close"
                    }
                }
                for section in [
                    Section::Moving,
                    Section::Search,
                    Section::Review,
                    Section::Capture,
                    Section::Account,
                ] {
                    h3 { key: "{section.heading()}", class: "help__section", "{section.heading()}" }
                    dl { class: "help__list",
                        for binding in BINDINGS.iter().filter(|binding| binding.section == section) {
                            div { key: "{binding.keys}-{binding.label}", class: "help__row",
                                dt { kbd { "{binding.keys}" } }
                                dd { "{binding.label}" }
                                dd { class: "help__touch", "{binding.touch}" }
                            }
                        }
                    }
                }
                p { class: "help__hint", "Esc closes this map." }
            }
        }
    }
}
