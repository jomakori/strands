//! Link judging, end to end against Postgres (ticket T10).
//!
//! Gated like the rest of the DB suite: skipped without `DATABASE_URL`, failed
//! when `STRANDS_TEST_REQUIRE_DB=1` cannot reach one. The judge is a fake, so no
//! test here touches the network.

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{StatusCode, header};
use chrono::{Duration, Utc};
use http_body_util::BodyExt;
use sqlx::PgPool;
use strands_api_types::{JudgeLinksResponse, LinkSummary};
use strands_core::{Registry, UserId};
use strands_server::app::AppState;
use strands_server::auth::identity::BoxFuture as IdentityBoxFuture;
use strands_server::auth::{
    AuthError, ConsentRequest, Identity, IdentityProvider, session_cookie, store,
};
use strands_server::config::AuthConfig;
use strands_server::links::{CandidatePair, Judge, PairJudgement};
use strands_server::llm::{BoxFuture, LlmError};
use strands_server::summary::{BoxFuture as SummarizeBoxFuture, SummarizeError, Summarizer};
use tower::ServiceExt;
use uuid::Uuid;

/// Deterministic judge: a fixed score, and a rationale naming both tags so a
/// leak of the wrong pair into a row would be visible.
struct FakeJudge;

impl Judge for FakeJudge {
    fn model_alias(&self) -> &str {
        "test/cheap"
    }

    fn judge<'a>(
        &'a self,
        pairs: &'a [CandidatePair],
    ) -> BoxFuture<'a, Result<Vec<PairJudgement>, LlmError>> {
        Box::pin(async move {
            Ok(pairs
                .iter()
                .map(|pair| PairJudgement {
                    score: 82,
                    rationale: format!(
                        "{} and {} keep surfacing in the same place.",
                        pair.a_tag_name, pair.b_tag_name
                    ),
                })
                .collect())
        })
    }
}

/// Fails every call, to prove a failed judge writes nothing.
struct FailingJudge;

impl Judge for FailingJudge {
    fn model_alias(&self) -> &str {
        "test/failing"
    }

    fn judge<'a>(
        &'a self,
        _pairs: &'a [CandidatePair],
    ) -> BoxFuture<'a, Result<Vec<PairJudgement>, LlmError>> {
        Box::pin(async { Err(LlmError::Request("judge is down".into())) })
    }
}

/// No test signs in through the provider; a call here would be the wrong path.
struct UnusedIdentity;

impl IdentityProvider for UnusedIdentity {
    fn consent_url(&self, _request: &ConsentRequest<'_>) -> String {
        "https://accounts.example/unused".to_owned()
    }

    fn exchange_code<'a>(
        &'a self,
        _code: &'a str,
        _code_verifier: &'a str,
        _redirect_uri: &'a str,
    ) -> IdentityBoxFuture<'a, Result<Identity, AuthError>> {
        Box::pin(async { Err(AuthError::InvalidCode) })
    }
}

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

fn test_config() -> AuthConfig {
    AuthConfig {
        client_id: "test-client".into(),
        client_secret: "test-secret".into(),
        public_base_url: "https://strands.test".into(),
        cookie_secure: true,
        session_ttl_days: 30,
        authorization_endpoint: "https://accounts.example/consent".into(),
        token_endpoint: "https://accounts.example/token".into(),
    }
}

struct Harness {
    pool: PgPool,
    app: Router,
}

struct TestUser {
    id: UserId,
    cookie: String,
}

impl Harness {
    async fn new() -> Option<Self> {
        let pool = common::connect("links").await?;
        let app = app_with(pool.clone(), Arc::new(FakeJudge));
        Some(Self { pool, app })
    }

    async fn user(&self) -> TestUser {
        let run = Uuid::new_v4().simple().to_string();
        let identity = Identity {
            subject: format!("subject-{run}"),
            email: format!("{run}@strands.test"),
            name: "Link Tester".to_owned(),
            avatar_url: None,
        };

        let user = store::upsert_user(&self.pool, &identity)
            .await
            .expect("upsert test user");
        store::seed_default_categories(&self.pool, user)
            .await
            .expect("seed categories");
        let session_id = store::create_session(&self.pool, user, Duration::days(1))
            .await
            .expect("create session");

        TestUser {
            id: user,
            cookie: session_cookie(session_id, Duration::days(1), false, Utc::now()),
        }
    }

    async fn tag(&self, user: &TestUser, name: &str) -> Uuid {
        let category_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM categories WHERE user_id = $1 ORDER BY position LIMIT 1",
        )
        .bind(user.id)
        .fetch_one(&self.pool)
        .await
        .expect("load category");

