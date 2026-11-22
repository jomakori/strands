//! Envelope encryption for connector credentials (tickets T14, C2).
//!
//! `connector_secrets.ciphertext` is opaque bytes to the rest of the server: it
//! holds `nonce || ciphertext || tag` from AES-256-GCM. The plaintext — an OAuth
//! access token or a WhatsApp session blob, in practice — exists only inside
//! this module's callers and is never logged.
//!
//! The master key lives in `STRANDS_SECRET_KEY` (base64, 32 bytes). It is not
//! used to seal new rows directly: each account gets its own key, derived with
//! `HKDF-SHA256(master, info = user_id)`, so a blob sealed for one user cannot be
//! opened in another user's context even though one process holds one master.
//!
//! `connector_secrets.key_version` says which derivation sealed a row:
//!
//! * [`KEY_VERSION_GLOBAL`] (1) — the master key used directly, which is how
//!   every row written before C2 was sealed;
//! * [`KEY_VERSION_PER_USER`] (2) — the per-user derivation, which is what every
//!   new write uses.
//!
//! Reads dispatch on that column, so a v1 row still decrypts after the deploy
//! and is re-sealed as v2 the first time it is read (see [`access_token`]).
//! Rotating the master invalidates both versions, which is the intended trade: a
//! connector reconnects rather than an old token lingering.

use aes_gcm::aead::{Aead, KeyInit, OsRng, rand_core::RngCore};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ring::hkdf::{HKDF_SHA256, KeyType, Salt};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;

/// Environment variable holding the base64 master key.
pub const KEY_ENV: &str = "STRANDS_SECRET_KEY";

/// Sealed by the master key used directly, before per-account keys existed.
pub const KEY_VERSION_GLOBAL: i16 = 1;

/// Sealed by `HKDF-SHA256(master, info = user_id)`.
pub const KEY_VERSION_PER_USER: i16 = 2;

/// The version every new write uses.
pub const CURRENT_KEY_VERSION: i16 = KEY_VERSION_PER_USER;

/// Salt for the per-user derivation. Fixed and public: it separates this
/// derivation from any other use of the master key; the entropy is the master.
const HKDF_SALT: &[u8] = b"strands/connector-secret/v2";

const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;

/// A HKDF output length of 32 bytes — one AES-256 key.
struct Aes256KeyLen;

impl KeyType for Aes256KeyLen {
    fn len(&self) -> usize {
        32
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("{KEY_ENV} is not valid base64")]
    Base64,
    #[error("{KEY_ENV} must decode to exactly 32 bytes")]
    KeyLength,
    #[error("the stored secret is truncated")]
    Truncated,
    /// A v1 blob that will not open: the master key changed, or the bytes did.
    #[error(
        "the stored secret could not be decrypted; the master key differs from the one that sealed it"
    )]
    Undecryptable,
    /// A v2 blob that will not open under *this* account's key. A blob written
    /// for another account fails the tag check here, a tampered one fails it the
    /// same way, and a master-key rotation invalidates every row at once — all
    /// three are reported as this one error rather than a panic.
    #[error(
        "the stored secret could not be decrypted in this account's context; the master key changed, or the row belongs to another account"
    )]
    ForeignUser,
    #[error("the stored secret was sealed with unknown key version {0}")]
    UnknownVersion(i16),
    #[error("a per-account key could not be derived")]
    Derivation,
    #[error("the secret could not be encrypted")]
    Unencryptable,
}

/// The master connector-secret key, proven to be 32 bytes at construction.
pub struct SecretKey {
    /// Kept alongside the v1 cipher because every per-user key is derived from
    /// it on each seal and open.
    master: [u8; 32],
    /// The v1 cipher: the master key used directly.
    global: Aes256Gcm,
}

/// What a read produced: the plaintext, and the re-sealed bytes when the row was
/// still on an older key version and should be written back.
pub struct Opened {
    pub plaintext: Vec<u8>,
    pub upgraded: Option<Vec<u8>>,
}

impl SecretKey {
    pub fn from_base64(value: &str) -> Result<Self, SecretError> {
        let bytes = STANDARD
            .decode(value.trim())
            .map_err(|_| SecretError::Base64)?;
        Self::from_bytes(&bytes).ok_or(SecretError::KeyLength)
    }

