//! History sync: the chunk a freshly paired phone pushes, turned into chats.
//!
//! Pairing hands the server an `Event::HistorySync` chunk of compressed
//! protobuf — the phone's conversation list — before any live message arrives.
//! Without draining it the picker stays empty until somebody types, which is
//! the one moment a user is watching it.
//!
//! Decoding is kept apart from the cache: [`drain`] borrows the
//! [`LazyHistorySync`] and returns what it found, so the transport can run it
//! under `spawn_blocking` and the merge rules stay testable without a device.
//! The chunk is read with a [`HistorySyncStream`], one conversation at a time,
//! because the blob can be tens of megabytes and inflating it whole would
//! double the peak footprint of data that is about to be folded into maps
//! anyway.
//!
//! Properties this module holds to:
//!
//! * A clean end of stream is `Ok(false)`: done, not failed.
//! * One conversation that fails to decode is skipped and counted by the
//!   library ([`HistorySyncStream::skipped_conversations`]), never fatal — a
//!   corrupt entry must not cost the rest of the phone's history.
//! * `remainder()` is never called: it walks (and keeps) every field the
//!   conversation drain did not consume, for data this server does not read.
//! * A fatal mid-drain error keeps everything already drained, so the caller
//!   can merge the partial chunk instead of losing the whole chunk.

use chrono::DateTime;
use chrono::Utc;
use whatsapp_rust::prelude::*;
use whatsapp_rust::wacore::types::events::LazyHistorySync;

use super::bridge::{BufferedMessage, ChatSummary, Jid};

/// Nothing WhatsApp stored can predate the service. A stamp below this floor is
/// a unit mistake — seconds read as millis lands in 1970 — so it is dropped
/// rather than imported at a date the user would see and not believe.
const WHATSAPP_EPOCH_MILLIS: i64 = 1_230_768_000_000; // 2009-01-01T00:00:00Z

/// What one drained chunk yielded, plus how much of it the server refused.
#[derive(Debug, Default)]
pub struct Chunk {
    /// Chats the chunk named, in arrival order.
    pub chats: Vec<ChatSummary>,
    /// Messages it carried, ready for the same buffer live messages use.
    pub messages: Vec<BufferedMessage>,
    /// Conversations the stream handed back.
    pub counted: usize,
    /// Conversations the library skipped because they could not be decoded.
    pub skipped: usize,
    /// Entries this server refused: a chat it cannot address, or a message it
    /// cannot place in time.
    pub rejected: usize,
    /// Why the drain stopped early, when it did. Everything already drained is
    /// still returned alongside it.
    pub failed: Option<String>,
}

/// Drain one history-sync chunk.
///
/// Blocking-safe: no I/O, no awaiting, so callers run it on a blocking thread.
/// Nothing here takes a lock or touches the session, so a chunk that dies
/// halfway cannot unlink a device.
pub fn drain(lazy: &LazyHistorySync) -> Chunk {
    let mut stream = lazy.stream();
    // One struct for the whole loop: buffa's `clear()` keeps the message-spine
    // capacity, so a chunk of thousands of chats does not reallocate per entry.
    let mut conversation = wa::Conversation::default();
    let mut chunk = Chunk::default();

    loop {
        match stream.next_conversation_into(&mut conversation) {
            Ok(true) => {
                chunk.counted += 1;
                match absorbed(&conversation) {
                    Some((chat, mut messages, refused)) => {
                        chunk.rejected += refused;
                        chunk.messages.append(&mut messages);
                        chunk.chats.push(chat);
                    }
                    None => chunk.rejected += 1,
                }
            }
            Ok(false) => break,
            Err(error) => {
                // Keep what was drained: a partial chunk still populates the
                // picker, and the caller logs the reason.
                chunk.failed = Some(error.to_string());
                break;
            }
        }
    }

    chunk.skipped = stream.skipped_conversations();
    chunk
}

/// Turn one conversation into its chat row and the messages it carried, plus
/// how many of those messages had to be refused. `None` when the conversation's
/// id is not an address this server can store.
fn absorbed(conversation: &wa::Conversation) -> Option<(ChatSummary, Vec<BufferedMessage>, usize)> {
    let jid = Jid::parse(&conversation.id).ok()?;
    let chat = ChatSummary {
        jid: jid.clone(),
        display_name: subject(conversation),
        is_group: jid.is_group(),
    };
    let mut messages = Vec::with_capacity(conversation.messages.len());
    let mut refused = 0;
    for info in conversation
        .messages
        .iter()
        .filter_map(|entry| entry.message.as_option())
    {
        match buffered(&jid, &chat, info) {
            Some(message) => messages.push(message),
            None => refused += 1,
        }
    }
    Some((chat, messages, refused))
}

/// A chat's own name. For a group this is the subject, the only name the picker
/// may show; `displayName` is the newer field and wins when both are set.
fn subject(conversation: &wa::Conversation) -> Option<String> {
    [&conversation.display_name, &conversation.name]
        .into_iter()
        .flatten()
        .map(|name| name.trim())
        .find(|name| !name.is_empty())
        .map(str::to_owned)
}

