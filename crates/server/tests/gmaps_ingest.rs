//! T16 acceptance: Google Maps ingest, place lists, KML export.
//!
//! Everything here runs through the real router against a live Postgres when
//! `STRANDS_TEST_REQUIRE_DB=1` is set, with a fake place resolver standing in
//! for the Places API — the assertions are about what the importer wrote, not
//! about Google.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{Duration, Utc};
use http_body_util::BodyExt;
use quick_xml::Reader;
use quick_xml::events::Event;
use sqlx::PgPool;
use strands_api_types::{GmapsFormat, GmapsIngestResult, PlaceListSummary, PlaceSummary};
use strands_core::{Registry, UserId};
use strands_server::app::{AppState, router};
use strands_server::auth::identity::BoxFuture as IdentityFuture;
use strands_server::auth::{AuthError, ConsentRequest, Identity, IdentityProvider};
use strands_server::config::AuthConfig;
use strands_server::gmaps::places::{PlaceError, PlaceQuery, PlaceResolver, ResolvedPlace};
use strands_server::llm::BoxFuture;
use tower::ServiceExt;
use uuid::Uuid;

const BOUNDARY: &str = "----strands-t16-boundary";

/// Deterministic stand-in for the Places API: resolves anything with an
/// address, records how often it was asked, and never answers a blank address.
struct FakeGeocoder {
    calls: AtomicUsize,
}

impl FakeGeocoder {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl PlaceResolver for FakeGeocoder {
    fn resolve<'a>(
        &'a self,
        query: PlaceQuery<'a>,
    ) -> BoxFuture<'a, Result<Option<ResolvedPlace>, PlaceError>> {
        Box::pin(async move {
            if !query.is_resolvable() {
                // The importer must not even ask; a call here would be the
                // invent-a-coordinate bug this test exists to catch.
                return Ok(None);
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            let seed = query
                .name
                .bytes()
                .chain(query.address.bytes())
                .fold(0u64, |acc, byte| {
                    acc.wrapping_mul(31).wrapping_add(byte as u64)
                });
            Ok(Some(ResolvedPlace {
                name: query.name.to_owned(),
                address: Some(format!("{}, 94110", query.address.trim())),
                lat: 37.0 + (seed % 1000) as f64 / 100_000.0,
                lng: -122.0 - (seed % 700) as f64 / 100_000.0,
                place_id: Some(format!("fake-place-{seed}")),
                primary_type: Some("cafe".to_owned()),
                types: vec!["cafe".to_owned(), "food".to_owned()],
                metro: Some("San Francisco County".to_owned()),
                locality: Some("San Francisco".to_owned()),
                country: Some("United States".to_owned()),
            }))
        })
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
    geocoder: Arc<FakeGeocoder>,
}

struct TestUser {
    id: UserId,
    cookie: String,
}

impl Harness {
    async fn connect() -> Option<Self> {
        let pool = common::connect("gmaps_ingest").await?;
        let geocoder = Arc::new(FakeGeocoder::new());
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
        .with_geocoder(geocoder.clone());
        Some(Self {
            pool,
            app: router(state),
            geocoder,
        })
    }

    async fn user(&self) -> TestUser {
        let user_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, google_sub, email, name) VALUES ($1, $2, $3, 'Test')")
            .bind(user_id)
            .bind(format!("gmaps-test-subject-{user_id}"))
            .bind(format!("gmaps-test-{user_id}@example.com"))
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