    /// `None` unless `bytes` is exactly 32 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let master: [u8; 32] = bytes.try_into().ok()?;
        let global = Aes256Gcm::new_from_slice(&master).ok()?;
        Some(Self { master, global })
    }

    /// `None` when the variable is unset; `Some(Err(..))` when it is unusable.
    pub fn from_env() -> Option<Result<Self, SecretError>> {
        std::env::var(KEY_ENV)
            .ok()
            .map(|value| Self::from_base64(&value))
    }

    /// `HKDF-SHA256(master, info = user_id)`, as the account's AES-256 key.
    fn per_user_cipher(&self, user_id: Uuid) -> Result<Aes256Gcm, SecretError> {
        let prk = Salt::new(HKDF_SHA256, HKDF_SALT).extract(&self.master);
        let info: [&[u8]; 1] = [user_id.as_bytes()];
        let okm = prk
            .expand(&info, Aes256KeyLen)
            .map_err(|_| SecretError::Derivation)?;
        let mut derived = [0u8; 32];
        okm.fill(&mut derived)
            .map_err(|_| SecretError::Derivation)?;
        Aes256Gcm::new_from_slice(&derived).map_err(|_| SecretError::Derivation)
    }

    /// Seal `plaintext` for `user_id`: version 2, the only version written.
    pub fn seal(&self, user_id: Uuid, plaintext: &[u8]) -> Result<Vec<u8>, SecretError> {
        let cipher = self.per_user_cipher(user_id)?;
        seal_with(&cipher, plaintext)
    }

    /// Seal under the legacy global key. Tests use it to build v1 rows and to
    /// prove the old format still opens; production writes go through
    /// [`SecretKey::seal`].
    pub fn seal_v1(&self, plaintext: &[u8]) -> Result<Vec<u8>, SecretError> {
        seal_with(&self.global, plaintext)
    }

    /// Open a blob sealed with `key_version` in `user_id`'s context.
    pub fn open(
        &self,
        user_id: Uuid,
        key_version: i16,
        blob: &[u8],
    ) -> Result<Vec<u8>, SecretError> {
        match key_version {
            KEY_VERSION_GLOBAL => open_with(&self.global, blob, SecretError::Undecryptable),
            KEY_VERSION_PER_USER => {
                let cipher = self.per_user_cipher(user_id)?;
                open_with(&cipher, blob, SecretError::ForeignUser)
            }
            other => Err(SecretError::UnknownVersion(other)),
        }
    }

    /// Open a stored secret and, when it was sealed by an older version, produce
    /// the re-sealed bytes for the caller to write back.
    ///
    /// The upgrade is lazy by construction: nothing scans the table, so a row
    /// moves to v2 only when somebody actually reads it, one row at a time.
    pub fn open_upgrading(
        &self,
        user_id: Uuid,
        key_version: i16,
        blob: &[u8],
    ) -> Result<Opened, SecretError> {
        let plaintext = self.open(user_id, key_version, blob)?;
        let upgraded = if key_version == CURRENT_KEY_VERSION {
            None
        } else {
            Some(self.seal(user_id, &plaintext)?)
        };
        Ok(Opened {
            plaintext,
            upgraded,
        })
    }
}

fn seal_with(cipher: &Aes256Gcm, plaintext: &[u8]) -> Result<Vec<u8>, SecretError> {
    let mut nonce = [0u8; NONCE_BYTES];
    OsRng.fill_bytes(&mut nonce);
    let body = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| SecretError::Unencryptable)?;
    let mut blob = Vec::with_capacity(NONCE_BYTES + body.len());
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&body);
    Ok(blob)
}

fn open_with(
    cipher: &Aes256Gcm,
    blob: &[u8],
    failure: SecretError,
) -> Result<Vec<u8>, SecretError> {
    if blob.len() < NONCE_BYTES + TAG_BYTES {
        return Err(SecretError::Truncated);
    }
    let (nonce, body) = blob.split_at(NONCE_BYTES);
    cipher
        .decrypt(Nonce::from_slice(nonce), body)
        .map_err(|_| failure)
}

/// The access token behind the caller's connected connector of `kind`.
///
/// A row still on an older key version is re-sealed as v2 and written back here,
/// guarded on the version that was read, so two readers cannot fight over it and
/// the second write is a no-op.
pub async fn access_token(
    pool: &PgPool,
    key: &SecretKey,
    user_id: Uuid,
    kind: &str,
) -> Result<Option<String>, AppError> {
    let row: Option<(Uuid, Vec<u8>, i16)> = sqlx::query_as(
        "SELECT s.connector_id, s.ciphertext, s.key_version FROM connector_secrets s
           JOIN connectors c ON c.id = s.connector_id
          WHERE s.user_id = $1 AND c.kind = $2 AND c.status = 'connected'",
    )
    .bind(user_id)
    .bind(kind)
    .fetch_optional(pool)
    .await?;

    let Some((connector_id, blob, key_version)) = row else {
        return Ok(None);
    };

    let opened = key
        .open_upgrading(user_id, key_version, &blob)
        .map_err(|error| AppError::Internal {
            context: "connector secret",
            source: sqlx::Error::Protocol(error.to_string()),
        })?;

    if let Some(upgraded) = opened.upgraded {
        sqlx::query(
            "UPDATE connector_secrets SET ciphertext = $3, key_version = $4
              WHERE connector_id = $1 AND user_id = $2 AND key_version = $5",
        )
        .bind(connector_id)
        .bind(user_id)
        .bind(upgraded)
        .bind(CURRENT_KEY_VERSION)
        .bind(key_version)
        .execute(pool)
        .await?;
    }

    String::from_utf8(opened.plaintext)
        .map(Some)
        .map_err(|_| AppError::Internal {
            context: "connector secret",
            source: sqlx::Error::Protocol(SecretError::Truncated.to_string()),
        })
}

