//! Envelope encryption for connector credentials (ticket T14).
//!
//! `connector_secrets.ciphertext` is opaque bytes to the rest of the server: it
//! holds `nonce || ciphertext || tag` from AES-256-GCM under a key read from the
//! environment. The plaintext — an OAuth access token, in practice — exists only
//! inside this module's callers and is never logged.
//!
//! The key lives in `STRANDS_SECRET_KEY` (base64, 32 bytes). Rotating it makes
//! existing rows unreadable, which is the intended trade: a connector reconnects
//! rather than an old token lingering.

use aes_gcm::aead::{Aead, KeyInit, OsRng, rand_core::RngCore};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;

/// Environment variable holding the base64 key.
pub const KEY_ENV: &str = "STRANDS_SECRET_KEY";

const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("{KEY_ENV} is not valid base64")]
    Base64,
    #[error("{KEY_ENV} must decode to exactly 32 bytes")]
    KeyLength,
    #[error("the stored secret is truncated")]
    Truncated,
    #[error("the stored secret could not be decrypted")]
    Undecryptable,
    #[error("the secret could not be encrypted")]
    Unencryptable,
}

/// The connector-secret key, proven to be 32 bytes at construction.
pub struct SecretKey {
    cipher: Aes256Gcm,
}

impl SecretKey {
    pub fn from_base64(value: &str) -> Result<Self, SecretError> {
        let bytes = STANDARD
            .decode(value.trim())
            .map_err(|_| SecretError::Base64)?;
        let cipher = Aes256Gcm::new_from_slice(&bytes).map_err(|_| SecretError::KeyLength)?;
        Ok(Self { cipher })
    }

    /// `None` when the variable is unset; `Some(Err(..))` when it is unusable.
    pub fn from_env() -> Option<Result<Self, SecretError>> {
        std::env::var(KEY_ENV)
            .ok()
            .map(|value| Self::from_base64(&value))
    }

    /// Seal a plaintext the way the column stores it.
    pub fn seal(&self, plaintext: &str) -> Result<Vec<u8>, SecretError> {
        let mut nonce = [0u8; NONCE_BYTES];
        OsRng.fill_bytes(&mut nonce);
        let body = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext.as_bytes())
            .map_err(|_| SecretError::Unencryptable)?;
        let mut blob = Vec::with_capacity(NONCE_BYTES + body.len());
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&body);
        Ok(blob)
    }

    /// Open a stored secret back into its plaintext.
    pub fn open(&self, blob: &[u8]) -> Result<String, SecretError> {
        if blob.len() < NONCE_BYTES + TAG_BYTES {
            return Err(SecretError::Truncated);
        }
        let (nonce, body) = blob.split_at(NONCE_BYTES);
        let plaintext = self
            .cipher
            .decrypt(Nonce::from_slice(nonce), body)
            .map_err(|_| SecretError::Undecryptable)?;
        String::from_utf8(plaintext).map_err(|_| SecretError::Truncated)
    }
}

/// The access token behind the caller's connected connector of `kind`.
pub async fn access_token(
    pool: &PgPool,
    key: &SecretKey,
    user_id: Uuid,
    kind: &str,
) -> Result<Option<String>, AppError> {
    let ciphertext: Option<Vec<u8>> = sqlx::query_scalar(
        "SELECT s.ciphertext FROM connector_secrets s
           JOIN connectors c ON c.id = s.connector_id
          WHERE s.user_id = $1 AND c.kind = $2 AND c.status = 'connected'",
    )
    .bind(user_id)
    .bind(kind)
    .fetch_optional(pool)
    .await?;

    let Some(blob) = ciphertext else {
        return Ok(None);
    };
    key.open(&blob)
        .map(Some)
        .map_err(|error| AppError::Internal {
            context: "connector secret",
            source: sqlx::Error::Protocol(error.to_string()),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SecretKey {
        SecretKey::from_base64(&STANDARD.encode([7u8; 32])).expect("a 32-byte key")
    }

    #[test]
    fn a_sealed_secret_round_trips() {
        let key = key();
        let blob = key.seal("ya29.access-token").expect("seal");
        assert!(blob.len() > "ya29.access-token".len());
        assert_eq!(key.open(&blob).expect("open"), "ya29.access-token");
    }

    #[test]
    fn every_seal_uses_a_fresh_nonce() {
        let key = key();
        let first = key.seal("same").expect("seal");
        let second = key.seal("same").expect("seal");
        assert_ne!(first, second);
    }

    #[test]
    fn a_tampered_secret_does_not_open() {
        let key = key();
        let mut blob = key.seal("ya29.access-token").expect("seal");
        let last = blob.len() - 1;
        blob[last] ^= 0xff;
        assert!(matches!(key.open(&blob), Err(SecretError::Undecryptable)));
    }

    #[test]
    fn a_truncated_secret_does_not_open() {
        assert!(matches!(key().open(&[0u8; 8]), Err(SecretError::Truncated)));
    }

    #[test]
    fn another_key_cannot_open_it() {
        let blob = key().seal("ya29.access-token").expect("seal");
        let other = SecretKey::from_base64(&STANDARD.encode([9u8; 32])).expect("a 32-byte key");
        assert!(matches!(other.open(&blob), Err(SecretError::Undecryptable)));
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
}
