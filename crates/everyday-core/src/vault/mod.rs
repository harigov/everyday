//! Vault lifecycle: creation, locking, unlocking and password changes.
//!
//! A *vault* is a directory holding one person's journals. It contains a
//! plaintext header (`vault.json`) and whatever layout the chosen storage
//! backend wants underneath.
//!
//! ```text
//!   ~/Documents/Every Day/
//!     vault.json          <- header: backend, cipher suite, KDF params,
//!                            random salt, and the wrapped data key
//!     store/              <- owned by the backend
//! ```
//!
//! The header is deliberately *not* encrypted: something has to be readable
//! before the password is known in order to know how to ask for it. It
//! contains no journal content — only the parameters needed to derive a key,
//! the data key sealed under that key, and the backend's own settings sealed
//! under the same one. That last field is why the header is a *mixture*
//! rather than plaintext throughout: a Postgres URL carries a password, and
//! a vault that wrote its database credential in the clear next to the salt
//! would be handing away everything it had just encrypted.
//!
//! # Locking
//!
//! [`Vault::lock`] drops the storage backend, drops the search index and
//! zeroizes the data key. After it returns there is no plaintext journal
//! content in the process, which is what makes the app-level lock screen
//! meaningful rather than cosmetic.

mod agent;
mod calendars;
mod header;
mod journal;
mod library;
mod lifecycle;
mod maintenance;
mod notes;
mod profile;
mod purpose;
mod routines;
mod session;
mod tasks;
#[cfg(test)]
mod tests;
mod trackers;

pub use header::VaultHeader;

use crate::crypto::{
    KdfParams, SUITE_NONE, SUITE_XCHACHA20_POLY1305, SecretKey, derive_key, random_salt, wrap_key,
};
use crate::error::{Error, Result};
use crate::store::{BackendRegistry, BackendSettings, Capabilities, StoreStats};
use header::{
    cipher_for, read_header, remove_partial_vault, seal_key_check, seal_settings, to_hex,
    write_header,
};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use session::Unlocked;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};
use std::time::Instant;

/// Header format version. Bumped only for breaking changes; readers refuse
/// anything newer rather than guessing.
pub const FORMAT_VERSION: u32 = 1;

pub const HEADER_FILENAME: &str = "vault.json";

/// The spare copy of the header, kept one write behind the live one.
pub const HEADER_BACKUP_FILENAME: &str = "vault.json.bak";

const STORE_DIRNAME: &str = "store";

/// Options for [`Vault::create`].
#[derive(Debug, Clone)]
pub struct VaultConfig {
    pub name: String,
    pub backend: String,
    /// What the chosen backend needs to be told. Empty for the local ones.
    pub settings: BackendSettings,
    /// `None` creates an *unencrypted* vault. This is a deliberate choice
    /// the UI must surface, never a default.
    pub password: Option<String>,
    pub kdf: KdfParams,
    pub auto_lock_seconds: u64,
    #[allow(clippy::doc_markdown)]
    /// See [`VaultHeader::forget_key_seconds`]. 0, meaning never, is right
    /// for a vault that is going to be served.
    pub forget_key_seconds: u64,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            name: "My Journal".into(),
            backend: "sqlite".into(),
            settings: BackendSettings::default(),
            password: None,
            kdf: KdfParams::default(),
            auto_lock_seconds: 15 * 60,
            forget_key_seconds: 0,
        }
    }
}

/// What the UI needs to decide which screen to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultStatus {
    pub name: String,
    pub backend: String,
    pub unlocked: bool,
    pub encrypted: bool,
    pub auto_lock_seconds: u64,
    #[serde(default)]
    pub forget_key_seconds: u64,
    pub path: PathBuf,
    /// False when another process holds the vault's write lock, so this copy
    /// reads but cannot save. The interface uses it to say so plainly rather
    /// than letting every write fail one at a time.
    #[serde(default = "default_true")]
    pub writable: bool,
    /// `None` while locked — reading it would require decrypting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<StoreStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Capabilities>,
}

fn default_true() -> bool {
    true
}

/// A journal vault. Cheap to share: every method takes `&self`.
pub struct Vault {
    root: PathBuf,
    header: RwLock<VaultHeader>,
    state: RwLock<Option<Unlocked>>,
    registry: Arc<BackendRegistry>,
    /// Milliseconds since `epoch` at the last user-initiated operation.
    last_activity_ms: AtomicU64,
    epoch: Instant,
    /// The exclusive write claim on this directory, or `None` if another
    /// process holds it and this vault is therefore read-only. Dropped with
    /// the vault, which is what releases it. See [`crate::lockfile`].
    write_lock: Option<crate::lockfile::VaultLock>,
}

