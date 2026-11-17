//! Error type shared by every handler in the server (ticket T3).
//!
//! Handlers return [`AppError`]; it renders as JSON, matching T4's contract
//! that the API never answers with an HTML error page. Detail for `5xx` is
//! logged, never sent: an upstream failure message must not reach a browser.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::auth::AuthError;
use crate::llm::LlmError;
use crate::whatsapp::bridge::WhatsAppError;

#[derive(Debug)]
pub enum AppError {
    /// No cookie, an unknown session id, or an expired one. Also the answer for
    /// a malformed cookie — the client cannot tell those apart, which is the
    /// point.
    Unauthenticated,
    /// A request the server understood but refuses: bad OAuth state, a missing
    /// `code`, a user who denied consent.
    BadRequest(String),
    /// The row exists or does not exist; either way it is not this user's, so
    /// the answer is `404` and not `403` (spec acceptance: "direct object
    /// access across users gets 404").
    NotFound,
    /// A feature this build has no backend for: the WhatsApp link without
    /// `STRANDS_SECRET_KEY` configured, say.
    Unavailable(String),
    /// Our fault. Logged with its source, reported to the client as a bare 500.
    Internal {
        context: &'static str,
        source: sqlx::Error,
    },
}

impl AppError {
    pub fn internal(context: &'static str, source: sqlx::Error) -> Self {
        Self::Internal { context, source }
    }
}

impl From<sqlx::Error> for AppError {
    fn from(source: sqlx::Error) -> Self {
        Self::Internal {
            context: "database",
            source,
        }
    }
}

impl From<strands_core::ValidationError> for AppError {
    fn from(error: strands_core::ValidationError) -> Self {
        Self::BadRequest(error.to_string())
    }
}

impl From<LlmError> for AppError {
    fn from(error: LlmError) -> Self {
        eprintln!("strands: llm call failed: {error}");
        Self::Internal {
            context: "llm",
            source: sqlx::Error::Protocol(error.to_string()),
        }
    }
}

impl From<WhatsAppError> for AppError {
    fn from(error: WhatsAppError) -> Self {
        match error {
            // No link, no client, or WhatsApp refusing the link: each is a
            // condition the client or the operator can act on, so the answer
            // says "not available", not "server bug".
            WhatsAppError::NotLinked
            | WhatsAppError::NotConfigured
            | WhatsAppError::Rejected(_)
            | WhatsAppError::Transport(_) => Self::Unavailable(error.to_string()),
            // A stored session that no longer decrypts is our problem: the key
            // rotated or the row was tampered with.
            WhatsAppError::Session => Self::Internal {
                context: "whatsapp session",
                source: sqlx::Error::Protocol(error.to_string()),
            },
        }
    }
}

impl From<AuthError> for AppError {
    fn from(error: AuthError) -> Self {
        match error {
            // A rejected code, a userinfo answer we cannot trust, or a replayed
            // callback is the client's problem, not ours.
            AuthError::InvalidCode
            | AuthError::InvalidIdentity(_)
            | AuthError::UnknownState
            | AuthError::Denied(_) => Self::BadRequest(error.to_string()),
            // The provider is unreachable or misbehaving: our 500, and the
            // detail stays in the log.
            AuthError::Provider(_) => Self::Internal {
                context: "identity provider",
                source: sqlx::Error::Protocol(error.to_string()),
            },
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::Unauthenticated => (
                StatusCode::UNAUTHORIZED,
                "authentication required".to_owned(),
            ),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::NotFound => (StatusCode::NOT_FOUND, "not found".to_owned()),
            Self::Unavailable(message) => (StatusCode::SERVICE_UNAVAILABLE, message),
            Self::Internal { context, source } => {
                eprintln!("strands: internal error during {context}: {source}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_owned(),
                )
            }
        };

        (status, Json(json!({ "error": message }))).into_response()
    }
}
