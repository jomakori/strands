//! Strands server binary: axum router, health check, schema migrations on
//! boot (spec §6 — one deployable; spec §9.4 — Postgres is a hard runtime
//! dependency and migrations run on deploy).

use std::sync::Arc;

use strands_connector_photos::GooglePicker;
use strands_server::app::{AppState, router};
use strands_server::auth::IdentityProvider;
use strands_server::auth::identity::OidcIdentityProvider;
use strands_server::config::{AuthConfig, ConfigError};
use strands_server::connectors::{build_registry, dev_connectors_enabled};
use strands_server::db;
use strands_server::demo::{self, DemoIdentityProvider};
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
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info,whatsapp_rust=info".to_owned());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();

    // Auth config is resolved before the database is touched: with
    // `STRANDS_DEMO_MODE` set on a production-shaped deployment this is the
    // guard that refuses to start (ticket C3), and it has to fire before the
    // process opens a connection to real data.
    let auth = match AuthConfig::from_env() {
        Ok(auth) => auth,
        Err(error @ ConfigError::DemoInProduction { .. }) => {
            eprintln!("strands: refusing to start: {error}");
            std::process::exit(1);
        }
        Err(_) => panic!("OIDC_ISSUER and OIDC_CLIENT_ID must be set (see README)"),
    };
    let demo_requested = demo::requested(&|name: &str| std::env::var(name).ok());

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = db::connect_and_migrate(&database_url)
        .await
        .expect("connect to Postgres and apply migrations");

    // Staging demo mode: the account and its content are seeded once (every
    // insert is idempotent), and no OIDC provider is constructed at all.
    let demo_account = if demo_requested {
        let account = demo::seed(&pool).await.expect("seed the demo account");
        eprintln!(
            "strands: {} is set — every request resolves to the demo account {}; \
             OIDC is not used",
            demo::DEMO_ENV,
            account.user_id,
        );
        Some(account)
    } else {
        None
    };

    // Fail at boot, not on the first login: config errors and an unreachable
    // issuer are deployment mistakes, and the process should never appear
    // healthy without the settings its only auth flow needs. There is no client
    // secret to require — the app is a public Dex client (see README).
    let identity: Arc<dyn IdentityProvider> = match demo_account {
        Some(_) => Arc::new(DemoIdentityProvider::new()),
        None => {
            let provider = OidcIdentityProvider::discover(&auth)
                .await
                .unwrap_or_else(|error| panic!("discover OIDC endpoints: {error}"));
            eprintln!(
                "strands: OIDC issuer {} → authorize {}, token {}, userinfo {}",
                auth.issuer,
                provider.endpoints().authorization,
                provider.endpoints().token,
                provider.endpoints().userinfo,
            );
            Arc::new(provider)
        }
    };

    // Without the sealing key, stored connector tokens cannot be opened, so a
    // pick could never start. Resolve it before the registry so the Photos
    // connector's row and the pick routes share one key: with no key the row
    // reports itself unconfigured instead of the process dying on first boot
    // (which is exactly what the deployed pod logged).
    let secrets: Option<Arc<SecretKey>> = match SecretKey::from_env() {
        Some(Ok(key)) => Some(Arc::new(key)),
        Some(Err(error)) => {
            eprintln!("strands: STRANDS_SECRET_KEY is unusable ({error}); Google Photos is off");
            None
        }
        None => {
            eprintln!(
                "strands: STRANDS_SECRET_KEY is not set; stored connector tokens cannot be \
                 opened, so Google Photos will report itself unconfigured"
            );
            None
        }
    };

    // The dev flag is a switch, not a build feature: a release binary that is
    // asked for fake connectors still has to log that it is running them.
    let dev_connectors = dev_connectors_enabled();
    if dev_connectors {
        eprintln!(
            "strands: {} is set; registering the stub, the failing connector and the \
             placeholders — this is a development server",
            strands_server::connectors::DEV_CONNECTORS_ENV
        );
    }

    // The WhatsApp transport links a phone inside this process, so it is built
    // once here: the settings row and every `/api/v1/whatsapp` route share the
    // same session registry, and a deploy without a connector key gets none at
    // all (the row then reports the missing key instead of offering a dead link).
    let whatsapp_cipher = SessionCipher::from_env();
    let whatsapp_sessions = whatsapp_cipher
        .as_ref()
        .map(|_| Arc::new(WhatsAppRegistry::new(Arc::new(LiveFactory::from_env()))));
    if whatsapp_cipher.is_none() {
        eprintln!(
            "strands: STRANDS_SECRET_KEY is unset or not 32 base64 bytes; \
             the WhatsApp link stays unconfigured"
        );
    }
    let registry = build_registry(
        pool.clone(),
        secrets.clone(),
        whatsapp_sessions.clone(),
        dev_connectors,
    );

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
        identity,
        Arc::new(registry),
        Arc::new(summarizer),
    )
    .with_tagger(Arc::new(tagger))
    .with_judge(Arc::new(judge));

    // Staging demo mode: every request is the seeded account (ticket C3).
    let state = match demo_account {
        Some(account) => state.with_demo(account),
        None => state,
    };

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

    let state = match &secrets {
        Some(key) => state.with_secrets(Arc::clone(key)),
        None => state,
    };

    // WhatsApp rides on the same key: without it there is no cipher, hence no
    // stored session, hence no link — the row already said so at boot.
    let state = match (whatsapp_sessions, whatsapp_cipher) {
        (Some(sessions), Some(cipher)) => state.with_whatsapp(sessions, Arc::new(cipher)),
        _ => state,
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

    // Default is the deployed contract; `STRANDS_BIND_ADDR` exists so a test can
    // run the real binary next to whatever else is on 8080.
    let bind_addr =
        std::env::var("STRANDS_BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .unwrap_or_else(|error| panic!("bind {bind_addr}: {error}"));
    axum::serve(listener, router(state)).await.expect("serve");
}
