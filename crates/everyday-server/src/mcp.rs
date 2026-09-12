//! Letting somebody else's model reach this vault's tools, over HTTP.
//!
//! `everyday-mcp` translates one JSON-RPC message into one [`everyday_mcp::Outcome`]
//! and knows nothing about a vault. This module is the other half: a
//! [`Config`], a [`VaultHost`] that answers [`everyday_mcp::Host`] by calling
//! [`Service::call`], and a `POST /mcp` route -- on its own listener, on its
//! own port, deliberately not folded into the `/v1` router in `routes.rs`.
//! That router is for another copy of this application; this one is for
//! somebody else's, and the two have different shapes of client, different
//! headers, and a different set of things worth checking before a request is
//! allowed to run.
//!
//! # Calls go through `Service::call`, not `tools::dispatch`
//!
//! `everyday_service::domains::meta::run_tool` already filters against what
//! this vault can actually offer and already refuses a destructive tool
//! without confirmation. Reaching past it into `tools::dispatch` would skip
//! two things `docs/plans/mcp.md` calls out by name: the change-event
//! fan-out, so a task an external agent creates appears in an open window
//! immediately, and the scope check on the caller's [`Ctx`]. [`VaultHost`]
//! is built precisely so that every call from this route is a
//! [`Service::call`] and nothing else.
//!
//! # What a retry is not
//!
//! The plan claimed a third thing -- idempotency -- and it is not true here,
//! so it is written down rather than left to be discovered. A [`Ctx`] built
//! from a bearer token carries no `request_id`, and
//! [`Service::call`](Service::call) only records a write when it has one. A
//! `tools/call` that creates a task and is retried therefore creates two.
//!
//! That is not an oversight to fix later; MCP gives us nothing to key on. A
//! JSON-RPC id is unique only within a connection, the modern era has no
//! connection to speak of -- every request is self-contained -- and the
//! specification's own retry mechanism requires the id to *change* between an
//! attempt and its retry. There is no stable key in the protocol, and
//! inventing one from the arguments would make two deliberate identical calls
//! into one. A model that wants to know whether its write landed should read
//! it back, which is what the catalogue's listing tools are for.
//!
//! # Destructive tools are absent, not refused
//!
//! `run_tool` answers a destructive call with `confirm_required` and a
//! message explaining how to get past it -- which is the right answer to a
//! script and the wrong one to a model, because a model reads that message
//! and simply calls again with the flag set. So [`VaultHost`] never takes
//! `confirmDestructive` from the wire. It sets it from `allow_destructive`,
//! which is this server's own configuration, and when that configuration
//! says no, the tool is filtered out of `tools/list` entirely rather than
//! listed and then refused. A gate whose refusal explains how to get past it
//! is not a gate.
//!
//! # A locked vault offers nothing
//!
//! [`VaultHost::tools`] answers `Ok(vec![])` -- not an error -- for a vault
//! that is locked or has never been opened. That is the accurate statement
//! of what this server can do right now, and `everyday-mcp`'s baseline
//! instructions already tell a calling model as much, so the empty list does
//! not read as a broken connection.
//!
//! # Telling a stream the vault opened
//!
//! [`Server`] is an [`EventSink`]; its `lock_state` pushes onto a broadcast
//! channel every open stream is reading from. Nothing in this module wires
//! it into the application's own event fan-out -- that is
//! [`Running::sink`]'s job to make possible and phase 4's job to actually
//! do, the same way `everyday_server::fanout` folds the TLS server's
//! broadcaster in today.
//!
//! # Loopback by default, and `Origin` is checked
//!
//! [`Config::default`] listens on `127.0.0.1`, not `0.0.0.0`: sharing
//! defaults the other way because sharing is *for* other machines, and an
//! MCP server is for an agent on this desk. Either way, a present-but-invalid
//! `Origin` header is refused with `403` -- a plain HTTP server on a loopback
//! port is reachable by any web page the person has open, via DNS rebinding,
//! and this is the one control that stops it. There is no TLS here at all:
//! a pinned certificate is right for this application talking to itself and
//! wrong for an MCP client, which has no pinning story and will simply
//! refuse to connect to one.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use everyday_core::agent::tools::Effect;
use everyday_mcp::{HostError, Outcome, ToolDef};
use everyday_service::{CommandError, CommandResult, Ctx, EventSink, Scope, Service};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::auth::Registry;

