//! The WhatsApp link routes: pair, review chats, allowlist, import, sync, unlink.
//!
//! The order of the endpoints is the ticket's order of events. Pairing yields a
//! QR code the client renders; `GET /api/v1/whatsapp` keeps returning the chat
//! list until the user has picked a subset; only `import` writes notes, and it
//! writes for allowlisted chats alone.

pub mod bridge;
pub mod live;
pub mod qr;
pub mod store;

use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use strands_api_types::{
    WHATSAPP_DISCLOSURE, WhatsAppAllowlist, WhatsAppChat, WhatsAppImportReport, WhatsAppLinkState,
    WhatsAppStatus,
};
use strands_core::UserId;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use bridge::{AllowList, BufferedMessage, Jid, LinkState, WhatsAppSession, select};
use store::{SessionCipher, StoredChat};

/// Mount the link under the versioned API.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/whatsapp", get(status))
        .route("/api/v1/whatsapp/qr.svg", get(qr_image))
        .route("/api/v1/whatsapp/pair", post(pair))
        .route("/api/v1/whatsapp/allowlist", put(set_allowlist))
        .route("/api/v1/whatsapp/import", post(import))
        .route("/api/v1/whatsapp/sync", post(sync_now))
        .route("/api/v1/whatsapp/disconnect", post(disconnect))
}

async fn status(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<WhatsAppStatus>, AppError> {
    Ok(Json(build_status(&state, user.id).await?))
}

/// The live pairing payload as an SVG the phone can scan.
///
/// Served as its own response rather than a field on [`WhatsAppStatus`]: the QR
/// changes every time the bridge refreshes it, and an `<img>` that only reloads
/// when the payload changes keeps the status poll cheap.
async fn qr_image(State(state): State<AppState>, user: CurrentUser) -> Result<Response, AppError> {
    let payload = state
        .whatsapp
        .session(user.id)
        .and_then(|session| match session.state() {
            LinkState::AwaitingScan { qr: Some(code), .. } => Some(code.value),
            _ => None,
        })
        .ok_or_else(|| {
            AppError::BadRequest("no pairing code is live: press Connect to start one".to_owned())
        })?;
    let svg = qr::svg(&payload).ok_or_else(|| {
        AppError::Unavailable("the pairing code did not encode as a QR".to_owned())
    })?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            // A stale QR is worse than none: it silently stops working.
            (header::CACHE_CONTROL, "no-store"),
        ],
        svg,
    )
        .into_response())
}

