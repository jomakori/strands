//! Cross-user isolation acceptance test (ticket T3).
//!
//! Spec acceptance, verbatim:
//!
//! 1. logging in with two Google accounts yields two isolated datasets;
//! 2. a request without the cookie gets 401;
//! 3. direct object access across users gets 404.
//!
//! The whole login flow runs here for real — state, PKCE, the code exchange,
//! the `users` upsert, the `sessions` row, the `Set-Cookie` header — with a fake
//! [`IdentityProvider`] in place of Google's network calls. That is the point of
//! the provider trait: the security-relevant half of OAuth is ours, and it is
//! exercised, while the half we do not own (Google's consent screen) is stubbed.
//!
//! Needs a live, migrated Postgres (`DATABASE_URL`); see `tests/common/mod.rs`
//! for the skip rules. CI's `schema` job provides a postgres:17 service.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use strands_core::UserId;
use strands_server::app::{AppState, router};
use strands_server::auth::identity::{BoxFuture, ConsentRequest};
use strands_server::auth::{AuthError, Identity, IdentityProvider};
use strands_server::config::AuthConfig;
use tower::ServiceExt;
use uuid::Uuid;

mod common;

/// Stands in for Google. Each test run gets fresh users (`run` is mixed into the
/// subject and email) so repeated local runs never collide on `users.email`.
struct FakeIdentityProvider {
    run: String,
}

impl FakeIdentityProvider {
    fn person(&self, code: &str) -> Option<(&'static str, String, &'static str)> {
        match code {
            "alice" => Some(("alice", format!("alice+{}@example.com", self.run), "Alice")),
            "bob" => Some(("bob", format!("bob+{}@example.com", self.run), "Bob")),
            _ => None,
        }
    }
}

impl IdentityProvider for FakeIdentityProvider {
    fn consent_url(&self, request: &ConsentRequest<'_>) -> String {
        format!(
            "https://accounts.example/consent?state={}&code_challenge={}&redirect_uri={}",
            request.state, request.code_challenge, request.redirect_uri
        )
    }

    fn exchange_code<'a>(
        &'a self,
        code: &'a str,
        code_verifier: &'a str,
        _redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<Identity, AuthError>> {
        Box::pin(async move {
            // The handler must have replayed the stored verifier; an empty one
            // means `oauth_requests` was not wired in.
            assert!(
                !code_verifier.is_empty(),
                "no PKCE verifier reached the exchange"
            );

            let (subject, email, name) = self.person(code).ok_or(AuthError::InvalidCode)?;
            Ok(Identity {
                subject: format!("sub-{subject}-{}", self.run),
                email,
                name: name.to_owned(),
                avatar_url: None,
            })
        })
    }
}

struct Harness {
    app: Router,
    pool: PgPool,
    run: String,
}

