//! Runtime configuration read from the environment (spec §9.5: secrets come
//! from the environment / Doppler, never from the image and never from code).

use std::env;

/// Default Google consent screen endpoint.
pub const DEFAULT_AUTHORIZATION_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// Default Google token endpoint.
pub const DEFAULT_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

/// Auth settings for the Google authorization-code flow and the session cookie
/// (ticket T3, spec §6 "Auth flow": 30-day cookie, `HttpOnly; Secure;
/// SameSite=Lax`).
#[derive(Clone, Debug)]
pub struct AuthConfig {
    pub client_id: String,
    pub client_secret: String,
    /// Public origin the browser reaches this service on, without a trailing
    /// slash. The OAuth `redirect_uri` is derived from it, so it must match the
    /// URI registered in the Google Cloud console byte for byte.
    pub public_base_url: String,
    /// `Secure` on the session cookie. Defaults to `true`; only a local
    /// plain-HTTP dev run sets `SESSION_COOKIE_SECURE=false`.
    pub cookie_secure: bool,
    /// Session lifetime in days (spec §6: 30d).
    pub session_ttl_days: i64,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
}

/// Missing or malformed configuration, reported at boot.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing required environment variable {0}")]
    Missing(&'static str),
    #[error("environment variable {name} is not a valid value: {value}")]
    Invalid { name: &'static str, value: String },
}

impl AuthConfig {
    /// Read `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET` (required),
    /// `PUBLIC_BASE_URL`, `SESSION_COOKIE_SECURE`, `SESSION_TTL_DAYS`,
    /// `GOOGLE_AUTHORIZATION_ENDPOINT`, `GOOGLE_TOKEN_ENDPOINT` (optional).
    pub fn from_env() -> Result<Self, ConfigError> {
        let client_id = required("GOOGLE_CLIENT_ID")?;
        let client_secret = required("GOOGLE_CLIENT_SECRET")?;

        let public_base_url = env::var("PUBLIC_BASE_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "http://localhost:8080".to_owned());
        let public_base_url = public_base_url.trim_end_matches('/').to_owned();

        let cookie_secure = match env::var("SESSION_COOKIE_SECURE") {
            Ok(value) => parse_bool("SESSION_COOKIE_SECURE", &value)?,
            Err(_) => true,
        };

        let session_ttl_days = match env::var("SESSION_TTL_DAYS") {
            Ok(value) => match value.parse::<i64>().ok().filter(|days| *days > 0) {
                Some(days) => days,
                None => {
                    return Err(ConfigError::Invalid {
                        name: "SESSION_TTL_DAYS",
                        value,
                    });
                }
            },
            Err(_) => 30,
        };

        Ok(Self {
            client_id,
            client_secret,
            public_base_url,
            cookie_secure,
            session_ttl_days,
            authorization_endpoint: optional("GOOGLE_AUTHORIZATION_ENDPOINT")
                .unwrap_or_else(|| DEFAULT_AUTHORIZATION_ENDPOINT.to_owned()),
            token_endpoint: optional("GOOGLE_TOKEN_ENDPOINT")
                .unwrap_or_else(|| DEFAULT_TOKEN_ENDPOINT.to_owned()),
        })
    }

    /// The `redirect_uri` sent on the consent redirect and again on the code
    /// exchange. Both must be identical, which is why the value the browser was
    /// redirected with is persisted in `oauth_requests`.
    pub fn redirect_uri(&self) -> String {
        format!("{}/auth/google/callback", self.public_base_url)
    }
}

fn required(name: &'static str) -> Result<String, ConfigError> {
    optional(name).ok_or(ConfigError::Missing(name))
}

fn optional(name: &'static str) -> Option<String> {
    env::var(name)
        .ok()
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

    #[test]
    fn redirect_uri_is_derived_from_the_public_base_url() {
        let config = AuthConfig {
            client_id: "id".into(),
            client_secret: "secret".into(),
            public_base_url: "https://strands.example".into(),
            cookie_secure: true,
            session_ttl_days: 30,
            authorization_endpoint: DEFAULT_AUTHORIZATION_ENDPOINT.into(),
            token_endpoint: DEFAULT_TOKEN_ENDPOINT.into(),
        };
        assert_eq!(
            config.redirect_uri(),
            "https://strands.example/auth/google/callback"
        );
    }

    #[test]
    fn bool_parsing_rejects_garbage() {
        assert!(parse_bool("X", "true").unwrap());
        assert!(!parse_bool("X", "FALSE").unwrap());
        assert!(parse_bool("X", "sometimes").is_err());
    }
}
