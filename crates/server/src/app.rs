//! Router construction (ticket T3).
//!
//! The application state is assembled here and nowhere else, so tests can build
//! the exact production router with a fake [`IdentityProvider`] and a database
//! pool of their choosing.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::routing::get;
use serde_json::{Value, json};
use sqlx::PgPool;
use strands_core::Registry;

use crate::account::routes as account_routes;
use crate::api;
use crate::auth::IdentityProvider;
use crate::auth::routes::routes as auth_routes;
use crate::config::AuthConfig;
use crate::connectors::routes as connector_routes;
use crate::gmaps::places::{PlaceResolver, UnconfiguredResolver};
use crate::gmaps::routes as gmaps_routes;
use crate::ingest::routes as ingest_routes;
use crate::links::handlers::routes as link_routes;
use crate::links::{Judge, UnconfiguredJudge};
use crate::photos::routes as photos_routes;
use crate::places::routes as places_routes;
use crate::search::routes as search_routes;
use crate::secrets::SecretKey;
use crate::static_files::{Assets, fallback as spa_fallback};
use crate::summary::Summarizer;
use crate::summary::handlers::routes as summary_routes;
use crate::tagging::{Tagger, UnconfiguredTagger};
use crate::whatsapp::bridge::{Registry as WhatsAppRegistry, UnconfiguredFactory};
use crate::whatsapp::routes as whatsapp_routes;
use crate::whatsapp::store::SessionCipher;
use strands_connector_photos::{PickerApi, UnconfiguredPicker};

/// Everything a handler may reach.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub auth: AuthConfig,
    /// `dyn` so production can use Google and tests a fake, without generics
    /// leaking into every handler signature.
    pub identity: Arc<dyn IdentityProvider>,
    /// Shared behind an `Arc`: one registry instance for the process, cloned
    /// into every handler's `AppState` for free (T13).
    pub registry: Arc<Registry>,
    /// `dyn` so production can use LiteLLM and tests a fake that never hits
    /// the network (ticket T7).
    pub summarizer: Arc<dyn Summarizer>,
    /// `dyn` for the same reason as `summarizer` (ticket T9). `AppState::new`
    /// installs a tagger that fails loudly, so an unconfigured deploy cannot
    /// silently skip tag assignment; production calls `with_tagger`.
    pub tagger: Arc<dyn Tagger>,
    /// Google Places lookups for imported saved places (ticket T16). `dyn` so
    /// tests use a fake and never touch the network; an unconfigured deploy gets
    /// [`UnconfiguredResolver`], which resolves nothing and marks every row
    /// unresolved rather than inventing coordinates.
    pub geocoder: Arc<dyn PlaceResolver>,
    /// `dyn` so production can use LiteLLM and tests a fake that never hits
    /// the network (ticket T10). Defaults to a judge that fails loudly, so a
    /// deploy without one cannot look successful with an empty constellation.
    pub judge: Arc<dyn Judge>,
    /// The Google Photos Picker API (ticket T14). `dyn` so tests script the
    /// pick and never touch the network; an unconfigured deploy gets
    /// [`UnconfiguredPicker`], which fails loudly instead of reporting a pick
    /// that never happened.
    pub photos: Arc<dyn PickerApi>,
    /// The key that opens `connector_secrets`; `None` means no connector token
    /// can be read, which the photos routes report as a misconfigured deploy.
    pub secrets: Option<Arc<SecretKey>>,
    /// Open WhatsApp sessions (ticket T15). Defaults to a registry whose factory
    /// refuses to open anything, so an unconfigured deploy answers 503 rather
    /// than acting as if a phone were paired.
    pub whatsapp: Arc<WhatsAppRegistry>,
    /// Key for `connector_secrets`; `None` until `with_whatsapp` installs one.
    pub whatsapp_cipher: Option<Arc<SessionCipher>>,
    /// The wasm bundle, when this process has one to serve. `None` in a dev
    /// run with no `dx build`, and in the router tests, which exercise the API
    /// surface only (ticket T20a).
    pub assets: Option<Assets>,
}

