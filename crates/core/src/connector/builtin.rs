//! Built-in connectors: a stub that completes, a connector that always fails
//! (both exist purely to exercise [`super::Registry`]'s "one failure doesn't
//! block the others" guarantee), and a placeholder for connectors not yet
//! implemented (Gmail, Notion, Drive, Telegram — spec §7.7, T13).

use super::{Connector, ConnectorError, ConnectorFuture, ConnectorManifest};
use crate::ConnectorStatus;
use uuid::Uuid;

/// Always reports `Connected`. Exercises the happy path end to end — the
/// registry, the API, and the settings screen all need at least one connector
/// that just works.
pub struct StubConnector;

impl Connector for StubConnector {
    fn manifest(&self) -> ConnectorManifest {
        ConnectorManifest {
            id: "stub",
            label: "Stub",
            scopes: &["demo:read"],
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

    fn sync(&self, _user: Uuid) -> Result<(), ConnectorError> {
        Ok(())
    }

    fn disconnect(&self, _user: Uuid) -> Result<(), ConnectorError> {
        Ok(())
    }
}

/// Always fails. Exists so the registry and the settings screen have
/// something to prove isolation against: this connector's error must stay on
/// its own row.
pub struct FailingConnector;

impl Connector for FailingConnector {
    fn manifest(&self) -> ConnectorManifest {
        ConnectorManifest {
            id: "failing",
            label: "Failing Demo",
            scopes: &["demo:read"],
        }
    }

    fn status<'a>(
        &'a self,
        _user: Uuid,
    ) -> ConnectorFuture<'a, Result<ConnectorStatus, ConnectorError>> {
        Box::pin(async { Err(ConnectorError("upstream service is unreachable".to_owned())) })
    }

    fn auth(&self, _user: Uuid) -> Result<(), ConnectorError> {
        Err(ConnectorError("upstream service is unreachable".to_owned()))
    }

    fn sync(&self, _user: Uuid) -> Result<(), ConnectorError> {
        Err(ConnectorError("upstream service is unreachable".to_owned()))
    }

    fn disconnect(&self, _user: Uuid) -> Result<(), ConnectorError> {
        Err(ConnectorError("upstream service is unreachable".to_owned()))
    }
}

/// A connector kind with no real implementation yet (Gmail, Notion, Drive,
/// Telegram). Always reports `Disconnected` — never `Error` — so it occupies
/// its settings row without looking broken; every write operation is refused.
pub struct PlaceholderConnector {
    manifest: ConnectorManifest,
}

impl PlaceholderConnector {
    pub fn new(id: &'static str, label: &'static str, scopes: &'static [&'static str]) -> Self {
        Self {
            manifest: ConnectorManifest { id, label, scopes },
        }
    }
}

impl Connector for PlaceholderConnector {
    fn manifest(&self) -> ConnectorManifest {
        self.manifest
    }

    fn status<'a>(
        &'a self,
        _user: Uuid,
    ) -> ConnectorFuture<'a, Result<ConnectorStatus, ConnectorError>> {
        Box::pin(async { Ok(ConnectorStatus::Disconnected) })
    }

    fn auth(&self, _user: Uuid) -> Result<(), ConnectorError> {
        Err(ConnectorError(format!(
            "{} is not yet implemented",
            self.manifest.label
        )))
    }

    fn sync(&self, _user: Uuid) -> Result<(), ConnectorError> {
        Err(ConnectorError(format!(
            "{} is not yet implemented",
            self.manifest.label
        )))
    }

    fn disconnect(&self, _user: Uuid) -> Result<(), ConnectorError> {
        Err(ConnectorError(format!(
            "{} is not yet implemented",
            self.manifest.label
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Any id works: these connectors hold no per-user state.
    fn user() -> Uuid {
        Uuid::nil()
    }

    #[tokio::test]
    async fn stub_connector_completes() {
        assert_eq!(
            StubConnector.status(user()).await,
            Ok(ConnectorStatus::Connected)
        );
    }

    #[tokio::test]
    async fn failing_connector_reports_its_own_error() {
        assert!(FailingConnector.status(user()).await.is_err());
    }

    #[tokio::test]
    async fn placeholder_connector_is_disconnected_not_errored() {
        let gmail = PlaceholderConnector::new("gmail", "Gmail", &["gmail.readonly"]);
        assert_eq!(
            gmail.status(user()).await,
            Ok(ConnectorStatus::Disconnected)
        );
        assert!(gmail.auth(user()).is_err());
    }
}
