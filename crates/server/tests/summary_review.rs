//! T8 acceptance: a generated summary is a proposal — approving it commits it,
//! discarding it drops it, editing it records a new `edited` version — and every
//! one of those actions scopes to the owner (a foreign tag answers 404).

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{Duration, Utc};
use http_body_util::BodyExt;
use sqlx::PgPool;
use strands_api_types::{
    EditSummaryRequest, GenerateSummaryRequest, SummaryGuide, SummaryHistory, SummaryState,
    SummaryVersion,
};
use strands_core::{Registry, TagId, UserId};
use strands_server::app::{AppState, router};
use strands_server::auth::identity::BoxFuture as IdentityFuture;
use strands_server::auth::{AuthError, ConsentRequest, Identity, IdentityProvider};
use strands_server::config::AuthConfig;
use strands_server::summary::{BoxFuture, SummarizeError, Summarizer};
use tower::ServiceExt;
use uuid::Uuid;

const MODEL_ALIAS: &str = "test/summary";

/// Answers every prompt with the same text; these tests are about the review
/// lifecycle, not about what the model would say.
struct FixedSummarizer;

impl Summarizer for FixedSummarizer {
    fn model_alias(&self) -> &str {
        MODEL_ALIAS
    }

    fn summarize<'a>(&'a self, _prompt: &'a str) -> BoxFuture<'a, Result<String, SummarizeError>> {
        Box::pin(async { Ok("generated proposal".to_owned()) })
    }
}

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
    ) -> IdentityFuture<'a, Result<Identity, AuthError>> {
        Box::pin(async { Err(AuthError::InvalidCode) })
    }
}

struct Harness {
    pool: PgPool,
    app: Router,
}

struct TestUser {
    id: UserId,
    cookie: String,
}

impl Harness {
    async fn connect() -> Option<Self> {
        let pool = common::connect("summary_review").await?;
        let state = AppState::new(
            pool.clone(),
            AuthConfig {
                client_id: "unused-client-id".into(),
                client_secret: "unused-client-secret".into(),
                public_base_url: "https://test.example".into(),
                cookie_secure: true,
                session_ttl_days: 30,
                authorization_endpoint: "https://identity.example/authorize".into(),
                token_endpoint: "https://identity.example/token".into(),
            },
            Arc::new(UnusedIdentity),
            Arc::new(Registry::new()),
            Arc::new(FixedSummarizer),
        );
        Some(Self {
            pool,
            app: router(state),
        })
    }

