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

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::DateTime;
use strands_core::UserId;
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore::store::InMemoryBackend;
use whatsapp_rust::wacore::types::events::LazyHistorySync;

use super::bridge::{
    BufferedMessage, ChatSummary, Jid, LinkState, MediaKind, PairingCode, SessionFactory,
    WhatsAppError, WhatsAppSession,
};
use super::history;
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
        // A group's name is its subject, which history sync supplies; the push
        // name here belongs to whoever just spoke, so it may name a 1:1 chat
        // only. A group already named by an import keeps that subject.
        let display_name = (!chat.is_group())
            .then(|| ctx.info.push_name.trim())
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
        if let Ok(mut chats) = self.chats.lock() {
            upsert(
                &mut chats,
                &ChatSummary {
                    is_group: chat.is_group(),
                    jid: chat.clone(),
                    display_name,
                },
            );
        }

        self.append(vec![BufferedMessage {
            jid: chat,
            message_id: ctx.info.id.to_string(),
            sent_at: DateTime::from_timestamp_millis(ctx.info.timestamp.as_millis())
                .unwrap_or_default(),
            from_me: ctx.info.source.is_from_me,
            sender: Some(ctx.info.source.sender.to_string()),
            body: ctx.message.text_content().unwrap_or_default().to_owned(),
            media: media_kind(&ctx.message),
        }]);
    }

    /// Fold one drained history chunk into the cache: chats in a single lock
    /// acquisition, then the inbox in another, each chat and message inserted
    /// once so a re-sync cannot duplicate what the live path already recorded.
    fn merge(&self, chunk: history::Chunk) -> Cached {
        if let Ok(mut chats) = self.chats.lock() {
            for incoming in &chunk.chats {
                upsert(&mut chats, incoming);
            }
        }
        self.append(chunk.messages)
    }

    /// Append to the inbox in one pass: drop entries already buffered (same
    /// chat, same message id), then evict from the front until the bound holds.
    fn append(&self, incoming: Vec<BufferedMessage>) -> Cached {
        let Ok(mut buffer) = self.buffer.lock() else {
            return Cached::default();
        };
        let mut known: HashSet<(Jid, String)> = buffer
            .iter()
            .map(|message| (message.jid.clone(), message.message_id.clone()))
            .collect();
        let fresh: Vec<BufferedMessage> = incoming
            .into_iter()
            .filter(|message| known.insert((message.jid.clone(), message.message_id.clone())))
            .collect();
        // A chunk larger than the bound keeps its newest tail: the oldest
        // history is the least interesting part of a resync.
        let overflow = fresh.len().saturating_sub(BUFFER_LIMIT);
        let buffered = fresh.len() - overflow;
        let room = BUFFER_LIMIT.saturating_sub(fresh.len());
        let dropped = buffer.len().saturating_sub(room);
        buffer.drain(..dropped);
        buffer.extend(fresh.into_iter().skip(overflow));
        Cached {
            buffered,
            evicted: dropped + overflow,
        }
    }
}

/// What one chunk actually put in the cache, after dedupe and eviction.
#[derive(Debug, Default, Clone, Copy)]
struct Cached {
    buffered: usize,
    evicted: usize,
}

/// Insert a chat, or fill in what the row is missing. Chats are keyed by their
/// normalised JID, so an import and a live message about the same conversation
/// land on one row: nothing is duplicated and a name is never erased by an
/// event that had none.
fn upsert(chats: &mut BTreeMap<Jid, ChatSummary>, incoming: &ChatSummary) {
    match chats.get_mut(&incoming.jid) {
        Some(known) => {
            if incoming.display_name.is_some() {
                known.display_name.clone_from(&incoming.display_name);
            }
        }
        None => {
            chats.insert(incoming.jid.clone(), incoming.clone());
        }
    }
}

