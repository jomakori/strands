//! The transport-independent half of the WhatsApp link.
//!
//! `whatsapp-rust` needs a phone to move, so the decisions that must be right —
//! what a JID is, whether a chat may be imported, when a link is allowed to
//! change state — live here with no I/O at all. The live client in `live.rs`
//! only carries these decisions out.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use strands_core::UserId;

use crate::auth::identity::BoxFuture;

/// A WhatsApp address as this server stores it.
///
/// JIDs arrive in several spellings for the same chat (`+15551234@s.whatsapp.net`,
/// `15551234:3@s.whatsapp.net`, `15551234@c.us`), so everything is normalised on
/// the way in and chats are matched on the normalised form only.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Jid(String);

impl Jid {
    /// Parse and normalise one address, or say why it is not a chat JID.
    pub fn parse(raw: &str) -> Result<Self, JidError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(JidError::Empty);
        }
        let Some((local, server)) = trimmed.split_once('@') else {
            return Err(JidError::MissingServer);
        };
        if local.is_empty() {
            return Err(JidError::EmptyLocalPart);
        }
        // Device suffix (`:3`) addresses one phone's session, not the chat.
        let local = local.split(':').next().unwrap_or(local);
        if local.is_empty() {
            return Err(JidError::EmptyLocalPart);
        }
        let local = local.trim_start_matches('+');
        let server = match server.trim().to_ascii_lowercase().as_str() {
            "s.whatsapp.net" | "c.us" => "s.whatsapp.net",
            "g.us" => "g.us",
            "lid" => "lid",
            other => return Err(JidError::UnsupportedServer(other.to_owned())),
        };
        Ok(Self(format!("{local}@{server}")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_group(&self) -> bool {
        self.0.ends_with("@g.us")
    }
}

impl fmt::Display for Jid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JidError {
    #[error("empty JID")]
    Empty,
    #[error("JID has no @server part")]
    MissingServer,
    #[error("JID has an empty local part")]
    EmptyLocalPart,
    #[error("unsupported JID server: {0}")]
    UnsupportedServer(String),
}

/// The chats an account has allowed history import to touch.
///
/// Empty is meaningful and is the default: an account that has not ticked a
/// chat imports nothing, which is the safe direction for an unofficial link.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllowList(HashSet<Jid>);

impl AllowList {
    pub fn empty() -> Self {
        Self(HashSet::new())
    }

    /// Build from stored text, reporting the first entry that is not a JID.
    pub fn parse<I, S>(raw: I) -> Result<Self, JidError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut jids = HashSet::new();
        for entry in raw {
            jids.insert(Jid::parse(entry.as_ref())?);
        }
        Ok(Self(jids))
    }

    pub fn permits(&self, jid: &Jid) -> bool {
        self.0.contains(jid)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The allowlist in a stable order, for storage and for comparison in tests.
    pub fn jids(&self) -> Vec<Jid> {
        let mut jids: Vec<Jid> = self.0.iter().cloned().collect();
        jids.sort();
        jids
    }

    pub fn insert(&mut self, jid: Jid) {
        self.0.insert(jid);
    }
}

/// One chat the phone reports, before any import decision is made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSummary {
    pub jid: Jid,
    pub display_name: Option<String>,
    pub is_group: bool,
}

/// One message the bridge has buffered, text already extracted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferedMessage {
    pub jid: Jid,
    /// WhatsApp's own message id: the dedupe key across history sync and live
    /// delivery, which repeat the same message by design.
    pub message_id: String,
    pub sent_at: DateTime<Utc>,
    pub from_me: bool,
    pub sender: Option<String>,
    /// Empty for messages with no text (media, polls, stickers): imported as
    /// nothing, because the ticket imports message text and nothing else.
    pub body: String,
}

/// What one import pass may write, and what it must skip.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub allowed: BTreeMap<Jid, Vec<BufferedMessage>>,
    /// Chats with buffered messages the allowlist excludes.
    pub skipped_chats: usize,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }

    pub fn messages(&self) -> usize {
        self.allowed.values().map(Vec::len).sum()
    }
}

