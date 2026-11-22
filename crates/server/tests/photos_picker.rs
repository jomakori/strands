//! Google Photos Picker acceptance test (ticket T14).
//!
//! Acceptance, verbatim:
//!
//! 1. three picked items become three notes, and their thumbnails still exist
//!    after a restart;
//! 2. an expired `baseUrl` recovers through a new session instead of failing;
//! 3. a pick with nothing in it leaves the account clean;
//! 4. another user's session is a `404`, never a `403`.
//!
//! Google itself is replaced by a scripted [`PickerApi`]: the lane's contract
//! with Google is already covered against a real socket in
//! `crates/connector-photos/tests/picker_http.rs`, so what is left to prove here
//! is everything *we* own — the session row, the notes, the stored bytes and the
//! rotation. The restart check builds a second router from the same pool, which
//! is the part a fake in-memory store would fake away: the bytes have to come
//! back out of Postgres.
//!
//! Needs a live, migrated Postgres (`DATABASE_URL`); see `tests/common/mod.rs`
//! for the skip rules, and `STRANDS_TEST_REQUIRE_DB=1` to make a missing database
//! a failure rather than a skip.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{Duration, Utc};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use strands_connector_photos::{
    PhotosError, PickedItem, PickerApi, PickerFuture, PickerSession, SessionStatus, Thumbnail,
};
use strands_core::UserId;
use strands_server::app::{AppState, router};
use strands_server::auth::identity::{BoxFuture, ConsentRequest};
use strands_server::auth::{AuthError, Identity, IdentityProvider};
use strands_server::config::AuthConfig;
use strands_server::secrets::{CURRENT_KEY_VERSION, SecretKey};
use tower::ServiceExt;
use uuid::Uuid;

mod common;

/// One picked item, as the script knows it before it has a session.
struct Pick {
    id: String,
    caption: Option<String>,
}

/// The picker, scripted: it hands out sessions, then serves whatever was picked.
struct FakePicker {
    state: Mutex<ScriptState>,
}

struct ScriptState {
    picks: Vec<Pick>,
    sessions: usize,
    /// Sessions whose `baseUrl`s are already dead, as if their 60 minutes had
    /// passed before the download. Google answers 403 to those.
    lapsed: Vec<usize>,
}

impl FakePicker {
    fn new(picks: Vec<Pick>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ScriptState {
                picks,
                sessions: 0,
                lapsed: Vec::new(),
            }),
        })
    }

    /// The session number in `fake-session-{n}`.
    fn index(session_id: &str) -> usize {
        session_id
            .rsplit('-')
            .next()
            .and_then(|tail| tail.parse().ok())
            .unwrap_or(0)
    }
}

impl PickerApi for FakePicker {
    fn create_session<'a>(
        &'a self,
        _access_token: &'a str,
    ) -> PickerFuture<'a, Result<PickerSession, PhotosError>> {
        Box::pin(async move {
            let mut state = self.state.lock().expect("picker script");
            state.sessions += 1;
            let number = state.sessions;
            Ok(PickerSession {
                id: format!("fake-session-{number}"),
                picker_uri: format!("https://photos.example/pick/{number}"),
                expires_at: Utc::now() + Duration::minutes(15),
            })
        })
    }

    fn session_status<'a>(
        &'a self,
        _access_token: &'a str,
        _session_id: &'a str,
    ) -> PickerFuture<'a, Result<SessionStatus, PhotosError>> {
        // The script's user always finishes picking immediately, which is the
        // state the import cares about.
        Box::pin(async move {
            Ok(SessionStatus {
                media_items_set: true,
            })
        })
    }

    fn list_items<'a>(
        &'a self,
        _access_token: &'a str,
        session_id: &'a str,
    ) -> PickerFuture<'a, Result<Vec<PickedItem>, PhotosError>> {
        Box::pin(async move {
            let state = self.state.lock().expect("picker script");
            let number = Self::index(session_id);
            let lapsed = state.lapsed.contains(&number);
            Ok(state
                .picks
                .iter()
                .map(|pick| PickedItem {
                    id: pick.id.clone(),
                    caption: pick.caption.clone(),
                    create_time: Some(Utc::now() - Duration::days(2)),
                    base_url: format!(
                        "https://photos.example/preview/{}?session={number}&lapsed={lapsed}",
                        pick.id
                    ),
                    mime_type: "image/jpeg".to_owned(),
                    width: 1024,
                    height: 768,
                })
                .collect())
        })
    }

    fn thumbnail<'a>(
        &'a self,
        base_url: &'a str,
    ) -> PickerFuture<'a, Result<Thumbnail, PhotosError>> {
        Box::pin(async move {
            // A lapsed `baseUrl` is a 403 from Google: unrecoverable, so the
            // connector reports it as expired and the server rotates.
            if base_url.contains("lapsed=true") {
                return Err(PhotosError::Expired);
            }
            let id = base_url
                .rsplit('/')
                .next()
                .and_then(|tail| tail.split('?').next())
                .unwrap_or("unknown")
                .to_owned();
            Ok(Thumbnail {
                content_type: "image/jpeg".to_owned(),
                bytes: format!("jpeg-bytes-{id}").into_bytes(),
            })
        })
    }
}

