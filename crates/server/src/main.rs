//! Strands server binary: axum router, health check, schema migrations on
//! boot (spec §6 — one deployable; spec §9.4 — Postgres is a hard runtime
//! dependency and migrations run on deploy).

use std::sync::Arc;

use strands_server::app::{AppState, router};
use strands_server::auth::identity::GoogleIdentityProvider;
use strands_server::config::AuthConfig;
use strands_server::db;

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

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("bind 0.0.0.0:8080");
    axum::serve(
        listener,
        router(AppState::new(pool, auth, Arc::new(identity))),
    )
    .await
    .expect("serve");
}