impl AppState {
    pub fn new(
        pool: PgPool,
        auth: AuthConfig,
        identity: Arc<dyn IdentityProvider>,
        registry: Arc<Registry>,
        summarizer: Arc<dyn Summarizer>,
    ) -> Self {
        Self {
            pool,
            auth,
            identity,
            registry,
            summarizer,
            tagger: Arc::new(UnconfiguredTagger),
            geocoder: Arc::new(UnconfiguredResolver),
            judge: Arc::new(UnconfiguredJudge),
            photos: Arc::new(UnconfiguredPicker),
            secrets: None,
            whatsapp: Arc::new(WhatsAppRegistry::new(Arc::new(UnconfiguredFactory))),
            whatsapp_cipher: None,
            assets: None,
        }
    }

    /// Install the bundle this process serves at the site root (ticket T20a).
    /// `None` is a deliberate state, not an error: the API stands on its own.
    pub fn with_assets(mut self, assets: Option<Assets>) -> Self {
        self.assets = assets;
        self
    }

    /// Install the production tagger (ticket T9).
    pub fn with_tagger(mut self, tagger: Arc<dyn Tagger>) -> Self {
        self.tagger = tagger;
        self
    }

    /// Install the place lookup used by Google Maps imports (ticket T16).
    pub fn with_geocoder(mut self, geocoder: Arc<dyn PlaceResolver>) -> Self {
        self.geocoder = geocoder;
        self
    }

    /// Install the production link judge (ticket T10).
    pub fn with_judge(mut self, judge: Arc<dyn Judge>) -> Self {
        self.judge = judge;
        self
    }

    /// Install the Google Photos Picker client (ticket T14). Production passes
    /// [`crate::photos::PickerHandle`]; tests pass one that scripts the pick.
    pub fn with_photos(mut self, photos: Arc<dyn PickerApi>) -> Self {
        self.photos = photos;
        self
    }

    /// Install the key that opens stored connector tokens. Without it the photos
    /// routes report a misconfigured deploy instead of importing nothing.
    pub fn with_secrets(mut self, key: Arc<SecretKey>) -> Self {
        self.secrets = Some(key);
        self
    }

    /// Install the WhatsApp session registry and secret key (ticket T15).
    pub fn with_whatsapp(
        mut self,
        registry: Arc<WhatsAppRegistry>,
        cipher: Arc<SessionCipher>,
    ) -> Self {
        self.whatsapp = registry;
        self.whatsapp_cipher = Some(cipher);
        self
    }
}