/// The port an MCP listener binds by default.
///
/// One along from [`crate::DEFAULT_PORT`] and, like it, unassigned by IANA.
/// A different port from sharing's on purpose: the two listeners answer
/// different protocols to different audiences and can be on or off
/// independently, so they must never be able to collide.
pub const DEFAULT_MCP_PORT: u16 = 7398;

/// The file this configuration is written to, beside `server.json` in the
/// application's own configuration directory -- not the vault, for the same
/// reason `server.json` is not: `everyday backup` copies the vault, and a
/// bearer token kept there would end up in every backup ever made.
pub const CONFIG_FILE: &str = "mcp.json";

/// How an MCP listener is set up, and what it remembers between runs.
///
/// # Why the token is plaintext, and why that is all right
///
/// Every other credential this application keeps is hashed -- [`Registry`]
/// stores only a `token_hash` for this device, exactly as it does for every
/// paired phone. This file is the one deliberate exception, and it is worth
/// justifying rather than assuming.
///
/// `everyday mcp` (phase 3) has to send this exact token on every request it
/// forwards, and it has no keychain of its own to ask -- it is a pipe
/// between stdin/stdout and this route, spawned fresh by whatever client
/// launched it, with no session to remember anything in. The alternative to
/// a plaintext file is a person copying the token from wherever it was shown
/// once into a second place by hand, which is both a worse security property
/// -- it sits in shell history, a clipboard manager, a text editor's swap
/// file -- and worse for no compensating benefit, since a hash cannot be
/// turned back into the token that produced it and so buys nothing here that
/// plaintext does not already have to concede.
///
/// Two things mitigate it. [`Config::save`] writes this file `0600` on
/// unix, the same idiom `restrict_socket` uses in `lib.rs`, so only this
/// user's own processes can read it. And revoking access is deleting the
/// device from [`Registry`] -- which is the one list "what can reach this
/// vault" has ever had -- and that invalidates the token regardless of
/// what this file still says, because [`Registry::authenticate`] is what a
/// request is actually checked against, never this file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Where to listen. Loopback by default -- see the module doc's
    /// "Loopback by default, and `Origin` is checked".
    pub listen: SocketAddr,
    /// Whether the listener is started. Persisted, so a machine that had
    /// this on has it on again after a restart.
    pub enabled: bool,
    /// May a destructive tool be offered at all? Off by default; see the
    /// module doc's "Destructive tools are absent, not refused".
    pub allow_destructive: bool,
    /// The bearer token an MCP client authenticates with, in plaintext.
    /// See this struct's own doc for why that is the deliberate choice.
    pub token: Option<String>,
    /// The [`Registry`] device id this token was issued as, so the settings
    /// panel can show "issued" without holding the token itself and so
    /// revoking the right row is unambiguous.
    pub device_id: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], DEFAULT_MCP_PORT)),
            enabled: false,
            allow_destructive: false,
            token: None,
            device_id: None,
        }
    }
}

impl Config {
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join(CONFIG_FILE))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> CommandResult<()> {
        std::fs::create_dir_all(dir)
            .map_err(|e| CommandError::new("io", format!("{}: {e}", dir.display())))?;
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| CommandError::new("internal", e.to_string()))?;
        let path = dir.join(CONFIG_FILE);
        everyday_core::fsutil::write_atomic(&path, text.as_bytes(), "mcp")
            .map_err(|e| CommandError::new("io", e.to_string()))?;
        restrict_config(&path);
        Ok(())
    }
}

