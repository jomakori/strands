//! Who the visitor is, and the gate that keeps the app for signed-in visitors (T24).
//!
//! The browser is told nothing about the session except the cookie it already holds, so the
//! only trustworthy answer comes from the API: `GET /api/v1/me` is 200 for a valid session and
//! 401 otherwise. Nothing here reads `id_token`, and nothing here treats "some cookie exists"
//! as a session — a forged or stale cookie must still land on the gate.
//!
//! Two things are deliberately global rather than threaded through every component:
//!
//! * the probe, which runs once above the router, and
//! * the refusal hook, so a 401 from any endpoint returns the visitor to the gate instead of
//!   painting an error panel inside a screen that no longer has a session behind it.

use std::cell::RefCell;

use dioxus::prelude::*;
use strands_api_types::Profile;

/// Where the sign-in control sends the browser; the server starts the OIDC flow there.
pub const SIGN_IN_PATH: &str = "/auth/google";

/// Resolved identity of the visitor. `Probing` is a real state, not a placeholder: while the
/// answer is in flight neither the gate nor the app may render, or a signed-in reload would
/// flash the gate before the grid appeared.
#[derive(Clone, Debug, PartialEq)]
pub enum Session {
    Probing,
    SignedOut,
    SignedIn(Profile),
}

thread_local! {
    /// The one session signal the app owns, set once by [`install`]. A `thread_local` rather
    /// than a `static` because the bundle is single-threaded wasm and the signal is `Copy`.
    static CURRENT: RefCell<Option<Signal<Session>>> = const { RefCell::new(None) };
}

/// Point the refusal hook at the app's session signal. Called once, above the router.
pub fn install(session: Signal<Session>) {
    CURRENT.with(|slot| *slot.borrow_mut() = Some(session));
}

/// The server refused a call as unauthenticated: the gate takes over.
///
/// Called from [`crate::api`] on any 401, which is why it is tolerant of being called before
/// [`install`] (a probe that runs first) or twice in a row (two parallel 401s).
pub fn refused() {
    let Some(mut signal) = CURRENT.with(|slot| *slot.borrow()) else {
        return;
    };
    // Read and release before writing: the guard must not outlive this statement.
    let signed_in = signal.peek().is_signed_in();
    if signed_in {
        signal.set(Session::SignedOut);
    }
}

impl Session {
    /// The visitor is known to hold a valid session.
    pub fn is_signed_in(&self) -> bool {
        matches!(self, Self::SignedIn(_))
    }
}

/// The sign-in URL for the route the visitor asked for, so a deep link survives the round trip
/// through the provider. The root carries no `next` — the server defaults there.
pub fn sign_in_href(path: &str) -> String {
    match path {
        "" | "/" => SIGN_IN_PATH.to_owned(),
        deep => format!("{SIGN_IN_PATH}?next={}", encode_path(deep)),
    }
}

/// Percent-encode everything outside the unreserved set plus `/`, so the path survives a query
/// string unchanged. The server refuses anything that does not decode back to a local path.
fn encode_path(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                encoded.push(char::from(byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}
