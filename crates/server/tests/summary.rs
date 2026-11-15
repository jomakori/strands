//! Guided summary generation and history acceptance tests (ticket T7).
//!
//! Spec acceptance, verbatim:
//!
//! 1. a second generate with unchanged notes makes zero LLM calls;
//! 2. each preset is recorded on its version;
//! 3. history lists versions newest first.
//!
//! A counting fake [`Summarizer`] stands in for LiteLLM, mirroring how
//! `auth_isolation.rs` uses a fake [`IdentityProvider`] in place of Google.
//! Needs a live, migrated Postgres (`DATABASE_URL`); see `tests/common/mod.rs`
//! for the skip rules.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use strands_core::UserId;
use strands_server::app::{AppState, router};
use strands_server::auth::identity::BoxFuture as IdentityBoxFuture;
use strands_server::auth::{AuthError, ConsentRequest, Identity, IdentityProvider};
use strands_server::config::AuthConfig;
use strands_server::summary::{BoxFuture as SummarizeBoxFuture, SummarizeError, Summarizer};
use tower::ServiceExt;
use uuid::Uuid;

mod common;

/// Stands in for Google, same shape as `auth_isolation.rs`'s fake.
struct FakeIdentityProvider {
    run: String,
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
    ) -> IdentityBoxFuture<'a, Result<Identity, AuthError>> {
        Box::pin(async move {
            assert!(
                !code_verifier.is_empty(),
                "no PKCE verifier reached the exchange"
            );
            Ok(Identity {
                subject: format!("sub-{code}-{}", self.run),
                email: format!("{code}+{}@example.com", self.run),
                name: code.to_owned(),
                avatar_url: None,
            })
        })
    }
}

/// Stands in for LiteLLM: counts every call so a short-circuited generate is
/// provable, and answers deterministically so version bodies are inspectable.
struct CountingSummarizer {
    calls: Arc<AtomicUsize>,
}

impl Summarizer for CountingSummarizer {
    fn model_alias(&self) -> &str {
        "strands.summary"
    }

    fn summarize<'a>(
        &'a self,
        prompt: &'a str,
    ) -> SummarizeBoxFuture<'a, Result<String, SummarizeError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let body = format!("generated summary ({} chars of prompt)", prompt.len());
        Box::pin(async move { Ok(body) })
    }
}

struct Harness {
    app: Router,
    pool: PgPool,
    run: String,
    calls: Arc<AtomicUsize>,
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
        let calls = Arc::new(AtomicUsize::new(0));
        let app = router(AppState::new(
            pool.clone(),
            config,
            Arc::new(FakeIdentityProvider { run: run.clone() }),
            Arc::new(strands_core::Registry::new()),
            Arc::new(CountingSummarizer {
                calls: calls.clone(),
            }),
        ));