/// Keep the plaintext token readable by nobody but this user. See
/// [`Config`]'s own doc for why the file holds plaintext at all.
#[cfg(unix)]
fn restrict_config(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!(error = %e, "could not restrict mcp.json's permissions");
    }
}

#[cfg(not(unix))]
fn restrict_config(_path: &Path) {}

/// The name a token minted by [`issue_token`] is recorded under.
///
/// A generic name rather than a client's own: this function is called
/// before any client has connected to say what it is, unlike
/// [`Registry::pair`], where the device names itself as part of the
/// exchange. A panel built on top of this in phase 4 is free to ask the
/// person what to call it and pass that name straight to [`Registry::issue`]
/// instead of going through here.
const DEVICE_NAME: &str = "MCP client";

/// Mint a token for an MCP client, and remember it.
///
/// The minting half of the token a settings panel shows once, with a copy
/// button. Wraps [`Registry::issue`] -- so an MCP client becomes a row in
/// the same device list as a paired phone, not a second parallel notion of
/// who may reach this vault -- and then writes the token and device id into
/// `mcp.json` so `everyday mcp` can authenticate without a person pasting it
/// into two places. See [`Config`]'s doc for what that costs and why it is
/// accepted.
pub fn issue_token(registry: &Registry, dir: &Path, scopes: Vec<Scope>) -> CommandResult<String> {
    let (token, device_id) = registry.issue(DEVICE_NAME, scopes)?;
    let mut config = Config::load(dir);
    config.token = Some(token.clone());
    config.device_id = Some(device_id);
    config.save(dir)?;
    Ok(token)
}

// ---- the host --------------------------------------------------------

/// One field of `list_tools`'s answer, as it comes back over the wire.
///
/// Not `everyday_service::domains::meta::ToolInfo` itself -- that type
/// derives `Serialize` only, for the same reason every command's result
/// type does: it crosses the JSON boundary in one direction, out of the
/// service. Deserialising `Service::call`'s own `Value` back into a
/// matching shape here is cheaper than adding a derive to a crate this one
/// otherwise has no reason to change, and it costs nothing: this struct and
/// `ToolInfo` are kept in step by the same JSON both sides already agree on
/// the shape of.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTool {
    name: String,
    title: String,
    description: String,
    effect: String,
    schema: Value,
}

fn host_error(e: CommandError) -> HostError {
    HostError { code: e.code, message: e.message }
}

/// [`everyday_mcp::Host`] over a vault, for one caller.
///
/// Built fresh per request rather than once per server, because the
/// [`Ctx`] it carries is the calling device's own -- built by
/// [`Registry::authenticate`] from that request's bearer token -- and two
/// requests from two different tokens must never share one.
pub struct VaultHost {
    service: Arc<Service>,
    ctx: Ctx,
    allow_destructive: bool,
}

impl VaultHost {
    pub fn new(service: Arc<Service>, ctx: Ctx, allow_destructive: bool) -> Self {
        Self { service, ctx, allow_destructive }
    }
}

