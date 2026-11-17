//! T21 acceptance: geocode cache, export backfill, Nearby staging.
//!
//! Everything runs through the real router against a live Postgres when
//! `STRANDS_TEST_REQUIRE_DB=1` is set. The fake resolver counts its calls, so
//! "the cache worked" is asserted as `reverse_calls` staying put across a second
//! request — not as the absence of an error.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{Duration, Utc};
use http_body_util::BodyExt;
use serde::de::DeserializeOwned;
use sqlx::PgPool;
use strands_api_types::{AcceptedPlace, BackfillResult, NearbyResult};
use strands_core::{Registry, UserId};
use strands_server::app::{AppState, router};
use strands_server::auth::identity::BoxFuture as IdentityFuture;
use strands_server::auth::{AuthError, ConsentRequest, Identity, IdentityProvider};
use strands_server::config::AuthConfig;
use strands_server::gmaps::places::{PlaceError, PlaceQuery, PlaceResolver, ResolvedPlace};
use strands_server::llm::BoxFuture;
use tower::ServiceExt;
use uuid::Uuid;

/// Stand-in for Geocoding + Nearby Search. Counts both lanes so the assertions
/// can distinguish a cache hit from a silent re-query.
struct FakePlaces {
    reverse_calls: AtomicUsize,
    nearby_calls: AtomicUsize,
}

impl FakePlaces {
    fn new() -> Self {
        Self {
            reverse_calls: AtomicUsize::new(0),
            nearby_calls: AtomicUsize::new(0),
        }
    }

    fn reverse_calls(&self) -> usize {
        self.reverse_calls.load(Ordering::SeqCst)
    }

    fn nearby_calls(&self) -> usize {
        self.nearby_calls.load(Ordering::SeqCst)
    }
}

impl PlaceResolver for FakePlaces {
    fn resolve<'a>(
        &'a self,
        _query: PlaceQuery<'a>,
    ) -> BoxFuture<'a, Result<Option<ResolvedPlace>, PlaceError>> {
        Box::pin(async { Ok(None) })
    }

    fn reverse<'a>(
        &'a self,
        lat: f64,
        lng: f64,
    ) -> BoxFuture<'a, Result<Option<ResolvedPlace>, PlaceError>> {
        Box::pin(async move {
            self.reverse_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Some(ResolvedPlace {
                name: String::new(),
                address: Some("Somewhere in Lisbon".to_owned()),
                lat,
                lng,
                place_id: None,
                primary_type: None,
                types: Vec::new(),
                metro: Some("Lisbon District".to_owned()),
                locality: Some("Lisbon".to_owned()),
                state: Some("Lisbon".to_owned()),
                country: Some("Portugal".to_owned()),
            }))
        })
    }

    fn nearby<'a>(
        &'a self,
        lat: f64,
        lng: f64,
        _radius_m: u32,
        _max: usize,
    ) -> BoxFuture<'a, Result<Vec<ResolvedPlace>, PlaceError>> {
        Box::pin(async move {
            self.nearby_calls.fetch_add(1, Ordering::SeqCst);
            // Eleven results on purpose: the handler must cap at ten. The first
            // one carries a type no vocabulary entry knows, and the formatted
            // address deliberately names a different city than the locality
            // component so the metro tag cannot come from the address string.
            Ok((0..11)
                .map(|index| ResolvedPlace {
                    name: format!("Candidate {index}"),
                    address: Some("1 Nowhere Road, Faraway".to_owned()),
                    lat: lat + index as f64 * 0.00001,
                    lng,
                    place_id: Some(format!("fake-place-{index}")),
                    primary_type: Some(if index == 0 {
                        "quantum_teleport_hub".to_owned()
                    } else {
                        "cafe".to_owned()
                    }),
                    types: vec!["point_of_interest".to_owned()],
                    metro: Some("Lisbon District".to_owned()),
                    locality: Some("Lisbon".to_owned()),
                    state: Some("Lisbon".to_owned()),
                    country: Some("Portugal".to_owned()),
                })
                .collect())
        })
    }
}

struct Harness {
    pool: PgPool,
    app: Router,
    places: Arc<FakePlaces>,
}

struct TestUser {
    id: UserId,
    cookie: String,
}

