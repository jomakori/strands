//! T9 acceptance: AI tag assignment on ingest.
//!
//! Covers the whole path — request in, model call, tag/note/thread writes, grid
//! read — against a real Postgres when `STRANDS_TEST_REQUIRE_DB=1` is set.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{Duration, Utc};
use http_body_util::BodyExt;
use sqlx::PgPool;
use strands_api_types::{IngestItem, IngestRequest, IngestResult, TagSummary};
use strands_core::{Registry, UserId};
use strands_server::app::{AppState, router};
use strands_server::auth::identity::BoxFuture as IdentityFuture;
use strands_server::auth::{AuthError, ConsentRequest, Identity, IdentityProvider};
use strands_server::config::AuthConfig;
use strands_server::llm::{BoxFuture, LlmError};
use strands_server::tagging::{TagItem, Tagger};
use tower::ServiceExt;
use uuid::Uuid;

const MODEL_ALIAS: &str = "strands.cheap";

/// Answers every item with one of `names`, cycling, and records each batch.
struct RotatingTagger {
    names: Vec<String>,
    batch_sizes: std::sync::Mutex<Vec<usize>>,
}

impl RotatingTagger {
    fn new(names: &[&str]) -> Self {
        Self {
            names: names.iter().map(|name| (*name).to_owned()).collect(),
            batch_sizes: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl Tagger for RotatingTagger {
    fn model_alias(&self) -> &str {
        MODEL_ALIAS
    }

    fn assign<'a>(
        &'a self,
        _existing: &'a [String],
        items: &'a [TagItem],
    ) -> BoxFuture<'a, Result<Vec<String>, LlmError>> {
        Box::pin(async move {
            self.batch_sizes
                .lock()
                .expect("batch sizes lock")
                .push(items.len());
            Ok(items
                .iter()
                .enumerate()
                .map(|(index, _)| self.names[index % self.names.len()].clone())
                .collect())
        })
    }
}

/// Succeeds on the first batch and fails on the second, so a 41-item ingest
/// exercises "the model failed after some work was already counted".
struct FailsOnSecondBatch {
    calls: AtomicUsize,
}

impl Tagger for FailsOnSecondBatch {
    fn model_alias(&self) -> &str {
        MODEL_ALIAS
    }

    fn assign<'a>(
        &'a self,
        _existing: &'a [String],
        items: &'a [TagItem],
    ) -> BoxFuture<'a, Result<Vec<String>, LlmError>> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 1 {
                return Err(LlmError::Request("model exploded".into()));
            }
            Ok(vec!["Travel".to_owned(); items.len()])
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
}

struct TestUser {
    id: UserId,
    cookie: String,
}

impl Harness {
    async fn connect(tagger: Arc<dyn Tagger>) -> Option<Self> {
        let pool = common::connect("ingest_tagging").await?;
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
            // Neither model client is called by ingest tests; both are wired
            // with unused endpoints so a missing call cannot pass silently.
            Arc::new(strands_server::summary::LiteLlmSummarizer::new(
                strands_server::llm::LiteLlmClient::new("http://unused.invalid", "unused"),
            )),
        )
        .with_tagger(tagger);
        Some(Self {
            pool,
            app: router(state),
        })
    }

    /// A user with one category, the state a real signed-up user is in.
    async fn user(&self) -> TestUser {
        let user_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, google_sub, email, name) VALUES ($1, $2, $3, 'Test')")
            .bind(user_id)
            .bind(format!("ingest-test-subject-{user_id}"))
            .bind(format!("ingest-test-{user_id}@example.com"))
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
        sqlx::query(
            "INSERT INTO categories (id, user_id, name, color, position) \
             VALUES ($1, $2, 'Personal', '#123456', 0)",
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .execute(&self.pool)
        .await
        .expect("insert test category");
        TestUser {
            id: user_id,
            cookie: format!("strands_session={session_id}"),
        }
    }

    async fn ingest(&self, user: &TestUser, items: Vec<IngestItem>) -> (StatusCode, IngestResult) {
        let (status, body) = self
            .ingest_raw(
                user,
                serde_json::to_vec(&IngestRequest { items }).expect("serialize request"),
            )
            .await;
        let result = serde_json::from_value(body).unwrap_or_else(|error| {
            panic!("decode ingest result ({status}): {error}");
        });
        (status, result)
    }

    async fn ingest_raw(&self, user: &TestUser, body: Vec<u8>) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/ingest")
            .header(header::COOKIE, &user.cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .expect("build ingest request");
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
        let body = serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("non-JSON {status} body: {error}"));
        (status, body)
    }

    async fn grid_tags(&self, user: &TestUser) -> Vec<TagSummary> {
        let request = Request::builder()
            .uri("/api/v1/tags")
            .header(header::COOKIE, &user.cookie)
            .body(Body::empty())
            .expect("build tag list request");
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .expect("router response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("read tag list")
            .to_bytes();
        serde_json::from_slice(&bytes).expect("decode tag list")
    }

    async fn count(&self, table: &str, user: &TestUser) -> i64 {
        let sql = format!("SELECT count(*) FROM {table} WHERE user_id = $1");
        sqlx::query_scalar(&sql)
            .bind(user.id)
            .fetch_one(&self.pool)
            .await
            .expect("count rows")
    }
}

