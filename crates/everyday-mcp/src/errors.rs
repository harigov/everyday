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
pub(crate) const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
/// Outside `-32768`..`-32000` on purpose: MCP partitions that whole block
/// for itself, `-32000`..`-32019` as legacy implementation-defined codes
/// ("new implementations SHOULD NOT use codes from this sub-range at
/// all") and `-32020`..`-32099` reserved to the specification, so an
/// undefined code from either is not this crate's to spend. See
/// [`transport_error`]'s own doc for who does use this one.
pub(crate) const TRANSPORT_ERROR: i64 = -31000;
pub(crate) const REQUEST_REFUSED: i64 = -31001;

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

/// `-31000 Transport error`: nothing answered a `POST /mcp` at all.
///
/// Not a failure of the protocol this crate implements but of whatever sits
/// underneath it -- `everyday mcp`'s pipe is the one caller today, forwarding
/// a request to a listener that may not even be running, since the switch it
/// depends on is off by default. Every transport this crate answers directly
/// always gets an HTTP response of some kind; a caller reaching for this
/// code is, by definition, one that did not.
pub fn transport_error(id: &Value, detail: &str) -> Outcome {
    error_reply(id, TRANSPORT_ERROR, "Transport error", Some(json!({ "detail": detail })), 502)
}

/// `-31001 Request refused`: the listener answered, but with a status whose
/// body is empty by design -- `401`, `403`, `405`, or anything else with
/// nothing in it to forward as this reply's `data`.
///
/// Kept apart from [`transport_error`]: that code means no HTTP answer
/// arrived at all; this one means an answer arrived and said no. `status`
/// is echoed in `data` rather than reused as this `Outcome`'s own HTTP
/// status, because a caller piping this over stdio -- as `everyday mcp`
/// does -- has no HTTP response of its own to carry one on.
pub fn request_refused(id: &Value, status: u16, detail: &str) -> Outcome {
    error_reply(
        id,
        REQUEST_REFUSED,
        "Request refused",
        Some(json!({ "status": status, "detail": detail })),
        status,
    )
}
