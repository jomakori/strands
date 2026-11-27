//! Connector settings endpoints: `GET /api/v1/connectors` and
//! `GET /api/v1/connectors/{id}/status` (spec §7.7, T13), plus this process's
//! connector set (T27).
//!
//! Both handlers go straight through [`strands_core::Registry`]: a connector
//! whose own status check fails still answers with its own row (list) or its
//! own body (single lookup) — never a 500 that takes the rest down with it.

pub mod ledger;
mod photos;
mod photos_connect;
mod photos_refresh;
mod whatsapp;

pub use photos::PhotosConnector;
pub use photos_connect::PhotosOAuth;
pub use photos_refresh::access_token as photos_access_token;
pub use whatsapp::WhatsAppConnector;

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use sqlx::PgPool;
use strands_api_types::{
    ConnectorHistory, ConnectorHistoryRun, ConnectorImportItemResult, ConnectorImportOutcome,
    ConnectorImportRequest, ConnectorImportResult, ConnectorPreview, ConnectorPreviewItem,
    ConnectorStatus as WireStatus, ConnectorSummary,
};
use strands_core::ConnectorStatus;
use strands_core::Registry;
use strands_core::connector::{Connector, ConnectorReport, ImportOutcome};
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::secrets::SecretKey;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/connectors", get(list_connectors))
        .route("/api/v1/connectors/{id}/status", get(connector_status))
        .route(
            "/api/v1/connectors/{id}/disconnect",
            post(connector_disconnect),
        )
        .route("/api/v1/connectors/{id}/preview", get(connector_preview))
        .route("/api/v1/connectors/{id}/import", post(connector_import))
        .route("/api/v1/connectors/{id}/history", get(connector_history))
        // The Google Photos grant lives beside the registry because it is the
        // only connector with a real OAuth flow so far (ticket B2).
        .merge(photos_connect::routes())
}

/// Environment switch that adds the stub, the deliberately failing connector
/// and the not-yet-built placeholders. Never set it in a deployment: the fake
/// rows it adds are indistinguishable from real ones on the settings screen.
pub const DEV_CONNECTORS_ENV: &str = "STRANDS_DEV_CONNECTORS";