        sqlx::query_scalar(
            "INSERT INTO tags (id, user_id, category_id, name) VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(user.id)
        .bind(category_id)
        .bind(name)
        .fetch_one(&self.pool)
        .await
        .expect("insert tag")
    }

    /// One thread owned by `owner`, carrying one note for each entry in `notes`.
    async fn thread_with_notes(
        &self,
        user: &TestUser,
        owner: Uuid,
        notes: &[(Uuid, &str)],
    ) -> Uuid {
        let thread_id: Uuid = sqlx::query_scalar(
            "INSERT INTO threads (id, user_id, tag_id, title) \
             VALUES ($1, $2, $3, 'Shared thread') RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(user.id)
        .bind(owner)
        .fetch_one(&self.pool)
        .await
        .expect("insert thread");

        for (tag_id, body) in notes {
            sqlx::query(
                "INSERT INTO notes \
                     (id, user_id, tag_id, thread_id, kind, body, source, dedupe_key, occurred_at) \
                 VALUES ($1, $2, $3, $4, 'text', $5, 'manual', $6, $7)",
            )
            .bind(Uuid::new_v4())
            .bind(user.id)
            .bind(tag_id)
            .bind(thread_id)
            .bind(body)
            .bind(Uuid::new_v4().to_string())
            .bind(Utc::now())
            .execute(&self.pool)
            .await
            .expect("insert note");
        }

        thread_id
    }

    /// One list carrying one place for each entry in `places`.
    async fn list_with_places(&self, user: &TestUser, places: &[(Uuid, &str)]) -> Uuid {
        let list_id: Uuid = sqlx::query_scalar(
            "INSERT INTO lists (id, user_id, name, source) \
             VALUES ($1, $2, 'Trip list', 'manual') RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(user.id)
        .fetch_one(&self.pool)
        .await
        .expect("insert list");

        for (tag_id, name) in places {
            sqlx::query(
                "INSERT INTO places (id, user_id, tag_id, list_id, name, source) \
                 VALUES ($1, $2, $3, $4, $5, 'manual')",
            )
            .bind(Uuid::new_v4())
            .bind(user.id)
            .bind(tag_id)
            .bind(list_id)
            .bind(name)
            .execute(&self.pool)
            .await
            .expect("insert place");
        }

        list_id
    }

    async fn judge(&self, user: &TestUser) -> (StatusCode, serde_json::Value) {
        call(&self.app, user, "POST", "/api/v1/links/judge").await
    }

    async fn links(&self, user: &TestUser) -> Vec<LinkSummary> {
        let (status, body) = call(&self.app, user, "GET", "/api/v1/links").await;
        assert_eq!(status, StatusCode::OK, "GET /api/v1/links: {body}");
        serde_json::from_value(body).expect("decode links")
    }

    async fn row_count(&self, user: &TestUser) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM links WHERE user_id = $1")
            .bind(user.id)
            .fetch_one(&self.pool)
            .await
            .expect("count links")
    }
}

fn app_with(pool: PgPool, judge: Arc<dyn Judge>) -> Router {
    let state = AppState::new(
        pool,
        test_config(),
        Arc::new(UnusedIdentity),
        Arc::new(Registry::new()),
        Arc::new(UnusedSummarizer),
    )
    .with_judge(judge);
    strands_server::app::router(state)
}

async fn call(
    app: &Router,
    user: &TestUser,
    method: &str,
    uri: &str,
) -> (StatusCode, serde_json::Value) {
    let request = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, &user.cookie)
        .body(Body::empty())
        .expect("build request");
    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("non-JSON {status} body: {error}"));
    (status, body)
}

/// Three tags, two evidence sources: Food/Travel share a thread, Travel/Sights
/// share a place list. Food and Sights share nothing.
async fn seed_trip(harness: &Harness, user: &TestUser) -> (Uuid, Uuid, Uuid) {
    let food = harness.tag(user, "Food").await;
    let travel = harness.tag(user, "Travel").await;
    let sights = harness.tag(user, "Sights").await;

    harness
        .thread_with_notes(
            user,
            food,
            &[
                (food, "Ramen near the station"),
                (travel, "Night bus to Kyoto"),
            ],
        )
        .await;
    harness
        .list_with_places(
            user,
            &[(travel, "Kyoto Station"), (sights, "Fushimi Inari")],
        )
        .await;

    (food, travel, sights)
}

fn pair_set(links: &[LinkSummary]) -> Vec<(Uuid, Uuid)> {
    let mut pairs: Vec<_> = links
        .iter()
        .map(|link| (link.a_tag_id, link.b_tag_id))
        .collect();
    pairs.sort();
    pairs
}

