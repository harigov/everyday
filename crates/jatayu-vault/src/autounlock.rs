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
//! want it: the desktop app's switch, and `jatayu serve --keychain` on a
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

use crate::legacy::Folders;
use jatayu_core::crypto::KeyText;
use jatayu_core::{Error, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The keychain service every entry is filed under. Shared with `remotes.rs`:
/// one application, one service name, distinguished by the account.
const SERVICE: &str = "app.jatayu.Jatayu";

/// The account name a vault's key is filed under.
///
/// The path, prefixed, so that two vaults on one machine do not collide and so
/// that an entry says what it is when somebody opens Keychain Access to look.
///
/// Canonicalised first, because the two callers reach the same vault by
/// different names: the desktop app holds an absolute path, and
/// `jatayu --vault ./Journal` holds whatever was typed. Without this the
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
    looked_up(service, account).ok().flatten()
}

/// [`stored`], telling "nothing filed there" (`Ok(None)`) apart from a
/// keychain that did not answer (`Err`): only the first is worth
/// remembering -- see [`carry_over`].
fn looked_up(service: &str, account: &str) -> std::result::Result<Option<String>, ()> {
    match keyring::Entry::new(service, account).and_then(|e| e.get_password()) {
        Ok(key) => Ok(Some(key).filter(|k| !k.is_empty())),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err(()),
    }
}

/// Where the vaults whose earlier entries have all been looked through, and
/// held nothing, are noted, one account per line. See [`carry_over`].
const SEARCHED_FILENAME: &str = "keychain-searched";

/// The same, for this process: the file's lines once read, and every vault
/// searched since, whether or not it went into the file.
static SEARCHED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn searched_file() -> PathBuf {
    crate::config_dir().join(SEARCHED_FILENAME)
}

fn read_searched(file: &Path) -> HashSet<String> {
    std::fs::read_to_string(file)
        .map(|text| text.lines().filter(|l| !l.is_empty()).map(str::to_string).collect())
        .unwrap_or_default()
}

/// Whether `account`'s earlier entries are known to hold nothing.
fn already_searched(account: &str) -> bool {
    let mut searched = SEARCHED.lock().unwrap_or_else(|p| p.into_inner());
    searched.get_or_insert_with(|| read_searched(&searched_file())).contains(account)
}

/// Note that `account`'s earlier entries hold nothing: for this process, and
/// in the file too when `keep`.
fn note_searched(account: &str, keep: bool) {
    let mut searched = SEARCHED.lock().unwrap_or_else(|p| p.into_inner());
    searched.get_or_insert_with(|| read_searched(&searched_file())).insert(account.to_string());
    drop(searched);
    if keep {
        add_searched(&searched_file(), account);
    }
}

/// Add `account` to the file, best effort: a file that cannot be written
/// only means the next start looks again.
fn add_searched(file: &Path, account: &str) {
    if account.contains('\n') {
        return;
    }
    let mut lines: Vec<String> = read_searched(file).into_iter().collect();
    if lines.iter().any(|l| l == account) {
        return;
    }
    lines.push(account.to_string());
    lines.sort();
    let mut text = lines.join("\n");
    text.push('\n');
    if let Err(e) = jatayu_core::fsutil::write_atomic(file, text.as_bytes(), "keychain") {
        tracing::debug!(error = %e, "could not note the keychain's earlier entries as searched");
    }
}

/// Every (service, account) this vault's key may have been filed under
/// before now, other than the one it belongs under today, in the order they
/// are tried.
///
/// A build under an earlier name filed it under that name's bundle id
/// ([`jatayu_core::legacy::Former::bundle_id`]), by the vault's path; those
/// come first, for a vault that has not moved. The default vault's path
/// changed with each rename -- its folder moved -- so for it the path it had
/// under each earlier name is tried too, in each spelling it may have been
/// canonicalised to: as the platform names it, and with its parent resolved
/// (a home folder reached through a link, Windows' `\\?\` form). Under this
/// service first, where this build filed a key while the folder's move was
/// put off, then under each earlier name's, newest first -- Karma Yoga's
/// service by Karma Yoga's folder is where a machine that ran Karma Yoga has
/// it.
fn earlier_entries(vault: &Path) -> Vec<(&'static str, String)> {
    earlier_entries_in(vault, crate::legacy::Folders::of_this_user().as_ref())
}

