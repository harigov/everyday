//! One JSON-RPC message in, one [`Outcome`] out.
//!
//! Everything here is a pure function of the message and the [`Host`] it is
//! answered against. There is no session object because the modern era has
//! none to hold — "Servers MUST NOT rely on prior requests over the same
//! connection to establish context" — and the legacy era's session lives
//! entirely in the `Mcp-Session-Id` the HTTP binding mints and remembers,
//! which this crate never sees. `handle` reads one `Value`, calls `Host` at
//! most once, and returns; nothing here outlives the call.
//!
//! # The one distinction worth getting right
//!
//! A [`HostError`] arrives from two different kinds of failure, and they
//! are answered on the wire in two different ways:
//!
//! - **Protocol errors** — a `tools/call` naming a tool that does not
//!   exist — mean the *request* was wrong. These become a JSON-RPC error
//!   (`-32602`), because there is no result to hand back.
//! - **Tool execution errors** — everything else `Host::call` can fail
//!   with: a locked vault, a bad argument, a destructive call nobody
//!   confirmed — mean the request was fine and the *tool* refused. These
//!   become an ordinary JSON-RPC **result** with `isError: true` and the
//!   message as text content.
//!
//! The specification's own reasoning is the one to keep in mind here:
//! "Clients SHOULD provide tool execution errors to language models to
//! enable self-correction." A model that gets `-32602` learns nothing
//! except that the call failed; a model that gets `isError: true` and
//! `"add_task: due_date must be a date like 2026-09-14, got \"next
//! friday\""` gets a corrected call on the next turn. That message is not
//! incidental — it is exactly what the `Args` accessors in
//! `everyday-core/src/agent/tools.rs` were written to produce, and this
//! function is the reason they were written that way. The only signal we
//! use to tell the two apart is `HostError::code == "unknown_tool"`,
//! which is the code `everyday-service`'s `run_tool` already raises for
//! exactly this case.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::{
    Era, Host, META_CLIENT_CAPABILITIES, META_PROTOCOL_VERSION, META_SERVER_INFO,
    META_SUBSCRIPTION_ID, NEWEST_LEGACY, Outcome, SUPPORTED_VERSIONS, ToolDef, errors,
    instructions,
};

/// How long a `server/discover` answer may be cached before asking again.
///
/// An hour: long enough that a client is not re-discovering on every call,
/// short enough that a vault whose backend or destructive setting changed
/// is not stale for a whole session. Nothing here depends on this being
/// exact, since we have no cache invalidation story beyond "ask again
/// eventually" -- there is no long-lived session to invalidate it in.
const DISCOVER_TTL_MS: u64 = 3_600_000;

/// A message that at least parses as JSON-RPC, before we know what era it
/// belongs to or whether it is one we answer.
///
/// Every field borrows from the message rather than copying out of it. The
/// message outlives the envelope by construction -- `handle` takes it by
/// reference and nothing here escapes the call -- and the alternative is a
/// deep clone of `params` on every request, which for `tools/call` is the
/// whole of a note body or an entry's text.
struct Envelope<'a> {
    method: &'a str,
    params: &'a Value,
    /// `Some` for a request that wants a reply; `None` for a notification.
    /// The distinction is "has an `id` member at all", not "has a
    /// non-null one" -- see [`parse`].
    id: Option<&'a Value>,
}