struct UnusedIdentity;

impl IdentityProvider for UnusedIdentity {
    fn consent_url(&self, _request: &ConsentRequest<'_>) -> String {
        "https://identity.example/authorize".to_owned()
    }

    fn exchange_code<'a>(
        &'a self,
        _code: &'a str,
        _code_verifier: &'a str,
        _redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<Identity, AuthError>> {
        Box::pin(async move { Err(AuthError::InvalidCode) })
    }
}

struct TestUser {
    id: UserId,
    cookie: String,
}

struct Harness {
    pool: PgPool,
    app: Router,
    picker: Arc<FakePicker>,
    key: Arc<SecretKey>,
}

impl Harness {
    async fn connect(picks: Vec<Pick>) -> Option<Self> {
        let pool = common::connect("photos_picker").await?;
        let picker = FakePicker::new(picks);
        // A fixed key: the test's concern is that bytes come back out of
        // Postgres, not how the key is generated (that is `secrets`' own test).
        let key = Arc::new(
            SecretKey::from_base64(&STANDARD.encode([9u8; 32])).expect("a 32-byte test key"),
        );
        let app = build(&pool, picker.clone(), key.clone());
        Some(Self {
            pool,
            app,
            picker,
            key,
        })
    }

    /// Point the harness at a fresh router built from the same pool: the state a
    /// restarted process would have, with nothing kept in memory.
    fn restart(&self) -> Router {
        build(&self.pool, self.picker.clone(), self.key.clone())
    }

    async fn user(&self, label: &str) -> TestUser {
        let user_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, google_sub, email, name) VALUES ($1, $2, $3, 'Test')")
            .bind(user_id)
            .bind(format!("photos-test-subject-{user_id}"))
            .bind(format!("photos-{label}-{user_id}@example.com"))
            .execute(&self.pool)
            .await
            .expect("insert test user");
        sqlx::query("INSERT INTO sessions (id, user_id, expires_at) VALUES ($1, $2, $3)")
            .bind(session_id)
            .bind(user_id)
            .bind(Utc::now() + Duration::hours(1))
            .execute(&self.pool)
            .await
            .expect("insert test session");
        sqlx::query(
            "INSERT INTO categories (id, user_id, name, color, position) \
             VALUES ($1, $2, 'Personal', '#123456', 0)",
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .execute(&self.pool)
        .await
        .expect("insert test category");
        TestUser {
            id: user_id,
            cookie: format!("strands_session={session_id}"),
        }
    }

    /// Give the user a connected Google Photos connector, as the OAuth callback
    /// would: a row, plus the token sealed.
    async fn connect_photos(&self, user: &TestUser) {
        let connector_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO connectors (id, user_id, kind, status) \
             VALUES ($1, $2, 'google_photos', 'connected')",
        )
        .bind(connector_id)
        .bind(user.id)
        .execute(&self.pool)
        .await
        .expect("insert connector");
        let sealed = self
            .key
            .seal(user.id, b"ya29.test-token")
            .expect("seal the token");
        sqlx::query(
            "INSERT INTO connector_secrets (connector_id, user_id, ciphertext, key_version) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(connector_id)
        .bind(user.id)
        .bind(sealed)
        .bind(CURRENT_KEY_VERSION)
        .execute(&self.pool)
        .await
        .expect("insert connector secret");
    }

    async fn start_pick(&self, user: &TestUser) -> String {
        let (status, body) = self
            .send(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/connectors/photos/sessions")
                    .header(header::COOKIE, &user.cookie)
                    .body(Body::empty())
                    .expect("build create request"),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "create pick: {body}");
        body["session_id"]
            .as_str()
            .expect("a session id to poll with")
            .to_owned()
    }

