//! Loading, failure and empty chrome (T18): every fetch-backed surface renders
//! one of these three shapes, so a failed request always offers a way to retry.
//!
//! The shapes are markup only — each view keeps its own state fields, because a
//! view knows what it is waiting for and what "empty" means for it.

use dioxus::prelude::*;

use crate::api::ApiError;

/// Skeleton lines standing in for a list that has not arrived yet.
#[component]
pub fn Skeleton(#[props(default = 3)] lines: usize, label: String) -> Element {
    rsx! {
        div { class: "skeleton", "data-state": "loading", role: "status",
            span { class: "sr-only", "{label}" }
            for index in 0..lines {
                span { key: "{index}", class: "skeleton-line", "aria-hidden": "true" }
            }
        }
    }
}

/// A failed fetch: what went wrong, and the control that asks again.
#[component]
pub fn RetryNotice(message: String, onretry: EventHandler<MouseEvent>) -> Element {
    rsx! {
        div { class: "load-error", "data-state": "error", role: "alert",
            p { class: "load-error-text", "{message}" }
            button {
                class: "button retry-button",
                r#type: "button",
                "data-retry": "true",
                onclick: move |event| onretry.call(event),
                "Try again"
            }
        }
    }
}

/// Why a WhatsApp surface is empty at zero chats (A3).
///
/// A fresh link reports no chats until the phone pushes its history, so the
/// honest answer is the reason — not a preview failure and not a receipt that
/// counts zeroes. Both the import picker and the Settings row read these, so the
/// two cannot drift apart.
pub const WHATSAPP_ZERO_CHAT_TITLE: &str = "WhatsApp history has not synced yet.";
pub const WHATSAPP_ZERO_CHAT_HINT: &str = "Chats appear here as the sync arrives; reopen this screen once the phone has sent its history.";
/// The same reason as one line, for a receipt that would otherwise read
/// `Imported 0 notes from 0 chats`.
pub const WHATSAPP_ZERO_CHAT_LINE: &str = "WhatsApp history has not synced yet, so there is nothing to import. Chats appear as the sync arrives.";
/// The row's chat count when the phone has reported no chats at all.
pub const WHATSAPP_ZERO_CHAT_META: &str =
    "No chats on the phone yet — history has not synced. Chats appear here as the sync arrives.";

/// Nothing to show, and why that is not a failure.
#[component]
pub fn EmptyNotice(title: String, hint: String) -> Element {
    rsx! {
        div { class: "empty-notice", "data-state": "empty",
            p { class: "empty-title", "{title}" }
            p { class: "sub", "{hint}" }
        }
    }
}

/// Short, user-facing text for a failed call.
pub fn describe(error: &ApiError) -> String {
    match error {
        ApiError::Network(_) => "network error".to_owned(),
        ApiError::Status(401) => "you are signed out".to_owned(),
        ApiError::Status(404) => "not found".to_owned(),
        ApiError::Status(status) => format!("server returned {status}"),
    }
}