impl Harness {
    async fn connect() -> Option<Self> {
        let pool = common::connect("places").await?;
        let places = Arc::new(FakePlaces::new());
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
            Arc::new(strands_server::summary::LiteLlmSummarizer::new(
                strands_server::llm::LiteLlmClient::new("http://unused.invalid", "unused"),
            )),
        )
        .with_geocoder(places.clone());
        Some(Self {
            pool,
            app: router(state),
            places,
        })
    }

    async fn user(&self) -> TestUser {
        let user_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, google_sub, email, name) VALUES ($1, $2, $3, 'Test')")
            .bind(user_id)
            .bind(format!("places-test-subject-{user_id}"))
            .bind(format!("places-test-{user_id}@example.com"))
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

    async fn post<T: DeserializeOwned>(
        &self,
        user: &TestUser,
        uri: &str,
        body: serde_json::Value,
    ) -> (StatusCode, Vec<u8>) {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::COOKIE, &user.cookie)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("build POST request");
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
            .expect("read body")
            .to_bytes();
        let _ = std::marker::PhantomData::<T>;
        (status, bytes.to_vec())
    }

    async fn backfill(
        &self,
        user: &TestUser,
        body: serde_json::Value,
    ) -> (StatusCode, BackfillResult) {
        let (status, bytes) = self
            .post::<BackfillResult>(user, "/api/v1/places/backfill", body)
            .await;
        let result = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "decode backfill result ({status}): {error}: {}",
                String::from_utf8_lossy(&bytes)
            )
        });
        (status, result)
    }

    async fn nearby(&self, user: &TestUser, body: serde_json::Value) -> (StatusCode, NearbyResult) {
        let (status, bytes) = self
            .post::<NearbyResult>(user, "/api/v1/places/nearby", body)
            .await;
        let result = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "decode nearby result ({status}): {error}: {}",
                String::from_utf8_lossy(&bytes)
            )
        });
        (status, result)
    }

    async fn accept(
        &self,
        user: &TestUser,
        candidate_id: Uuid,
    ) -> (StatusCode, Option<AcceptedPlace>) {
        let (status, bytes) = self
            .post::<AcceptedPlace>(
                user,
                &format!("/api/v1/places/candidates/{candidate_id}/accept"),
                serde_json::json!({}),
            )
            .await;
        let accepted = serde_json::from_slice(&bytes).ok();
        (status, accepted)
    }

    async fn count(&self, user: &TestUser, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .bind(user.id)
            .fetch_one(&self.pool)
            .await
            .expect("count rows")
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

fn item(lat: f64, lng: f64) -> serde_json::Value {
    serde_json::json!({
        "source": "timeline",
        "lat": lat,
        "lng": lng,
        "occurred_at": "2024-03-01T10:00:00Z"
    })
}

#[tokio::test]
async fn same_rounded_key_geocodes_once_across_two_requests() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    // Two GPS fixes five metres apart: same four-decimal key.
    let first_body = serde_json::json!({ "items": [item(38.72231, -9.13931)] });

    let (status, first) = harness.backfill(&user, first_body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first.seen, 1);
    assert_eq!(first.resolved, 1);
    assert_eq!(first.skipped, 0);
    assert_eq!(first.cache_hits, 0);
    assert_eq!(first.cache_misses, 1);
    assert_eq!(first.places.len(), 1);
    assert_eq!(first.places[0].coord_key, "38.7223,-9.1393");
    assert_eq!(first.places[0].metro_tag.as_deref(), Some("Metro: Lisbon"));
    assert_eq!(harness.places.reverse_calls(), 1);

    // Same place, coordinate nudged by ~5 m: still the same key, and the answer
    // now comes from Postgres — the fake is never asked again.
    let (status, second) = harness
        .backfill(
            &user,
            serde_json::json!({ "items": [item(38.72226, -9.13934)] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second.resolved, 1);
    assert_eq!(
        second.cache_hits, 1,
        "second request must report a cache hit"
    );
    assert_eq!(second.cache_misses, 0);
    assert_eq!(
        harness.places.reverse_calls(),
        1,
        "a cache hit must not re-query the provider"
    );

    // The persisted row is the evidence, not just the counter.
    let cached: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM geocode_cache WHERE user_id = $1 AND coord_key = $2",
    )
    .bind(user.id)
    .bind("38.7223,-9.1393")
    .fetch_one(&harness.pool)
    .await
    .expect("count cache rows");
    assert_eq!(cached, 1);
}

#[tokio::test]
async fn unknown_primary_type_lands_in_the_catch_all() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;

    let (status, result) = harness
        .nearby(&user, serde_json::json!({ "lat": 38.7223, "lng": -9.1393 }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result.candidates.len(), 10, "eleven results cap at ten");

    let unknown = &result.candidates[0];
    assert_eq!(
        unknown.primary_type.as_deref(),
        Some("quantum_teleport_hub")
    );
    assert_eq!(
        unknown.poi_tag, "Other",
        "unknown type falls to the catch-all"
    );
    assert_eq!(
        unknown.metro_tag.as_deref(),
        Some("Metro: Lisbon"),
        "metro comes from the locality component, not the formatted address"
    );
    assert!(result.candidates[1..].iter().all(|c| c.poi_tag == "Cafe"));
}

#[tokio::test]
async fn timeline_backfill_resolves_n_and_skips_duplicates() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    // Three distinct places, each appearing twice in the export.
    let items = vec![
        item(38.7223, -9.1393),
        item(38.7223, -9.1393),
        item(38.7333, -9.1444),
        item(38.7333, -9.1444),
        item(38.7444, -9.1555),
        item(38.7444, -9.1555),
    ];
    let body = serde_json::json!({ "items": items });

    let (status, first) = harness.backfill(&user, body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first.seen, 6);
    assert_eq!(first.resolved, 3, "one provider answer per distinct key");
    assert_eq!(first.skipped, 3, "duplicates are skipped, not re-geocoded");
    assert_eq!(first.cache_misses, 3);
    assert_eq!(first.cache_hits, 0);
    assert_eq!(harness.places.reverse_calls(), 3);
    assert_eq!(first.places.len(), 3);

    let (status, second) = harness.backfill(&user, body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second.resolved, 3);
    assert_eq!(second.skipped, 3);
    assert_eq!(second.cache_hits, 3);
    assert_eq!(second.cache_misses, 0);
    assert_eq!(harness.places.reverse_calls(), 3, "replay is all cache");
}