impl Vault {
    // ---- construction ---------------------------------------------------

    /// Is there a vault here?
    ///
    /// The backup header counts. It is what [`Vault::create`] consults before
    /// refusing to overwrite, and a vault whose live header was lost to a bad
    /// shutdown is still a vault -- answering "no" would invite creating a
    /// fresh one on top of it, which is the one mistake this code must never
    /// make.
    pub fn exists(root: &Path) -> bool {
        root.join(HEADER_FILENAME).is_file() || root.join(HEADER_BACKUP_FILENAME).is_file()
    }

    /// Create a new vault at `root` and leave it unlocked.
    pub fn create(root: &Path, cfg: VaultConfig, registry: Arc<BackendRegistry>) -> Result<Self> {
        if Self::exists(root) {
            return Err(Error::AlreadyInitialised(root.to_path_buf()));
        }
        if !registry.ids().contains(&cfg.backend.as_str()) {
            return Err(Error::UnknownBackend(cfg.backend.clone()));
        }
        std::fs::create_dir_all(root).map_err(|e| Error::io(root, e))?;

        let (mut header, dek) = match cfg.password.as_deref() {
            Some(password) => {
                if password.is_empty() {
                    return Err(Error::Invalid("password must not be empty".into()));
                }
                let salt = random_salt();
                let kek = derive_key(password, &salt, cfg.kdf)?;
                let dek = SecretKey::random();
                let wrapped = wrap_key(&kek, &dek)?;
                (
                    VaultHeader {
                        format: FORMAT_VERSION,
                        name: cfg.name.clone(),
                        backend: cfg.backend.clone(),
                        cipher: SUITE_XCHACHA20_POLY1305.into(),
                        kdf: Some(cfg.kdf),
                        salt: Some(to_hex(&salt)),
                        wrapped_key: Some(to_hex(&wrapped)),
                        backend_settings: None,
                        // Made by a build that has both timers, so there is
                        // nothing to carry across.
                        migrated_lock: Some(true),
                        key_check: Some(seal_key_check(&dek)?),
                        created_at: Timestamp::now(),
                        auto_lock_seconds: cfg.auto_lock_seconds,
                        forget_key_seconds: cfg.forget_key_seconds,
                    },
                    Some(dek),
                )
            }
            None => (
                VaultHeader {
                    format: FORMAT_VERSION,
                    name: cfg.name.clone(),
                    backend: cfg.backend.clone(),
                    cipher: SUITE_NONE.into(),
                    kdf: None,
                    salt: None,
                    wrapped_key: None,
                    backend_settings: None,
                    migrated_lock: Some(true),
                    // Nothing to check: an unencrypted vault has no key.
                    key_check: None,
                    created_at: Timestamp::now(),
                    auto_lock_seconds: cfg.auto_lock_seconds,
                    forget_key_seconds: cfg.forget_key_seconds,
                },
                None,
            ),
        };

        // Sealed with the key that was just made, before it is handed to the
        // cipher that `activate` builds. Nothing else can read it afterwards
        // without the password, which is the point.
        header.backend_settings = seal_settings(&cipher_for(dek.as_ref()), &cfg.settings)?;

        write_header(root, &header)?;

        let vault = Self {
            root: root.to_path_buf(),
            header: RwLock::new(header),
            state: RwLock::new(None),
            registry,
            last_activity_ms: AtomicU64::new(0),
            epoch: Instant::now(),
            // A vault nobody could write to is not worth creating, so unlike
            // `open` this does not fall back to read-only. In practice it is
            // always ours: `create` refused an existing vault above, and the
            // only way to lose the race is two processes creating the same
            // new vault at the same instant.
            write_lock: crate::lockfile::acquire(root)?,
        };

        // A backend that cannot be opened must not leave a vault behind.
        //
        // Until there was a backend on the far side of a network, nothing
        // here could realistically fail: opening a file inside a directory
        // this function had just made is about as certain as anything gets.
        // A *connection* is not -- a typo in the URL, a password that has
        // changed, a server that is down -- and by the time that is known the
        // header is already on disk. `Vault::exists` would then be true, so
        // the corrected retry is refused with `AlreadyInitialised` and the
        // only way forward is deleting a directory by hand. That is a poor
        // thing to ask of someone whose first attempt at making a journal has
        // just failed, and impossible to ask through a window that offers no
        // way to do it.
        let held_the_lock = vault.write_lock.is_some();
        if let Err(e) = vault.activate(dek) {
            // The lock is released by dropping the vault, which has to happen
            // before its file is removed.
            drop(vault);
            remove_partial_vault(root, held_the_lock);
            return Err(e);
        }
        Ok(vault)
    }

