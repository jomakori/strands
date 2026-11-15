//! Strands server binary: axum router, health check, schema migrations on
//! boot (spec §6 — one deployable; spec §9.4 — Postgres is a hard runtime
//! dependency and migrations run on deploy).

use std::sync::Arc;

use strands_core::Registry;
use strands_core::connector::{FailingConnector, PlaceholderConnector, StubConnector};
use strands_server::app::{AppState, router};
use strands_server::auth::identity::GoogleIdentityProvider;
use strands_server::config::AuthConfig;
use strands_server::db;
use strands_server::summary::LiteLlmSummarizer;

#[tokio::main]
async fn main() {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = db::connect_and_migrate(&database_url)
        .await
        .expect("connect to Postgres and apply migrations");

    // Fail at boot, not on the first login: a missing client id/secret is a
    // deployment mistake, and the process should never appear healthy without
    // the credentials its only auth flow needs.
    let auth = AuthConfig::from_env()
        .expect("GOOGLE_CLIENT_ID and GOOGLE_CLIENT_SECRET must be set (see README)");
    let identity =
        GoogleIdentityProvider::from_config(&auth).expect("build the Google identity provider");

    let registry = build_registry();

    // Same fail-at-boot rule as the identity provider: a missing LiteLLM
    // config should never pass health checks and fail only on first use.
    let summarizer =
        LiteLlmSummarizer::from_env().expect("LITELLM_BASE_URL and LITELLM_MASTER_KEY must be set");

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("bind 0.0.0.0:8080");
    axum::serve(
        listener,
        router(AppState::new(
            pool,
            auth,
            Arc::new(identity),
            Arc::new(registry),
            Arc::new(summarizer),
        )),
    )
    .await
    .expect("serve");
}

/// The process's connector set (T13): one working stub, one deliberately
/// failing connector, and a disabled placeholder per not-yet-built service.
fn build_registry() -> Registry {
    let mut registry = Registry::new();
    registry.register(StubConnector);
    registry.register(FailingConnector);
    registry.register(PlaceholderConnector::new(
        "gmail",
        "Gmail",
        &["gmail.readonly"],
    ));
    registry.register(PlaceholderConnector::new(
        "notion",
        "Notion",
        &["notion.read"],
    ));
    registry.register(PlaceholderConnector::new(
        "drive",
        "Drive",
        &["drive.readonly"],
    ));
    registry.register(PlaceholderConnector::new(
        "telegram",
        "Telegram",
        &["telegram.messages"],
    ));
    registry
}
