//! The session cookie (ticket T3, spec §6 "Auth flow": an `HttpOnly`,
//! `Secure`, `SameSite=Lax` cookie with a 30-day life).
//!
//! The cookie carries exactly one thing: our own `sessions.id`, an opaque v4
//! UUID. No provider access token, no refresh token, no email — nothing about
//! Google (or the user) is exposed to the browser, and revoking a session is a
//! single row delete.

use axum::http::HeaderMap;
use axum::http::header;
use chrono::{DateTime, Duration, Utc};
use strands_core::SessionId;
use uuid::Uuid;

/// Cookie name, also documented in the README.
pub const SESSION_COOKIE_NAME: &str = "strands_session";

/// RFC 7231 IMF-fixdate, the only format browsers accept for `Expires`.
const HTTP_DATE: &str = "%a, %d %b %Y %H:%M:%S GMT";
const EPOCH: &str = "Thu, 01 Jan 1970 00:00:00 GMT";

/// `Set-Cookie` value that installs a session.
///
/// `SameSite=Lax` (not `Strict`) is deliberate: the OAuth callback is a
/// top-level cross-site redirect from Google, and `Strict` would drop the
/// cookie on the request that follows it.
pub fn session_cookie(
    session_id: SessionId,
    ttl: Duration,
    secure: bool,
    now: DateTime<Utc>,
) -> String {
    let mut cookie = format!(
        "{SESSION_COOKIE_NAME}={session_id}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}; Expires={}",
        ttl.num_seconds().max(0),
        (now + ttl).format(HTTP_DATE),
    );
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// `Set-Cookie` value that deletes the cookie.
pub fn cleared_session_cookie(secure: bool) -> String {
    let mut cookie = format!(
        "{SESSION_COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0; Expires={EPOCH}"
    );
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// The session id from the request's `Cookie` headers, if it parses.
///
/// A malformed or absent cookie both come back as `None`; the caller answers
/// 401 either way, so a probe learns nothing about our id format.
pub fn session_id_from_headers(headers: &HeaderMap) -> Option<SessionId> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| name.trim() == SESSION_COOKIE_NAME)
        .and_then(|(_, value)| Uuid::parse_str(value.trim().trim_matches('"')).ok())
}

/// Cookie that carries the route the visitor asked for across the OIDC round trip (T24).
///
/// Scoped to `/auth` so the app's own requests never see it: it exists only between
/// {@link start} and {@link crate::auth::routes::callback}.
pub const AFTER_SIGN_IN_COOKIE_NAME: &str = "strands_after_sign_in";

/// How long the return path may wait for a human to get through the consent screen.
const AFTER_SIGN_IN_TTL: Duration = Duration::minutes(10);

/// A return path is a route in this app, not a document: anything longer is not ours.
const MAX_RETURN_PATH: usize = 512;

/// Characters a return path may contain once the client has encoded it: the URI `pchar` set
/// minus the ones that would break a cookie header. This is the whole anti-open-redirect
/// story — no scheme, no host, no backslash, no `//`, no control characters, no `;`.
fn return_path_char_allowed(character: char) -> bool {
    character.is_ascii_alphanumeric() || "-._~/%:@!$&'()*+=".contains(character)
}

/// The route to return to after sign-in, or `None` when the request did not ask for one.
///
/// A `next` value is attacker-controlled (it arrives in a link), so it is only honoured when it
/// is a root-relative path in this app. `//evil.example` and `/\\evil.example` are the classic
/// ways to turn one into a redirect somewhere else; both are refused here. The auth and API
/// surfaces are refused too, so `next` can never bounce the visitor back into the flow.
pub fn safe_return_path(raw: &str) -> Option<String> {
    let path = raw.trim();
    if path.is_empty() || path.len() > MAX_RETURN_PATH {
        return None;
    }
    if !path.starts_with('/') || path.starts_with("//") {
        return None;
    }
    if !path.chars().all(return_path_char_allowed) {
        return None;
    }
    if path == "/auth" || path.starts_with("/auth/") || path.starts_with("/api/") {
        return None;
    }
    Some(path.to_owned())
}

