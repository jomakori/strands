//! T7 acceptance: generation caching, forced versions, guide persistence,
//! history aliases and ordering, pruning, and cross-user 404s.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{Duration, Utc};
use http_body_util::BodyExt;
use sqlx::PgPool;
use strands_api_types::{GenerateSummaryRequest, SummaryGuide, SummaryHistory, SummaryVersion};
use strands_core::{Registry, SummaryPreset, TagId, UserId};
use strands_server::app::{AppState, router};
use strands_server::auth::identity::BoxFuture as IdentityFuture;
use strands_server::auth::{AuthError, ConsentRequest, Identity, IdentityProvider};
use strands_server::config::AuthConfig;
use strands_server::summary::store::{self, NewVersion};
use strands_server::summary::{BoxFuture, SummarizeError, Summarizer};
use tower::ServiceExt;
use uuid::Uuid;

const MODEL_ALIAS: &str = "test/summary";

#[derive(Default)]
struct CountingSummarizer {
    calls: AtomicUsize,
}

impl CountingSummarizer {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Summarizer for CountingSummarizer {
    fn model_alias(&self) -> &str {
        MODEL_ALIAS
    }

    fn summarize<'a>(&'a self, _prompt: &'a str) -> BoxFuture<'a, Result<String, SummarizeError>> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(format!("summary from fake call {call}"))
        })
    }
}

struct UnusedIdentity;

impl IdentityProvider for UnusedIdentity {
    fn consent_url(&self, _request: &ConsentRequest<'_>) -> String {
        "https://identity.example/unused".to_owned()
    }

    fn exchange_code<'a>(
        &'a self,
        _code: &'a str,
        _code_verifier: &'a str,
        _redirect_uri: &'a str,
    ) -> IdentityFuture<'a, Result<Identity, AuthError>> {
        Box::pin(async { Err(AuthError::InvalidCode) })
    }
}

struct Harness {
    pool: PgPool,
    app: Router,
    summarizer: Arc<CountingSummarizer>,
}

struct TestUser {
    id: UserId,
    cookie: String,
}

impl Harness {
    async fn connect() -> Option<Self> {
        let pool = common::connect("summary_generation").await?;
        let summarizer = Arc::new(CountingSummarizer::default());
        let state = AppState::new(
            pool.clone(),
            AuthConfig {
                client_id: "unused-client-id".into(),
                client_secret: "unused-client-secret".into(),
                public_base_url: "https://test.example".into(),
                cookie_secure: true,
                session_ttl_days: 30,
                authorization_endpoint: "https://identity.example/authorize".into(),
                token_endpoint: "https://identity.example/token".into(),
            },
            Arc::new(UnusedIdentity),
            Arc::new(Registry::new()),
            summarizer.clone(),
        );
        Some(Self {
            pool,
            app: router(state),
            summarizer,
        })
    }

    async fn user(&self) -> TestUser {
        let user_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO users (id, google_sub, email, name) VALUES ($1, $2, $3, 'Test User')",
        )
        .bind(user_id)
        .bind(format!("summary-test-subject-{user_id}"))
        .bind(format!("summary-test-{user_id}@example.com"))
        .execute(&self.pool)
        .await
        .expect("insert test user");
        sqlx::query("INSERT INTO sessions (id, user_id, expires_at) VALUES ($1, $2, $3)")
            .bind(session_id)
            .bind(user_id)
            .bind(Utc::now() + Duration::hours(1))
            .execute(&self.pool)
            .await
            .expect("insert test session");
        TestUser {
            id: user_id,
            cookie: format!("strands_session={session_id}"),
        }
    }

    async fn tag_with_note(&self, user: &TestUser, note_body: &str) -> TagId {
        let category_id = Uuid::new_v4();
        let tag_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, user_id, name, color, position) \
             VALUES ($1, $2, 'Trips', '#123456', 0)",
        )
        .bind(category_id)
        .bind(user.id)
        .execute(&self.pool)
        .await
        .expect("insert test category");
        sqlx::query(
            "INSERT INTO tags (id, user_id, category_id, name) VALUES ($1, $2, $3, 'Japan')",
        )
        .bind(tag_id)
        .bind(user.id)
        .bind(category_id)
        .execute(&self.pool)
        .await
        .expect("insert test tag");
        sqlx::query(
            "INSERT INTO notes \
                (id, user_id, tag_id, kind, body, occurred_at, source, dedupe_key) \
             VALUES ($1, $2, $3, 'text', $4, $5, 'manual', $6)",
        )
        .bind(Uuid::new_v4())
        .bind(user.id)
        .bind(tag_id)
        .bind(note_body)
        .bind(Utc::now())
        .bind(format!("summary-test-note-{}", Uuid::new_v4()))
        .execute(&self.pool)
        .await
        .expect("insert test note");
        tag_id
    }

    async fn generate_response(
        &self,
        user: &TestUser,
        tag_id: TagId,
        guide: SummaryGuide,
        force: bool,
    ) -> (StatusCode, serde_json::Value) {
        let request = GenerateSummaryRequest { guide, force };
        self.send_json(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/tags/{tag_id}/summary"))
                .header(header::COOKIE, &user.cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&request).expect("serialize generation request"),
                ))
                .expect("build generation request"),
        )
        .await
    }

    async fn generate(
        &self,
        user: &TestUser,
        tag_id: TagId,
        guide: SummaryGuide,
        force: bool,
    ) -> SummaryVersion {
        let (status, body) = self.generate_response(user, tag_id, guide, force).await;
        assert_eq!(status, StatusCode::OK, "generation failed: {body}");
        serde_json::from_value(body).expect("decode summary version")
    }

    async fn history(&self, user: &TestUser, tag_id: TagId) -> SummaryHistory {
        let (status, body) = self
            .send_json(
                Request::builder()
                    .uri(format!("/api/v1/tags/{tag_id}/summary/history"))
                    .header(header::COOKIE, &user.cookie)
                    .body(Body::empty())
                    .expect("build history request"),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "history failed: {body}");
        serde_json::from_value(body).expect("decode summary history")
    }

    async fn send_json(&self, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .expect("router response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("read response body")
            .to_bytes();
        let body = serde_json::from_slice(&bytes).expect("JSON response body");
        (status, body)
    }
}

