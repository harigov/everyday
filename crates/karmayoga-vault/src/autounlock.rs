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
//! want it: the desktop app's switch, and `karmayoga serve --keychain` on a
//! machine under a desk that reboots overnight.
//!
//! # The idle timers, once this is on
//!
//! Signing in to this computer already is unlocking this vault, so neither
//! idle clock has anything left to protect: the window's own screen timer
//! would only hide what the OS login already reopens, and the key's own
//! idle timeout (`forget_key_seconds`) would only make the assistant's
//! routines wait for somebody to notice and type a password a reboot was
//! going to hand back anyway. Both are suspended while this is on --
//! `Vault::set_key_in_keychain` is the cached bit a caller here sets once it
//! knows the key actually unlocked the vault, and
//! `Vault::seconds_until_forget_key` reads it, rather than this module ever
//! being asked on the five-second poll that watches for idleness. The
//! stored timeouts are untouched either way, so turning the switch back off
//! brings them straight back. Locking the vault by hand -- Ctrl/Cmd+L, the
//! Lock button, the tray -- is unaffected either way: none of those go
//! through this flag at all.

use karmayoga_core::crypto::KeyText;
use karmayoga_core::{Error, Result};
use std::path::Path;

/// The keychain service every entry is filed under. Shared with `remotes.rs`:
/// one application, one service name, distinguished by the account.
const SERVICE: &str = "app.karmayogi.KarmaYoga";

/// The account name a vault's key is filed under.
///
/// The path, prefixed, so that two vaults on one machine do not collide and so
/// that an entry says what it is when somebody opens Keychain Access to look.
///
/// Canonicalised first, because the two callers reach the same vault by
/// different names: the desktop app holds an absolute path, and
/// `karmayoga --vault ./Journal` holds whatever was typed. Without this the
/// key stored by one is invisible to the other, and the failure is silent --
/// a machine that boots, finds no key, stays locked, and does not run the
/// seven o'clock brief the whole feature exists for.
fn account(vault: &Path) -> String {
    let path = vault.canonicalize().unwrap_or_else(|_| vault.to_path_buf());
    format!("vault-key:{}", path.display())
}

/// Is this vault set to open itself on this machine?
pub fn enabled(vault: &Path) -> bool {
    if crate::under_test() {
        return false;
    }
    stored(SERVICE, &account(vault)).is_some() || carry_over(vault).is_some()
}

fn stored(service: &str, account: &str) -> Option<String> {
    keyring::Entry::new(service, account)
        .and_then(|e| e.get_password())
        .ok()
        .filter(|k| !k.is_empty())
}

/// Every (service, account) this vault's key may have been filed under
/// before now, other than the one it belongs under today.
///
/// An Every Day build filed it under the old service, by the vault's path.
/// The default vault's path changed with the rename -- its folder moved --
/// so for it the path it had before is tried too, in each spelling it may
/// have been canonicalised to: as the platform names it, and with its parent
/// resolved (a home folder reached through a link, Windows' `\\?\` form).
/// And a key this build stored while that move was put off is under this
/// service by the old path.
fn earlier_entries(vault: &Path) -> Vec<(&'static str, String)> {
    let old_service = karmayoga_core::legacy::BUNDLE_ID;
    let mut entries = vec![(old_service, account(vault))];
    if let Some(folders) = crate::legacy::Folders::of_this_user() {
        let here = vault.canonicalize().unwrap_or_else(|_| vault.to_path_buf());
        let old_paths = old_spellings(&folders.old_data);
        let is_default = here == folders.new_data.canonicalize().unwrap_or(folders.new_data)
            || old_paths.contains(&here);
        if is_default {
            for path in old_paths {
                let old_account = format!("vault-key:{}", path.display());
                for service in [old_service, SERVICE] {
                    entries.push((service, old_account.clone()));
                }
            }
        }
    }
    let current = account(vault);
    let mut seen = std::collections::HashSet::new();
    entries.retain(|(service, account)| {
        !(*service == SERVICE && *account == current) && seen.insert((*service, account.clone()))
    });
    entries
}

/// The ways the old data folder's path may have been written down.
fn old_spellings(old: &Path) -> Vec<std::path::PathBuf> {
    let mut paths = vec![old.to_path_buf()];
    if !old.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink())
        && let Ok(real) = old.canonicalize()
    {
        paths.push(real);
    }
    if let (Some(parent), Some(name)) = (old.parent(), old.file_name())
        && let Ok(parent) = parent.canonicalize()
    {
        paths.push(parent.join(name));
    }
    paths.dedup();
    paths
}

/// Is the vault still in the old data folder, its move put off? Then a key
/// found under an earlier entry is used as it is, and filed again only once
/// the vault is where it is going to stay.
fn move_put_off(vault: &Path) -> bool {
    let Some(folders) = crate::legacy::Folders::of_this_user() else { return false };
    let is_link = folders.old_data.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink());
    let here = vault.canonicalize().unwrap_or_else(|_| vault.to_path_buf());
    !is_link && old_spellings(&folders.old_data).iter().any(|old| here.starts_with(old))
}

/// A key filed under an earlier entry: filed again under this application's
/// own service and the vault's current path, and the earlier entry deleted
/// once the new one is written -- unless the vault's own move is still put
/// off. `None` when there is none.
fn carry_over(vault: &Path) -> Option<String> {
    for (service, earlier) in earlier_entries(vault) {
        let Some(key) = stored(service, &earlier) else { continue };
        if move_put_off(vault) {
            return Some(key);
        }
        let filed = keyring::Entry::new(SERVICE, &account(vault))
            .and_then(|entry| entry.set_password(&key))
            .is_ok();
        if filed && let Ok(entry) = keyring::Entry::new(service, &earlier) {
            let _ = entry.delete_credential();
        }
        return Some(key);
    }
    None
}

/// The key, if it is there. `None` covers every failure, deliberately: a
/// keychain that cannot be reached at startup is a lock screen, not an error
/// dialog over a window nobody has read yet.
///
/// Wrapped in [`KeyText`] rather than handed back as a `String`, so the copy
/// this crate is responsible for is wiped when the caller drops it.
pub fn recall(vault: &Path) -> Option<KeyText> {
    if crate::under_test() {
        return None;
    }
    let text = stored(SERVICE, &account(vault)).or_else(|| carry_over(vault))?;
    Some(KeyText::new(text))
}

pub fn remember(vault: &Path, key: &KeyText) -> Result<()> {
    if crate::under_test() {
        return Err(Error::Invalid("a test may not write to this computer's keychain".into()));
    }
    let entry = keyring::Entry::new(SERVICE, &account(vault)).map_err(unavailable)?;
    entry.set_password(key.as_str()).map_err(unavailable)
}

/// Take it out again. Not an error if it was never there.
pub fn forget(vault: &Path) -> Result<()> {
    if crate::under_test() {
        return Ok(());
    }
    // Any earlier copy is forgotten too: turning the switch off must not
    // leave a key behind under the old name.
    for (service, earlier) in earlier_entries(vault) {
        if let Ok(entry) = keyring::Entry::new(service, &earlier) {
            let _ = entry.delete_credential();
        }
    }
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
