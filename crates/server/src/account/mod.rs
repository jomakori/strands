//! Account and data-control endpoints (ticket T17).
//!
//! * `GET /api/v1/account/export` — the caller's whole dataset as one JSON
//!   document, with a `Content-Disposition` so the browser saves it as a file
//!   instead of rendering it.
//! * `DELETE /api/v1/account/data` — erase that dataset.
//!
//! Both learn who is calling only from [`CurrentUser`], so a request can reach
//! exactly one account's rows. There is no confirm token on the delete: the
//! explicit confirmation is the browser's job (the UI keeps a second click in
//! front of it), and a `DELETE` on this path is already the unambiguous verb.

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use chrono::Utc;

use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;

mod store;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/account/export", get(export))
        .route("/api/v1/account/data", delete(delete_data))
}

/// `GET /api/v1/account/export` — everything the caller owns, as one file.
pub async fn export(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Response, AppError> {
    let Some(export) = store::export(&state.pool, user.id).await? else {
        // The session outlived its user row; treat it like any other session
        // that names nobody.
        return Err(AppError::Unauthenticated);
    };

    let filename = format!("strands-export-{}.json", Utc::now().format("%Y-%m-%d"));

    Ok((
        [
            (
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/json; charset=utf-8"),
            ),
            (
                header::CONTENT_DISPOSITION,
                header::HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
                    .map_err(|_| AppError::BadRequest("unrepresentable export filename".into()))?,
            ),
        ],
        Json(export),
    )
        .into_response())
}

/// `DELETE /api/v1/account/data` — delete the caller's content, in one
/// transaction, and keep their account.
pub async fn delete_data(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<StatusCode, AppError> {
    store::delete_all(&state.pool, user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}