/// `Set-Cookie` value that remembers where the visitor was headed.
pub fn after_sign_in_cookie(path: &str, secure: bool) -> String {
    let mut cookie = format!(
        "{AFTER_SIGN_IN_COOKIE_NAME}={path}; Path=/auth; HttpOnly; SameSite=Lax; Max-Age={}",
        AFTER_SIGN_IN_TTL.num_seconds().max(0),
    );
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// `Set-Cookie` value that forgets it, sent alongside the session on the callback.
pub fn cleared_after_sign_in_cookie(secure: bool) -> String {
    let mut cookie = format!(
        "{AFTER_SIGN_IN_COOKIE_NAME}=; Path=/auth; HttpOnly; SameSite=Lax; Max-Age=0; Expires={EPOCH}"
    );
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// The remembered return path from the request's cookies, if it still passes validation.
///
/// Re-validated on the way out rather than trusted because it was validated on the way in: the
/// cookie is client-held, so it is as untrusted as the `next` it came from.
pub fn after_sign_in_path_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| name.trim() == AFTER_SIGN_IN_COOKIE_NAME)
        .and_then(|(_, value)| safe_return_path(value.trim().trim_matches('"')))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn the_cookie_is_httponly_secure_lax_and_carries_only_the_session_id() {
        let session_id = Uuid::nil();
        let cookie = session_cookie(session_id, Duration::days(30), true, fixed_now());

        assert!(cookie.starts_with(&format!("{SESSION_COOKIE_NAME}={session_id};")));
        assert!(cookie.contains("; HttpOnly"));
        assert!(cookie.contains("; Secure"));
        assert!(cookie.contains("; SameSite=Lax"));
        assert!(cookie.contains("; Path=/"));
        assert!(cookie.contains("; Max-Age=2592000"));
        // 2026-01-01 + 30 days, in the only date format browsers parse.
        assert!(cookie.contains("; Expires=Sat, 31 Jan 2026 00:00:00 GMT"));
        // Nothing but our own id: no token, no email, no provider name.
        assert!(!cookie.to_ascii_lowercase().contains("google"));
    }

    #[test]
    fn secure_can_be_dropped_for_a_local_http_run() {
        let cookie = session_cookie(Uuid::nil(), Duration::days(1), false, fixed_now());
        assert!(!cookie.contains("; Secure"));
        assert!(cookie.contains("; HttpOnly"));
    }

    #[test]
    fn clearing_expires_the_cookie_immediately() {
        let cookie = cleared_session_cookie(true);
        assert!(cookie.starts_with(&format!("{SESSION_COOKIE_NAME}=;")));
        assert!(cookie.contains("Max-Age=0"));
        assert!(cookie.contains("Expires=Thu, 01 Jan 1970 00:00:00 GMT"));
    }

    #[test]
    fn a_local_deep_link_is_a_valid_return_path() {
        assert_eq!(
            safe_return_path("/tags/6f1c1f5e-0000-4000-8000-000000000001").as_deref(),
            Some("/tags/6f1c1f5e-0000-4000-8000-000000000001")
        );
        assert_eq!(safe_return_path("/").as_deref(), Some("/"));
        assert_eq!(
            safe_return_path("/search%3Fq%3Dramen").as_deref(),
            Some("/search%3Fq%3Dramen")
        );
    }

    #[test]
    fn a_return_path_can_never_leave_the_app() {
        for hostile in [
            "https://evil.example/",
            "//evil.example/",
            "/\\evil.example",
            "/\t/evil.example",
            "tags/1",
            "",
            "   ",
            "/auth/google",
            "/api/v1/me",
        ] {
            assert_eq!(safe_return_path(hostile), None, "accepted {hostile:?}");
        }
    }

    #[test]
    fn a_return_path_is_a_route_not_a_document() {
        let long = format!("/{}", "a".repeat(MAX_RETURN_PATH));
        assert_eq!(safe_return_path(&long), None);
    }

    #[test]
    fn the_return_path_survives_a_round_trip_through_cookies() {
        let cookie = after_sign_in_cookie("/tags/abc", true);
        assert!(cookie.starts_with(&format!("{AFTER_SIGN_IN_COOKIE_NAME}=/tags/abc;")));
        assert!(cookie.contains("; Path=/auth;"));
        assert!(cookie.contains("; HttpOnly"));
        assert!(cookie.contains("; SameSite=Lax"));
        assert!(cookie.contains("; Max-Age=600"));
        assert!(cookie.contains("; Secure"));

        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("theme=dark; {cookie}")).unwrap(),
        );
        assert_eq!(
            after_sign_in_path_from_headers(&headers).as_deref(),
            Some("/tags/abc")
        );
    }

    #[test]
    fn a_tampered_return_cookie_is_ignored() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("{AFTER_SIGN_IN_COOKIE_NAME}=//evil.example")).unwrap(),
        );
        assert_eq!(after_sign_in_path_from_headers(&headers), None);
        assert_eq!(after_sign_in_path_from_headers(&HeaderMap::new()), None);
    }

    #[test]
    fn clearing_the_return_cookie_expires_it() {
        let cookie = cleared_after_sign_in_cookie(false);
        assert!(cookie.starts_with(&format!("{AFTER_SIGN_IN_COOKIE_NAME}=;")));
        assert!(cookie.contains("; Path=/auth;"));
        assert!(cookie.contains("Max-Age=0"));
        assert!(!cookie.contains("; Secure"));
    }

    #[test]
    fn the_session_id_is_read_out_of_a_crowded_cookie_header() {
        let mut headers = HeaderMap::new();
        let id = Uuid::new_v4();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("theme=dark; {SESSION_COOKIE_NAME}={id}; other=1"))
                .unwrap(),
        );

        assert_eq!(session_id_from_headers(&headers), Some(id));
    }

    #[test]
    fn missing_or_garbage_cookies_yield_nothing() {
        assert_eq!(session_id_from_headers(&HeaderMap::new()), None);

        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("{SESSION_COOKIE_NAME}=not-a-uuid")).unwrap(),
        );
        assert_eq!(session_id_from_headers(&headers), None);
    }
}
