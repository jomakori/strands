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