    async fn user(&self) -> TestUser {
        let user_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO users (id, google_sub, email, name) VALUES ($1, $2, $3, 'Test User')",
        )
        .bind(user_id)
        .bind(format!("review-test-subject-{user_id}"))
        .bind(format!("review-test-{user_id}@example.com"))
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
        TestUser {
            id: user_id,
            cookie: format!("strands_session={session_id}"),
        }
    }

    async fn tag_with_note(&self, user: &TestUser, note_body: &str) -> TagId {
        let category_id = Uuid::new_v4();
        let tag_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO categories (id, user_id, name, color, position) \
             VALUES ($1, $2, 'Trips', '#123456', 0)",
        )
        .bind(category_id)
        .bind(user.id)
        .execute(&self.pool)
        .await
        .expect("insert test category");
        sqlx::query(
            "INSERT INTO tags (id, user_id, category_id, name) VALUES ($1, $2, $3, 'Japan')",
        )
        .bind(tag_id)
        .bind(user.id)
        .bind(category_id)
        .execute(&self.pool)
        .await
        .expect("insert test tag");
        sqlx::query(
            "INSERT INTO notes \
                (id, user_id, tag_id, kind, body, occurred_at, source, dedupe_key) \
             VALUES ($1, $2, $3, 'text', $4, $5, 'manual', $6)",
        )
        .bind(Uuid::new_v4())
        .bind(user.id)
        .bind(tag_id)
        .bind(note_body)
        .bind(Utc::now())
        .bind(format!("review-test-note-{}", Uuid::new_v4()))
        .execute(&self.pool)
        .await
        .expect("insert test note");
        tag_id
    }

    async fn generate(&self, user: &TestUser, tag_id: TagId, force: bool) -> SummaryVersion {
        let request = GenerateSummaryRequest {
            guide: SummaryGuide::Shorter,
            force,
        };
        let (status, body) = self
            .post_json(
                &format!("/api/v1/tags/{tag_id}/summary"),
                &user.cookie,
                &request,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "generation failed: {body}");
        serde_json::from_value(body).expect("decode summary version")
    }

    async fn approve(
        &self,
        user: &TestUser,
        tag_id: TagId,
        summary_id: Uuid,
    ) -> (StatusCode, serde_json::Value) {
        self.post_json(
            &format!("/api/v1/tags/{tag_id}/summary/{summary_id}/approve"),
            &user.cookie,
            &serde_json::json!({}),
        )
        .await
    }

    async fn discard(
        &self,
        user: &TestUser,
        tag_id: TagId,
        summary_id: Uuid,
    ) -> (StatusCode, serde_json::Value) {
        self.post_json(
            &format!("/api/v1/tags/{tag_id}/summary/{summary_id}/discard"),
            &user.cookie,
            &serde_json::json!({}),
        )
        .await
    }

    async fn edit(
        &self,
        user: &TestUser,
        tag_id: TagId,
        summary_id: Uuid,
        body: &str,
    ) -> (StatusCode, serde_json::Value) {
        let request = EditSummaryRequest {
            body: body.to_owned(),
        };
        self.post_json(
            &format!("/api/v1/tags/{tag_id}/summary/{summary_id}/edit"),
            &user.cookie,
            &request,
        )
        .await
    }

    async fn history(&self, user: &TestUser, tag_id: TagId) -> SummaryHistory {
        let (status, body) = self
            .get_json(&format!("/api/v1/tags/{tag_id}/summary/history"), user)
            .await;
        assert_eq!(status, StatusCode::OK, "history failed: {body}");
        serde_json::from_value(body).expect("decode summary history")
    }

    async fn get_json(&self, uri: &str, user: &TestUser) -> (StatusCode, serde_json::Value) {
        self.send(
            Request::builder()
                .uri(uri)
                .header(header::COOKIE, &user.cookie)
                .body(Body::empty())
                .expect("build GET"),
        )
        .await
    }

    async fn post_json<T: serde::Serialize>(
        &self,
        uri: &str,
        cookie: &str,
        body: &T,
    ) -> (StatusCode, serde_json::Value) {
        self.send(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(body).expect("serialize request body"),
                ))
                .expect("build POST"),
        )
        .await
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .expect("router response");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("read response body")
            .to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, body)
    }
}

#[tokio::test]
async fn approving_a_proposal_commits_it_and_survives_a_reload() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let tag_id = harness.tag_with_note(&user, "Tokyo on 2026-03-10").await;
    let version = harness.generate(&user, tag_id, false).await;

    let before = harness.history(&user, tag_id).await;
    assert_eq!(before.approved_version_id, None);
    assert_eq!(before.versions[0].state, SummaryState::Proposed);

    let (status, body) = harness.approve(&user, tag_id, version.id).await;
    assert_eq!(status, StatusCode::OK, "approve failed: {body}");

    // A second request is the reload: the pointer lives on the tag, not in the
    // client, so what the card reads back is what was committed.
    let after = harness.history(&user, tag_id).await;
    assert_eq!(after.approved_version_id, Some(version.id));
    assert_eq!(after.versions.len(), 1);
    assert_eq!(after.versions[0].state, SummaryState::Approved);
    assert_eq!(after.versions[0].body, version.body);
}

#[tokio::test]
async fn discarding_a_proposal_restores_the_previously_approved_body() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let tag_id = harness.tag_with_note(&user, "Tokyo on 2026-03-10").await;

    let approved = harness.generate(&user, tag_id, false).await;
    let (status, body) = harness.approve(&user, tag_id, approved.id).await;
    assert_eq!(status, StatusCode::OK, "approve failed: {body}");

    let proposal = harness.generate(&user, tag_id, true).await;
    assert_ne!(proposal.id, approved.id);

    let (status, body) = harness.discard(&user, tag_id, proposal.id).await;
    assert_eq!(status, StatusCode::OK, "discard failed: {body}");

    let after = harness.history(&user, tag_id).await;
    assert_eq!(
        after.approved_version_id,
        Some(approved.id),
        "discarding the proposal must leave the approved version in force"
    );
    assert_eq!(after.versions[0].id, proposal.id);
    assert_eq!(after.versions[0].state, SummaryState::Discarded);
    assert_eq!(after.versions[1].state, SummaryState::Approved);
}

