//! The Model Context Protocol, and nothing that carries it.
//!
//! This crate translates between one JSON-RPC message and one answer. It
//! has no socket, no clock beyond what a caller hands it, no notion of a
//! vault, and no async runtime dependency beyond the futures [`Host`]
//! itself returns. That is not caution for its own sake; it is the same
//! argument `everyday-core::agent::tools` makes about itself, in its own
//! module doc, and it is worth restating here because this crate is where
//! that argument gets tested against a harder case: a *wire* protocol,
//! with a specification, versions, and a revision that changed while this
//! was being written.
//!
//! "The protocol lives apart from the transport" means a bug in framing —
//! did we choose the right era, did we build the right error code, does a
//! tool failure read back as something a model can act on — is a unit
//! test in `tests/protocol.rs` that runs in microseconds, rather than a
//! thing discovered by attaching Claude Desktop to a running server and
//! reading a stack trace. `everyday-server` is the only place this crate
//! is expected to be driven from, and it owes that crate exactly two
//! things: a [`Host`] to call, and the JSON body of a request. Everything
//! this crate returns is an [`Outcome`], which says what to send and at
//! what HTTP status, so the transport never has to know a JSON-RPC error
//! code to serve a request correctly.
//!
//! # Two eras, on purpose
//!
//! `2026-07-28` — [`MODERN`] — removed MCP's `initialize` handshake,
//! protocol-level sessions and the standalone `GET` stream: every request
//! now carries its own version and capabilities in `_meta`, and a server
//! answers statelessly. `docs/plans/mcp-protocol-notes.md` is the
//! normative account of what changed and why; read it before touching
//! [`handle`]. We also answer `2025-11-25` and `2025-06-18` — collected
//! under [`Era::Legacy`] — because the revision above is six weeks old at
//! the time of writing and the clients on this desk still mostly speak
//! the handshake. A message selects its own era: modern per-request
//! `_meta` means `2026-07-28`; an `initialize` method means whichever
//! legacy revision the client asked for. There is no session object
//! anywhere in this crate, in either era — see [`handle`]'s module doc
//! for what that does and does not mean for the legacy side.
//!
//! # What is deliberately not here
//!
//! `resources/`, `prompts/`, `sampling/`, `elicitation/`, pagination
//! beyond one page, and anything to do with a network. `docs/plans/mcp.md`
//! explains each of those choices; they are not oversights.

mod errors;
mod handle;
mod headers;
mod instructions;

pub use errors::{
    header_mismatch, invalid_params, invalid_request, method_not_found,
    missing_required_client_capability, unsupported_protocol_version,
};
pub use handle::{handle, tools_list_changed};
pub use headers::{ExpectedHeaders, decode_header_value, expected_headers};

use serde_json::Value;

/// The revision this server implements first and answers by default when
/// a legacy client gives us no steer at all.
pub const MODERN: &str = "2026-07-28";

/// Every protocol revision this server understands, newest first. What
/// [`unsupported_protocol_version`] lists as `data.supported`, and the
/// one place "which versions do we speak" is written down — a version
/// dropped here is a version dropped everywhere, per
/// `docs/plans/mcp.md`'s note on how this is meant to age.
pub const SUPPORTED_VERSIONS: &[&str] = &[MODERN, "2025-11-25", "2025-06-18"];

/// What a legacy `initialize` gets when the client's own request did not
/// name a revision we support: the newest one that still has the
/// handshake, i.e. `SUPPORTED_VERSIONS[1]`. Kept as a literal rather than
/// an index into the slice so it stays a compile-time constant.
const NEWEST_LEGACY: &str = "2025-11-25";

/// `_meta` keys the specification defines under
/// `io.modelcontextprotocol/`. Written out as constants rather than typed
/// wherever they are used, so a typo in one of these strings is a compile
/// error in exactly one place instead of a request that silently reads as
/// legacy.
const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
const META_SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";

/// Which envelope an answer takes.
///
/// Not stored anywhere — worked out fresh from each message by
/// [`handle`] — because nothing in this crate remembers one message to
/// the next. See the crate doc's "Two eras, on purpose".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Era {
    /// `2026-07-28` and, in principle, whatever supersedes it: stateless,
    /// `_meta`-driven, every result carries `resultType`.
    Modern,
    /// `2025-11-25` and `2025-06-18`: the `initialize` handshake, no
    /// `resultType`, no `_meta.serverInfo` on ordinary results.
    Legacy,
}

/// Who a server says it is, in both eras' `serverInfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Implementation {
    pub name: String,
    pub version: String,
}

/// One tool, as the wire protocol describes it.
///
/// Built by the caller from `everyday-core`'s catalogue — this crate never
/// sees a `Tool` and does not link against `everyday-core`. That is the
/// whole shape of the boundary `docs/plans/mcp.md` draws: the tool table
/// lives in exactly one place, this struct is what it looks like once
/// translated, and nothing here can drift from it because nothing here
/// can see it.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub title: String,
    pub description: String,
    pub input_schema: Value,
    /// `annotations.readOnlyHint`. `Effect::Read` in the catalogue below
    /// this crate.
    pub read_only: bool,
    /// `annotations.destructiveHint`. `Effect::Destructive` in the
    /// catalogue below this crate — and, per `docs/plans/mcp.md`, a tool
    /// this flag is true for should usually not have reached
    /// [`Host::tools`] at all unless the host's own destructive switch is
    /// on.
    pub destructive: bool,
}

