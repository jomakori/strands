//! The settings screen's connector rows, end to end (ticket T27).
//!
//! Acceptance, verbatim: the row states have to *work* — `NeedsSignIn` before a
//! user has connected, `connected` once a token is stored and openable, and a
//! provider failure visible on the row instead of a blank or a 500 — and the
//! connector has to report itself unconfigured rather than crash when
//! `STRANDS_SECRET_KEY` is absent (what the deployed pod logged on first boot).
//!
//! These go through the same `build_registry()` a deploy uses and the same HTTP
//! route the settings screen calls, so they pin the wiring `main.rs` used to get
//! wrong (a registry with no real connector in it at all) rather than a
//! hand-built registry that could stay green while the server drifted.
//!
//! Needs a live, migrated Postgres (`DATABASE_URL`); see `tests/common/mod.rs`
//! for the skip rules.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{Duration, Utc};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use strands_core::connector::Connector;
use strands_server::app::{AppState, router};
use strands_server::auth::identity::BoxFuture;
use strands_server::auth::{AuthError, ConsentRequest, Identity, IdentityProvider};
use strands_server::config::AuthConfig;
use strands_server::connectors::build_registry;
use strands_server::secrets::SecretKey;
use tower::ServiceExt;
use uuid::Uuid;

mod common;

struct TestUser {
    id: Uuid,
    cookie: String,
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

/// The test key: a fixed 32-byte secret, so the assertions are about the
/// connector and never about key generation (that is `secrets`' own test).
fn test_key() -> Arc<SecretKey> {
    Arc::new(SecretKey::from_base64(&STANDARD.encode([7u8; 32])).expect("a 32-byte test key"))
}

fn app(pool: &PgPool, key: Option<Arc<SecretKey>>) -> Router {
    let state = AppState::new(
        pool.clone(),
        AuthConfig {
            client_id: "unused-client-id".into(),
            issuer: "https://identity.example".into(),
            public_base_url: "https://test.example".into(),
            cookie_secure: true,
            session_ttl_days: 30,
        },
        Arc::new(UnusedIdentity),
        Arc::new(build_registry(pool.clone(), key.clone(), None, false)),
        Arc::new(strands_server::summary::LiteLlmSummarizer::new(
            strands_server::llm::LiteLlmClient::new("http://unused.invalid", "unused"),
        )),
    );
    router(state)
}

async fn user(pool: &PgPool, label: &str) -> TestUser {
    let user_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id, google_sub, email, name) VALUES ($1, $2, $3, 'Test')")
        .bind(user_id)
        .bind(format!("registry-test-subject-{user_id}"))
        .bind(format!("registry-{label}-{user_id}@example.com"))
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

/// Store the connector row a pick needs, and seal `token` under `key`.
async fn connect_photos(pool: &PgPool, user: &TestUser, key: &SecretKey, token: &str) -> Uuid {
    let connector_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO connectors (id, user_id, kind, status) VALUES ($1, $2, 'google_photos', 'connected')",
    )
    .bind(connector_id)
    .bind(user.id)
    .execute(pool)
    .await
    .expect("insert connector");
    sqlx::query(
        "INSERT INTO connector_secrets (connector_id, user_id, ciphertext) VALUES ($1, $2, $3)",
    )
    .bind(connector_id)
    .bind(user.id)
    .bind(key.seal(token).expect("seal the token"))
    .execute(pool)
    .await
    .expect("insert connector secret");
    connector_id
}

async fn get_row(app: &Router, user: &TestUser, id: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/connectors/{id}/status"))
                .header(axum::http::header::COOKIE, &user.cookie)
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("router response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).expect("connector row as JSON"),
    )
}

#[tokio::test]
async fn a_user_who_never_connected_sees_needs_sign_in() {
    let Some(pool) = common::connect("connector_registry").await else {
        return;
    };
    let app = app(&pool, Some(test_key()));
    let user = user(&pool, "needs-sign-in").await;

    let (status, row) = get_row(&app, &user, "photos").await;

    assert_eq!(status, StatusCode::OK, "{row}");
    assert_eq!(row["id"], "photos");
    assert_eq!(row["status"], "needs_sign_in");
    assert!(row["error"].is_null(), "no error, just an unconnected row");
}

#[tokio::test]
async fn a_stored_token_reads_back_as_connected() {
    let Some(pool) = common::connect("connector_registry").await else {
        return;
    };
    let key = test_key();
    let app = app(&pool, Some(key.clone()));
    let user = user(&pool, "connected").await;
    connect_photos(&pool, &user, &key, "ya29.round-trip-token").await;

    let (status, row) = get_row(&app, &user, "photos").await;

    assert_eq!(status, StatusCode::OK, "{row}");
    assert_eq!(row["status"], "connected", "the sealed token opened: {row}");
    assert!(row["error"].is_null());
}