    /// Open an existing vault. The returned vault is **locked** if it is
    /// encrypted, and already unlocked if it is not.
    pub fn open(root: &Path, registry: Arc<BackendRegistry>) -> Result<Self> {
        Self::open_inner(root, registry, true)
    }

    /// Open a vault's *header* without opening its storage backend.
    ///
    /// The repair path, and the only one there can be. Everything else here
    /// reaches the backend: `open` activates an unencrypted vault
    /// immediately, and `unlock` activates an encrypted one -- so a vault
    /// whose database has moved, or whose database password has been rotated,
    /// cannot be opened at all in the ordinary way. That would be fine if the
    /// thing needing to change lived somewhere else, but it does not: the
    /// connection URL is sealed in this vault's own header, and reading it
    /// needs the vault password and nothing else. See
    /// [`Vault::backend_settings`].
    ///
    /// The result is inert. It knows its name, its backend and its header,
    /// and every method that touches a record answers [`Error::Locked`].
    pub fn open_dormant(root: &Path, registry: Arc<BackendRegistry>) -> Result<Self> {
        Self::open_inner(root, registry, false)
    }

    fn open_inner(root: &Path, registry: Arc<BackendRegistry>, activate: bool) -> Result<Self> {
        let mut header = read_header(root)?;
        if header.format > FORMAT_VERSION {
            return Err(Error::UnsupportedVaultVersion {
                found: header.format,
                supported: FORMAT_VERSION,
            });
        }
        // Claim the vault for writing if nobody else has it. Failing to get
        // it is not an error: the vault opens read-only, so `everyday list`
        // beside an open window still works and only the writes are refused.
        let write_lock = crate::lockfile::acquire(root)?;
        if write_lock.is_none() {
            tracing::info!(
                path = %root.display(),
                "vault is open for writing elsewhere; opening read-only"
            );
        }

        // A vault written before the lock was split into two.
        //
        // `auto_lock_seconds` used to drop the key; it now hides a screen, and
        // the timer that drops the key is `forget_key_seconds`. Somebody who
        // had set fifteen minutes had set fifteen minutes for *the key*, and
        // an upgrade that silently turned that into "never" would have made
        // their vault less careful than they left it. So the old value is
        // carried across, once, on the first open by a build that knows about
        // both. Zero is honoured as zero -- "never" was already sayable.
        //
        // *After* the write lock, and only while holding it. This ran before
        // it and wrote the header regardless, which made a read-only open a
        // writing one: `everyday list` in a terminal would read the header,
        // and a moment later write its own stale copy back over whatever the
        // open window had done in between -- a `change_password` landing in
        // that window leaves the new salt and wrapped key overwritten by the
        // old pair, and the new password no longer opens the vault. Every
        // other header write in this file is gated on `writable()`; this is
        // the same gate, taken before there is a `self` to ask.
        if write_lock.is_some()
            && header.forget_key_seconds == 0
            && header.auto_lock_seconds > 0
            && header.migrated_lock.is_none()
        {
            header.forget_key_seconds = header.auto_lock_seconds;
            header.migrated_lock = Some(true);
            // Best effort: a failure here is retried on the next open, and
            // nothing below depends on it having landed.
            let _ = write_header(root, &header);
        }

        let encrypted = header.is_encrypted();
        let vault = Self {
            root: root.to_path_buf(),
            header: RwLock::new(header),
            state: RwLock::new(None),
            registry,
            last_activity_ms: AtomicU64::new(0),
            epoch: Instant::now(),
            write_lock,
        };
        if activate && !encrypted {
            vault.activate(None)?;
        }
        Ok(vault)
    }

    // ---- status ---------------------------------------------------------

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// The header, for tests that want to look at or rewrite it directly.
    ///
    /// Nothing outside this crate's own tests names `VaultHeader` today, so
    /// this stays narrower than `path`/`status` rather than growing a public
    /// accessor on the strength of that one caller.
    #[cfg(test)]
    pub(crate) fn header(&self) -> VaultHeader {
        self.header_read().clone()
    }

    pub fn status(&self) -> VaultStatus {
        let header = self.header_read().clone();
        let guard = self.state_read();
        VaultStatus {
            name: header.name,
            backend: header.backend,
            unlocked: guard.is_some(),
            encrypted: header.cipher != SUITE_NONE,
            auto_lock_seconds: header.auto_lock_seconds,
            forget_key_seconds: header.forget_key_seconds,
            path: self.root.clone(),
            writable: self.write_lock.is_some(),
            stats: guard.as_ref().and_then(|u| u.store.stats().ok()),
            capabilities: guard.as_ref().map(|u| u.store.capabilities()),
        }
    }
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("root", &self.root)
            .field("unlocked", &self.is_unlocked())
            .finish()
    }
}
