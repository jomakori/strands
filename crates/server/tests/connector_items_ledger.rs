//! `connector_items` dedupe ledger acceptance (ticket T33b).
//!
//! Persistence only: no HTTP route exercises `connector_items` directly yet
//! (that is T33c), so this drives the already-wired WhatsApp import path
//! (`whatsapp::store::write_messages`) and the ledger module itself, proving:
//!
//! 1. importing the same message twice writes one strand and the repeat
//!    reports it as skipped rather than duplicating it;
//! 2. a genuinely new key adds exactly one strand and one ledger row, in the
//!    same transaction, with the ledger row pointing at that strand's thread;
//! 3. when a later statement in the same transaction fails, the whole
//!    transaction rolls back — a strand is never left stranded without its
//!    ledger row, nor the reverse.
//!
//! Needs a live, migrated Postgres (`DATABASE_URL`); see `tests/common/mod.rs`
//! for the skip rules.

mod common;

use chrono::{Duration, Utc};
use sqlx::PgPool;
use strands_server::connectors::ledger;
use strands_server::whatsapp::bridge::{BufferedMessage, Jid};
use strands_server::whatsapp::store::{StoredChat, write_messages};
use uuid::Uuid;

/// A user with a category, as `write_messages` needs to resolve a tag under.
async fn user_with_category(pool: &PgPool, label: &str) -> (Uuid, Uuid) {
    let user_id = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id, google_sub, email, name) VALUES ($1, $2, $3, 'Test')")
        .bind(user_id)
        .bind(format!("ledger-test-subject-{label}-{user_id}"))
        .bind(format!("ledger-{label}-{user_id}@example.com"))
        .execute(pool)
        .await
        .expect("insert test user");

    let category_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO categories (id, user_id, name, color, position) \
         VALUES ($1, $2, 'Personal', '#123456', 0)",
    )
    .bind(category_id)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("insert test category");

    (user_id, category_id)
}

fn chat(jid: &str) -> StoredChat {
    StoredChat {
        jid: Jid::parse(jid).expect("valid test JID"),
        display_name: Some("Ledger Test Chat".to_owned()),
        is_group: false,
        imported_through: None,
    }
}

fn message(message_id: &str, body: &str, jid: &str) -> BufferedMessage {
    BufferedMessage {
        jid: Jid::parse(jid).expect("valid test JID"),
        message_id: message_id.to_owned(),
        sent_at: Utc::now() - Duration::minutes(5),
        from_me: false,
        sender: Some("+15550100".to_owned()),
        body: body.to_owned(),
        media: None,
    }
}

async fn notes_with_dedupe_key(pool: &PgPool, user_id: Uuid, dedupe_key: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM notes WHERE user_id = $1 AND dedupe_key = $2")
        .bind(user_id)
        .bind(dedupe_key)
        .fetch_one(pool)
        .await
        .expect("count notes")
}

async fn ledger_row(
    pool: &PgPool,
    user_id: Uuid,
    connector: &str,
    item_key: &str,
) -> Option<(Uuid, Option<Uuid>)> {
    sqlx::query_as(
        "SELECT id, thread_id FROM connector_items \
          WHERE user_id = $1 AND connector = $2 AND item_key = $3",
    )
    .bind(user_id)
    .bind(connector)
    .bind(item_key)
    .fetch_optional(pool)
    .await
    .expect("read ledger row")
}

/// (a) Re-running the same keys writes nothing and reports the repeat as
/// skipped — the ledger's unique constraint is what makes this a no-op, not
/// an application-level check that could drift from the real guarantee.
#[tokio::test]
async fn reimporting_the_same_message_twice_writes_one_strand_and_skips_the_second_time() {
    let Some(pool) = common::connect("connector_items_ledger").await else {
        return;
    };
    let (user_id, category_id) = user_with_category(&pool, "dedupe").await;
    const JID: &str = "15550100@s.whatsapp.net";
    let batch = vec![(
        chat(JID),
        vec![message("msg-dedupe-1", "same message, imported twice", JID)],
    )];
    let dedupe_key = format!("whatsapp:{JID}:msg-dedupe-1");

    let first = write_messages(&pool, user_id, category_id, &batch)
        .await
        .expect("first import");
    assert_eq!(first.notes_created, 1, "the first run writes the strand");

    let second = write_messages(&pool, user_id, category_id, &batch)
        .await
        .expect("second import");
    assert_eq!(
        second.notes_created, 0,
        "the repeat must report the key as already imported, not write again"
    );

    assert_eq!(
        notes_with_dedupe_key(&pool, user_id, &dedupe_key).await,
        1,
        "exactly one strand, no duplicate from the re-run"
    );
    let ledger_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM connector_items \
          WHERE user_id = $1 AND connector = 'whatsapp' AND item_key = $2",
    )
    .bind(user_id)
    .bind(&dedupe_key)
    .fetch_one(&pool)
    .await
    .expect("count ledger rows");
    assert_eq!(
        ledger_rows, 1,
        "exactly one ledger row, no duplicate from the re-run"
    );

    // Preview's read: the key now shows up as already imported.
    let seen = ledger::existing_keys(
        &pool,
        user_id,
        "whatsapp",
        std::slice::from_ref(&dedupe_key),
    )
    .await
    .expect("existing_keys read");
    assert!(seen.contains(&dedupe_key));
}

