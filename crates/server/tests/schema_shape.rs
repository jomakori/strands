//! Schema-shape acceptance test (ticket T2): "a repo test asserts every
//! user-scoped table has `user_id`" (spec §6 closing line: "Every table
//! carries `user_id` and every query filters on it").
//!
//! This queries `information_schema` against a live, migrated database —
//! it is not a hardcoded list of table names, so it fails the moment a new
//! migration adds a table without `user_id`. It needs a real Postgres
//! (`DATABASE_URL`), which this host does not have, so it skips cleanly
//! when that variable is unset rather than failing the whole suite; CI's
//! `schema` job (.github/workflows/lint-test.yml) provides a postgres:17
//! service container and sets `DATABASE_URL` so this assertion actually runs.

use strands_server::db;

#[tokio::test]
async fn every_user_scoped_table_has_a_user_id_column_or_db_unset_skip() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!(
            "skipping every_user_scoped_table_has_a_user_id_column_or_db_unset_skip: \
             DATABASE_URL is not set (no live Postgres on this host)"
        );
        return;
    };

    let pool = db::connect_and_migrate(&database_url)
        .await
        .expect("connect to Postgres and apply migrations");

    // Not "user-scoped" by design, not by omission:
    // - `users` is the identity root; its own primary key *is* the user id.
    // - `_sqlx_migrations` is sqlx's own bookkeeping table, not domain data.
    const NOT_USER_SCOPED: &[&str] = &["users", "_sqlx_migrations"];

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_type = 'BASE TABLE' \
         ORDER BY table_name",
    )
    .fetch_all(&pool)
    .await
    .expect("list base tables in the public schema");

    assert!(
        tables.len() > NOT_USER_SCOPED.len(),
        "expected migrations to have created domain tables, found only {tables:?}"
    );

    let mut missing_user_id = Vec::new();
    for table in &tables {
        if NOT_USER_SCOPED.contains(&table.as_str()) {
            continue;
        }

        let has_user_id: bool = sqlx::query_scalar(
            "SELECT EXISTS ( \
                SELECT 1 FROM information_schema.columns \
                WHERE table_schema = 'public' \
                  AND table_name = $1 \
                  AND column_name = 'user_id' \
             )",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .expect("check for a user_id column");

        if !has_user_id {
            missing_user_id.push(table.clone());
        }
    }

    assert!(
        missing_user_id.is_empty(),
        "user-scoped tables missing a user_id column: {missing_user_id:?}"
    );
}
