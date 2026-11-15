//! Quick-capture acceptance test (ticket T12).
//!
//! Two things the ticket makes testable live here, both against the real
//! router and a live Postgres:
//!
//! 1. the 5000-character body limit — 5000 accepted, 5001 rejected, and the
//!    rejected one leaves no row behind;
//! 2. user scoping of both writes — the caller's id comes from the session
//!    cookie and a foreign tag or category answers 404, never a write.
//!
//! The session rows are inserted directly instead of walking the OAuth flow:
//! `auth_isolation` (T3) already owns that walk, and repeating it here would
//! only re-test the provider stub. See `tests/common/mod.rs` for the database
//! skip rules.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderValue, Request, StatusCode, header};
use chrono::TimeDelta;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use strands_core::{MAX_BODY_CHARS, SessionId, UserId};
use strands_server::app::{AppState, router};
use strands_server::auth::identity::BoxFuture;
use strands_server::auth::{AuthError, Identity, IdentityProvider, store};
use strands_server::config::AuthConfig;
use strands_server::summary::{BoxFuture as SummarizeBoxFuture, SummarizeError, Summarizer};
use tower::ServiceExt;
use uuid::Uuid;

mod common;

struct Harness {
    app: Router,
    pool: PgPool,
    run: String,
}

/// No capture path summarizes; a request here would mean the wrong code ran.
struct UnusedSummarizer;

impl Summarizer for UnusedSummarizer {
    fn model_alias(&self) -> &str {
        "unused"
    }

    fn summarize<'a>(
        &'a self,
        _prompt: &'a str,
    ) -> SummarizeBoxFuture<'a, Result<String, SummarizeError>> {
        Box::pin(async { Err(SummarizeError::Request("never called".into())) })
    }
}

impl Harness {
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
            // The capture endpoints never touch the provider: any stub will do.
            std::sync::Arc::new(UnusedProvider),
            std::sync::Arc::new(strands_core::Registry::new()),
            std::sync::Arc::new(UnusedSummarizer),
        ));

        Some(Self { app, pool, run })
    }

    /// A signed-in user with the five seeded categories, plus its cookie.
    async fn user(&self, name: &str) -> (UserId, String) {
        let identity = Identity {
            subject: format!("sub-{name}-{}", self.run),
            email: format!("{name}+{}@example.com", self.run),
            name: name.to_owned(),
            avatar_url: None,
        };
        let user_id = store::upsert_user(&self.pool, &identity)
            .await
            .expect("insert user");
        store::seed_default_categories(&self.pool, user_id)
            .await
            .expect("seed categories");
        let session_id: SessionId =
            store::create_session(&self.pool, user_id, TimeDelta::seconds(3600))
                .await
                .expect("create session");

        (user_id, format!("strands_session={session_id}"))
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, Value) {
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn post_json(&self, uri: &str, cookie: Option<&str>, body: Value) -> (StatusCode, Value) {
        let mut builder = Request::post(uri).header(header::CONTENT_TYPE, "application/json");
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        self.send(builder.body(Body::from(body.to_string())).unwrap())
            .await
    }

    async fn get(&self, uri: &str, cookie: &str) -> (StatusCode, Value) {
        let request = Request::get(uri)
            .header(header::COOKIE, HeaderValue::from_str(cookie).unwrap())
            .body(Body::empty())
            .unwrap();
        self.send(request).await
    }

    async fn category_id(&self, user_id: UserId) -> Uuid {
        sqlx::query_scalar("SELECT id FROM categories WHERE user_id = $1 ORDER BY position LIMIT 1")
            .bind(user_id)
            .fetch_one(&self.pool)
            .await
            .expect("login seeded the default categories")
    }

    /// Insert a tag straight into the table: this test is about capture, not
    /// about how a tag came to exist.
    async fn tag(&self, user_id: UserId, name: &str) -> Uuid {
        let category_id = self.category_id(user_id).await;
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

    async fn count_notes(&self, user_id: UserId) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM notes WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(&self.pool)
            .await
            .expect("count notes")
    }
}

/// No capture path asks the provider anything; a panic here would say otherwise.
struct UnusedProvider;

impl IdentityProvider for UnusedProvider {
    fn consent_url(&self, _request: &strands_server::auth::ConsentRequest<'_>) -> String {
        unreachable!("capture does not start a login")
    }

