//! Where a WhatsApp link keeps its state: the connector row, the encrypted
//! session blob, the chat snapshot, and the notes an import writes.
//!
//! Nothing here decides *whether* a chat may be imported — that is the
//! allowlist's job in `bridge.rs`. This module only stores what the allowlist
//! already approved.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::{DateTime, Utc};
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use sqlx::{PgPool, Postgres, Transaction};
use strands_core::{Note, Source};
use uuid::Uuid;

use crate::error::AppError;
use crate::ingest::TagIndex;
use crate::whatsapp::bridge::{AllowList, ChatSummary, Jid};

/// The `connectors.kind` this module owns.
pub const CONNECTOR_KIND: &str = "whatsapp";

/// Tag every imported chat's thread sits under.
const IMPORT_TAG: &str = "WhatsApp";

/// Nonce length AES-GCM requires; the prefix of every stored ciphertext.
const NONCE_BYTES: usize = 12;

/// Encrypts connector secrets at rest with AES-256-GCM.
///
/// The key comes from the environment and is never derived from, or stored
/// beside, the ciphertext: `connector_secrets.ciphertext` holds
/// `nonce || ciphertext || tag` and nothing else.
#[derive(Clone)]
pub struct SessionCipher {
    key: std::sync::Arc<LessSafeKey>,
}

impl SessionCipher {
    /// Read the shared connector key as base64 of exactly 32 bytes.
    pub fn from_env() -> Result<Self, std::env::VarError> {
        let raw = std::env::var(crate::secrets::KEY_ENV)?;
        Self::from_base64(&raw).ok_or(std::env::VarError::NotPresent)
    }

    pub fn from_base64(encoded: &str) -> Option<Self> {
        let bytes = BASE64.decode(encoded.trim()).ok()?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let unbound = UnboundKey::new(&AES_256_GCM, bytes).ok()?;
        Some(Self {
            key: std::sync::Arc::new(LessSafeKey::new(unbound)),
        })
    }

    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, AppError> {
        let mut nonce_bytes = [0u8; NONCE_BYTES];
        SystemRandom::new().fill(&mut nonce_bytes).map_err(|_| {
            AppError::internal("whatsapp nonce", sqlx::Error::Protocol("rng".into()))
        })?;
        let mut sealed = plaintext.to_vec();
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce_bytes),
                Aad::empty(),
                &mut sealed,
            )
            .map_err(|_| {
                AppError::internal("whatsapp seal", sqlx::Error::Protocol("aead".into()))
            })?;
        let mut stored = Vec::with_capacity(NONCE_BYTES + sealed.len());
        stored.extend_from_slice(&nonce_bytes);
        stored.extend_from_slice(&sealed);
        Ok(stored)
    }

    pub fn open(&self, stored: &[u8]) -> Result<Vec<u8>, AppError> {
        if stored.len() <= NONCE_BYTES {
            return Err(AppError::internal(
                "whatsapp open",
                sqlx::Error::Protocol("truncated secret".into()),
            ));
        }
        let (nonce_bytes, sealed) = stored.split_at(NONCE_BYTES);
        let mut nonce = [0u8; NONCE_BYTES];
        nonce.copy_from_slice(nonce_bytes);
        let mut buffer = sealed.to_vec();
        let plaintext = self
            .key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::empty(),
                &mut buffer,
            )
            .map_err(|_| {
                AppError::internal("whatsapp open", sqlx::Error::Protocol("aead".into()))
            })?;
        Ok(plaintext.to_vec())
    }
}

