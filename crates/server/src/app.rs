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

use crate::api;
use crate::auth::IdentityProvider;
use crate::auth::routes::routes as auth_routes;
use crate::config::AuthConfig;

/// Everything a handler may reach.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub auth: AuthConfig,
    /// `dyn` so production can use Google and tests a fake, without generics
    /// leaking into every handler signature.
    pub identity: Arc<dyn IdentityProvider>,
}

impl AppState {
    pub fn new(pool: PgPool, auth: AuthConfig, identity: Arc<dyn IdentityProvider>) -> Self {
        Self {
            pool,
            auth,
            identity,
        }
    }
}

/// Liveness probe: always `{"status": "ok"}` once the process is up.
pub async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .merge(auth_routes())
        .merge(api::routes())
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
            "/api/v1/tags/00000000-0000-0000-0000-000000000001",
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
