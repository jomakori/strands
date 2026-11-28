//! Connector plugin trait: manifest, status, auth, preview, import, disconnect,
//! scopes (spec §7.7, T13; select-then-import ledger, T33).
//!
//! A connector is a pluggable integration (Gmail, Notion, Drive, Telegram, …). The trait
//! is the only surface the registry and the API depend on: a connector that fails
//! `status` reports its own [`ConnectorError`] instead of taking any other connector
//! down with it — see [`Registry::reports`].

mod builtin;
mod registry;

pub use builtin::{FailingConnector, PlaceholderConnector, StubConnector};
pub use registry::{ConnectorReport, Registry};

use chrono::{DateTime, Utc};
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

/// One row of `preview`: something this connector can currently see — a
/// container or an item — that is a candidate for import, plus whether a
/// previous `import` call already landed it (T33).
///
/// `preview` is purely a read: it must never write the ledger, only consult
/// it to set `already_imported`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewItem {
    /// Stable identity for this item within the connector (a chat JID, a
    /// media item id, …). This is what `import` is called with and what the
    /// `connector_items` ledger dedupes on.
    pub item_key: String,
    /// Human-readable label for the selection screen.
    pub label: String,
    /// When this item was produced or last seen upstream.
    pub timestamp: DateTime<Utc>,
    /// Whether a previous `import` already wrote a strand for this item_key.
    pub already_imported: bool,
}

/// What `import` did with one requested item key (T33).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    /// A new strand was written and the ledger now holds this item_key.
    Imported,
    /// The ledger already held this item_key; nothing was written.
    SkippedAlreadyImported,
    /// Importing this item failed; the string is safe to show in a report row.
    Failed(String),
}

/// One selected item's outcome, returned from `import`, in request order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportItemResult {
    pub item_key: String,
    pub outcome: ImportOutcome,
}

/// A pluggable integration. Implementors own their own auth/preview/import/disconnect
/// logic; the registry and API only ever see this trait.
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

    /// Everything this connector can currently see for `user`: containers or
    /// items eligible for import, each flagged with whether a previous
    /// `import` already landed it (T33). Purely a read — must never write.
    fn preview<'a>(
        &'a self,
        user: Uuid,
    ) -> ConnectorFuture<'a, Result<Vec<PreviewItem>, ConnectorError>>;

    /// Import exactly the selected item keys: ones already in the ledger are
    /// skipped (never re-written), new ones are written — the ledger row and
    /// the strand insert happen in the same transaction, so a crash cannot
    /// leave one without the other. Returns one outcome per requested key, in
    /// request order (T33).
    fn import<'a>(
        &'a self,
        user: Uuid,
        item_keys: Vec<String>,
    ) -> ConnectorFuture<'a, Result<Vec<ImportItemResult>, ConnectorError>>;

    /// Revoke access and forget this user's local state.
    ///
    /// Async because the purge is a transaction: the stored material and the
    /// connector row's own status move together or not at all. Connecting
    /// nothing is not an error — a second `disconnect` is a no-op, so the
    /// caller can retry a failed revoke without inventing state first.
    fn disconnect<'a>(&'a self, user: Uuid) -> ConnectorFuture<'a, Result<(), ConnectorError>>;

    /// Old no-selection "sync everything" behaviour (spec §7.7), kept as a
    /// call site through `preview` + `import` rather than a second code path
    /// (T33): it imports every item `preview` currently reports.
    fn sync<'a>(
        &'a self,
        user: Uuid,
    ) -> ConnectorFuture<'a, Result<Vec<ImportItemResult>, ConnectorError>> {
        Box::pin(async move {
            let items = self.preview(user).await?;
            let item_keys = items.into_iter().map(|item| item.item_key).collect();
            self.import(user, item_keys).await
        })
    }
}

#[cfg(test)]
mod default_sync_tests {
    use std::sync::Mutex;

    use super::*;

    /// Records what it was called with so the test can assert on call order
    /// and arguments, not just the final result.
    #[derive(Default)]
    struct RecordingConnector {
        import_calls: Mutex<Vec<Vec<String>>>,
    }

    fn user() -> Uuid {
        Uuid::nil()
    }

    impl Connector for RecordingConnector {
        fn manifest(&self) -> ConnectorManifest {
            ConnectorManifest {
                id: "recording",
                label: "Recording",
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
            _user: Uuid,
        ) -> ConnectorFuture<'a, Result<Vec<PreviewItem>, ConnectorError>> {
            Box::pin(async {
                Ok(vec![
                    PreviewItem {
                        item_key: "a".to_owned(),
                        label: "A".to_owned(),
                        timestamp: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
                        already_imported: false,
                    },
                    PreviewItem {
                        item_key: "b".to_owned(),
                        label: "B".to_owned(),
                        timestamp: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
                        already_imported: true,
                    },
                ])
            })
        }

        fn import<'a>(
            &'a self,
            _user: Uuid,
            item_keys: Vec<String>,
        ) -> ConnectorFuture<'a, Result<Vec<ImportItemResult>, ConnectorError>> {
            self.import_calls.lock().unwrap().push(item_keys.clone());
            Box::pin(async move {
                Ok(item_keys
                    .into_iter()
                    .map(|item_key| ImportItemResult {
                        item_key,
                        outcome: ImportOutcome::Imported,
                    })
                    .collect())
            })
        }

        fn disconnect<'a>(
            &'a self,
            _user: Uuid,
        ) -> ConnectorFuture<'a, Result<(), ConnectorError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn default_sync_imports_exactly_the_previewed_keys() {
        let connector = RecordingConnector::default();

        let results = connector.sync(user()).await.expect("sync succeeds");

        assert_eq!(
            connector.import_calls.lock().unwrap().as_slice(),
            [vec!["a".to_owned(), "b".to_owned()]],
            "import was called once, with exactly the keys preview reported, in order"
        );
        assert_eq!(
            results
                .iter()
                .map(|r| r.item_key.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
    }
}