impl Harness {
    /// `None` when there is no usable database — the caller skips, loudly.
    /// See `tests/common/mod.rs` for the rule.
    async fn connect(test_name: &str) -> Option<Self> {
        let pool = common::connect(test_name).await?;

        let config = AuthConfig {
            client_id: "test-client".into(),
            client_secret: "test-secret".into(),
            public_base_url: "https://strands.test".into(),
            cookie_secure: true,
            session_ttl_days: 30,
            authorization_endpoint: "https://accounts.example/consent".into(),
            token_endpoint: "https://accounts.example/token".into(),
        };

        let run = Uuid::new_v4().simple().to_string();
        let app = router(AppState::new(
            pool.clone(),
            config,
            Arc::new(FakeIdentityProvider { run: run.clone() }),
        ));

        Some(Self { app, pool, run })
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, headers, body)
    }

    async fn get(&self, uri: &str, cookie: Option<&str>) -> (StatusCode, HeaderMap, Value) {
        let mut builder = Request::get(uri);
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        self.send(builder.body(Body::empty()).unwrap()).await
    }

    async fn post(&self, uri: &str, cookie: Option<&str>) -> (StatusCode, HeaderMap, Value) {
        let mut builder = Request::post(uri);
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        self.send(builder.body(Body::empty()).unwrap()).await
    }

    /// Walk the real flow: consent redirect → callback → session cookie.
    async fn login(&self, code: &str) -> String {
        let (status, headers, _) = self.get("/auth/google", None).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "consent redirect");

        let location = headers
            .get(header::LOCATION)
            .expect("consent redirect has a Location")
            .to_str()
            .unwrap()
            .to_owned();
        let state = query_param(&location, "state").expect("consent URL carries state");
        assert!(
            query_param(&location, "code_challenge").is_some(),
            "consent URL carries a PKCE challenge"
        );

        let (status, headers, _) = self
            .get(
                &format!("/auth/google/callback?code={code}&state={state}"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::SEE_OTHER, "callback for {code}");

        let set_cookie = headers
            .get(header::SET_COOKIE)
            .expect("callback sets a session cookie")
            .to_str()
            .unwrap()
            .to_owned();

        // Cookie contract from spec §6, checked on the real header.
        assert!(set_cookie.starts_with("strands_session="), "{set_cookie}");
        assert!(set_cookie.contains("; HttpOnly"), "{set_cookie}");
        assert!(set_cookie.contains("; SameSite=Lax"), "{set_cookie}");
        assert!(set_cookie.contains("; Secure"), "{set_cookie}");
        assert!(set_cookie.contains("; Path=/"), "{set_cookie}");
        assert!(
            !set_cookie.to_ascii_lowercase().contains("token"),
            "the cookie must not carry a provider token: {set_cookie}"
        );

        set_cookie
            .split(';')
            .next()
            .expect("cookie pair before the first attribute")
            .to_owned()
    }

    async fn user_id(&self, cookie: &str) -> UserId {
        let (status, _, body) = self.get("/api/v1/me", Some(cookie)).await;
        assert_eq!(status, StatusCode::OK, "GET /api/v1/me body: {body}");
        body["id"]
            .as_str()
            .expect("me carries an id")
            .parse()
            .unwrap()
    }

    /// Insert a tag directly: T3 has no create endpoint, and the isolation rule
    /// is about reads, not about how the row got there. The category comes from
    /// the five the login flow seeded.
    async fn create_tag(&self, user_id: UserId, name: &str) -> Uuid {
        let category_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM categories WHERE user_id = $1 ORDER BY position LIMIT 1",
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .expect("login seeded the default categories");

        let tag_id = Uuid::new_v4();
        sqlx::query("INSERT INTO tags (id, user_id, name, category_id) VALUES ($1, $2, $3, $4)")
            .bind(tag_id)
            .bind(user_id)
            .bind(name)
            .bind(category_id)
            .execute(&self.pool)
            .await
            .expect("insert tag");

        tag_id
    }

    /// Insert a session row that is already past its expiry.
    async fn create_expired_session(&self, user_id: UserId) -> String {
        let session_id = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions (id, user_id, expires_at) VALUES ($1, $2, now() - interval '1 second')")
            .bind(session_id)
            .bind(user_id)
            .execute(&self.pool)
            .await
            .expect("insert expired session");

        format!("strands_session={session_id}")
    }
}

fn query_param(url: &str, name: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.to_owned())
}