/// Forty mixed items land under two normalized tags, visible in the grid, in
/// one call — and a second run over the same items reuses those tags and adds
/// no notes, so nothing is duplicated.
#[tokio::test]
async fn forty_items_are_tagged_in_one_call_and_a_repeat_run_creates_no_duplicates() {
    let tagger = Arc::new(RotatingTagger::new(&["Travel", "  TRAVEL  ", "Food"]));
    let Some(harness) = Harness::connect(tagger.clone()).await else {
        return;
    };
    let user = harness.user().await;
    let items: Vec<IngestItem> = (0..40)
        .map(|index| IngestItem {
            body: format!("item {index}: mixed content about trips and meals"),
            external_id: Some(format!("ext-{index}")),
        })
        .collect();

    let (status, first) = harness.ingest(&user, items.clone()).await;
    assert_eq!(status, StatusCode::OK, "first ingest failed");
    assert_eq!(first.notes_created, 40);
    assert_eq!(
        *tagger.batch_sizes.lock().expect("batch sizes lock"),
        vec![40],
        "40 items must be one model call"
    );

    let grid = harness.grid_tags(&user).await;
    assert_eq!(
        grid.len(),
        2,
        "Travel variants collapse, Food is new: {grid:?}"
    );
    let mut names: Vec<&str> = grid.iter().map(|tag| tag.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["Food", "Travel"]);

    let (status, second) = harness.ingest(&user, items).await;
    assert_eq!(status, StatusCode::OK, "repeat ingest failed");
    assert_eq!(second.notes_created, 0, "repeat run must not add notes");
    let grid_after = harness.grid_tags(&user).await;
    assert_eq!(grid_after.len(), 2, "repeat run must not add tags");
    assert_eq!(harness.count("notes", &user).await, 40);
    assert_eq!(harness.count("threads", &user).await, 40);
}

/// A name the model repeats in a different case and with padding is attached to
/// the tag that already exists, not recreated beside it.
#[tokio::test]
async fn an_existing_tag_is_attached_not_recreated() {
    let tagger = Arc::new(RotatingTagger::new(&["  tRaVeL "]));
    let Some(harness) = Harness::connect(tagger).await else {
        return;
    };
    let user = harness.user().await;

    let category_id: Uuid = sqlx::query_scalar("SELECT id FROM categories WHERE user_id = $1")
        .bind(user.id)
        .fetch_one(&harness.pool)
        .await
        .expect("read category");
    let existing_tag = Uuid::new_v4();
    sqlx::query("INSERT INTO tags (id, user_id, category_id, name) VALUES ($1, $2, $3, 'Travel')")
        .bind(existing_tag)
        .bind(user.id)
        .bind(category_id)
        .execute(&harness.pool)
        .await
        .expect("insert existing tag");

    let (status, result) = harness
        .ingest(
            &user,
            vec![IngestItem {
                body: "train to Kyoto".to_owned(),
                external_id: Some("kyoto".to_owned()),
            }],
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(result.tags.len(), 1);
    assert_eq!(result.tags[0].id, existing_tag);
    let tags: i64 = sqlx::query_scalar("SELECT count(*) FROM tags WHERE user_id = $1")
        .bind(user.id)
        .fetch_one(&harness.pool)
        .await
        .expect("count tags");
    assert_eq!(tags, 1, "model's variant must not create a second tag");
    let note_tag: Uuid = sqlx::query_scalar("SELECT tag_id FROM notes WHERE user_id = $1")
        .bind(user.id)
        .fetch_one(&harness.pool)
        .await
        .expect("read note tag");
    assert_eq!(note_tag, existing_tag);
}

/// A failure in the second batch (41 items) surfaces as a server error and
/// leaves no tags, notes or threads behind: the first batch is not committed
/// either.
#[tokio::test]
async fn a_model_failure_leaves_no_half_applied_batch() {
    let tagger = Arc::new(FailsOnSecondBatch {
        calls: AtomicUsize::new(0),
    });
    let Some(harness) = Harness::connect(tagger).await else {
        return;
    };
    let user = harness.user().await;
    let items: Vec<IngestItem> = (0..41)
        .map(|index| IngestItem {
            body: format!("item {index}"),
            external_id: Some(format!("fail-{index}")),
        })
        .collect();

    let (status, body) = harness
        .ingest_raw(
            &user,
            serde_json::to_vec(&IngestRequest { items }).expect("serialize request"),
        )
        .await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        body,
        serde_json::json!({ "error": "internal server error" })
    );
    assert_eq!(harness.count("tags", &user).await, 0, "{body}");
    assert_eq!(harness.count("notes", &user).await, 0);
    assert_eq!(harness.count("threads", &user).await, 0);
}

/// More items than one request may carry is a client error, before any call.
#[tokio::test]
async fn oversized_ingests_are_rejected() {
    let tagger = Arc::new(RotatingTagger::new(&["Travel"]));
    let Some(harness) = Harness::connect(tagger).await else {
        return;
    };
    let user = harness.user().await;
    let items: Vec<IngestItem> = (0..201)
        .map(|index| IngestItem {
            body: format!("item {index}"),
            external_id: None,
        })
        .collect();

    let (status, body) = harness
        .ingest_raw(
            &user,
            serde_json::to_vec(&IngestRequest { items }).expect("serialize request"),
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("at most")
    );
}