/// Pull `message` apart into an [`Envelope`], or say why it cannot be
/// answered as JSON-RPC at all.
///
/// Three shapes are rejected here rather than downstream, because they are
/// not "a request for something we don't do" -- they are not a request.
/// A missing or wrong `jsonrpc`, a missing or empty `method`, and an
/// explicit `id: null` are all malformed and get `-32600`. An *absent*
/// `id` is different: JSON-RPC spells a notification that way, and it is
/// not an error, it is [`handle`] returning [`Outcome::Accepted`] instead
/// of ever reaching this function's error path.
fn parse(message: &Value) -> Result<Envelope<'_>, Outcome> {
    let obj = message.as_object();

    let jsonrpc_ok = obj.and_then(|o| o.get("jsonrpc")).and_then(Value::as_str) == Some("2.0");
    let method =
        obj.and_then(|o| o.get("method")).and_then(Value::as_str).filter(|m| !m.is_empty());
    let id_present = obj.is_some_and(|o| o.contains_key("id"));
    let id_is_null = obj.and_then(|o| o.get("id")).is_some_and(Value::is_null);

    if !jsonrpc_ok || method.is_none() || (id_present && id_is_null) {
        // The id used on the error reply itself: the caller's, if they
        // sent one we can make sense of, else `null` -- which is the
        // conventional JSON-RPC answer when the request's own id could
        // not be established, and happens to be exactly right for the
        // `id: null` case too.
        let id = if id_present && !id_is_null {
            obj.and_then(|o| o.get("id")).cloned().unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        return Err(errors::invalid_request(&id, "not a well-formed JSON-RPC 2.0 message"));
    }

    // An absent `params` reads as an empty object, and the empty object is a
    // `static` so that borrowing one is free and needs no owner to outlive.
    static NO_PARAMS: OnceLock<Value> = OnceLock::new();
    let params =
        obj.and_then(|o| o.get("params")).unwrap_or_else(|| NO_PARAMS.get_or_init(|| json!({})));
    let id = if id_present { obj.and_then(|o| o.get("id")) } else { None };

    Ok(Envelope { method: method.expect("checked above"), params, id })
}

/// Whether `params` carries the modern per-request `_meta` this era is
/// defined by.
///
/// Checked by key prefix rather than "does `_meta` exist at all", because
/// a legacy client is free to send its own unrelated `_meta` (some do, for
/// tracing) and that must not be misread as an attempt at `2026-07-28`.
/// Only the presence of an `io.modelcontextprotocol/` key is that attempt
/// -- what happens if the *required* keys under it are missing is
/// [`handle_modern`]'s job, not this function's.
fn is_modern_attempt(params: &Value) -> bool {
    params
        .get("_meta")
        .and_then(Value::as_object)
        .is_some_and(|meta| meta.keys().any(|k| k.starts_with("io.modelcontextprotocol/")))
}

/// One JSON-RPC message in, one [`Outcome`] out. See the module doc for
/// the error-shape distinction this function exists to get right.
pub async fn handle<H: Host>(host: &H, message: &Value) -> Outcome {
    let envelope = match parse(message) {
        Ok(envelope) => envelope,
        Err(outcome) => return outcome,
    };

    let Some(id) = envelope.id else {
        // A notification. JSON-RPC notifications never get a JSON-RPC
        // response, whatever they name. `notifications/initialized` is
        // the only one a legacy client sends us; anything else arriving
        // with no `id` is still accepted rather than answered, because a
        // reply to a message that asked for none is itself a protocol
        // violation.
        return Outcome::Accepted;
    };

    // `initialize` and its companion notification are legacy-only:
    // 2026-07-28 removed the handshake, so a modern client never sends
    // either. Everything else is sorted by whether it carries the modern
    // `_meta` -- see `is_modern_attempt`.
    let modern = envelope.method != "initialize" && is_modern_attempt(envelope.params);

    if modern {
        handle_modern(host, &envelope, id).await
    } else {
        handle_legacy(host, &envelope, id).await
    }
}

async fn handle_modern<H: Host>(host: &H, envelope: &Envelope<'_>, id: &Value) -> Outcome {
    let protocol_version = crate::meta_str(envelope.params, META_PROTOCOL_VERSION);
    let has_capabilities = envelope
        .params
        .get("_meta")
        .and_then(Value::as_object)
        .is_some_and(|m| m.contains_key(META_CLIENT_CAPABILITIES));

    let Some(version) = protocol_version else {
        return errors::invalid_params(
            id,
            "_meta[\"io.modelcontextprotocol/protocolVersion\"] is required",
        );
    };
    if !has_capabilities {
        return errors::invalid_params(
            id,
            "_meta[\"io.modelcontextprotocol/clientCapabilities\"] is required",
        );
    }
    if !SUPPORTED_VERSIONS.contains(&version) {
        return errors::unsupported_protocol_version(id, version);
    }

    match envelope.method {
        "server/discover" => discover_result(host, id),
        "tools/list" => tools_list(host, id, Era::Modern).await,
        "tools/call" => tools_call(host, id, envelope.params, Era::Modern).await,
        // Modern-only: the legacy standalone stream is a `GET`, which
        // never reaches `handle` at all, so this is the only era in which
        // a subscription is a JSON-RPC method.
        "subscriptions/listen" => listen_result(),
        other => errors::method_not_found(id, other),
    }
}

