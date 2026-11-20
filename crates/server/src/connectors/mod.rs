//! Connector settings endpoints: `GET /api/v1/connectors` and
//! `GET /api/v1/connectors/{id}/status` (spec §7.7, T13), plus this process's
//! connector set (T27).
//!
//! Both handlers go straight through [`strands_core::Registry`]: a connector
//! whose own status check fails still answers with its own row (list) or its
//! own body (single lookup) — never a 500 that takes the rest down with it.

pub mod ledger;
mod photos;
mod whatsapp;

pub use photos::PhotosConnector;
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
use strands_core::connector::{ConnectorReport, ImportOutcome};

use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::secrets::SecretKey;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/connectors", get(list_connectors))
        .route("/api/v1/connectors/{id}/status", get(connector_status))
        .route("/api/v1/connectors/{id}/preview", get(connector_preview))
        .route("/api/v1/connectors/{id}/import", post(connector_import))
        .route("/api/v1/connectors/{id}/history", get(connector_history))
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
    let manifest = connector.manifest();
    let (status, error) = match connector.status(user.id).await {
        Ok(status) => (Some(map_status(status)), None),
        Err(error) => (None, Some(error.to_string())),
    };

    Ok(Json(ConnectorSummary {
        id: manifest.id.to_owned(),
        label: manifest.label.to_owned(),
        scopes: manifest
            .scopes
            .iter()
            .map(|scope| scope.to_string())
            .collect(),
        status,
        error,
    }))
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

    use sqlx::PgPool;
    use strands_core::Registry;
    use strands_core::connector::{FailingConnector, PlaceholderConnector, StubConnector};
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
}
