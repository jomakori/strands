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