fn canonical(a: Uuid, b: Uuid) -> (Uuid, Uuid) {
    if a <= b { (a, b) } else { (b, a) }
}

#[tokio::test]
async fn three_tags_sharing_evidence_judge_to_at_least_one_edge_with_a_rationale() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let user = harness.user().await;
    let (food, travel, sights) = seed_trip(&harness, &user).await;

    let (status, body) = harness.judge(&user).await;

    assert_eq!(status, StatusCode::OK, "judge: {body}");
    let counts: JudgeLinksResponse = serde_json::from_value(body).expect("decode counts");
    assert_eq!(counts.judged, 2, "two evidence-backed pairs");
    assert_eq!(counts.created, 2);

    let links = harness.links(&user).await;
    assert!(!links.is_empty(), "at least one edge");

    let mut expected = vec![canonical(food, travel), canonical(travel, sights)];
    expected.sort();
    assert_eq!(pair_set(&links), expected);

    for link in &links {
        assert!(
            !link.rationale.trim().is_empty(),
            "every edge has a rationale"
        );
        assert!(link.score <= 100);
        assert!(
            link.a_tag_id < link.b_tag_id,
            "edges are stored in canonical order"
        );
        assert!(!link.a_tag_name.is_empty() && !link.b_tag_name.is_empty());
    }
}

#[tokio::test]
async fn a_tag_with_no_candidates_is_absent_from_the_links() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let user = harness.user().await;
    let solo = harness.tag(&user, "Solo").await;
    let (food, travel, sights) = seed_trip(&harness, &user).await;

    let (status, _) = harness.judge(&user).await;
    assert_eq!(status, StatusCode::OK);

    let links = harness.links(&user).await;
    // The trip itself still judges its two evidence-backed pairs; the point is
    // that `Solo` shares nothing, so it never appears on either end.
    let mut expected = vec![canonical(food, travel), canonical(travel, sights)];
    expected.sort();
    assert_eq!(pair_set(&links), expected);
    assert!(
        !links
            .iter()
            .any(|link| link.a_tag_id == solo || link.b_tag_id == solo),
        "an isolated tag must not gain an edge, so the constellation renders its empty state"
    );
}

#[tokio::test]
async fn judging_twice_keeps_exactly_one_row_per_pair() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let user = harness.user().await;
    seed_trip(&harness, &user).await;

    let (status, body) = harness.judge(&user).await;
    assert_eq!(status, StatusCode::OK, "first judge: {body}");
    let first: JudgeLinksResponse = serde_json::from_value(body).expect("decode first counts");
    let rows_after_first = harness.row_count(&user).await;
    assert_eq!(rows_after_first as u32, first.created);

    let (status, body) = harness.judge(&user).await;
    assert_eq!(status, StatusCode::OK, "second judge: {body}");
    let second: JudgeLinksResponse = serde_json::from_value(body).expect("decode second counts");

    assert_eq!(second.judged, first.judged);
    assert_eq!(second.created, 0, "nothing new on the second run");
    assert_eq!(second.updated, first.judged, "every pair was refreshed");
    assert_eq!(
        harness.row_count(&user).await,
        rows_after_first,
        "one row per pair, not one per judge run"
    );

    let links = harness.links(&user).await;
    let unique: std::collections::HashSet<_> = links
        .iter()
        .map(|link| (link.a_tag_id, link.b_tag_id))
        .collect();
    assert_eq!(unique.len(), links.len());
}

#[tokio::test]
async fn a_failed_judge_writes_no_rows() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let user = harness.user().await;
    seed_trip(&harness, &user).await;

    let failing = app_with(harness.pool.clone(), Arc::new(FailingJudge));

    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/api/v1/links/judge")
        .header(header::COOKIE, &user.cookie)
        .body(Body::empty())
        .expect("build request");
    let response = failing.oneshot(request).await.expect("router response");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        harness.row_count(&user).await,
        0,
        "a failed run must not leave a partial constellation"
    );
}

#[tokio::test]
async fn links_are_scoped_to_the_caller() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let first = harness.user().await;
    let second = harness.user().await;
    seed_trip(&harness, &first).await;

    assert_eq!(harness.judge(&first).await.0, StatusCode::OK);
    assert!(harness.links(&second).await.is_empty());

    let (status, _) = harness.judge(&second).await;
    assert_eq!(status, StatusCode::OK, "empty evidence is still a success");
    assert!(harness.links(&second).await.is_empty());
}

#[tokio::test]
async fn links_require_a_session() {
    let Some(harness) = Harness::new().await else {
        return;
    };
    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/api/v1/links")
        .body(Body::empty())
        .expect("build request");
    let response = harness.app.clone().oneshot(request).await.expect("routing");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
