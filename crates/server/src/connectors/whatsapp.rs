//! The WhatsApp connector row (T28).
//!
//! Second real connector after Google Photos, and the one the spec's §7.9 bridge
//! describes. The row answers one question — is this account's phone linked
//! right now? — from the live session first and the stored connector row
//! second, so a green row can never mean "a previous process was linked".
//!
//! Three deployment facts are reported rather than hidden:
//!
//! * the link is **unofficial**: WhatsApp's ToS forbids third-party clients and
//!   using one can get the number banned. The disclosure text lives in
//!   `strands-api-types::WHATSAPP_DISCLOSURE` and the settings row renders it
//!   verbatim next to the status chip.
//! * with no `STRANDS_SECRET_KEY` there is no cipher, so no session can be
//!   stored: the row says exactly that instead of offering a link that cannot
//!   work.
//! * a link that the process no longer holds (a restart drops the in-memory
//!   transport) is `NeedsSignIn`, never `Connected`: the phone must be paired
//!   again, and the row says so by being a sign-in row.

use std::sync::Arc;

use sqlx::PgPool;
use strands_core::ConnectorStatus;
use strands_core::connector::{
    Connector, ConnectorError, ConnectorFuture, ConnectorManifest, ImportItemResult, PreviewItem,
};
use uuid::Uuid;

use crate::whatsapp::bridge::{LinkState, Registry as SessionRegistry};
use crate::whatsapp::store::CONNECTOR_KIND;

/// The id the settings screen keys the WhatsApp panel off.
pub const ID: &str = "whatsapp";

/// Reads one user's WhatsApp link state out of the live sessions and the store.
pub struct WhatsAppConnector {
    pool: PgPool,
    /// `None` when the deploy has no connector key: the link cannot exist, and
    /// the row reports that rather than pretending it is merely signed out.
    sessions: Option<Arc<SessionRegistry>>,
}

impl WhatsAppConnector {
    pub fn new(pool: PgPool, sessions: Option<Arc<SessionRegistry>>) -> Self {
        Self { pool, sessions }
    }
}

impl Connector for WhatsAppConnector {
    fn manifest(&self) -> ConnectorManifest {
        // No OAuth scopes: the phone grants a device session, not API scopes.
        ConnectorManifest {
            id: ID,
            label: "WhatsApp",
            scopes: &[],
        }
    }

    fn status<'a>(
        &'a self,
        user: Uuid,
    ) -> ConnectorFuture<'a, Result<ConnectorStatus, ConnectorError>> {
        Box::pin(async move {
            let Some(sessions) = self.sessions.as_ref() else {
                return Err(ConnectorError(format!(
                    "{} is unset or not 32 base64 bytes, so no WhatsApp session can be stored",
                    crate::secrets::KEY_ENV
                )));
            };

            // The live session is the only thing that can honestly say
            // "linked": the stored row outlives it across a restart.
            if let Some(session) = sessions.session(user) {
                return Ok(match session.state() {
                    LinkState::Linked => ConnectorStatus::Connected,
                    LinkState::AwaitingScan { .. } => ConnectorStatus::NeedsSignIn,
                    LinkState::Unlinked => ConnectorStatus::Disconnected,
                    LinkState::Failed { .. } => ConnectorStatus::Error,
                });
            }

            let row: Option<(String, Option<String>)> = sqlx::query_as(
                "SELECT status::text, last_error FROM connectors \
                  WHERE user_id = $1 AND kind = $2",
            )
            .bind(user)
            .bind(CONNECTOR_KIND)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| ConnectorError(format!("read the WhatsApp connector row: {error}")))?;

            let Some((status, last_error)) = row else {
                return Ok(ConnectorStatus::NeedsSignIn);
            };

            match status.as_str() {
                // The phone is still linked upstream, but this process holds no
                // session: pairing again is the only way back, so say that.
                "connected" => Ok(ConnectorStatus::NeedsSignIn),
                "error" => {
                    Err(ConnectorError(last_error.unwrap_or_else(|| {
                        "the WhatsApp bridge reported an error".to_owned()
                    })))
                }
                "needs_sign_in" => Ok(ConnectorStatus::NeedsSignIn),
                _ => Ok(ConnectorStatus::Disconnected),
            }
        })
    }

    /// Pairing is a two-step conversation with the phone (the QR has to be
    /// rendered, then scanned), so it cannot be a bodyless trait call: the row's
    /// own buttons drive `POST /api/v1/whatsapp/pair`.
    fn auth(&self, _user: Uuid) -> Result<(), ConnectorError> {
        Err(ConnectorError(
            "pairing needs the QR code: press Connect on this row".to_owned(),
        ))
    }

    /// Pairing/importing is driven by the row's own Sync control
    /// (`POST /api/v1/whatsapp/sync`), not the generic trait path — same
    /// refusal `sync` used to give.
    fn preview<'a>(
        &'a self,
        _user: Uuid,
    ) -> ConnectorFuture<'a, Result<Vec<PreviewItem>, ConnectorError>> {
        Box::pin(async {
            Err(ConnectorError(
                "WhatsApp imports through the row's Sync control".to_owned(),
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
                "WhatsApp imports through the row's Sync control".to_owned(),
            ))
        })
    }

    fn disconnect<'a>(&'a self, _user: Uuid) -> ConnectorFuture<'a, Result<(), ConnectorError>> {
        Box::pin(async {
            Err(ConnectorError(
                "unlinking a phone is asynchronous: press Disconnect on this row".to_owned(),
            ))
        })
    }
}