async fn handle_legacy<H: Host>(host: &H, envelope: &Envelope<'_>, id: &Value) -> Outcome {
    match envelope.method {
        "initialize" => initialize_result(host, id, envelope.params),
        "tools/list" => tools_list(host, id, Era::Legacy).await,
        "tools/call" => tools_call(host, id, envelope.params, Era::Legacy).await,
        other => errors::method_not_found(id, other),
    }
}

/// `_meta["io.modelcontextprotocol/serverInfo"]`, which every modern
/// result "SHOULD" carry. Built fresh per call rather than cached on
/// `Host`, since a host is free to make `server_info` reflect something
/// that changes (a version string embedded at build time is the only
/// case today, but nothing here assumes it stays that way).
fn server_meta<H: Host>(host: &H) -> Value {
    let info = host.server_info();
    json!({ META_SERVER_INFO: { "name": info.name, "version": info.version } })
}

/// Add `resultType` and `_meta.serverInfo` to a legacy-shaped result when
/// the era calls for it. Kept as one function rather than three copies of
/// the same two `insert`s, because the day a fourth field joins
/// `resultType` there should be one place that needs it.
fn modern_envelope<H: Host>(mut result: Value, host: &H) -> Value {
    let obj = result.as_object_mut().expect("result is always built as an object");
    obj.insert("resultType".to_string(), json!("complete"));
    obj.insert("_meta".to_string(), server_meta(host));
    result
}

fn reply(id: &Value, result: Value) -> Outcome {
    Outcome::Reply { status: 200, body: json!({ "jsonrpc": "2.0", "id": id, "result": result }) }
}

fn discover_result<H: Host>(host: &H, id: &Value) -> Outcome {
    // Through `modern_envelope` like the other two, rather than spelling
    // `resultType` and `_meta` out again here. `server/discover` is
    // modern-only, so there is no era to branch on -- but that is a reason
    // for the envelope to be unconditional, not a reason to write a third
    // copy of the two fields the function exists to own.
    let result = json!({
        "supportedVersions": SUPPORTED_VERSIONS,
        "capabilities": { "tools": { "listChanged": true } },
        "instructions": instructions::full(host.instructions().as_deref()),
        "ttlMs": DISCOVER_TTL_MS,
        "cacheScope": "public",
    });
    reply(id, modern_envelope(result, host))
}

fn initialize_result<H: Host>(host: &H, id: &Value, params: &Value) -> Outcome {
    // Echo what the client asked for when we can serve it; fall back to
    // our newest legacy revision otherwise. This never fails the
    // handshake outright -- a legacy client with no way to fall forward
    // is exactly the caller the specification says to be lenient with,
    // and refusing outright would just be a worse answer than "here is
    // the closest thing I speak".
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let echoed = match requested {
        Some(v) if SUPPORTED_VERSIONS.contains(&v) => v,
        _ => NEWEST_LEGACY,
    };
    let info = host.server_info();
    reply(
        id,
        json!({
            "protocolVersion": echoed,
            "capabilities": { "tools": { "listChanged": true } },
            "serverInfo": { "name": info.name, "version": info.version },
            "instructions": instructions::full(host.instructions().as_deref()),
        }),
    )
}

