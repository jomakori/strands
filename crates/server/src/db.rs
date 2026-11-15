//! Database connection pool + embedded migrations (ticket T2, spec §6, §9.4:
//! "migrations run on deploy via `sqlx migrate run` in an init container or
//! entrypoint"). Embedding means the server binary applies its own schema on
//! boot — no separate migration step in the deploy pipeline.
//!
//! Uses runtime-checked `sqlx` calls only (no `query!`/`query_as!` macros),
//! so building this crate needs no `DATABASE_URL` / live database.

use sqlx::PgPool;
use sqlx::migrate::Migrator;

/// Migrations under `migrations/` at the repo root, embedded at compile time.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// Connect to Postgres and apply every pending migration.
pub async fn connect_and_migrate(database_url: &str) -> Result<PgPool, sqlx::Error> {
    let pool = PgPool::connect(database_url).await?;
    MIGRATOR.run(&pool).await?;
    Ok(pool)
}
