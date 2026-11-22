//! Keyboard glue (T19): the app's one `keydown` listener and the state it feeds.
//!
//! `strands-keymap` decides what a press *means*; this module only translates a DOM event
//! into its [`KeyPress`], holds the shell's keyboard state, and keeps the listener single.
//! Screens read [`Shortcuts`] instead of installing handlers of their own, so the typing
//! guard and the modifier rules exist once, in one place.

use dioxus::prelude::*;
use strands_keymap::{Action, Key, KeyPress, Overlay, Screen, resolve};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{HtmlElement, KeyboardEvent};

/// Keyboard state the shell owns: the listener fills `pending`, the shell routes it, and
/// the mounted screen drains what is left in `screen_action`.
#[derive(Clone, Copy)]
pub struct Shortcuts {
    pending: Signal<Option<Action>>,
    screen_action: Signal<Option<Action>>,
    /// The search sheet is up.
    pub search_open: Signal<bool>,
    /// The `?` map is up.
    pub help_open: Signal<bool>,
    /// The new-strand sheet is up.
    pub compose_open: Signal<bool>,
}

impl Shortcuts {
    /// Fresh state, one per shell mount.
    pub fn new() -> Shortcuts {
        Shortcuts {
            pending: Signal::new(None),
            screen_action: Signal::new(None),
            search_open: Signal::new(false),
            help_open: Signal::new(false),
            compose_open: Signal::new(false),
        }
    }

    /// Claim the action the listener raised, so it is handled exactly once.
    pub fn take_pending(self) -> Option<Action> {
        let action = (self.pending)()?;
        let mut state = self;
        state.pending.set(None);
        Some(action)
    }

    /// Raise an action from a pointer, through the same path the keys use.
    pub fn raise(self, action: Action) {
        let mut state = self;
        state.pending.set(Some(action));
    }

    /// Hand an action to the mounted screen; it decides whether the action is its own.
    pub fn park(self, action: Action) {
        let mut state = self;
        state.screen_action.set(Some(action));
    }

    /// Claim the action aimed at the mounted screen, if it is one of `mine`. Clearing it
    /// here is what keeps two screens from both reacting to the same press.
    pub fn take_screen_action(self, mine: &[Action]) -> Option<Action> {
        let action = (self.screen_action)()?;
        if !mine.contains(&action) {
            return None;
        }
        let mut state = self;
        state.screen_action.set(None);
        Some(action)
    }
}

/// Install the app's only key listener; called once, from the shell.
pub fn use_listener(shortcuts: Shortcuts) {
    use_hook(move || {
        let listener = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
            if let Some(action) = translate(&event, shortcuts) {
                // Claimed keys do not also reach the browser: no quick-find on `'/'`, no
                // scroll on `Tab`, no back-navigation on `Esc`.
                event.prevent_default();
                let mut state = shortcuts;
                state.pending.set(Some(action));
            }
        });

        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            let _ = document
                .add_event_listener_with_callback("keydown", listener.as_ref().unchecked_ref());
        }
        // The shell outlives every route, so the listener is kept for the page's lifetime
        // on purpose; dropping it would leave a freed callback registered.
        listener.forget();
    });
}

/// The action this press asks for, given the screen behind it and any open sheet.
fn translate(event: &KeyboardEvent, shortcuts: Shortcuts) -> Option<Action> {
    let press = KeyPress {
        key: Key::from_dom(&event.key())?,
        shift: event.shift_key(),
        ctrl: event.ctrl_key(),
        meta: event.meta_key(),
        alt: event.alt_key(),
        typing: is_typing(event),
    };
    // An open sheet owns the keys; the map is the outermost surface, so it wins.
    let overlay = if (shortcuts.help_open)() {
        Some(Overlay::Help)
    } else if (shortcuts.search_open)() {
        Some(Overlay::Search)
    } else if (shortcuts.compose_open)() {
        Some(Overlay::Compose)
    } else {
        None
    };
    resolve(press, screen(), overlay)
}

/// True when the key event came from a field that already consumes typing.
fn is_typing(event: &KeyboardEvent) -> bool {
    event
        .target()
        .and_then(|target| target.dyn_into::<HtmlElement>().ok())
        .is_some_and(|element| {
            matches!(element.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT")
                || element.is_content_editable()
        })
}

/// The screen the browser is showing; unreadable paths count as the grid.
fn screen() -> Screen {
    web_sys::window()
        .and_then(|window| window.location().pathname().ok())
        .map(|path| Screen::from_path(&path))
        .unwrap_or(Screen::Home)
}

/// Move focus to `selector`; a no-op when the element is not on the page yet.
pub fn focus(selector: &str) {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    if let Ok(Some(element)) = document.query_selector(selector)
        && let Some(element) = element.dyn_ref::<HtmlElement>()
    {
        let _ = element.focus();
    }
}