    async fn poll(&self, user: &TestUser, session_id: &str) -> (StatusCode, Value) {
        self.send(
            Request::builder()
                .uri(format!("/api/v1/connectors/photos/sessions/{session_id}"))
                .header(header::COOKIE, &user.cookie)
                .body(Body::empty())
                .expect("build poll request"),
        )
        .await
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, Value) {
        send(&self.app, request).await
    }

    async fn thumbnail(
        &self,
        app: &Router,
        user: &TestUser,
        note_id: &str,
    ) -> (StatusCode, Vec<u8>) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/notes/{note_id}/thumbnail"))
                    .header(header::COOKIE, &user.cookie)
                    .body(Body::empty())
                    .expect("build thumbnail request"),
            )
            .await
            .expect("router response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("read body")
            .to_bytes()
            .to_vec();
        (status, bytes)
    }

    async fn count(&self, user: &TestUser, sql: &str) -> i64 {
        sqlx::query_scalar::<_, i64>(sql)
            .bind(user.id)
            .fetch_one(&self.pool)
            .await
            .expect("count")
    }

    async fn note_ids(&self, user: &TestUser) -> Vec<(Uuid, String)> {
        sqlx::query_as("SELECT id, body FROM notes WHERE user_id = $1 ORDER BY occurred_at")
            .bind(user.id)
            .fetch_all(&self.pool)
            .await
            .expect("read notes")
    }
}

fn build(pool: &PgPool, picker: Arc<FakePicker>, key: Arc<SecretKey>) -> Router {
    let state = AppState::new(
        pool.clone(),
        AuthConfig {
            client_id: "unused-client-id".into(),
            issuer: "https://identity.example".into(),
            public_base_url: "https://test.example".into(),
            cookie_secure: true,
            session_ttl_days: 30,
        },
        Arc::new(UnusedIdentity),
        Arc::new(strands_core::Registry::new()),
        Arc::new(strands_server::summary::LiteLlmSummarizer::new(
            strands_server::llm::LiteLlmClient::new("http://unused.invalid", "unused"),
        )),
    )
    .with_photos(picker)
    .with_secrets(key);
    router(state)
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.expect("router response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read response body")
        .to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "decode body ({status}): {error}: {}",
            String::from_utf8_lossy(&bytes)
        )
    });
    (status, body)
}

fn three_picks() -> Vec<Pick> {
    vec![
        Pick {
            id: "item-a".to_owned(),
            caption: Some("Sunrise over the bay".to_owned()),
        },
        Pick {
            id: "item-b".to_owned(),
            caption: None,
        },
        Pick {
            id: "item-c".to_owned(),
            caption: Some("Sunrise over the bay".to_owned()),
        },
    ]
}

#[tokio::test]
async fn three_picked_items_become_three_notes_with_durable_thumbnails() {
    let Some(harness) = Harness::connect(three_picks()).await else {
        return;
    };
    let user = harness.user("alice").await;
    harness.connect_photos(&user).await;

    let session_id = harness.start_pick(&user).await;
    let (status, body) = harness.poll(&user, &session_id).await;
    assert_eq!(status, StatusCode::OK, "poll: {body}");
    assert_eq!(body["state"], "imported");
    assert_eq!(body["notes_created"], 3);
    assert_eq!(body["notes_skipped"], 0);

    let notes = harness.note_ids(&user).await;
    assert_eq!(notes.len(), 3, "one note per picked item");

    // The caption wins the tag hint, so the two captioned items share a tag.
    let tag: (String, i64) = sqlx::query_as(
        "SELECT t.name, count(*) FROM notes n JOIN tags t ON t.id = n.tag_id \
          WHERE n.user_id = $1 AND t.name = 'Sunrise over the bay' GROUP BY t.name",
    )
    .bind(user.id)
    .fetch_one(&harness.pool)
    .await
    .expect("the caption became a tag");
    assert_eq!(tag.1, 2, "both captioned items share the caption tag");

    assert_eq!(
        harness
            .count(
                &user,
                "SELECT count(*) FROM note_thumbnails WHERE user_id = $1"
            )
            .await,
        3
    );

    // Restart: a second router, same pool, no memory of the import.
    let restarted = harness.restart();
    let mut seen = 0;
    for (note_id, _) in &notes {
        let (status, bytes) = harness
            .thumbnail(&restarted, &user, &note_id.to_string())
            .await;
        assert_eq!(status, StatusCode::OK, "thumbnail after restart");
        assert!(
            String::from_utf8_lossy(&bytes).starts_with("jpeg-bytes-"),
            "the stored bytes are the download, not a placeholder"
        );
        seen += 1;
    }
    assert_eq!(seen, 3);

    // Re-polling the same session must not duplicate anything.
    let (status, body) = harness.poll(&user, &session_id).await;
    assert_eq!(status, StatusCode::OK, "repeat poll: {body}");
    assert_eq!(body["state"], "imported");
    assert_eq!(body["notes_created"], 0, "a repeat poll creates no notes");
    assert_eq!(body["notes_skipped"], 3);
    assert_eq!(harness.note_ids(&user).await.len(), 3);
    assert_eq!(
        harness
            .count(
                &user,
                "SELECT count(*) FROM note_thumbnails WHERE user_id = $1"
            )
            .await,
        3
    );
}