/// Acceptance rule 1: two Google accounts, two isolated datasets.
#[tokio::test]
async fn two_google_accounts_get_two_isolated_datasets() {
    let Some(harness) = Harness::connect("two_google_accounts_get_two_isolated_datasets").await
    else {
        return;
    };

    let alice_cookie = harness.login("alice").await;
    let bob_cookie = harness.login("bob").await;
    assert_ne!(alice_cookie, bob_cookie, "each login is its own session");

    let alice_id = harness.user_id(&alice_cookie).await;
    let bob_id = harness.user_id(&bob_cookie).await;
    assert_ne!(alice_id, bob_id, "two accounts, two users");

    let alice_tag = harness.create_tag(alice_id, "alice-only").await;
    let bob_tag = harness.create_tag(bob_id, "bob-only").await;

    // Each user sees exactly their own tag.
    let (status, _, body) = harness
        .get(&format!("/api/v1/tags/{alice_tag}"), Some(&alice_cookie))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "alice-only");

    let (status, _, body) = harness
        .get(&format!("/api/v1/tags/{bob_tag}"), Some(&bob_cookie))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "bob-only");

    // The datasets do not overlap: the same query, scoped per user, returns one row each.
    let alice_count: i64 = sqlx::query_scalar("SELECT count(*) FROM tags WHERE user_id = $1")
        .bind(alice_id)
        .fetch_one(&harness.pool)
        .await
        .unwrap();
    let bob_count: i64 = sqlx::query_scalar("SELECT count(*) FROM tags WHERE user_id = $1")
        .bind(bob_id)
        .fetch_one(&harness.pool)
        .await
        .unwrap();
    assert_eq!(alice_count, 1, "alice's dataset holds only her tag");
    assert_eq!(bob_count, 1, "bob's dataset holds only his tag");

    // Logging back in returns the same user, not a duplicate one, and does not
    // seed a second set of categories.
    let alice_again = harness.login("alice").await;
    assert_eq!(harness.user_id(&alice_again).await, alice_id);

    let alice_categories: i64 =
        sqlx::query_scalar("SELECT count(*) FROM categories WHERE user_id = $1")
            .bind(alice_id)
            .fetch_one(&harness.pool)
            .await
            .unwrap();
    assert_eq!(
        alice_categories, 5,
        "spec §6: five categories on first login"
    );
}

/// Acceptance rule 2: no cookie, no entry.
#[tokio::test]
async fn requests_without_the_session_cookie_are_rejected_with_401() {
    let Some(harness) =
        Harness::connect("requests_without_the_session_cookie_are_rejected_with_401").await
    else {
        return;
    };

    let cookie = harness.login("alice").await;
    let alice_id = harness.user_id(&cookie).await;
    let tag = harness.create_tag(alice_id, "private").await;
    let tag_uri = format!("/api/v1/tags/{tag}");

    for uri in ["/api/v1/me", tag_uri.as_str()] {
        let (status, _, body) = harness.get(uri, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} without a cookie");
        assert_eq!(body["error"], "authentication required", "{uri}");

        // A forged or stale id is no better than no cookie at all.
        let forged = format!("strands_session={}", Uuid::new_v4());
        let (status, _, _) = harness.get(uri, Some(&forged)).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{uri} with an unknown session"
        );

        let garbage = "strands_session=not-a-uuid";
        let (status, _, _) = harness.get(uri, Some(garbage)).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{uri} with a malformed cookie"
        );
    }

    // An expired row is indistinguishable from no row.
    let expired = harness.create_expired_session(alice_id).await;
    let (status, _, _) = harness.get("/api/v1/me", Some(&expired)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "expired session");
}

