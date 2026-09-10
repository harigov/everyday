//! Opening a vault without a password when the machine starts.
//!
//! Off, in a new vault and in every vault that has not been told otherwise.
//! What it costs is stated where the switch is and is worth stating here too:
//! a vault whose key is in the keychain is as safe as the login on that
//! computer, rather than as safe as its password. Somebody who is already at
//! your desk, logged in as you, can read it.
//!
//! What it buys is the only thing that buys it. The assistant's routines run
//! where the vault lives, and a vault that is locked from the moment the
//! machine boots until somebody sits down and types is a vault whose seven
//! o'clock brief does not happen. Everything else about this feature has
//! assumed a process that is already unlocked; this is the one gap left, and
//! it cannot be closed without keeping a key somewhere.
//!
//! # Why the whole key, and why the keychain
//!
//! The key itself, hex-encoded, and nothing on disk. A wrapped key would need
//! its wrapping key somewhere, and "somewhere" on one machine with no person
//! present is a file -- which is the arrangement this is trying not to be.
//! The operating system's own store is the one place on a machine that is
//! meant to hold exactly this, and it is where the device tokens already go.
//!
//! Where there is no keychain -- a Linux session with no secret service, a
//! container -- the switch says so and refuses, rather than writing a key into
//! a settings file. That is the same refusal `remotes.rs` makes about a device
//! token, for a stronger version of the same reason.
//!
//! It lives in this crate, which is the assembly point, because two front ends
//! want it: the desktop app's switch, and `everyday serve --keychain` on a
//! machine under a desk that reboots overnight.

use everyday_core::{Error, Result};
use std::path::Path;

/// The keychain service every entry is filed under. Shared with `remotes.rs`:
/// one application, one service name, distinguished by the account.
const SERVICE: &str = "app.everyday.journal";

/// The account name a vault's key is filed under.
///
/// The path, prefixed, so that two vaults on one machine do not collide and so
/// that an entry says what it is when somebody opens Keychain Access to look.
fn account(vault: &Path) -> String {
    format!("vault-key:{}", vault.display())
}

/// Is this vault set to open itself on this machine?
pub fn enabled(vault: &Path) -> bool {
    keyring::Entry::new(SERVICE, &account(vault))
        .and_then(|e| e.get_password())
        .is_ok_and(|k| !k.is_empty())
}

/// The key, if it is there. `None` covers every failure, deliberately: a
/// keychain that cannot be reached at startup is a lock screen, not an error
/// dialog over a window nobody has read yet.
pub fn recall(vault: &Path) -> Option<String> {
    keyring::Entry::new(SERVICE, &account(vault))
        .ok()?
        .get_password()
        .ok()
        .filter(|k| !k.is_empty())
}

pub fn remember(vault: &Path, key: &str) -> Result<()> {
    let entry = keyring::Entry::new(SERVICE, &account(vault)).map_err(unavailable)?;
    entry.set_password(key).map_err(unavailable)
}

/// Take it out again. Not an error if it was never there.
pub fn forget(vault: &Path) -> Result<()> {
    let Ok(entry) = keyring::Entry::new(SERVICE, &account(vault)) else { return Ok(()) };
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(unavailable(e)),
    }
}

fn unavailable(e: keyring::Error) -> Error {
    Error::Invalid(format!(
        "this computer's keychain could not be reached, and the key to your vault is not \
         something to write into a settings file. ({e})"
    ))
}
