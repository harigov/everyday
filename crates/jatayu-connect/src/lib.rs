//! An MCP client: what lets the assistant use a server somebody else wrote.
//!
//! See `docs/plans/connections.md` for the whole feature, and
//! `docs/plans/mcp-protocol-notes.md` for the protocol as this tree reads it
//! (written for the server side; this is the other end of the same wire).
//!
//! # What is here
//!
//! - [`Client`]: one open connection to one server -- `tools/list`,
//!   `tools/call` -- over either [`transport::http`] (Streamable HTTP) or
//!   [`transport::stdio`] (a child process), in either protocol era.
//! - [`oauth`]: the MCP authorization flow -- discovery, dynamic client
//!   registration, PKCE, code exchange and refresh -- as plain functions over
//!   one `reqwest::Client`. Catching the browser's redirect is
//!   `jatayu_mail::oauth::Loopback`'s, and deciding where a token is kept
//!   is the caller's.
//!
//! # What is not
//!
//! No vault, no core, no notion of a "connection": this crate takes an
//! [`Endpoint`] and credentials as values and hands back values. Nothing in
//! it logs a token, a header value or an environment value.

pub mod client;
pub mod command;
pub mod error;
pub mod jsonrpc;
pub mod oauth;
pub mod sse;
pub mod transport;

use std::path::PathBuf;
use std::time::Duration;

pub use client::{Client, ServerInfo, Tool, ToolAnnotations, ToolResult};
pub use error::ConnectError;

/// The newest legacy revision: what `initialize` asks for.
pub const LEGACY_VERSION: &str = "2025-11-25";

/// The modern revision, spoken when a server refuses `initialize` and names
/// it among its supported versions.
pub const MODERN_VERSION: &str = "2026-07-28";

/// Every revision this client can speak, newest first.
pub const SUPPORTED_VERSIONS: [&str; 4] =
    [MODERN_VERSION, LEGACY_VERSION, "2025-06-18", "2025-03-26"];

/// The most characters of a tool's result handed back, with a note saying
/// the rest was cut.
pub const MAX_RESULT_CHARS: usize = 40_000;

/// Where a server is and how to present oneself to it. Holds secrets
/// (`bearer`, header and environment values), so `Debug` prints names only.
#[derive(Clone)]
pub enum Endpoint {
    /// Streamable HTTP at `url`, with `headers` on every request and
    /// `bearer`, if any, as `Authorization: Bearer`.
    Http { url: String, headers: Vec<(String, String)>, bearer: Option<String> },
    /// A program spoken to over its standard streams. `env` is added to a
    /// short list of the variables this process has, not to all of them: see
    /// [`transport::stdio`]'s `is_inherited`.
    Stdio { command: String, args: Vec<String>, env: Vec<(String, String)>, cwd: Option<PathBuf> },
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Endpoint::Http { url, headers, bearer } => f
                .debug_struct("Http")
                .field("url", url)
                .field("headers", &headers.iter().map(|(k, _)| k).collect::<Vec<_>>())
                .field("bearer", &bearer.as_ref().map(|_| "[redacted]"))
                .finish(),
            Endpoint::Stdio { command, args, env, cwd } => f
                .debug_struct("Stdio")
                .field("command", command)
                .field("args", args)
                .field("env", &env.iter().map(|(k, _)| k).collect::<Vec<_>>())
                .field("cwd", cwd)
                .finish(),
        }
    }
}

/// How a [`Client`] introduces itself and how long it waits.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// `clientInfo.name`.
    pub name: String,
    /// `clientInfo.version`.
    pub version: String,
    /// The whole handshake, from opening the connection (or spawning the
    /// program) to the server's answer.
    pub connect_timeout: Duration,
    /// One request's answer.
    pub call_timeout: Duration,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            name: "Jatayu".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            connect_timeout: Duration::from_secs(30),
            call_timeout: Duration::from_secs(120),
        }
    }
}