/// Liveness probe: `200` with the build identity, once the process is up.
///
/// Deliberately database-free: it answers what the process knows about itself,
/// so a probe distinguishes "serving" from "starting" without turning a
/// transient Postgres hiccup into a pod restart. Readiness for the API is the
/// API — a request that needs a row is the honest test of the pool.
pub async fn healthz() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": crate::build_info::VERSION,
        "commit": crate::build_info::COMMIT,
    }))
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .merge(account_routes())
        .merge(auth_routes())
        .merge(api::routes())
        .merge(connector_routes())
        .merge(gmaps_routes())
        .merge(ingest_routes())
        .merge(link_routes())
        .merge(photos_routes())
        .merge(places_routes())
        .merge(search_routes())
        .merge(summary_routes())
        .merge(whatsapp_routes())
        // Everything no route claimed is either a file from the bundle or the
        // SPA shell — never a blind 404 (ticket T20a). The fallback keeps
        // `/api` and `/auth` misses on the JSON error shape, so a client
        // parsing `{"error": …}` never meets an HTML body instead.
        .fallback(spa_fallback)
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    use crate::auth::identity::BoxFuture;
    use crate::auth::{AuthError, ConsentRequest, Identity};
    use crate::summary::{SummarizeError, Summarizer};

    struct NeverCalled;

    impl IdentityProvider for NeverCalled {
        fn consent_url(&self, _request: &ConsentRequest<'_>) -> String {
            "https://accounts.example/consent".to_owned()
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

    /// Never reached by these router-shape tests: no route here calls the LLM.
    struct NeverSummarized;

    impl Summarizer for NeverSummarized {
        fn model_alias(&self) -> &str {
            "unused"
        }

        fn summarize<'a>(
            &'a self,
            _prompt: &'a str,
        ) -> crate::summary::BoxFuture<'a, Result<String, SummarizeError>> {
            Box::pin(async { Err(SummarizeError::Request("never called".into())) })
        }
    }

    fn test_state() -> AppState {
        AppState::new(
            // Lazy pool: building a router must not require a live database.
            PgPool::connect_lazy("postgres://strands@localhost/strands").unwrap(),
            AuthConfig {
                client_id: "client-id".into(),
                client_secret: "client-secret".into(),
                public_base_url: "http://localhost:8080".into(),
                cookie_secure: false,
                session_ttl_days: 30,
                authorization_endpoint: "https://accounts.example/consent".into(),
                token_endpoint: "https://accounts.example/token".into(),
            },
            Arc::new(NeverCalled),
            Arc::new(strands_core::Registry::new()),
            Arc::new(NeverSummarized),
        )
    }

    /// A throwaway bundle on disk: the file serving is the thing under test,
    /// so it gets real files rather than a mock of a directory.
    struct Bundle {
        dir: std::path::PathBuf,
    }

    impl Bundle {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("strands-bundle-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("assets")).unwrap();
            std::fs::write(
                dir.join("index.html"),
                "<!doctype html><title>Strands shell</title>",
            )
            .unwrap();
            std::fs::write(
                dir.join("assets/index-9f8e7d6c5b4a.js"),
                "console.log('strands');",
            )
            .unwrap();
            std::fs::write(dir.join("assets/main.css"), "body{margin:0}").unwrap();
            Self { dir }
        }

        fn state(&self) -> AppState {
            test_state().with_assets(Some(Assets::new(&self.dir)))
        }
    }

    impl Drop for Bundle {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn get(state: AppState, uri: &str) -> axum::response::Response {
        router(state)
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    async fn body_of(response: axum::response::Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn cache_control(response: &axum::response::Response) -> &str {
        response
            .headers()
            .get(axum::http::header::CACHE_CONTROL)
            .expect("every served file carries a cache policy")
            .to_str()
            .unwrap()
    }

    // Split out of `main` so the health check can be tested without a database:
    // a router is cheap, a connection is not.
    #[tokio::test]
    async fn healthz_returns_status_ok() {
        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["status"], json!("ok"));
        // The probe identifies the build, so a rollout can be checked against
        // the commit it claims to be running.
        assert_eq!(body["version"], json!(env!("CARGO_PKG_VERSION")));
        assert!(
            body["commit"].as_str().is_some_and(|sha| !sha.is_empty()),
            "healthz names the commit it was built from: {body}"
        );
    }

    #[tokio::test]
    async fn api_routes_reject_anonymous_callers_before_touching_the_database() {
        // No cookie ⇒ 401 from the extractor, without a connection attempt.
        // (A query against the lazy pool would hang or fail instead.)
        for uri in [
            "/api/v1/me",
            "/api/v1/account/export",
            "/api/v1/tags/00000000-0000-0000-0000-000000000001",
            "/api/v1/search?q=anything",
        ] {
            let response = router(test_state())
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
        }
    }

    #[tokio::test]
    async fn logout_without_a_session_is_a_no_op_and_never_touches_the_database() {
        // No cookie means no `sessions` lookup, so this answers on the lazy pool.
        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/logout")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            response
                .headers()
                .get(axum::http::header::SET_COOKIE)
                .is_some(),
            "logout always clears the cookie"
        );
    }

    #[tokio::test]
    async fn unknown_paths_answer_a_json_404_not_html() {
        let response = get(test_state(), "/api/v1/nope").await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body, json!({ "error": "not found" }));
    }

    #[tokio::test]
    async fn the_site_root_is_the_shell_and_it_is_never_cached() {
        let bundle = Bundle::new("root");
        let response = get(bundle.state(), "/").await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "text/html");
        assert_eq!(cache_control(&response), "no-store");
        assert!(body_of(response).await.contains("Strands shell"));
    }

    #[tokio::test]
    async fn a_client_side_route_reloads_into_the_shell() {
        // The server has no route for `/tags/:id`: the client router owns it,
        // so a reload must still boot the app rather than 404.
        let bundle = Bundle::new("client-route");
        let response = get(bundle.state(), "/tags/00000000-0000-0000-0000-000000000001").await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(cache_control(&response), "no-store");
        assert!(body_of(response).await.contains("Strands shell"));
    }

    #[tokio::test]
    async fn hashed_assets_are_cached_forever_and_hand_named_ones_are_not() {
        let bundle = Bundle::new("cache");

        let hashed = get(bundle.state(), "/assets/index-9f8e7d6c5b4a.js").await;
        assert_eq!(hashed.status(), StatusCode::OK);
        assert_eq!(
            hashed.headers()["content-type"],
            "text/javascript",
            "the browser needs a script content type, not a file download"
        );
        assert_eq!(
            cache_control(&hashed),
            "public, max-age=31536000, immutable"
        );

        let css = get(bundle.state(), "/assets/main.css").await;
        assert_eq!(css.status(), StatusCode::OK);
        assert_eq!(css.headers()["content-type"], "text/css");
        assert_eq!(cache_control(&css), "public, max-age=3600");
    }

    #[tokio::test]
    async fn a_missing_asset_is_a_404_and_not_the_shell() {
        // Serving the shell here would hand the browser HTML under a `.js`
        // content type and fail somewhere far less legible.
        let bundle = Bundle::new("missing-asset");
        let response = get(bundle.state(), "/assets/gone-9f8e7d6c5b4a.js").await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn api_misses_stay_json_404_with_a_bundle_installed() {
        let bundle = Bundle::new("api-miss");
        let response = get(bundle.state(), "/api/v1/nope").await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()["content-type"], "application/json");
        let body: Value = serde_json::from_str(&body_of(response).await).unwrap();
        assert_eq!(body, json!({ "error": "not found" }));
    }

    #[tokio::test]
    async fn auth_misses_stay_json_404_with_a_bundle_installed() {
        let bundle = Bundle::new("auth-miss");
        let response = get(bundle.state(), "/auth/nope").await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()["content-type"], "application/json");
    }

    #[tokio::test]
    async fn a_write_to_a_client_route_is_not_a_successful_spa_load() {
        let bundle = Bundle::new("post");
        let response = router(bundle.state())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tags/1234")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn traversal_out_of_the_bundle_reads_nothing() {
        let bundle = Bundle::new("traversal");
        for uri in [
            "/../Cargo.toml",
            "/assets/../../Cargo.toml",
            "/%2e%2e/%2e%2e/Cargo.toml",
        ] {
            let response = get(bundle.state(), uri).await;
            assert_ne!(response.status(), StatusCode::OK, "{uri}");
            assert!(!body_of(response).await.contains("[workspace]"), "{uri}");
        }
    }

    /// The smoke run the image needs: a real listener, a real browser-side
    /// request, over TCP rather than a router call, so hyper and tower-http are
    /// exercised the way the container will exercise them.
    #[tokio::test]
    async fn a_real_socket_serves_the_bundle_and_the_api_together() {
        let bundle = Bundle::new("socket");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router(bundle.state())).await.unwrap();
        });

        let client = reqwest::Client::new();
        let base = format!("http://{address}");

        let root = client.get(format!("{base}/")).send().await.unwrap();
        assert_eq!(root.status(), 200);
        assert_eq!(root.headers()["cache-control"], "no-store");
        assert!(root.text().await.unwrap().contains("Strands shell"));

        let route = client
            .get(format!("{base}/tags/00000000-0000-0000-0000-000000000001"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            route.status(),
            200,
            "a reload of a client route boots the app"
        );

        let asset = client
            .get(format!("{base}/assets/index-9f8e7d6c5b4a.js"))
            .send()
            .await
            .unwrap();
        assert_eq!(asset.status(), 200);
        assert_eq!(
            asset.headers()["cache-control"],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(asset.headers()["content-type"], "text/javascript");

        let api_miss = client
            .get(format!("{base}/api/v1/nope"))
            .send()
            .await
            .unwrap();
        assert_eq!(api_miss.status(), 404);
        assert!(
            api_miss.headers()["content-type"]
                .to_str()
                .unwrap()
                .contains("json")
        );

        let health = client.get(format!("{base}/healthz")).send().await.unwrap();
        assert_eq!(health.status(), 200);
        let body: Value = health.json().await.unwrap();
        assert_eq!(body["status"], "ok");
    }

    #[tokio::test]
    async fn a_process_without_a_bundle_answers_404_rather_than_pretending() {
        let response = get(test_state(), "/").await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()["content-type"], "application/json");
    }
}
