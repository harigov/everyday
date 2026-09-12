//! The error type every command answers with.
//!
//! Commands reject with `{ code, message }` rather than a bare string, so a
//! caller can react to `locked` or `bad_password` structurally instead of
//! matching on English prose that translation would break. It crosses the
//! JavaScript bridge, and it crosses the wire: `Deserialize` is what lets a
//! remote client hand the interface the same error the server raised, rather
//! than flattening everything a network can do into `unknown`.

use everyday_core::Error;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub code: String,
    pub message: String,
}

impl From<Error> for CommandError {
    fn from(e: Error) -> Self {
        Self { code: e.code().to_string(), message: e.to_string() }
    }
}

impl CommandError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// So that a caller can put this *behind* an error of its own rather than
/// flattening it into one.
///
/// The command line does exactly that: `everyday do` reports a tool that
/// failed as a cause under `error:`, which needs a `source()` to hang it
/// from. Without this it had to `format!` the code and message into a string
/// and pick a variant to carry it, and the variant it picked said "invalid
/// data" over a disk that was full.
impl std::error::Error for CommandError {}

pub type CommandResult<T> = Result<T, CommandError>;

/// Every code a `CommandError` from this crate can carry.
///
/// `CommandError::new` took a bare string literal at each call site, so
/// `everyday-server`'s HTTP status map -- the one other place that has to
/// know every code that exists -- could only cover the ones somebody had
/// remembered to add a case for. `bad_password`, `vault_in_use` and five
/// others were arriving as a bare 500 for want of a line in that match. This
/// is the list both sides now read from: every code [`everyday_core::Error`]
/// produces, by its own [`Error::code`](everyday_core::Error::code), and
/// every code this crate mints of its own. The literals inside this crate
/// still exist -- a constant does not stop meaning what it says -- but they
/// now spell the same word as `ALL`, so a new one is a compile error in the
/// status map's test rather than a silent 500.
pub mod codes {
    // ---- from `everyday_core::Error::code` -------------------------------
    pub const LOCKED: &str = "locked";
    pub const BAD_PASSWORD: &str = "bad_password";
    pub const ALREADY_INITIALISED: &str = "already_initialised";
    pub const NO_VAULT: &str = "no_vault";
    pub const UNSUPPORTED_VERSION: &str = "unsupported_version";
    pub const UNKNOWN_BACKEND: &str = "unknown_backend";
    pub const UNKNOWN_CIPHER: &str = "unknown_cipher";
    pub const NOT_FOUND: &str = "not_found";
    pub const DECRYPT_FAILED: &str = "decrypt_failed";
    pub const UNSUPPORTED: &str = "unsupported";
    pub const VAULT_IN_USE: &str = "vault_in_use";
    pub const CONFLICT: &str = "conflict";
    pub const INVALID: &str = "invalid";
    pub const IO: &str = "io";
    pub const SERDE: &str = "serde";
    pub const BACKEND: &str = "backend";

    // ---- minted by this crate ---------------------------------------------
    pub const AGENT: &str = "agent";
    pub const BUSY: &str = "busy";
    pub const CONFIRM_REQUIRED: &str = "confirm_required";
    pub const FORBIDDEN: &str = "forbidden";
    pub const INTERNAL: &str = "internal";
    pub const NETWORK: &str = "network";
    pub const NOT_AN_IMAGE: &str = "not_an_image";
    pub const PANIC: &str = "panic";
    pub const QUICK: &str = "quick";
    pub const RETRY: &str = "retry";
    pub const TOO_LARGE: &str = "too_large";
    pub const UNKNOWN_COMMAND: &str = "unknown_command";
    pub const UNKNOWN_TOOL: &str = "unknown_tool";
    pub const UNREADABLE: &str = "unreadable";

    /// Every constant above, for a test to check off against a status map or
    /// a match arm rather than trusting that this list and that one were
    /// kept in step by hand.
    pub const ALL: &[&str] = &[
        LOCKED,
        BAD_PASSWORD,
        ALREADY_INITIALISED,
        NO_VAULT,
        UNSUPPORTED_VERSION,
        UNKNOWN_BACKEND,
        UNKNOWN_CIPHER,
        NOT_FOUND,
        DECRYPT_FAILED,
        UNSUPPORTED,
        VAULT_IN_USE,
        CONFLICT,
        INVALID,
        IO,
        SERDE,
        BACKEND,
        AGENT,
        BUSY,
        CONFIRM_REQUIRED,
        FORBIDDEN,
        INTERNAL,
        NETWORK,
        NOT_AN_IMAGE,
        PANIC,
        QUICK,
        RETRY,
        TOO_LARGE,
        UNKNOWN_COMMAND,
        UNKNOWN_TOOL,
        UNREADABLE,
    ];
}