/// (b) A new key is unaffected by any other key's history: it adds exactly
/// one strand and one ledger row, written in the same transaction, with the
/// ledger row pointing at the thread the strand landed under.
#[tokio::test]
async fn a_new_message_key_adds_exactly_one_strand_and_one_ledger_row() {
    let Some(pool) = common::connect("connector_items_ledger").await else {
        return;
    };
    let (user_id, category_id) = user_with_category(&pool, "new-key").await;
    const JID: &str = "15550200@s.whatsapp.net";
    let batch = vec![(
        chat(JID),
        vec![message(
            "msg-new-1",
            "a message nobody has imported before",
            JID,
        )],
    )];
    let dedupe_key = format!("whatsapp:{JID}:msg-new-1");

    let outcome = write_messages(&pool, user_id, category_id, &batch)
        .await
        .expect("import a new key");
    assert_eq!(outcome.notes_created, 1);

    let thread_id: Uuid =
        sqlx::query_scalar("SELECT thread_id FROM notes WHERE user_id = $1 AND dedupe_key = $2")
            .bind(user_id)
            .bind(&dedupe_key)
            .fetch_one(&pool)
            .await
            .expect("the strand's thread");

    let (_, ledger_thread_id) = ledger_row(&pool, user_id, "whatsapp", &dedupe_key)
        .await
        .expect("exactly one ledger row for the new key");
    assert_eq!(
        ledger_thread_id,
        Some(thread_id),
        "the ledger row must point at the strand it protects"
    );

    // No key of any other known key leaked into this one's row.
    assert_eq!(
        notes_with_dedupe_key(&pool, user_id, &dedupe_key).await,
        1,
        "exactly one strand for the new key"
    );
}

/// (c) The strand and its ledger row commit — or roll back — together.
/// Forces a later statement in the *same* transaction to fail after a valid
/// note insert and a valid `record_import` call, then rolls back, and proves
/// neither row survived: a crash between the two halves cannot leave one
/// without the other, because there is no "between" inside one transaction.
#[tokio::test]
async fn a_failed_transaction_leaves_no_strand_and_no_ledger_row() {
    let Some(pool) = common::connect("connector_items_ledger").await else {
        return;
    };
    let (user_id, category_id) = user_with_category(&pool, "failed-tx").await;

    let tag_id = Uuid::new_v4();
    sqlx::query("INSERT INTO tags (id, user_id, category_id, name) VALUES ($1, $2, $3, 'Ledger')")
        .bind(tag_id)
        .bind(user_id)
        .bind(category_id)
        .execute(&pool)
        .await
        .expect("insert test tag");

    let note_id = Uuid::new_v4();
    let dedupe_key = "whatsapp:15550300@s.whatsapp.net:msg-fails";

    let mut tx = pool.begin().await.expect("begin transaction");

    sqlx::query(
        "INSERT INTO notes (id, user_id, tag_id, kind, body, occurred_at, source, dedupe_key) \
         VALUES ($1, $2, $3, 'text', 'a strand about to be rolled back', now(), 'whatsapp', $4)",
    )
    .bind(note_id)
    .bind(user_id)
    .bind(tag_id)
    .bind(dedupe_key)
    .execute(&mut *tx)
    .await
    .expect("insert the note half of this item");

    let inserted = ledger::record_import(
        &mut tx,
        user_id,
        "whatsapp",
        dedupe_key,
        &ledger::source_hash(b"a strand about to be rolled back"),
        None,
    )
    .await
    .expect("record the ledger half of this item");
    assert!(
        inserted,
        "the ledger insert itself must succeed before the failure"
    );

    // The failing statement: a duplicate primary key, the same failure shape
    // a real constraint violation elsewhere in the item's write would take.
    let duplicate_id_failed = sqlx::query(
        "INSERT INTO notes (id, user_id, tag_id, kind, body, occurred_at, source, dedupe_key) \
         VALUES ($1, $2, $3, 'text', 'colliding row', now(), 'whatsapp', 'unused-key')",
    )
    .bind(note_id)
    .bind(user_id)
    .bind(tag_id)
    .execute(&mut *tx)
    .await
    .is_err();
    assert!(duplicate_id_failed, "the forced failure must actually fail");

    tx.rollback().await.expect("roll back the whole item");

    assert_eq!(
        notes_with_dedupe_key(&pool, user_id, dedupe_key).await,
        0,
        "the note half must not survive a rollback of its own transaction"
    );
    assert!(
        ledger_row(&pool, user_id, "whatsapp", dedupe_key)
            .await
            .is_none(),
        "the ledger half must not survive a rollback of its own transaction"
    );
}