#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::STANDARD;

    use super::*;

    fn key() -> SecretKey {
        SecretKey::from_base64(&STANDARD.encode([7u8; 32])).expect("a 32-byte key")
    }

    fn alice() -> Uuid {
        Uuid::from_u128(1)
    }

    fn bob() -> Uuid {
        Uuid::from_u128(2)
    }

    #[test]
    fn a_sealed_secret_round_trips() {
        let key = key();
        let blob = key.seal(alice(), b"ya29.access-token").expect("seal");
        assert!(blob.len() > "ya29.access-token".len());
        assert_eq!(
            key.open(alice(), KEY_VERSION_PER_USER, &blob)
                .expect("open"),
            b"ya29.access-token"
        );
    }

    #[test]
    fn every_seal_uses_a_fresh_nonce() {
        let key = key();
        let first = key.seal(alice(), b"same").expect("seal");
        let second = key.seal(alice(), b"same").expect("seal");
        assert_ne!(first, second);
    }

    #[test]
    fn a_tampered_secret_does_not_open() {
        let key = key();
        let mut blob = key.seal(alice(), b"ya29.access-token").expect("seal");
        let last = blob.len() - 1;
        blob[last] ^= 0xff;
        assert!(matches!(
            key.open(alice(), KEY_VERSION_PER_USER, &blob),
            Err(SecretError::ForeignUser)
        ));
    }

    #[test]
    fn a_truncated_secret_does_not_open() {
        assert!(matches!(
            key().open(alice(), KEY_VERSION_PER_USER, &[0u8; 8]),
            Err(SecretError::Truncated)
        ));
    }

    #[test]
    fn another_key_cannot_open_it() {
        let blob = key().seal(alice(), b"ya29.access-token").expect("seal");
        let other = SecretKey::from_base64(&STANDARD.encode([9u8; 32])).expect("a 32-byte key");
        assert!(matches!(
            other.open(alice(), KEY_VERSION_PER_USER, &blob),
            Err(SecretError::ForeignUser)
        ));
    }

    #[test]
    fn a_short_key_is_refused() {
        assert!(matches!(
            SecretKey::from_base64(&STANDARD.encode([7u8; 16])),
            Err(SecretError::KeyLength)
        ));
    }

    #[test]
    fn a_non_base64_key_is_refused() {
        assert!(matches!(
            SecretKey::from_base64("not base64!"),
            Err(SecretError::Base64)
        ));
    }

    /// The point of C2: the derivation, not just the master key, decides who can
    /// read a row. B's context rejects A's blob with a typed error, not a panic.
    #[test]
    fn per_user_key_rejects_foreign_user() {
        let key = key();
        let blob = key.seal(alice(), b"ya29.alice-token").expect("seal");

        assert_eq!(
            key.open(alice(), KEY_VERSION_PER_USER, &blob)
                .expect("alice opens her own"),
            b"ya29.alice-token"
        );
        assert!(matches!(
            key.open(bob(), KEY_VERSION_PER_USER, &blob),
            Err(SecretError::ForeignUser)
        ));
    }

    /// Rows written before C2 were sealed under the global key and must keep
    /// opening after the deploy.
    #[test]
    fn v1_blob_still_decrypts() {
        let key = key();
        let blob = key.seal_v1(b"ya29.legacy-token").expect("seal");

        for user in [alice(), bob()] {
            assert_eq!(
                key.open(user, KEY_VERSION_GLOBAL, &blob)
                    .expect("a v1 row opens for the account that owns it"),
                b"ya29.legacy-token"
            );
        }
    }

    /// A v1 read hands back v2 bytes, and those bytes open only in the owner's
    /// context — which is what the write-back in `access_token` stores.
    #[test]
    fn lazy_upgrade_writes_v2() {
        let key = key();
        let v1 = key.seal_v1(b"ya29.legacy-token").expect("seal");

        let opened = key
            .open_upgrading(alice(), KEY_VERSION_GLOBAL, &v1)
            .expect("open");
        assert_eq!(opened.plaintext, b"ya29.legacy-token");
        let upgraded = opened.upgraded.expect("a v1 row upgrades");

        assert_eq!(
            key.open(alice(), KEY_VERSION_PER_USER, &upgraded)
                .expect("v2 opens for alice"),
            b"ya29.legacy-token"
        );
        assert!(matches!(
            key.open(bob(), KEY_VERSION_PER_USER, &upgraded),
            Err(SecretError::ForeignUser)
        ));

        // A v2 row is left alone: the upgrade happens once.
        assert!(
            key.open_upgrading(alice(), KEY_VERSION_PER_USER, &upgraded)
                .expect("open")
                .upgraded
                .is_none(),
            "a v2 row must not be re-sealed on read"
        );
    }

    #[test]
    fn an_unknown_key_version_is_refused() {
        let blob = key().seal(alice(), b"x").expect("seal");
        assert!(matches!(
            key().open(alice(), 3, &blob),
            Err(SecretError::UnknownVersion(3))
        ));
    }
}