impl everyday_mcp::Host for VaultHost {
    fn server_info(&self) -> everyday_mcp::Implementation {
        everyday_mcp::Implementation {
            name: "Every Day".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    fn instructions(&self) -> Option<String> {
        None
    }

    fn tools(&self) -> impl std::future::Future<Output = Result<Vec<ToolDef>, HostError>> + Send {
        let service = self.service.clone();
        let ctx = self.ctx.clone();
        let allow_destructive = self.allow_destructive;
        async move {
            // `list_tools` itself only asks that a vault be open, not that
            // it be unlocked -- most of its other callers are the window
            // about to draw the unlock screen, which needs that distinction.
            // This caller does not, so the check is made here rather than
            // leaning on a service command that answers a different
            // question. See the module doc's "A locked vault offers
            // nothing".
            let unlocked = service.get().is_some_and(|vault| vault.is_unlocked());
            if !unlocked {
                return Ok(Vec::new());
            }

            let value = service.call(ctx, "list_tools", Value::Null).await.map_err(host_error)?;
            let tools: Vec<WireTool> = serde_json::from_value(value).map_err(|e| HostError {
                code: "internal".to_string(),
                message: format!("list_tools answered with something unexpected: {e}"),
            })?;

            Ok(tools
                .into_iter()
                // Absent, not refused -- see the module doc. Filtered here
                // rather than trusted to `run_tool`'s own refusal, because
                // the whole point is that a destructive tool a model was
                // never told about is a tool it was never tempted to ask
                // for confirmation to get around.
                .filter(|t| allow_destructive || t.effect != "destructive")
                .map(|t| ToolDef {
                    name: t.name,
                    title: t.title,
                    description: t.description,
                    input_schema: t.schema,
                    read_only: t.effect == "read",
                    destructive: t.effect == "destructive",
                })
                .collect())
        }
    }

    fn call(
        &self,
        name: &str,
        arguments: &Value,
    ) -> impl std::future::Future<Output = Result<Value, HostError>> + Send {
        let service = self.service.clone();
        let ctx = self.ctx.clone();
        let allow_destructive = self.allow_destructive;
        let name = name.to_string();
        let arguments = arguments.clone();
        async move {
            // Checked against the catalogue directly, not against this
            // caller's own `tools()` answer -- cheaper than a round trip
            // through `Service::call` just to ask a question this process
            // can answer from `everyday_core::agent::tools` alone, and the
            // answer is the same one `tools()` would have filtered on.
            if !allow_destructive
                && everyday_core::agent::tools::find(&name)
                    .is_some_and(|t| t.effect == Effect::Destructive)
            {
                return Err(HostError {
                    code: "unsupported".to_string(),
                    message: format!(
                        "{name} deletes something, and this server's destructive-tools \
                         switch is off"
                    ),
                });
            }

            let args = json!({
                "name": name,
                "arguments": arguments,
                // Never the caller's: see the module doc's "Destructive
                // tools are absent, not refused". This is the server's own
                // configuration, not a field `tools/call` accepted from the
                // wire.
                "confirmDestructive": allow_destructive,
            });
            service.call(ctx, "run_tool", args).await.map_err(host_error)
        }
    }
}

// ---- the server and the route -----------------------------------------

/// Shared state behind `/mcp`: the vault, the device registry, and the
/// switch every [`VaultHost`] built from a request is handed.
pub struct Server {
    service: Arc<Service>,
    registry: Arc<Registry>,
    allow_destructive: bool,
    /// Fires once per vault unlock. See the module doc's "Telling a stream
    /// the vault opened".
    unlocked: broadcast::Sender<()>,
}

impl Server {
    pub fn new(
        service: Arc<Service>,
        registry: Arc<Registry>,
        allow_destructive: bool,
    ) -> Arc<Self> {
        // A small backlog: this only ever carries unlock notifications, and
        // a stream that missed several still only needs to know "at least
        // one happened" to ask `tools/list` again.
        let (unlocked, _) = broadcast::channel(8);
        Arc::new(Self { service, registry, allow_destructive, unlocked })
    }
}

impl EventSink for Server {
    fn lock_state(&self, locked: bool) {
        if !locked {
            // No listener is not an error: it is the ordinary state of a
            // server nobody has opened a stream against yet.
            let _ = self.unlocked.send(());
        }
    }
}

fn router(server: Arc<Server>) -> Router {
    Router::new().route("/mcp", post(post_mcp).get(get_mcp).delete(delete_mcp)).with_state(server)
}

/// Whether `message` is attempting the modern, `_meta`-driven era, by the
/// same rule `everyday_mcp::handle` uses to make the same decision
/// internally: any `_meta` key under `io.modelcontextprotocol/`.
///
/// Duplicated rather than imported, because that predicate is
/// `pub(crate)` inside `everyday-mcp` and this is its only caller outside
/// that crate. It has to agree exactly with the protocol crate's own
/// answer -- header mirroring is only required for the era `handle` itself
/// treats as modern -- so it is written against the same two facts
/// (`params._meta`, the key prefix) rather than approximated from a
/// narrower signal such as "does `_meta.protocolVersion` happen to be set".
fn is_modern_request(message: &Value) -> bool {
    message
        .get("params")
        .and_then(|p| p.get("_meta"))
        .and_then(Value::as_object)
        .is_some_and(|meta| meta.keys().any(|k| k.starts_with("io.modelcontextprotocol/")))
}

/// Check the three headers the modern Streamable HTTP binding mirrors from
/// the body, and answer the mismatch `everyday-mcp` already knows how to
/// spell if one disagrees.
///
/// Legacy requests carry none of this -- there is no per-request `_meta`
/// for a header to mirror -- so this is a no-op for anything
/// [`is_modern_request`] does not recognise as an attempt at `2026-07-28`.
fn check_header_mirroring(headers: &HeaderMap, message: &Value) -> Option<Response> {
    if !is_modern_request(message) {
        return None;
    }
    let expected = everyday_mcp::expected_headers(message);
    let id = message.get("id").cloned().unwrap_or(Value::Null);

    let checks: [(&str, Option<&String>); 3] = [
        ("mcp-protocol-version", expected.protocol_version.as_ref()),
        ("mcp-method", expected.method.as_ref()),
        ("mcp-name", expected.name.as_ref()),
    ];

    for (header_name, want) in checks {
        let Some(want) = want else { continue };
        let got = headers
            .get(header_name)
            .and_then(|v| v.to_str().ok())
            .map(everyday_mcp::decode_header_value);
        if got.as_deref() != Some(want.as_str()) {
            let detail = match got {
                Some(got) => format!("{header_name}: expected {want:?}, got {got:?}"),
                None => format!("{header_name} is required and was not sent"),
            };
            return Some(expect_reply(everyday_mcp::header_mismatch(&id, &detail)));
        }
    }
    None
}

/// Turn an [`Outcome`] this module built itself -- as opposed to one
/// `everyday_mcp::handle` returned -- into a response.
///
/// Every constructor in `everyday_mcp::errors` this binding calls directly
/// answers with `Outcome::Reply`; there is no header check that could
/// plausibly open a stream. Spelled out as a match anyway, with the other
/// arms panicking, so a change to what these constructors can return is a
/// compile error here rather than a silently wrong response on the wire.
fn expect_reply(outcome: Outcome) -> Response {
    match outcome {
        Outcome::Reply { status, body } => reply_response(status, body),
        Outcome::Accepted | Outcome::Listen { .. } => {
            unreachable!("this binding only ever calls constructors that reply")
        }
    }
}

fn reply_response(status: u16, body: Value) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(body)).into_response()
}

