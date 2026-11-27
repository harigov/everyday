//! `POST /v1/health/hae/{source}`: the door the phone's Health Auto Export
//! automations push through.
//!
//! Served on every transport, web access included -- web access is the one
//! listener a phone can reach with a certificate it already trusts (the
//! tailnet's), and its gatekeeper has already checked that the peer is a
//! device of the same tailnet user. Unlike every other route there, it is
//! authenticated by a bearer token, not the browser session: a token that
//! holds [`Scope::HealthIngest`](jatayu_service::ctx::Scope::HealthIngest)
//! (or, from the owner's own copies, `All`), minted by the shell's
//! `health_push_setup` for one source. The body is read only after the
//! token is accepted, up to [`MAX_BODY_BYTES`], and handed to the
//! `record_health_push` command with the automation headers beside it.
//!
//! 401 without a token the registry knows, 403 without the scope, 400 for a
//! body that is not a push, 413 past the limit, 409 `locked` while the vault
//! is locked (Health Auto Export logs it; its next "Previous 7 Days" run
//! brings the data again).
//!
//! # A token is for one source
//!
//! This route only decides that the token may push at all. That it is *this
//! source's* token is `record_health_push`'s to check, against the registry
//! id the shell bound to the source when it minted the token -- which is why
//! the command is handed the very `Ctx` that `authenticate` built, whose caller
//! is `Caller::Device(<that id>)`, and never one made here.
//!
//! # Why a session is not enough, even in a browser's own tab
//!
//! The session cookie a signed-in browser carries is for the interface. The
//! phone sends none, and a page in some other tab of that browser must not
//! be able to write readings by posting here with the cookie it happens to
//! be sending along. So whatever the gatekeeper found -- the two headers it
//! sets after a cookie checks out -- is never looked at: the route wants the
//! `Authorization` header, and nothing else says who is pushing.
//!
//! # A token minted for MCP is not a phone
//!
//! The same exception [`crate::screentime`] makes. A token issued for an MCP
//! client is `All` unless its owner narrows it, for somebody else's model;
//! letting that pose as the phone would let a coding agent write made-up
//! readings into somebody's health record. It reaches this route only if it
//! names `healthIngest` itself.
//!
//! # What is never logged
//!
//! Nothing in here writes the body, a header value or a source's name to a
//! log: what a push carries is somebody's body.

use crate::Server;
use crate::auth::{Leniency, Registry, bearer};
use crate::routes::Transport;
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use jatayu_core::health::PushHeaders;
use jatayu_core::id::HealthSourceId;
use jatayu_service::error::codes;
use jatayu_service::{CommandError, CommandResult, Ctx, Scope};
use serde_json::{Value, json};
use std::sync::Arc;

/// The most one push may be. Health Auto Export with "Batch Requests" on
/// sends far less; a week of raw heart rate without it is a few megabytes.
pub const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// The longest an automation header is kept. The phone's own are a sentence
/// at most; this stops a stranger's from being stored by the page.
const MAX_HEADER_CHARS: usize = 200;

pub(crate) async fn push(
    State((server, _transport)): State<(Arc<Server>, Transport)>,
    Path(source): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    match accept(&server, &source, &headers, body).await {
        Ok(report) => Json(report).into_response(),
        Err(e) => (status_of(&e.code), Json(e)).into_response(),
    }
}

/// Check who is pushing and what to, read the body, and hand it to the
/// vault. The order is the contract: nothing is read from the body until the
/// token has checked out.
async fn accept(
    server: &Arc<Server>,
    source: &str,
    headers: &HeaderMap,
    body: Body,
) -> CommandResult<Value> {
    let ctx = authenticate(&server.registry, headers)?;
    let source: HealthSourceId = source.trim().parse().map_err(|_| {
        CommandError::new(
            codes::INVALID,
            "that address names no Health source; set the phone up again",
        )
    })?;
    let bytes = crate::routes::read_body(body, MAX_BODY_BYTES).await?;
    // Off the async threads: a week of raw samples is tens of megabytes of
    // JSON, which is a good fraction of a second to parse.
    let body = jatayu_service::blocking(move || {
        let value: Value = serde_json::from_slice(&bytes).map_err(|e| {
            CommandError::new(codes::INVALID, format!("that is not a Health Auto Export push: {e}"))
        })?;
        if value.is_object() {
            Ok(value)
        } else {
            Err(CommandError::new(
                codes::INVALID,
                "that is not a Health Auto Export push: it is not a JSON object",
            ))
        }
    })
    .await?;
    let args = json!({
        "sourceId": source.to_string(),
        "headers": push_headers(headers),
        "body": body,
    });
    server.service.call(ctx, "record_health_push", args).await
}