    fn exchange_code<'a>(
        &'a self,
        _code: &'a str,
        _code_verifier: &'a str,
        _redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<Identity, AuthError>> {
        unreachable!("capture does not exchange a code")
    }
}

#[tokio::test]
async fn capture_accepts_a_body_at_the_limit_and_previews_it_truncated() {
    let Some(harness) = Harness::connect("capture_accepts_a_body_at_the_limit").await else {
        return;
    };
    let (user_id, cookie) = harness.user("alice").await;
    let tag_id = harness.tag(user_id, "Tokyo").await;

    let body: String = "night train ".repeat(MAX_BODY_CHARS / 11 + 1);
    let body: String = body.chars().take(MAX_BODY_CHARS).collect();
    assert_eq!(body.chars().count(), MAX_BODY_CHARS);

    let (status, created) = harness
        .post_json(
            "/api/v1/threads",
            Some(&cookie),
            json!({ "body": body, "tag_id": tag_id }),
        )
        .await;

    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["tag_id"], json!(tag_id));
    assert_eq!(created["source_count"], json!(1));

    // The preview is the body's opening line, cut short — never all 5000 chars.
    let title = created["title"].as_str().expect("title");
    assert!(title.chars().count() < MAX_BODY_CHARS, "{title}");
    assert!(title.ends_with('…'), "{title}");

    // "Survives reload": the note is on disk and the list endpoint returns it.
    assert_eq!(harness.count_notes(user_id).await, 1);
    let thread_id: Uuid = created["id"].as_str().unwrap().parse().expect("uuid id");
    let stored_len: i32 =
        sqlx::query_scalar("SELECT char_length(body) FROM notes WHERE thread_id = $1")
            .bind(thread_id)
            .fetch_one(&harness.pool)
            .await
            .expect("the captured note is stored");
    assert_eq!(stored_len as usize, MAX_BODY_CHARS);

    let (status, threads) = harness
        .get(&format!("/api/v1/tags/{tag_id}/threads"), &cookie)
        .await;
    assert_eq!(status, StatusCode::OK, "{threads}");
    assert_eq!(threads.as_array().expect("array").len(), 1);
    assert_eq!(threads[0]["id"], created["id"]);
}

