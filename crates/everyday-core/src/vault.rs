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
//! contains no journal content — only the parameters needed to derive a key
//! and the data key sealed under that key.
//!
//! # Locking
//!
//! [`Vault::lock`] drops the storage backend, drops the search index and
//! zeroizes the data key. After it returns there is no plaintext journal
//! content in the process, which is what makes the app-level lock screen
//! meaningful rather than cosmetic.

use crate::calendar::{Calendar, Event, SyncReport};
use crate::crypto::{
    AeadCipher, Cipher, KdfParams, NullCipher, SUITE_NONE, SUITE_XCHACHA20_POLY1305, SecretKey,
    derive_key, random_salt, unwrap_key, wrap_key,
};
use crate::error::{Error, Result};
use crate::fsutil;
use crate::id::{
    BlobId, BlockId, CalendarId, EntryId, EventId, ItemId, JournalId, KindId, LogId, ProjectId,
    TaskId,
};
use crate::library::{Item, ItemStatus, Kind, KindCount, LibraryStats, LogEntry, default_kinds};
use crate::model::{Entry, EntrySummary, Journal};
use crate::search::{SearchHit, SearchIndex};
use crate::store::calendars::{CalendarStore, EventQuery};
use crate::store::library::{ItemQuery, LibraryStore, LogQuery};
use crate::store::tasks::{BlockQuery, TaskQuery, TaskStore};
use crate::store::{
    BackendRegistry, Capabilities, EntryQuery, JournalStore, StoreContext, StoreStats,
};
use crate::task::{Project, Task, TaskStats, TimeBlock};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

/// Header format version. Bumped only for breaking changes; readers refuse
/// anything newer rather than guessing.
pub const FORMAT_VERSION: u32 = 1;

pub const HEADER_FILENAME: &str = "vault.json";

/// The spare copy of the header, kept one write behind the live one.
pub const HEADER_BACKUP_FILENAME: &str = "vault.json.bak";

const STORE_DIRNAME: &str = "store";

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
    pub created_at: Timestamp,
    /// Seconds of inactivity before the vault locks itself. 0 disables.
    #[serde(default)]
    pub auto_lock_seconds: u64,
}

impl VaultHeader {
    pub fn is_encrypted(&self) -> bool {
        self.cipher != SUITE_NONE
    }
}

/// Options for [`Vault::create`].
#[derive(Debug, Clone)]
pub struct VaultConfig {
    pub name: String,
    pub backend: String,
    /// `None` creates an *unencrypted* vault. This is a deliberate choice
    /// the UI must surface, never a default.
    pub password: Option<String>,
    pub kdf: KdfParams,
    pub auto_lock_seconds: u64,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            name: "My Journal".into(),
            backend: "sqlite".into(),
            password: None,
            kdf: KdfParams::default(),
            auto_lock_seconds: 15 * 60,
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

/// Live state that exists only while unlocked.
struct Unlocked {
    store: Box<dyn JournalStore>,
    index: SearchIndex,
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