/// Split buffered messages into importable and skipped chats.
///
/// Grouping by chat is what makes "a non-allowlisted chat imports zero"
/// checkable: the excluded chats are counted, and their messages never reach
/// the caller at all.
pub fn select(messages: impl IntoIterator<Item = BufferedMessage>, allow: &AllowList) -> Selection {
    let mut selection = Selection::default();
    let mut skipped: HashSet<Jid> = HashSet::new();
    for message in messages {
        if allow.permits(&message.jid) {
            selection
                .allowed
                .entry(message.jid.clone())
                .or_default()
                .push(message);
        } else {
            skipped.insert(message.jid.clone());
        }
    }
    for (jid, mut batch) in std::mem::take(&mut selection.allowed) {
        batch.sort_by(|left, right| {
            left.sent_at
                .cmp(&right.sent_at)
                .then_with(|| left.message_id.cmp(&right.message_id))
        });
        selection.allowed.insert(jid, batch);
    }
    selection.skipped_chats = skipped.len();
    selection
}

/// A pairing code shown to the client, with its advertised lifetime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCode {
    pub value: String,
    pub expires_in_seconds: u64,
}

/// The link's lifecycle, enforced as a state machine so a late event from a
/// dead connection cannot resurrect a link the user unlinked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkState {
    Unlinked,
    AwaitingScan {
        qr: Option<PairingCode>,
        pair_code: Option<PairingCode>,
    },
    Linked,
}

impl LinkState {
    /// A QR code arrived (or was refreshed) during pairing.
    pub fn on_qr(&mut self, qr: PairingCode) -> Result<(), LinkError> {
        match self {
            Self::AwaitingScan { qr: slot, .. } => {
                *slot = Some(qr);
                Ok(())
            }
            Self::Unlinked => Err(LinkError::NotPairing("qr code")),
            Self::Linked => Err(LinkError::AlreadyLinked("qr code")),
        }
    }

    /// A pair code arrived; the phone types it instead of scanning.
    pub fn on_pair_code(&mut self, code: PairingCode) -> Result<(), LinkError> {
        match self {
            Self::AwaitingScan { pair_code, .. } => {
                *pair_code = Some(code);
                Ok(())
            }
            Self::Unlinked => Err(LinkError::NotPairing("pair code")),
            Self::Linked => Err(LinkError::AlreadyLinked("pair code")),
        }
    }

    /// The phone finished linking.
    pub fn on_paired(&mut self) -> Result<(), LinkError> {
        match self {
            Self::AwaitingScan { .. } => {
                *self = Self::Linked;
                Ok(())
            }
            Self::Linked => Ok(()),
            Self::Unlinked => Err(LinkError::NotPairing("pair success")),
        }
    }

    /// The device was unlinked — by us, or from the phone.
    pub fn on_unlinked(&mut self) -> Result<(), LinkError> {
        if matches!(self, Self::Unlinked) {
            return Err(LinkError::NotLinked("unlink"));
        }
        *self = Self::Unlinked;
        Ok(())
    }