/// Start pairing, or report the one already running.
async fn pair(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<WhatsAppStatus>, AppError> {
    let cipher = cipher(&state)?;
    let row = store::connector(&state.pool, user.id).await?;

    if state.whatsapp.session(user.id).is_none() {
        // A parked session means the phone is still linked: reopen it instead of
        // asking the user to scan again.
        let parked = store::load_session(&state.pool, row.id, cipher).await?;
        if let Err(error) = state.whatsapp.connect(user.id, parked).await {
            store::set_status(&state.pool, row.id, "error", Some(&error.to_string())).await?;
            return Err(error.into());
        }
    }
    Ok(Json(build_status(&state, user.id).await?))
}

/// Replace the JID allowlist; an empty list is valid and imports nothing.
async fn set_allowlist(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<WhatsAppAllowlist>,
) -> Result<Json<WhatsAppStatus>, AppError> {
    let allowlist = AllowList::parse(body.jids.iter().map(String::as_str))
        .map_err(|error| AppError::BadRequest(format!("not a WhatsApp JID: {error}")))?;
    let row = store::connector(&state.pool, user.id).await?;
    store::save_allowlist(&state.pool, row.id, &allowlist).await?;
    Ok(Json(build_status(&state, user.id).await?))
}

/// Import the allowlisted backlog.
async fn import(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<WhatsAppImportReport>, AppError> {
    let session = linked_session(&state, user.id)?;
    let row = store::connector(&state.pool, user.id).await?;
    let report = drain_into_notes(&state, user.id, row.id, &row.allowlist, &session).await?;
    Ok(Json(report))
}

/// Ask the phone for anything new, then import it.
async fn sync_now(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<WhatsAppImportReport>, AppError> {
    let session = linked_session(&state, user.id)?;
    let row = store::connector(&state.pool, user.id).await?;

    if let Err(error) = session.sync().await {
        store::set_status(&state.pool, row.id, "error", Some(&error.to_string())).await?;
        return Err(error.into());
    }
    store::save_chats(&state.pool, user.id, &session.chats()).await?;
    let report = drain_into_notes(&state, user.id, row.id, &row.allowlist, &session).await?;
    Ok(Json(report))
}

/// Unlink the device on the phone and forget the session here.
async fn disconnect(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<WhatsAppStatus>, AppError> {
    let row = store::connector(&state.pool, user.id).await?;
    if let Some(session) = state.whatsapp.session(user.id) {
        // Unlinking is the one operation that must reach the phone: if it fails,
        // the session stays in place and the client is told, rather than
        // pretending the device is gone.
        session.unlink().await?;
        state.whatsapp.forget(user.id);
    }
    store::clear_session(&state.pool, row.id).await?;
    store::set_status(&state.pool, row.id, "disconnected", None).await?;
    Ok(Json(build_status(&state, user.id).await?))
}

/// Select, write, then consume: a failed write leaves the buffer intact.
async fn drain_into_notes(
    state: &AppState,
    user_id: Uuid,
    connector_id: Uuid,
    allowlist: &AllowList,
    session: &std::sync::Arc<dyn WhatsAppSession>,
) -> Result<WhatsAppImportReport, AppError> {
    let buffered = session.peek();
    let selection = select(buffered, allowlist);
    if selection.is_empty() {
        return Ok(WhatsAppImportReport {
            notes_created: 0,
            chats_imported: 0,
            chats_skipped: selection.skipped_chats,
        });
    }

    let stored = store::chats(&state.pool, user_id).await?;
    let category_id = current_category(&state.pool, user_id).await?;
    let batches: Vec<(StoredChat, Vec<BufferedMessage>)> = selection
        .allowed
        .into_iter()
        .map(|(jid, messages)| {
            let chat = stored
                .iter()
                .find(|chat| chat.jid == jid)
                .cloned()
                .unwrap_or(StoredChat {
                    display_name: None,
                    is_group: jid.is_group(),
                    imported_through: None,
                    jid,
                });
            (chat, messages)
        })
        .collect();
    let chats_imported = batches.len();

    let notes_created = store::write_messages(&state.pool, user_id, category_id, &batches).await?;
    session.drain();
    store::set_status(&state.pool, connector_id, "connected", None).await?;

    Ok(WhatsAppImportReport {
        notes_created,
        chats_imported,
        chats_skipped: selection.skipped_chats,
    })
}

async fn build_status(state: &AppState, user_id: Uuid) -> Result<WhatsAppStatus, AppError> {
    let row = store::connector(&state.pool, user_id).await?;
    let session = state.whatsapp.session(user_id);

    let state_snapshot = session
        .as_ref()
        .map(|session| session.state())
        .unwrap_or(LinkState::Unlinked);

    // The chat list comes from the live session once it has one, because that is
    // the list the client must render before any import. A parked link falls
    // back to the stored snapshot so the list survives a restart.
    let live = session
        .as_ref()
        .map(|session| session.chats())
        .unwrap_or_default();
    let stored = store::chats(&state.pool, user_id).await?;
    // Per-chat backlog size, so the client can show what is waiting without
    // pulling the messages themselves.
    let buffered = session
        .as_ref()
        .map(|session| session.peek())
        .unwrap_or_default();
    let waiting = |jid: &Jid| {
        buffered
            .iter()
            .filter(|message| &message.jid == jid)
            .count()
    };
    let chats: Vec<WhatsAppChat> = if live.is_empty() {
        stored
            .iter()
            .map(|chat| WhatsAppChat {
                jid: chat.jid.to_string(),
                display_name: chat.display_name.clone(),
                is_group: chat.is_group,
                allowed: row.allowlist.permits(&chat.jid),
                buffered_messages: waiting(&chat.jid),
            })
            .collect()
    } else {
        live.iter()
            .map(|chat| WhatsAppChat {
                jid: chat.jid.to_string(),
                display_name: chat.display_name.clone(),
                is_group: chat.is_group,
                allowed: row.allowlist.permits(&chat.jid),
                buffered_messages: waiting(&chat.jid),
            })
            .collect()
    };

    let mut last_error = row.last_error.clone();
    // A stored "connected" with no live session means the process that held the
    // device link is gone: a restart drops the in-memory transport, and the
    // phone is still linked upstream. Repair the row once and tell the user what
    // to do, rather than showing a green link that can never receive anything.
    if session.is_none() && row.status == "connected" {
        let message = "the WhatsApp bridge is not holding a session (a restart drops the \
                       device link): press Connect and scan again";
        store::set_status(&state.pool, row.id, "needs_sign_in", Some(message)).await?;
        last_error = Some(message.to_owned());
    }

    Ok(WhatsAppStatus {
        state: link_state(state_snapshot),
        disclosure: WHATSAPP_DISCLOSURE.to_owned(),
        allowlist: row.allowlist.jids().iter().map(Jid::to_string).collect(),
        chats,
        last_sync_at: row.last_sync_at,
        last_error,
    })
}

fn link_state(state: LinkState) -> WhatsAppLinkState {
    match state {
        LinkState::Unlinked => WhatsAppLinkState::Unlinked,
        LinkState::AwaitingScan { qr, pair_code } => WhatsAppLinkState::AwaitingScan {
            // The countdown belongs to the QR code: the pair code lives for the
            // same window, so reading either from the QR keeps one owner.
            expires_in_seconds: qr.as_ref().map_or(0, |code| code.expires_in_seconds),
            qr: qr.map(|code| code.value),
            pair_code: pair_code.map(|code| code.value),
        },
        LinkState::Linked => WhatsAppLinkState::Linked,
    }
}

fn cipher(state: &AppState) -> Result<&SessionCipher, AppError> {
    state.whatsapp_cipher.as_deref().ok_or_else(|| {
        AppError::Unavailable(
            "WhatsApp is not configured on this server (STRANDS_SECRET_KEY is unset)".to_owned(),
        )
    })
}

fn linked_session(
    state: &AppState,
    user_id: UserId,
) -> Result<std::sync::Arc<dyn WhatsAppSession>, AppError> {
    state.whatsapp.session(user_id).ok_or_else(|| {
        AppError::BadRequest("no WhatsApp link is open: pair a phone first".to_owned())
    })
}

/// The tag/thread tree every import hangs off; the oldest category is the default.
async fn current_category(pool: &sqlx::PgPool, user_id: Uuid) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM categories WHERE user_id = $1 ORDER BY position, name LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::BadRequest("create a category before importing".to_owned()))
}