/// [`earlier_entries`], over explicit folders so tests can give it their own.
fn earlier_entries_in(vault: &Path, folders: Option<&Folders>) -> Vec<(&'static str, String)> {
    let old_services: Vec<&'static str> = jatayu_core::legacy::FORMERS
        .iter()
        .map(|former| former.bundle_id)
        .filter(|service| !service.is_empty())
        .collect();
    let current = account(vault);
    let mut entries: Vec<(&'static str, String)> =
        old_services.iter().map(|service| (*service, current.clone())).collect();
    if let Some(folders) = folders {
        let here = vault.canonicalize().unwrap_or_else(|_| vault.to_path_buf());
        let old_paths: Vec<PathBuf> =
            folders.old.iter().flat_map(|old| old_spellings(&old.data)).collect();
        let new = &folders.new_data;
        let is_default =
            here == new.canonicalize().unwrap_or_else(|_| new.clone()) || old_paths.contains(&here);
        if is_default {
            for service in std::iter::once(SERVICE).chain(old_services.iter().copied()) {
                for path in &old_paths {
                    entries.push((service, format!("vault-key:{}", path.display())));
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    entries.retain(|(service, account)| {
        !(*service == SERVICE && *account == current) && seen.insert((*service, account.clone()))
    });
    entries
}

/// The ways an old data folder's path may have been written down.
fn old_spellings(old: &Path) -> Vec<PathBuf> {
    let mut paths = vec![old.to_path_buf()];
    if !is_link(old)
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

fn is_link(path: &Path) -> bool {
    path.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink())
}

/// Is the vault still in an earlier name's data folder, that folder's move
/// put off? Then a key found under an earlier entry is used as it is, and
/// filed again only once the vault is where it is going to stay. A folder
/// that is a link -- the one a move leaves, or one an earlier rename left --
/// has moved already, whatever path the vault was reached by.
fn move_put_off(vault: &Path) -> bool {
    move_put_off_in(vault, crate::legacy::Folders::of_this_user().as_ref())
}

fn move_put_off_in(vault: &Path, folders: Option<&Folders>) -> bool {
    let Some(folders) = folders else { return false };
    let here = vault.canonicalize().unwrap_or_else(|_| vault.to_path_buf());
    folders.old.iter().any(|old| {
        !is_link(&old.data) && old_spellings(&old.data).iter().any(|path| here.starts_with(path))
    })
}

/// A key filed under an earlier entry: filed again under this application's
/// own service and the vault's current path, and the earlier entry deleted
/// once the new one is written -- unless the vault's own move is still put
/// off. `None` when there is none.
///
/// That is ten or so keychain lookups for the default vault, each a new
/// D-Bus connection on Linux, and with the switch off it was every start,
/// every window and every unlock. So a search that found nothing, with
/// every lookup answered, is remembered for the process -- and, once the
/// vault is where it is going to stay, in the settings folder, so later
/// starts skip it too. A keychain that did not answer is not remembered: a
/// key it was hiding is still to be found.
fn carry_over(vault: &Path) -> Option<String> {
    let current = account(vault);
    if already_searched(&current) {
        return None;
    }
    let mut answered = true;
    for (service, earlier) in earlier_entries(vault) {
        let key = match looked_up(service, &earlier) {
            Ok(Some(key)) => key,
            Ok(None) => continue,
            Err(()) => {
                answered = false;
                continue;
            }
        };
        if move_put_off(vault) {
            return Some(key);
        }
        let filed = keyring::Entry::new(SERVICE, &current)
            .and_then(|entry| entry.set_password(&key))
            .is_ok();
        if filed && let Ok(entry) = keyring::Entry::new(service, &earlier) {
            let _ = entry.delete_credential();
        }
        return Some(key);
    }
    if answered {
        note_searched(&current, !move_put_off(vault));
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

// Unix only, as the folder tests it borrows its layouts from are.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::legacy::tests::{folders, owners_install};
    use crate::legacy::{OldFolders, migrate};
    use jatayu_core::legacy::{EVERY_DAY, KARMA_YOGA};

    fn key(path: &Path) -> String {
        format!("vault-key:{}", path.display())
    }

    fn canonical(path: &Path) -> PathBuf {
        path.canonicalize().unwrap()
    }

    fn position(entries: &[(&str, String)], service: &str, account: &str) -> usize {
        entries
            .iter()
            .position(|(s, a)| *s == service && a == account)
            .unwrap_or_else(|| panic!("no ({service}, {account}) in {entries:?}"))
    }

    #[test]
    fn the_owners_key_is_found_under_karma_yogas_service_by_karma_yogas_folder() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        owners_install(&f);
        let (ky, ed): (&OldFolders, &OldFolders) = (&f.old[0], &f.old[1]);

        // While the folders' move is put off, the vault is opened where it
        // is, and the key found by that path is used without filing it again.
        let entries = earlier_entries_in(&ky.data, Some(&f));
        assert_eq!(entries[0], (KARMA_YOGA.bundle_id, key(&canonical(&ky.data))));
        assert!(move_put_off_in(&ky.data, Some(&f)));

        migrate(&f, &|_| None, false);
        let entries = earlier_entries_in(&f.new_data, Some(&f));
        // Where the owner's machine has it, and where an Every Day build
        // left one, by the path each folder had.
        let ky_at_ky = position(&entries, KARMA_YOGA.bundle_id, &key(&ky.data));
        let ed_at_ed = position(&entries, EVERY_DAY.bundle_id, &key(&ed.data));
        assert!(ky_at_ky < ed_at_ed, "Karma Yoga's service is tried before Every Day's");
        // One this build filed by the old path while the move was put off.
        position(&entries, SERVICE, &key(&ky.data));
        position(&entries, SERVICE, &key(&ed.data));
        // Never the entry it is going to be filed under.
        assert!(!entries.contains(&(SERVICE, account(&f.new_data))), "{entries:?}");
        // The folders are links now: the vault has moved, and a key found
        // is filed again under the new path.
        assert!(!move_put_off_in(&f.new_data, Some(&f)));
        assert!(!move_put_off_in(&ky.data, Some(&f)));
    }

    #[test]
    fn the_searched_file_keeps_each_vault_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(SEARCHED_FILENAME);
        assert!(read_searched(&file).is_empty(), "no file is nothing searched");
        add_searched(&file, "vault-key:/b");
        add_searched(&file, "vault-key:/a");
        add_searched(&file, "vault-key:/b");
        add_searched(&file, "vault-key:/odd\nname");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "vault-key:/a\nvault-key:/b\n");
        assert_eq!(
            read_searched(&file),
            HashSet::from(["vault-key:/a".to_string(), "vault-key:/b".to_string()])
        );
    }

    #[test]
    fn a_vault_elsewhere_is_looked_for_by_its_own_path_only() {
        let dir = tempfile::tempdir().unwrap();
        let f = folders(dir.path());
        owners_install(&f);
        let elsewhere = dir.path().join("external/Journal");
        std::fs::create_dir_all(&elsewhere).unwrap();

        let entries = earlier_entries_in(&elsewhere, Some(&f));
        let here = key(&canonical(&elsewhere));
        assert_eq!(
            entries,
            [(KARMA_YOGA.bundle_id, here.clone()), (EVERY_DAY.bundle_id, here)],
            "an earlier name's service by the vault's own path, newest first"
        );
        assert!(!move_put_off_in(&elsewhere, Some(&f)));
    }
}
