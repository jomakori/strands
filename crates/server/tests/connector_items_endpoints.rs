//! HTTP surface for the connector select-then-import flow (ticket T33c):
//! `GET .../preview`, `POST .../import`, `GET .../history`.
//!
//! No production connector currently runs the generic `preview`/`import`
//! trait path over HTTP (Photos is pick-only, WhatsApp is sync-only — both
//! refuse the generic path on purpose), so this file registers a small
//! test-only connector that does, exactly so the HTTP route <-> ledger wiring
//! itself gets exercised end to end rather than only at the ledger's own
//! unit-test level (`connector_items_ledger.rs`, T33b).
//!
//! Needs a live, migrated Postgres (`DATABASE_URL`); see `tests/common/mod.rs`
//! for the skip rules.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{DateTime, Duration, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use strands_core::ConnectorStatus;
use strands_core::Registry;
use strands_core::connector::{
    Connector, ConnectorError, ConnectorFuture, ConnectorManifest, ImportItemResult, ImportOutcome,
    PreviewItem,
};
use strands_server::app::{AppState, router};
use strands_server::auth::identity::BoxFuture;
use strands_server::auth::{AuthError, ConsentRequest, Identity, IdentityProvider};
use strands_server::config::AuthConfig;
use strands_server::connectors::ledger;
use tower::ServiceExt;
use uuid::Uuid;

/// Fixed catalogue `TestLedgerConnector::preview` always offers. `import`
/// accepts any key, not only these, so the ledger dedupe is the only thing
/// under test, never the candidate list.
const CANDIDATE_KEYS: &[&str] = &["item-1", "item-2"];

/// Runs `preview`/`import` straight through the real ledger (T33b) with no
/// strand attached (`thread_id: None`) — everything this test needs to prove
/// lives in `connector_items`, not in `notes`.
struct TestLedgerConnector {
    pool: PgPool,
}

impl Connector for TestLedgerConnector {
    fn manifest(&self) -> ConnectorManifest {
        ConnectorManifest {
            id: "test-ledger",
            label: "Test Ledger",
            scopes: &[],
        }
    }

    fn status<'a>(
        &'a self,
        _user: Uuid,
    ) -> ConnectorFuture<'a, Result<ConnectorStatus, ConnectorError>> {
        Box::pin(async { Ok(ConnectorStatus::Connected) })
    }

    fn auth(&self, _user: Uuid) -> Result<(), ConnectorError> {
        Ok(())
    }

    fn preview<'a>(
        &'a self,
        user: Uuid,
    ) -> ConnectorFuture<'a, Result<Vec<PreviewItem>, ConnectorError>> {
        Box::pin(async move {
            let candidates: Vec<String> =
                CANDIDATE_KEYS.iter().map(|key| key.to_string()).collect();
            let existing = ledger::existing_keys(&self.pool, user, "test-ledger", &candidates)
                .await
                .map_err(|error| ConnectorError(format!("read ledger: {error}")))?;
            Ok(candidates
                .into_iter()
                .map(|item_key| {
                    let already_imported = existing.contains(&item_key);
                    PreviewItem {
                        label: format!("Test item {item_key}"),
                        timestamp: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
                        already_imported,
                        item_key,
                    }
                })
                .collect())
        })
    }

    fn import<'a>(
        &'a self,
        user: Uuid,
        item_keys: Vec<String>,
    ) -> ConnectorFuture<'a, Result<Vec<ImportItemResult>, ConnectorError>> {
        Box::pin(async move {
            let mut results = Vec::with_capacity(item_keys.len());
            for item_key in item_keys {
                let mut tx = self
                    .pool
                    .begin()
                    .await
                    .map_err(|error| ConnectorError(format!("begin tx: {error}")))?;
                let inserted = ledger::record_import(
                    &mut tx,
                    user,
                    "test-ledger",
                    &item_key,
                    "test-hash",
                    None,
                )
                .await
                .map_err(|error| ConnectorError(format!("record import: {error}")))?;
                tx.commit()
                    .await
                    .map_err(|error| ConnectorError(format!("commit: {error}")))?;
                let outcome = if inserted {
                    ImportOutcome::Imported
                } else {
                    ImportOutcome::SkippedAlreadyImported
                };
                results.push(ImportItemResult { item_key, outcome });
            }
            Ok(results)
        })
    }

    fn disconnect<'a>(&'a self, _user: Uuid) -> ConnectorFuture<'a, Result<(), ConnectorError>> {
        Box::pin(async { Ok(()) })
    }
}

struct UnusedIdentity;

impl IdentityProvider for UnusedIdentity {
    fn consent_url(&self, _request: &ConsentRequest<'_>) -> String {
        "https://accounts.example/consent".to_owned()
    }

    fn exchange_code<'a>(
        &'a self,
        _code: &'a str,
        _code_verifier: &'a str,
        _redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<Identity, AuthError>> {
        Box::pin(async { Err(AuthError::InvalidCode) })
    }
}

/// Registers only `TestLedgerConnector`, so a lookup for any other id is a
/// genuine 404 rather than accidentally matching Photos or WhatsApp.
fn app(pool: &PgPool) -> Router {
    let mut registry = Registry::new();
    registry.register(TestLedgerConnector { pool: pool.clone() });

    let state = AppState::new(
        pool.clone(),
        AuthConfig {
            client_id: "unused-client-id".into(),
            issuer: "https://identity.example".into(),
            public_base_url: "https://test.example".into(),
            cookie_secure: true,
            session_ttl_days: 30,
        },
        std::sync::Arc::new(UnusedIdentity),
        std::sync::Arc::new(registry),
        std::sync::Arc::new(strands_server::summary::LiteLlmSummarizer::new(
            strands_server::llm::LiteLlmClient::new("http://unused.invalid", "unused"),
        )),
    );
    router(state)
}