    /// Upload one Takeout file the way the browser would.
    async fn upload(
        &self,
        user: &TestUser,
        file_name: &str,
        text: &str,
    ) -> (StatusCode, GmapsIngestResult) {
        let (status, bytes) = self.upload_raw(user, file_name, text).await;
        let result = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "decode ingest result ({status}): {error}: {}",
                String::from_utf8_lossy(&bytes)
            )
        });
        (status, result)
    }

    /// The same request, without assuming the response is JSON.
    async fn upload_raw(
        &self,
        user: &TestUser,
        file_name: &str,
        text: &str,
    ) -> (StatusCode, Vec<u8>) {
        let mut body = Vec::new();
        body.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; \
                 filename=\"{file_name}\"\r\nContent-Type: text/plain\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(text.as_bytes());
        body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/ingest/gmaps")
            .header(header::COOKIE, &user.cookie)
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .body(Body::from(body))
            .expect("build upload request");
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
            .expect("read upload response")
            .to_bytes();
        (status, bytes.to_vec())
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, user: &TestUser, uri: &str) -> T {
        let request = Request::builder()
            .uri(uri)
            .header(header::COOKIE, &user.cookie)
            .body(Body::empty())
            .expect("build GET request");
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .expect("router response");
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("read body")
            .to_bytes();
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!("decode {uri}: {error}: {}", String::from_utf8_lossy(&bytes))
        })
    }

    async fn count(&self, user: &TestUser, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .bind(user.id)
            .fetch_one(&self.pool)
            .await
            .expect("count rows")
    }
}

/// A 40-visit Timeline export in the 2024+ `semanticSegments` shape.
fn timeline_json(visits: usize) -> String {
    let segments: Vec<String> = (0..visits)
        .map(|index| {
            let hour = index % 24;
            let day = 1 + index / 24;
            let kind = if index % 2 == 0 { "Cafe" } else { "Museum" };
            format!(
                r#"{{"startTime":"2024-03-{day:02}T{hour:02}:00:00.000Z",
                    "endTime":"2024-03-{day:02}T{hour:02}:45:00.000Z",
                    "visit":{{"topCandidate":{{
                        "placeId":"ChIJvisit{index}",
                        "semanticType":"{kind}",
                        "placeLocation":{{"latLng":"35.{index:04},135.{index:04}"}}}}}}}}"#
            )
        })
        .collect();
    format!(r#"{{"semanticSegments":[{}]}}"#, segments.join(","))
}

/// Twelve saved places, four of them with a blank address — the shape that must
/// produce eight derivable rows and four honestly unresolved ones.
fn saved_places_csv() -> String {
    let mut csv = String::from("Title,Note,URL,Comment,Address\n");
    for index in 0..12 {
        let address = if index < 4 {
            String::new()
        } else {
            format!("{index} Valencia St, San Francisco, CA")
        };
        csv.push_str(&format!(
            "\"Saved place {index}\",\"note {index}\",\"https://maps.app.goo.gl/{index}\",\
             \"comment {index}\",\"{address}\"\n"
        ));
    }
    // Blank rows are padding Takeout sometimes leaves behind.
    csv.push_str(",,,,\n\n,,,,\n");
    csv
}

/// Forty visits become forty notes carrying a geo hook, a repeat upload adds
/// nothing, and the tag hint comes from the place type.
#[tokio::test]
async fn forty_visits_become_forty_notes_and_a_reupload_creates_no_duplicates() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let file = timeline_json(40);

    let (status, first) = harness.upload(&user, "Timeline.json", &file).await;
    assert_eq!(status, StatusCode::OK, "{first:?}");
    assert_eq!(first.format, GmapsFormat::Timeline);
    assert_eq!(first.visits_seen, 40);
    assert_eq!(first.visits_skipped, 0);
    assert_eq!(first.notes_created, 40);

    let notes = harness
        .count(
            &user,
            "SELECT count(*) FROM notes WHERE user_id = $1 AND kind = 'visit'",
        )
        .await;
    assert_eq!(notes, 40, "one note per visit");

    let with_geo = harness
        .count(
            &user,
            "SELECT count(*) FROM notes WHERE user_id = $1 AND body LIKE '%geo:35.%'",
        )
        .await;
    assert_eq!(with_geo, 40, "every visit note carries a geo hook");

    assert!(
        first.tags.iter().any(|tag| tag.name == "Cafe"),
        "the tag hint comes from the place type: {:?}",
        first.tags
    );

    let (status, second) = harness.upload(&user, "Timeline.json", &file).await;
    assert_eq!(status, StatusCode::OK, "{second:?}");
    assert_eq!(second.visits_seen, 40);
    assert_eq!(second.notes_created, 0, "a re-upload creates no notes");

    let notes_after = harness
        .count(
            &user,
            "SELECT count(*) FROM notes WHERE user_id = $1 AND kind = 'visit'",
        )
        .await;
    assert_eq!(notes_after, 40, "still forty visits, not eighty");
}

