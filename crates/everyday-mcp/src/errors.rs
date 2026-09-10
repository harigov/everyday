//! The JSON-RPC error envelopes this server can send, in one place.
//!
//! MCP reserves `-32020`..`-32099` for itself on top of the standard
//! JSON-RPC codes, and a couple of these are built both inside
//! [`handle`](crate::handle) and by the HTTP binding that sits in front of
//! it — the binding validates headers against [`expected_headers`], which
//! this crate has no way to see for itself, and needs to answer a mismatch
//! the same way `handle` answers everything else. Building the response
//! shape here rather than in `everyday-server` means there is exactly one
//! place that knows what a `-32020` looks like on the wire, and the HTTP
//! layer never has to reconstruct a JSON-RPC error object by hand.

use serde_json::{Value, json};

use crate::Outcome;

pub(crate) const INVALID_REQUEST: i64 = -32600;
pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
pub(crate) const INVALID_PARAMS: i64 = -32602;
pub(crate) const INTERNAL_ERROR: i64 = -32603;
pub(crate) const HEADER_MISMATCH: i64 = -32020;
pub(crate) const MISSING_CLIENT_CAPABILITY: i64 = -32021;
pub(crate) const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// Build the `Outcome::Reply` for a JSON-RPC error, with the HTTP status
/// the specification pairs it with.
pub(crate) fn error_reply(
    id: &Value,
    code: i64,
    message: &str,
    data: Option<Value>,
    status: u16,
) -> Outcome {
    let mut error = json!({ "code": code, "message": message });
    if let Some(data) = data {
        error.as_object_mut().expect("object literal above").insert("data".into(), data);
    }
    Outcome::Reply { status, body: json!({ "jsonrpc": "2.0", "id": id.clone(), "error": error }) }
}

/// `-32600 Invalid Request`: the envelope itself does not parse as
/// JSON-RPC — no `method`, `jsonrpc` missing or wrong, or an explicit
/// `null` id (as opposed to no `id` at all, which is a notification and
/// not an error). HTTP `400`.
pub fn invalid_request(id: &Value, detail: &str) -> Outcome {
    error_reply(id, INVALID_REQUEST, "Invalid Request", Some(json!({ "detail": detail })), 400)
}

/// `-32601 Method not found`. The specification requires HTTP `404` here,
/// not `400` or `200` — the one JSON-RPC error this codebase's ordinary
/// `/v1` mapping would not otherwise produce for "unknown method", so it
/// is worth a comment rather than trusting it to be obvious at the call
/// site.
pub fn method_not_found(id: &Value, method: &str) -> Outcome {
    error_reply(id, METHOD_NOT_FOUND, "Method not found", Some(json!({ "method": method })), 404)
}

/// `-32602 Invalid params`, for a modern request missing required `_meta`
/// fields, or for a `tools/call` naming a tool nobody has heard of. HTTP
/// `400`. The latter is a *protocol* error rather than a tool-execution
/// one — see [`crate::handle`]'s module doc for why that distinction is
/// the one thing in this crate worth getting right.
pub fn invalid_params(id: &Value, detail: &str) -> Outcome {
    error_reply(id, INVALID_PARAMS, "Invalid params", Some(json!({ "detail": detail })), 400)
}

/// `-32603 Internal error`, for a failure that is the host's rather than
/// the caller's — `Host::tools` failing outright, say, as opposed to a
/// single tool call failing. HTTP `500`.
pub fn internal_error(id: &Value, detail: &str) -> Outcome {
    error_reply(id, INTERNAL_ERROR, "Internal error", Some(json!({ "detail": detail })), 500)
}

/// `-32020 HeaderMismatch`: a `POST /mcp` header does not match the body
/// it was sent with, or a required header is missing or malformed. HTTP
/// `400`. Built here so the HTTP binding, which is the only thing that can
/// see the real headers to compare, never has to hand-assemble a JSON-RPC
/// error object of its own.
pub fn header_mismatch(id: &Value, detail: &str) -> Outcome {
    error_reply(id, HEADER_MISMATCH, "Header mismatch", Some(json!({ "detail": detail })), 400)
}

/// `-32021 MissingRequiredClientCapability`: `data.requiredCapabilities`
/// names what was needed. Nothing this server does today requires a
/// specific client capability — we neither sample nor elicit — so nothing
/// in [`crate::handle`] raises this on its own. It is exposed anyway,
/// because it is part of what dual-era MCP promises a caller it can raise,
/// and the day a capability *is* required this is where the error comes
/// from rather than a second definition invented at the call site.
pub fn missing_required_client_capability(id: &Value, missing: &[&str]) -> Outcome {
    error_reply(
        id,
        MISSING_CLIENT_CAPABILITY,
        "Missing required client capability",
        Some(json!({ "requiredCapabilities": missing })),
        400,
    )
}

/// `-32022 UnsupportedProtocolVersion`: `data.supported` lists
/// [`crate::SUPPORTED_VERSIONS`], `data.requested` echoes what the caller
/// asked for. HTTP `400`.
pub fn unsupported_protocol_version(id: &Value, requested: &str) -> Outcome {
    error_reply(
        id,
        UNSUPPORTED_PROTOCOL_VERSION,
        "Unsupported protocol version",
        Some(json!({ "supported": crate::SUPPORTED_VERSIONS, "requested": requested })),
        400,
    )
}
