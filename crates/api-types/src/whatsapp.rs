//! WhatsApp link DTOs (ticket T15).
//!
//! The connector links a phone in-process through `whatsapp-rust`, so every
//! shape here describes a session this server drives itself: no webhook ever
//! answers these endpoints. The chat list is its own response — the UI must be
//! able to show what is on the phone *before* any history is imported, so the
//! list is never folded into the import report.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The ToS/ban warning the UI must show before pairing (decision D10).
///
/// The route is unofficial: WhatsApp bans numbers it finds behind third-party
/// clients, and the user accepted that risk when choosing this connector.
pub const WHATSAPP_DISCLOSURE: &str = "Unofficial WhatsApp link. WhatsApp's Terms of Service \
     forbid third-party clients, and using one can get this phone number banned. Strands only \
     reads: there is no send path in this build.";

/// Where the link is in its lifecycle, as the UI renders it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WhatsAppLinkState {
    /// No session: either never paired, unlinked from the phone, or logged out.
    Unlinked,
    /// A pairing attempt is live; the client scans `qr` or types `pair_code`.
    AwaitingScan {
        qr: Option<String>,
        pair_code: Option<String>,
        expires_in_seconds: u64,
    },
    /// The phone is linked and the chat list is available.
    Linked,
}

/// One chat as the phone reports it, plus whether this account allows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhatsAppChat {
    pub jid: String,
    pub display_name: Option<String>,
    pub is_group: bool,
    /// Whether history import may touch this chat.
    pub allowed: bool,
    /// Messages the bridge has buffered for this chat, imported or not.
    pub buffered_messages: usize,
}

/// `GET /api/v1/whatsapp`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhatsAppStatus {
    pub state: WhatsAppLinkState,
    pub disclosure: String,
    /// The JID allowlist as stored; empty means "import nothing".
    pub allowlist: Vec<String>,
    pub chats: Vec<WhatsAppChat>,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

/// `PUT /api/v1/whatsapp/allowlist`.
///
/// Replaces the whole list: the client sends the chats it wants, and anything
/// absent stops being imported. Unknown JIDs are rejected rather than stored,
/// so a typo cannot silently import nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhatsAppAllowlist {
    pub jids: Vec<String>,
}

/// Receipt for `POST /import` and `POST /sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhatsAppImportReport {
    /// Chats whose messages were written.
    pub chats_imported: usize,
    /// Chats the bridge held messages for but the allowlist excluded.
    pub chats_skipped: usize,
    /// Notes actually inserted; duplicates and redeliveries leave this at 0.
    pub notes_created: usize,
}
