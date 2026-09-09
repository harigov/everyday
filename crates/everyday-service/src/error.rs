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

pub type CommandResult<T> = Result<T, CommandError>;