#[tokio::test]
async fn eleven_candidates_yield_ten_and_no_notes_until_tapped() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let other = harness.user().await;

    let (status, result) = harness
        .nearby(&user, serde_json::json!({ "lat": 38.7223, "lng": -9.1393 }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result.candidates.len(), 10);
    assert_eq!(harness.places.nearby_calls(), 1);
    assert_eq!(
        harness
            .count(&user, "SELECT count(*) FROM notes WHERE user_id = $1")
            .await,
        0,
        "Nearby must not auto-add a note"
    );

    // Another user cannot accept this candidate: not found, not forbidden.
    let (status, _) = harness.accept(&other, result.candidates[0].id).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        harness
            .count(&user, "SELECT count(*) FROM notes WHERE user_id = $1")
            .await,
        0
    );

    let (status, accepted) = harness.accept(&user, result.candidates[0].id).await;
    assert_eq!(status, StatusCode::OK);
    let accepted = accepted.expect("accept returns JSON");
    assert_eq!(accepted.name, result.candidates[0].name);
    assert_eq!(
        harness
            .count(&user, "SELECT count(*) FROM notes WHERE user_id = $1")
            .await,
        1,
        "the tap is what creates the note"
    );

    // The note is filed under the catch-all tag and the metro tag.
    let tags: Vec<String> = sqlx::query_scalar(
        "SELECT t.name FROM note_tags nt JOIN tags t ON t.id = nt.tag_id \
         WHERE nt.user_id = $1 ORDER BY t.name",
    )
    .bind(user.id)
    .fetch_all(&harness.pool)
    .await
    .expect("read note tags");
    assert!(tags.contains(&"Other".to_owned()), "{tags:?}");
    assert!(tags.contains(&"Metro: Lisbon".to_owned()), "{tags:?}");

    // A second tap is idempotent, not a second note.
    let (status, again) = harness.accept(&user, result.candidates[0].id).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        again.expect("repeat accept returns JSON").note_id,
        accepted.note_id
    );
    assert_eq!(
        harness
            .count(&user, "SELECT count(*) FROM notes WHERE user_id = $1")
            .await,
        1
    );
}
