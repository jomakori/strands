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
use crate::error::AppError;
use crate::gmaps::places::{PlaceResolver, UnconfiguredResolver};
use crate::gmaps::routes as gmaps_routes;
use crate::ingest::routes as ingest_routes;
use crate::links::handlers::routes as link_routes;
use crate::links::{Judge, UnconfiguredJudge};
use crate::photos::routes as photos_routes;
use crate::places::routes as places_routes;
use crate::search::routes as search_routes;
use crate::secrets::SecretKey;
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
        }
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

/// Liveness probe: always `{"status": "ok"}` once the process is up.
pub async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
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
        // Unknown paths answer the same JSON error shape as every handler, so a
        // client parsing `{"error": …}` never meets an HTML body instead.
        .fallback(not_found)
        .with_state(state)
}

/// Fallback for any path no route claimed: JSON 404, never an HTML error page.
async fn not_found() -> AppError {
    AppError::NotFound
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
        assert_eq!(body, json!({ "status": "ok" }));
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
        let response = router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body, json!({ "error": "not found" }));
    }
}
