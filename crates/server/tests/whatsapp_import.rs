//! WhatsApp connector acceptance: allowlisted chats import, nothing else does.
//!
//! The bridge is exercised through its fake transport so the import rules can
//! be checked without a phone: the fake session is the same trait the live
//! client implements, so what passes here is the code the live path runs.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{Duration, TimeZone, Utc};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use strands_api_types::{WhatsAppAllowlist, WhatsAppImportReport};
use strands_core::{Registry as CoreRegistry, UserId};
use strands_server::app::{AppState, router};
use strands_server::auth::identity::BoxFuture;
use strands_server::auth::{AuthError, ConsentRequest, Identity, IdentityProvider};
use strands_server::config::AuthConfig;
use strands_server::whatsapp::bridge::{
    BufferedMessage, ChatSummary, Jid, LinkState, Registry, SessionFactory, WhatsAppError,
    WhatsAppSession,
};
use strands_server::whatsapp::store::SessionCipher;

const ALLOWED: &str = "15550100@s.whatsapp.net";
const OTHER: &str = "15550200@s.whatsapp.net";

/// A session that answers from a buffer the test controls.
struct FakeSession {
    state: Mutex<LinkState>,
    chats: Mutex<Vec<ChatSummary>>,
    buffer: Mutex<Vec<BufferedMessage>>,
}

impl FakeSession {
    fn linked() -> Self {
        Self {
            state: Mutex::new(LinkState::Linked),
            chats: Mutex::new(Vec::new()),
            buffer: Mutex::new(Vec::new()),
        }
    }

    fn push(&self, messages: Vec<BufferedMessage>) {
        self.buffer.lock().expect("buffer lock").extend(messages);
    }
}

impl WhatsAppSession for FakeSession {
    fn state(&self) -> LinkState {
        self.state.lock().expect("state lock").clone()
    }

    fn chats(&self) -> Vec<ChatSummary> {
        self.chats.lock().expect("chats lock").clone()
    }

    fn peek(&self) -> Vec<BufferedMessage> {
        self.buffer.lock().expect("buffer lock").clone()
    }

    fn drain(&self) -> Vec<BufferedMessage> {
        std::mem::take(&mut *self.buffer.lock().expect("buffer lock"))
    }

    fn sync(&self) -> BoxFuture<'_, Result<(), WhatsAppError>> {
        Box::pin(async { Ok(()) })
    }

    fn unlink(&self) -> BoxFuture<'_, Result<(), WhatsAppError>> {
        Box::pin(async {
            *self.state.lock().expect("state lock") = LinkState::Unlinked;
            Ok(())
        })
    }

    fn park(&self) -> BoxFuture<'_, Result<Option<Vec<u8>>, WhatsAppError>> {
        Box::pin(async { Ok(None) })
    }
}

/// Hands every account the same fake session, keyed by user.
struct FakeFactory {
    sessions: Mutex<HashMap<UserId, Arc<FakeSession>>>,
}

impl FakeFactory {
    fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn session(&self, user_id: UserId) -> Arc<FakeSession> {
        self.sessions
            .lock()
            .expect("factory lock")
            .entry(user_id)
            .or_insert_with(|| Arc::new(FakeSession::linked()))
            .clone()
    }
}

impl SessionFactory for FakeFactory {
    fn open(
        &self,
        user_id: UserId,
        _restore: Option<Vec<u8>>,
    ) -> BoxFuture<'_, Result<Arc<dyn WhatsAppSession>, WhatsAppError>> {
        let session = self.session(user_id);
        Box::pin(async move { Ok(session as Arc<dyn WhatsAppSession>) })
    }
}

struct Harness {
    pool: PgPool,
    app: Router,
    factory: Arc<FakeFactory>,
    whatsapp: Arc<Registry>,
}

/// Stands in for the browser-facing provider; no route here reaches it.
struct UnusedIdentity;

impl IdentityProvider for UnusedIdentity {
    fn consent_url(&self, _request: &ConsentRequest<'_>) -> String {
        "https://identity.example/unused".to_owned()
    }

    fn exchange_code<'a>(
        &'a self,
        _code: &'a str,
        _code_verifier: &'a str,
        _redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<Identity, AuthError>> {
        Box::pin(async { Err(AuthError::InvalidCode) })
    }
}

