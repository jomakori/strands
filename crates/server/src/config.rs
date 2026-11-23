//! Runtime configuration read from the environment (spec §9.5: secrets come
//! from the environment / Doppler, never from the image and never from code).
//!
//! Since T20c the service is a **public** OIDC client of the cluster's Dex
//! broker (`https://dex.maklab.net`), not a Google client. Dex holds the one
//! Google client there is; we get a client id and nothing else, because a public
//! client proves possession with PKCE instead of with a secret. That is why
//! there is no client-secret variable to read, require, rotate or leak.

use std::env;

/// Where a local plain-HTTP run listens when `PUBLIC_BASE_URL` is unset.
pub const DEFAULT_PUBLIC_BASE_URL: &str = "http://localhost:8080";

/// Auth settings for the OIDC authorization-code flow and the session cookie
/// (ticket T3, spec §6 "Auth flow": 30-day cookie, `HttpOnly; Secure;
/// SameSite=Lax`).
#[derive(Clone, Debug)]
pub struct AuthConfig {
    /// Issuer URL, without a trailing slash — `https://dex.maklab.net`. The
    /// discovery document and the `iss` claim we accept both hang off it, so it
    /// must be the issuer's public host, byte for byte.
    pub issuer: String,
    /// Public client id registered at the issuer (`strands`). Not a secret: it
    /// travels in the browser's URL bar on every login.
    pub client_id: String,
    /// Public origin the browser reaches this service on, without a trailing
    /// slash. The OAuth `redirect_uri` is derived from it, so it must match the
    /// URI registered at Dex byte for byte.
    pub public_base_url: String,
    /// `Secure` on the session cookie. Defaults to `true`; only a local
    /// plain-HTTP dev run sets `SESSION_COOKIE_SECURE=false`.
    pub cookie_secure: bool,
    /// Session lifetime in days (spec §6: 30d).
    pub session_ttl_days: i64,
}

/// Missing or malformed configuration, reported at boot.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing required environment variable {0}")]
    Missing(&'static str),
    #[error("environment variable {name} is not a valid value: {value}")]
    Invalid { name: &'static str, value: String },
    /// `STRANDS_DEMO_MODE` was requested while a production marker is present
    /// (ticket C3). See [`crate::demo::refusal`] for what counts as one.
    #[error(
        "{demo_env} is set, but this process is configured as production: {reason}. \
         Demo mode answers every request as one fixed account and seeds sample content into it, \
         so it must never run against production data — unset {demo_env} or run it on staging",
        demo_env = crate::demo::DEMO_ENV
    )]
    DemoInProduction { reason: String },
}

impl AuthConfig {
    /// Read `OIDC_ISSUER` and `OIDC_CLIENT_ID` (required), `PUBLIC_BASE_URL`,
    /// `SESSION_COOKIE_SECURE` and `SESSION_TTL_DAYS` (optional).
    ///
    /// Nothing else is required: a public client has no secret, so a deploy that
    /// has no secret is a complete deploy.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| env::var(name).ok())
    }

    /// The presence rules, split out of [`Self::from_env`] so tests can exercise
    /// them by handing in a lookup rather than mutating the process environment
    /// (`std::env::set_var` is `unsafe` in edition 2024, and a test that sets it
    /// races every other test in the binary).
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let public_base_url = optional(&lookup, "PUBLIC_BASE_URL")
            .unwrap_or_else(|| DEFAULT_PUBLIC_BASE_URL.to_owned());
        let public_base_url = public_base_url.trim_end_matches('/').to_owned();

        // Staging demo mode (ticket C3) is the one configuration in which the
        // OIDC variables may be absent; it is also refused wherever this process
        // looks like production, so the switch cannot widen staging's reach.
        let demo = crate::demo::requested(&lookup);
        if demo && let Some(reason) = crate::demo::refusal(&lookup, &public_base_url) {
            return Err(ConfigError::DemoInProduction { reason });
        }

        let issuer = match optional(&lookup, "OIDC_ISSUER") {
            Some(issuer) => issuer.trim_end_matches('/').to_owned(),
            None if demo => crate::demo::DEMO_ISSUER.to_owned(),
            None => return Err(ConfigError::Missing("OIDC_ISSUER")),
        };

        // Discovery is a URL join, so a scheme-less host would turn into a
        // request for `dex.example/...` that fails only at the first login.
        // Reject it at boot, where the message is useful.
        if !(issuer.starts_with("https://") || issuer.starts_with("http://")) {
            return Err(ConfigError::Invalid {
                name: "OIDC_ISSUER",
                value: issuer,
            });
        }

        let client_id = match optional(&lookup, "OIDC_CLIENT_ID") {
            Some(client_id) => client_id,
            None if demo => crate::demo::DEMO_CLIENT_ID.to_owned(),
            None => return Err(ConfigError::Missing("OIDC_CLIENT_ID")),
        };

        let cookie_secure = match optional(&lookup, "SESSION_COOKIE_SECURE") {
            Some(value) => parse_bool("SESSION_COOKIE_SECURE", &value)?,
            None => true,
        };

        let session_ttl_days = match optional(&lookup, "SESSION_TTL_DAYS") {
            Some(value) => match value.parse::<i64>().ok().filter(|days| *days > 0) {
                Some(days) => days,
                None => {
                    return Err(ConfigError::Invalid {
                        name: "SESSION_TTL_DAYS",
                        value,
                    });
                }
            },
            None => 30,
        };

        Ok(Self {
            issuer,
            client_id,
            public_base_url,
            cookie_secure,
            session_ttl_days,
        })
    }

    /// The `redirect_uri` sent on the consent redirect and again on the code
    /// exchange. Both must be identical, which is why the value the browser was
    /// redirected with is persisted in `oauth_requests`.
    pub fn redirect_uri(&self) -> String {
        format!("{}/auth/google/callback", self.public_base_url)
    }

    /// Where the endpoint list comes from (OIDC Discovery 1.0 §4).
    pub fn discovery_url(&self) -> String {
        format!("{}/.well-known/openid-configuration", self.issuer)
    }
}