/// The round trip at the level the connector owns: what was sealed is what the
/// pick route will hand to Google, and a token sealed under a *different* key
/// never reads as connected.
#[tokio::test]
async fn a_token_sealed_under_another_key_is_reported_not_hidden() {
    let Some(pool) = common::connect("connector_registry").await else {
        return;
    };
    let stored_key = test_key();
    let user = user(&pool, "rotated-key").await;
    connect_photos(&pool, &user, &stored_key, "ya29.old-token").await;

    // A restart with a rotated STRANDS_SECRET_KEY: same rows, different key.
    let rotated =
        Arc::new(SecretKey::from_base64(&STANDARD.encode([8u8; 32])).expect("a 32-byte test key"));
    let app = app(&pool, Some(rotated));

    let (status, row) = get_row(&app, &user, "photos").await;

    assert_eq!(status, StatusCode::OK, "{row}");
    assert!(row["status"].is_null(), "not connected, not silently fine");
    let error = row["error"].as_str().unwrap_or_default();
    assert!(error.contains("cannot be opened"), "got: {error}");
    assert!(error.contains("STRANDS_SECRET_KEY"), "got: {error}");
}

#[tokio::test]
async fn a_provider_failure_shows_on_the_row() {
    let Some(pool) = common::connect("connector_registry").await else {
        return;
    };
    let key = test_key();
    let app = app(&pool, Some(key.clone()));
    let user = user(&pool, "provider-error").await;
    let connector_id = connect_photos(&pool, &user, &key, "ya29.revoked").await;

    // What the pick lane records when Google answers `invalid_grant`.
    sqlx::query("UPDATE connectors SET status = 'error', last_error = $2 WHERE id = $1")
        .bind(connector_id)
        .bind("invalid_grant: Token has been expired or revoked.")
        .execute(&pool)
        .await
        .expect("record the provider failure");

    let (status, row) = get_row(&app, &user, "photos").await;

    assert_eq!(status, StatusCode::OK, "{row}");
    assert!(row["status"].is_null());
    let error = row["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("invalid_grant"),
        "the row has to carry the provider's own words, got: {error}"
    );
}

/// No `STRANDS_SECRET_KEY`: the row reports the deployment problem in its own
/// error — the pod logged exactly this on first boot — and the list still
/// answers `200` with a row, so Settings never renders blank.
#[tokio::test]
async fn without_the_sealing_key_the_row_reports_unconfigured() {
    let Some(pool) = common::connect("connector_registry").await else {
        return;
    };
    let app = app(&pool, None);
    let user = user(&pool, "no-key").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/connectors")
                .header(axum::http::header::COOKIE, &user.cookie)
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("router response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes();
    let rows: Vec<Value> = serde_json::from_slice(&bytes).expect("rows as JSON");

    assert_eq!(
        rows.len(),
        2,
        "the release registry is Photos and WhatsApp and nothing else"
    );
    let row = rows
        .iter()
        .find(|row| row["id"] == "photos")
        .expect("photos row");
    assert!(row["status"].is_null());
    let error = row["error"].as_str().unwrap_or_default();
    assert!(error.contains("STRANDS_SECRET_KEY"), "got: {error}");
    assert!(error.contains("cannot be opened"), "got: {error}");

    // The WhatsApp row is registered too, and says why it cannot link (T28).
    let whatsapp = rows
        .iter()
        .find(|row| row["id"] == "whatsapp")
        .expect("whatsapp row");
    assert!(whatsapp["status"].is_null());
    let error = whatsapp["error"].as_str().unwrap_or_default();
    assert!(error.contains("STRANDS_SECRET_KEY"), "got: {error}");
}

/// The manifest the settings screen renders is the real one, not a placeholder.
#[tokio::test]
async fn the_registered_connector_carries_its_real_manifest() {
    let Some(pool) = common::connect("connector_registry").await else {
        return;
    };
    let registry = build_registry(pool.clone(), Some(test_key()), None, false);
    let connector = registry.get("photos").expect("photos is registered");
    let manifest: &'static strands_core::connector::ConnectorManifest =
        Box::leak(Box::new(Connector::manifest(connector)));

    assert_eq!(manifest.label, "Google Photos");
    assert!(
        manifest
            .scopes
            .iter()
            .any(|scope| scope.contains("photospicker")),
        "the pick scopes, not a library listing: {:?}",
        manifest.scopes
    );
}