fn tool_json(tool: &ToolDef) -> Value {
    json!({
        "name": tool.name,
        "title": tool.title,
        "description": tool.description,
        "inputSchema": tool.input_schema,
        "annotations": {
            "readOnlyHint": tool.read_only,
            "destructiveHint": tool.destructive,
        },
    })
}

async fn tools_list<H: Host>(host: &H, id: &Value, era: Era) -> Outcome {
    let tools = match host.tools().await {
        Ok(tools) => tools,
        Err(e) => return errors::internal_error(id, &e.message),
    };
    // The host's own order, never re-sorted. `tools.rs` orders its
    // catalogue as a table of contents a model reads top to bottom; a
    // `HashMap` anywhere on this path would scramble that back into
    // whatever the allocator felt like, silently, in only some builds.
    let listed: Vec<Value> = tools.iter().map(tool_json).collect();

    let result = json!({ "tools": listed });
    let result = if era == Era::Modern { modern_envelope(result, host) } else { result };
    reply(id, result)
}

async fn tools_call<H: Host>(host: &H, id: &Value, params: &Value, era: Era) -> Outcome {
    let Some(name) = crate::call_target("tools/call", params) else {
        return errors::invalid_params(id, "`name` is required");
    };
    // Borrowed, never cloned: this is the value carrying a note's body or an
    // entry's text, and `Host::call` only ever reads it.
    static NO_ARGUMENTS: OnceLock<Value> = OnceLock::new();
    let arguments =
        params.get("arguments").unwrap_or_else(|| NO_ARGUMENTS.get_or_init(|| json!({})));

    let result = match host.call(name, arguments).await {
        Ok(value) => call_result(&value),
        // `unknown_tool` is the one `HostError` this function treats as
        // the *request's* fault rather than the tool's -- see the module
        // doc. Everything else, including a tool that does not exist
        // becoming reachable some other way, is answered as a result the
        // model can read and correct itself from.
        Err(e) if e.code == "unknown_tool" => return errors::invalid_params(id, &e.message),
        Err(e) => call_error(&e.message),
    };
    let result = if era == Era::Modern { modern_envelope(result, host) } else { result };
    reply(id, result)
}

fn call_result(value: &Value) -> Value {
    // Pretty-printed for the client that only reads `content`, and passed
    // through untouched as `structuredContent` for the one that parses
    // it. Both point at the same value; nothing here loses information
    // one reader has and the other does not.
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": value,
        "isError": false,
    })
}

fn call_error(message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true,
    })
}

/// A counter, not a random value.
///
/// This crate has no source of randomness and no I/O to fetch one from,
/// and a subscription id's only job is to be distinct from every other
/// live subscription in this process -- a monotonic counter already does
/// that, without reaching for the `uuid` crate over a single call site.
/// It has the useful side effect of making subscription ids predictable
/// in tests.
static NEXT_SUBSCRIPTION: AtomicU64 = AtomicU64::new(1);

fn fresh_subscription_id() -> String {
    format!("sub-{}", NEXT_SUBSCRIPTION.fetch_add(1, Ordering::Relaxed))
}

fn listen_result() -> Outcome {
    let subscription_id = fresh_subscription_id();
    let ack = json!({
        "jsonrpc": "2.0",
        "method": "notifications/subscriptions/acknowledged",
        "params": { "_meta": { META_SUBSCRIPTION_ID: subscription_id.clone() } },
    });
    Outcome::Listen { subscription_id, ack }
}

/// Build a `notifications/tools/list_changed` notification for a stream a
/// `subscriptions/listen` call opened, correlated back to it the way the
/// specification requires: `_meta["io.modelcontextprotocol/subscriptionId"]`
/// naming the subscription this notification rides on.
///
/// Exposed rather than kept internal because the transport, not this
/// crate, decides *when* to send one -- on the vault's lock-state
/// transition, per `docs/plans/mcp.md`'s "A locked vault offers nothing"
/// -- and needs to build the body without re-deriving the `_meta` shape
/// for itself.
pub fn tools_list_changed(subscription_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/tools/list_changed",
        "params": { "_meta": { META_SUBSCRIPTION_ID: subscription_id } },
    })
}
