//! The Google Photos connector (T27).
//!
//! It is the first *real* connector in the registry: no stub, no placeholder.
//! Its job on the settings screen is to answer one question truthfully — can
//! this user start a pick right now? — and it answers it from the same two
//! pieces of state the pick route uses, so a "Connected" row can never mean
//! something the pick would then refuse.
//!
//! Two failure modes are reported as the row's own error rather than a panic or
//! an empty row, because both are deployment facts the operator can act on:
//!
//! * `STRANDS_SECRET_KEY` is unset or unusable — stored tokens cannot be opened.
//!   The deployed pod logged exactly this on first boot, so it must stay a
//!   reported state, not a crash.
//! * a stored token exists but will not decrypt (a rotated key) — the row says
//!   so instead of pretending the connector is fine.

use std::sync::Arc;

use sqlx::PgPool;
use strands_core::ConnectorStatus;
use strands_core::connector::{
    Connector, ConnectorError, ConnectorFuture, ConnectorManifest, ImportItemResult, PreviewItem,
};
use uuid::Uuid;

use crate::photos::CONNECTOR_KIND;
use crate::secrets::{self, SecretKey};

/// Scopes a pick needs: the Picker API is the only photos path (no library
/// listing), so read-only access to picked media items is the whole surface.
pub(super) const SCOPES: &[&str] =
    &["https://www.googleapis.com/auth/photospicker.mediaitems.readonly"];

/// Reads one user's Google Photos connection state out of the store.
///
/// Holds the pool and the sealing key, not a token: the token is per user and
/// read on demand, so one instance serves the whole process.
pub struct PhotosConnector {
    pool: PgPool,
    key: Option<Arc<SecretKey>>,
}

impl PhotosConnector {
    pub fn new(pool: PgPool, key: Option<Arc<SecretKey>>) -> Self {
        Self { pool, key }
    }
}

impl Connector for PhotosConnector {
    fn manifest(&self) -> ConnectorManifest {
        ConnectorManifest {
            id: "photos",
            label: "Google Photos",
            scopes: SCOPES,
        }
    }

    fn status<'a>(
        &'a self,
        user: Uuid,
    ) -> ConnectorFuture<'a, Result<ConnectorStatus, ConnectorError>> {
        Box::pin(async move {
            let Some(key) = self.key.as_ref() else {
                return Err(ConnectorError(format!(
                    "{} is not set, so stored Google Photos tokens cannot be opened",
                    secrets::KEY_ENV
                )));
            };

            let row: Option<(String, Option<String>)> = sqlx::query_as(
                "SELECT c.status::text, c.last_error FROM connectors c
                  WHERE c.user_id = $1 AND c.kind = $2",
            )
            .bind(user)
            .bind(CONNECTOR_KIND)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| {
                ConnectorError(format!("read the Google Photos connector row: {error}"))
            })?;

            let Some((status, last_error)) = row else {
                // No row yet: the connector is registered and offered, the user
                // has simply never been through the picker.
                return Ok(ConnectorStatus::NeedsSignIn);
            };

            match status.as_str() {
                "connected" => {
                    match secrets::access_token(&self.pool, key.as_ref(), user, CONNECTOR_KIND)
                        .await
                    {
                        Ok(Some(_)) => Ok(ConnectorStatus::Connected),
                        // The row claims a connection the store cannot back: report
                        // it as an error rather than a green row a pick would break.
                        Ok(None) => Err(ConnectorError(
                            "the connector row says connected but no stored token was found"
                                .to_owned(),
                        )),
                        Err(error) => Err(ConnectorError(format!(
                            "the stored Google Photos token cannot be opened ({error:?}); \
                         was {} rotated?",
                            secrets::KEY_ENV
                        ))),
                    }
                }
                "error" => {
                    Err(ConnectorError(last_error.unwrap_or_else(|| {
                        "Google Photos reported an error".to_owned()
                    })))
                }
                "needs_sign_in" => Ok(ConnectorStatus::NeedsSignIn),
                _ => Ok(ConnectorStatus::Disconnected),
            }
        })
    }

    /// There is no separate auth handshake: a pick *is* the sign-in. Refuse
    /// rather than return `Ok(())` and let the UI call it connected.
    fn auth(&self, _user: Uuid) -> Result<(), ConnectorError> {
        Err(ConnectorError(
            "Google Photos has no separate sign-in step; start a pick from Settings".to_owned(),
        ))
    }

    /// A pick *is* the preview+import step (the picker UI shows what is
    /// available and importing it happens in the same round trip through
    /// `create_session`/`poll_session`), so the generic trait path has
    /// nothing to preview here — same refusal `sync` used to give.
    fn preview<'a>(
        &'a self,
        _user: Uuid,
    ) -> ConnectorFuture<'a, Result<Vec<PreviewItem>, ConnectorError>> {
        Box::pin(async {
            Err(ConnectorError(
                "Google Photos only imports through a pick; there is nothing to preview".to_owned(),
            ))
        })
    }

    fn import<'a>(
        &'a self,
        _user: Uuid,
        _item_keys: Vec<String>,
    ) -> ConnectorFuture<'a, Result<Vec<ImportItemResult>, ConnectorError>> {
        Box::pin(async {
            Err(ConnectorError(
                "Google Photos only imports through a pick; there is nothing to sync".to_owned(),
            ))
        })
    }

    fn disconnect(&self, _user: Uuid) -> Result<(), ConnectorError> {
        Err(ConnectorError(
            "disconnecting Google Photos needs the account screen; it is not wired here".to_owned(),
        ))
    }
}
