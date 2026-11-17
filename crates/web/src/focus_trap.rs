//! Modal focus containment (T18): an overlay takes focus when it opens, Tab
//! cycles inside it while it is open, and closing hands focus back to the
//! control that opened it.
//!
//! One hook, driven by the overlay's own open flag — the search sheet and the
//! shortcut map are always mounted, so containment follows the flag rather than
//! the component's lifetime.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{Document, Element, HtmlElement, KeyboardEvent};

/// What a Tab press can land on, in document order.
const FOCUSABLE: &str = "a[href], button:not([disabled]), input:not([disabled]), \
                         select:not([disabled]), textarea:not([disabled]), \
                         [tabindex]:not([tabindex='-1'])";

/// The listener and focus move this overlay installed, kept so closing can undo both.
struct Trap {
    document: Document,
    listener: Closure<dyn FnMut(KeyboardEvent)>,
    trigger: Option<Element>,
}

/// Contain focus inside `container_id` while `active` is true.
///
/// The flag is a signal, not a bool: the effect has to read it *inside* the closure,
/// otherwise the closure captures one value at mount and never re-runs when the
/// overlay opens — the trap installs never and the overlay opens unfocused.
pub fn use_focus_trap(active: Signal<bool>, container_id: &'static str) {
    let slot = use_hook(|| Rc::new(RefCell::new(None::<Trap>)));

    use_effect(move || {
        let open = *active.read();
        let mut held = slot.borrow_mut();
        match (open, held.is_some()) {
            (true, false) => *held = install(container_id),
            (false, true) => release(held.as_mut()),
            _ => {}
        }
    });
}

/// Move focus into the container and start cycling Tab inside it.
fn install(container_id: &str) -> Option<Trap> {
    let document = web_sys::window()?.document()?;
    let container = document.get_element_by_id(container_id)?;
    let trigger = document.active_element();

    focus_first(&container);

    let listener = {
        let document = document.clone();
        let container = container.clone();
        Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
            if event.key() != "Tab" {
                return;
            }
            cycle(&document, &container, &event);
        })
    };

    document
        .add_event_listener_with_callback("keydown", listener.as_ref().unchecked_ref())
        .ok()?;

    Some(Trap {
        document,
        listener,
        trigger,
    })
}

/// Stop cycling and put focus back where the overlay took it from.
fn release(held: Option<&mut Trap>) {
    let Some(trap) = held else {
        return;
    };
    let _ = trap
        .document
        .remove_event_listener_with_callback("keydown", trap.listener.as_ref().unchecked_ref());
    if let Some(element) = trap
        .trigger
        .as_ref()
        .and_then(|it| it.dyn_ref::<HtmlElement>())
    {
        let _ = element.focus();
    }
}

/// Wrap the tab order at either end, and pull focus back in if it escaped.
fn cycle(document: &Document, container: &Element, event: &KeyboardEvent) {
    let focusable = focusable(container);
    let (Some(first), Some(last)) = (focusable.first(), focusable.last()) else {
        event.prevent_default();
        return;
    };

    let active = document.active_element();
    let inside = active
        .as_ref()
        .is_some_and(|element| container.contains(Some(element)));
    let forward_wrap = !event.shift_key() && active.as_ref() == Some(last);
    let backward_wrap = event.shift_key() && active.as_ref() == Some(first);

    if !inside || forward_wrap {
        event.prevent_default();
        focus(first);
    } else if backward_wrap {
        event.prevent_default();
        focus(last);
    }
}

/// The visible focusable descendants of `container`.
fn focusable(container: &Element) -> Vec<Element> {
    let Ok(nodes) = container.query_selector_all(FOCUSABLE) else {
        return Vec::new();
    };
    (0..nodes.length())
        .filter_map(|index| nodes.get(index))
        .filter_map(|node| node.dyn_into::<Element>().ok())
        .filter(|element| {
            element
                .dyn_ref::<HtmlElement>()
                .is_some_and(|html| html.offset_parent().is_some())
        })
        .collect()
}

/// Focus the container's first control, falling back to the container itself.
fn focus_first(container: &Element) {
    match focusable(container).first() {
        Some(element) => focus(element),
        None => focus(container),
    }
}

/// Move focus to `element`; a detached node is ignored.
fn focus(element: &Element) {
    if let Some(element) = element.dyn_ref::<HtmlElement>() {
        let _ = element.focus();
    }
}
