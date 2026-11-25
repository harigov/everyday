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
//!
//! # Android
//!
//! The keychain library reaches no store there. What the keychain buys on a
//! desktop -- a token no other program running as this user can read -- is
//! what Android gives every app's private storage: each app runs as a user of
//! its own, and its data directory is readable by that user alone. So on a
//! phone the token goes in a file there (`tokens.json`, beside
//! `remotes.json`), and nowhere a home directory would put it.

use karmayoga_server::client::Connection;
use karmayoga_service::error::{CommandError, CommandResult};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[cfg(not(target_os = "android"))]
/// The keychain entry every token is filed under.
const SERVICE: &str = "app.karmayogi.KarmaYoga";

const FILE: &str = "remotes.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    connections: Vec<Connection>,
}

fn path() -> PathBuf {
    karmayoga_vault::config_dir().join(FILE)
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
        karmayoga_core::fsutil::create_private_dir(parent)
            .map_err(|e| CommandError::new("io", format!("{}: {e}", parent.display())))?;
    }
    let text = serde_json::to_string_pretty(&File { connections: connections.to_vec() })
        .map_err(|e| CommandError::new("internal", e.to_string()))?;
    karmayoga_core::fsutil::write_atomic(&path, text.as_bytes(), "remotes")
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
    if karmayoga_vault::under_test() {
        return Ok(());
    }
    delete_token(id);
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn delete_token(id: &str) {
    for service in [SERVICE, karmayoga_core::legacy::BUNDLE_ID] {
        if let Ok(entry) = keyring::Entry::new(service, id) {
            let _ = entry.delete_credential();
        }
    }
}

#[cfg(not(target_os = "android"))]
pub fn token(id: &str) -> CommandResult<String> {
    refuse_under_test()?;
    let entry = keyring::Entry::new(SERVICE, id).map_err(keychain_unavailable)?;
    match entry.get_password() {
        Ok(token) => Ok(token),
        Err(keyring::Error::NoEntry) => carry_over(id).ok_or_else(|| {
            CommandError::new(
                "unauthorized",
                "the token for this connection is not in your keychain any more. Pair again.",
            )
        }),
        Err(other) => Err(keychain_unavailable(other)),
    }
}

/// A token an Every Day build filed under its own service, filed again under
/// this one and the old entry deleted once the new one is written.
#[cfg(not(target_os = "android"))]
fn carry_over(id: &str) -> Option<String> {
    let old = keyring::Entry::new(karmayoga_core::legacy::BUNDLE_ID, id).ok()?;
    let token = old.get_password().ok().filter(|t| !t.is_empty())?;
    if put_token(id, &token).is_ok() {
        let _ = old.delete_credential();
    }
    Some(token)
}

#[cfg(not(target_os = "android"))]
fn put_token(id: &str, token: &str) -> CommandResult<()> {
    refuse_under_test()?;
    let entry = keyring::Entry::new(SERVICE, id).map_err(keychain_unavailable)?;
    entry.set_password(token).map_err(keychain_unavailable)
}

/// The tokens on a phone, by connection id: the module doc's "Android".
#[cfg(target_os = "android")]
const TOKENS: &str = "tokens.json";

#[cfg(target_os = "android")]
fn tokens() -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(karmayoga_vault::config_dir().join(TOKENS))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

#[cfg(target_os = "android")]
fn write_tokens(tokens: &std::collections::BTreeMap<String, String>) -> CommandResult<()> {
    let dir = karmayoga_vault::config_dir();
    karmayoga_core::fsutil::create_private_dir(&dir)
        .map_err(|e| CommandError::new("io", format!("{}: {e}", dir.display())))?;
    let text =
        serde_json::to_string(tokens).map_err(|e| CommandError::new("internal", e.to_string()))?;
    karmayoga_core::fsutil::write_atomic_private(&dir.join(TOKENS), text.as_bytes(), "tokens")
        .map_err(|e| CommandError::new("io", e.to_string()))
}

#[cfg(target_os = "android")]
pub fn token(id: &str) -> CommandResult<String> {
    refuse_under_test()?;
    tokens().remove(id).ok_or_else(|| {
        CommandError::new("unauthorized", "the token for this connection is gone. Pair again.")
    })
}

#[cfg(target_os = "android")]
fn put_token(id: &str, token: &str) -> CommandResult<()> {
    refuse_under_test()?;
    let mut all = tokens();
    all.insert(id.to_string(), token.to_string());
    write_tokens(&all)
}

#[cfg(target_os = "android")]
fn delete_token(id: &str) {
    let mut all = tokens();
    if all.remove(id).is_some() {
        let _ = write_tokens(&all);
    }
}

/// A test does not reach this computer's keychain; see
/// [`karmayoga_vault::under_test`].
fn refuse_under_test() -> CommandResult<()> {
    if karmayoga_vault::under_test() {
        return Err(CommandError::new(
            "no_keychain",
            "a test may not use this computer's keychain",
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn keychain_unavailable(e: keyring::Error) -> CommandError {
    CommandError::new(
        "no_keychain",
        format!(
            "this computer's keychain could not be reached, and a token for another machine's \
             vault is not something to write into a settings file. ({e})"
        ),
    )
}
