//! Connector OAuth connect state and stored tokens (ticket B1).
//!
//! Connecting a connector starts from a signed-in user, so unlike the login
//! flow's `oauth_requests` the pending grant is scoped to that user: a `state`
//! issued for one account cannot be claimed by another. The state row carries
//! the PKCE verifier the challenge was derived from and the connector `kind` it
//! was issued for.
//!
//! Rows are single use. [`take_connect`] deletes as it reads, so a replayed
//! callback finds nothing; [`begin_connect`] sweeps rows that already expired so
//! abandoned connects do not accumulate.
//!
//! [`store_tokens`]/[`load_tokens`] are the model for the token columns added by
//! migration `0014`: an access token (the existing sealed `ciphertext`), an
//! optional refresh token sealed under the same per-account key, and an optional
//! expiry. A row written before those columns existed — the WhatsApp session
//! blob — reads back with `refresh_token: None` and `expires_at: None`.

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use strands_core::UserId;
use uuid::Uuid;

use crate::auth::token::random_token;
use crate::error::AppError;
use crate::secrets::{CURRENT_KEY_VERSION, SecretError, SecretKey};

/// How long a half-finished connect may sit in `connector_oauth_states`.
/// Same window as a half-finished login.
pub const STATE_TTL: Duration = Duration::minutes(10);

/// A claimed `state` row: already deleted, so it cannot be claimed twice.
pub struct PendingConnect {
    pub user_id: UserId,
    pub kind: String,
    pub pkce_verifier: String,
}

/// Issue a `state` for `user_id`'s attempt to connect a `kind` connector.
///
/// Returns the value to send to the provider as `state`. `pkce_verifier` is
/// stored beside it; the challenge derived from it goes in the consent URL.
pub async fn begin_connect(
    pool: &PgPool,
    user_id: UserId,
    kind: &str,
    pkce_verifier: &str,
    now: DateTime<Utc>,
) -> Result<String, sqlx::Error> {
    // Cheap sweep so abandoned connects do not accumulate.
    sqlx::query("DELETE FROM connector_oauth_states WHERE expires_at <= now()")
        .execute(pool)
        .await?;

    let state = random_token();
    sqlx::query(
        "INSERT INTO connector_oauth_states (state, user_id, kind, pkce_verifier, expires_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(&state)
    .bind(user_id)
    .bind(kind)
    .bind(pkce_verifier)
    .bind(now + STATE_TTL)
    .execute(pool)
    .await?;

    Ok(state)
}

/// Claim a `state` row: deleted as it is read, so a replayed callback finds
/// nothing. `None` for unknown, already-used or expired state.
pub async fn take_connect(
    pool: &PgPool,
    state: &str,
) -> Result<Option<PendingConnect>, sqlx::Error> {
    let row: Option<(UserId, String, String)> = sqlx::query_as(
        "DELETE FROM connector_oauth_states WHERE state = $1 AND expires_at > now() \
         RETURNING user_id, kind, pkce_verifier",
    )
    .bind(state)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|(user_id, kind, pkce_verifier)| PendingConnect {
        user_id,
        kind,
        pkce_verifier,
    }))
}

/// What a provider grant gives us, in the clear, on the way in or out.
///
/// `refresh_token` and `expires_at` are optional because not every grant has
/// them: Google omits the refresh token on a repeat consent unless asked for
/// offline access, and a long-lived session blob has no expiry at all.
pub struct ConnectorTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Seal and write the tokens for `connector_id`, replacing whatever was there.
///
/// Both secrets are sealed under the caller's per-account key, and the row's
/// `key_version` is set to the version currently written, so a row reconnected
/// by this build never reads back through the legacy global key.
pub async fn store_tokens(
    pool: &PgPool,
    key: &SecretKey,
    connector_id: Uuid,
    user_id: UserId,
    tokens: &ConnectorTokens,
) -> Result<(), AppError> {
    let ciphertext = seal(key, user_id, &tokens.access_token)?;
    let refresh = match &tokens.refresh_token {
        Some(token) => Some(seal(key, user_id, token)?),
        None => None,
    };

    sqlx::query(
        "INSERT INTO connector_secrets \
             (connector_id, user_id, ciphertext, key_version, refresh_token_encrypted, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (connector_id) DO UPDATE SET \
             ciphertext = EXCLUDED.ciphertext, \
             key_version = EXCLUDED.key_version, \
             refresh_token_encrypted = EXCLUDED.refresh_token_encrypted, \
             expires_at = EXCLUDED.expires_at \
         WHERE connector_secrets.user_id = EXCLUDED.user_id",
    )
    .bind(connector_id)
    .bind(user_id)
    .bind(ciphertext)
    .bind(CURRENT_KEY_VERSION)
    .bind(refresh)
    .bind(tokens.expires_at)
    .execute(pool)
    .await?;

    Ok(())
}

/// One `connector_secrets` row, as it comes back from Postgres.
#[derive(sqlx::FromRow)]
struct TokenRow {
    ciphertext: Vec<u8>,
    key_version: i16,
    refresh_token_encrypted: Option<Vec<u8>>,
    expires_at: Option<DateTime<Utc>>,
}

