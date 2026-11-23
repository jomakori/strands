//! Staging demo mode (ticket C3, `plans/staging-and-connectors.md` §C3).
//!
//! Staging sits behind Cloudflare Access and has no identity provider, so the
//! server there must boot with **no** `OIDC_*` variables and still answer every
//! request as a real account: one fixed demo user, seeded with content shaped
//! like the design mockup so the deployed site shows the design instead of an
//! empty state.
//!
//! Three rules hold the feature together:
//!
//! * `STRANDS_DEMO_MODE=1` is the only configuration in which an OIDC variable
//!   may be absent ([`crate::config::AuthConfig::from_lookup`]). With the
//!   variable absent, production reads exactly the same variables and fails
//!   exactly the same way as before.
//! * Demo mode is refused outright when a production marker is present
//!   ([`refusal`]): the switch is for staging, and running it in production
//!   would hand every visitor one account's data.
//! * Seeding is idempotent by construction — every row has a deterministic id
//!   (or dedupe key) and every insert is `ON CONFLICT DO NOTHING`, so a restart
//!   re-uses the rows instead of duplicating them.
//!
//! The real auth path is untouched: [`crate::auth::extractor::CurrentUser`]
//! consults [`crate::app::AppState::demo`] only when the process was started in
//! demo mode, and there the OIDC provider is never constructed at all.

use chrono::{Duration, Utc};
use sqlx::PgPool;
use strands_core::{SessionId, UserId};
use uuid::Uuid;

use crate::auth::AuthError;
use crate::auth::identity::{BoxFuture, ConsentRequest, Identity, IdentityProvider};

/// The one switch that turns staging demo mode on.
pub const DEMO_ENV: &str = "STRANDS_DEMO_MODE";

/// The environment marker a deployment may set to declare itself production.
pub const PRODUCTION_ENV: &str = "STRANDS_ENV";

/// The only host demo mode is allowed to serve.
pub const STAGING_HOST: &str = "staging-strands.maklab.net";

/// Placeholder issuer used to keep [`crate::config::AuthConfig`] a complete
/// value in demo mode. It is never discovered and never dialled: demo mode does
/// not talk to an identity provider.
pub const DEMO_ISSUER: &str = "https://demo.invalid";

/// Placeholder public client id, same reasoning as [`DEMO_ISSUER`].
pub const DEMO_CLIENT_ID: &str = "strands-demo";

/// The account every request resolves to while demo mode is on.
pub const DEMO_SUBJECT: &str = "demo|staging";
pub const DEMO_EMAIL: &str = "demo@strands.test";
pub const DEMO_NAME: &str = "Demo account";

const KIND_USER: u8 = 1;
const KIND_SESSION: u8 = 2;
const KIND_CATEGORY: u8 = 3;
const KIND_TAG: u8 = 4;
const KIND_THREAD: u8 = 5;
const KIND_NOTE: u8 = 6;

/// How long the demo session cookie lasts. Long, because a staging login is not
/// supposed to expire mid-demo; it is refreshed on every boot.
const DEMO_SESSION_TTL_DAYS: i64 = 30;

/// Whether the process was asked to run in demo mode.
///
/// Accepts the shapes people actually put in a values file; anything else is a
/// "no", because the safe reading of an unparseable switch is "off".
pub fn requested(lookup: &impl Fn(&str) -> Option<String>) -> bool {
    lookup(DEMO_ENV)
        .map(|value| value.trim().to_ascii_lowercase())
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
}

/// Why demo mode must not start here, if it must not.
///
/// Two production markers are recognised, because either one alone has been
/// true of a real deployment: an explicit `STRANDS_ENV=production`, and a public
/// base URL that is not the staging host. The reason is returned as text so the
/// boot error can name it instead of saying "misconfigured".
pub fn refusal(lookup: &impl Fn(&str) -> Option<String>, public_base_url: &str) -> Option<String> {
    if let Some(marker) = lookup(PRODUCTION_ENV)
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        && matches!(marker.as_str(), "production" | "prod")
    {
        return Some(format!("{PRODUCTION_ENV}={marker}"));
    }

    let host = host_of(public_base_url);
    if host != STAGING_HOST {
        return Some(format!(
            "PUBLIC_BASE_URL points at {host}, not the staging host {STAGING_HOST}"
        ));
    }

    None
}

