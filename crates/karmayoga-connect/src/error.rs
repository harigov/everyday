//! Every way talking to a server can fail, sorted by what the caller does
//! about it.

/// Why a request to a server did not produce an answer.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConnectError {
    /// The server wants credentials it was not given, or refused the ones it
    /// was (HTTP 401, or 403 with `insufficient_scope`). `resource_metadata`
    /// and `scope` are what its `WWW-Authenticate` named, for the sign-in to
    /// start from.
    #[error("the server wants you to sign in")]
    Unauthorized { resource_metadata: Option<String>, scope: Option<String> },
    /// The program could not be started: not found, not executable.
    #[error("could not start the server: {0}")]
    Spawn(String),
    /// The connection failed or the server answered with something that is
    /// not HTTP success: refused, reset, TLS, a 5xx. The text says which,
    /// with nothing secret in it.
    #[error("{0}")]
    Transport(String),
    /// The server answered, but not in MCP: malformed JSON, a reply to a
    /// request nobody sent, a missing field.
    #[error("the server answered in a way this client does not understand: {0}")]
    Protocol(String),
    /// A JSON-RPC error the server returned.
    #[error("the server refused: {message} ({code})")]
    Rpc { code: i64, message: String, data: Option<serde_json::Value> },
    #[error("the server did not answer in time")]
    Timeout,
    /// The connection is gone: the program exited, or the client was closed.
    /// `detail` is the end of what the program wrote to stderr, if anything.
    #[error("the connection closed{}", detail.as_ref().map(|d| format!(": {d}")).unwrap_or_default())]
    Closed { detail: Option<String> },
}

impl ConnectError {
    /// Whether opening a fresh connection and trying again could help: a
    /// dropped connection or an expired session, but not a refusal.
    pub fn is_retryable(&self) -> bool {
        matches!(self, ConnectError::Closed { .. } | ConnectError::Transport(_))
    }
}