/// The connector row's own state, as the routes need it.
#[derive(Debug, Clone)]
pub struct ConnectorRow {
    pub id: Uuid,
    /// `connectors.status` as stored: the last thing this link did, which is the
    /// only evidence left after a restart drops the in-memory session (T28).
    pub status: String,
    pub allowlist: AllowList,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

/// One stored chat, with the watermark an import advances.
#[derive(Debug, Clone)]
pub struct StoredChat {
    pub jid: Jid,
    pub display_name: Option<String>,
    pub is_group: bool,
    pub imported_through: Option<DateTime<Utc>>,
}

/// Fetch the connector row, creating it on first use.
///
/// `config.allowlist` is where the JID allowlist lives; its absence means the
/// account allows nothing, which is also the safe default.
pub async fn connector(pool: &PgPool, user_id: Uuid) -> Result<ConnectorRow, AppError> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO connectors (id, user_id, kind) VALUES ($1, $2, $3) \
         ON CONFLICT (user_id, kind) DO UPDATE SET kind = EXCLUDED.kind \
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(CONNECTOR_KIND)
    .fetch_one(pool)
    .await?;

    let (config, last_sync_at, last_error, status) = sqlx::query_as::<
        _,
        (
            serde_json::Value,
            Option<DateTime<Utc>>,
            Option<String>,
            String,
        ),
    >(
        "SELECT config, last_sync_at, last_error, status::text FROM connectors WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await?;

    let allowlist = parse_allowlist(&config)?;
    Ok(ConnectorRow {
        id,
        status,
        allowlist,
        last_sync_at,
        last_error,
    })
}

fn parse_allowlist(config: &serde_json::Value) -> Result<AllowList, AppError> {
    let entries: Vec<String> = config
        .get("allowlist")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    AllowList::parse(entries).map_err(|error| {
        AppError::internal(
            "whatsapp allowlist",
            sqlx::Error::Protocol(format!("stored allowlist is invalid: {error}")),
        )
    })
}

/// Replace the JID allowlist.
pub async fn save_allowlist(
    pool: &PgPool,
    connector_id: Uuid,
    allowlist: &AllowList,
) -> Result<(), AppError> {
    let jids: Vec<String> = allowlist.jids().iter().map(|jid| jid.to_string()).collect();
    let config = serde_json::json!({ "allowlist": jids });
    sqlx::query("UPDATE connectors SET config = $2 WHERE id = $1")
        .bind(connector_id)
        .bind(config)
        .execute(pool)
        .await?;
    Ok(())
}

/// Record the link's status and, when it failed, why.
pub async fn set_status(
    pool: &PgPool,
    connector_id: Uuid,
    status: &str,
    last_error: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE connectors SET status = $2::connector_status, last_error = $3 WHERE id = $1",
    )
    .bind(connector_id)
    .bind(status)
    .bind(last_error)
    .execute(pool)
    .await?;
    Ok(())
}

/// Store the session blob, encrypted, replacing any previous one.
pub async fn save_session(
    pool: &PgPool,
    connector_id: Uuid,
    user_id: Uuid,
    cipher: &SessionCipher,
    session: &[u8],
) -> Result<(), AppError> {
    let ciphertext = cipher.seal(session)?;
    sqlx::query(
        "INSERT INTO connector_secrets (connector_id, user_id, ciphertext) VALUES ($1, $2, $3) \
         ON CONFLICT (connector_id) DO UPDATE \
         SET ciphertext = EXCLUDED.ciphertext, user_id = EXCLUDED.user_id",
    )
    .bind(connector_id)
    .bind(user_id)
    .bind(ciphertext)
    .execute(pool)
    .await?;
    Ok(())
}

/// Read back the session blob, decrypted.
pub async fn load_session(
    pool: &PgPool,
    connector_id: Uuid,
    cipher: &SessionCipher,
) -> Result<Option<Vec<u8>>, AppError> {
    let stored: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT ciphertext FROM connector_secrets WHERE connector_id = $1")
            .bind(connector_id)
            .fetch_optional(pool)
            .await?;
    stored.map(|bytes| cipher.open(&bytes)).transpose()
}

