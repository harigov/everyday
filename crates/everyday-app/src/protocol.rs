//! The `everyday://` media protocol.
//!
//! Attachments are served over a custom scheme rather than inlined into the
//! document as `data:` URLs. That matters for more than tidiness:
//!
//! * A `data:` URL for a 400 MB video would be base64-encoded (+33%) into the
//!   entry's JSON, then re-encrypted and rewritten on every keystroke.
//! * `<video>` cannot seek a `data:` URL. Serving byte ranges is what makes
//!   scrubbing work, and it pairs with the chunked blob format so a seek
//!   decrypts two chunks instead of the whole file.
//!
//! The interesting decisions -- range parsing, response sizing, type sniffing
//! -- live in [`everyday_vault::media`] so they are testable without a
//! webview. This file is the adapter.

use everyday_core::BlobId;
use everyday_vault::media;
use tauri::http::{Request, Response, StatusCode, header};
use tauri::{Manager, Runtime};

use crate::state::AppState;

pub fn handle<R: Runtime>(
    app: &tauri::AppHandle<R>,
    request: Request<Vec<u8>>,
    responder: tauri::UriSchemeResponder,
) {
    let app = app.clone();
    // Reading media decrypts chunks off disk; keep it off the UI thread.
    tauri::async_runtime::spawn_blocking(move || {
        responder.respond(serve(&app, &request));
    });
}

fn error(status: StatusCode, message: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(message.as_bytes().to_vec())
        .expect("a static response is always well formed")
}

fn serve<R: Runtime>(app: &tauri::AppHandle<R>, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let Some(state) = app.try_state::<AppState>() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "application state is missing");
    };
    let Some(vault) = state.get() else {
        return error(StatusCode::NOT_FOUND, "no vault is open");
    };
    // A locked vault must not serve media. Without this the lock screen would
    // still leak every photo in the journal to anything that guessed a URL.
    if !vault.is_unlocked() {
        return error(StatusCode::FORBIDDEN, "vault is locked");
    }

    // everyday://localhost/<64 hex characters>
    let Ok(id) = BlobId::parse(request.uri().path().trim_start_matches('/')) else {
        return error(StatusCode::BAD_REQUEST, "not a blob address");
    };

    let total = match crate::commands::blob_len(&vault, id) {
        Ok(n) => n,
        Err(e) => {
            tracing::debug!(%id, error = %e, "media request for an unknown blob");
            return error(StatusCode::NOT_FOUND, "no such attachment");
        }
    };

    let plan = media::plan(
        total,
        request.headers().get(header::RANGE).and_then(|v| v.to_str().ok()),
    );

    let bytes = match crate::commands::read_blob_range(&vault, id, plan.start, plan.len) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(%id, error = %e, "could not read attachment");
            return error(StatusCode::INTERNAL_SERVER_ERROR, "could not read the attachment");
        }
    };

    let mut builder = Response::builder()
        .header(header::CONTENT_TYPE, media::sniff_mime(&bytes))
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        // Blobs are content-addressed, so the bytes behind a URL can never
        // change and the webview may cache them indefinitely.
        .header(header::CACHE_CONTROL, "private, max-age=31536000, immutable");

    builder = if plan.partial {
        builder
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, plan.content_range())
    } else {
        builder.status(StatusCode::OK)
    };

    builder
        .body(bytes)
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "could not build a response"))
}
