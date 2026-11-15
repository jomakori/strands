//! Registry keyed by connector id (spec §7.7, T13).
//!
//! The registry's whole job is isolation: [`Registry::reports`] calls every
//! connector's `status()` independently and turns a failure into a row of its
//! own, so one connector going down can never take the others' rows with it.

use std::collections::BTreeMap;

use super::{Connector, ConnectorManifest};
use crate::ConnectorStatus;

/// One row of the settings screen / `GET /api/v1/connectors`: a connector's
/// manifest plus its freshly queried status (or the error querying it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorReport {
    pub manifest: ConnectorManifest,
    pub status: Option<ConnectorStatus>,
    pub error: Option<String>,
}

/// Connectors keyed by [`ConnectorManifest::id`]. `BTreeMap` for a stable,
/// deterministic row order (alphabetical by id) with no extra sort step.
#[derive(Default)]
pub struct Registry {
    connectors: BTreeMap<&'static str, Box<dyn Connector>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a connector under its own manifest id. Panics on a duplicate
    /// id: two connectors claiming the same id is a programming error, not a
    /// runtime condition to recover from.
    pub fn register(&mut self, connector: impl Connector + 'static) -> &mut Self {
        let id = connector.manifest().id;
        let previous = self.connectors.insert(id, Box::new(connector));
        assert!(previous.is_none(), "duplicate connector id {id}");
        self
    }

    pub fn get(&self, id: &str) -> Option<&dyn Connector> {
        self.connectors.get(id).map(|boxed| boxed.as_ref())
    }

    /// One report per registered connector, in id order. A connector whose
    /// `status()` fails still gets a row — with its own error — instead of
    /// aborting the whole list.
    pub fn reports(&self) -> Vec<ConnectorReport> {
        self.connectors
            .values()
            .map(|connector| match connector.status() {
                Ok(status) => ConnectorReport {
                    manifest: connector.manifest(),
                    status: Some(status),
                    error: None,
                },
                Err(error) => ConnectorReport {
                    manifest: connector.manifest(),
                    status: None,
                    error: Some(error.0),
                },
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::{FailingConnector, StubConnector};

    #[test]
    fn a_failing_connector_does_not_block_the_others() {
        let mut registry = Registry::new();
        registry.register(StubConnector);
        registry.register(FailingConnector);

        let reports = registry.reports();
        assert_eq!(reports.len(), 2, "both connectors get a row");

        let stub = reports
            .iter()
            .find(|report| report.manifest.id == "stub")
            .expect("stub connector reported");
        assert_eq!(stub.status, Some(ConnectorStatus::Connected));
        assert!(stub.error.is_none());

        let failing = reports
            .iter()
            .find(|report| report.manifest.id == "failing")
            .expect("failing connector still gets its own row");
        assert!(failing.status.is_none());
        assert!(failing.error.is_some(), "its own error, not a panic");
    }

    #[test]
    fn reports_are_in_stable_id_order() {
        let mut registry = Registry::new();
        registry.register(FailingConnector);
        registry.register(StubConnector);

        let ids: Vec<&str> = registry.reports().iter().map(|r| r.manifest.id).collect();
        assert_eq!(ids, vec!["failing", "stub"]);
    }

    #[test]
    #[should_panic(expected = "duplicate connector id")]
    fn registering_the_same_id_twice_panics() {
        let mut registry = Registry::new();
        registry.register(StubConnector);
        registry.register(StubConnector);
    }

    #[test]
    fn get_looks_up_by_manifest_id() {
        let mut registry = Registry::new();
        registry.register(StubConnector);
        assert!(registry.get("stub").is_some());
        assert!(registry.get("missing").is_none());
    }
}
