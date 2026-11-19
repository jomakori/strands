//! The `whatsapp-rust` transport behind the link.
//!
//! One [`LiveSession`] per account. It owns a `whatsapp-rust` client on a
//! background task and keeps the pairing surface (QR / pair code) plus the
//! chats it has seen in memory.
//!
//! Deliberate properties of this layer:
//!
//! * **Read-only.** No send path exists here. The type implements only
//!   [`WhatsAppSession`], which has no method that transmits a message, and
//!   the inbound handler copies text out of the event and nothing else.
//! * **Allowlist-blind.** Filtering by chat lives in `store::write_messages`
//!   (one place, exercised by tests). This layer buffers what arrives; a chat
//!   that is not allowlisted is dropped before it can become a note.
//! * **Pairing is server-driven.** `open` returns as soon as the client is
//!   spawned, with [`LinkState::AwaitingScan`]; the QR / pair code arrives on
//!   a callback and is surfaced through `state()`.
//!
//! ## Why the session is not durable
//!
//! `whatsapp-rust`'s only durable backend is `whatsapp-rust-sqlite-storage`,
//! which pins `libsqlite3-sys ^0.37`. `sqlx` (already this crate's Postgres
//! driver) resolves `sqlx-sqlite` and its `libsqlite3-sys 0.30`, and cargo
//! permits exactly one package per `links = "sqlite3"` in a workspace — the
//! two cannot coexist (`cargo` fails resolution, not link time). Restoring the
//! device that way would mean dropping `sqlite` from sqlx, which no crate in
//! this workspace asks for but the lock still carries; that is a dependency
//! change for the whole server, not a transport detail.
//!
//! So this layer uses the crate's in-memory backend and `park` returns
//! `None`: a restart drops the link and the user re-pairs. That is a real
//! limitation, stated here rather than hidden — the moment the workspace can
//! carry `whatsapp-rust-sqlite-storage`, `park`/`open` are the only two
//! functions that change.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use strands_core::UserId;
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore::store::InMemoryBackend;

use super::bridge::{
    BufferedMessage, ChatSummary, Jid, LinkState, MediaKind, PairingCode, SessionFactory,
    WhatsAppError, WhatsAppSession,
};
use crate::auth::identity::BoxFuture;

/// Bound on the in-memory inbox. A phone that syncs years of history at once
/// must not grow the process without limit; the oldest entries are dropped
/// first, because `peek`/`drain` consume the buffer from the front.
const BUFFER_LIMIT: usize = 5_000;

/// How long a shutdown may take before the session is abandoned. Parking or
/// unlinking must not hang a request: the device is already gone by then.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// State shared with the client's event callbacks.
///
/// `std::sync::Mutex` on purpose: every critical section is a handful of
/// clones, and none is held across an await.
struct Shared {
    state: Mutex<LinkState>,
    chats: Mutex<BTreeMap<Jid, ChatSummary>>,
    buffer: Mutex<Vec<BufferedMessage>>,
}

impl Shared {
    fn new(state: LinkState) -> Self {
        Self {
            state: Mutex::new(state),
            chats: Mutex::new(BTreeMap::new()),
            buffer: Mutex::new(Vec::new()),
        }
    }

    fn state(&self) -> LinkState {
        self.state
            .lock()
            .expect("link state is never poisoned")
            .clone()
    }

    /// Drive the committed state machine. Late events from a dead connection
    /// are rejected there (a late QR must not resurrect an unlinked device),
    /// so an error here is expected noise, not a failure to report.
    fn mutate(&self, event: impl FnOnce(&mut LinkState) -> Result<(), super::bridge::LinkError>) {
        let mut state = self.state.lock().expect("link state is never poisoned");
        let _ = event(&mut state);
    }

    fn record(&self, ctx: &MessageContext) {
        let Ok(chat) = Jid::parse(&ctx.info.source.chat.to_string()) else {
            // A JID this server cannot address can never be allowlisted, so
            // dropping it loses nothing an import could have used.
            return;
        };
        let display_name = {
            let name = ctx.info.push_name.trim();
            (!name.is_empty()).then(|| name.to_owned())
        };
        if let Ok(mut chats) = self.chats.lock() {
            chats.entry(chat.clone()).or_insert_with(|| ChatSummary {
                is_group: chat.is_group(),
                jid: chat.clone(),
                display_name,
            });
        }

        let message = BufferedMessage {
            jid: chat,
            message_id: ctx.info.id.clone(),
            sent_at: ctx.info.timestamp,
            from_me: ctx.info.source.is_from_me,
            sender: Some(ctx.info.source.sender.to_string()),
            body: ctx.message.text_content().unwrap_or_default().to_owned(),
            media: media_kind(ctx),
        };
        if let Ok(mut buffer) = self.buffer.lock() {
            let excess = (buffer.len() + 1).saturating_sub(BUFFER_LIMIT);
            buffer.drain(..excess);
            buffer.push(message);
        }
    }
}

