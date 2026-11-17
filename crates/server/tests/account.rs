//! Account and data-control acceptance tests (ticket T17).
//!
//! Spec acceptance, verbatim:
//!
//! * the export returns all of the caller's tags, threads, summaries and
//!   summary versions in one versioned JSON envelope with `exported_at`;
//! * the export is complete for one account and empty of any other account's
//!   rows;
//! * deleting data removes only the caller's rows and leaves their account
//!   usable.
//!
//! The login flow is real (fake [`IdentityProvider`] for the network half),
//! then rows are inserted directly: these tests are about what the export and
//! the delete see, not about how a row was created.
//!
//! Needs a live, migrated Postgres (`DATABASE_URL`); see `tests/common/mod.rs`
//! for the skip rules.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use strands_core::UserId;
use strands_server::app::{AppState, router};
use strands_server::auth::identity::{BoxFuture, ConsentRequest};
use strands_server::auth::{AuthError, Identity, IdentityProvider};
use strands_server::config::AuthConfig;
use strands_server::summary::{BoxFuture as SummarizeBoxFuture, SummarizeError, Summarizer};
use tower::ServiceExt;
use uuid::Uuid;

mod common;

/// Stands in for Google; each run gets fresh subjects and emails so repeated
/// local runs never collide on `users.email`.
struct FakeIdentityProvider {
    run: String,
}

impl FakeIdentityProvider {
    fn person(&self, code: &str) -> Option<(&'static str, String, &'static str)> {
        match code {
            "alice" => Some(("alice", format!("alice+{}@example.com", self.run), "Alice")),
            "bob" => Some(("bob", format!("bob+{}@example.com", self.run), "Bob")),
            _ => None,
        }
    }
}

impl IdentityProvider for FakeIdentityProvider {
    fn consent_url(&self, request: &ConsentRequest<'_>) -> String {
        format!(
            "https://accounts.example/consent?state={}&code_challenge={}&redirect_uri={}",
            request.state, request.code_challenge, request.redirect_uri
        )
    }

    fn exchange_code<'a>(
        &'a self,
        code: &'a str,
        _code_verifier: &'a str,
        _redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<Identity, AuthError>> {
        Box::pin(async move {
            let (subject, email, name) = self.person(code).ok_or(AuthError::InvalidCode)?;
            Ok(Identity {
                subject: format!("sub-{subject}-{}", self.run),
                email,
                name: name.to_owned(),
                avatar_url: None,
            })
        })
    }
}

/// Never reached: no route here generates a summary.
struct UnusedSummarizer;

impl Summarizer for UnusedSummarizer {
    fn model_alias(&self) -> &str {
        "unused"
    }

    fn summarize<'a>(
        &'a self,
        _prompt: &'a str,
    ) -> SummarizeBoxFuture<'a, Result<String, SummarizeError>> {
        Box::pin(async { Err(SummarizeError::Request("never called".into())) })
    }
}

struct Harness {
    app: Router,
    pool: PgPool,
    run: String,
}