        Some(Self {
            app,
            pool,
            run,
            calls,
        })
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, Value) {
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    async fn get(&self, uri: &str, cookie: &str) -> (StatusCode, Value) {
        self.send(
            Request::get(uri)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn post_json(&self, uri: &str, cookie: &str, body: &Value) -> (StatusCode, Value) {
        self.send(
            Request::post(uri)
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        )
        .await
    }

    /// Walk the login flow for one fake account, as `auth_isolation.rs` does.
    async fn login(&self, code: &str) -> String {
        let response = self
            .app
            .clone()
            .oneshot(Request::get("/auth/google").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let location = response
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let state = query_param(&location, "state").expect("consent URL carries state");

        let response = self
            .app
            .clone()
            .oneshot(
                Request::get(format!("/auth/google/callback?code={code}&state={state}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("callback sets a session cookie")
            .to_str()
            .unwrap()
            .to_owned();

        set_cookie.split(';').next().unwrap().to_owned()
    }

    async fn user_id(&self, cookie: &str) -> UserId {
        let (status, body) = self.get("/api/v1/me", cookie).await;
        assert_eq!(status, StatusCode::OK, "GET /api/v1/me body: {body}");
        body["id"].as_str().unwrap().parse().unwrap()
    }

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

    async fn insert_note(&self, user_id: UserId, tag_id: Uuid, body: &str) {
        let note_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO notes (id, user_id, tag_id, kind, body, occurred_at, source, dedupe_key) \
             VALUES ($1, $2, $3, 'text', $4, now(), 'manual', $5)",
        )
        .bind(note_id)
        .bind(user_id)
        .bind(tag_id)
        .bind(body)
        .bind(format!("note-{note_id}"))
        .execute(&self.pool)
        .await
        .expect("insert note");
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

/// Acceptance rule 1: a second generate with unchanged notes makes zero LLM calls.
#[tokio::test]
async fn second_generate_with_unchanged_notes_makes_zero_llm_calls() {
    let Some(harness) =
        Harness::connect("second_generate_with_unchanged_notes_makes_zero_llm_calls").await
    else {
        return;
    };

    let cookie = harness.login(&format!("gen-{}", harness.run)).await;
    let user_id = harness.user_id(&cookie).await;
    let tag_id = harness.create_tag(user_id, "unchanged-notes").await;
    harness
        .insert_note(user_id, tag_id, "a note about Tokyo")
        .await;

    let request_body = json!({ "guide": { "kind": "shorter" }, "force": false });

    let (status, first) = harness
        .post_json(
            &format!("/api/v1/tags/{tag_id}/summary"),
            &cookie,
            &request_body,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "first generate: {first}");
    assert_eq!(first["version"], 1);
    assert_eq!(
        harness.calls.load(Ordering::SeqCst),
        1,
        "first generate calls the LLM exactly once"
    );

    let (status, second) = harness
        .post_json(
            &format!("/api/v1/tags/{tag_id}/summary"),
            &cookie,
            &request_body,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "second generate: {second}");
    assert_eq!(
        second["version"], 1,
        "unchanged notes return the same version, not a new one"
    );
    assert_eq!(second["body"], first["body"]);
    assert_eq!(
        harness.calls.load(Ordering::SeqCst),
        1,
        "second generate with unchanged notes makes zero additional LLM calls"
    );

    // `force: true` always calls the LLM, even on unchanged notes (regenerate).
    let forced = json!({ "guide": { "kind": "shorter" }, "force": true });
    let (status, third) = harness
        .post_json(&format!("/api/v1/tags/{tag_id}/summary"), &cookie, &forced)
        .await;
    assert_eq!(status, StatusCode::OK, "forced regenerate: {third}");
    assert_eq!(third["version"], 2, "force always creates a new version");
    assert_eq!(harness.calls.load(Ordering::SeqCst), 2);
}

/// Acceptance rule 2: each preset (and freeform) is recorded on its version.
#[tokio::test]
async fn each_preset_is_recorded_on_its_version() {
    let Some(harness) = Harness::connect("each_preset_is_recorded_on_its_version").await else {
        return;
    };

    let cookie = harness.login(&format!("preset-{}", harness.run)).await;
    let user_id = harness.user_id(&cookie).await;
    let tag_id = harness.create_tag(user_id, "every-preset").await;
    harness
        .insert_note(user_id, tag_id, "a note about Berlin")
        .await;

    let guides = [
        json!({ "kind": "shorter" }),
        json!({ "kind": "takeaways" }),
        json!({ "kind": "detail" }),
        json!({ "kind": "dates" }),
        json!({ "kind": "freeform", "text": "focus on dates and places" }),
    ];

    for guide in &guides {
        let (status, version) = harness
            .post_json(
                &format!("/api/v1/tags/{tag_id}/summary"),
                &cookie,
                &json!({ "guide": guide, "force": false }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "generate for {guide}: {version}");
        assert_eq!(
            &version["guide"], guide,
            "the recorded version carries the guide it was generated with"
        );
        assert_eq!(version["model_alias"], "strands.summary");
    }

    // Every guide hashes differently, so each one minted its own version.
    assert_eq!(harness.calls.load(Ordering::SeqCst), guides.len());
}

/// Acceptance rule 3: history lists versions newest first.
#[tokio::test]
async fn history_lists_versions_newest_first() {
    let Some(harness) = Harness::connect("history_lists_versions_newest_first").await else {
        return;
    };

    let cookie = harness.login(&format!("hist-{}", harness.run)).await;
    let user_id = harness.user_id(&cookie).await;
    let tag_id = harness.create_tag(user_id, "history-order").await;
    harness
        .insert_note(user_id, tag_id, "a note about Lisbon")
        .await;

    // Three forced regenerations mint versions 1, 2, 3 in that order.
    for _ in 0..3 {
        let (status, _) = harness
            .post_json(
                &format!("/api/v1/tags/{tag_id}/summary"),
                &cookie,
                &json!({ "guide": { "kind": "shorter" }, "force": true }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }

    let (status, history) = harness
        .get(&format!("/api/v1/tags/{tag_id}/summary/history"), &cookie)
        .await;
    assert_eq!(status, StatusCode::OK, "{history}");

    let versions = history["versions"].as_array().expect("versions array");
    assert_eq!(versions.len(), 3);
    let numbers: Vec<i64> = versions
        .iter()
        .map(|version| version["version"].as_i64().unwrap())
        .collect();
    assert_eq!(numbers, vec![3, 2, 1], "newest version first");

    // A foreign tag 404s, same path as "does not exist".
    let other_cookie = harness.login(&format!("other-{}", harness.run)).await;
    let (status, body) = harness
        .get(
            &format!("/api/v1/tags/{tag_id}/summary/history"),
            &other_cookie,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not found");
}