/// One historic message, in the shape the live path already buffers.
fn buffered(jid: &Jid, chat: &ChatSummary, info: &wa::WebMessageInfo) -> Option<BufferedMessage> {
    let key = info.key.as_option()?;
    let message_id = key.id.as_deref()?.trim();
    if message_id.is_empty() {
        return None;
    }
    let sent_at = instant(info.message_timestamp?)?;
    let message = info.message.as_option();
    Some(BufferedMessage {
        jid: jid.clone(),
        message_id: message_id.to_owned(),
        sent_at,
        from_me: key.from_me.unwrap_or(false),
        sender: key
            .participant
            .clone()
            .or_else(|| (!chat.is_group).then(|| jid.as_str().to_owned())),
        body: message
            .and_then(|message| message.text_content())
            .unwrap_or_default()
            .to_owned(),
        media: message.and_then(super::live::media_kind),
    })
}

/// WhatsApp stamps history messages in **seconds**. The buffer stores instants,
/// so the conversion has to be explicit: reading seconds as millis dates every
/// imported note to 1970, which is what `seconds_timestamp_lands_in_millis`
/// pins.
fn instant(seconds: u64) -> Option<DateTime<Utc>> {
    let millis = i64::try_from(seconds).ok()?.checked_mul(1_000)?;
    let at = DateTime::from_timestamp_millis(millis)?;
    (at.timestamp_millis() >= WHATSAPP_EPOCH_MILLIS).then_some(at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use whatsapp_rust::bytes::Bytes;
    use whatsapp_rust::wacore::history_sync::MAX_DECOMPRESSED;

    /// A zlib-compressed `HistorySync` with `syncType = INITIAL_BOOTSTRAP` and
    /// four conversations: a 1:1 chat (`15550100@s.whatsapp.net`, unread, one
    /// message), a group called "Project Atlas" with one member message, a
    /// `status@broadcast` entry no address can name, and one payload that is
    /// not a `Conversation` at all. The bytes are generated rather than
    /// captured, so the test needs no device.
    const FIXTURE: &[u8] =
        include_bytes!("../../tests/fixtures/whatsapp_history_initial_bootstrap.bin");

    fn fixture() -> LazyHistorySync {
        LazyHistorySync::new(
            Bytes::from_static(FIXTURE),
            MAX_DECOMPRESSED as usize,
            0, // INITIAL_BOOTSTRAP
            None,
            None,
        )
    }

    #[test]
    fn enumerates_chats_from_initial_bootstrap() {
        let chunk = drain(&fixture());

        assert!(
            chunk.failed.is_none(),
            "a clean end of stream is done, not failed: {:?}",
            chunk.failed
        );
        assert_eq!(chunk.counted, 3, "every readable conversation is counted");
        assert_eq!(
            chunk.skipped, 1,
            "the undecodable entry is skipped, not fatal"
        );
        assert_eq!(
            chunk.rejected, 1,
            "status@broadcast is not a chat this server can store"
        );

        let direct = chunk
            .chats
            .iter()
            .find(|chat| chat.jid.as_str() == "15550100@s.whatsapp.net")
            .expect("the 1:1 chat is enumerated");
        assert!(!direct.is_group);
        assert_eq!(direct.display_name, None, "a 1:1 chat carries no subject");

        let group = chunk
            .chats
            .iter()
            .find(|chat| chat.is_group)
            .expect("the group is enumerated");
        assert_eq!(group.jid.as_str(), "120363000000000001@g.us");
        assert_eq!(
            group.display_name.as_deref(),
            Some("Project Atlas"),
            "a group is named by its subject, never by a member's push name"
        );

        let bodies: Vec<&str> = chunk.messages.iter().map(|m| m.body.as_str()).collect();
        assert_eq!(bodies, vec!["handshake one", "group history"]);
        assert_eq!(
            chunk.messages[1].sender.as_deref(),
            Some("15550200@s.whatsapp.net"),
            "a group message keeps the member who wrote it"
        );
    }

    #[test]
    fn seconds_timestamp_lands_in_millis() {
        let chunk = drain(&fixture());
        let message = chunk
            .messages
            .iter()
            .find(|m| m.message_id == "3A1")
            .expect("the first message is buffered");

        // The wire says 1_699_999_900, in seconds.
        assert_eq!(
            message.sent_at,
            DateTime::from_timestamp_millis(1_699_999_900_000).expect("a real instant")
        );
        assert_eq!(message.sent_at.timestamp(), 1_699_999_900);
        assert_eq!(message.sent_at.timestamp_millis(), 1_699_999_900_000);

        // A stamp below the service's own beginning is a unit mistake, not a
        // date: refused rather than imported as 1970.
        assert_eq!(instant(1), None);
    }
}
