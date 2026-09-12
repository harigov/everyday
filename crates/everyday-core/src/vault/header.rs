//! The vault's header: the one plaintext file that says how to get in.
//!
//! [`VaultHeader`] is what [`super::Vault::create`] writes and
//! [`super::Vault::open`] reads before anything else, and everything else in
//! this module exists to keep that file honest: durable writes with a spare
//! copy, sealing the pieces that must not be read without the data key, and
//! the hex encoding those sealed pieces are stored in.

use super::{HEADER_BACKUP_FILENAME, HEADER_FILENAME, STORE_DIRNAME};
use crate::crypto::{AeadCipher, Cipher, KdfParams, NullCipher, SUITE_NONE, SecretKey};
use crate::error::{Error, Result};
use crate::fsutil;
use crate::store::BackendSettings;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

/// Persisted, unencrypted vault metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultHeader {
    pub format: u32,
    /// Display name for the vault.
    pub name: String,
    /// Storage backend id, e.g. `"sqlite"`.
    pub backend: String,
    /// Cipher suite id, or `"none"` for an unencrypted vault.
    pub cipher: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kdf: Option<KdfParams>,
    /// Hex-encoded Argon2 salt. Absent on unencrypted vaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salt: Option<String>,
    /// Hex-encoded data key, sealed under the password-derived key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_key: Option<String>,
    /// Hex-encoded [`BackendSettings`], sealed under the *data* key.
    ///
    /// Sealed rather than stored plainly because of what tends to be in it:
    /// a connection URL with a password. Absent on a backend that needs no
    /// settings, which is every file-backed one.
    ///
    /// Sealing it here is also what makes the ordering work. The store is
    /// only ever opened from [`Vault::activate`], which already holds the
    /// data key, so nothing needs these before there is a key to open them
    /// with. On an unencrypted vault the cipher is a no-op and this is hex
    /// of cleartext -- honest, and one more thing "no password" costs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_settings: Option<String>,
    pub created_at: Timestamp,
    /// Seconds of inactivity before a *client* hides what it is showing and
    /// asks for the password again. 0 disables.
    ///
    /// This is a screen timeout, not a key timeout: it is enforced by each
    /// client on its own clock, and the vault it is looking at stays open.
    /// See [`VaultHeader::forget_key_seconds`] for the other one.
    #[serde(default)]
    pub auto_lock_seconds: u64,
    /// Set once the old single lock timeout has been carried into
    /// [`VaultHeader::forget_key_seconds`], so that somebody who afterwards
    /// chooses "never" is not overruled the next time the vault is opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrated_lock: Option<bool>,
    /// A known plaintext sealed under the *data* key, so that a key offered
    /// without a password can be checked before it is trusted.
    ///
    /// It exists because of one specific way to lose everything. Nothing else
    /// in this file can tell a wrong data key from a right one: the wrapped
    /// key checks a *password*, and a key that arrives already unwrapped --
    /// from the keychain, when a vault opens itself -- is checked only by
    /// failing to decrypt some record. A vault with no records yet decrypts
    /// nothing, so a stale key would open it, every write of that session
    /// would be sealed under a key the header does not hold, and the right
    /// password would afterwards open a vault it could not read a line of.
    ///
    /// Absent on vaults written before this existed, and on unencrypted ones.
    /// [`Vault::unlock`] writes it the first time such a vault is opened with
    /// a password, so it appears without anybody being asked for anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_check: Option<String>,
    /// Seconds of inactivity before the *key* is dropped. 0 disables, and
    /// that is the default.
    ///
    /// The distinction matters because of what a vault is now. A machine
    /// holding one serves it -- to its own window, to a phone, and to the
    /// assistant's own scheduler -- so the key has to outlive any one
    /// window's screen going dark. What ends it is quitting, locking the
    /// vault deliberately, or this.
    #[serde(default)]
    pub forget_key_seconds: u64,
}

impl VaultHeader {
    pub fn is_encrypted(&self) -> bool {
        self.cipher != SUITE_NONE
    }
}

