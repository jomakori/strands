//! Shared helper for the acceptance tests.
//!
//! Both `schema_shape` (T2) and `auth_isolation` (T3) assert against a live,
//! migrated database. The rule they share:
//!
//! * no `DATABASE_URL`, or one that cannot be reached ⇒ skip with a loud
//!   message, because a plain `cargo test` on a laptop has no Postgres;
//! * `STRANDS_TEST_REQUIRE_DB=1` ⇒ the environment promised a database, so
//!   failing to connect is a test failure. The CI steps set it, which is what
//!   stops a broken service container from passing as "skipped".

use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use strands_server::db;

/// Connect and migrate, or `None` when this environment has no database.
pub async fn connect(test_name: &str) -> Option<PgPool> {
    let database_url = std::env::var("DATABASE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty());

    let Some(database_url) = database_url else {
        eprintln!("skipping {test_name}: DATABASE_URL is not set (no live Postgres on this host)");
        return None;
    };

    let pool = match PgPoolOptions::new()
        .acquire_timeout(Duration::from_secs(3))
        .connect(&database_url)
        .await
    {
        Ok(pool) => pool,
        Err(error) if required() => {
            panic!("STRANDS_TEST_REQUIRE_DB is set but Postgres is unreachable: {error}")
        }
        Err(error) => {
            eprintln!("skipping {test_name}: cannot reach DATABASE_URL ({error})");
            return None;
        }
    };

    db::MIGRATOR.run(&pool).await.expect("apply migrations");

    Some(pool)
}

fn required() -> bool {
    std::env::var("STRANDS_TEST_REQUIRE_DB").is_ok_and(|value| value == "1")
}
