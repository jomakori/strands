//! Every SQL statement the auth flow runs (ticket T3).
//!
//! Runtime-checked `sqlx` calls only (`query`/`query_as`, no `query!` macros),
//! so building this crate needs no `DATABASE_URL`. Each function takes the pool
//! explicitly and there is no ambient "current user" — the user id is always a
//! parameter, which is what makes the scoping auditable.

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use strands_core::{SessionId, UserId};
use uuid::Uuid;

use super::identity::Identity;

/// How long a half-finished login may sit in `oauth_requests`.
pub const OAUTH_REQUEST_TTL: Duration = Duration::minutes(10);

/// A `state` row claimed by the callback: single use, already deleted.
pub struct PendingOAuth {
    pub pkce_verifier: String,
    pub redirect_uri: String,
}

/// Create the user on first login, refresh the profile on later ones.
///
/// Keyed on `google_sub` (which since T20c holds the *issuer's* `sub` — Dex's,
/// not Google's), never on email: the email is mutable profile data and
/// reassigning it must not silently hand one account's data to another person.
/// The column keeps its historical name because renaming it would rewrite the
/// schema and every test insert for no behavioural gain.
///
/// `users.email` is `UNIQUE`, so a provider that reuses an email across two
/// subjects makes this fail loudly (500) instead of merging two people.
///
/// `users.provider` is left at its migration-0007 default (`google`): it records
/// which upstream vouched for the address, and putting a broker in front of that
/// upstream does not change the answer. Rewriting it would touch every existing
/// row and the account-export format for a label.
pub async fn upsert_user(pool: &PgPool, identity: &Identity) -> Result<UserId, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO users (id, google_sub, email, name, avatar_url) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (google_sub) DO UPDATE \
            SET email = EXCLUDED.email, \
                name = EXCLUDED.name, \
                avatar_url = EXCLUDED.avatar_url \
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(&identity.subject)
    .bind(&identity.email)
    .bind(&identity.name)
    .bind(&identity.avatar_url)
    .fetch_one(pool)
    .await
}

/// Give a brand-new user the five starting categories (spec §6: "categories —
/// seeded 5 on first login", names from the home-grid mockup in spec §4).
///
/// "First login" is expressed as "this user has no categories yet", so the call
/// is idempotent and self-healing: re-running it, or a login racing another,
/// still leaves exactly five rows. Colours are ours to choose; the spec fixes
/// the names and the order, not the palette.
pub async fn seed_default_categories(pool: &PgPool, user_id: UserId) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO categories (id, user_id, name, color, position) \
         SELECT gen_random_uuid(), $1, defaults.name, defaults.color, defaults.position \
         FROM (VALUES \
            ('Travel',  '#2F6F4F', 0), \
            ('Tech',    '#2F5D8C', 1), \
            ('Food',    '#8C4A2F', 2), \
            ('Reading', '#5B4A8C', 3), \
            ('Ideas',   '#7A6A2F', 4) \
         ) AS defaults (name, color, position) \
         WHERE NOT EXISTS (SELECT 1 FROM categories WHERE user_id = $1)",
    )
    .bind(user_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Insert a session row and return its id — the value the cookie carries.
pub async fn create_session(
    pool: &PgPool,
    user_id: UserId,
    ttl: Duration,
) -> Result<SessionId, sqlx::Error> {
    let session_id = Uuid::new_v4();

    sqlx::query("DELETE FROM sessions WHERE user_id = $1 AND expires_at <= now()")
        .bind(user_id)
        .execute(pool)
        .await?;

    sqlx::query("INSERT INTO sessions (id, user_id, expires_at) VALUES ($1, $2, $3)")
        .bind(session_id)
        .bind(user_id)
        .bind(Utc::now() + ttl)
        .execute(pool)
        .await?;

    Ok(session_id)
}

/// Resolve a session id to its user, or `None` when it is unknown or expired.
///
/// The expiry filter lives in SQL, not in Rust: a row that has aged out is
/// indistinguishable from one that never existed, so both produce a 401.
pub async fn find_session_user(
    pool: &PgPool,
    session_id: SessionId,
) -> Result<Option<UserId>, sqlx::Error> {
    sqlx::query_scalar("SELECT user_id FROM sessions WHERE id = $1 AND expires_at > now()")
        .bind(session_id)
        .fetch_optional(pool)
        .await
}

/// Revoke a session (logout). Deleting a row that is already gone is fine.
pub async fn delete_session(pool: &PgPool, session_id: SessionId) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM sessions WHERE id = $1")
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Persist the state token, PKCE verifier and exact `redirect_uri` before the
/// browser is sent to the provider.
pub async fn store_oauth_request(
    pool: &PgPool,
    state: &str,
    pkce_verifier: &str,
    redirect_uri: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    // Cheap sweep so abandoned logins do not accumulate.
    sqlx::query("DELETE FROM oauth_requests WHERE expires_at <= now()")
        .execute(pool)
        .await?;

    sqlx::query(
        "INSERT INTO oauth_requests (id, state, pkce_verifier, redirect_uri, expires_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(state)
    .bind(pkce_verifier)
    .bind(redirect_uri)
    .bind(now + OAUTH_REQUEST_TTL)
    .execute(pool)
    .await?;

    Ok(())
}

/// Claim a `state` row: deleted as it is read, so a replayed callback finds
/// nothing. Returns `None` for unknown, already-used or expired state.
pub async fn take_oauth_request(
    pool: &PgPool,
    state: &str,
) -> Result<Option<PendingOAuth>, sqlx::Error> {
    let row: Option<(String, String)> = sqlx::query_as(
        "DELETE FROM oauth_requests WHERE state = $1 AND expires_at > now() \
         RETURNING pkce_verifier, redirect_uri",
    )
    .bind(state)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|(pkce_verifier, redirect_uri)| PendingOAuth {
        pkce_verifier,
        redirect_uri,
    }))
}

/// The `users` row behind a session, for `GET /api/v1/me`.
pub struct Profile {
    pub id: UserId,
    pub email: String,
    pub name: String,
    pub avatar_url: Option<String>,
    pub provider: String,
}

pub async fn find_profile(pool: &PgPool, user_id: UserId) -> Result<Option<Profile>, sqlx::Error> {
    let row: Option<(UserId, String, String, Option<String>, String)> =
        sqlx::query_as("SELECT id, email, name, avatar_url, provider FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(pool)
            .await?;

    Ok(row.map(|(id, email, name, avatar_url, provider)| Profile {
        id,
        email,
        name,
        avatar_url,
        provider,
    }))
}