impl Harness {
    async fn connect(test_name: &str) -> Option<Self> {
        let pool = common::connect(test_name).await?;

        let config = AuthConfig {
            client_id: "test-client".into(),
            client_secret: "test-secret".into(),
            public_base_url: "https://strands.test".into(),
            cookie_secure: true,
            session_ttl_days: 30,
            authorization_endpoint: "https://accounts.example/consent".into(),
            token_endpoint: "https://accounts.example/token".into(),
        };

        let run = Uuid::new_v4().simple().to_string();
        let app = router(AppState::new(
            pool.clone(),
            config,
            Arc::new(FakeIdentityProvider { run: run.clone() }),
            Arc::new(strands_core::Registry::new()),
            Arc::new(UnusedSummarizer),
        ));

        Some(Self { app, pool, run })
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, headers, body)
    }

    async fn get(&self, uri: &str, cookie: Option<&str>) -> (StatusCode, HeaderMap, Value) {
        let mut builder = Request::get(uri);
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        self.send(builder.body(Body::empty()).unwrap()).await
    }

    async fn delete(&self, uri: &str, cookie: Option<&str>) -> (StatusCode, HeaderMap, Value) {
        let mut builder = Request::delete(uri);
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        self.send(builder.body(Body::empty()).unwrap()).await
    }

    /// Consent redirect → callback → session cookie, for real.
    async fn login(&self, code: &str) -> String {
        let (status, headers, _) = self.get("/auth/google", None).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "consent redirect");

        let location = headers
            .get(header::LOCATION)
            .expect("consent redirect has a Location")
            .to_str()
            .unwrap()
            .to_owned();
        let state = query_param(&location, "state").expect("consent URL carries state");

        let (status, headers, _) = self
            .get(
                &format!("/auth/google/callback?code={code}&state={state}"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::SEE_OTHER, "callback for {code}");

        headers
            .get(header::SET_COOKIE)
            .expect("callback sets a session cookie")
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .expect("cookie pair before the first attribute")
            .to_owned()
    }

    async fn user_id(&self, cookie: &str) -> UserId {
        let (status, _, body) = self.get("/api/v1/me", Some(cookie)).await;
        assert_eq!(status, StatusCode::OK, "GET /api/v1/me body: {body}");
        body["id"]
            .as_str()
            .expect("me carries an id")
            .parse()
            .unwrap()
    }

    async fn first_category(&self, user_id: UserId) -> Uuid {
        sqlx::query_scalar("SELECT id FROM categories WHERE user_id = $1 ORDER BY position LIMIT 1")
            .bind(user_id)
            .fetch_one(&self.pool)
            .await
            .expect("login seeded the default categories")
    }

    async fn create_tag(&self, user_id: UserId, name: &str) -> Uuid {
        let category_id = self.first_category(user_id).await;
        let tag_id = Uuid::new_v4();
        sqlx::query("INSERT INTO tags (id, user_id, name, category_id) VALUES ($1, $2, $3, $4)")
            .bind(tag_id)
            .bind(user_id)
            .bind(name)
            .bind(category_id)
            .execute(&self.pool)
            .await
            .expect("insert tag");
        tag_id
    }

    async fn create_thread_with_note(
        &self,
        user_id: UserId,
        tag_id: Uuid,
        title: &str,
        body: &str,
    ) -> (Uuid, Uuid) {
        let thread_id = Uuid::new_v4();
        sqlx::query("INSERT INTO threads (id, user_id, tag_id, title) VALUES ($1, $2, $3, $4)")
            .bind(thread_id)
            .bind(user_id)
            .bind(tag_id)
            .bind(title)
            .execute(&self.pool)
            .await
            .expect("insert thread");

        let note_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO notes (id, user_id, tag_id, thread_id, kind, body, occurred_at, source, \
                    dedupe_key) \
             VALUES ($1, $2, $3, $4, 'text', $5, now(), 'manual', $6)",
        )
        .bind(note_id)
        .bind(user_id)
        .bind(tag_id)
        .bind(thread_id)
        .bind(body)
        .bind(format!("{}/{}", self.run, note_id))
        .execute(&self.pool)
        .await
        .expect("insert note");

        (thread_id, note_id)
    }

    /// A summary version, approved when `approved` is set (which also points the
    /// tag at it — the state the export calls "the summary in force").
    async fn create_summary(
        &self,
        user_id: UserId,
        tag_id: Uuid,
        version: i32,
        body: &str,
        approved: bool,
    ) -> Uuid {
        let summary_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO summaries (id, user_id, tag_id, version, body, state, model_alias, \
                    preset, source_hash) \
             VALUES ($1, $2, $3, $4, $5, $6::summary_state, 'test-model', 'shorter', 'hash')",
        )
        .bind(summary_id)
        .bind(user_id)
        .bind(tag_id)
        .bind(version)
        .bind(body)
        .bind(if approved { "approved" } else { "proposed" })
        .execute(&self.pool)
        .await
        .expect("insert summary");

        if approved {
            sqlx::query("UPDATE tags SET summary_approved_id = $1 WHERE id = $2")
                .bind(summary_id)
                .bind(tag_id)
                .execute(&self.pool)
                .await
                .expect("approve summary");
        }

        summary_id
    }

    /// A judged similarity row between two of one owner's tags.
    async fn create_link(&self, user_id: UserId, tag_a: Uuid, tag_b: Uuid) {
        let (a, b) = if tag_a < tag_b {
            (tag_a, tag_b)
        } else {
            (tag_b, tag_a)
        };
        sqlx::query(
            "INSERT INTO links (id, user_id, a_tag_id, b_tag_id, score, rationale) \
             VALUES ($1, $2, $3, $4, 88, 'same trip')",
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .bind(a)
        .bind(b)
        .execute(&self.pool)
        .await
        .expect("insert link");
    }

    async fn create_list(&self, user_id: UserId, name: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO lists (id, user_id, name, source) VALUES ($1, $2, $3, 'import')")
            .bind(id)
            .bind(user_id)
            .bind(name)
            .execute(&self.pool)
            .await
            .expect("insert list");
        id
    }

    async fn create_place(&self, user_id: UserId, list_id: Uuid, name: &str) {
        sqlx::query(
            "INSERT INTO places (id, user_id, list_id, name, source) \
             VALUES ($1, $2, $3, $4, 'import')",
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .bind(list_id)
        .bind(name)
        .execute(&self.pool)
        .await
        .expect("insert place");
    }

    /// A connected connector and the ciphertext that keeps it connected: the
    /// rows a data wipe has to unlink, or the login outlives the account's data.
    async fn create_connector(&self, user_id: UserId) {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO connectors (id, user_id, kind) VALUES ($1, $2, 'google')")
            .bind(id)
            .bind(user_id)
            .execute(&self.pool)
            .await
            .expect("insert connector");
        sqlx::query(
            "INSERT INTO connector_secrets (connector_id, user_id, ciphertext) VALUES ($1, $2, $3)",
        )
        .bind(id)
        .bind(user_id)
        .bind(vec![0u8, 1, 2, 3])
        .execute(&self.pool)
        .await
        .expect("insert connector secret");
    }

    async fn count(&self, table: &str, user_id: UserId) -> i64 {
        // `table` is a literal from this file, never user input.
        let sql = format!("SELECT count(*) FROM {table} WHERE user_id = $1");
        sqlx::query_scalar(&sql)
            .bind(user_id)
            .fetch_one(&self.pool)
            .await
            .expect("count rows")
    }
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then(|| value.to_owned())
    })
}

