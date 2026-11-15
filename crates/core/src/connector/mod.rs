//! Connector plugin trait: manifest, status, auth, sync, disconnect, scopes (spec §7.7, T13).
//!
//! A connector is a pluggable integration (Gmail, Notion, Drive, Telegram, …). The trait
//! is the only surface the registry and the API depend on: a connector that fails
//! `status` reports its own [`ConnectorError`] instead of taking any other connector
//! down with it — see [`Registry::reports`].

mod builtin;
mod registry;

pub use builtin::{FailingConnector, PlaceholderConnector, StubConnector};
pub use registry::{ConnectorReport, Registry};

use thiserror::Error;

use crate::ConnectorStatus;

/// Static identity and capabilities of a connector, independent of any user's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectorManifest {
    pub id: &'static str,
    pub label: &'static str,
    pub scopes: &'static [&'static str],
}

/// A connector operation's failure, safe to show in a settings row.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct ConnectorError(pub String);

/// A pluggable integration. Implementors own their own auth/sync/disconnect logic; the
/// registry and API only ever see this trait.
pub trait Connector: Send + Sync {
    /// Static identity: id, label, scopes. Never fails.
    fn manifest(&self) -> ConnectorManifest;

    /// Current connection status, queried fresh on every call (no caching here).
    fn status(&self) -> Result<ConnectorStatus, ConnectorError>;

    /// Start or continue the auth flow.
    fn auth(&self) -> Result<(), ConnectorError>;

    /// Pull new data from the upstream service.
    fn sync(&self) -> Result<(), ConnectorError>;

    /// Revoke access and forget local state.
    fn disconnect(&self) -> Result<(), ConnectorError>;
}