/// The older `locations[]` export imports the same way.
#[tokio::test]
async fn a_legacy_timeline_still_imports() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let file = r#"{"locations":[
        {"timestamp":"2023-05-02T18:30:00Z","name":"Katz's Delicatessen",
         "address":"205 E Houston St, New York","latitudeE7":407222000,
         "longitudeE7":-739874000,"placeId":"ChIJkatz"},
        {"timestamp":"2023-05-02T20:00:00Z","name":"Washington Square Park",
         "address":"Washington Square, New York","latitudeE7":407307000,
         "longitudeE7":-739977000}
    ]}"#;

    let (status, result) = harness.upload(&user, "Timeline.json", file).await;
    assert_eq!(status, StatusCode::OK, "{result:?}");
    assert_eq!(result.format, GmapsFormat::Timeline);
    assert_eq!(result.notes_created, 2);

    let body: String = sqlx::query_scalar(
        "SELECT body FROM notes WHERE user_id = $1 AND body LIKE '%Katz%' LIMIT 1",
    )
    .bind(user.id)
    .fetch_one(&harness.pool)
    .await
    .expect("legacy visit note");
    assert!(body.starts_with("Visited Katz's Delicatessen"), "{body}");
    assert!(body.contains("geo:40.72220,-73.98740"), "{body}");
}

/// A saved-places CSV becomes one list plus its rows; blank addresses stay
/// unresolved, are marked, and are never sent to the geocoder.
#[tokio::test]
async fn twelve_saved_places_become_a_list_with_four_honestly_unresolved() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let file = saved_places_csv();

    let (status, first) = harness.upload(&user, "Saved Places.csv", &file).await;
    assert_eq!(status, StatusCode::OK, "{first:?}");
    assert_eq!(first.format, GmapsFormat::SavedPlacesCsv);
    assert_eq!(first.lists_created, 1);
    assert_eq!(
        first.places_seen, 12,
        "blank rows are dropped, real rows kept"
    );
    assert_eq!(first.places_created, 12);
    assert_eq!(first.places_unresolved, 4);
    assert_eq!(
        harness.geocoder.calls(),
        8,
        "only rows with an address are looked up"
    );

    let resolved = harness
        .count(
            &user,
            "SELECT count(*) FROM places WHERE user_id = $1 AND enrichment = 'resolved' \
             AND lat IS NOT NULL AND lng IS NOT NULL",
        )
        .await;
    assert_eq!(resolved, 8, "at least three of the addressed rows resolve");
    assert!(resolved >= 3, "acceptance floor");

    let unresolved = harness
        .count(
            &user,
            "SELECT count(*) FROM places WHERE user_id = $1 AND enrichment = 'unresolved' \
             AND address IS NULL",
        )
        .await;
    assert_eq!(unresolved, 4, "blank-address rows stay, marked unresolved");

    let lists: Vec<PlaceListSummary> = harness.get_json(&user, "/api/v1/lists").await;
    assert_eq!(lists.len(), 1, "one list per file");
    assert_eq!(lists[0].name, "Saved Places");
    assert_eq!(lists[0].place_count, 12);

    let places: Vec<PlaceSummary> = harness
        .get_json(&user, &format!("/api/v1/lists/{}/places", lists[0].id))
        .await;
    assert_eq!(places.len(), 12);
    assert_eq!(
        places
            .iter()
            .filter(|place| place.enrichment.as_str() == "unresolved")
            .count(),
        4
    );

    let (status, second) = harness.upload(&user, "Saved Places.csv", &file).await;
    assert_eq!(status, StatusCode::OK, "{second:?}");
    assert_eq!(second.lists_created, 0, "the list is not duplicated");
    assert_eq!(second.places_created, 0, "the places are not duplicated");
    assert_eq!(
        harness
            .count(&user, "SELECT count(*) FROM places WHERE user_id = $1")
            .await,
        12
    );

    let lists: Vec<PlaceListSummary> = harness.get_json(&user, "/api/v1/lists").await;
    assert_eq!(lists.len(), 1, "still one list");
}