#[tokio::test]
async fn the_export_returns_every_row_the_caller_owns() {
    let Some(harness) = Harness::connect("account::export_completeness").await else {
        return;
    };

    let cookie = harness.login("alice").await;
    let user_id = harness.user_id(&cookie).await;
    let tokyo = harness.create_tag(user_id, "Tokyo").await;
    let lisbon = harness.create_tag(user_id, "Lisbon").await;

    let (thread_id, note_id) = harness
        .create_thread_with_note(user_id, tokyo, "The 6 a.m. train", "It leaves on time.")
        .await;
    let first = harness
        .create_summary(user_id, tokyo, 1, "Trains, mostly.", false)
        .await;
    let second = harness
        .create_summary(user_id, tokyo, 2, "Trains, punctually.", true)
        .await;

    let (status, _, body) = harness.get("/api/v1/account/export", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "export body: {body}");

    assert_eq!(body["format_version"], 1, "{body}");
    assert!(
        body["exported_at"]
            .as_str()
            .is_some_and(|at| at.contains('T')),
        "exported_at is an RFC 3339 timestamp: {body}"
    );
    assert_eq!(
        body["profile"]["email"],
        format!("alice+{}@example.com", harness.run)
    );
    assert_eq!(body["profile"]["provider"], "google");

    let tags = body["tags"].as_array().expect("tags array");
    assert_eq!(tags.len(), 2, "{body}");
    assert!(tags.iter().any(|tag| tag["name"] == "Tokyo"), "{body}");
    assert!(tags.iter().any(|tag| tag["name"] == "Lisbon"), "{body}");

    let threads = body["threads"].as_array().expect("threads array");
    assert_eq!(threads.len(), 1, "{body}");
    assert_eq!(threads[0]["id"], thread_id.to_string());
    assert_eq!(threads[0]["title"], "The 6 a.m. train");

    let notes = body["notes"].as_array().expect("notes array");
    assert_eq!(notes.len(), 1, "{body}");
    assert_eq!(notes[0]["id"], note_id.to_string());
    assert_eq!(notes[0]["body"], "It leaves on time.");
    assert_eq!(notes[0]["kind"], "text");
    assert_eq!(notes[0]["source"], "manual");

    let summaries = body["summaries"].as_array().expect("summaries array");
    assert_eq!(
        summaries.len(),
        1,
        "only the approved one is in force: {body}"
    );
    assert_eq!(summaries[0]["tag_id"], tokyo.to_string());
    assert_eq!(summaries[0]["version_id"], second.to_string());

    let versions = body["summary_versions"]
        .as_array()
        .expect("summary_versions array");
    assert_eq!(
        versions.len(),
        2,
        "history keeps the proposed one too: {body}"
    );
    let ids: Vec<&str> = versions
        .iter()
        .map(|version| version["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&first.to_string().as_str()), "{body}");
    assert!(ids.contains(&second.to_string().as_str()), "{body}");
    assert!(
        versions
            .iter()
            .any(|version| version["state"] == "approved"),
        "{body}"
    );

    let _ = lisbon;
}

#[tokio::test]
async fn the_export_is_a_download_not_a_page() {
    let Some(harness) = Harness::connect("account::export_headers").await else {
        return;
    };

    let cookie = harness.login("alice").await;
    let (status, headers, _) = harness.get("/api/v1/account/export", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);

    let disposition = headers
        .get(header::CONTENT_DISPOSITION)
        .expect("export sets Content-Disposition")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(disposition.starts_with("attachment;"), "{disposition}");
    assert!(disposition.contains(".json"), "{disposition}");

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .expect("export sets Content-Type")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        content_type.starts_with("application/json"),
        "{content_type}"
    );
}

#[tokio::test]
async fn an_export_never_carries_another_users_rows() {
    let Some(harness) = Harness::connect("account::export_isolation").await else {
        return;
    };

    let alice_cookie = harness.login("alice").await;
    let alice_id = harness.user_id(&alice_cookie).await;
    let alice_tag = harness.create_tag(alice_id, "Alice's tag").await;
    let (_, alice_note) = harness
        .create_thread_with_note(alice_id, alice_tag, "Alice's thread", "Alice's note body")
        .await;

    let bob_cookie = harness.login("bob").await;
    let bob_id = harness.user_id(&bob_cookie).await;
    let bob_tag = harness.create_tag(bob_id, "Bob's tag").await;
    let (bob_thread, bob_note) = harness
        .create_thread_with_note(bob_id, bob_tag, "Bob's thread", "Bob's note body")
        .await;

    let (status, _, export) = harness
        .get("/api/v1/account/export", Some(&alice_cookie))
        .await;
    assert_eq!(status, StatusCode::OK, "export body: {export}");

    let text = export.to_string();
    for foreign in [bob_tag, bob_thread, bob_note] {
        assert!(
            !text.contains(&foreign.to_string()),
            "alice's export leaked bob's {foreign}: {text}"
        );
    }
    assert!(text.contains(&alice_tag.to_string()), "{text}");
    assert!(text.contains(&alice_note.to_string()), "{text}");
    assert_eq!(export["tags"].as_array().map(Vec::len), Some(1), "{export}");
}

#[tokio::test]
async fn deleting_my_data_erases_only_my_rows_and_keeps_my_account() {
    let Some(harness) = Harness::connect("account::delete_my_data").await else {
        return;
    };

    let alice_cookie = harness.login("alice").await;
    let alice_id = harness.user_id(&alice_cookie).await;
    let alice_tag = harness.create_tag(alice_id, "Tokyo").await;
    harness
        .create_thread_with_note(
            alice_id,
            alice_tag,
            "The 6 a.m. train",
            "It leaves on time.",
        )
        .await;
    harness
        .create_summary(alice_id, alice_tag, 1, "Trains, mostly.", true)
        .await;

    let bob_cookie = harness.login("bob").await;
    let bob_id = harness.user_id(&bob_cookie).await;
    let bob_tag = harness.create_tag(bob_id, "Lisbon").await;
    harness
        .create_thread_with_note(bob_id, bob_tag, "The 28 tram", "It is full.")
        .await;

    let (status, _, _) = harness
        .delete("/api/v1/account/data", Some(&alice_cookie))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    for table in ["tags", "threads", "notes", "summaries"] {
        assert_eq!(
            harness.count(table, alice_id).await,
            0,
            "{table} still has rows for alice"
        );
    }

    // The account survives: the session still resolves and the export is empty
    // rather than a 401.
    let (status, _, me) = harness.get("/api/v1/me", Some(&alice_cookie)).await;
    assert_eq!(status, StatusCode::OK, "me body: {me}");
    assert_eq!(me["id"], alice_id.to_string());

    let (status, _, export) = harness
        .get("/api/v1/account/export", Some(&alice_cookie))
        .await;
    assert_eq!(status, StatusCode::OK, "export body: {export}");
    for key in ["tags", "threads", "notes", "summaries", "summary_versions"] {
        assert_eq!(
            export[key].as_array().map(Vec::len),
            Some(0),
            "{key} should be empty after the delete: {export}"
        );
    }

    // Bob is untouched.
    assert_eq!(harness.count("tags", bob_id).await, 1);
    assert_eq!(harness.count("threads", bob_id).await, 1);
    let (status, _, bob_export) = harness
        .get("/api/v1/account/export", Some(&bob_cookie))
        .await;
    assert_eq!(status, StatusCode::OK, "bob export body: {bob_export}");
    assert!(
        bob_export.to_string().contains(&bob_tag.to_string()),
        "{bob_export}"
    );
}

/// The two halves of data control have to agree: everything a wipe destroys is
/// something the user was able to export first. The table list is read from the
/// delete path itself, so a collection added to one side alone fails here.
#[tokio::test]
async fn the_export_carries_every_collection_the_delete_removes() {
    const STORE: &str = include_str!("../src/account/store.rs");
    /// Rows the export deliberately leaves out: connective rows, machine state,
    /// and the ciphertext that must never leave the database.
    const NOT_EXPORTED: &[&str] = &[
        "summary_notes",
        "jobs",
        "import_runs",
        "connector_secrets",
        "connectors",
    ];
    const EXPORTED: &[&str] = &[
        "tags",
        "threads",
        "notes",
        "summaries",
        "links",
        "lists",
        "places",
    ];

    let deleted: Vec<&str> = STORE
        .match_indices("DELETE FROM ")
        .map(|(at, needle)| {
            STORE[at + needle.len()..]
                .split_whitespace()
                .next()
                .expect("a table name follows DELETE FROM")
        })
        .collect();

    for table in &deleted {
        assert!(
            EXPORTED.contains(table) || NOT_EXPORTED.contains(table),
            "`{table}` is wiped but never exported: give it an export field or name it in NOT_EXPORTED"
        );
    }

    let Some(harness) = Harness::connect("account::export_covers_delete").await else {
        return;
    };

    let alice = harness.login("alice").await;
    let bob = harness.login("bob").await;
    let alice_id = harness.user_id(&alice).await;
    let bob_id = harness.user_id(&bob).await;

    let first = harness.create_tag(alice_id, "Tokyo").await;
    let second = harness.create_tag(alice_id, "Kyoto").await;
    harness.create_link(alice_id, first, second).await;
    let list = harness.create_list(alice_id, "Saved 2024").await;
    harness.create_place(alice_id, list, "Nishiki Market").await;
    harness.create_connector(alice_id).await;
    harness.create_connector(bob_id).await;

    let (status, _, export) = harness.get("/api/v1/account/export", Some(&alice)).await;
    assert_eq!(status, StatusCode::OK, "export body: {export}");
    assert_eq!(
        export["links"].as_array().map(Vec::len),
        Some(1),
        "{export}"
    );
    assert_eq!(
        export["lists"].as_array().map(Vec::len),
        Some(1),
        "{export}"
    );
    assert_eq!(
        export["places"].as_array().map(Vec::len),
        Some(1),
        "{export}"
    );
    assert_eq!(export["places"][0]["name"], "Nishiki Market", "{export}");

    let (status, _, body) = harness.delete("/api/v1/account/data", Some(&alice)).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "delete body: {body}");

    for table in [
        "links",
        "lists",
        "places",
        "connectors",
        "connector_secrets",
    ] {
        assert_eq!(
            harness.count(table, alice_id).await,
            0,
            "`{table}` survived the wipe"
        );
    }
    assert_eq!(
        harness.count("connectors", bob_id).await,
        1,
        "bob's login was collateral"
    );
    assert_eq!(
        harness.count("connector_secrets", bob_id).await,
        1,
        "bob's secret was collateral"
    );
}

#[tokio::test]
async fn data_controls_need_a_session() {
    let Some(harness) = Harness::connect("account::needs_session").await else {
        return;
    };

    for (method, uri) in [
        ("GET", "/api/v1/account/export"),
        ("DELETE", "/api/v1/account/data"),
    ] {
        let (status, _, _) = if method == "GET" {
            harness.get(uri, None).await
        } else {
            harness.delete(uri, None).await
        };
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}
