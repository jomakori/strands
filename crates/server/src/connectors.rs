//! Connector settings endpoints: `GET /api/v1/connectors` and
//! `GET /api/v1/connectors/{id}/status` (spec §7.7, T13).
//!
//! Both handlers go straight through [`strands_core::Registry`]: a connector
//! whose own status check fails still answers with its own row (list) or its
//! own body (single lookup) — never a 500 that takes the rest down with it.

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::get;
use strands_api_types::{ConnectorStatus as WireStatus, ConnectorSummary};
use strands_core::ConnectorStatus;
use strands_core::connector::ConnectorReport;

use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/connectors", get(list_connectors))
        .route("/api/v1/connectors/{id}/status", get(connector_status))
}

/// Every registered connector, in the registry's stable id order.
pub async fn list_connectors(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> Json<Vec<ConnectorSummary>> {
    Json(
        state
            .registry
            .reports()
            .into_iter()
            .map(to_summary)
            .collect(),
    )
}

/// One connector's row, if and only if that id is registered.
pub async fn connector_status(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<ConnectorSummary>, AppError> {
    let connector = state.registry.get(&id).ok_or(AppError::NotFound)?;
    let manifest = connector.manifest();
    let (status, error) = match connector.status() {
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

    // Mirrors `app::tests::test_state`, plus the registry this module needs: a
    // router is cheap to build from a lazy pool, so these tests never touch Postgres.
    fn test_state(registry: Registry) -> AppState {
        AppState::new(
            PgPool::connect_lazy("postgres://strands@localhost/strands").unwrap(),
            AuthConfig {
                client_id: "client-id".into(),
                client_secret: "client-secret".into(),
                public_base_url: "http://localhost:8080".into(),
                cookie_secure: false,
                session_ttl_days: 30,
                authorization_endpoint: "https://accounts.example/consent".into(),
                token_endpoint: "https://accounts.example/token".into(),
            },
            Arc::new(NoopIdentity),
            Arc::new(registry),
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