struct TestUser {
    id: Uuid,
    cookie: String,
}

async fn user(pool: &PgPool, label: &str) -> TestUser {
    let user_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id, google_sub, email, name) VALUES ($1, $2, $3, 'Test')")
        .bind(user_id)
        .bind(format!("connector-endpoints-subject-{user_id}"))
        .bind(format!("connector-endpoints-{label}-{user_id}@example.com"))
        .execute(pool)
        .await
        .expect("insert test user");
    sqlx::query("INSERT INTO sessions (id, user_id, expires_at) VALUES ($1, $2, $3)")
        .bind(session_id)
        .bind(user_id)
        .bind(Utc::now() + Duration::hours(1))
        .execute(pool)
        .await
        .expect("insert test session");
    TestUser {
        id: user_id,
        cookie: format!("strands_session={session_id}"),
    }
}

async fn body_json(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body as JSON")
    };
    (status, value)
}

async fn get(app: &Router, user: &TestUser, path: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header(header::COOKIE, &user.cookie)
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("router response");
    body_json(response).await
}

async fn post_json(app: &Router, user: &TestUser, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(header::COOKIE, &user.cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&body).expect("serialize body"),
                ))
                .expect("build request"),
        )
        .await
        .expect("router response");
    body_json(response).await
}

#[tokio::test]
async fn unknown_connector_is_404_on_all_three_routes() {
    let Some(pool) = common::connect("connector_items_endpoints").await else {
        return;
    };
    let app = app(&pool);
    let user = user(&pool, "unknown-connector").await;

    let (status, _) = get(&app, &user, "/api/v1/connectors/nope/preview").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = post_json(
        &app,
        &user,
        "/api/v1/connectors/nope/import",
        json!({ "item_keys": ["x"] }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = get(&app, &user, "/api/v1/connectors/nope/history").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn importing_an_empty_selection_is_400() {
    let Some(pool) = common::connect("connector_items_endpoints").await else {
        return;
    };
    let app = app(&pool);
    let user = user(&pool, "empty-selection").await;

    let (status, body) = post_json(
        &app,
        &user,
        "/api/v1/connectors/test-ledger/import",
        json!({ "item_keys": [] }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn preview_import_history_cycle() {
    let Some(pool) = common::connect("connector_items_endpoints").await else {
        return;
    };
    let app = app(&pool);
    let user = user(&pool, "cycle").await;

    // Nothing imported yet: preview reports both candidates fresh.
    let (status, preview) = get(&app, &user, "/api/v1/connectors/test-ledger/preview").await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["connector"], "test-ledger");
    let items = preview["items"].as_array().expect("items array");
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|item| item["already_imported"].as_bool() == Some(false)),
        "{items:?}"
    );

    // First run: import item-1 only.
    let (status, result) = post_json(
        &app,
        &user,
        "/api/v1/connectors/test-ledger/import",
        json!({ "item_keys": ["item-1"] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["imported"], 1);
    assert_eq!(result["skipped"], 0);
    assert_eq!(result["failed"], 0);
    assert_eq!(result["results"][0]["item_key"], "item-1");
    assert_eq!(result["results"][0]["outcome"], "imported");

    // Push that run's row an hour into the past so it lands in a different
    // `date_trunc('second', ...)` bucket than the second run below — the
    // real-world gap between two separate imports, made deterministic
    // instead of depending on the test's own wall-clock speed.
    sqlx::query(
        "UPDATE connector_items SET imported_at = imported_at - interval '1 hour' \
          WHERE user_id = $1 AND connector = 'test-ledger' AND item_key = 'item-1'",
    )
    .bind(user.id)
    .execute(&pool)
    .await
    .expect("age the first run's row");

    // Re-importing the same key reports it as skipped, writes nothing new.
    let (status, result) = post_json(
        &app,
        &user,
        "/api/v1/connectors/test-ledger/import",
        json!({ "item_keys": ["item-1"] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["imported"], 0);
    assert_eq!(result["skipped"], 1);
    assert_eq!(result["results"][0]["outcome"], "skipped_already_imported");

    // Preview now reports item-1 as already imported, item-2 still fresh.
    let (_, preview) = get(&app, &user, "/api/v1/connectors/test-ledger/preview").await;
    let items = preview["items"].as_array().expect("items array");
    let item1 = items
        .iter()
        .find(|item| item["item_key"] == "item-1")
        .expect("item-1 in preview");
    assert_eq!(item1["already_imported"].as_bool(), Some(true));
    let item2 = items
        .iter()
        .find(|item| item["item_key"] == "item-2")
        .expect("item-2 in preview");
    assert_eq!(item2["already_imported"].as_bool(), Some(false));

    // Second run: import item-2, a genuinely new key.
    let (status, result) = post_json(
        &app,
        &user,
        "/api/v1/connectors/test-ledger/import",
        json!({ "item_keys": ["item-2"] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["imported"], 1);
    assert_eq!(result["skipped"], 0);

    // History shows both runs, each with exactly one imported row.
    let (status, history) = get(&app, &user, "/api/v1/connectors/test-ledger/history").await;
    assert_eq!(status, StatusCode::OK, "{history}");
    assert_eq!(history["connector"], "test-ledger");
    let runs = history["runs"].as_array().expect("runs array");
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0]["imported"], 1, "{runs:?}");
    assert_eq!(runs[1]["imported"], 1, "{runs:?}");
}