/// Whether [`DEV_CONNECTORS_ENV`] asks for the fake connectors.
pub fn dev_connectors_enabled() -> bool {
    matches!(
        std::env::var(DEV_CONNECTORS_ENV).as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// This process's connector set.
///
/// Google Photos is always registered: it is the one connector that can
/// actually work, so a settings screen that omits it is lying about what the
/// deploy can do. Everything else here is scaffolding — the stub, the
/// deliberately failing connector and the four placeholders (Gmail, Notion,
/// Drive, Telegram) exist to exercise row states — and stays behind `dev`.
pub fn build_registry(
    pool: PgPool,
    secrets: Option<Arc<SecretKey>>,
    whatsapp_sessions: Option<Arc<crate::whatsapp::bridge::Registry>>,
    dev: bool,
) -> Registry {
    use strands_core::connector::{FailingConnector, PlaceholderConnector, StubConnector};

    let mut registry = Registry::new();
    registry.register(PhotosConnector::new(pool.clone(), secrets));
    // WhatsApp is always a row, configured or not: the spec's §7.7 asks for the
    // connector to be visible, and an unconfigured deploy has to say *why* it
    // cannot link instead of hiding the row (T28).
    registry.register(WhatsAppConnector::new(pool, whatsapp_sessions));

    if dev {
        registry.register(StubConnector);
        registry.register(FailingConnector);
        registry.register(PlaceholderConnector::new(
            "gmail",
            "Gmail",
            &["gmail.readonly"],
        ));
        registry.register(PlaceholderConnector::new(
            "notion",
            "Notion",
            &["notion.read"],
        ));
        registry.register(PlaceholderConnector::new(
            "drive",
            "Drive",
            &["drive.readonly"],
        ));
        registry.register(PlaceholderConnector::new(
            "telegram",
            "Telegram",
            &["telegram.messages"],
        ));
    }

    registry
}

/// Every registered connector for the caller, in the registry's stable id order.
pub async fn list_connectors(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Json<Vec<ConnectorSummary>> {
    Json(
        state
            .registry
            .reports(user.id)
            .await
            .into_iter()
            .map(to_summary)
            .collect(),
    )
}

/// One connector's row, if and only if that id is registered.
pub async fn connector_status(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<ConnectorSummary>, AppError> {
    let connector = state.registry.get(&id).ok_or(AppError::NotFound)?;
    Ok(Json(summary(connector, user.id).await))
}

/// `POST /api/v1/connectors/{id}/disconnect` — revoke the connector's access
/// and forget what was stored for this user, then answer with the row as it
/// now reads (ticket B4).
///
/// The body is the same [`ConnectorSummary`] `status` returns, so the settings
/// screen updates a row from the disconnect's own answer instead of asking
/// again. A repeat call is a `200` with the same body: the connector's
/// `disconnect` is specified as a no-op once there is nothing left to purge.
pub async fn connector_disconnect(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<ConnectorSummary>, AppError> {
    let connector = state.registry.get(&id).ok_or(AppError::NotFound)?;
    connector.disconnect(user.id).await?;
    Ok(Json(summary(connector, user.id).await))
}

/// The wire row for one connector: its own status if it can answer, its own
/// error if it cannot — never a 500 that takes the rest of the screen down.
async fn summary(connector: &dyn Connector, user: Uuid) -> ConnectorSummary {
    let manifest = connector.manifest();
    let (status, error) = match connector.status(user).await {
        Ok(status) => (Some(map_status(status)), None),
        Err(error) => (None, Some(error.to_string())),
    };

    ConnectorSummary {
        id: manifest.id.to_owned(),
        label: manifest.label.to_owned(),
        scopes: manifest
            .scopes
            .iter()
            .map(|scope| scope.to_string())
            .collect(),
        status,
        error,
    }
}

/// Everything this connector can currently see for the caller, each item
/// flagged with whether an earlier `import` already landed it. The connector
/// itself owns the already-imported check (it is the one holding the
/// ledger-backed `preview` implementation), so this handler is a pure
/// lookup-and-map: unknown id is 404, everything else is the connector's own
/// answer.
pub async fn connector_preview(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<ConnectorPreview>, AppError> {
    let connector = state.registry.get(&id).ok_or(AppError::NotFound)?;
    let items = connector.preview(user.id).await?;

    Ok(Json(ConnectorPreview {
        connector: id,
        items: items
            .into_iter()
            .map(|item| ConnectorPreviewItem {
                item_key: item.item_key,
                label: item.label,
                timestamp: item.timestamp,
                already_imported: item.already_imported,
            })
            .collect(),
    }))
}

/// Import exactly the requested item keys. An empty selection is a client
/// mistake (nothing to do), not a vacuous success, so it is rejected with
/// `400` before the connector is even asked.
pub async fn connector_import(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
    payload: Result<Json<ConnectorImportRequest>, JsonRejection>,
) -> Result<Json<ConnectorImportResult>, AppError> {
    let connector = state.registry.get(&id).ok_or(AppError::NotFound)?;
    let Json(request) = payload.map_err(|_| AppError::BadRequest("invalid import body".into()))?;
    if request.item_keys.is_empty() {
        return Err(AppError::BadRequest("item_keys must not be empty".into()));
    }

    let outcomes = connector.import(user.id, request.item_keys).await?;

    let mut imported = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let results = outcomes
        .into_iter()
        .map(|result| {
            let outcome = match result.outcome {
                ImportOutcome::Imported => {
                    imported += 1;
                    ConnectorImportOutcome::Imported
                }
                ImportOutcome::SkippedAlreadyImported => {
                    skipped += 1;
                    ConnectorImportOutcome::SkippedAlreadyImported
                }
                ImportOutcome::Failed(reason) => {
                    failed += 1;
                    ConnectorImportOutcome::Failed { reason }
                }
            };
            ConnectorImportItemResult {
                item_key: result.item_key,
                outcome,
            }
        })
        .collect();

    Ok(Json(ConnectorImportResult {
        results,
        imported,
        skipped,
        failed,
    }))
}

/// This user's import runs for `connector`, newest first (see
/// `ledger::history` for how a "run" is derived with no run id column).
pub async fn connector_history(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<ConnectorHistory>, AppError> {
    // Same 404 rule as the other two routes: an unknown connector id answers
    // 404 even though this query never touches the connector itself.
    state.registry.get(&id).ok_or(AppError::NotFound)?;

    let runs = ledger::history(&state.pool, user.id, &id).await?;

    Ok(Json(ConnectorHistory {
        connector: id,
        runs: runs
            .into_iter()
            .map(|run| ConnectorHistoryRun {
                imported_at: run.imported_at,
                imported: run.imported,
                // See `ConnectorHistoryRun`'s doc comment: a skip never
                // writes a ledger row, so there is nothing to count it from.
                skipped: 0,
            })
            .collect(),
    }))
}

fn to_summary(report: ConnectorReport) -> ConnectorSummary {
    ConnectorSummary {
        id: report.manifest.id.to_owned(),
        label: report.manifest.label.to_owned(),
        scopes: report
            .manifest
            .scopes
            .iter()
            .map(|scope| scope.to_string())
            .collect(),
        status: report.status.map(map_status),
        error: report.error,
    }
}

/// `strands-core`'s status carries a `sqlx::Type` derive that the wasm `strands-web`
/// build must never depend on, so the wire enum in `strands-api-types` is its own
/// type and this is the one place the two get mapped into each other.
fn map_status(status: ConnectorStatus) -> WireStatus {
    match status {
        ConnectorStatus::Disconnected => WireStatus::Disconnected,
        ConnectorStatus::NeedsSignIn => WireStatus::NeedsSignIn,
        ConnectorStatus::Connected => WireStatus::Connected,
        ConnectorStatus::Error => WireStatus::Error,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration as StdDuration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use chrono::{Duration, Utc};
    use sqlx::PgPool;
    use sqlx::postgres::PgPoolOptions;
    use strands_core::Registry;
    use strands_core::connector::{FailingConnector, PlaceholderConnector, StubConnector};
    use tower::ServiceExt;
    use uuid::Uuid;

    use super::*;
    use crate::auth::identity::BoxFuture;
    use crate::auth::{AuthError, ConsentRequest, Identity, IdentityProvider};
    use crate::config::AuthConfig;
    use crate::summary::{SummarizeError, Summarizer};

    struct NoopIdentity;

    impl IdentityProvider for NoopIdentity {
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

    /// Never reached: no connector route generates a summary (ticket T7).
    struct NeverSummarized;

    impl Summarizer for NeverSummarized {
        fn model_alias(&self) -> &str {
            "unused"
        }

        fn summarize<'a>(
            &'a self,
            _prompt: &'a str,
        ) -> crate::summary::BoxFuture<'a, Result<String, SummarizeError>> {
            Box::pin(async { Err(SummarizeError::Request("never called".into())) })
        }
    }

    // Mirrors `app::tests::test_state`, plus the registry this module needs: a
    // router is cheap to build from a lazy pool, so these tests never touch Postgres.
    fn test_state(registry: Registry) -> AppState {
        AppState::new(
            PgPool::connect_lazy("postgres://strands@localhost/strands").unwrap(),
            AuthConfig {
                client_id: "client-id".into(),
                issuer: "https://accounts.example".into(),
                public_base_url: "http://localhost:8080".into(),
                cookie_secure: false,
                session_ttl_days: 30,
            },
            Arc::new(NoopIdentity),
            Arc::new(registry),
            Arc::new(NeverSummarized),
        )
    }

    fn fake_user() -> CurrentUser {
        CurrentUser {
            id: Uuid::nil(),
            session_id: Uuid::nil(),
        }
    }

    #[tokio::test]
    async fn listing_connectors_gives_the_failing_one_its_own_row_not_a_500() {
        let mut registry = Registry::new();
        registry.register(StubConnector);
        registry.register(FailingConnector);

        let Json(summaries) = list_connectors(State(test_state(registry)), fake_user()).await;

        assert_eq!(summaries.len(), 2, "both connectors get a row");

        let stub = summaries.iter().find(|s| s.id == "stub").unwrap();
        assert_eq!(stub.status, Some(WireStatus::Connected));
        assert!(stub.error.is_none());

        let failing = summaries.iter().find(|s| s.id == "failing").unwrap();
        assert!(failing.status.is_none());
        assert!(failing.error.is_some(), "its own error, not a panic");
    }

    #[tokio::test]
    async fn status_for_an_unregistered_id_is_404_not_a_panic() {
        let result = connector_status(
            State(test_state(Registry::new())),
            fake_user(),
            Path("missing".to_owned()),
        )
        .await;

        assert!(matches!(result, Err(AppError::NotFound)));
    }

    fn lazy_pool() -> PgPool {
        PgPool::connect_lazy("postgres://strands@localhost/strands").unwrap()
    }

    /// What a deploy registers: the two connectors that can actually work —
    /// Google Photos and the WhatsApp bridge (spec §7.7) — and nothing that
    /// fakes a status. This is the test the settings screen hangs on:
    /// "connectors appear once one is configured" must not stay true by
    /// omission.
    #[tokio::test]
    async fn a_release_registry_registers_the_live_connectors_and_no_scaffolding() {
        let registry = build_registry(lazy_pool(), None, None, false);

        assert_eq!(registry.ids(), vec!["photos", "whatsapp"]);
        let manifest = registry.get("photos").unwrap().manifest();
        assert_eq!(manifest.label, "Google Photos");
        assert_eq!(manifest.scopes, photos::SCOPES);

        let whatsapp = registry.get("whatsapp").unwrap().manifest();
        assert_eq!(whatsapp.label, "WhatsApp");
        // The phone grants a device session, not OAuth scopes.
        assert!(whatsapp.scopes.is_empty());
    }

    /// The dev switch adds the scaffolding rows back, still in id order.
    #[tokio::test]
    async fn the_dev_switch_adds_the_scaffolding_connectors() {
        let registry = build_registry(lazy_pool(), None, None, true);

        assert_eq!(
            registry.ids(),
            vec![
                "drive", "failing", "gmail", "notion", "photos", "stub", "telegram", "whatsapp"
            ],
        );
        assert!(registry.get("failing").is_some());
        assert!(registry.get("stub").is_some());
    }

    /// Without the sealing key both real rows report the deployment problem in
    /// their own row — the pod logged exactly this on first boot — instead of
    /// panicking on boot or 500ing the whole settings screen.
    #[tokio::test]
    async fn an_unconfigured_key_is_reported_in_each_real_row() {
        let registry = build_registry(lazy_pool(), None, None, false);

        let Json(summaries) = list_connectors(State(test_state(registry)), fake_user()).await;

        assert_eq!(summaries.len(), 2);
        let photos = summaries.iter().find(|row| row.id == "photos").unwrap();
        assert!(photos.status.is_none());
        let error = photos.error.as_deref().unwrap_or_default();
        assert!(
            error.contains(crate::secrets::KEY_ENV),
            "the row has to name the missing key, got: {error}"
        );
        assert!(error.contains("cannot be opened"), "got: {error}");

        let whatsapp = summaries.iter().find(|row| row.id == "whatsapp").unwrap();
        assert!(whatsapp.status.is_none());
        let error = whatsapp.error.as_deref().unwrap_or_default();
        assert!(
            error.contains(crate::secrets::KEY_ENV),
            "the WhatsApp row has to name the missing key, got: {error}"
        );
    }

    #[tokio::test]
    async fn placeholder_connectors_report_disconnected_not_error() {
        let mut registry = Registry::new();
        registry.register(PlaceholderConnector::new(
            "gmail",
            "Gmail",
            &["gmail.readonly"],
        ));

        let Json(summary) = connector_status(
            State(test_state(registry)),
            fake_user(),
            Path("gmail".to_owned()),
        )
        .await
        .unwrap();

        assert_eq!(summary.status, Some(WireStatus::Disconnected));
        assert!(summary.error.is_none());
    }

    // ---------------------------------------------------------------------
    // Ticket B4: disconnect. These need real rows — the claim under test is
    // that a stored token is gone and the other connector's row survived, and
    // neither can be observed on a lazy pool.
    // ---------------------------------------------------------------------

    /// Same skip-or-fail contract as `tests/common`: a laptop without Postgres
    /// skips loudly, CI (`STRANDS_TEST_REQUIRE_DB=1`) fails instead of skipping.
    async fn pool_or_skip(test_name: &str) -> Option<PgPool> {
        let url = std::env::var("DATABASE_URL")
            .ok()
            .filter(|url| !url.trim().is_empty());
        let Some(url) = url else {
            eprintln!(
                "skipping {test_name}: DATABASE_URL is not set (no live Postgres on this host)"
            );
            return None;
        };

        let required = std::env::var("STRANDS_TEST_REQUIRE_DB").is_ok_and(|value| value == "1");

        let pool = match PgPoolOptions::new()
            .acquire_timeout(StdDuration::from_secs(3))
            .connect(&url)
            .await
        {
            Ok(pool) => pool,
            Err(error) if required => {
                panic!("STRANDS_TEST_REQUIRE_DB is set but Postgres is unreachable: {error}")
            }
            Err(error) => {
                eprintln!("skipping {test_name}: cannot reach DATABASE_URL ({error})");
                return None;
            }
        };

        crate::db::MIGRATOR
            .run(&pool)
            .await
            .expect("apply migrations");
        Some(pool)
    }

    fn db_key() -> Arc<SecretKey> {
        Arc::new(SecretKey::from_bytes(&[9u8; 32]).expect("a 32-byte key"))
    }

    /// A signed-in account, and the cookie the real router authenticates it by.
    async fn db_account(pool: &PgPool) -> (Uuid, String) {
        let id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, google_sub, email, name) VALUES ($1, $2, $3, $4)")
            .bind(id)
            .bind(format!("disconnect-sub-{id}"))
            .bind(format!("disconnect-{id}@example.test"))
            .bind("Disconnect Test")
            .execute(pool)
            .await
            .expect("insert user");
        sqlx::query("INSERT INTO sessions (id, user_id, expires_at) VALUES ($1, $2, $3)")
            .bind(session_id)
            .bind(id)
            .bind(Utc::now() + Duration::hours(1))
            .execute(pool)
            .await
            .expect("insert session");
        (id, format!("strands_session={session_id}"))
    }

    /// The real router over a real pool: the disconnect route is what the spec
    /// names, so the tests drive the route, not the handler function.
    fn db_app(pool: &PgPool, key: Arc<SecretKey>) -> Router {
        let state = AppState::new(
            pool.clone(),
            AuthConfig {
                client_id: "unused-client-id".into(),
                issuer: "https://identity.example".into(),
                public_base_url: "https://test.example".into(),
                cookie_secure: true,
                session_ttl_days: 30,
            },
            Arc::new(NoopIdentity),
            Arc::new(build_registry(pool.clone(), Some(key.clone()), None, false)),
            Arc::new(NeverSummarized),
        )
        .with_secrets(key);
        crate::app::router(state)
    }

    /// One connector row in the state `connect` leaves behind, plus the sealed
    /// token row a pick reads — exactly what a disconnect has to undo.
    async fn connected_row(pool: &PgPool, user: Uuid, kind: &str, key: &SecretKey) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO connectors (id, user_id, kind, status) VALUES ($1, $2, $3, 'connected')",
        )
        .bind(id)
        .bind(user)
        .bind(kind)
        .execute(pool)
        .await
        .expect("insert connector");

        let sealed = key.seal(user, b"ya29.disconnect-test").expect("seal token");
        sqlx::query(
            "INSERT INTO connector_secrets (connector_id, user_id, ciphertext, key_version)
             VALUES ($1, $2, $3, 2)",
        )
        .bind(id)
        .bind(user)
        .bind(sealed)
        .execute(pool)
        .await
        .expect("insert sealed token");

        id
    }

    async fn sealed_rows(pool: &PgPool, connector_id: Uuid) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM connector_secrets WHERE connector_id = $1")
            .bind(connector_id)
            .fetch_one(pool)
            .await
            .expect("count sealed rows")
    }

    async fn row_status(pool: &PgPool, connector_id: Uuid) -> String {
        sqlx::query_scalar("SELECT status::text FROM connectors WHERE id = $1")
            .bind(connector_id)
            .fetch_one(pool)
            .await
            .expect("read connector status")
    }

    async fn post_disconnect(app: &Router, cookie: &str) -> axum::http::Response<Body> {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/connectors/photos/disconnect")
                    .header(axum::http::header::COOKIE, cookie)
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("router response")
    }

    async fn summary_body(response: axum::http::Response<Body>) -> ConnectorSummary {
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("read body");
        serde_json::from_slice(&bytes).expect("the body is a connector summary")
    }

    /// B4 acceptance: disconnecting Google Photos deletes the sealed token, the
    /// row reads Not connected, and no other connector's rows are touched.
    #[tokio::test]
    async fn disconnect_clears_photos_token() {
        let Some(pool) = pool_or_skip("connectors::tests::disconnect_clears_photos_token").await
        else {
            return;
        };
        let (user, cookie) = db_account(&pool).await;
        let key = db_key();
        let app = db_app(&pool, Arc::clone(&key));

        let photos = connected_row(&pool, user, crate::photos::CONNECTOR_KIND, key.as_ref()).await;
        let whatsapp = connected_row(&pool, user, "whatsapp", key.as_ref()).await;

        let response = post_disconnect(&app, &cookie).await;
        assert_eq!(response.status(), StatusCode::OK, "disconnect answers 200");

        let summary = summary_body(response).await;
        assert_eq!(summary.id, "photos");
        assert_eq!(summary.status, Some(WireStatus::Disconnected));
        assert!(
            summary.error.is_none(),
            "a clean revoke has nothing to report: {:?}",
            summary.error
        );

        assert_eq!(
            sealed_rows(&pool, photos).await,
            0,
            "the sealed Google Photos token must be gone"
        );
        assert_eq!(row_status(&pool, photos).await, "disconnected");
        let last_error: Option<String> =
            sqlx::query_scalar("SELECT last_error FROM connectors WHERE id = $1")
                .bind(photos)
                .fetch_one(&pool)
                .await
                .expect("read last_error");
        assert!(last_error.is_none(), "the purge clears last_error too");

        // The other connector is untouched: its row still says connected and
        // its own sealed row is still there.
        assert_eq!(row_status(&pool, whatsapp).await, "connected");
        assert_eq!(sealed_rows(&pool, whatsapp).await, 1);
    }

    /// B4 acceptance: a second disconnect is a no-op — same body, nothing left
    /// to purge, no error.
    #[tokio::test]
    async fn second_disconnect_is_a_no_op() {
        let Some(pool) = pool_or_skip("connectors::tests::second_disconnect_is_a_no_op").await
        else {
            return;
        };
        let (user, cookie) = db_account(&pool).await;
        let key = db_key();
        let app = db_app(&pool, Arc::clone(&key));
        let photos = connected_row(&pool, user, crate::photos::CONNECTOR_KIND, key.as_ref()).await;

        let first = post_disconnect(&app, &cookie).await;
        assert_eq!(first.status(), StatusCode::OK);
        drop(summary_body(first).await);

        let second = post_disconnect(&app, &cookie).await;
        assert_eq!(
            second.status(),
            StatusCode::OK,
            "disconnecting again is not an error"
        );
        let summary = summary_body(second).await;
        assert_eq!(summary.status, Some(WireStatus::Disconnected));
        assert!(summary.error.is_none(), "got: {:?}", summary.error);

        assert_eq!(sealed_rows(&pool, photos).await, 0);
        assert_eq!(row_status(&pool, photos).await, "disconnected");
    }
}