fn parse_error() -> Response {
    let body = json!({
        "jsonrpc": "2.0",
        "id": Value::Null,
        "error": { "code": -32700, "message": "Parse error" },
    });
    (StatusCode::BAD_REQUEST, Json(body)).into_response()
}

/// Whether `origin` is one this loopback listener should trust.
///
/// Exactly the three spellings a browser gives a page served from this
/// machine -- `http://localhost`, `http://127.0.0.1`, `http://[::1]` --
/// each with or without a port, and nothing else. Not `https`: this
/// listener never terminates TLS (see the module doc), so an `Origin`
/// claiming it is not a browser tab talking to this server.
fn origin_is_local(origin: &str) -> bool {
    let Some(rest) = origin.strip_prefix("http://") else { return false };
    let host = match rest.strip_prefix('[') {
        Some(after_bracket) => match after_bracket.split_once(']') {
            Some((addr, _)) => format!("[{addr}]"),
            None => return false,
        },
        None => rest.split(['/', ':']).next().unwrap_or(rest).to_string(),
    };
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]")
}

/// Refuse a request whose `Origin` is present and not local.
///
/// The exact condition matters: an *absent* `Origin` is not refused by
/// this rule. A native client -- `everyday mcp`, `claude mcp add
/// --transport http` -- sends no `Origin` at all, and refusing it for
/// omitting a header only a browser ever sends would lock out every client
/// that is not a browser.
fn reject_bad_origin(headers: &HeaderMap) -> Option<Response> {
    match headers.get(header::ORIGIN) {
        None => None,
        Some(value) => match value.to_str() {
            Ok(origin) if origin_is_local(origin) => None,
            _ => Some(StatusCode::FORBIDDEN.into_response()),
        },
    }
}