#[tokio::test]
async fn a_lapsed_download_url_recovers_through_a_new_session() {
    let Some(harness) = Harness::connect(three_picks()).await else {
        return;
    };
    // The first session's baseUrls are already past their 60 minutes.
    harness.picker.state.lock().expect("script").lapsed = vec![1];

    let user = harness.user("bob").await;
    harness.connect_photos(&user).await;

    let first = harness.start_pick(&user).await;
    let (status, body) = harness.poll(&user, &first).await;
    assert_eq!(status, StatusCode::OK, "expired poll: {body}");
    assert_eq!(body["state"], "expired", "a dead baseUrl is a state");

    let replacement = body["session"]["session_id"]
        .as_str()
        .expect("the poll must hand back the replacement session")
        .to_owned();
    assert_ne!(replacement, first, "the replacement is a new session");
    assert!(
        body["session"]["picker_uri"].as_str().is_some(),
        "the client needs the URL to send the user to"
    );
    assert_eq!(harness.note_ids(&user).await.len(), 0);

    // Polling the dead session again must reuse that successor, not mint one.
    let (status, again) = harness.poll(&user, &first).await;
    assert_eq!(status, StatusCode::OK, "repeat expired poll: {again}");
    assert_eq!(again["session"]["session_id"], replacement.as_str());

    let (status, done) = harness.poll(&user, &replacement).await;
    assert_eq!(status, StatusCode::OK, "replacement poll: {done}");
    assert_eq!(done["state"], "imported");
    assert_eq!(done["notes_created"], 3);
    assert_eq!(harness.note_ids(&user).await.len(), 3);
}

#[tokio::test]
async fn a_pick_with_nothing_in_it_leaves_the_account_clean() {
    let Some(harness) = Harness::connect(Vec::new()).await else {
        return;
    };
    let user = harness.user("carol").await;
    harness.connect_photos(&user).await;

    let session_id = harness.start_pick(&user).await;
    let (status, body) = harness.poll(&user, &session_id).await;
    assert_eq!(status, StatusCode::OK, "empty poll: {body}");
    assert_eq!(body["state"], "empty");
    assert_eq!(body["notes_created"], 0);
    assert_eq!(body["tags"].as_array().map(Vec::len), Some(0));

    assert_eq!(harness.note_ids(&user).await.len(), 0);
    assert_eq!(
        harness
            .count(
                &user,
                "SELECT count(*) FROM note_thumbnails WHERE user_id = $1"
            )
            .await,
        0
    );
    assert_eq!(
        harness
            .count(&user, "SELECT count(*) FROM threads WHERE user_id = $1")
            .await,
        0
    );
    // Closed, not left pending: the session cannot be imported later.
    // Scoped by user like every production query: two harnesses in this binary
    // can mint the same fake session id for different users, and an unscoped
    // read would sometimes return the other test's still-pending row.
    let state: String = sqlx::query_scalar(
        "SELECT state FROM photo_picker_sessions WHERE session_id = $1 AND user_id = $2",
    )
    .bind(&session_id)
    .bind(user.id)
    .fetch_one(&harness.pool)
    .await
    .expect("read session state");
    assert_eq!(state, "imported", "an empty pick is finished, not pending");
}