#[tokio::test]
async fn discarding_the_only_proposal_leaves_no_approved_body() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let tag_id = harness.tag_with_note(&user, "Tokyo on 2026-03-10").await;
    let proposal = harness.generate(&user, tag_id, false).await;

    let (status, body) = harness.discard(&user, tag_id, proposal.id).await;
    assert_eq!(status, StatusCode::OK, "discard failed: {body}");

    let after = harness.history(&user, tag_id).await;
    assert_eq!(after.approved_version_id, None);
    assert_eq!(after.versions[0].state, SummaryState::Discarded);
}

#[tokio::test]
async fn a_discarded_version_cannot_be_approved() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let tag_id = harness.tag_with_note(&user, "Tokyo on 2026-03-10").await;
    let proposal = harness.generate(&user, tag_id, false).await;
    let (status, _) = harness.discard(&user, tag_id, proposal.id).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = harness.approve(&user, tag_id, proposal.id).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a dropped proposal cannot come back: {body}"
    );
    assert_eq!(
        harness.history(&user, tag_id).await.approved_version_id,
        None
    );
}

#[tokio::test]
async fn editing_a_proposal_records_a_new_version_and_keeps_the_original() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let tag_id = harness.tag_with_note(&user, "Tokyo on 2026-03-10").await;
    let generated = harness.generate(&user, tag_id, false).await;

    let (status, body) = harness
        .edit(&user, tag_id, generated.id, "my own words")
        .await;
    assert_eq!(status, StatusCode::OK, "edit failed: {body}");

    let after = harness.history(&user, tag_id).await;
    assert_eq!(after.versions.len(), 2, "an edit adds a version");
    assert_eq!(after.versions[0].state, SummaryState::Edited);
    assert_eq!(after.versions[0].body, "my own words");
    assert_eq!(after.versions[0].version, generated.version + 1);

    // The generated row is untouched: same body, still a proposal.
    let original = after
        .versions
        .iter()
        .find(|version| version.id == generated.id)
        .expect("generated version is still in history");
    assert_eq!(original.body, generated.body);
    assert_eq!(original.state, SummaryState::Proposed);
    assert_eq!(after.approved_version_id, None);
}

#[tokio::test]
async fn approving_an_edited_version_commits_the_rewrite() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let tag_id = harness.tag_with_note(&user, "Tokyo on 2026-03-10").await;
    let generated = harness.generate(&user, tag_id, false).await;
    let (status, body) = harness
        .edit(&user, tag_id, generated.id, "my own words")
        .await;
    assert_eq!(status, StatusCode::OK, "edit failed: {body}");
    let edited = harness.history(&user, tag_id).await.versions[0].clone();

    let (status, body) = harness.approve(&user, tag_id, edited.id).await;
    assert_eq!(status, StatusCode::OK, "approve failed: {body}");

    let after = harness.history(&user, tag_id).await;
    assert_eq!(after.approved_version_id, Some(edited.id));
    assert_eq!(after.versions[0].state, SummaryState::Approved);
    assert_eq!(after.versions[0].body, "my own words");
}

#[tokio::test]
async fn another_users_tag_answers_404_for_every_review_action() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let owner = harness.user().await;
    let stranger = harness.user().await;
    let tag_id = harness.tag_with_note(&owner, "Tokyo on 2026-03-10").await;
    let version = harness.generate(&owner, tag_id, false).await;

    let (status, body) = harness
        .get_json(&format!("/api/v1/tags/{tag_id}/summary/history"), &stranger)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "history leaked: {body}");

    let (status, body) = harness.approve(&stranger, tag_id, version.id).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "approve leaked: {body}");

    let (status, body) = harness.discard(&stranger, tag_id, version.id).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "discard leaked: {body}");

    let (status, body) = harness
        .edit(&stranger, tag_id, version.id, "not mine")
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "edit leaked: {body}");

    // And the owner's version is still exactly as it was.
    let owner_history = harness.history(&owner, tag_id).await;
    assert_eq!(owner_history.versions.len(), 1);
    assert_eq!(owner_history.versions[0].state, SummaryState::Proposed);
}