/// The KML download is a real XML document with one placemark per resolved
/// place and no placemark for the row that has no coordinates.
#[tokio::test]
async fn the_kml_export_parses_and_has_one_placemark_per_resolved_place() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let (status, _) = harness
        .upload(&user, "Saved Places.csv", &saved_places_csv())
        .await;
    assert_eq!(status, StatusCode::OK);

    let request = Request::builder()
        .uri("/api/v1/places/kml")
        .header(header::COOKIE, &user.cookie)
        .body(Body::empty())
        .expect("build KML request");
    let response = harness
        .app
        .clone()
        .oneshot(request)
        .await
        .expect("router response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/vnd.google-earth.kml+xml")
    );
    assert_eq!(
        response
            .headers()
            .get("x-strands-unresolved-places")
            .and_then(|value| value.to_str().ok()),
        Some("4")
    );

    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read KML")
        .to_bytes();
    let xml = String::from_utf8(bytes.to_vec()).expect("KML is UTF-8");

    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    let mut documents = 0usize;
    let mut placemarks = 0usize;
    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => match start.name().as_ref() {
                b"Document" => documents += 1,
                b"Placemark" => placemarks += 1,
                _ => {}
            },
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => panic!("KML is not well-formed XML: {error}"),
        }
    }
    assert_eq!(documents, 1, "one Document element");
    assert_eq!(placemarks, 8, "one Placemark per resolved place");
    assert!(
        xml.contains("4 places without coordinates excluded"),
        "the download says what it left out: {xml}"
    );
}

/// A user's lists are theirs: another account sees nothing.
#[tokio::test]
async fn another_users_lists_are_not_visible() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let owner = harness.user().await;
    let stranger = harness.user().await;
    harness
        .upload(&owner, "Saved Places.csv", &saved_places_csv())
        .await;

    let lists: Vec<PlaceListSummary> = harness.get_json(&stranger, "/api/v1/lists").await;
    assert!(lists.is_empty(), "no lists leak across users");

    let owner_lists: Vec<PlaceListSummary> = harness.get_json(&owner, "/api/v1/lists").await;
    let request = Request::builder()
        .uri(format!("/api/v1/lists/{}/places", owner_lists[0].id))
        .header(header::COOKIE, &stranger.cookie)
        .body(Body::empty())
        .expect("build cross-user request");
    let response = harness
        .app
        .clone()
        .oneshot(request)
        .await
        .expect("router response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// An upload that is neither Timeline nor saved places is refused, not guessed.
#[tokio::test]
async fn an_unrecognised_upload_is_a_bad_request() {
    let Some(harness) = Harness::connect().await else {
        return;
    };
    let user = harness.user().await;
    let (status, body) = harness
        .upload_raw(&user, "notes.txt", "just some prose with no places in it")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("Title"),
        "the refusal names what it was looking for: {body}"
    );
    assert_eq!(
        harness
            .count(&user, "SELECT count(*) FROM notes WHERE user_id = $1")
            .await,
        0,
        "a refused upload writes nothing"
    );
}