#[tokio::test]
async fn another_users_session_is_a_404_not_a_403() {
    let Some(harness) = Harness::connect(three_picks()).await else {
        return;
    };
    let alice = harness.user("alice-owner").await;
    harness.connect_photos(&alice).await;
    let bob = harness.user("bob-stranger").await;
    harness.connect_photos(&bob).await;

    let session_id = harness.start_pick(&alice).await;

    // Bob knows the id and is authenticated: the answer is still "no such
    // session", because a 403 would confirm Alice has one.
    let (status, body) = harness.poll(&bob, &session_id).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger poll: {body}");

    let (status, body) = harness.poll(&alice, &session_id).await;
    assert_eq!(status, StatusCode::OK, "owner poll: {body}");
    assert_eq!(body["state"], "imported");

    // And the note's bytes stay private the same way.
    let notes = harness.note_ids(&alice).await;
    let note_id = notes[0].0;
    let (status, _) = harness
        .thumbnail(&harness.app, &bob, &note_id.to_string())
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "stranger thumbnail");
    let (status, _) = harness
        .thumbnail(&harness.app, &alice, &note_id.to_string())
        .await;
    assert_eq!(status, StatusCode::OK, "owner thumbnail");
}

#[tokio::test]
async fn a_user_without_a_connected_connector_is_told_so() {
    let Some(harness) = Harness::connect(three_picks()).await else {
        return;
    };
    let user = harness.user("dave").await;

    let (status, body) = harness
        .send(
            Request::builder()
                .method("POST")
                .uri("/api/v1/connectors/photos/sessions")
                .header(header::COOKIE, &user.cookie)
                .body(Body::empty())
                .expect("build create request"),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "no connector, no pick: {body}"
    );
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("not connected"),
        "the message must say what is missing: {body}"
    );
}

/// The same selection picked twice files the items once (T27 acceptance).
///
/// Google hands out a fresh session per pick, so "twice" is two sessions over
/// the same media item ids — the dedupe key, not the session, is what makes the
/// second run a skip. And the notes are the *picker's* notes: a second user
/// picking the same photos gets their own copy, because dedupe is per user.
#[tokio::test]
async fn picking_the_same_selection_twice_imports_it_once() {
    let Some(harness) = Harness::connect(three_picks()).await else {
        return;
    };
    let alice = harness.user("alice-twice").await;
    harness.connect_photos(&alice).await;

    let first = harness.start_pick(&alice).await;
    let (status, body) = harness.poll(&alice, &first).await;
    assert_eq!(status, StatusCode::OK, "first pick: {body}");
    assert_eq!(body["state"], "imported");
    assert_eq!(body["notes_created"], 3);
    assert_eq!(body["notes_skipped"], 0);

    // A second session, the same three items.
    let second = harness.start_pick(&alice).await;
    let (status, body) = harness.poll(&alice, &second).await;
    assert_eq!(status, StatusCode::OK, "second pick: {body}");
    assert_eq!(body["state"], "imported");
    assert_eq!(body["notes_created"], 0, "nothing new: {body}");
    assert_eq!(body["notes_skipped"], 3, "all three already filed: {body}");

    assert_eq!(harness.note_ids(&alice).await.len(), 3, "still three notes");

    // Attribution: the notes are hers, and a different account picking the same
    // photos gets its own three rather than sharing or stealing them.
    let bob = harness.user("bob-twice").await;
    harness.connect_photos(&bob).await;
    let bobs_pick = harness.start_pick(&bob).await;
    let (status, body) = harness.poll(&bob, &bobs_pick).await;
    assert_eq!(status, StatusCode::OK, "bob's pick: {body}");
    assert_eq!(body["notes_created"], 3, "his own copy: {body}");

    let alice_notes = harness.note_ids(&alice).await;
    let bob_notes = harness.note_ids(&bob).await;
    assert_eq!(alice_notes.len(), 3);
    assert_eq!(bob_notes.len(), 3);
    assert!(
        alice_notes
            .iter()
            .all(|(id, _)| !bob_notes.iter().any(|(other, _)| other == id)),
        "no note is shared between accounts"
    );

    let owners: Vec<(Uuid, i64)> = sqlx::query_as(
        "SELECT user_id, count(*) FROM notes WHERE user_id = ANY($1) GROUP BY user_id",
    )
    .bind(vec![alice.id, bob.id])
    .fetch_all(&harness.pool)
    .await
    .expect("count notes per owner");
    assert_eq!(owners.len(), 2, "both accounts own rows: {owners:?}");
    assert!(owners.iter().all(|(_, count)| *count == 3), "{owners:?}");
}