fn optional(lookup: &impl Fn(&str) -> Option<String>, name: &str) -> Option<String> {
    lookup(name)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn parse_bool(name: &'static str, value: &str) -> Result<bool, ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Ok(true),
        "0" | "false" | "no" => Ok(false),
        _ => Err(ConfigError::Invalid {
            name,
            value: value.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lookup over a fixed set of pairs — the point of `from_lookup`.
    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    fn complete() -> impl Fn(&str) -> Option<String> {
        lookup(&[
            ("OIDC_ISSUER", "https://dex.example"),
            ("OIDC_CLIENT_ID", "strands"),
            ("PUBLIC_BASE_URL", "https://strands.example"),
        ])
    }

    #[test]
    fn redirect_uri_is_derived_from_the_public_base_url() {
        assert_eq!(
            AuthConfig::from_lookup(complete()).unwrap().redirect_uri(),
            "https://strands.example/auth/google/callback"
        );
    }

    #[test]
    fn discovery_url_hangs_off_the_issuer() {
        let config = AuthConfig::from_lookup(complete()).unwrap();
        assert_eq!(
            config.discovery_url(),
            "https://dex.example/.well-known/openid-configuration"
        );
    }

    #[test]
    fn a_trailing_slash_on_the_issuer_is_not_doubled() {
        let config = AuthConfig::from_lookup(lookup(&[
            ("OIDC_ISSUER", "https://dex.example/"),
            ("OIDC_CLIENT_ID", "strands"),
        ]))
        .unwrap();
        assert_eq!(config.issuer, "https://dex.example");
        assert_eq!(
            config.discovery_url(),
            "https://dex.example/.well-known/openid-configuration"
        );
    }

    /// The ticket's own case: no issuer is a boot failure whose message names the
    /// variable rather than the symptom.
    #[test]
    fn a_missing_issuer_is_a_missing_variable_error() {
        let error = AuthConfig::from_lookup(lookup(&[("OIDC_CLIENT_ID", "strands")])).unwrap_err();
        assert!(
            matches!(error, ConfigError::Missing("OIDC_ISSUER")),
            "{error}"
        );
        assert!(error.to_string().contains("OIDC_ISSUER"), "{error}");
    }

    #[test]
    fn a_missing_client_id_is_a_missing_variable_error() {
        let error =
            AuthConfig::from_lookup(lookup(&[("OIDC_ISSUER", "https://dex.example")])).unwrap_err();
        assert!(
            matches!(error, ConfigError::Missing("OIDC_CLIENT_ID")),
            "{error}"
        );
    }

    /// A blank value is absent, not "the empty issuer".
    #[test]
    fn a_blank_issuer_counts_as_missing() {
        let error = AuthConfig::from_lookup(lookup(&[
            ("OIDC_ISSUER", "   "),
            ("OIDC_CLIENT_ID", "strands"),
        ]))
        .unwrap_err();
        assert!(
            matches!(error, ConfigError::Missing("OIDC_ISSUER")),
            "{error}"
        );
    }

    #[test]
    fn a_scheme_less_issuer_is_refused() {
        let error = AuthConfig::from_lookup(lookup(&[
            ("OIDC_ISSUER", "dex.example"),
            ("OIDC_CLIENT_ID", "strands"),
        ]))
        .unwrap_err();
        assert!(
            matches!(
                error,
                ConfigError::Invalid {
                    name: "OIDC_ISSUER",
                    ..
                }
            ),
            "{error}"
        );
    }

    /// Regression guard for the ticket: nothing secret-shaped is read, so a
    /// config that carries no secret at all still builds.
    #[test]
    fn no_client_secret_is_required() {
        let config = AuthConfig::from_lookup(complete()).unwrap();
        assert_eq!(config.client_id, "strands");
        assert_eq!(config.public_base_url, "https://strands.example");
        assert!(config.cookie_secure, "secure by default");
        assert_eq!(config.session_ttl_days, 30);
    }

    #[test]
    fn bool_parsing_rejects_garbage() {
        assert!(parse_bool("X", "true").unwrap());
        assert!(!parse_bool("X", "FALSE").unwrap());
        assert!(parse_bool("X", "sometimes").is_err());
    }
}