impl Harness {
    async fn connect() -> Option<Self> {
        let pool = common::connect("whatsapp_import").await?;
        let factory = Arc::new(FakeFactory::new());
        let whatsapp = Arc::new(Registry::new(factory.clone()));
        let cipher =
            Arc::new(SessionCipher::from_bytes(&[7u8; 32]).expect("32-byte test key is valid"));
        let state = AppState::new(
            pool.clone(),
            AuthConfig {
                client_id: "test-client".into(),
                issuer: "https://identity.example".into(),
                public_base_url: "https://strands.test".into(),
                cookie_secure: true,
                session_ttl_days: 30,
            },
            Arc::new(UnusedIdentity),
            Arc::new(CoreRegistry::new()),
            Arc::new(strands_server::summary::LiteLlmSummarizer::new(
                strands_server::llm::LiteLlmClient::new("http://unused.invalid", "unused"),
            )),
        )
        .with_whatsapp(whatsapp.clone(), cipher);
        Some(Self {
            pool,
            app: router(state),
            factory,
            whatsapp,
        })
    }

    async fn user(&self) -> User {
        let user_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, google_sub, email, name) VALUES ($1, $2, $3, 'Test')")
            .bind(user_id)
            .bind(format!("whatsapp-test-subject-{user_id}"))
            .bind(format!("whatsapp-test-{user_id}@example.com"))
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
        User {
            id: user_id,
            cookie: format!("strands_session={session_id}"),
        }
    }

    /// Open the account's session the way a paired client would.
    async fn open_session(&self, user: &User) -> Arc<FakeSession> {
        let session = self.factory.session(user.id);
        self.whatsapp
            .connect(user.id, None)
            .await
            .expect("open fake session");
        session
    }

    async fn set_allowlist(&self, user: &User, jids: &[&str]) -> StatusCode {
        let body = serde_json::to_vec(&WhatsAppAllowlist {
            jids: jids.iter().map(|jid| (*jid).to_owned()).collect(),
        })
        .expect("encode allowlist");
        let request = Request::builder()
            .method("PUT")
            .uri("/api/v1/whatsapp/allowlist")
            .header(header::COOKIE, &user.cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .expect("build allowlist request");
        self.app
            .clone()
            .oneshot(request)
            .await
            .expect("router response")
            .status()
    }

    async fn import(&self, user: &User) -> (StatusCode, WhatsAppImportReport) {
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/whatsapp/import")
            .header(header::COOKIE, &user.cookie)
            .body(Body::empty())
            .expect("build import request");
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .expect("router response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("read import body");
        let report = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "decode import report ({status}): {error}: {}",
                String::from_utf8_lossy(&bytes)
            )
        });
        (status, report)
    }
}

struct User {
    id: UserId,
    cookie: String,
}

fn message(chat: &str, id: &str, body: &str, minute: u32) -> BufferedMessage {
    BufferedMessage {
        jid: Jid::parse(chat).expect("valid jid"),
        message_id: id.to_owned(),
        sent_at: Utc
            .with_ymd_and_hms(2026, 3, 1, 9, minute, 0)
            .single()
            .expect("valid timestamp"),
        from_me: false,
        sender: Some("Ana".to_owned()),
        body: body.to_owned(),
    }
}

async fn note_count(pool: &PgPool, user_id: UserId) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM notes WHERE user_id = $1 AND dedupe_key LIKE 'whatsapp:%'",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .expect("count whatsapp notes")
}