#[tokio::test]
async fn unchanged_notes_reuse_the_existing_version_without_another_llm_call() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let tag_id = harness.tag_with_note(&user, "Tokyo on 2026-03-10").await;

    let first = harness
        .generate(&user, tag_id, SummaryGuide::Shorter, false)
        .await;
    let second = harness
        .generate(&user, tag_id, SummaryGuide::Shorter, false)
        .await;

    assert_eq!(harness.summarizer.calls(), 1);
    assert_eq!(second, first);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM summaries WHERE tag_id = $1")
        .bind(tag_id)
        .fetch_one(&harness.pool)
        .await
        .expect("count summary versions");
    assert_eq!(rows, 1);
}

#[tokio::test]
async fn force_true_generates_a_new_version_for_unchanged_notes() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let tag_id = harness.tag_with_note(&user, "Tokyo on 2026-03-10").await;

    let first = harness
        .generate(&user, tag_id, SummaryGuide::Shorter, false)
        .await;
    let forced = harness
        .generate(&user, tag_id, SummaryGuide::Shorter, true)
        .await;

    assert_eq!(harness.summarizer.calls(), 2);
    assert_eq!(first.version, 1);
    assert_eq!(forced.version, 2);
    assert_ne!(forced.id, first.id);
    assert_eq!(forced.body, "summary from fake call 2");
}

#[tokio::test]
async fn every_preset_and_freeform_text_is_recorded_on_its_version_row() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let tag_id = harness.tag_with_note(&user, "Tokyo on 2026-03-10").await;
    let freeform = "Focus on rail journeys and exact dates.".to_owned();
    let guides = [
        SummaryGuide::Shorter,
        SummaryGuide::Takeaways,
        SummaryGuide::Detail,
        SummaryGuide::Dates,
        SummaryGuide::Freeform {
            text: freeform.clone(),
        },
    ];

    for guide in guides {
        harness.generate(&user, tag_id, guide, false).await;
    }

    let rows: Vec<(i32, Option<SummaryPreset>, Option<String>)> = sqlx::query_as(
        "SELECT version, preset, guide FROM summaries WHERE tag_id = $1 ORDER BY version",
    )
    .bind(tag_id)
    .fetch_all(&harness.pool)
    .await
    .expect("read persisted guides");
    assert_eq!(
        rows,
        vec![
            (1, Some(SummaryPreset::Shorter), None),
            (2, Some(SummaryPreset::Takeaways), None),
            (3, Some(SummaryPreset::Detail), None),
            (4, Some(SummaryPreset::Dates), None),
            (5, None, Some(freeform)),
        ]
    );
}

#[tokio::test]
async fn history_is_newest_first_and_every_version_carries_its_model_alias() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let tag_id = harness.tag_with_note(&user, "Tokyo on 2026-03-10").await;
    let mut generated = Vec::new();
    for _ in 0..3 {
        generated.push(
            harness
                .generate(&user, tag_id, SummaryGuide::Takeaways, true)
                .await,
        );
    }

    let history = harness.history(&user, tag_id).await;
    let versions: Vec<i32> = history
        .versions
        .iter()
        .map(|version| version.version)
        .collect();
    assert_eq!(versions, vec![3, 2, 1]);
    let ids: Vec<Uuid> = history.versions.iter().map(|version| version.id).collect();
    let expected_ids: Vec<Uuid> = generated.iter().rev().map(|version| version.id).collect();
    assert_eq!(ids, expected_ids);
    assert!(
        history
            .versions
            .iter()
            .all(|version| version.model_alias == MODEL_ALIAS)
    );
}

#[tokio::test]
async fn inserting_twenty_one_versions_prunes_the_tag_to_twenty_rows() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let tag_id = harness.tag_with_note(&user, "Tokyo on 2026-03-10").await;

    for version in 1..=21 {
        let body = format!("summary {version}");
        let source_hash = format!("source-hash-{version}");
        store::insert_version(
            &harness.pool,
            NewVersion {
                tag_id,
                user_id: user.id,
                guide: &SummaryGuide::Shorter,
                body: &body,
                model_alias: MODEL_ALIAS,
                source_hash: &source_hash,
            },
        )
        .await
        .expect("insert summary version");
    }

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM summaries WHERE tag_id = $1")
        .bind(tag_id)
        .fetch_one(&harness.pool)
        .await
        .expect("count retained versions");
    assert_eq!(rows, 20);
    let retained: Vec<i32> =
        sqlx::query_scalar("SELECT version FROM summaries WHERE tag_id = $1 ORDER BY version DESC")
            .bind(tag_id)
            .fetch_all(&harness.pool)
            .await
            .expect("read retained version numbers");
    assert_eq!(retained, (2..=21).rev().collect::<Vec<_>>());
}

#[tokio::test]
async fn another_users_tag_returns_404_without_calling_the_summarizer() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let owner = harness.user().await;
    let intruder = harness.user().await;
    let foreign_tag = harness.tag_with_note(&owner, "Private travel notes").await;

    let (status, body) = harness
        .generate_response(&intruder, foreign_tag, SummaryGuide::Shorter, false)
        .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, serde_json::json!({ "error": "not found" }));
    assert_eq!(harness.summarizer.calls(), 0);
}
