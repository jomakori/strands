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
use uuid::Uuid;

use crate::ConnectorStatus;

/// A boxed, `Send` future. The connector trait returns this instead of an
/// `async fn` so `Box<dyn Connector>` stays object-safe while a real connector
/// (Photos) can still await the database inside `status`.
pub type ConnectorFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

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
///
/// Every method is user-scoped: one connector instance serves every account, so the
/// caller's id is what selects the row to read or write. A connector that is not
/// configured for the deployment (no key, no credentials) reports that as its own
/// [`ConnectorError`] rather than panicking or refusing to build.
pub trait Connector: Send + Sync {
    /// Static identity: id, label, scopes. Never fails.
    fn manifest(&self) -> ConnectorManifest;

    /// This user's current connection status, queried fresh on every call (no
    /// caching here). Async because a real connector reads it from the store.
    fn status<'a>(
        &'a self,
        user: Uuid,
    ) -> ConnectorFuture<'a, Result<ConnectorStatus, ConnectorError>>;

    /// Start or continue the auth flow for this user.
    fn auth(&self, user: Uuid) -> Result<(), ConnectorError>;

    /// Pull new data from the upstream service for this user.
    fn sync(&self, user: Uuid) -> Result<(), ConnectorError>;

    /// Revoke access and forget this user's local state.
    fn disconnect(&self, user: Uuid) -> Result<(), ConnectorError>;
}
