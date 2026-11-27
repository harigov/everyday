//! The two ways to reach a server. Internal to this crate: [`crate::Client`]
//! is the only public way in.
//!
//! A transport moves JSON-RPC messages and knows nothing of what they mean,
//! with two exceptions that live on the wire rather than in the protocol:
//! the session id and protocol-version headers of Streamable HTTP, and the
//! `tools/list_changed` notification, which both carry and which the client
//! reads as a flag.

pub mod http;
pub mod stdio;

use std::sync::{Mutex, MutexGuard};

use serde_json::Value;

use crate::{ClientOptions, ConnectError, Endpoint};

/// Why a request produced no result: either an ordinary failure, or the
/// server forgot the session the request carried and the client may start a
/// new one and ask again.
#[derive(Debug)]
pub enum Failure {
    /// The server said 404 to a request that carried this session id.
    SessionExpired(String),
    Error(ConnectError),
}

impl From<ConnectError> for Failure {
    fn from(e: ConnectError) -> Self {
        Failure::Error(e)
    }
}

impl Failure {
    /// The error a caller sees when it will not (or cannot) start a new
    /// session.
    pub fn into_error(self) -> ConnectError {
        match self {
            Failure::SessionExpired(_) => ConnectError::Transport(
                "the server ended the session and would not start another".into(),
            ),
            Failure::Error(e) => e,
        }
    }
}

/// A lock that a panicking holder cannot poison for everybody else.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One open way of talking to a server.
pub enum Transport {
    Http(Box<http::Http>),
    Stdio(Box<stdio::Stdio>),
}

impl Transport {
    /// Reach the server named by `endpoint`: build an HTTP client, or spawn
    /// the program. Nothing is sent yet.
    pub fn open(endpoint: Endpoint, options: &ClientOptions) -> Result<Transport, ConnectError> {
        match endpoint {
            Endpoint::Http { url, headers, bearer } => Ok(Transport::Http(Box::new(
                http::Http::new(url, headers, bearer, options.connect_timeout)?,
            ))),
            Endpoint::Stdio { command, args, env, cwd } => Ok(Transport::Stdio(Box::new(
                stdio::Stdio::spawn(&command, &args, &env, cwd.as_deref())?,
            ))),
        }
    }

    /// Send a request and wait for its result. A JSON-RPC error is an
    /// [`ConnectError::Rpc`].
    pub async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, Failure> {
        match self {
            Transport::Http(t) => t.request(method, params).await,
            Transport::Stdio(t) => t.request(method, params).await,
        }
    }

    /// Send a notification.
    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), ConnectError> {
        match self {
            Transport::Http(t) => t.notify(method, params).await.map_err(Failure::into_error),
            Transport::Stdio(t) => t.notify(method, params),
        }
    }

    /// Say which revision was agreed, and whether it is the stateless one.
    /// Over HTTP this decides the headers every later request carries.
    pub fn use_protocol(&self, version: &str, modern: bool) {
        if let Transport::Http(t) = self {
            t.use_protocol(version, modern);
        }
    }

    /// The session the server minted, if it did.
    pub fn session_id(&self) -> Option<String> {
        match self {
            Transport::Http(t) => t.session_id(),
            Transport::Stdio(_) => None,
        }
    }

    /// Forget the session and the agreed revision, ready for a new
    /// `initialize`.
    pub fn reset_session(&self) {
        if let Transport::Http(t) = self {
            t.reset_session();
        }
    }

    /// Whether the server announced new tools since the flag was last taken.
    pub fn tools_changed(&self) -> bool {
        match self {
            Transport::Http(t) => t.tools_changed(),
            Transport::Stdio(t) => t.tools_changed(),
        }
    }

    /// Read the changed-tools flag and clear it.
    pub fn take_tools_changed(&self) -> bool {
        match self {
            Transport::Http(t) => t.take_tools_changed(),
            Transport::Stdio(t) => t.take_tools_changed(),
        }
    }

    pub fn is_alive(&self) -> bool {
        match self {
            Transport::Http(t) => t.is_alive(),
            Transport::Stdio(t) => t.is_alive(),
        }
    }

    /// Replace the bearer token (HTTP only).
    pub fn set_bearer(&self, token: Option<String>) {
        if let Transport::Http(t) = self {
            t.set_bearer(token);
        }
    }

    pub async fn close(&self) {
        match self {
            Transport::Http(t) => t.close().await,
            Transport::Stdio(t) => t.close().await,
        }
    }
}