/// Classify an inbound attachment without downloading it. `text_content`
/// covers captions, so a captioned photo still imports as text; this only
/// names what the import report counted as skipped.
fn media_kind(ctx: &MessageContext) -> Option<MediaKind> {
    let message = &ctx.message;
    if message.image_message.is_set() {
        Some(MediaKind::Image)
    } else if message.video_message.is_set() || message.ptv_message.is_set() {
        Some(MediaKind::Video)
    } else if message.audio_message.is_set() {
        Some(MediaKind::Audio)
    } else if message.document_message.is_set() {
        Some(MediaKind::Document)
    } else if message.sticker_message.is_set() {
        Some(MediaKind::Sticker)
    } else {
        None
    }
}

/// One account's live client.
pub struct LiveSession {
    shared: Arc<Shared>,
    handle: Mutex<Option<BotHandle>>,
}

impl WhatsAppSession for LiveSession {
    fn state(&self) -> LinkState {
        self.shared.state()
    }

    fn chats(&self) -> Vec<ChatSummary> {
        self.shared
            .chats
            .lock()
            .expect("chat map is never poisoned")
            .values()
            .cloned()
            .collect()
    }

    fn peek(&self) -> Vec<BufferedMessage> {
        self.shared
            .buffer
            .lock()
            .expect("inbox is never poisoned")
            .clone()
    }

    fn drain(&self) -> Vec<BufferedMessage> {
        let mut buffer = self.shared.buffer.lock().expect("inbox is never poisoned");
        std::mem::take(&mut *buffer)
    }

    fn sync(&self) -> BoxFuture<'_, Result<(), WhatsAppError>> {
        Box::pin(async move {
            match self.shared.state() {
                LinkState::Linked => {
                    // History is pushed by the phone and the library
                    // acknowledges/downloads it on its own; there is nothing to
                    // request. "Sync now" therefore means "drain what
                    // arrived", which the route does right after this returns.
                    Ok(())
                }
                _ => Err(WhatsAppError::NotLinked),
            }
        })
    }

    fn unlink(&self) -> BoxFuture<'_, Result<(), WhatsAppError>> {
        Box::pin(async move {
            let Some(handle) = self.handle.lock().expect("handle is never poisoned").take() else {
                return Err(WhatsAppError::NotLinked);
            };
            // Tell the phone first — this is what removes the device from its
            // linked-devices list. Local teardown happens regardless, so a
            // failed request cannot leave a half-alive session behind.
            handle.client().logout().await;
            let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, handle).await;
            self.shared.mutate(LinkState::on_unlinked);
            Ok(())
        })
    }

    fn park(&self) -> BoxFuture<'_, Result<Option<Vec<u8>>, WhatsAppError>> {
        Box::pin(async move {
            let handle = self.handle.lock().expect("handle is never poisoned").take();
            if let Some(handle) = handle {
                let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, handle.shutdown()).await;
            }
            self.shared.mutate(LinkState::on_unlinked);
            // Nothing to hand back: see the module note on `libsqlite3-sys`.
            Ok(None)
        })
    }
}

/// Opens [`LiveSession`]s.
pub struct LiveFactory {
    pair_phone: Option<String>,
}

impl Default for LiveFactory {
    fn default() -> Self {
        Self::from_env()
    }
}

impl LiveFactory {
    /// Pairing by code is optional; QR always works.
    pub fn new(pair_phone: Option<String>) -> Self {
        Self {
            pair_phone: pair_phone
                .map(|phone| phone.trim().to_owned())
                .filter(|phone| !phone.is_empty()),
        }
    }

    /// `STRANDS_WHATSAPP_PAIR_PHONE` enables pairing by code as well as QR.
    pub fn from_env() -> Self {
        Self::new(std::env::var("STRANDS_WHATSAPP_PAIR_PHONE").ok())
    }
}

