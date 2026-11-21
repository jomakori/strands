//! Connector import dedupe ledger (T33b): `connector_items`.
//!
//! One row per `(user_id, connector, item_key)` an import has ever written.
//! `existing_keys` is a pure read a connector's `preview` can use to flag
//! `already_imported` without writing anything; `record_import` takes the
//! caller's own transaction, so the ledger row and the strand (note/thread)
//! it protects commit — or roll back — together. A crash between the two is
//! impossible because there is no "between": they are one transaction.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
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

/// One run's row in `GET .../history`: every `connector_items` row sharing the
/// same `imported_at` truncated to the second groups into one run, because
/// rows a single `import` call writes land in the same transaction and
/// therefore the same wall-clock second. There is no run id column (T33c
/// scope keeps the migration as-is), so this grouping is the only "run"
/// boundary available.
pub struct HistoryRun {
    pub imported_at: DateTime<Utc>,
    pub imported: usize,
}

/// This user's import runs for `connector`, newest first.
///
/// A skip (an item already in the ledger) writes no row at all —
/// `record_import`'s `ON CONFLICT DO NOTHING` is exactly what makes a repeat
/// import a no-op — so there is nothing in `connector_items` to recover a
/// historical skip count from; the caller reports `skipped: 0` for every run.
pub async fn history(
    pool: &PgPool,
    user_id: Uuid,
    connector: &str,
) -> Result<Vec<HistoryRun>, sqlx::Error> {
    let rows: Vec<(DateTime<Utc>, i64)> = sqlx::query_as(
        "SELECT date_trunc('second', imported_at) AS run_at, count(*) \
          FROM connector_items \
          WHERE user_id = $1 AND connector = $2 \
          GROUP BY run_at \
          ORDER BY run_at DESC",
    )
    .bind(user_id)
    .bind(connector)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(imported_at, imported)| HistoryRun {
            imported_at,
            imported: imported as usize,
        })
        .collect())
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
