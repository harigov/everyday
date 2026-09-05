//! The error type crossing the JavaScript bridge.
//!
//! Commands reject with `{ code, message }` rather than a bare string, so the
//! interface can react to `locked` or `bad_password` structurally instead of
//! matching on English prose that translation would break.

use everyday_core::Error;
use serde::Serialize;

#[derive(Debug, Serialize)]
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