/// Who is pushing: whoever the bearer token belongs to, if it may push.
/// `401` without a token the registry knows, `403` with one that may not --
/// see the module doc for why a token minted for an MCP client has to name
/// the scope.
fn authenticate(registry: &Arc<Registry>, headers: &HeaderMap) -> CommandResult<Ctx> {
    let token = bearer(headers, Leniency::AnyCase)
        .ok_or_else(|| CommandError::new("unauthorized", "this request carried no token"))?;
    registry.authenticate_for(
        token,
        Scope::HealthIngest,
        "this token may not push health data; set the phone up again in Settings → Health",
    )
}

/// What Health Auto Export says about the automation that sent this, each
/// capped and cleaned: they are shown in Settings.
fn push_headers(headers: &HeaderMap) -> PushHeaders {
    PushHeaders {
        automation_id: header_text(headers, "automation-id"),
        automation_name: header_text(headers, "automation-name"),
        automation_period: header_text(headers, "automation-period"),
        automation_aggregation: header_text(headers, "automation-aggregation"),
        session_id: header_text(headers, "session-id"),
    }
}

/// One header's text, or empty: lossy on bytes that are not UTF-8 (an
/// automation named in another language), without control or invisible
/// characters, and at most [`MAX_HEADER_CHARS`] long.
fn header_text(headers: &HeaderMap, name: &str) -> String {
    let Some(value) = headers.get(name) else { return String::new() };
    String::from_utf8_lossy(value.as_bytes())
        .chars()
        .filter(|c| !c.is_control() && !jatayu_core::money::text::is_invisible(*c))
        .take(MAX_HEADER_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

/// `routes.rs`'s status for a code, except the two that mean "not now": a
/// phone that is told 422 reads it as a request that is wrong, and a
/// vault that is locked is not one. See [`crate::screentime`] for the same
/// choice.
fn status_of(code: &str) -> StatusCode {
    match code {
        codes::LOCKED | codes::NO_VAULT => StatusCode::CONFLICT,
        other => crate::routes::status_for(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn with(name: &'static str, value: &[u8]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_bytes(value).unwrap());
        headers
    }

    #[test]
    fn a_bearer_token_is_read_whatever_the_case_of_its_scheme() {
        for (given, want) in [
            ("Bearer abc", Some("abc")),
            ("bearer abc", Some("abc")),
            ("BEARER  abc ", Some("abc")),
            ("Bearer ", None),
            ("Bearer", None),
            ("Basic abc", None),
            ("abc", None),
        ] {
            let headers = with("authorization", given.as_bytes());
            assert_eq!(bearer(&headers, Leniency::AnyCase), want, "{given:?}");
        }
        assert_eq!(bearer(&HeaderMap::new(), Leniency::AnyCase), None);
    }

    #[test]
    fn automation_headers_are_capped_and_cleaned() {
        let mut headers = HeaderMap::new();
        headers.insert("automation-id", HeaderValue::from_static("  3F2A  "));
        // Not UTF-8 on the wire, and with an override character in it.
        headers.insert(
            "automation-name",
            HeaderValue::from_bytes("Hourly \u{202E}\u{e9}t\u{e9}".as_bytes()).unwrap(),
        );
        headers.insert(
            "automation-period",
            HeaderValue::from_str(&"x".repeat(MAX_HEADER_CHARS + 50)).unwrap(),
        );
        let read = push_headers(&headers);
        assert_eq!(read.automation_id, "3F2A");
        assert_eq!(read.automation_name, "Hourly \u{e9}t\u{e9}");
        assert_eq!(read.automation_period.chars().count(), MAX_HEADER_CHARS);
        assert_eq!(read.automation_aggregation, "", "absent is empty, not an error");
        assert_eq!(read.session_id, "");
    }

    #[test]
    fn a_locked_vault_is_a_conflict_and_the_rest_keep_their_usual_status() {
        assert_eq!(status_of(codes::LOCKED), StatusCode::CONFLICT);
        assert_eq!(status_of(codes::NO_VAULT), StatusCode::CONFLICT);
        assert_eq!(status_of("unauthorized"), StatusCode::UNAUTHORIZED);
        assert_eq!(status_of(codes::FORBIDDEN), StatusCode::FORBIDDEN);
        assert_eq!(status_of(codes::INVALID), StatusCode::BAD_REQUEST);
        assert_eq!(status_of(codes::TOO_LARGE), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
