//! Strands server binary: axum router, health check, schema migrations on
//! boot (spec §6 — one deployable; spec §9.4 — Postgres is a hard runtime
//! dependency and migrations run on deploy).

use std::sync::Arc;

use strands_connector_photos::GooglePicker;
use strands_core::Registry;
use strands_core::connector::{FailingConnector, PlaceholderConnector, StubConnector};
use strands_server::app::{AppState, router};
use strands_server::auth::identity::GoogleIdentityProvider;
use strands_server::config::AuthConfig;
use strands_server::db;
use strands_server::gmaps::places::PlacesApiResolver;
use strands_server::links::LiteLlmJudge;
use strands_server::llm::LiteLlmClient;
use strands_server::secrets::SecretKey;
use strands_server::static_files::Assets;
use strands_server::summary::LiteLlmSummarizer;
use strands_server::tagging::LiteLlmTagger;
use strands_server::whatsapp::bridge::Registry as WhatsAppRegistry;
use strands_server::whatsapp::live::LiveFactory;
use strands_server::whatsapp::store::SessionCipher;

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
    // One client, one connection pool: summaries (T7), tagging (T9) and link
    // judging (T10) share it.
    let llm =
        LiteLlmClient::from_env().expect("LITELLM_BASE_URL and LITELLM_MASTER_KEY must be set");
    let summarizer = LiteLlmSummarizer::new(llm.clone());
    let tagger = LiteLlmTagger::new(llm.clone());
    let judge = LiteLlmJudge::new(llm);

    let state = AppState::new(
        pool,
        auth,
        Arc::new(identity),
        Arc::new(registry),
        Arc::new(summarizer),
    )
    .with_tagger(Arc::new(tagger))
    .with_judge(Arc::new(judge));

    // Place enrichment is optional where auth and the LLM are not: without a
    // key the import still works and marks every row unresolved, which is the
    // honest outcome. Say so once at boot instead of failing health checks.
    let state = match PlacesApiResolver::from_env() {
        Some(resolver) => state.with_geocoder(Arc::new(resolver)),
        None => {
            eprintln!(
                "strands: GOOGLE_MAPS_API_KEY is not set; imported places stay unresolved \
                 (they are imported, counted and reported, just not geocoded)"
            );
            state
        }
    };

    // The picker is the only photos path: there is no library listing, so the
    // client must be real here. It needs no credential of its own — the user's
    // stored token is passed per request.
    let state = state.with_photos(Arc::new(
        GooglePicker::new().expect("build the Google Photos Picker client"),
    ));

    // Without the sealing key, stored connector tokens cannot be opened, so a
    // pick could never start. Say so at boot rather than at the first tap.
    let state = match SecretKey::from_env() {
        Some(Ok(key)) => state.with_secrets(Arc::new(key)),
        Some(Err(error)) => {
            eprintln!("strands: STRANDS_SECRET_KEY is unusable ({error}); Google Photos is off");
            state
        }
        None => {
            eprintln!(
                "strands: STRANDS_SECRET_KEY is not set; stored connector tokens cannot be \
                 opened, so Google Photos picks will fail until it is"
            );
            state
        }
    };

    // WhatsApp is optional in the same way, but for a different reason: the
    // transport (whatsapp-rust) links a phone, so a deploy with no connector key
    // must not offer a link that can never be turned on. Without the key the
    // router keeps `UnconfiguredFactory`, and every endpoint says so plainly.
    let state = match SessionCipher::from_env() {
        Ok(cipher) => state.with_whatsapp(
            Arc::new(WhatsAppRegistry::new(Arc::new(LiveFactory::from_env()))),
            Arc::new(cipher),
        ),
        Err(_) => {
            eprintln!(
                "strands: STRANDS_SECRET_KEY is unset or not 32 base64 bytes; \
                 the WhatsApp link stays unconfigured"
            );
            state
        }
    };

    // The browser is served by this process (spec §6, one deployable): the
    // wasm bundle at the site root, the API under `/api`. A dev run without a
    // bundle still answers the API, which is worth having on its own.
    let state = match Assets::resolve() {
        Ok(Some(assets)) => {
            eprintln!(
                "strands: serving the web bundle from {}",
                assets.dir().display()
            );
            state.with_assets(Some(assets))
        }
        Ok(None) => {
            eprintln!(
                "strands: no web bundle found (STRANDS_ASSETS_DIR unset and ./dist missing); \
                 serving the API only"
            );
            state
        }
        // An explicitly configured bundle path that is not there is a
        // packaging mistake — say so at boot instead of 404ing the site.
        Err(error) => panic!("serve the web bundle: {error}"),
    };

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("bind 0.0.0.0:8080");
    axum::serve(listener, router(state)).await.expect("serve");
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