impl SessionFactory for LiveFactory {
    fn open(
        &self,
        _user_id: UserId,
        restore: Option<Vec<u8>>,
    ) -> BoxFuture<'_, Result<Arc<dyn WhatsAppSession>, WhatsAppError>> {
        Box::pin(async move {
            // A parked session never exists in this build (see the module note
            // on `libsqlite3-sys`), so there is nothing to restore: the account
            // pairs again. Bytes from an older build are dropped here rather
            // than half-honoured.
            let _ = restore;

            let shared = Arc::new(Shared::new(LinkState::AwaitingScan {
                qr: None,
                pair_code: None,
            }));

            let qr = Arc::clone(&shared);
            let pair = Arc::clone(&shared);
            let connected = Arc::clone(&shared);
            let logged_out = Arc::clone(&shared);
            let inbox = Arc::clone(&shared);

            let mut builder = Bot::builder()
                .with_backend(InMemoryBackend::new())
                .on_qr_code(move |code, timeout| {
                    let shared = Arc::clone(&qr);
                    async move {
                        shared.mutate(|s| {
                            s.on_qr(PairingCode {
                                value: code,
                                expires_in_seconds: timeout.as_secs(),
                            })
                        })
                    }
                })
                .on_pair_code(move |code, timeout| {
                    let shared = Arc::clone(&pair);
                    async move {
                        shared.mutate(|s| {
                            s.on_pair_code(PairingCode {
                                value: code,
                                expires_in_seconds: timeout.as_secs(),
                            })
                        })
                    }
                })
                .on_connected(move |_client| {
                    let shared = Arc::clone(&connected);
                    async move { shared.mutate(LinkState::on_paired) }
                })
                .on_logged_out(move |_info| {
                    let shared = Arc::clone(&logged_out);
                    async move { shared.mutate(LinkState::on_unlinked) }
                })
                // The only place a message is ever read, and it only copies
                // text out of the event: no reply is ever sent from here.
                .on_message(move |ctx| {
                    let shared = Arc::clone(&inbox);
                    async move { shared.record(&ctx) }
                });

            if let Some(phone) = &self.pair_phone {
                builder = builder.with_pair_code(PairCodeOptions {
                    phone_number: phone.clone(),
                    ..Default::default()
                });
            }

            let bot = builder
                .build()
                .await
                .map_err(|e| WhatsAppError::Transport(format!("client build: {e}")))?;
            let handle = bot.spawn();

            Ok(Arc::new(LiveSession {
                shared,
                handle: Mutex::new(Some(handle)),
            }) as Arc<dyn WhatsAppSession>)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jid(raw: &str) -> Jid {
        Jid::parse(raw).expect("test jid parses")
    }

    fn message(id: &str) -> BufferedMessage {
        BufferedMessage {
            jid: jid("15551230000@s.whatsapp.net"),
            message_id: id.to_owned(),
            sent_at: chrono::Utc::now(),
            from_me: false,
            sender: None,
            body: "x".into(),
            media: None,
        }
    }

    #[test]
    fn pairing_surfaces_both_codes_while_scanning() {
        let shared = Shared::new(LinkState::AwaitingScan {
            qr: None,
            pair_code: None,
        });
        shared.mutate(|s| {
            s.on_qr(PairingCode {
                value: "qr-1".into(),
                expires_in_seconds: 20,
            })
        });
        shared.mutate(|s| {
            s.on_pair_code(PairingCode {
                value: "1234-5678".into(),
                expires_in_seconds: 60,
            })
        });
        match shared.state() {
            LinkState::AwaitingScan { qr, pair_code } => {
                assert_eq!(qr.expect("qr kept").value, "qr-1");
                let code = pair_code.expect("pair code kept");
                assert_eq!(code.value, "1234-5678");
                assert_eq!(code.expires_in_seconds, 60);
            }
            other => panic!("expected a scan in progress, got {other:?}"),
        }

        // Once linked, a late QR refresh must not drag the state backwards.
        shared.mutate(LinkState::on_paired);
        shared.mutate(|s| {
            s.on_qr(PairingCode {
                value: "qr-2".into(),
                expires_in_seconds: 20,
            })
        });
        assert_eq!(shared.state(), LinkState::Linked);
    }

    #[test]
    fn inbox_is_bounded() {
        let shared = Shared::new(LinkState::Linked);
        let mut buffer = shared.buffer.lock().expect("inbox is never poisoned");
        for n in 0..BUFFER_LIMIT {
            buffer.push(message(&n.to_string()));
        }
        // What `record` does for the next message: evict from the front first.
        let excess = (buffer.len() + 1).saturating_sub(BUFFER_LIMIT);
        buffer.drain(..excess);
        buffer.push(message("last"));
        assert_eq!(buffer.len(), BUFFER_LIMIT);
        assert_eq!(buffer.last().expect("last").message_id, "last");
    }

    #[test]
    fn pair_phone_is_trimmed_and_blank_is_no_phone() {
        assert_eq!(
            LiveFactory::new(Some(" 15551230000 ".into()))
                .pair_phone
                .as_deref(),
            Some("15551230000")
        );
        assert!(LiveFactory::new(Some("   ".into())).pair_phone.is_none());
        assert!(LiveFactory::new(None).pair_phone.is_none());
    }
}