/// The tokens stored for `connector_id`, opened in `user_id`'s context.
///
/// Returns `None` when the connector has no secret row. A row written before
/// migration `0014` opens with the two new fields empty rather than failing.
pub async fn load_tokens(
    pool: &PgPool,
    key: &SecretKey,
    connector_id: Uuid,
    user_id: UserId,
) -> Result<Option<ConnectorTokens>, AppError> {
    let row: Option<TokenRow> = sqlx::query_as::<_, TokenRow>(
        "SELECT ciphertext, key_version, refresh_token_encrypted, expires_at \
           FROM connector_secrets WHERE connector_id = $1 AND user_id = $2",
    )
    .bind(connector_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    let access_token = open_text(key, user_id, row.key_version, &row.ciphertext)?;
    let refresh_token = match row.refresh_token_encrypted {
        Some(blob) => Some(open_text(key, user_id, row.key_version, &blob)?),
        None => None,
    };

    Ok(Some(ConnectorTokens {
        access_token,
        refresh_token,
        expires_at: row.expires_at,
    }))
}

fn seal(key: &SecretKey, user_id: UserId, plaintext: &str) -> Result<Vec<u8>, AppError> {
    key.seal(user_id, plaintext.as_bytes())
        .map_err(secret_failure)
}

fn open_text(
    key: &SecretKey,
    user_id: UserId,
    key_version: i16,
    blob: &[u8],
) -> Result<String, AppError> {
    let plaintext = key
        .open(user_id, key_version, blob)
        .map_err(secret_failure)?;
    String::from_utf8(plaintext).map_err(|_| secret_failure(SecretError::Truncated))
}

/// A secret that will not seal or open is a server-side problem with no client
/// action attached, so it is reported as a bare 500 like `secrets::access_token`
/// does. The detail stays in the log, never in the response.
fn secret_failure(error: SecretError) -> AppError {
    AppError::Internal {
        context: "connector tokens",
        source: sqlx::Error::Protocol(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration as StdDuration;

    use chrono::SubsecRound;
    use sqlx::PgPool;
    use sqlx::postgres::PgPoolOptions;

    use super::*;

    /// Same skip-or-fail contract as `tests/common`: a laptop without Postgres
    /// skips loudly, CI (`STRANDS_TEST_REQUIRE_DB=1`) fails instead of skipping.
    async fn pool_or_skip(test_name: &str) -> Option<PgPool> {
        let url = std::env::var("DATABASE_URL")
            .ok()
            .filter(|url| !url.trim().is_empty());
        let Some(url) = url else {
            eprintln!(
                "skipping {test_name}: DATABASE_URL is not set (no live Postgres on this host)"
            );
            return None;
        };

        let required = std::env::var("STRANDS_TEST_REQUIRE_DB").is_ok_and(|value| value == "1");

        let pool = match PgPoolOptions::new()
            .acquire_timeout(StdDuration::from_secs(3))
            .connect(&url)
            .await
        {
            Ok(pool) => pool,
            Err(error) if required => {
                panic!("STRANDS_TEST_REQUIRE_DB is set but Postgres is unreachable: {error}")
            }
            Err(error) => {
                eprintln!("skipping {test_name}: cannot reach DATABASE_URL ({error})");
                return None;
            }
        };

        crate::db::MIGRATOR
            .run(&pool)
            .await
            .expect("apply migrations");
        Some(pool)
    }

    fn key() -> SecretKey {
        SecretKey::from_bytes(&[7u8; 32]).expect("a 32-byte key")
    }

    async fn account(pool: &PgPool) -> UserId {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, google_sub, email, name) VALUES ($1, $2, $3, $4)")
            .bind(id)
            .bind(format!("sub-{id}"))
            .bind(format!("{id}@example.test"))
            .bind("OAuth Test")
            .execute(pool)
            .await
            .expect("insert user");
        id
    }

    async fn connector(pool: &PgPool, user_id: UserId) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO connectors (id, user_id, kind, status) VALUES ($1, $2, 'photos', 'connected')",
        )
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await
        .expect("insert connector");
        id
    }

    /// B1 acceptance: migration 0014 applies to a clean database and leaves the
    /// shape the rest of the ticket assumes — a user-scoped state table plus the
    /// two optional token columns.
    #[tokio::test]
    async fn migration_applies_on_a_clean_database() {
        let Some(pool) = pool_or_skip("oauth::tests::migration_applies_on_a_clean_database").await
        else {
            return;
        };

        // `MIGRATOR.run` in `pool_or_skip` is a no-op when everything is
        // applied, so a second run here proves the file is recorded as applied
        // rather than replayed.
        crate::db::MIGRATOR
            .run(&pool)
            .await
            .expect("migrations are idempotent");

        let applied: Option<(bool,)> =
            sqlx::query_as("SELECT success FROM _sqlx_migrations WHERE version = 14")
                .fetch_optional(&pool)
                .await
                .expect("read applied migrations");
        assert_eq!(
            applied,
            Some((true,)),
            "migration 0014 must be recorded as applied, not failed"
        );

        let columns: Vec<(String, String)> = sqlx::query_as(
            "SELECT table_name, column_name FROM information_schema.columns \
              WHERE table_schema = 'public' \
                AND ((table_name = 'connector_oauth_states') \
                  OR (table_name = 'connector_secrets' \
                      AND column_name IN ('refresh_token_encrypted', 'expires_at'))) \
              ORDER BY table_name, column_name",
        )
        .fetch_all(&pool)
        .await
        .expect("read column metadata");

        let names: Vec<String> = columns
            .iter()
            .map(|(table, column)| format!("{table}.{column}"))
            .collect();
        for expected in [
            "connector_oauth_states.expires_at",
            "connector_oauth_states.kind",
            "connector_oauth_states.pkce_verifier",
            "connector_oauth_states.state",
            "connector_oauth_states.user_id",
            "connector_secrets.expires_at",
            "connector_secrets.refresh_token_encrypted",
        ] {
            assert!(
                names.iter().any(|name| name == expected),
                "missing {expected} after 0014; found {names:?}"
            );
        }
    }

    /// B1 acceptance: the state row round-trips and is consumable exactly once.
    #[tokio::test]
    async fn state_is_single_use() {
        let Some(pool) = pool_or_skip("oauth::tests::state_is_single_use").await else {
            return;
        };

        let user_id = account(&pool).await;
        let now = Utc::now();
        let state = begin_connect(&pool, user_id, "photos", "verifier-abc", now)
            .await
            .expect("issue state");
        assert_eq!(state.len(), 64, "state is two v4 UUIDs of hex");

        let claimed = take_connect(&pool, &state)
            .await
            .expect("claim state")
            .expect("an unexpired state is claimable");
        assert_eq!(claimed.user_id, user_id);
        assert_eq!(claimed.kind, "photos");
        assert_eq!(claimed.pkce_verifier, "verifier-abc");

        assert!(
            take_connect(&pool, &state)
                .await
                .expect("second claim")
                .is_none(),
            "a replayed callback must find nothing"
        );

        // An expired state is refused and swept by the next issue.
        let stale = begin_connect(
            &pool,
            user_id,
            "photos",
            "verifier-old",
            now - STATE_TTL * 2,
        )
        .await
        .expect("issue stale state");
        assert!(
            take_connect(&pool, &stale)
                .await
                .expect("claim stale")
                .is_none(),
            "an expired state must not be claimable"
        );
        let _ = begin_connect(&pool, user_id, "photos", "verifier-new", now)
            .await
            .expect("issue after sweep");
        let swept: i64 =
            sqlx::query_scalar("SELECT count(*) FROM connector_oauth_states WHERE state = $1")
                .bind(&stale)
                .fetch_one(&pool)
                .await
                .expect("count stale rows");
        assert_eq!(swept, 0, "issuing a state sweeps the expired ones");
    }

    /// B1 acceptance: a row written before the new columns existed still opens,
    /// and a row written through the new model round-trips both of them.
    #[tokio::test]
    async fn existing_connector_tokens_survive_migration() {
        let Some(pool) =
            pool_or_skip("oauth::tests::existing_connector_tokens_survive_migration").await
        else {
            return;
        };

        let key = key();
        let user_id = account(&pool).await;
        let legacy = connector(&pool, user_id).await;

        // Exactly the insert shape the pre-0014 schema allowed: three columns,
        // sealed under the legacy global key the way an old row was.
        sqlx::query(
            "INSERT INTO connector_secrets (connector_id, user_id, ciphertext) VALUES ($1, $2, $3)",
        )
        .bind(legacy)
        .bind(user_id)
        .bind(key.seal_v1(b"ya29.legacy-access").expect("seal"))
        .execute(&pool)
        .await
        .expect("old-shaped insert still valid");

        let read = load_tokens(&pool, &key, legacy, user_id)
            .await
            .expect("load legacy tokens")
            .expect("the row exists");
        assert_eq!(read.access_token, "ya29.legacy-access");
        assert_eq!(read.refresh_token, None);
        assert_eq!(read.expires_at, None);

        // A reconnect writes over the same row — `connectors` allows one row per
        // `(user_id, kind)` — and carries the refresh token and the expiry.
        // Postgres keeps microseconds, so the value we compare against is
        // truncated the same way.
        let expires_at = (Utc::now() + Duration::hours(1)).trunc_subsecs(6);
        store_tokens(
            &pool,
            &key,
            legacy,
            user_id,
            &ConnectorTokens {
                access_token: "ya29.fresh-access".to_string(),
                refresh_token: Some("1//refresh".to_string()),
                expires_at: Some(expires_at),
            },
        )
        .await
        .expect("store tokens");

        let read = load_tokens(&pool, &key, legacy, user_id)
            .await
            .expect("load fresh tokens")
            .expect("the row exists");
        assert_eq!(read.access_token, "ya29.fresh-access");
        assert_eq!(read.refresh_token.as_deref(), Some("1//refresh"));
        assert_eq!(read.expires_at, Some(expires_at));
    }
}