// ---- header i/o ---------------------------------------------------------

/// Read the header, falling back to the backup copy if the live one is
/// unreadable.
///
/// This is the single most valuable file in the vault: it holds the wrapped
/// data key, which exists nowhere else. If it is lost, every entry in the
/// store is ciphertext under a key nobody can derive any more -- so a
/// header that fails to parse is worth a second look at the spare before
/// reporting that there is no vault here.
pub(super) fn read_header(root: &Path) -> Result<VaultHeader> {
    let path = root.join(HEADER_FILENAME);
    let backup = root.join(HEADER_BACKUP_FILENAME);

    let live = match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<VaultHeader>(&bytes) {
            Ok(header) => return Ok(header),
            Err(e) => Some(Error::Serde(e)),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(Error::io(&path, e)),
    };

    // Either there is no live header or it did not parse. Try the spare.
    if let Ok(bytes) = std::fs::read(&backup)
        && let Ok(header) = serde_json::from_slice::<VaultHeader>(&bytes)
    {
        tracing::warn!(
            path = %path.display(),
            "vault header was unreadable; recovered it from the backup copy"
        );
        // Put the recovered copy back so the next open does not have to.
        let _ = write_header(root, &header);
        return Ok(header);
    }

    match live {
        // A header that exists but is corrupt is a different problem from no
        // vault at all, and saying so is the difference between "you picked
        // the wrong folder" and "restore your backup".
        Some(e) => Err(e),
        None => Err(Error::NoVault(root.to_path_buf())),
    }
}

/// Write the header durably, keeping the previous one as a spare.
///
/// Two things beyond a plain write, both because of what this file holds:
///
/// * The write goes through [`fsutil::write_atomic`], which fsyncs the bytes
///   before the rename and the directory after it. A rename alone is atomic
///   for readers but says nothing about a power cut, and the failure mode
///   here is not a lost setting -- it is a vault whose data key is gone.
///
/// * The header it replaces is copied to `vault.json.bak` first, so there is
///   always a second copy of a *working* wrapped key on disk. `change_password`
///   in particular rewrites the salt and the wrapped key together; without the
///   spare, that one write is the whole vault's single point of failure.
pub(super) fn write_header(root: &Path, header: &VaultHeader) -> Result<()> {
    let path = root.join(HEADER_FILENAME);
    let backup = root.join(HEADER_BACKUP_FILENAME);

    // Only ever promote a header we can actually read back -- copying a
    // corrupt live file over the last good spare would defeat the point.
    if let Ok(bytes) = std::fs::read(&path)
        && serde_json::from_slice::<VaultHeader>(&bytes).is_ok()
    {
        let _ = fsutil::write_atomic(&backup, &bytes, &fsutil::unique_tag());
    }

    let json = serde_json::to_vec_pretty(header)?;
    fsutil::write_atomic(&path, &json, &fsutil::unique_tag())?;

    // Seed the spare on the very first write, rather than waiting for a
    // second one to promote this header into it. Otherwise a vault created
    // and then not reconfigured -- which is most of them -- runs on a single
    // copy of its data key for as long as nobody changes a setting, and
    // that is precisely the window the spare exists to cover.
    if !backup.is_file() {
        let _ = fsutil::write_atomic(&backup, &json, &fsutil::unique_tag());
    }
    Ok(())
}

/// Undo a [`Vault::create`](super::Vault::create) that got as far as the
/// header and no further.
///
/// Best effort, and deliberately narrow: only the files `create` itself
/// writes. `create` refused an existing vault before any of them, so none can
/// belong to anybody else -- but `root` may well have existed already, and
/// the store directory may have had something in it before today, so both are
/// removed only by the non-recursive call that refuses when they are not
/// empty. Leaving a stray directory behind is a much smaller failure than
/// deleting one, and either way the header is gone, which is the part that
/// decides whether the retry is allowed.
pub(super) fn remove_partial_vault(root: &Path, held_the_lock: bool) {
    let _ = std::fs::remove_file(root.join(HEADER_FILENAME));
    let _ = std::fs::remove_file(root.join(HEADER_BACKUP_FILENAME));
    if held_the_lock {
        let _ = std::fs::remove_file(root.join(crate::lockfile::LOCK_FILENAME));
    }
    let _ = std::fs::remove_dir(root.join(STORE_DIRNAME));
    let _ = std::fs::remove_dir(root);
}