    pub fn is_linked(&self) -> bool {
        matches!(self, Self::Linked)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error("link is not pairing, cannot accept {0}")]
    NotPairing(&'static str),
    #[error("link is already paired, cannot accept {0}")]
    AlreadyLinked(&'static str),
    #[error("link is not paired, cannot {0}")]
    NotLinked(&'static str),
}

/// Something the link could not do, in the words the client can act on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WhatsAppError {
    #[error("no WhatsApp session is open for this account")]
    NotLinked,
    #[error("the WhatsApp client is not configured on this server")]
    NotConfigured,
    #[error("WhatsApp rejected the link: {0}")]
    Rejected(String),
    #[error("WhatsApp transport failed: {0}")]
    Transport(String),
    #[error("the stored session could not be decrypted")]
    Session,
}

/// A live, per-user WhatsApp session: the only thing routes talk to.
pub trait WhatsAppSession: Send + Sync {
    /// The state at the moment of the call.
    fn state(&self) -> LinkState;

    /// Every chat the phone has reported so far.
    fn chats(&self) -> Vec<ChatSummary>;

    /// The messages buffered so far, without consuming them.
    ///
    /// Status polling must not swallow a message the next import needs, so the
    /// buffered view is separate from [`WhatsAppSession::drain`].
    fn peek(&self) -> Vec<BufferedMessage>;

    /// Hand over the messages buffered since the last drain.
    ///
    /// Draining is what makes "Sync now" work without a second source of
    /// truth: history sync and live delivery both land in the same buffer.
    fn drain(&self) -> Vec<BufferedMessage>;

    /// Ask the phone for anything new (newest messages, missed while offline).
    fn sync(&self) -> BoxFuture<'_, Result<(), WhatsAppError>>;

    /// Unlink the device on the phone side and drop the local session.
    fn unlink(&self) -> BoxFuture<'_, Result<(), WhatsAppError>>;

    /// Stop the client without unlinking, returning the session bytes the
    /// server keeps encrypted in `connector_secrets`.
    fn park(&self) -> BoxFuture<'_, Result<Option<Vec<u8>>, WhatsAppError>>;
}

/// Opens per-user sessions. Production backs this with `whatsapp-rust`;
/// tests back it with a fake, because a real link needs a phone to scan a QR.
pub trait SessionFactory: Send + Sync {
    /// Open a session, restoring `session` (previously parked bytes) if given.
    fn open(
        &self,
        user_id: UserId,
        restore: Option<Vec<u8>>,
    ) -> BoxFuture<'_, Result<Arc<dyn WhatsAppSession>, WhatsAppError>>;
}

/// The factory a deploy without WhatsApp configured gets.
///
/// Refusing to open keeps `GET /api/v1/whatsapp` honest: it reports an unlinked
/// account instead of implying a phone could be paired.
pub struct UnconfiguredFactory;

impl SessionFactory for UnconfiguredFactory {
    fn open(
        &self,
        _user_id: UserId,
        _restore: Option<Vec<u8>>,
    ) -> BoxFuture<'_, Result<Arc<dyn WhatsAppSession>, WhatsAppError>> {
        Box::pin(async { Err(WhatsAppError::NotConfigured) })
    }
}

/// The open sessions, one per account.
///
/// Pairing is a single attempt at a time per account, so the map is also the
/// guard: a second pair request joins the attempt already running instead of
/// opening a second device link.
pub struct Registry {
    factory: Arc<dyn SessionFactory>,
    sessions: std::sync::Mutex<HashMap<UserId, Arc<dyn WhatsAppSession>>>,
}

impl Registry {
    pub fn new(factory: Arc<dyn SessionFactory>) -> Self {
        Self {
            factory,
            sessions: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn session(&self, user_id: UserId) -> Option<Arc<dyn WhatsAppSession>> {
        self.sessions
            .lock()
            .expect("session map is never poisoned")
            .get(&user_id)
            .cloned()
    }

    /// Open (or reuse) this account's session.
    pub async fn connect(
        &self,
        user_id: UserId,
        restore: Option<Vec<u8>>,
    ) -> Result<Arc<dyn WhatsAppSession>, WhatsAppError> {
        if let Some(session) = self.session(user_id) {
            return Ok(session);
        }
        let session = self.factory.open(user_id, restore).await?;
        self.sessions
            .lock()
            .expect("session map is never poisoned")
            .insert(user_id, session.clone());
        Ok(session)
    }

    /// Drop the handle after the device was unlinked.
    pub fn forget(&self, user_id: UserId) {
        self.sessions
            .lock()
            .expect("session map is never poisoned")
            .remove(&user_id);
    }

    /// Park every open session, so a graceful shutdown persists its keys.
    pub async fn park_all(&self) -> Vec<(UserId, Vec<u8>)> {
        let sessions: Vec<(UserId, Arc<dyn WhatsAppSession>)> = self
            .sessions
            .lock()
            .expect("session map is never poisoned")
            .iter()
            .map(|(user_id, session)| (*user_id, session.clone()))
            .collect();

        let mut parked = Vec::new();
        for (user_id, session) in sessions {
            if let Ok(Some(bytes)) = session.park().await {
                parked.push((user_id, bytes));
            }
            self.forget(user_id);
        }
        parked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(value: &str) -> PairingCode {
        PairingCode {
            value: value.to_owned(),
            expires_in_seconds: 60,
        }
    }

    fn message(jid: &str, id: &str, body: &str) -> BufferedMessage {
        BufferedMessage {
            jid: Jid::parse(jid).expect("valid jid"),
            message_id: id.to_owned(),
            sent_at: Utc::now(),
            from_me: false,
            sender: None,
            body: body.to_owned(),
        }
    }

    #[test]
    fn jid_normalises_device_suffix_and_aliases() {
        let jid = Jid::parse("+1 555-0100:12@c.us").expect("valid");
        assert_eq!(jid.as_str(), "1 555-0100@s.whatsapp.net");
        assert_eq!(
            Jid::parse("15550100@s.whatsapp.net")
                .expect("valid")
                .as_str(),
            "15550100@s.whatsapp.net"
        );
        assert!(Jid::parse("120363@g.us").expect("valid").is_group());
        assert!(Jid::parse("status@broadcast").is_err());
        assert!(Jid::parse("15550100").is_err());
        assert!(Jid::parse("@s.whatsapp.net").is_err());
    }

    #[test]
    fn allowlist_matches_only_listed_chats() {
        let allow = AllowList::parse(["15550100@s.whatsapp.net"]).expect("valid list");
        assert!(allow.permits(&Jid::parse("15550100:4@c.us").expect("valid")));
        assert!(!allow.permits(&Jid::parse("15550199@s.whatsapp.net").expect("valid")));
        assert!(!allow.permits(&Jid::parse("120363@g.us").expect("valid")));
    }

    #[test]
    fn empty_allowlist_permits_nothing() {
        let allow = AllowList::empty();
        assert!(allow.is_empty());
        let selection = select([message("15550100@s.whatsapp.net", "A", "hi")], &allow);
        assert!(selection.is_empty());
        assert_eq!(selection.skipped_chats, 1);
    }

    #[test]
    fn select_imports_allowlisted_and_zero_from_others() {
        let allow = AllowList::parse(["15550100@s.whatsapp.net"]).expect("valid list");
        let selection = select(
            [
                message("15550100@s.whatsapp.net", "A", "allowed one"),
                message("15550199@s.whatsapp.net", "B", "not allowed"),
                message("15550100@s.whatsapp.net", "C", "allowed two"),
            ],
            &allow,
        );
        assert_eq!(selection.messages(), 2);
        assert_eq!(selection.skipped_chats, 1);
        let allowed = selection
            .allowed
            .get(&Jid::parse("15550100@s.whatsapp.net").expect("valid"))
            .expect("chat present");
        assert_eq!(allowed.len(), 2);
        assert!(
            !selection
                .allowed
                .contains_key(&Jid::parse("15550199@s.whatsapp.net").expect("valid"))
        );
    }

    #[test]
    fn select_orders_by_time_then_id() {
        let allow = AllowList::parse(["15550100@s.whatsapp.net"]).expect("valid list");
        let mut older = message("15550100@s.whatsapp.net", "B", "older");
        older.sent_at = Utc::now() - chrono::Duration::minutes(5);
        let newer = message("15550100@s.whatsapp.net", "A", "newer");
        let selection = select([newer, older], &allow);
        let batch = selection
            .allowed
            .get(&Jid::parse("15550100@s.whatsapp.net").expect("valid"))
            .expect("chat present");
        assert_eq!(batch.first().expect("first").body, "older");
    }

    #[test]
    fn pairing_accepts_qr_then_success() {
        let mut state = LinkState::AwaitingScan {
            qr: None,
            pair_code: None,
        };
        state.on_qr(code("qr-1")).expect("qr accepted");
        state
            .on_pair_code(code("123-456"))
            .expect("pair code accepted");
        match &state {
            LinkState::AwaitingScan { qr, pair_code } => {
                assert_eq!(qr.as_ref().expect("qr").value, "qr-1");
                assert_eq!(pair_code.as_ref().expect("code").value, "123-456");
            }
            other => panic!("still pairing, got {other:?}"),
        }
        state.on_paired().expect("paired");
        assert!(state.is_linked());
        state.on_unlinked().expect("unlinked");
        assert_eq!(state, LinkState::Unlinked);
    }

    #[test]
    fn late_events_cannot_resurrect_a_dead_link() {
        let mut state = LinkState::Unlinked;
        assert_eq!(
            state.on_qr(code("qr")),
            Err(LinkError::NotPairing("qr code"))
        );
        assert_eq!(state.on_unlinked(), Err(LinkError::NotLinked("unlink")));

        let mut linked = LinkState::Linked;
        assert_eq!(
            linked.on_pair_code(code("123")),
            Err(LinkError::AlreadyLinked("pair code"))
        );
        assert_eq!(linked.on_paired(), Ok(()));
        assert!(linked.is_linked());
    }
}
