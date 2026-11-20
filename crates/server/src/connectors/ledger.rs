//! Connector import dedupe ledger (T33b): `connector_items`.
//!
//! One row per `(user_id, connector, item_key)` an import has ever written.
//! `existing_keys` is a pure read a connector's `preview` can use to flag
//! `already_imported` without writing anything; `record_import` takes the
//! caller's own transaction, so the ledger row and the strand (note/thread)
//! it protects commit — or roll back — together. A crash between the two is
//! impossible because there is no "between": they are one transaction.

use std::collections::HashSet;

use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// Hex-encoded SHA-256 of `bytes`, for the ledger's `source_hash` column.
///
/// Not a dedupe key (that is `item_key`): this is a content fingerprint a
/// future slice can compare to notice "the upstream item changed" rather than
/// "this item was already imported".
pub fn source_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Which of `keys` this user has already imported through `connector`.
///
/// Pure read against the pool — no transaction, nothing written. Intended
/// for `preview`, which must never mutate the ledger.
pub async fn existing_keys(
    pool: &PgPool,
    user_id: Uuid,
    connector: &str,
    keys: &[String],
) -> Result<HashSet<String>, sqlx::Error> {
    if keys.is_empty() {
        return Ok(HashSet::new());
    }
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT item_key FROM connector_items \
          WHERE user_id = $1 AND connector = $2 AND item_key = ANY($3)",
    )
    .bind(user_id)
    .bind(connector)
    .bind(keys)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Record that `item_key` was imported, inside the caller's transaction.
///
/// Call this in the exact transaction that writes the strand (note/thread)
/// it protects, right after that insert succeeds — never on a separate
/// connection. Returns whether a row was actually inserted: `ON CONFLICT DO
/// NOTHING` makes a second call for the same key a no-op (`false`) rather
/// than an error, matching the unique constraint that is the real guarantee
/// against a double import.
pub async fn record_import(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    connector: &str,
    item_key: &str,
    source_hash: &str,
    thread_id: Option<Uuid>,
) -> Result<bool, sqlx::Error> {
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO connector_items (id, user_id, connector, item_key, source_hash, thread_id) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (user_id, connector, item_key) DO NOTHING \
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .bind(connector)
    .bind(item_key)
    .bind(source_hash)
    .bind(thread_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(inserted.is_some())
}

#[cfg(test)]
mod tests {
    use sqlx::postgres::PgPoolOptions;

    use super::*;

    /// `connect_lazy` never opens a socket, so this exercises the
    /// empty-input short circuit — the one behaviour `existing_keys` has
    /// that needs no live database — without a Postgres instance.
    #[tokio::test]
    async fn existing_keys_of_an_empty_slice_never_touches_the_pool() {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://unreachable-host-for-this-test/db")
            .expect("lazy pool construction never connects");
        let keys = existing_keys(&pool, Uuid::new_v4(), "whatsapp", &[])
            .await
            .expect("empty input short-circuits before any query");
        assert!(keys.is_empty());
    }
}