#[tokio::test]
async fn capture_rejects_a_body_past_the_limit_and_writes_nothing() {
    let Some(harness) = Harness::connect("capture_rejects_a_body_past_the_limit").await else {
        return;
    };
    let (user_id, cookie) = harness.user("alice").await;
    let tag_id = harness.tag(user_id, "Tokyo").await;

    let body = "a".repeat(MAX_BODY_CHARS + 1);
    let (status, error) = harness
        .post_json(
            "/api/v1/threads",
            Some(&cookie),
            json!({ "body": body, "tag_id": tag_id }),
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(
        error["error"],
        json!("body must not exceed 5000 characters"),
        "{error}"
    );
    assert_eq!(harness.count_notes(user_id).await, 0);
}

#[tokio::test]
async fn capture_without_a_tag_lands_in_the_callers_inbox() {
    let Some(harness) = Harness::connect("capture_without_a_tag_lands_in_inbox").await else {
        return;
    };
    let (user_id, cookie) = harness.user("alice").await;

    let (status, created) = harness
        .post_json(
            "/api/v1/threads",
            Some(&cookie),
            json!({ "body": "call the plumber", "tag_id": null }),
        )
        .await;

    assert_eq!(status, StatusCode::CREATED, "{created}");

    let name: String = sqlx::query_scalar("SELECT name FROM tags WHERE id = $1 AND user_id = $2")
        .bind(created["tag_id"].as_str().unwrap().parse::<Uuid>().unwrap())
        .bind(user_id)
        .fetch_one(&harness.pool)
        .await
        .expect("the inbox tag exists and belongs to the caller");
    assert_eq!(name, "Inbox");

    // A second tagless capture reuses the same Inbox instead of piling up.
    let (status, again) = harness
        .post_json(
            "/api/v1/threads",
            Some(&cookie),
            json!({ "body": "and the boiler", "tag_id": null }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{again}");
    assert_eq!(again["tag_id"], created["tag_id"]);
}

#[tokio::test]
async fn capture_into_another_users_tag_is_a_404_and_writes_nothing() {
    let Some(harness) = Harness::connect("capture_into_another_users_tag").await else {
        return;
    };
    let (alice, alice_cookie) = harness.user("alice").await;
    let (bob, _bob_cookie) = harness.user("bob").await;
    let bobs_tag = harness.tag(bob, "Bob's notes").await;

    let (status, error) = harness
        .post_json(
            "/api/v1/threads",
            Some(&alice_cookie),
            json!({ "body": "not mine", "tag_id": bobs_tag }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");

    assert_eq!(harness.count_notes(alice).await, 0);
    assert_eq!(harness.count_notes(bob).await, 0);
}

#[tokio::test]
async fn capture_without_a_session_is_a_401() {
    let Some(harness) = Harness::connect("capture_without_a_session").await else {
        return;
    };

    let (status, error) = harness
        .post_json(
            "/api/v1/threads",
            None,
            json!({ "body": "who am i", "tag_id": null }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{error}");

    let (status, error) = harness
        .post_json(
            "/api/v1/tags",
            None,
            json!({ "name": "Kyoto", "category_id": Uuid::nil() }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{error}");
}

#[tokio::test]
async fn a_new_tag_is_created_for_the_caller_only() {
    let Some(harness) = Harness::connect("a_new_tag_is_created_for_the_caller").await else {
        return;
    };
    let (alice, alice_cookie) = harness.user("alice").await;
    let (bob, _bob_cookie) = harness.user("bob").await;
    let alice_category = harness.category_id(alice).await;

    let (status, created) = harness
        .post_json(
            "/api/v1/tags",
            Some(&alice_cookie),
            json!({ "name": "Kyoto", "category_id": alice_category }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["name"], json!("Kyoto"));
    assert_eq!(created["thread_count"], json!(0));

    let owner: UserId = sqlx::query_scalar("SELECT user_id FROM tags WHERE id = $1")
        .bind(created["id"].as_str().unwrap().parse::<Uuid>().unwrap())
        .fetch_one(&harness.pool)
        .await
        .expect("the tag exists");
    assert_eq!(owner, alice);
    assert_ne!(owner, bob);

    // A capture can now be filed under it, and it shows up in the tag list.
    let (status, thread) = harness
        .post_json(
            "/api/v1/threads",
            Some(&alice_cookie),
            json!({ "body": "temple at dawn", "tag_id": created["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{thread}");
    assert_eq!(thread["tag_id"], created["id"]);

    let (status, tags) = harness.get("/api/v1/tags", &alice_cookie).await;
    assert_eq!(status, StatusCode::OK, "{tags}");
    let kyoto = tags
        .as_array()
        .expect("array")
        .iter()
        .find(|tag| tag["name"] == json!("Kyoto"))
        .expect("Kyoto is in the caller's tag list");
    assert_eq!(kyoto["thread_count"], json!(1));
}

#[tokio::test]
async fn a_new_tag_in_another_users_category_is_a_404() {
    let Some(harness) = Harness::connect("a_new_tag_in_another_users_category").await else {
        return;
    };
    let (_alice, alice_cookie) = harness.user("alice").await;
    let (bob, _bob_cookie) = harness.user("bob").await;
    let bob_category = harness.category_id(bob).await;

    let (status, error) = harness
        .post_json(
            "/api/v1/tags",
            Some(&alice_cookie),
            json!({ "name": "Kyoto", "category_id": bob_category }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");

    let tags: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tags WHERE user_id = $1")
        .bind(bob)
        .fetch_one(&harness.pool)
        .await
        .expect("count tags");
    assert_eq!(tags, 0, "the foreign category wrote no tag for its owner");
}

#[tokio::test]
async fn a_new_tag_needs_a_name() {
    let Some(harness) = Harness::connect("a_new_tag_needs_a_name").await else {
        return;
    };
    let (alice, cookie) = harness.user("alice").await;
    let category = harness.category_id(alice).await;

    let (status, error) = harness
        .post_json(
            "/api/v1/tags",
            Some(&cookie),
            json!({ "name": "   ", "category_id": category }),
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["error"], json!("name must not be empty"), "{error}");
}

#[tokio::test]
async fn categories_are_the_callers_own_in_chip_order() {
    let Some(harness) = Harness::connect("categories_are_the_callers_own").await else {
        return;
    };
    let (_alice, cookie) = harness.user("alice").await;

    let (status, categories) = harness.get("/api/v1/categories", &cookie).await;
    assert_eq!(status, StatusCode::OK, "{categories}");

    let names: Vec<&str> = categories
        .as_array()
        .expect("array")
        .iter()
        .map(|category| category["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, ["Travel", "Tech", "Food", "Reading", "Ideas"]);
}
