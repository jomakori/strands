//! Strands server binary: axum router, health check, schema migrations on
//! boot (spec §6 — one deployable; spec §9.4 — Postgres is a hard runtime
//! dependency and migrations run on deploy).

use axum::{Json, Router, routing::get};
use serde_json::{Value, json};
use strands_server::db;

/// Build the router. Split out so tests can exercise it without binding a socket.
fn app() -> Router {
    Router::new().route("/healthz", get(healthz))
}

/// Liveness probe: always `{"status": "ok"}` once the process is up.
async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

#[tokio::main]
async fn main() {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    db::connect_and_migrate(&database_url)
        .await
        .expect("connect to Postgres and apply migrations");

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("bind 0.0.0.0:8080");
    axum::serve(listener, app()).await.expect("serve");
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_returns_status_ok() {
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/healthz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body, json!({ "status": "ok" }));
    }
}