/// The host part of a URL, lowercased: scheme, user info, port and path dropped.
fn host_of(url: &str) -> String {
    let without_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = without_scheme.split('/').next().unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    host.split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// Deterministic ids: the same seed run always targets the same rows, which is
/// what makes a restart a no-op instead of a duplicate.
fn demo_id(kind: u8, index: u16) -> Uuid {
    let mut bytes = [0u8; 16];
    bytes[0] = 0xd3; // marks every row this mode owns
    bytes[1] = kind;
    bytes[14..16].copy_from_slice(&index.to_be_bytes());
    Uuid::from_bytes(bytes)
}

/// Identity the demo account is built from — also what the demo provider hands
/// back if anything ever asks for an authorization code.
pub fn identity() -> Identity {
    Identity {
        subject: DEMO_SUBJECT.to_owned(),
        email: DEMO_EMAIL.to_owned(),
        name: DEMO_NAME.to_owned(),
        avatar_url: None,
    }
}

/// The demo user and the session cookie value that resolves to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemoAccount {
    pub user_id: UserId,
    pub session_id: SessionId,
}

/// Create (or re-use) the demo account, its session and its content.
///
/// Safe to call on every boot: each statement is either an upsert keyed on a
/// deterministic id or an insert with `ON CONFLICT DO NOTHING`, so the row counts
/// after the second call are exactly the row counts after the first.
pub async fn seed(pool: &PgPool) -> Result<DemoAccount, sqlx::Error> {
    let user_id = demo_id(KIND_USER, 0);
    let session_id = demo_id(KIND_SESSION, 0);

    sqlx::query(
        "INSERT INTO users (id, google_sub, email, name, avatar_url) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (google_sub) DO UPDATE \
            SET email = EXCLUDED.email, \
                name = EXCLUDED.name, \
                avatar_url = EXCLUDED.avatar_url",
    )
    .bind(user_id)
    .bind(DEMO_SUBJECT)
    .bind(DEMO_EMAIL)
    .bind(DEMO_NAME)
    .bind(Option::<String>::None)
    .execute(pool)
    .await?;

    // The five starting categories, with deterministic ids: the real first-login
    // seed cannot be used here because it keys on "this user has none yet", which
    // two processes booting at once could both satisfy.
    for (index, (name, color, position)) in STARTING_CATEGORIES.iter().enumerate() {
        sqlx::query(
            "INSERT INTO categories (id, user_id, name, color, position) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(demo_id(KIND_CATEGORY, index as u16))
        .bind(user_id)
        .bind(*name)
        .bind(*color)
        .bind(*position)
        .execute(pool)
        .await?;
    }

    let mut note_index: u16 = 0;
    let now = Utc::now();

    for (topic_index, topic) in TOPICS.iter().enumerate() {
        let tag_id = demo_id(KIND_TAG, topic_index as u16);
        let category_id = STARTING_CATEGORIES
            .iter()
            .position(|(name, _, _)| *name == topic.category)
            .expect("every demo topic names a starting category");

        sqlx::query(
            "INSERT INTO tags (id, user_id, name, category_id) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(tag_id)
        .bind(user_id)
        .bind(topic.tag)
        .bind(demo_id(KIND_CATEGORY, category_id as u16))
        .execute(pool)
        .await?;

        for (thread_index, thread) in topic.threads.iter().enumerate() {
            let thread_id = demo_id(KIND_THREAD, (topic_index * 8 + thread_index) as u16);

            // History spreads backwards from "now": the first note of the first
            // thread is the newest, which is the order the mockup shows. The
            // thread carries its first note's timestamp, so it sorts with them.
            let updated_at = now - Duration::hours(6 + i64::from(note_index) * 11);

            sqlx::query(
                "INSERT INTO threads (id, user_id, tag_id, title, source_count, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(thread_id)
            .bind(user_id)
            .bind(tag_id)
            .bind(thread.title)
            .bind(thread.notes.len() as i32)
            .bind(updated_at)
            .execute(pool)
            .await?;

            for (kind, source, body) in thread.notes {
                let occurred_at = now - Duration::hours(6 + i64::from(note_index) * 11);

                sqlx::query(
                    "INSERT INTO notes \
                        (id, user_id, tag_id, thread_id, kind, body, occurred_at, source, dedupe_key) \
                     VALUES ($1, $2, $3, $4, $5::note_kind, $6, $7, $8::note_source, $9) \
                     ON CONFLICT (user_id, dedupe_key) DO NOTHING",
                )
                .bind(demo_id(KIND_NOTE, note_index))
                .bind(user_id)
                .bind(tag_id)
                .bind(thread_id)
                .bind(*kind)
                .bind(*body)
                .bind(occurred_at)
                .bind(*source)
                .bind(format!("demo:{topic_index}:{thread_index}:{note_index}"))
                .execute(pool)
                .await?;

                note_index += 1;
            }
        }
    }

    // One fixed session id, refreshed rather than re-inserted, so `/auth/google`
    // in a browser is stable across restarts.
    sqlx::query(
        "INSERT INTO sessions (id, user_id, expires_at) VALUES ($1, $2, $3) \
         ON CONFLICT (id) DO UPDATE SET user_id = EXCLUDED.user_id, \
                                        expires_at = EXCLUDED.expires_at",
    )
    .bind(session_id)
    .bind(user_id)
    .bind(now + Duration::days(DEMO_SESSION_TTL_DAYS))
    .execute(pool)
    .await?;

    Ok(DemoAccount {
        user_id,
        session_id,
    })
}

/// The identity provider used in demo mode: no discovery, no network, no code
/// exchange — it just answers with the demo identity.
pub struct DemoIdentityProvider {
    identity: Identity,
}

impl DemoIdentityProvider {
    pub fn new() -> Self {
        Self {
            identity: identity(),
        }
    }
}

impl Default for DemoIdentityProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl IdentityProvider for DemoIdentityProvider {
    fn consent_url(&self, _request: &ConsentRequest<'_>) -> String {
        // Never used: the login route short-circuits in demo mode. Returning the
        // site root keeps the contract honest without inventing a provider URL.
        "/".to_owned()
    }

    fn exchange_code<'a>(
        &'a self,
        _code: &'a str,
        _code_verifier: &'a str,
        _redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<Identity, AuthError>> {
        let identity = self.identity.clone();
        Box::pin(async move { Ok(identity) })
    }
}

/// The five starting categories (spec §6, names from the home-grid mockup).
const STARTING_CATEGORIES: [(&str, &str, i32); 5] = [
    ("Travel", "#2F6F4F", 0),
    ("Tech", "#5B4A8C", 1),
    ("Food", "#8C4A2F", 2),
    ("Reading", "#2F5D8C", 3),
    ("Ideas", "#7A6A2F", 4),
];

struct Topic {
    tag: &'static str,
    category: &'static str,
    threads: &'static [Thread],
}

struct Thread {
    title: &'static str,
    /// `(kind, source, body)` — the three columns the mockup's note rows show.
    notes: &'static [(&'static str, &'static str, &'static str)],
}

/// Content shaped like the design mockup: six strands across the five starting
/// categories, each with two threads of two notes, using every note source so the
/// source filter and the connector badges have something real to show.
const TOPICS: &[Topic] = &[
    Topic {
        tag: "Kyoto in November",
        category: "Travel",
        threads: &[
            Thread {
                title: "Temples before the crowds",
                notes: &[
                    (
                        "text",
                        "manual",
                        "Fushimi Inari at 06:40: the first hundred torii to myself, mist in the valley. \
                         Go before the first train and it is a different place.",
                    ),
                    (
                        "photo",
                        "google_photos",
                        "Torii gates stacked up the hillside, nobody in frame, wet stone underfoot.",
                    ),
                ],
            },
            Thread {
                title: "Where we ate",
                notes: &[
                    (
                        "visit",
                        "google_maps",
                        "Nishiki Market stall, seventh counter on the left: tamagoyaki still warm.",
                    ),
                    (
                        "text",
                        "whatsapp",
                        "Mum says book the ryokan with the cypress bath, not the one by the station.",
                    ),
                ],
            },
        ],
    },
    Topic {
        tag: "Rust on the server",
        category: "Tech",
        threads: &[
            Thread {
                title: "Axum extractors",
                notes: &[
                    (
                        "text",
                        "manual",
                        "FromRequestParts is the whole trick: if a handler cannot take CurrentUser, \
                         it cannot see a user id at all.",
                    ),
                    (
                        "text",
                        "manual",
                        "sqlx runtime-checked queries keep `cargo check` free of DATABASE_URL — worth \
                         the lost compile-time column checking.",
                    ),
                ],
            },
            Thread {
                title: "Deploys",
                notes: &[
                    (
                        "text",
                        "manual",
                        "Staging image is labelled with the PR number; the site falls back to a splash \
                         page when no label is present.",
                    ),
                    (
                        "photo",
                        "google_photos",
                        "Whiteboard sketch of the sync-wave order: secrets, database, app, ingress.",
                    ),
                ],
            },
        ],
    },
    Topic {
        tag: "Slow bread",
        category: "Food",
        threads: &[
            Thread {
                title: "Sunday loaf",
                notes: &[
                    (
                        "text",
                        "manual",
                        "75% hydration, 20% wholemeal, four folds in the first two hours. Oven at 250, \
                         lid on for 20, lid off for 25.",
                    ),
                    (
                        "photo",
                        "google_photos",
                        "Crumb shot: open, glossy, a little under on the base.",
                    ),
                ],
            },
            Thread {
                title: "Starter notes",
                notes: &[
                    (
                        "text",
                        "whatsapp",
                        "Fed at 8 and again at 8. Doubles in five hours on the counter, three in the fridge.",
                    ),
                    (
                        "text",
                        "manual",
                        "Rye starter smells of yoghurt when it is happy, of nail polish when it is hungry.",
                    ),
                ],
            },
        ],
    },
    Topic {
        tag: "Reading queue",
        category: "Reading",
        threads: &[
            Thread {
                title: "On the desk",
                notes: &[
                    (
                        "text",
                        "manual",
                        "The Timeless Way of Building — forty pages in, and the pattern language idea is \
                         already doing work on how I think about screens.",
                    ),
                    (
                        "text",
                        "manual",
                        "Piranesi, for the evenings. The tide is the plot.",
                    ),
                ],
            },
            Thread {
                title: "Finished",
                notes: &[
                    (
                        "text",
                        "manual",
                        "Station Eleven: the chapter that jumps twenty years is the whole book in miniature.",
                    ),
                    (
                        "text",
                        "manual",
                        "Notes to steal: keep the past tense for the ending only.",
                    ),
                ],
            },
        ],
    },
    Topic {
        tag: "Product ideas",
        category: "Ideas",
        threads: &[
            Thread {
                title: "Small bets",
                notes: &[
                    (
                        "text",
                        "manual",
                        "A weekly digest that is one screen: what changed, what stalled, what to drop. \
                         No charts, no streaks.",
                    ),
                    (
                        "text",
                        "manual",
                        "Photos need a picker, not a library: the user chooses per import what leaves \
                         their account.",
                    ),
                ],
            },
            Thread {
                title: "Things to refuse",
                notes: &[
                    (
                        "text",
                        "manual",
                        "No second inbox. Notes go to a strand or they go nowhere.",
                    ),
                    (
                        "text",
                        "whatsapp",
                        "Voice note to self: if a feature needs a tutorial, it is two features.",
                    ),
                ],
            },
        ],
    },
    Topic {
        tag: "Lisbon, maybe",
        category: "Travel",
        threads: &[
            Thread {
                title: "Spring plan",
                notes: &[
                    (
                        "text",
                        "manual",
                        "Late April, five days, stay in Alfama and walk everywhere. Tram 28 is a trap \
                         at nine in the morning.",
                    ),
                    (
                        "visit",
                        "google_maps",
                        "Time Out Market, back corner, the shortest queue.",
                    ),
                ],
            },
            Thread {
                title: "To check",
                notes: &[
                    (
                        "text",
                        "manual",
                        "Whether the ferry to Cacilhas runs late enough for dinner on the other side.",
                    ),
                    (
                        "photo",
                        "google_photos",
                        "Screenshot of a tiled façade, green and white.",
                    ),
                ],
            },
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuthConfig, ConfigError};
    use std::time::Duration as StdDuration;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    /// The staging boot this ticket exists for: demo mode, no OIDC variables at
    /// all, and a config that is complete enough to start.
    #[test]
    fn boots_without_any_oidc_variable() {
        let config = AuthConfig::from_lookup(lookup(&[
            (DEMO_ENV, "1"),
            ("PUBLIC_BASE_URL", "https://staging-strands.maklab.net"),
        ]))
        .expect("staging demo boot");

        assert_eq!(config.issuer, DEMO_ISSUER);
        assert_eq!(config.client_id, DEMO_CLIENT_ID);
        assert_eq!(config.public_base_url, "https://staging-strands.maklab.net");
    }

    /// Absence of the switch changes nothing: production still demands OIDC.
    #[test]
    fn without_the_switch_oidc_is_still_required() {
        let error = AuthConfig::from_lookup(lookup(&[(
            "PUBLIC_BASE_URL",
            "https://staging-strands.maklab.net",
        )]))
        .unwrap_err();

        assert!(
            matches!(error, ConfigError::Missing("OIDC_ISSUER")),
            "{error}"
        );
    }

    /// The guard: a production marker plus the demo switch is a startup error
    /// whose message names the marker, not just "misconfigured".
    #[test]
    fn refuses_to_start_in_production() {
        let error = AuthConfig::from_lookup(lookup(&[
            (DEMO_ENV, "1"),
            (PRODUCTION_ENV, "production"),
            ("PUBLIC_BASE_URL", "https://staging-strands.maklab.net"),
        ]))
        .unwrap_err();

        assert!(
            matches!(error, ConfigError::DemoInProduction { .. }),
            "{error}"
        );

        let message = error.to_string();
        assert!(message.contains(DEMO_ENV), "{message}");
        assert!(message.contains("STRANDS_ENV=production"), "{message}");

        // A production host with no marker at all is refused as well, and the
        // reason names the host it saw.
        let error = AuthConfig::from_lookup(lookup(&[
            (DEMO_ENV, "1"),
            ("PUBLIC_BASE_URL", "https://strands.maklab.net"),
        ]))
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("strands.maklab.net"), "{message}");
        assert!(message.contains(STAGING_HOST), "{message}");
    }

    /// Every note source and kind used by the seed is one the schema defines —
    /// a typo here would only show up as a 500 on a staging boot.
    #[test]
    fn demo_content_only_uses_schema_vocabulary() {
        const SOURCES: [&str; 4] = ["manual", "google_photos", "whatsapp", "google_maps"];
        const KINDS: [&str; 3] = ["text", "photo", "visit"];

        for topic in TOPICS {
            assert!(
                STARTING_CATEGORIES
                    .iter()
                    .any(|(name, _, _)| *name == topic.category),
                "{} names an unknown category",
                topic.tag
            );

            for thread in topic.threads {
                assert!(
                    !thread.notes.is_empty(),
                    "{} has an empty thread",
                    thread.title
                );
                for (kind, source, _) in thread.notes {
                    assert!(KINDS.contains(kind), "unknown kind {kind}");
                    assert!(SOURCES.contains(source), "unknown source {source}");
                }
            }
        }
    }

    /// Re-seeding leaves the same row counts (acceptance criterion of C3).
    #[tokio::test]
    async fn seed_is_idempotent() {
        let Some(pool) = pool_or_skip("demo::tests::seed_is_idempotent").await else {
            return;
        };

        let account = seed(&pool).await.expect("first seed");
        let first = counts(&pool, account.user_id).await;
        seed(&pool).await.expect("second seed");
        let second = counts(&pool, account.user_id).await;

        assert_eq!(first, second, "re-seeding must not add rows");
        assert_eq!(first.categories, 5);
        assert_eq!(first.tags, 6);
        assert_eq!(first.threads, 12);
        assert_eq!(first.notes, 24);
        assert_eq!(
            count(
                &pool,
                "SELECT count(*) FROM sessions WHERE user_id = $1",
                account.user_id
            )
            .await,
            1,
            "one fixed demo session, refreshed rather than duplicated"
        );
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Counts {
        categories: i64,
        tags: i64,
        threads: i64,
        notes: i64,
    }

    async fn counts(pool: &PgPool, user_id: UserId) -> Counts {
        Counts {
            categories: count(
                pool,
                "SELECT count(*) FROM categories WHERE user_id = $1",
                user_id,
            )
            .await,
            tags: count(
                pool,
                "SELECT count(*) FROM tags WHERE user_id = $1",
                user_id,
            )
            .await,
            threads: count(
                pool,
                "SELECT count(*) FROM threads WHERE user_id = $1",
                user_id,
            )
            .await,
            notes: count(
                pool,
                "SELECT count(*) FROM notes WHERE user_id = $1",
                user_id,
            )
            .await,
        }
    }

    async fn count(pool: &PgPool, sql: &str, user_id: UserId) -> i64 {
        sqlx::query_scalar(sql)
            .bind(user_id)
            .fetch_one(pool)
            .await
            .expect("count rows")
    }

    /// Same skip-or-fail contract as `tests/common`: a laptop without Postgres
    /// skips loudly, CI (`STRANDS_TEST_REQUIRE_DB=1`) fails instead of skipping.
    async fn pool_or_skip(test_name: &str) -> Option<PgPool> {
        use sqlx::postgres::PgPoolOptions;

        let url = std::env::var("DATABASE_URL")
            .ok()
            .filter(|url| !url.trim().is_empty());
        let Some(url) = url else {
            eprintln!(
                "skipping {test_name}: DATABASE_URL is not set (no live Postgres on this host)"
            );
            return None;
        };

        let required = std::env::var("STRANDS_TEST_REQUIRE_DB").is_ok_and(|value| value == "1");

        let pool = match PgPoolOptions::new()
            .acquire_timeout(StdDuration::from_secs(3))
            .connect(&url)
            .await
        {
            Ok(pool) => pool,
            Err(error) if required => {
                panic!("STRANDS_TEST_REQUIRE_DB is set but Postgres is unreachable: {error}")
            }
            Err(error) => {
                eprintln!("skipping {test_name}: cannot reach DATABASE_URL ({error})");
                return None;
            }
        };

        crate::db::MIGRATOR
            .run(&pool)
            .await
            .expect("apply migrations");
        Some(pool)
    }
}