/// Acceptance rule 3: another user's object id is a 404, and the 404 says nothing.
#[tokio::test]
async fn direct_object_access_across_users_gets_404() {
    let Some(harness) = Harness::connect("direct_object_access_across_users_gets_404").await else {
        return;
    };

    let alice_cookie = harness.login("alice").await;
    let bob_cookie = harness.login("bob").await;
    let alice_id = harness.user_id(&alice_cookie).await;
    let bob_id = harness.user_id(&bob_cookie).await;

    let alice_tag = harness.create_tag(alice_id, "alice-only").await;
    let bob_tag = harness.create_tag(bob_id, "bob-only").await;

    // Bob asking for Alice's tag: 404, not 403 — "not yours" and "not there"
    // must be the same answer, so the id cannot be confirmed as real.
    let (status, _, body) = harness
        .get(&format!("/api/v1/tags/{alice_tag}"), Some(&bob_cookie))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "cross-user read");
    assert_eq!(body["error"], "not found");
    assert!(
        !body.to_string().contains("alice-only"),
        "the 404 must not leak the object: {body}"
    );

    // Symmetric: Alice asking for Bob's tag.
    let (status, _, _) = harness
        .get(&format!("/api/v1/tags/{bob_tag}"), Some(&alice_cookie))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "cross-user read, other way");

    // A tag that never existed answers identically.
    let (status, _, body) = harness
        .get(
            &format!("/api/v1/tags/{}", Uuid::new_v4()),
            Some(&alice_cookie),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown id");
    assert_eq!(body["error"], "not found");

    // The owner still gets their own row.
    let (status, _, _) = harness
        .get(&format!("/api/v1/tags/{alice_tag}"), Some(&alice_cookie))
        .await;
    assert_eq!(status, StatusCode::OK);
}

/// The `state` parameter is single use, so a replayed callback cannot mint a
/// second session (login CSRF / replay).
#[tokio::test]
async fn replayed_or_unknown_state_does_not_create_a_session() {
    let Some(harness) =
        Harness::connect("replayed_or_unknown_state_does_not_create_a_session").await
    else {
        return;
    };

    let (_, headers, _) = harness.get("/auth/google", None).await;
    let location = headers.get(header::LOCATION).unwrap().to_str().unwrap();
    let state = query_param(location, "state").unwrap();

    let first = harness
        .get(
            &format!("/auth/google/callback?code=alice&state={state}"),
            None,
        )
        .await;
    assert_eq!(first.0, StatusCode::SEE_OTHER, "first use succeeds");
    assert!(first.1.contains_key(header::SET_COOKIE));

    let replay = harness
        .get(
            &format!("/auth/google/callback?code=alice&state={state}"),
            None,
        )
        .await;
    assert_eq!(replay.0, StatusCode::BAD_REQUEST, "state is single use");
    assert!(
        !replay.1.contains_key(header::SET_COOKIE),
        "no session on replay"
    );

    let unknown = harness
        .get("/auth/google/callback?code=alice&state=guessed-state", None)
        .await;
    assert_eq!(unknown.0, StatusCode::BAD_REQUEST);

    let missing = harness.get("/auth/google/callback?code=alice", None).await;
    assert_eq!(missing.0, StatusCode::BAD_REQUEST);

    // No cookie was minted by any of the failures, so the rows are one login's
    // worth, not four.
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&harness.pool)
        .await
        .unwrap();
    assert!(sessions >= 1, "the successful login's session exists");
}

/// Logout deletes the row and clears the cookie, so the old cookie is worthless.
#[tokio::test]
async fn logout_revokes_the_session() {
    let Some(harness) = Harness::connect("logout_revokes_the_session").await else {
        return;
    };

    let cookie = harness.login("alice").await;
    assert_eq!(
        harness.get("/api/v1/me", Some(&cookie)).await.0,
        StatusCode::OK
    );

    let (status, headers, _) = harness.post("/auth/logout", Some(&cookie)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let cleared = headers
        .get(header::SET_COOKIE)
        .expect("logout clears the cookie")
        .to_str()
        .unwrap();
    assert!(cleared.contains("Max-Age=0"), "{cleared}");

    // Same cookie, now revoked.
    assert_eq!(
        harness.get("/api/v1/me", Some(&cookie)).await.0,
        StatusCode::UNAUTHORIZED
    );

    // Logging out without a session is a no-op, not an error.
    assert_eq!(
        harness.post("/auth/logout", None).await.0,
        StatusCode::NO_CONTENT
    );

    // The run-scoped email exists exactly once even after several logins.
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE email = $1")
        .bind(format!("alice+{}@example.com", harness.run))
        .fetch_one(&harness.pool)
        .await
        .unwrap();
    assert_eq!(users, 1, "repeated logins reuse the same users row");
}
