//! Where a client remembers the servers it has paired with.
//!
//! Two halves, kept apart on purpose. The *connection* -- a host, a name, and
//! the certificate to pin -- goes in a file beside the application's other
//! settings, because it is configuration and losing it costs a re-pairing. The
//! *token* goes in the operating system's keychain, because it is a bearer
//! credential to an unlocked vault and a file in a home directory is readable
//! by anything running as that user.
//!
//! # When the keychain is not there
//!
//! A Linux session with no secret service, a container, a machine where the
//! user declined. The honest answer is to say so and refuse to connect rather
//! than quietly writing the token to a file the user thinks is settings: the
//! difference between "your token is in your keychain" and "your token is in
//! `~/.config`" is exactly the thing somebody choosing to self-host cares
//! about, and it must not be decided silently.

use everyday_server::client::Connection;
use everyday_service::error::{CommandError, CommandResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The keychain entry every token is filed under.
const SERVICE: &str = "app.everyday.journal";

const FILE: &str = "remotes.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    connections: Vec<Connection>,
}

fn path() -> PathBuf {
    everyday_vault::config_dir().join(FILE)
}

pub fn list() -> Vec<Connection> {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|text| serde_json::from_str::<File>(&text).ok())
        .map(|f| f.connections)
        .unwrap_or_default()
}

fn write(connections: &[Connection]) -> CommandResult<()> {
    let path = path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CommandError::new("io", format!("{}: {e}", parent.display())))?;
    }
    let text = serde_json::to_string_pretty(&File { connections: connections.to_vec() })
        .map_err(|e| CommandError::new("internal", e.to_string()))?;
    everyday_core::fsutil::write_atomic(&path, text.as_bytes(), "remotes")
        .map_err(|e| CommandError::new("io", e.to_string()))
}

/// Record a pairing: the connection to a file, the token to the keychain.
///
/// The token first. A connection saved without one is a row in the picker that
/// cannot be used, and the reverse -- a token with nothing pointing at it -- is
/// merely an unused keychain entry.
pub fn remember(connection: &Connection, token: &str) -> CommandResult<()> {
    put_token(&connection.id, token)?;
    let mut connections = list();
    connections.retain(|c| c.id != connection.id);
    connections.push(connection.clone());
    write(&connections)
}

pub fn forget(id: &str) -> CommandResult<()> {
    let mut connections = list();
    connections.retain(|c| c.id != id);
    write(&connections)?;
    // Best-effort: the connection is gone from the picker either way, and a
    // token with nothing pointing at it cannot be used from here.
    if let Ok(entry) = keyring::Entry::new(SERVICE, id) {
        let _ = entry.delete_credential();
    }
    Ok(())
}

pub fn token(id: &str) -> CommandResult<String> {
    let entry = keyring::Entry::new(SERVICE, id).map_err(keychain_unavailable)?;
    entry.get_password().map_err(|e| match e {
        keyring::Error::NoEntry => CommandError::new(
            "unauthorized",
            "the token for this connection is not in your keychain any more. Pair again.",
        ),
        other => keychain_unavailable(other),
    })
}

fn put_token(id: &str, token: &str) -> CommandResult<()> {
    let entry = keyring::Entry::new(SERVICE, id).map_err(keychain_unavailable)?;
    entry.set_password(token).map_err(keychain_unavailable)
}

fn keychain_unavailable(e: keyring::Error) -> CommandError {
    CommandError::new(
        "no_keychain",
        format!(
            "this computer's keychain could not be reached, and a token for another machine's \
             vault is not something to write into a settings file. ({e})"
        ),
    )
}