/// Turn a bearer token into a [`Ctx`], or answer `401`.
///
/// Answers with a bare [`StatusCode`] rather than a built [`Response`]: the
/// only failure this ever reports is "unauthorized", so there is nothing a
/// response body would add, and `Result<_, Response>` would make this
/// function's error the size of a whole HTTP response for a caller that
/// only ever throws it straight into `into_response`.
fn authenticate(server: &Server, headers: &HeaderMap) -> Result<Ctx, StatusCode> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let Some(token) = token else { return Err(StatusCode::UNAUTHORIZED) };
    server.registry.authenticate(token).map_err(|_| StatusCode::UNAUTHORIZED)
}

/// How often to send a comment down an idle stream, so a proxy or a phone
/// radio between here and the client does not decide the connection is
/// dead. Same value `sse.rs` uses for the same reason.
const KEEPALIVE: Duration = Duration::from_secs(15);

/// The subscription id the legacy standalone `GET` stream's notifications
/// carry.
///
/// Legacy has no `subscriptions/listen` request to correlate a subscription
/// id back to -- there is no per-request `_meta` in that era at all -- so
/// this is a fixed placeholder rather than one minted per connection. A
/// legacy client has nothing to compare it against and will not look.
const LEGACY_SUBSCRIPTION: &str = "legacy";

fn sse_json(value: &Value) -> Event {
    Event::default().data(value.to_string())
}

