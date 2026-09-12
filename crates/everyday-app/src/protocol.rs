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
//! -- live in [`everyday_core::media`] so they are testable without a
//! webview. This file is the adapter.

use everyday_core::BlobId;
use everyday_core::media;
use tauri::http::{Request, Response, StatusCode, header};
use tauri::{Manager, Runtime};

use crate::state::{AppState, SessionHandle};

pub fn handle<R: Runtime>(
    app: &tauri::AppHandle<R>,
    request: Request<Vec<u8>>,
    responder: tauri::UriSchemeResponder,
) {
    let app = app.clone();
    let session = app.try_state::<AppState>().map(|state| state.session());
    match session {
        // A vault on another machine. The bytes come over the same pinned
        // connection every command uses, with the `Range` passed through, so a
        // video seeks over a network exactly as it seeks off a disk -- and the
        // webview's content security policy is untouched, because the only
        // outbound connection is still one this process makes.
        Some(SessionHandle::Remote(remote)) => {
            tauri::async_runtime::spawn(async move {
                responder.respond(serve_remote(&remote, &request).await);
            });
        }
        _ => {
            // Reading media decrypts chunks off disk; keep it off the UI thread.
            tauri::async_runtime::spawn_blocking(move || {
                responder.respond(serve(&app, &request));
            });
        }
    }
}

fn error(status: StatusCode, message: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(message.as_bytes().to_vec())
        .expect("a static response is always well formed")
}

/// The same response, from a vault this process does not hold.
async fn serve_remote(
    remote: &std::sync::Arc<crate::remote::Remote>,
    request: &Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let path = request.uri().path().trim_start_matches('/').to_string();
    if BlobId::parse(&path).is_err() {
        return error(StatusCode::BAD_REQUEST, "not a blob address");
    }

    let total = match remote.client.blob_len(&path).await {
        Ok(n) => n,
        Err(e) => {
            tracing::debug!(id = %path, error = %e, "media request for an unknown blob");
            return error(StatusCode::NOT_FOUND, "no such attachment");
        }
    };
    // The same planner the local path uses, so a range means the same thing on
    // both -- including the cap that stops a webview asking for a whole film.
    let plan =
        media::plan(total, request.headers().get(header::RANGE).and_then(|v| v.to_str().ok()));

    let bytes = match remote.client.blob_range(&path, plan.start, plan.len).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(id = %path, error = %e, "could not read a remote attachment");
            return error(StatusCode::INTERNAL_SERVER_ERROR, "could not read the attachment");
        }
    };
    respond(plan, bytes)
}

fn serve<R: Runtime>(app: &tauri::AppHandle<R>, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let Some(state) = app.try_state::<AppState>() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "application state is missing");
    };
    let Some(vault) = state.service().get() else {
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

    let service = state.service();
    let total = match service.blob_len(id) {
        Ok(n) => n,
        Err(e) => {
            tracing::debug!(%id, error = %e, "media request for an unknown blob");
            return error(StatusCode::NOT_FOUND, "no such attachment");
        }
    };

    let plan =
        media::plan(total, request.headers().get(header::RANGE).and_then(|v| v.to_str().ok()));

    let bytes = match service.blob_range(id, plan.start, plan.len) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(%id, error = %e, "could not read attachment");
            return error(StatusCode::INTERNAL_SERVER_ERROR, "could not read the attachment");
        }
    };

    respond(plan, bytes)
}

/// The response both paths build, so a local vault and a remote one answer a
/// range request identically.
fn respond(plan: media::ResponsePlan, bytes: Vec<u8>) -> Response<Vec<u8>> {
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