/// A failure from the layer below, in the shape `CommandError` already
/// uses (`everyday-service::error::CommandError`) — same two fields, same
/// reason: a caller reacts to `code`, a person reads `message`.
///
/// [`handle`] reads exactly one thing out of `code`: whether it is
/// `"unknown_tool"`. Every other value, including ones this crate has
/// never heard of, is answered as a tool execution error. That is not
/// carelessness — it is the safe default for a code this crate does not
/// recognise. See [`handle`]'s module doc for why the two answers differ
/// and why getting this default wrong in the *other* direction (treating
/// an unrecognised code as a protocol error) would be the worse mistake:
/// a model that cannot read what went wrong cannot correct itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError {
    pub code: String,
    pub message: String,
}

/// Everything [`handle`] needs from the layer that actually knows what a
/// vault is.
///
/// Two methods because that is the whole of what MCP tools need from a
/// host: the list, and running one entry from it. `server_info` and
/// `instructions` are synchronous because a host is expected to have both
/// on hand without asking the vault anything — a name and version are
/// build-time facts, and instructions are static prose plus whatever the
/// host wants to add about *this* vault (see `instructions::full`).
///
/// Written with a hand-desugared `-> impl Future + Send` rather than
/// native `async fn`, and rather than a boxed `dyn Host`: this crate has
/// exactly one implementor per binary (`everyday-server`'s `Arc<Service>`
/// wrapper), so there is nothing a trait object would buy that a generic
/// parameter does not, and no reason to pull in a boxing crate for it.
/// `async fn` in a public trait cannot name auto trait bounds on the
/// future it returns, so nothing would stop an implementation from
/// producing a future that is not `Send` — and the HTTP binding that
/// implements this trait runs inside axum handlers on a multi-threaded
/// tokio runtime, where a future that is not `Send` cannot be spawned.
/// The desugared form says that requirement in the signature itself,
/// where the compiler enforces it on every implementor, rather than
/// leaving it as a fact someone has to already know. Do not "simplify"
/// this back to `async fn` — that would silently drop the bound this
/// trait exists to carry.
pub trait Host {
    /// This server's own name and version, for `serverInfo`.
    fn server_info(&self) -> Implementation;

    /// Anything this host wants said about *this* vault, appended to the
    /// baseline guidance in [`instructions::full`]. `None` when there is
    /// nothing to add.
    fn instructions(&self) -> Option<String>;

    /// The tools this vault can offer right now, in the order a model
    /// should see them.
    ///
    /// An empty `Ok(vec![])` is not an edge case to special-case at the
    /// call site — it is the correct answer for a locked or absent vault,
    /// per `docs/plans/mcp.md`'s "A locked vault offers nothing", and
    /// [`instructions::full`] tells the model as much. `Err` is for a
    /// host-level failure unrelated to any one tool, which `handle`
    /// answers as `-32603` rather than folding into a result no tool
    /// call is happening for a caller to read.
    fn tools(&self) -> impl std::future::Future<Output = Result<Vec<ToolDef>, HostError>> + Send;

    /// Run one tool by name.
    ///
    /// `Err(HostError { code: "unknown_tool", .. })` is read by `handle`
    /// as a protocol error; everything else is a tool execution error.
    /// See the crate doc and [`HostError`] for why, and
    /// `everyday-service::domains::meta::run_tool` for where the
    /// `"unknown_tool"` code itself comes from.
    fn call(
        &self,
        name: &str,
        arguments: &Value,
    ) -> impl std::future::Future<Output = Result<Value, HostError>> + Send;
}

/// What the transport should do with one incoming message.
///
/// This is the whole of what crosses back out of this crate — a
/// transport that only ever matches on `Outcome` never needs to know a
/// JSON-RPC error code, an HTTP status convention, or an `_meta` key, all
/// of which stay inside [`handle`].
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Send this JSON-RPC body with this HTTP status. Covers both
    /// success and every JSON-RPC error this crate raises — the status
    /// is part of the answer precisely because the specification ties
    /// some errors to a status other than `200` (`-32601` to `404`,
    /// `-32602`/`-32022`/`-32020`/`-32021` to `400`), and getting that
    /// pairing right belongs with the rest of the protocol's knowledge.
    Reply { status: u16, body: Value },
    /// A notification we accepted. `HTTP 202`, no body — there is
    /// nothing to say back, and JSON-RPC notifications never get a
    /// response regardless of whether we understood them.
    Accepted,
    /// Open a long-lived notification stream: `subscriptions/listen`'s
    /// answer. `ack` is the first event to send on it —
    /// `notifications/subscriptions/acknowledged` — and `subscription_id`
    /// is what later `notifications/tools/list_changed` events (built by
    /// [`tools_list_changed`]) must be correlated to.
    Listen { subscription_id: String, ack: Value },
}