/// Associated data for the sealed backend settings.
///
/// Distinct from every record AAD in the store, so a sealed connection URL
/// cannot be pasted into an entry's `data` column and decrypt there.
const SETTINGS_AAD: &[u8] = b"everyday.backend-settings.v1";

/// The cipher a data key implies. `None` -- an unencrypted vault -- gets the
/// no-op one, which is the same substitution [`Vault::activate`](super::Vault::activate) makes.
pub(super) fn cipher_for(dek: Option<&SecretKey>) -> Arc<dyn Cipher> {
    match dek {
        Some(k) => Arc::new(AeadCipher::new(k)),
        None => Arc::new(NullCipher),
    }
}

/// Seal `settings` for the header, or `None` if there is nothing to seal.
///
/// Empty settings are stored as an absent field rather than as the sealed
/// bytes of `{}`, so a SQLite vault's header looks exactly as it always did.
pub(super) fn seal_settings(
    cipher: &Arc<dyn Cipher>,
    settings: &BackendSettings,
) -> Result<Option<String>> {
    if settings.is_empty() {
        return Ok(None);
    }
    let sealed = cipher.seal(SETTINGS_AAD, &serde_json::to_vec(settings)?)?;
    Ok(Some(to_hex(&sealed)))
}

/// Recover what [`seal_settings`] wrote.
pub(super) fn open_settings(cipher: &dyn Cipher, sealed: Option<&str>) -> Result<BackendSettings> {
    let Some(sealed) = sealed else { return Ok(BackendSettings::default()) };
    let plain = cipher.open(SETTINGS_AAD, &from_hex(sealed)?)?;
    Ok(serde_json::from_slice(&plain)?)
}

/// What a key-check value seals, and the label it is sealed under.
///
/// Constant and public knowledge, which is the point: an attacker already
/// knows the plaintext, and the thing being tested is whether a candidate key
/// produces a tag that matches. That is the same question the wrapped key
/// answers about a password, asked about a key instead.
const KEY_CHECK_PLAINTEXT: &[u8] = b"everyday.key-check.v1";

/// Associated data the key-check value is sealed under. A constant rather
/// than a function that rebuilds the same bytes on every call -- there was
/// never anything to compute here, only a byte string to name.
const KEY_CHECK_AAD: &[u8] = b"everyday.key-check.v1";

pub(super) fn seal_key_check(dek: &SecretKey) -> Result<String> {
    Ok(to_hex(&AeadCipher::new(dek).seal(KEY_CHECK_AAD, KEY_CHECK_PLAINTEXT)?))
}

/// Does this key open the check value in the header?
///
/// Anything unreadable -- bad hex, a tag that does not verify, a plaintext
/// that is not the constant -- is a no. There is no case here where a
/// malformed header should be taken as a pass.
pub(super) fn key_check_passes(dek: &SecretKey, sealed_hex: &str) -> bool {
    let Ok(sealed) = from_hex(sealed_hex) else { return false };
    AeadCipher::new(dek).open(KEY_CHECK_AAD, &sealed).is_ok_and(|p| p == KEY_CHECK_PLAINTEXT)
}

pub(super) fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

pub(super) fn from_hex(s: &str) -> Result<Vec<u8>> {
    if s.len() % 2 != 0 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Invalid("vault header contains malformed hex".into()));
    }
    Ok(s.as_bytes()
        .chunks_exact(2)
        .map(|c| {
            let hi = (c[0] as char).to_digit(16).unwrap() as u8;
            let lo = (c[1] as char).to_digit(16).unwrap() as u8;
            hi << 4 | lo
        })
        .collect())
}