/// Build the notification stream both eras open: `subscriptions/listen`'s
/// response (modern, `ack` carries the acknowledgement) and the legacy
/// standalone `GET` (no acknowledgement, since legacy has no handshake for
/// one to answer). One `notifications/tools/list_changed` per vault
/// unlock, for as long as the client keeps the connection open.
fn listen_stream(
    server: &Server,
    subscription_id: String,
    ack: Option<Value>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>> + use<>> {
    let rx = server.unlocked.subscribe();
    let stream = futures::stream::unfold((ack, rx), move |(pending, mut rx)| {
        let subscription_id = subscription_id.clone();
        async move {
            if let Some(ack) = pending {
                return Some((Ok(sse_json(&ack)), (None, rx)));
            }
            loop {
                match rx.recv().await {
                    Ok(()) => {
                        let body = everyday_mcp::tools_list_changed(&subscription_id);
                        return Some((Ok(sse_json(&body)), (None, rx)));
                    }
                    // A stream that fell behind on unlock notifications has
                    // lost nothing worth resending: the only useful content
                    // is "the list changed, ask again", and that is still
                    // true after a lag exactly as it was before one.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEPALIVE))
}

/// Tell a proxy in front of this stream not to buffer it. Meaningless on a
/// loopback connection with nothing between the two ends, and harmless
/// everywhere else -- cheap enough to send unconditionally rather than
/// worth a flag for whether one might be there.
fn without_buffering(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(HeaderName::from_static("x-accel-buffering"), HeaderValue::from_static("no"));
    response
}

async fn post_mcp(State(server): State<Arc<Server>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Some(response) = reject_bad_origin(&headers) {
        return response;
    }
    let ctx = match authenticate(&server, &headers) {
        Ok(ctx) => ctx,
        Err(status) => return status.into_response(),
    };
    let message: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return parse_error(),
    };
    if let Some(response) = check_header_mirroring(&headers, &message) {
        return response;
    }

    let host = VaultHost::new(server.service.clone(), ctx, server.allow_destructive);
    match everyday_mcp::handle(&host, &message).await {
        Outcome::Reply { status, body } => reply_response(status, body),
        Outcome::Accepted => StatusCode::ACCEPTED.into_response(),
        Outcome::Listen { subscription_id, ack } => {
            without_buffering(listen_stream(&server, subscription_id, Some(ack)).into_response())
        }
    }
}

/// The legacy standalone stream: `notifications/tools/list_changed` for a
/// client with no `subscriptions/listen` to open one from. A modern-only
/// client never sends this -- see `delete_mcp` and the crate doc.
async fn get_mcp(State(server): State<Arc<Server>>, headers: HeaderMap) -> Response {
    if let Some(response) = reject_bad_origin(&headers) {
        return response;
    }
    if let Err(status) = authenticate(&server, &headers) {
        return status.into_response();
    }
    without_buffering(listen_stream(&server, LEGACY_SUBSCRIPTION.to_string(), None).into_response())
}

/// `2026-07-28` removed protocol-level sessions and the `Mcp-Session-Id`
/// that would have needed terminating, so there is nothing for `DELETE` to
/// do here at all -- not even for a legacy client, which never had one to
/// begin with in this server's telling of it (see `handle_legacy`'s
/// `Mcp-Session-Id` handling, or rather the absence of it, in
/// `everyday-mcp`). The specification calls for `405` on a method this
/// endpoint does not implement, so that is the whole of this handler.
async fn delete_mcp() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}

// ---- lifecycle ----------------------------------------------------------

/// A running MCP listener, and the handle that stops it.
///
/// [`crate::Running`] generic, instantiated with an `Arc<dyn EventSink>`
/// rather than a name of its own: this listener and the sharing one
/// (`everyday_server::start`) used to keep two copies of the same
/// `address`/`shutdown`/`stopped` and the same `stop`/`stop_and_wait`/`Drop`,
/// and the copies had already drifted once -- the TLS branch of one forgot
/// to signal `stopped` on its way out, and the fix landed only there until
/// this module stopped keeping its own copy to fix. The field is still
/// called `server` rather than `sink`, matching `crate::Running`'s own
/// field: what a caller here gets back is this listener's [`Server`], seen
/// through the one trait it implements that a caller of *this* function
/// actually needs.
pub type Running = crate::Running<Arc<dyn EventSink>>;

/// Start answering `POST/GET/DELETE /mcp` on `config.listen`.
///
/// Must be spawned on the runtime the rest of the application uses, exactly
/// as `everyday_server::start` must -- see that function's doc for why a
/// second runtime here would be a second, uncoordinated set of threads
/// touching the same vault.
pub async fn start(
    service: Arc<Service>,
    registry: Arc<Registry>,
    config: &Config,
) -> CommandResult<Running> {
    let server = Server::new(service, registry, config.allow_destructive);

    let listener = tokio::net::TcpListener::bind(config.listen).await.map_err(|e| {
        CommandError::new("io", format!("could not listen on {}: {e}", config.listen))
    })?;
    let router = router(server.clone());

    // Always plain: an MCP client has no pinning story of its own and would
    // simply refuse to connect to a self-signed certificate -- see the
    // module doc's "Loopback by default, and `Origin` is checked".
    let running = crate::spawn_server(listener, router, None, server as Arc<dyn EventSink>)?;
    tracing::info!(address = %running.address, "serving MCP");
    Ok(running)
}