/// Forget the session: called when the device is unlinked or the phone logs us out.
pub async fn clear_session(pool: &PgPool, connector_id: Uuid) -> Result<(), AppError> {
    sqlx::query("DELETE FROM connector_secrets WHERE connector_id = $1")
        .bind(connector_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Replace the chat snapshot taken at pairing time.
///
/// Chats that are no longer on the phone are dropped, so the list the user sees
/// is the list the phone reports; chats that remain are updated in place,
/// because `imported_through` is the import watermark and must survive a resync.
pub async fn save_chats(
    pool: &PgPool,
    user_id: Uuid,
    chats: &[ChatSummary],
) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let jids: Vec<String> = chats.iter().map(|chat| chat.jid.to_string()).collect();
    sqlx::query("DELETE FROM whatsapp_chats WHERE user_id = $1 AND NOT (jid = ANY($2))")
        .bind(user_id)
        .bind(&jids)
        .execute(&mut *tx)
        .await?;
    for chat in chats {
        sqlx::query(
            "INSERT INTO whatsapp_chats (user_id, jid, display_name, is_group) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (user_id, jid) DO UPDATE \
             SET display_name = EXCLUDED.display_name, \
                 is_group = EXCLUDED.is_group, \
                 seen_at = now()",
        )
        .bind(user_id)
        .bind(chat.jid.as_str())
        .bind(chat.display_name.as_deref())
        .bind(chat.is_group)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// The stored chat snapshot.
pub async fn chats(pool: &PgPool, user_id: Uuid) -> Result<Vec<StoredChat>, AppError> {
    let rows = sqlx::query_as::<_, (String, Option<String>, bool, Option<DateTime<Utc>>)>(
        "SELECT jid, display_name, is_group, imported_through FROM whatsapp_chats \
         WHERE user_id = $1 ORDER BY display_name NULLS LAST, jid",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    let mut chats = Vec::with_capacity(rows.len());
    for (jid, display_name, is_group, imported_through) in rows {
        // A row written by an older build could hold a JID this build rejects;
        // skipping it keeps the list renderable rather than failing the request.
        if let Ok(jid) = Jid::parse(&jid) {
            chats.push(StoredChat {
                jid,
                display_name,
                is_group,
                imported_through,
            });
        }
    }
    Ok(chats)
}

/// Write one import pass inside a transaction: a thread per chat, one note per
/// message, deduped by message id.
///
/// Returns the number of notes actually inserted. Redeliveries and re-imports
/// collide on `(user_id, dedupe_key)` and insert nothing, which is what makes a
/// second "Sync now" safe.
pub async fn write_messages(
    pool: &PgPool,
    user_id: Uuid,
    category_id: Uuid,
    chats: &[(StoredChat, Vec<crate::whatsapp::bridge::BufferedMessage>)],
) -> Result<usize, AppError> {
    let mut tx = pool.begin().await?;
    let mut tags = TagIndex::load(&mut tx, user_id, category_id).await?;
    let tag_id = tags.resolve(&mut tx, user_id, IMPORT_TAG).await?;

    let mut notes_created = 0usize;
    for (chat, messages) in chats {
        let thread_id = ensure_thread(&mut tx, user_id, tag_id, chat).await?;
        let mut newest: Option<DateTime<Utc>> = None;
        for message in messages {
            if message.body.trim().is_empty() {
                continue;
            }
            let note = Note::new(message.body.clone(), Source::WhatsApp)
                .map_err(|error| AppError::BadRequest(error.to_string()))?;
            let dedupe_key = format!("whatsapp:{}:{}", chat.jid, message.message_id);
            let inserted: Option<Uuid> = sqlx::query_scalar(
                "INSERT INTO notes \
                    (id, user_id, tag_id, kind, body, occurred_at, source, external_id, \
                     dedupe_key, thread_id, meta) \
                 VALUES ($1, $2, $3, 'text', $4, $5, 'whatsapp', $6, $7, $8, $9) \
                 ON CONFLICT (user_id, dedupe_key) DO NOTHING \
                 RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(user_id)
            .bind(tag_id)
            .bind(note.body())
            .bind(message.sent_at)
            .bind(&message.message_id)
            .bind(&dedupe_key)
            .bind(thread_id)
            .bind(serde_json::json!({
                "chat_jid": chat.jid.to_string(),
                "chat_name": chat.display_name,
                "from_me": message.from_me,
                "sender": message.sender,
            }))
            .fetch_optional(&mut *tx)
            .await?;
            if inserted.is_none() {
                continue;
            }
            notes_created += 1;
            newest = Some(match newest {
                Some(previous) => previous.max(message.sent_at),
                None => message.sent_at,
            });
        }
        if let Some(imported_through) = newest {
            sqlx::query(
                "UPDATE whatsapp_chats SET imported_through = $3 \
                 WHERE user_id = $1 AND jid = $2 \
                   AND (imported_through IS NULL OR imported_through < $3)",
            )
            .bind(user_id)
            .bind(chat.jid.as_str())
            .bind(imported_through)
            .execute(&mut *tx)
            .await?;
        }
    }

    sqlx::query("UPDATE connectors SET last_sync_at = now() WHERE user_id = $1 AND kind = $2")
        .bind(user_id)
        .bind(CONNECTOR_KIND)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(notes_created)
}

/// One thread per chat, reused across imports so the chat reads as a conversation.
async fn ensure_thread(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    tag_id: Uuid,
    chat: &StoredChat,
) -> Result<Uuid, AppError> {
    let title = chat
        .display_name
        .clone()
        .unwrap_or_else(|| chat.jid.to_string());
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM threads WHERE user_id = $1 AND tag_id = $2 AND title = $3 \
         ORDER BY updated_at LIMIT 1",
    )
    .bind(user_id)
    .bind(tag_id)
    .bind(&title)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(thread_id) = existing {
        return Ok(thread_id);
    }
    let thread_id = Uuid::new_v4();
    sqlx::query("INSERT INTO threads (id, user_id, tag_id, title) VALUES ($1, $2, $3, $4)")
        .bind(thread_id)
        .bind(user_id)
        .bind(tag_id)
        .bind(&title)
        .execute(&mut **tx)
        .await?;
    Ok(thread_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cipher() -> SessionCipher {
        SessionCipher::from_bytes(&[7u8; 32]).expect("32-byte key")
    }

    #[test]
    fn session_round_trips_through_the_cipher() {
        let cipher = cipher();
        let plaintext = b"device keys and pre-keys".to_vec();
        let sealed = cipher.seal(&plaintext).expect("seals");
        assert_ne!(sealed, plaintext, "ciphertext must not be the plaintext");
        assert_eq!(&sealed[..NONCE_BYTES], &sealed[..NONCE_BYTES]);
        assert_eq!(cipher.open(&sealed).expect("opens"), plaintext);
    }

    #[test]
    fn sealing_twice_uses_a_fresh_nonce() {
        let cipher = cipher();
        let first = cipher.seal(b"same plaintext").expect("seals");
        let second = cipher.seal(b"same plaintext").expect("seals");
        assert_ne!(first, second, "a reused nonce would leak the plaintext");
    }

    #[test]
    fn wrong_key_or_tampering_fails_closed() {
        let sealed = cipher().seal(b"session").expect("seals");
        let other = SessionCipher::from_bytes(&[9u8; 32]).expect("32-byte key");
        assert!(other.open(&sealed).is_err());

        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;
        assert!(cipher().open(&tampered).is_err());
        assert!(cipher().open(&[0u8; 4]).is_err());
    }

    #[test]
    fn key_must_be_exactly_32_bytes() {
        assert!(SessionCipher::from_bytes(&[1u8; 31]).is_none());
        assert!(SessionCipher::from_bytes(&[1u8; 33]).is_none());
        let encoded = BASE64.encode([3u8; 32]);
        assert!(SessionCipher::from_base64(&encoded).is_some());
        assert!(SessionCipher::from_base64("not base64").is_none());
    }
}