/// Classify an inbound attachment without downloading it. `text_content`
/// covers captions, so a captioned photo still imports as text; this only
/// names what the import report counted as skipped.
///
/// Shared with [`super::history`], which classifies imported messages the same
/// way so a history sync and the live path agree on what counts as media.
pub(super) fn media_kind(message: &wa::Message) -> Option<MediaKind> {
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
            let _ = handle.client().logout().await;
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
            let history = Arc::clone(&shared);

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
                    async move {
                        shared.mutate(|state| {
                            state.on_failed(
                                "the phone unlinked this device or the bridge logged out: \\
                                 press Retry pairing to link it again",
                            )
                        })
                    }
                })
                // The only place a message is ever read, and it only copies
                // text out of the event: no reply is ever sent from here.
                .on_message(move |ctx| {
                    let shared = Arc::clone(&inbox);
                    async move { shared.record(&ctx) }
                })
                // The phone pushes its conversation list when the device pairs.
                // Draining it is what fills the picker before anyone sends a
                // message. Registering this handler is also what makes the
                // library keep the blob at all: it drops a chunk nobody is
                // listening for, and owns the ack/transfer itself.
                .on_event_for(&[EventKind::HistorySync], move |event, _client| {
                    let shared = Arc::clone(&history);
                    async move {
                        let Event::HistorySync(sync) = &*event else {
                            return;
                        };
                        // Inflating and decoding is CPU work on a blob that can
                        // be tens of megabytes, so it runs off the runtime
                        // threads that carry live traffic.
                        let compressed = sync.compressed_bytes().clone();
                        let size = sync.decompressed_size();
                        let sync_type = sync.sync_type();
                        let order = sync.chunk_order();
                        let progress = sync.progress();
                        let drained = tokio::task::spawn_blocking(move || {
                            let lazy =
                                LazyHistorySync::new(compressed, size, sync_type, order, progress);
                            history::drain(&lazy)
                        })
                        .await;
                        let mut chunk = match drained {
                            Ok(chunk) => chunk,
                            Err(error) => {
                                tracing::warn!(%error, "whatsapp history drain did not finish");
                                return;
                            }
                        };
                        let failed = chunk.failed.take();
                        let (counted, skipped, rejected) =
                            (chunk.counted, chunk.skipped, chunk.rejected);
                        let cached = shared.merge(chunk);
                        // One line per chunk: what arrived, what the library
                        // skipped, what this server refused, and what the
                        // bounded inbox kept or evicted.
                        match failed {
                            Some(failure) => tracing::warn!(
                                sync_type,
                                chunk_order = order,
                                progress,
                                counted,
                                skipped,
                                rejected,
                                buffered = cached.buffered,
                                evicted = cached.evicted,
                                failure = %failure,
                                "whatsapp history sync chunk drained with an error"
                            ),
                            None => tracing::info!(
                                sync_type,
                                chunk_order = order,
                                progress,
                                counted,
                                skipped,
                                rejected,
                                buffered = cached.buffered,
                                evicted = cached.evicted,
                                "whatsapp history sync chunk drained"
                            ),
                        }
                    }
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
        let filled: Vec<BufferedMessage> =
            (0..BUFFER_LIMIT).map(|n| message(&n.to_string())).collect();
        let first = shared.append(filled);
        assert_eq!(first.buffered, BUFFER_LIMIT);
        assert_eq!(first.evicted, 0);

        let next = shared.append(vec![message("last")]);
        assert_eq!(next.buffered, 1);
        assert_eq!(next.evicted, 1, "the oldest entry made room");
        let buffer = shared.buffer.lock().expect("inbox is never poisoned");
        assert_eq!(buffer.len(), BUFFER_LIMIT);
        assert_eq!(buffer.last().expect("last").message_id, "last");
    }

    #[test]
    fn a_resync_cannot_duplicate_a_chat_or_a_message() {
        let shared = Shared::new(LinkState::Linked);
        let group = ChatSummary {
            jid: jid("120363000000000001@g.us"),
            display_name: None,
            is_group: true,
        };
        // A live group message lands first: it has no subject to give the chat.
        {
            let mut chats = shared.chats.lock().expect("chat map is never poisoned");
            upsert(&mut chats, &group);
        }
        shared.append(vec![message("3A1")]);

        // The phone then resyncs the same conversation.
        let cached = shared.merge(history::Chunk {
            chats: vec![ChatSummary {
                display_name: Some("Project Atlas".into()),
                ..group
            }],
            messages: vec![message("3A1")],
            counted: 1,
            ..Default::default()
        });

        assert_eq!(cached.buffered, 0, "the message was already buffered");
        assert_eq!(cached.evicted, 0);
        let chats = shared.chats.lock().expect("chat map is never poisoned");
        assert_eq!(chats.len(), 1, "one row per chat, not one per source");
        assert_eq!(
            chats.values().next().expect("row").display_name.as_deref(),
            Some("Project Atlas"),
            "the import supplies the subject the live event could not"
        );
        drop(chats);
        assert_eq!(
            shared.buffer.lock().expect("inbox is never poisoned").len(),
            1
        );
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