        let (header, dek) = match cfg.password.as_deref() {
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
                        created_at: Timestamp::now(),
                        auto_lock_seconds: cfg.auto_lock_seconds,
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
                    created_at: Timestamp::now(),
                    auto_lock_seconds: cfg.auto_lock_seconds,
                },
                None,
            ),
        };

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
        vault.activate(dek)?;
        Ok(vault)
    }

    /// Open an existing vault. The returned vault is **locked** if it is
    /// encrypted, and already unlocked if it is not.
    pub fn open(root: &Path, registry: Arc<BackendRegistry>) -> Result<Self> {
        let header = read_header(root)?;
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
        if !encrypted {
            vault.activate(None)?;
        }
        Ok(vault)
    }

    // ---- locking --------------------------------------------------------

    pub fn is_unlocked(&self) -> bool {
        self.state_read().is_some()
    }

    /// Unlock with a password. Pass `None` for an unencrypted vault.
    ///
    /// Returns [`Error::BadPassword`] on the wrong password — the AEAD tag on
    /// the wrapped key is what detects this, so there is no separate
    /// password verifier to leak.
    pub fn unlock(&self, password: Option<&str>) -> Result<()> {
        if self.is_unlocked() {
            return Ok(());
        }
        let header = self.header_read().clone();

        let dek = if header.is_encrypted() {
            let password = password.ok_or(Error::BadPassword)?;
            let salt_hex = header.salt.as_deref().ok_or_else(|| {
                Error::Invalid("encrypted vault header is missing its salt".into())
            })?;
            let wrapped_hex = header.wrapped_key.as_deref().ok_or_else(|| {
                Error::Invalid("encrypted vault header is missing its wrapped key".into())
            })?;
            let kdf = header.kdf.ok_or_else(|| {
                Error::Invalid("encrypted vault header is missing its KDF parameters".into())
            })?;
            let kek = derive_key(password, &from_hex(salt_hex)?, kdf)?;
            Some(unwrap_key(&kek, &from_hex(wrapped_hex)?)?)
        } else {
            None
        };

        self.activate(dek)
    }

    /// Open the backend and build the search index. Assumes the key is right.
    fn activate(&self, dek: Option<SecretKey>) -> Result<()> {
        let header = self.header_read().clone();
        // The cipher owns the only copy of the key material from here on;
        // `dek` is dropped (and zeroized) at the end of this function.
        let cipher: Arc<dyn Cipher> = match &dek {
            Some(k) => Arc::new(AeadCipher::new(k)),
            None => Arc::new(NullCipher),
        };

        let store_root = self.root.join(STORE_DIRNAME);
        std::fs::create_dir_all(&store_root).map_err(|e| Error::io(&store_root, e))?;
        let store =
            self.registry.open(&header.backend, StoreContext { root: store_root, cipher })?;

        // Check the store before trusting it, but do not refuse to open on a
        // bad answer. A damaged vault is precisely the one someone needs to
        // get into -- to export what still reads, or to see how much of it
        // survived -- and locking them out would turn recoverable damage
        // into total loss. Say so loudly instead; `everyday check` reports
        // the same findings on demand.
        match store.check_integrity() {
            Ok(problems) if !problems.is_empty() => {
                tracing::error!(
                    count = problems.len(),
                    first = %problems[0],
                    "storage integrity check failed -- restore from a backup"
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "could not run the integrity check"),
        }

        let index = SearchIndex::build(&store.all_entries()?);

        *self.state_write() = Some(Unlocked { store, index });
        self.touch();
        Ok(())
    }

    /// Lock the vault: close the backend, drop the index, zeroize the key.
    pub fn lock(&self) {
        // Take the state out and drop it outside the lock so that a slow
        // backend shutdown does not hold every reader.
        let previous = self.state_write().take();
        drop(previous); // SecretKey zeroizes here; SearchIndex frees plaintext
    }

    /// Change the password, or add/remove encryption entirely.
    ///
    /// Only the *wrapped data key* is rewritten, so this is instant even for
    /// a vault with tens of thousands of entries.
    pub fn change_password(&self, current: Option<&str>, new: Option<&str>) -> Result<()> {
        self.writable()?;
        let header = self.header_read().clone();

        // Verify the current password by unwrapping, whether or not the
        // vault happens to be unlocked already.
        let dek = if header.is_encrypted() {
            let current = current.ok_or(Error::BadPassword)?;
            let kdf = header.kdf.ok_or_else(|| Error::Invalid("missing KDF params".into()))?;
            let salt = from_hex(header.salt.as_deref().unwrap_or_default())?;
            let kek = derive_key(current, &salt, kdf)?;
            unwrap_key(&kek, &from_hex(header.wrapped_key.as_deref().unwrap_or_default())?)?
        } else {
            SecretKey::random()
        };

        let mut next = header.clone();
        match new {
            Some(password) if !password.is_empty() => {
                let kdf = header.kdf.unwrap_or_default();
                let salt = random_salt();
                let kek = derive_key(password, &salt, kdf)?;
                next.cipher = SUITE_XCHACHA20_POLY1305.into();
                next.kdf = Some(kdf);
                next.salt = Some(to_hex(&salt));
                next.wrapped_key = Some(to_hex(&wrap_key(&kek, &dek)?));
            }
            _ => {
                // Removing the password would leave the on-disk records
                // sealed under a key nobody holds. Rewriting the whole vault
                // is a different, much heavier operation than this one.
                return Err(Error::Unsupported(
                    "removing a vault password (re-encrypting the whole vault)",
                ));
            }
        }

        write_header(&self.root, &next)?;
        *self.header_write() = next;

        // Same data key, so an unlocked session stays valid.
        Ok(())
    }

    pub fn set_auto_lock(&self, seconds: u64) -> Result<()> {
        self.writable()?;
        let mut header = self.header_write();
        header.auto_lock_seconds = seconds;
        write_header(&self.root, &header)
    }

    /// Record user activity, deferring the idle auto-lock.
    pub fn touch(&self) {
        let ms = self.epoch.elapsed().as_millis() as u64;
        self.last_activity_ms.store(ms, Ordering::Relaxed);
    }

    /// Seconds until the idle auto-lock fires, or `None` if it is disabled
    /// or the vault is already locked.
    pub fn seconds_until_auto_lock(&self) -> Option<u64> {
        let timeout = self.header_read().auto_lock_seconds;
        if timeout == 0 || !self.is_unlocked() {
            return None;
        }
        let idle_ms =
            self.epoch.elapsed().as_millis() as u64 - self.last_activity_ms.load(Ordering::Relaxed);
        Some(timeout.saturating_sub(idle_ms / 1000))
    }

    /// Lock the vault if it has been idle past its timeout. The application
    /// calls this on a timer. Returns whether it locked.
    pub fn auto_lock_if_idle(&self) -> bool {
        if self.seconds_until_auto_lock() == Some(0) {
            self.lock();
            return true;
        }
        false
    }

    // ---- status ---------------------------------------------------------

    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn header(&self) -> VaultHeader {
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
            path: self.root.clone(),
            writable: self.write_lock.is_some(),
            stats: guard.as_ref().and_then(|u| u.store.stats().ok()),
            capabilities: guard.as_ref().map(|u| u.store.capabilities()),
        }
    }

    // ---- the write claim -------------------------------------------------

    /// May this process write to the vault?
    ///
    /// False when another process held the directory's write lock at open
    /// time. The vault still reads; see [`crate::lockfile`] for why that is
    /// the right split.
    pub fn is_writable(&self) -> bool {
        self.write_lock.is_some()
    }

    /// Guard at the top of every method that changes something on disk.
    ///
    /// Deliberately one call per mutator rather than something clever in
    /// `write`: the read/write split in this type does not line up with the
    /// lock helpers -- `save_calendar` goes through `read` because
    /// `with_calendars` does -- so a central hook would either miss writes or
    /// refuse reads. A `self.writable()?` on each is greppable, and a new
    /// mutator that forgets it is a visible omission rather than an invisible
    /// one.
    fn writable(&self) -> Result<()> {
        if self.write_lock.is_some() {
            return Ok(());
        }
        Err(Error::VaultInUse {
            holder: crate::lockfile::holder(&self.root)
                .map(|h| h.to_string())
                .unwrap_or_else(|| "another process".into()),
        })
    }

    // ---- lock accessors --------------------------------------------------
    //
    // Every lock in this type is taken through the four helpers below, and
    // none of them treats poisoning as fatal.
    //
    // `unwrap()` on a poisoned lock turns a single panic anywhere in the
    // process into a vault that can never be used again: every later read and
    // every later *write* panics on the poison flag. For an app whose whole
    // job is not to lose what someone typed, that is the worst available
    // response -- the window stays open, the editor keeps accepting text, and
    // each autosave dies on a flag set minutes ago.
    //
    // What poisoning warns about does not really arise here either. The state
    // it guards is an open store plus a search index; a panic between the two
    // can leave the index stale, which shows up as a search result pointing
    // at an edited entry and is repaired by `reindex`. That is worth
    // continuing through, and losing the session's writing is not.

    fn state_read(&self) -> std::sync::RwLockReadGuard<'_, Option<Unlocked>> {
        self.state.read().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn state_write(&self) -> std::sync::RwLockWriteGuard<'_, Option<Unlocked>> {
        self.state.write().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn header_read(&self) -> std::sync::RwLockReadGuard<'_, VaultHeader> {
        self.header.read().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn header_write(&self) -> std::sync::RwLockWriteGuard<'_, VaultHeader> {
        self.header.write().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // ---- data access ----------------------------------------------------

    /// Run `f` against the unlocked store, or fail with [`Error::Locked`].
    fn read<T>(&self, f: impl FnOnce(&Unlocked) -> Result<T>) -> Result<T> {
        let guard = self.state_read();
        let unlocked = guard.as_ref().ok_or(Error::Locked)?;
        let out = f(unlocked);
        drop(guard);
        self.touch();
        out
    }

    fn write<T>(&self, f: impl FnOnce(&mut Unlocked) -> Result<T>) -> Result<T> {
        let mut guard = self.state_write();
        let unlocked = guard.as_mut().ok_or(Error::Locked)?;
        let out = f(unlocked);
        drop(guard);
        self.touch();
        out
    }

    pub fn journals(&self) -> Result<Vec<Journal>> {
        self.read(|u| {
            let mut js = u.store.list_journals()?;
            js.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)));
            Ok(js)
        })
    }

    pub fn journal(&self, id: JournalId) -> Result<Journal> {
        self.read(|u| u.store.get_journal(id))
    }

    pub fn save_journal(&self, journal: &Journal) -> Result<()> {
        self.writable()?;
        self.write(|u| u.store.put_journal(journal))
    }

    /// Delete a journal and every entry inside it.
    pub fn delete_journal(&self, id: JournalId) -> Result<()> {
        self.writable()?;
        self.write(|u| {
            for e in u.store.list_entries(&EntryQuery::in_journal(id))? {
                u.index.remove(e.id);
            }
            u.store.delete_journal(id)
        })
    }

    pub fn entries(&self, query: &EntryQuery) -> Result<Vec<EntrySummary>> {
        self.read(|u| u.store.list_entries(query))
    }

    pub fn entry(&self, id: EntryId) -> Result<Entry> {
        self.read(|u| u.store.get_entry(id))
    }

    /// Save `entry`, provided nobody else has written it since `expect`.
    ///
    /// `expect` is the `updated_at` the caller loaded, or `None` for a new
    /// entry; a mismatch is [`Error::Conflict`] and nothing is written. It
    /// cannot be read off `entry`, because the caller has already stamped a
    /// fresh `updated_at` on the copy it is trying to save.
    ///
    /// This is what stops the second saver of an entry silently overwriting
    /// the first. The vault write lock already keeps two *processes* from
    /// both being writers, so what is left for this to catch is the case the
    /// lock cannot: an editor that has had an entry open since before some
    /// other change to it -- a CLI edit made while the app was closed, a
    /// vault in a synced folder written on another machine, or simply a stale
    /// tab. Losing a paragraph to any of those is the failure this exists to
    /// prevent, so the conflict is reported and the author decides.
    pub fn save_entry(&self, entry: &Entry, expect: Option<Timestamp>) -> Result<()> {
        self.writable()?;
        entry.body.validate()?;
        self.write(|u| {
            u.store.put_entry_if(entry, expect)?;
            u.index.insert(entry);
            Ok(())
        })
    }

    /// Save `entry` regardless of what is already stored.
    ///
    /// The deliberate resolution of a conflict [`Vault::save_entry`]
    /// reported, and the path an import takes. Separate rather than an
    /// `Option` flag so that overwriting somebody's work is something a
    /// caller has to name.
    pub fn overwrite_entry(&self, entry: &Entry) -> Result<()> {
        self.writable()?;
        entry.body.validate()?;
        self.write(|u| {
            u.store.put_entry(entry)?;
            u.index.insert(entry);
            Ok(())
        })
    }

    pub fn delete_entry(&self, id: EntryId) -> Result<()> {
        self.writable()?;
        self.write(|u| {
            u.store.delete_entry(id)?;
            u.index.remove(id);
            Ok(())
        })
    }

    // ---- tasks, projects and time ---------------------------------------
    //
    // The second domain. Every method here goes through `with_tasks`, which
    // fails with `unsupported` on a backend that holds journals only, so a
    // Markdown vault reports the absence structurally rather than panicking
    // or silently returning nothing.

    /// Does this vault's backend store tasks at all?
    pub fn supports_tasks(&self) -> bool {
        self.read(|u| Ok(u.store.tasks().is_some())).unwrap_or(false)
    }

    /// Run `f` against the task store, or explain that there isn't one.
    fn with_tasks<T>(&self, f: impl FnOnce(&dyn TaskStore) -> Result<T>) -> Result<T> {
        self.read(|u| {
            let tasks = u
                .store
                .tasks()
                .ok_or(Error::Unsupported("tasks (this vault's backend stores journals only)"))?;
            f(tasks)
        })
    }

    pub fn projects(&self) -> Result<Vec<Project>> {
        self.with_tasks(|t| {
            let mut ps = t.list_projects()?;
            ps.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)));
            Ok(ps)
        })
    }

    pub fn project(&self, id: ProjectId) -> Result<Project> {
        self.with_tasks(|t| t.get_project(id))
    }

    pub fn save_project(&self, project: &Project) -> Result<()> {
        self.writable()?;
        self.with_tasks(|t| t.put_project(project))
    }

    /// Delete a project, its tasks and their time blocks.
    pub fn delete_project(&self, id: ProjectId) -> Result<()> {
        self.writable()?;
        self.with_tasks(|t| t.delete_project(id))
    }

    pub fn tasks(&self, query: &TaskQuery) -> Result<Vec<Task>> {
        self.with_tasks(|t| t.list_tasks(query))
    }

    pub fn task(&self, id: TaskId) -> Result<Task> {
        self.with_tasks(|t| t.get_task(id))
    }

    pub fn save_task(&self, task: &Task) -> Result<()> {
        self.writable()?;
        if task.title.trim().is_empty() {
            return Err(Error::Invalid("a task needs a title".into()));
        }
        if task.parent_id == Some(task.id) {
            return Err(Error::Invalid("a task cannot be its own subtask".into()));
        }
        self.with_tasks(|t| t.put_task(task))
    }

    /// Write several tasks as one operation. This is what a board reorder
    /// is: dragging one card renumbers everything below it in two columns.
    pub fn save_tasks(&self, tasks: &[Task]) -> Result<()> {
        self.writable()?;
        for t in tasks {
            if t.title.trim().is_empty() {
                return Err(Error::Invalid("a task needs a title".into()));
            }
        }
        self.with_tasks(|s| s.put_tasks(tasks))
    }

    /// Delete a task, its subtasks and their time blocks.
    pub fn delete_task(&self, id: TaskId) -> Result<()> {
        self.writable()?;
        self.with_tasks(|t| t.delete_task(id))
    }

    pub fn blocks(&self, query: &BlockQuery) -> Result<Vec<TimeBlock>> {
        self.with_tasks(|t| t.list_blocks(query))
    }

    pub fn block(&self, id: BlockId) -> Result<TimeBlock> {
        self.with_tasks(|t| t.get_block(id))
    }

    pub fn save_block(&self, block: &TimeBlock) -> Result<()> {
        self.writable()?;
        block.validate()?;
        self.with_tasks(|t| t.put_block(block))
    }

    pub fn delete_block(&self, id: BlockId) -> Result<()> {
        self.writable()?;
        self.with_tasks(|t| t.delete_block(id))
    }

    /// Counts for the sidebar, as of the calendar day `today`.
    pub fn task_stats(&self, today: jiff::civil::Date) -> Result<TaskStats> {
        self.with_tasks(|t| t.task_stats(today))
    }

    /// Every tag used on an entry, most used first, ties broken
    /// alphabetically.
    ///
    /// The journal-domain twin of [`Vault::task_tags`], and it lives here for
    /// the same reason that one does: which tags exist and how often they are
    /// used is a question about the vault's contents, not about the shell
    /// asking. It had been a loop in the desktop shell's command handler,
    /// which meant the CLI could not answer it and the two front ends were
    /// one copy-paste away from sorting the list differently.
    pub fn entry_tags(&self) -> Result<Vec<(String, u32)>> {
        let mut counts: std::collections::BTreeMap<String, u32> = Default::default();
        for e in self.entries(&EntryQuery::default())? {
            for tag in e.tags {
                *counts.entry(tag).or_default() += 1;
            }
        }
        let mut out: Vec<(String, u32)> = counts.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(out)
    }

    /// Every tag in the task domain with how often it is used, most used
    /// first, ties broken alphabetically.
    ///
    /// Tags are inside the sealed payloads -- the same trade the journal
    /// makes -- so this is a decrypt-and-count pass rather than an index
    /// lookup. That is affordable at the scale a person's todo list reaches,
    /// and it is what keeps the database file from listing what someone is
    /// working on to anyone who opens it.
    pub fn task_tags(&self) -> Result<Vec<(String, u32)>> {
        self.with_tasks(|t| {
            let mut counts: std::collections::BTreeMap<String, u32> = Default::default();
            let mut bump = |tags: &[String]| {
                for tag in tags {
                    *counts.entry(tag.clone()).or_default() += 1;
                }
            };
            for p in t.list_projects()? {
                bump(&p.tags);
            }
            for task in t.list_tasks(&TaskQuery::default())? {
                bump(&task.tags);
            }
            for b in t.list_blocks(&BlockQuery::default())? {
                bump(&b.tags);
            }
            let mut out: Vec<(String, u32)> = counts.into_iter().collect();
            out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            Ok(out)
        })
    }

    // ---- calendars ------------------------------------------------------
    //
    // The third domain, on exactly the terms of the second: everything goes
    // through `with_calendars`, which fails with `unsupported` on a backend
    // that holds journals only.
    //
    // Note the shape of the sync entry point. This layer takes iCalendar
    // *text*, never a URL: fetching is the shell's job, because the core has
    // no async runtime, no TLS stack and -- deliberately -- no ability to
    // open a socket at all. What the core owns is everything that happens to
    // those bytes afterwards, which is the part worth testing.

    /// Does this vault's backend store subscribed calendars?
    pub fn supports_calendars(&self) -> bool {
        self.read(|u| Ok(u.store.calendars().is_some())).unwrap_or(false)
    }

    fn with_calendars<T>(&self, f: impl FnOnce(&dyn CalendarStore) -> Result<T>) -> Result<T> {
        self.read(|u| {
            let calendars = u.store.calendars().ok_or(Error::Unsupported(
                "calendars (this vault's backend stores journals only)",
            ))?;
            f(calendars)
        })
    }

    pub fn calendars(&self) -> Result<Vec<Calendar>> {
        self.with_calendars(|c| c.list_calendars())
    }

    pub fn calendar(&self, id: CalendarId) -> Result<Calendar> {
        self.with_calendars(|c| c.get_calendar(id))
    }

    pub fn save_calendar(&self, calendar: &Calendar) -> Result<()> {
        self.writable()?;
        if calendar.name.trim().is_empty() {
            return Err(Error::Invalid("a calendar needs a name".into()));
        }
        // Validate the address on the way in rather than at fetch time, so a
        // `file://` URL is refused where it was typed instead of quietly
        // stored and refused an hour later by a background sync nobody is
        // watching.
        if calendar.origin.url().is_some() {
            calendar.fetch_url()?;
        }
        self.with_calendars(|c| c.put_calendar(calendar))
    }

    /// Unsubscribe: the calendar and every event that came from it.
    pub fn delete_calendar(&self, id: CalendarId) -> Result<()> {
        self.writable()?;
        self.with_calendars(|c| c.delete_calendar(id))
    }

    pub fn events(&self, query: &EventQuery) -> Result<Vec<Event>> {
        self.with_calendars(|c| c.list_events(query))
    }

    pub fn event(&self, id: EventId) -> Result<Event> {
        self.with_calendars(|c| c.get_event(id))
    }

    /// How many events are held for one calendar.
    pub fn event_count(&self, id: CalendarId) -> Result<u64> {
        self.with_calendars(|c| c.count_events(id))
    }

    /// Parse `ics` and make it the whole of what `id` holds.
    ///
    /// The window is the days worth materialising: recurring events are
    /// expanded into it and no further, which is what keeps a decade-old
    /// daily stand-up from becoming four thousand rows. `default_tz` is the
    /// zone a floating time is read in — the reader's own.
    pub fn sync_calendar_from_ics(
        &self,
        id: CalendarId,
        ics: &str,
        window: (jiff::civil::Date, jiff::civil::Date),
        default_tz: &str,
    ) -> Result<SyncReport> {
        self.writable()?;
        // Refuse anything that is not an iCalendar document *before* it can
        // replace one, and before the store is touched at all. A captive
        // portal's login page, an expired link's HTML error, a truncated
        // download: all of them parse to zero events, and all of them would
        // otherwise empty a working calendar. A genuine VCALENDAR with no
        // VEVENTs in it is a different thing -- that is a real answer,
        // meaning "nothing on here" -- and it is written.
        if !ics.to_ascii_uppercase().contains("BEGIN:VCALENDAR") {
            return Err(Error::Invalid(
                "that address did not return a calendar; the events already here have been kept"
                    .into(),
            ));
        }
        let mut calendar = self.calendar(id)?;
        let feed = crate::ics::parse(ics);
        let (events, skipped) = crate::ics::events_for(&calendar, &feed, window, default_tz);

        self.with_calendars(|c| c.replace_events(id, &events))?;

        calendar.mark_synced();
        // A calendar that never had a name of its own takes the publisher's,
        // which spares the subscriber naming something they did not create.
        if let Some(name) = feed.name.as_deref()
            && !name.is_empty()
            && calendar.name.trim().is_empty()
        {
            calendar.name = name.to_string();
        }
        self.with_calendars(|c| c.put_calendar(&calendar))?;

        Ok(SyncReport {
            calendar_id: Some(id),
            events: events.len() as u64,
            skipped,
            feed_name: feed.name,
        })
    }

    /// Record that a sync failed, keeping the events that are already there.
    pub fn mark_calendar_failed(&self, id: CalendarId, why: &str) -> Result<()> {
        self.writable()?;
        let mut calendar = self.calendar(id)?;
        calendar.mark_failed(why);
        self.with_calendars(|c| c.put_calendar(&calendar))
    }

    // ---- the library ----------------------------------------------------
    //
    // The fourth domain, on exactly the terms of the second and third:
    // everything goes through `with_library`, which fails with `unsupported`
    // on a backend that holds journals only.
    //
    // Note the same division of labour the calendar makes. This layer takes
    // a `SearchResult` -- something already fetched and already parsed --
    // never a query to run: the core has no socket, and
    // `crate::websearch` is built so that stays true. What the core owns is
    // the part worth testing, which is what a result does to an item once it
    // arrives.

    /// Does this vault's backend store a library?
    pub fn supports_library(&self) -> bool {
        self.read(|u| Ok(u.store.library().is_some())).unwrap_or(false)
    }

    fn with_library<T>(&self, f: impl FnOnce(&dyn LibraryStore) -> Result<T>) -> Result<T> {
        self.read(|u| {
            let library = u.store.library().ok_or(Error::Unsupported(
                "a library (this vault's backend stores journals only)",
            ))?;
            f(library)
        })
    }

    /// Every shelf, in sidebar order.
    pub fn kinds(&self) -> Result<Vec<Kind>> {
        self.with_library(|l| {
            let mut kinds = l.list_kinds()?;
            kinds.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)));
            Ok(kinds)
        })
    }

    pub fn kind(&self, id: KindId) -> Result<Kind> {
        self.with_library(|l| l.get_kind(id))
    }

    pub fn save_kind(&self, kind: &Kind) -> Result<()> {
        self.writable()?;
        if kind.name.trim().is_empty() {
            return Err(Error::Invalid("a shelf needs a name".into()));
        }
        if kind.slug.trim().is_empty() {
            return Err(Error::Invalid("a shelf needs a short name to look things up by".into()));
        }
        self.with_library(|l| l.put_kind(kind))
    }

    /// Delete the shelf, everything on it and every log row those items had.
    pub fn delete_kind(&self, id: KindId) -> Result<()> {
        self.writable()?;
        self.with_library(|l| l.delete_kind(id))
    }

    /// Put the built-in shelves in an empty library, and do nothing at all
    /// otherwise. Returns how many were added.
    ///
    /// Called on unlock rather than at creation, which is what makes it work
    /// for the vault somebody already had before this app existed: the
    /// library appears on their next launch with shelves in it rather than
    /// as an empty screen with a "make a category" button.
    ///
    /// The emptiness test is deliberately "no kinds at all", not "no kind
    /// with this slug". Somebody who deletes the Films shelf has said
    /// something, and an application that puts it back every time it starts
    /// is an application that is arguing.
    pub fn seed_library(&self) -> Result<usize> {
        if !self.supports_library() || !self.is_writable() {
            return Ok(0);
        }
        self.with_library(|l| {
            if !l.list_kinds()?.is_empty() {
                return Ok(0);
            }
            let seeds = default_kinds();
            for kind in &seeds {
                l.put_kind(kind)?;
            }
            Ok(seeds.len())
        })
    }

    pub fn items(&self, query: &ItemQuery) -> Result<Vec<Item>> {
        self.with_library(|l| l.list_items(query))
    }

    pub fn item(&self, id: ItemId) -> Result<Item> {
        self.with_library(|l| l.get_item(id))
    }

    pub fn save_item(&self, item: &Item) -> Result<()> {
        self.writable()?;
        if item.title.trim().is_empty() {
            return Err(Error::Invalid("an item needs a title".into()));
        }
        // A rating is stored out of a hundred. Anything above that is a
        // caller that has not read `library::from_stars`, and silently
        // clamping it would hide the bug rather than the number.
        if item.rating.is_some_and(|r| r > 100) {
            return Err(Error::Invalid("a rating runs from 0 to 100".into()));
        }
        // An item on a shelf that does not exist has no fields, no verbs and
        // nowhere to be drawn. Checked here rather than by a foreign key so
        // that every backend enforces it, including the ones that have no
        // such thing.
        self.with_library(|l| {
            l.get_kind(item.kind_id)?;
            l.put_item(item)
        })
    }

    /// One write for many items: a re-ordered shelf, or a bulk status change.
    pub fn save_items(&self, items: &[Item]) -> Result<()> {
        self.writable()?;
        if let Some(bad) = items.iter().find(|i| i.title.trim().is_empty()) {
            return Err(Error::Invalid(format!("item {} has no title", bad.id)));
        }
        self.with_library(|l| l.put_items(items))
    }

    /// Delete the item and its whole log.
    pub fn delete_item(&self, id: ItemId) -> Result<()> {
        self.writable()?;
        self.with_library(|l| l.delete_item(id))
    }

    pub fn logs(&self, query: &LogQuery) -> Result<Vec<LogEntry>> {
        self.with_library(|l| l.list_logs(query))
    }

    pub fn log(&self, id: LogId) -> Result<LogEntry> {
        self.with_library(|l| l.get_log(id))
    }

    pub fn save_log(&self, log: &LogEntry) -> Result<()> {
        self.writable()?;
        if log.rating.is_some_and(|r| r > 100) {
            return Err(Error::Invalid("a rating runs from 0 to 100".into()));
        }
        // The same argument as `save_item`: a log row pointing at nothing is
        // a date with no sentence attached.
        self.with_library(|l| {
            l.get_item(log.item_id)?;
            l.put_log(log)
        })
    }

    pub fn delete_log(&self, id: LogId) -> Result<()> {
        self.writable()?;
        self.with_library(|l| l.delete_log(id))
    }

    /// Counts for the library sidebar, as of the calendar year `year`.
    ///
    /// One pass over the items and one over the year's completions, done in
    /// the core rather than in the interface for the reason `task_stats` is:
    /// a sidebar counting the rows it happens to be showing says "3" for a
    /// shelf of four hundred.
    pub fn library_stats(&self, year: i16) -> Result<LibraryStats> {
        self.with_library(|l| {
            let kinds = l.list_kinds()?;
            let items = l.list_items(&ItemQuery::default())?;

            let mut stats = LibraryStats {
                kinds: kinds.len() as u64,
                items: items.len() as u64,
                ..Default::default()
            };
            let mut rated_total: u64 = 0;
            let mut by_kind: std::collections::BTreeMap<KindId, KindCount> = kinds
                .iter()
                .map(|k| (k.id, KindCount { kind_id: k.id, ..Default::default() }))
                .collect();

            for item in &items {
                match item.status {
                    ItemStatus::Wishlist => stats.wishlist += 1,
                    ItemStatus::Active => stats.active += 1,
                    ItemStatus::Done => stats.done += 1,
                    _ => {}
                }
                if let Some(rating) = item.rating {
                    stats.rated += 1;
                    rated_total += u64::from(rating);
                }
                // An item whose shelf has been deleted under it is counted in
                // the totals and in no shelf, which is honest: it is still in
                // the vault, and the sidebar has nowhere to put it.
                if let Some(count) = by_kind.get_mut(&item.kind_id) {
                    count.items += 1;
                    if item.status.is_open() {
                        count.open += 1;
                    }
                    if item.status == ItemStatus::Active {
                        count.active += 1;
                    }
                }
            }

            stats.mean_rating =
                (stats.rated > 0).then(|| (rated_total / stats.rated).min(100) as u8);

            let (from, to) = (jiff::civil::date(year, 1, 1), jiff::civil::date(year, 12, 31));
            stats.finished_this_year = l.list_logs(&LogQuery::completions(from, to))?.len() as u64;

            // Sidebar order, so the interface can render the counts against
            // the shelves without a join.
            let mut counts: Vec<KindCount> = by_kind.into_values().collect();
            let order: std::collections::BTreeMap<KindId, (i32, String)> =
                kinds.iter().map(|k| (k.id, (k.sort_order, k.name.clone()))).collect();
            counts.sort_by(|a, b| order.get(&a.kind_id).cmp(&order.get(&b.kind_id)));
            stats.by_kind = counts;
            Ok(stats)
        })
    }

    pub fn search(
        &self,
        query: &str,
        journal: Option<JournalId>,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        self.read(|u| Ok(u.index.search_in(query, journal, limit)))
    }

    pub fn suggest_terms(&self, prefix: &str, limit: usize) -> Result<Vec<String>> {
        self.read(|u| Ok(u.index.terms_with_prefix(prefix, limit)))
    }

    /// Store attachment bytes, returning their content address.
    pub fn put_blob(&self, bytes: &[u8]) -> Result<BlobId> {
        self.writable()?;
        self.read(|u| u.store.put_blob(bytes))
    }

    pub fn blob(&self, id: BlobId) -> Result<Vec<u8>> {
        self.read(|u| u.store.get_blob(id))
    }

    pub fn stats(&self) -> Result<StoreStats> {
        self.read(|u| u.store.stats())
    }

    /// Reclaim attachments no entry references any more.
    ///
    /// Takes the *write* lock, not the read lock it used to. Collection is
    /// two passes -- walk the entries for live blob ids, then delete
    /// everything else -- and under a read lock a save landing between them
    /// stores a blob the first pass could not have seen and the second pass
    /// deletes. The write lock closes that window inside this process;
    /// `grace` is what covers the rest, including a draft that has not been
    /// saved yet and a second process holding one open. See
    /// [`JournalStore::collect_garbage`].
    pub fn collect_garbage(&self, grace: std::time::Duration) -> Result<u64> {
        self.writable()?;
        self.write(|u| u.store.collect_garbage(grace))
    }

    /// Report storage-level damage, or an empty list if the vault is sound.
    ///
    /// Only checks structure. A vault whose header is intact and whose pages
    /// are sound can still be one you have forgotten the password to, and
    /// that is not what this answers.
    pub fn check_integrity(&self) -> Result<Vec<String>> {
        self.read(|u| u.store.check_integrity())
    }

    /// Copy the whole vault into `dir`, sealed exactly as it is.
    ///
    /// The result is a vault, not an archive: point `open` at `dir` and it
    /// unlocks with the same password. That is deliberate. A backup format
    /// that needs a working copy of this program to restore is a backup that
    /// fails on the day the program is what broke.
    ///
    /// The header is written *last*. `Vault::exists` is what everything else
    /// tests, so until the header lands the destination is not yet a vault
    /// and a backup interrupted halfway cannot be mistaken for a whole one.
    pub fn backup(&self, dir: &Path) -> Result<()> {
        if Self::exists(dir) {
            return Err(Error::AlreadyInitialised(dir.to_path_buf()));
        }
        // Checkpoint first so the snapshot is not reading around a large WAL
        // -- but only if this process is the writer. A checkpoint is a write
        // to the *source*, and a read-only copy has no business making one
        // behind the back of whoever holds the lock. Skipping it costs a
        // slower snapshot, not a wrong one.
        if self.is_writable() {
            self.read(|u| u.store.flush())?;
        }
        self.read(|u| u.store.snapshot(&dir.join(STORE_DIRNAME)))?;
        write_header(dir, &self.header_read().clone())?;
        Ok(())
    }

    /// Every entry, bodies included. For export.
    pub fn all_entries(&self) -> Result<Vec<Entry>> {
        self.read(|u| u.store.all_entries())
    }

    /// Rebuild the search index from storage. Useful after a bulk import.
    pub fn reindex(&self) -> Result<usize> {
        self.write(|u| {
            u.index = SearchIndex::build(&u.store.all_entries()?);
            Ok(u.index.len())
        })
    }

    /// Escape hatch for operations that need the raw backend, e.g. import.
    pub fn with_store<T>(&self, f: impl FnOnce(&dyn JournalStore) -> Result<T>) -> Result<T> {
        self.read(|u| f(u.store.as_ref()))
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

// ---- header i/o ---------------------------------------------------------

/// Read the header, falling back to the backup copy if the live one is
/// unreadable.
///
/// This is the single most valuable file in the vault: it holds the wrapped
/// data key, which exists nowhere else. If it is lost, every entry in the
/// store is ciphertext under a key nobody can derive any more -- so a
/// header that fails to parse is worth a second look at the spare before
/// reporting that there is no vault here.
fn read_header(root: &Path) -> Result<VaultHeader> {
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
fn write_header(root: &Path, header: &VaultHeader) -> Result<()> {
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

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn from_hex(s: &str) -> Result<Vec<u8>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::conformance;
    use crate::store::{SortOrder, StoreFactory};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    // A minimal in-memory backend, so the vault can be tested without
    // depending on a concrete storage crate.
    #[derive(Default)]
    struct MemStore {
        cipher: Option<Arc<dyn Cipher>>,
        journals: Mutex<BTreeMap<JournalId, Vec<u8>>>,
        entries: Mutex<BTreeMap<EntryId, Vec<u8>>>,
        /// Sealed bytes and when they were stored. The timestamp exists
        /// for the same reason the file stores keep an mtime: GC has to be
        /// able to tell a settled orphan from a blob written a moment ago.
        blobs: Mutex<BTreeMap<BlobId, (Vec<u8>, std::time::Instant)>>,
    }

    impl MemStore {
        fn cipher(&self) -> &dyn Cipher {
            self.cipher.as_deref().unwrap()
        }
    }

    impl JournalStore for MemStore {
        fn backend(&self) -> &'static str {
            "memory"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                blobs: true,
                transactional: false,
                human_readable: false,
                max_blob_bytes: None,
                tasks: false,
                calendars: false,
                library: false,
            }
        }
        fn list_journals(&self) -> Result<Vec<Journal>> {
            self.journals
                .lock()
                .unwrap()
                .iter()
                .map(|(id, v)| {
                    let plain = self.cipher().open(&crate::store::journal_aad(*id), v)?;
                    Ok(serde_json::from_slice(&plain)?)
                })
                .collect()
        }
        fn get_journal(&self, id: JournalId) -> Result<Journal> {
            let g = self.journals.lock().unwrap();
            let v = g.get(&id).ok_or_else(|| Error::not_found("journal", id))?;
            let plain = self.cipher().open(&crate::store::journal_aad(id), v)?;
            Ok(serde_json::from_slice(&plain)?)
        }
        fn put_journal(&self, j: &Journal) -> Result<()> {
            let sealed =
                self.cipher().seal(&crate::store::journal_aad(j.id), &serde_json::to_vec(j)?)?;
            self.journals.lock().unwrap().insert(j.id, sealed);
            Ok(())
        }
        fn delete_journal(&self, id: JournalId) -> Result<()> {
            self.journals.lock().unwrap().remove(&id);
            let doomed: Vec<EntryId> = self
                .all_entries()?
                .into_iter()
                .filter(|e| e.journal_id == id)
                .map(|e| e.id)
                .collect();
            let mut g = self.entries.lock().unwrap();
            for d in doomed {
                g.remove(&d);
            }
            Ok(())
        }
        fn list_entries(&self, q: &EntryQuery) -> Result<Vec<EntrySummary>> {
            Ok(q.apply(self.all_entries()?.iter().map(Entry::summarize).collect()))
        }
        fn get_entry(&self, id: EntryId) -> Result<Entry> {
            let g = self.entries.lock().unwrap();
            let v = g.get(&id).ok_or_else(|| Error::not_found("entry", id))?;
            let plain = self.cipher().open(&crate::store::entry_aad(id), v)?;
            Ok(serde_json::from_slice(&plain)?)
        }
        fn put_entry(&self, e: &Entry) -> Result<()> {
            let sealed =
                self.cipher().seal(&crate::store::entry_aad(e.id), &serde_json::to_vec(e)?)?;
            self.entries.lock().unwrap().insert(e.id, sealed);
            Ok(())
        }
        fn delete_entry(&self, id: EntryId) -> Result<()> {
            self.entries.lock().unwrap().remove(&id);
            Ok(())
        }
        fn all_entries(&self) -> Result<Vec<Entry>> {
            self.entries
                .lock()
                .unwrap()
                .iter()
                .map(|(id, v)| {
                    let plain = self.cipher().open(&crate::store::entry_aad(*id), v)?;
                    Ok(serde_json::from_slice(&plain)?)
                })
                .collect()
        }
        fn put_blob(&self, bytes: &[u8]) -> Result<BlobId> {
            let id = BlobId::of(bytes);
            let sealed = self.cipher().seal(&crate::store::blob_aad(id), bytes)?;
            self.blobs.lock().unwrap().insert(id, (sealed, std::time::Instant::now()));
            Ok(id)
        }
        fn get_blob(&self, id: BlobId) -> Result<Vec<u8>> {
            let g = self.blobs.lock().unwrap();
            let (v, _) = g.get(&id).ok_or_else(|| Error::not_found("blob", id))?;
            self.cipher().open(&crate::store::blob_aad(id), v)
        }
        fn has_blob(&self, id: BlobId) -> Result<bool> {
            Ok(self.blobs.lock().unwrap().contains_key(&id))
        }
        fn blob_age(&self, id: BlobId) -> Result<Option<std::time::Duration>> {
            Ok(self.blobs.lock().unwrap().get(&id).map(|(_, at)| at.elapsed()))
        }
        fn delete_blob(&self, id: BlobId) -> Result<()> {
            self.blobs.lock().unwrap().remove(&id);
            Ok(())
        }
        fn list_blobs(&self) -> Result<Vec<BlobId>> {
            Ok(self.blobs.lock().unwrap().keys().copied().collect())
        }
        fn stats(&self) -> Result<StoreStats> {
            // Take each lock into its own binding: temporaries inside a
            // struct expression live to the end of the statement, so locking
            // `blobs` twice inline would deadlock on the second acquire.
            let journals = self.journals.lock().unwrap().len() as u64;
            let entries = self.entries.lock().unwrap().len() as u64;
            let blobs = self.blobs.lock().unwrap();
            Ok(StoreStats {
                journals,
                entries,
                blobs: blobs.len() as u64,
                blob_bytes: blobs.values().map(|(v, _)| v.len() as u64).sum(),
            })
        }
    }

    // Backing maps persist across open/close so that "reopen the vault"
    // tests exercise real decryption rather than a fresh empty store.
    #[derive(Default)]
    struct MemFactory {
        stores: Mutex<BTreeMap<PathBuf, Arc<MemStore>>>,
    }

    struct Handle(Arc<MemStore>);

    impl JournalStore for Handle {
        fn backend(&self) -> &'static str {
            self.0.backend()
        }
        fn capabilities(&self) -> Capabilities {
            self.0.capabilities()
        }
        fn list_journals(&self) -> Result<Vec<Journal>> {
            self.0.list_journals()
        }
        fn get_journal(&self, id: JournalId) -> Result<Journal> {
            self.0.get_journal(id)
        }
        fn put_journal(&self, j: &Journal) -> Result<()> {
            self.0.put_journal(j)
        }
        fn delete_journal(&self, id: JournalId) -> Result<()> {
            self.0.delete_journal(id)
        }
        fn list_entries(&self, q: &EntryQuery) -> Result<Vec<EntrySummary>> {
            self.0.list_entries(q)
        }
        fn get_entry(&self, id: EntryId) -> Result<Entry> {
            self.0.get_entry(id)
        }
        fn put_entry(&self, e: &Entry) -> Result<()> {
            self.0.put_entry(e)
        }
        fn delete_entry(&self, id: EntryId) -> Result<()> {
            self.0.delete_entry(id)
        }
        fn all_entries(&self) -> Result<Vec<Entry>> {
            self.0.all_entries()
        }
        fn put_blob(&self, b: &[u8]) -> Result<BlobId> {
            self.0.put_blob(b)
        }
        fn get_blob(&self, id: BlobId) -> Result<Vec<u8>> {
            self.0.get_blob(id)
        }
        fn has_blob(&self, id: BlobId) -> Result<bool> {
            self.0.has_blob(id)
        }
        fn blob_age(&self, id: BlobId) -> Result<Option<std::time::Duration>> {
            self.0.blob_age(id)
        }
        fn delete_blob(&self, id: BlobId) -> Result<()> {
            self.0.delete_blob(id)
        }
        fn list_blobs(&self) -> Result<Vec<BlobId>> {
            self.0.list_blobs()
        }
        fn stats(&self) -> Result<StoreStats> {
            self.0.stats()
        }
    }

    impl StoreFactory for Arc<MemFactory> {
        fn id(&self) -> &'static str {
            "memory"
        }
        fn describe(&self) -> &'static str {
            "in-memory (tests only)"
        }
        fn open(&self, ctx: StoreContext) -> Result<Box<dyn JournalStore>> {
            let mut g = self.stores.lock().unwrap();
            let store = g
                .entry(ctx.root.clone())
                .or_insert_with(|| Arc::new(MemStore { cipher: None, ..Default::default() }));
            // Rebind the cipher for this session.
            let rebound = Arc::new(MemStore {
                cipher: Some(ctx.cipher.clone()),
                journals: Mutex::new(store.journals.lock().unwrap().clone()),
                entries: Mutex::new(store.entries.lock().unwrap().clone()),
                blobs: Mutex::new(store.blobs.lock().unwrap().clone()),
            });
            g.insert(ctx.root, rebound.clone());
            Ok(Box::new(Handle(rebound)))
        }
    }

    fn registry() -> Arc<BackendRegistry> {
        let mut reg = BackendRegistry::new();
        reg.register(Arc::new(MemFactory::default()));
        Arc::new(reg)
    }

    fn cfg(password: Option<&str>) -> VaultConfig {
        VaultConfig {
            name: "Test".into(),
            backend: "memory".into(),
            password: password.map(str::to_string),
            kdf: KdfParams::insecure_fast(),
            auto_lock_seconds: 0,
        }
    }

    #[test]
    fn the_in_memory_backend_passes_the_conformance_suite() {
        // Proves the suite itself is satisfiable, and exercises it under the
        // real encrypting cipher.
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        v.with_store(|s| {
            conformance::run_all(s);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn a_new_vault_is_unlocked_and_reports_its_settings() {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        let s = v.status();
        assert!(s.unlocked);
        assert!(s.encrypted);
        assert_eq!(s.backend, "memory");
        assert_eq!(s.name, "Test");
        assert!(dir.path().join(HEADER_FILENAME).is_file());
    }

    #[test]
    fn entry_tags_are_counted_most_used_first() {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        let j = Journal::new("J");
        v.save_journal(&j).unwrap();

        for tags in [vec!["travel", "spain"], vec!["travel", "food"], vec!["travel"], vec!["food"]]
        {
            let mut e = Entry::new(j.id, "UTC");
            e.title = "x".into();
            e.tags = tags.into_iter().map(str::to_string).collect();
            v.save_entry(&e, None).unwrap();
        }

        // Most used first; "food" and "spain" tie at the bottom on count and
        // are broken alphabetically, which is what makes the order stable
        // enough to drive an autocomplete.
        assert_eq!(
            v.entry_tags().unwrap(),
            vec![("travel".to_string(), 3), ("food".to_string(), 2), ("spain".to_string(), 1),]
        );
    }

    #[test]
    fn entry_tags_needs_an_unlocked_vault() {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        v.lock();
        assert!(matches!(v.entry_tags().unwrap_err(), Error::Locked));
    }

    #[test]
    fn creating_over_an_existing_vault_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        let err = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap_err();
        assert_eq!(err.code(), "already_initialised");
    }

    #[test]
    fn opening_a_missing_vault_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let err = Vault::open(&dir.path().join("nowhere"), registry()).unwrap_err();
        assert_eq!(err.code(), "no_vault");
    }

    #[test]
    fn an_encrypted_vault_reopens_locked_and_needs_the_password() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry();
        {
            let v = Vault::create(dir.path(), cfg(Some("correct horse")), reg.clone()).unwrap();
            let j = Journal::new("Daily");
            v.save_journal(&j).unwrap();
            let mut e = Entry::new(j.id, "UTC");
            e.body = crate::RichDoc::from_plain_text("a secret");
            v.save_entry(&e, None).unwrap();
        }

        let v = Vault::open(dir.path(), reg).unwrap();
        assert!(!v.is_unlocked(), "an encrypted vault must reopen locked");
        assert_eq!(v.journals().unwrap_err().code(), "locked");
        assert_eq!(v.entries(&EntryQuery::default()).unwrap_err().code(), "locked");

        assert_eq!(v.unlock(Some("wrong")).unwrap_err().code(), "bad_password");
        assert_eq!(v.unlock(None).unwrap_err().code(), "bad_password");
        assert!(!v.is_unlocked(), "a failed unlock must not half-open the vault");

        v.unlock(Some("correct horse")).unwrap();
        assert!(v.is_unlocked());
        assert_eq!(v.journals().unwrap().len(), 1);
        assert_eq!(v.search("secret", None, 10).unwrap().len(), 1);
    }

    #[test]
    fn locking_denies_access_until_unlocked_again() {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        v.save_journal(&Journal::new("Daily")).unwrap();

        v.lock();
        assert!(!v.is_unlocked());
        assert_eq!(v.journals().unwrap_err().code(), "locked");
        assert_eq!(v.stats().unwrap_err().code(), "locked");
        assert_eq!(v.search("x", None, 5).unwrap_err().code(), "locked");
        assert_eq!(v.put_blob(b"x").unwrap_err().code(), "locked");

        // Status is still answerable while locked — the UI needs it.
        let s = v.status();
        assert!(!s.unlocked);
        assert!(s.stats.is_none(), "stats must not be readable while locked");

        v.unlock(Some("pw")).unwrap();
        assert_eq!(v.journals().unwrap().len(), 1);
    }

    #[test]
    fn an_unencrypted_vault_opens_without_a_password() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry();
        {
            let v = Vault::create(dir.path(), cfg(None), reg.clone()).unwrap();
            assert!(!v.status().encrypted);
            v.save_journal(&Journal::new("Open")).unwrap();
        }
        let v = Vault::open(dir.path(), reg).unwrap();
        assert!(v.is_unlocked());
        assert_eq!(v.journals().unwrap().len(), 1);
    }

    #[test]
    fn creating_an_encrypted_vault_rejects_an_empty_password() {
        let dir = tempfile::tempdir().unwrap();
        let err = Vault::create(dir.path(), cfg(Some("")), registry()).unwrap_err();
        assert_eq!(err.code(), "invalid");
        assert!(!Vault::exists(dir.path()), "a rejected create must leave no vault behind");
    }

    #[test]
    fn changing_the_password_keeps_the_data_readable() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry();
        let v = Vault::create(dir.path(), cfg(Some("old pw")), reg.clone()).unwrap();
        let j = Journal::new("Daily");
        v.save_journal(&j).unwrap();

        assert_eq!(
            v.change_password(Some("nope"), Some("new pw")).unwrap_err().code(),
            "bad_password"
        );
        v.change_password(Some("old pw"), Some("new pw")).unwrap();

        v.lock();
        assert_eq!(v.unlock(Some("old pw")).unwrap_err().code(), "bad_password");
        v.unlock(Some("new pw")).unwrap();
        assert_eq!(v.journals().unwrap()[0].name, "Daily");

        // And it survives a full reopen from disk.
        drop(v);
        let v = Vault::open(dir.path(), reg).unwrap();
        v.unlock(Some("new pw")).unwrap();
        assert_eq!(v.journals().unwrap()[0].name, "Daily");
    }

    #[test]
    fn removing_a_password_is_refused_rather_than_silently_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        let err = v.change_password(Some("pw"), None).unwrap_err();
        assert_eq!(err.code(), "unsupported");
        // The old password must still work after the refusal.
        v.lock();
        v.unlock(Some("pw")).unwrap();
    }

    #[test]
    fn on_disk_bytes_do_not_contain_the_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        let j = Journal::new("Daily");
        v.save_journal(&j).unwrap();

        // The header is plaintext by design, but must not leak the key or
        // any journal content.
        let header = std::fs::read_to_string(dir.path().join(HEADER_FILENAME)).unwrap();
        assert!(!header.contains("Daily"));
        assert!(header.contains("xchacha20poly1305"));
    }

    #[test]
    fn the_search_index_tracks_edits_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        let j = Journal::new("Daily");
        v.save_journal(&j).unwrap();

        let mut e = Entry::new(j.id, "UTC");
        e.body = crate::RichDoc::from_plain_text("kingfisher on the wire");
        v.save_entry(&e, None).unwrap();
        assert_eq!(v.search("kingfisher", None, 10).unwrap().len(), 1);

        // An edit, so it carries the version it is replacing. `None` here
        // would be the caller claiming the entry is new, and is a conflict.
        let loaded = e.updated_at;
        e.body = crate::RichDoc::from_plain_text("heron on the wire");
        e.updated_at = Timestamp::now();
        v.save_entry(&e, Some(loaded)).unwrap();
        assert!(v.search("kingfisher", None, 10).unwrap().is_empty(), "edit must reindex");
        assert_eq!(v.search("heron", None, 10).unwrap().len(), 1);

        v.delete_entry(e.id).unwrap();
        assert!(v.search("heron", None, 10).unwrap().is_empty(), "delete must deindex");
    }

    #[test]
    fn deleting_a_journal_removes_its_entries_from_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        let j = Journal::new("Daily");
        v.save_journal(&j).unwrap();
        let mut e = Entry::new(j.id, "UTC");
        e.body = crate::RichDoc::from_plain_text("kingfisher");
        v.save_entry(&e, None).unwrap();

        v.delete_journal(j.id).unwrap();
        assert!(v.search("kingfisher", None, 10).unwrap().is_empty());
        assert!(v.entries(&EntryQuery::default()).unwrap().is_empty());
    }

    #[test]
    fn saving_an_entry_rejects_a_malformed_body() {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        let j = Journal::new("Daily");
        v.save_journal(&j).unwrap();

        let mut e = Entry::new(j.id, "UTC");
        e.body = crate::RichDoc(serde_json::json!({"type": "paragraph"}));
        assert_eq!(v.save_entry(&e, None).unwrap_err().code(), "invalid");
        assert!(v.entries(&EntryQuery::default()).unwrap().is_empty());
    }

    #[test]
    fn journals_come_back_in_sidebar_order() {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        for (order, name) in [(2, "Third"), (0, "First"), (1, "Second")] {
            let mut j = Journal::new(name);
            j.sort_order = order;
            v.save_journal(&j).unwrap();
        }
        let names: Vec<String> = v.journals().unwrap().into_iter().map(|j| j.name).collect();
        assert_eq!(names, ["First", "Second", "Third"]);
    }

    #[test]
    fn auto_lock_is_off_when_the_timeout_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        assert_eq!(v.seconds_until_auto_lock(), None);
        assert!(!v.auto_lock_if_idle());
        assert!(v.is_unlocked());
    }

    #[test]
    fn auto_lock_fires_once_the_idle_timeout_elapses() {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        v.set_auto_lock(1).unwrap();
        assert!(!v.auto_lock_if_idle(), "should not lock while still fresh");

        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(v.auto_lock_if_idle(), "should lock after the timeout");
        assert!(!v.is_unlocked());
        // Once locked there is nothing left to auto-lock.
        assert_eq!(v.seconds_until_auto_lock(), None);
    }

    #[test]
    fn activity_defers_the_auto_lock() {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        v.set_auto_lock(2).unwrap();
        for _ in 0..3 {
            std::thread::sleep(std::time::Duration::from_millis(700));
            v.journals().unwrap(); // each call touches the activity clock
            assert!(!v.auto_lock_if_idle(), "activity should keep the vault open");
        }
    }

    #[test]
    fn the_auto_lock_setting_survives_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry();
        {
            let v = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
            v.set_auto_lock(300).unwrap();
        }
        let v = Vault::open(dir.path(), reg).unwrap();
        assert_eq!(v.header().auto_lock_seconds, 300);
    }

    #[test]
    fn a_newer_header_format_is_refused_rather_than_misread() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry();
        Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();

        let path = dir.path().join(HEADER_FILENAME);
        let mut header: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        header["format"] = serde_json::json!(FORMAT_VERSION + 1);
        std::fs::write(&path, serde_json::to_vec(&header).unwrap()).unwrap();

        let err = Vault::open(dir.path(), reg).unwrap_err();
        assert_eq!(err.code(), "unsupported_version");
    }

    #[test]
    fn a_corrupt_wrapped_key_is_reported_as_a_bad_password_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry();
        Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();

        let path = dir.path().join(HEADER_FILENAME);
        let mut header: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut wrapped = header["wrappedKey"].as_str().unwrap().to_string();
        wrapped.replace_range(0..2, "ff");
        header["wrappedKey"] = serde_json::json!(wrapped);
        std::fs::write(&path, serde_json::to_vec(&header).unwrap()).unwrap();

        let v = Vault::open(dir.path(), reg).unwrap();
        assert_eq!(v.unlock(Some("pw")).unwrap_err().code(), "bad_password");
    }

    #[test]
    fn a_lost_header_is_recovered_from_the_backup_copy() {
        // The worst survivable accident: the file holding the wrapped data
        // key is gone. Every entry in the store is still ciphertext under a
        // key that exists nowhere else, so the spare is the difference
        // between a vault that opens and one that never will again.
        let dir = tempfile::tempdir().unwrap();
        let reg = registry();
        let v = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
        let j = Journal::new("J");
        v.save_journal(&j).unwrap();
        // Creating the vault is enough: the spare is seeded on the first
        // header write, not the second.
        assert!(dir.path().join(HEADER_BACKUP_FILENAME).is_file());

        std::fs::remove_file(dir.path().join(HEADER_FILENAME)).unwrap();

        let v = Vault::open(dir.path(), reg).unwrap();
        v.unlock(Some("pw")).unwrap();
        assert_eq!(v.journals().unwrap().len(), 1, "the vault still reads");
        // Recovery also restores the live header, so this is a one-off.
        assert!(dir.path().join(HEADER_FILENAME).is_file());
    }

    #[test]
    fn a_corrupt_header_falls_back_rather_than_reporting_no_vault() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry();
        let v = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
        v.set_auto_lock(60).unwrap();

        std::fs::write(dir.path().join(HEADER_FILENAME), b"{ this is not json").unwrap();

        let v = Vault::open(dir.path(), reg).unwrap();
        v.unlock(Some("pw")).unwrap();
        assert!(v.is_unlocked());
    }

    #[test]
    fn a_vault_with_only_a_backup_header_is_not_overwritten_by_create() {
        // `create` refuses an existing vault, and "existing" has to include
        // one whose live header was lost -- otherwise the recovery path above
        // races a fresh vault written on top of the entries it was meant to
        // save.
        let dir = tempfile::tempdir().unwrap();
        let reg = registry();
        let v = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
        v.set_auto_lock(60).unwrap();
        std::fs::remove_file(dir.path().join(HEADER_FILENAME)).unwrap();

        assert_eq!(
            Vault::create(dir.path(), cfg(Some("pw")), reg).unwrap_err().code(),
            "already_initialised"
        );
    }

    #[test]
    fn the_backup_header_is_only_ever_a_readable_one() {
        // A corrupt live header must not be promoted over the last good
        // spare, or one bad write destroys both copies.
        let dir = tempfile::tempdir().unwrap();
        let reg = registry();
        let v = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
        v.set_auto_lock(60).unwrap();
        let good = std::fs::read(dir.path().join(HEADER_BACKUP_FILENAME)).unwrap();

        std::fs::write(dir.path().join(HEADER_FILENAME), b"corrupt").unwrap();
        // Any header write now would otherwise copy the corruption across.
        write_header(dir.path(), &read_header(dir.path()).unwrap()).unwrap();

        assert_eq!(
            std::fs::read(dir.path().join(HEADER_BACKUP_FILENAME)).unwrap(),
            good,
            "the spare must still be the last header that parsed"
        );
    }

    #[test]
    fn a_second_process_opens_read_only_rather_than_racing_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry();
        let first = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
        assert!(first.is_writable(), "the creator holds the write lock");

        // The same vault opened again -- another process, in production.
        let second = Vault::open(dir.path(), reg).unwrap();
        second.unlock(Some("pw")).unwrap();
        assert!(!second.is_writable(), "a second opener must not also be a writer");
        assert!(!second.status().writable);

        // It still reads. (What it reads is this backend's business: the
        // in-memory one snapshots its maps per open, so the *cross-instance*
        // visibility of a write is pinned in the SQLite crate, against a
        // store two handles genuinely share.)
        let j = Journal::new("Daily");
        first.save_journal(&j).unwrap();
        second.journals().expect("a read-only vault must still read");

        // And refuses every write, naming what is holding it.
        let err = second.save_journal(&Journal::new("Nope")).unwrap_err();
        assert_eq!(err.code(), "vault_in_use");
        assert!(err.to_string().contains("read-only"), "the message must explain itself");

        let e = Entry::new(j.id, "UTC");
        assert_eq!(second.save_entry(&e, None).unwrap_err().code(), "vault_in_use");
        assert_eq!(second.delete_entry(e.id).unwrap_err().code(), "vault_in_use");
        assert_eq!(second.put_blob(b"x").unwrap_err().code(), "vault_in_use");
        assert_eq!(second.set_auto_lock(30).unwrap_err().code(), "vault_in_use");
        assert_eq!(
            second.collect_garbage(std::time::Duration::ZERO).unwrap_err().code(),
            "vault_in_use"
        );
    }

    #[test]
    fn one_process_reopening_a_path_it_already_holds_gets_a_read_only_vault() {
        // Pinning the behaviour the desktop shell has to work around: the
        // lock is on an open file description, not on a process, so a second
        // `open` of a path this process already holds conflicts with itself.
        // `AppState::close` exists because of this -- the vault in hand has
        // to be released before another is opened, even the same one.
        let dir = tempfile::tempdir().unwrap();
        let reg = registry();
        let held = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
        assert!(held.is_writable());

        let again = Vault::open(dir.path(), reg.clone()).unwrap();
        assert!(!again.is_writable(), "reopening without closing must not get the lock");

        // Release the first, and a fresh open is writable again.
        drop(held);
        drop(again);
        assert!(Vault::open(dir.path(), reg).unwrap().is_writable());
    }

    #[test]
    fn the_write_lock_is_released_when_the_vault_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry();
        {
            let first = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
            assert!(first.is_writable());
        }
        // Closing the app must hand the vault back, or a crash would leave it
        // read-only until the machine was rebooted.
        let next = Vault::open(dir.path(), reg).unwrap();
        assert!(next.is_writable(), "the lock must be reclaimable after a close");
    }

    #[test]
    fn a_read_only_vault_can_still_be_backed_up() {
        // The whole point of degrading to read-only instead of refusing: the
        // things that cannot lose data still work. Backing up is the one that
        // matters most, since it is what someone reaches for when they are
        // worried.
        let dir = tempfile::tempdir().unwrap();
        let reg = registry();
        let _first = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();

        let second = Vault::open(dir.path(), reg).unwrap();
        second.unlock(Some("pw")).unwrap();
        assert!(!second.is_writable());

        // The in-memory backend has no snapshot, so this asserts on *which*
        // error: it must be the backend's limitation, not the write lock.
        let dest = tempfile::tempdir().unwrap();
        assert_eq!(second.backup(&dest.path().join("copy")).unwrap_err().code(), "unsupported");
    }

    #[test]
    fn saving_an_entry_someone_else_changed_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        let j = Journal::new("Daily");
        v.save_journal(&j).unwrap();

        let mut e = Entry::new(j.id, "UTC");
        e.body = crate::RichDoc::from_plain_text("what I wrote");
        v.save_entry(&e, None).unwrap();
        let loaded = e.updated_at;

        // Something else writes it -- the CLI, another machine, a stale tab.
        let mut theirs = e.clone();
        theirs.body = crate::RichDoc::from_plain_text("what they wrote");
        theirs.updated_at = Timestamp::now();
        v.save_entry(&theirs, Some(loaded)).unwrap();

        // Our editor still thinks it holds the current version.
        e.body = crate::RichDoc::from_plain_text("my later paragraph");
        e.updated_at = Timestamp::now();
        assert_eq!(v.save_entry(&e, Some(loaded)).unwrap_err().code(), "conflict");
        assert_eq!(
            v.entry(e.id).unwrap().body.plain_text(),
            "what they wrote",
            "the refused save must not have touched anything"
        );

        // "Keep mine" is the deliberate override, and it also has to put the
        // search index straight.
        v.overwrite_entry(&e).unwrap();
        assert_eq!(v.entry(e.id).unwrap().body.plain_text(), "my later paragraph");
        assert_eq!(v.search("paragraph", None, 10).unwrap().len(), 1);
        assert!(v.search("they", None, 10).unwrap().is_empty(), "the index must follow the write");
    }

    // The backup *round trip* is exercised against SQLite, in that crate:
    // the in-memory backend here has no on-disk form to snapshot, so it
    // reports `snapshot` as unsupported. What is checkable at this layer is
    // the guard in front of it.
    #[test]
    fn backing_up_over_an_existing_vault_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry();
        let v = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
        let other = tempfile::tempdir().unwrap();
        Vault::create(other.path(), cfg(Some("pw")), reg).unwrap();

        assert_eq!(v.backup(other.path()).unwrap_err().code(), "already_initialised");
    }

    #[test]
    fn a_malformed_header_hex_field_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry();
        Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();

        let path = dir.path().join(HEADER_FILENAME);
        let mut header: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        header["salt"] = serde_json::json!("zzzz");
        std::fs::write(&path, serde_json::to_vec(&header).unwrap()).unwrap();

        let v = Vault::open(dir.path(), reg).unwrap();
        assert_eq!(v.unlock(Some("pw")).unwrap_err().code(), "invalid");
    }

    #[test]
    fn creating_with_an_unknown_backend_fails_before_touching_disk() {
        let dir = tempfile::tempdir().unwrap();
        let bad = VaultConfig { backend: "postgres".into(), ..cfg(Some("pw")) };
        assert_eq!(
            Vault::create(dir.path(), bad, registry()).unwrap_err().code(),
            "unknown_backend"
        );
        assert!(!Vault::exists(dir.path()));
    }

    #[test]
    fn entry_queries_are_forwarded_to_the_backend() {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        let a = Journal::new("A");
        let b = Journal::new("B");
        v.save_journal(&a).unwrap();
        v.save_journal(&b).unwrap();

        for (j, day) in [(a.id, 1), (a.id, 2), (b.id, 3)] {
            let mut e = Entry::new(j, "UTC");
            e.local_date = jiff::civil::date(2025, 1, day);
            v.save_entry(&e, None).unwrap();
        }
        assert_eq!(v.entries(&EntryQuery::default()).unwrap().len(), 3);
        assert_eq!(v.entries(&EntryQuery::in_journal(a.id)).unwrap().len(), 2);
        let asc = EntryQuery { sort: SortOrder::DateAsc, limit: Some(1), ..Default::default() };
        assert_eq!(v.entries(&asc).unwrap()[0].local_date, jiff::civil::date(2025, 1, 1));
    }

    #[test]
    fn a_backend_without_calendars_says_so_rather_than_failing_obscurely() {
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        assert!(!v.supports_calendars());
        assert!(!v.status().capabilities.unwrap().calendars);

        assert_eq!(v.calendars().unwrap_err().code(), "unsupported");
        assert_eq!(
            v.events(&crate::store::calendars::EventQuery::default()).unwrap_err().code(),
            "unsupported",
        );
    }

    #[test]
    fn a_calendar_with_an_address_we_would_never_fetch_is_refused_on_the_way_in() {
        // Refused where it is typed, not an hour later by a background sync
        // nobody is watching.
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        let bad = crate::calendar::Calendar::subscribed("Sneaky", "file:///etc/passwd");
        assert_eq!(v.save_calendar(&bad).unwrap_err().code(), "invalid");

        let nameless = crate::calendar::Calendar::subscribed("  ", "https://example.com/x.ics");
        assert_eq!(v.save_calendar(&nameless).unwrap_err().code(), "invalid");
    }

    #[test]
    fn a_reply_that_is_not_a_calendar_never_replaces_one_that_is() {
        // The failure mode of an automatic sync that people actually notice:
        // a captive portal, an expired link, a 200 with an error page in it.
        // The guard has to fire before any store call, which is what this
        // asserts -- the in-memory backend here has no calendar store, so a
        // check made any later would report `unsupported` instead.
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        let err = v
            .sync_calendar_from_ics(
                crate::CalendarId::new(),
                "<!doctype html><title>Sign in to the wifi</title>",
                (jiff::civil::date(2026, 1, 1), jiff::civil::date(2026, 12, 31)),
                "UTC",
            )
            .unwrap_err();
        assert_eq!(err.code(), "invalid", "an HTML page is not a calendar; got {err}");
    }

    #[test]
    fn a_backend_without_tasks_says_so_rather_than_failing_obscurely() {
        // The Markdown vault's situation: journals, and nothing else. The
        // interface reads `supports_tasks` to hide the todo app; a call that
        // slips through anyway must name the reason, not panic or return an
        // empty list that reads as "you have no tasks".
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        assert!(!v.supports_tasks());
        assert!(!v.status().capabilities.unwrap().tasks);

        let err = v.tasks(&crate::store::tasks::TaskQuery::default()).unwrap_err();
        assert_eq!(err.code(), "unsupported", "got {err}");
        assert_eq!(v.projects().unwrap_err().code(), "unsupported");
        assert_eq!(v.task_stats(jiff::civil::date(2026, 3, 10)).unwrap_err().code(), "unsupported");
    }

    #[test]
    fn a_locked_vault_refuses_task_reads_before_it_refuses_the_backend() {
        // "Locked" has to win over "unsupported": which backend is in use is
        // not something a locked vault should be answering questions about.
        let dir = tempfile::tempdir().unwrap();
        let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
        v.lock();
        assert_eq!(
            v.tasks(&crate::store::tasks::TaskQuery::default()).unwrap_err().code(),
            "locked"
        );
        assert!(!v.supports_tasks(), "a locked vault supports nothing");
    }

    #[test]
    fn hex_helpers_round_trip_and_reject_junk() {
        let bytes = [0u8, 1, 15, 16, 255];
        assert_eq!(from_hex(&to_hex(&bytes)).unwrap(), bytes);
        assert!(from_hex("abc").is_err(), "odd length");
        assert!(from_hex("zz").is_err(), "non-hex digits");
    }
}