#[tokio::test]
async fn allowlisted_chat_imports_once_and_others_never() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let session = harness.open_session(&user).await;

    session.push(vec![
        message(ALLOWED, "A1", "Dentist moved to Thursday", 1),
        message(ALLOWED, "A2", "Bring the insurance card", 2),
        message(OTHER, "B1", "not allowlisted", 3),
    ]);
    assert_eq!(
        harness.set_allowlist(&user, &[ALLOWED]).await,
        StatusCode::OK
    );

    let (status, report) = harness.import(&user).await;
    assert_eq!(status, StatusCode::OK, "import failed: {report:?}");
    assert_eq!(report.notes_created, 2, "only allowlisted chat imports");
    assert_eq!(report.chats_imported, 1);
    assert_eq!(
        report.chats_skipped, 1,
        "the other chat is seen and skipped"
    );

    assert_eq!(
        note_count(&harness.pool, user.id).await,
        2,
        "non-allowlisted chat wrote nothing"
    );

    // The same two messages are delivered again, as history sync and live
    // delivery do: the dedupe key must absorb them.
    session.push(vec![
        message(ALLOWED, "A1", "Dentist moved to Thursday", 1),
        message(ALLOWED, "A2", "Bring the insurance card", 2),
    ]);
    let (status, report) = harness.import(&user).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report.notes_created, 0, "re-delivery must not duplicate");
    assert_eq!(note_count(&harness.pool, user.id).await, 2);

    // The text reached the note, and the note is on the WhatsApp tag, so the
    // import went through the shared tagging path rather than beside it.
    let bodies: Vec<String> = sqlx::query_scalar(
        "SELECT body FROM notes WHERE user_id = $1 AND dedupe_key LIKE 'whatsapp:%' ORDER BY body",
    )
    .bind(user.id)
    .fetch_all(&harness.pool)
    .await
    .expect("read imported notes");
    assert_eq!(
        bodies,
        vec![
            "Bring the insurance card".to_owned(),
            "Dentist moved to Thursday".to_owned()
        ]
    );

    let tag: Option<String> = sqlx::query_scalar(
        "SELECT tags.name FROM notes JOIN tags ON tags.id = notes.tag_id \
         WHERE notes.user_id = $1 AND notes.dedupe_key LIKE 'whatsapp:%' \
         GROUP BY tags.name",
    )
    .bind(user.id)
    .fetch_optional(&harness.pool)
    .await
    .expect("read tag of imported notes");
    assert_eq!(tag.as_deref(), Some("WhatsApp"));
}

#[tokio::test]
async fn import_without_allowlist_writes_nothing() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let session = harness.open_session(&user).await;
    session.push(vec![message(ALLOWED, "C1", "unrequested", 4)]);

    let (status, report) = harness.import(&user).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report.notes_created, 0, "default allowlist is empty");
    assert_eq!(report.chats_skipped, 1);
    assert_eq!(note_count(&harness.pool, user.id).await, 0);
}

#[tokio::test]
async fn import_without_a_paired_session_is_rejected() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;

    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/whatsapp/import")
        .header(header::COOKIE, &user.cookie)
        .body(Body::empty())
        .expect("build import request");
    let status = harness
        .app
        .clone()
        .oneshot(request)
        .await
        .expect("router response")
        .status();
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn whatsapp_endpoints_require_a_session() {
    let Some(harness) = Harness::connect().await else {
        return;
    };

    // No cookie at all: the extractor answers before any handler runs, on every
    // endpoint, because there is nothing a caller could name to reach another
    // account (no user id or connector id appears in any path).
    for (method, uri) in [
        ("GET", "/api/v1/whatsapp"),
        ("POST", "/api/v1/whatsapp/pair"),
        ("PUT", "/api/v1/whatsapp/allowlist"),
        ("POST", "/api/v1/whatsapp/import"),
        ("POST", "/api/v1/whatsapp/sync"),
        ("POST", "/api/v1/whatsapp/disconnect"),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .expect("build request");
        let status = harness
            .app
            .clone()
            .oneshot(request)
            .await
            .expect("router response")
            .status();
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {uri} must reject an anonymous caller"
        );
    }
}

#[tokio::test]
async fn allowlist_and_import_are_scoped_to_the_caller() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let first = harness.user().await;
    let second = harness.user().await;

    // Both accounts are paired and both see the same chat; only the first
    // allowlists it. If any part of the path leaked the account, the second
    // import would pick up the first account's allowlist.
    let first_session = harness.open_session(&first).await;
    let second_session = harness.open_session(&second).await;
    first_session.push(vec![message(ALLOWED, "S1", "only for the first", 5)]);
    second_session.push(vec![message(ALLOWED, "S2", "only for the second", 6)]);
    assert_eq!(
        harness.set_allowlist(&first, &[ALLOWED]).await,
        StatusCode::OK
    );

    let (status, report) = harness.import(&second).await;
    assert_eq!(status, StatusCode::OK, "import failed: {report:?}");
    assert_eq!(
        report.notes_created, 0,
        "second account allowlisted nothing"
    );
    assert_eq!(note_count(&harness.pool, second.id).await, 0);

    let (status, report) = harness.import(&first).await;
    assert_eq!(status, StatusCode::OK, "import failed: {report:?}");
    assert_eq!(report.notes_created, 1);
    assert_eq!(note_count(&harness.pool, first.id).await, 1);
}
