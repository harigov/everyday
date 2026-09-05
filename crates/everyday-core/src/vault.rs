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

use crate::crypto::{
    AeadCipher, Cipher, KdfParams, NullCipher, SUITE_NONE, SUITE_XCHACHA20_POLY1305, SecretKey,
    derive_key, random_salt, unwrap_key, wrap_key,
};
use crate::error::{Error, Result};
use crate::id::{BlobId, EntryId, JournalId};
use crate::model::{Entry, EntrySummary, Journal};
use crate::search::{SearchHit, SearchIndex};
use crate::store::{
    BackendRegistry, Capabilities, EntryQuery, JournalStore, StoreContext, StoreStats,
};
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
    /// `None` while locked — reading it would require decrypting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<StoreStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Capabilities>,
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
}

impl Vault {
    // ---- construction ---------------------------------------------------

    pub fn exists(root: &Path) -> bool {
        root.join(HEADER_FILENAME).is_file()
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
        let encrypted = header.is_encrypted();
        let vault = Self {
            root: root.to_path_buf(),
            header: RwLock::new(header),
            state: RwLock::new(None),
            registry,
            last_activity_ms: AtomicU64::new(0),
            epoch: Instant::now(),
        };
        if !encrypted {
            vault.activate(None)?;
        }
        Ok(vault)
    }

    // ---- locking --------------------------------------------------------

    pub fn is_unlocked(&self) -> bool {
        self.state.read().unwrap().is_some()
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
        let header = self.header.read().unwrap().clone();

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
        let header = self.header.read().unwrap().clone();
        // The cipher owns the only copy of the key material from here on;
        // `dek` is dropped (and zeroized) at the end of this function.
        let cipher: Arc<dyn Cipher> = match &dek {
            Some(k) => Arc::new(AeadCipher::new(k)),
            None => Arc::new(NullCipher),
        };

        let store_root = self.root.join(STORE_DIRNAME);
        std::fs::create_dir_all(&store_root).map_err(|e| Error::io(&store_root, e))?;
        let store = self
            .registry
            .open(&header.backend, StoreContext { root: store_root, cipher })?;

        let index = SearchIndex::build(&store.all_entries()?);

        *self.state.write().unwrap() = Some(Unlocked { store, index });
        self.touch();
        Ok(())
    }

    /// Lock the vault: close the backend, drop the index, zeroize the key.
    pub fn lock(&self) {
        // Take the state out and drop it outside the lock so that a slow
        // backend shutdown does not hold every reader.
        let previous = self.state.write().unwrap().take();
        drop(previous); // SecretKey zeroizes here; SearchIndex frees plaintext
    }

    /// Change the password, or add/remove encryption entirely.
    ///
    /// Only the *wrapped data key* is rewritten, so this is instant even for
    /// a vault with tens of thousands of entries.
    pub fn change_password(&self, current: Option<&str>, new: Option<&str>) -> Result<()> {
        let header = self.header.read().unwrap().clone();

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
        *self.header.write().unwrap() = next;

        // Same data key, so an unlocked session stays valid.
        Ok(())
    }

    pub fn set_auto_lock(&self, seconds: u64) -> Result<()> {
        let mut header = self.header.write().unwrap();
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
        let timeout = self.header.read().unwrap().auto_lock_seconds;
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
        self.header.read().unwrap().clone()
    }

    pub fn status(&self) -> VaultStatus {
        let header = self.header.read().unwrap().clone();
        let guard = self.state.read().unwrap();
        VaultStatus {
            name: header.name,
            backend: header.backend,
            unlocked: guard.is_some(),
            encrypted: header.cipher != SUITE_NONE,
            auto_lock_seconds: header.auto_lock_seconds,
            path: self.root.clone(),
            stats: guard.as_ref().and_then(|u| u.store.stats().ok()),
            capabilities: guard.as_ref().map(|u| u.store.capabilities()),
        }
    }

    // ---- data access ----------------------------------------------------

    /// Run `f` against the unlocked store, or fail with [`Error::Locked`].
    fn read<T>(&self, f: impl FnOnce(&Unlocked) -> Result<T>) -> Result<T> {
        let guard = self.state.read().unwrap();
        let unlocked = guard.as_ref().ok_or(Error::Locked)?;
        let out = f(unlocked);
        drop(guard);
        self.touch();
        out
    }

    fn write<T>(&self, f: impl FnOnce(&mut Unlocked) -> Result<T>) -> Result<T> {
        let mut guard = self.state.write().unwrap();
        let unlocked = guard.as_mut().ok_or(Error::Locked)?;
        let out = f(unlocked);
        drop(guard);
        self.touch();
        out
    }

    pub fn journals(&self) -> Result<Vec<Journal>> {
        self.read(|u| {
            let mut js = u.store.list_journals()?;
            js.sort_by(|a, b| {
                a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name))
            });
            Ok(js)
        })
    }

    pub fn journal(&self, id: JournalId) -> Result<Journal> {
        self.read(|u| u.store.get_journal(id))
    }

    pub fn save_journal(&self, journal: &Journal) -> Result<()> {
        self.write(|u| u.store.put_journal(journal))
    }

    /// Delete a journal and every entry inside it.
    pub fn delete_journal(&self, id: JournalId) -> Result<()> {
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

    pub fn save_entry(&self, entry: &Entry) -> Result<()> {
        entry.body.validate()?;
        self.write(|u| {
            u.store.put_entry(entry)?;
            u.index.insert(entry);
            Ok(())
        })
    }

    pub fn delete_entry(&self, id: EntryId) -> Result<()> {
        self.write(|u| {
            u.store.delete_entry(id)?;
            u.index.remove(id);
            Ok(())
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
        self.read(|u| u.store.put_blob(bytes))
    }

    pub fn blob(&self, id: BlobId) -> Result<Vec<u8>> {
        self.read(|u| u.store.get_blob(id))
    }

    pub fn stats(&self) -> Result<StoreStats> {
        self.read(|u| u.store.stats())
    }

    /// Reclaim attachments no entry references any more.
    pub fn collect_garbage(&self) -> Result<u64> {
        self.read(|u| u.store.collect_garbage())
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

fn read_header(root: &Path) -> Result<VaultHeader> {
    let path = root.join(HEADER_FILENAME);
    if !path.is_file() {
        return Err(Error::NoVault(root.to_path_buf()));
    }
    let bytes = std::fs::read(&path).map_err(|e| Error::io(&path, e))?;
    serde_json::from_slice(&bytes).map_err(Error::Serde)
}

/// Write the header via a temp file + rename, so an interrupted write can
/// never leave a vault that cannot be opened.
fn write_header(root: &Path, header: &VaultHeader) -> Result<()> {
    let path = root.join(HEADER_FILENAME);
    let tmp = root.join(format!("{HEADER_FILENAME}.tmp"));
    let json = serde_json::to_vec_pretty(header)?;
    std::fs::write(&tmp, &json).map_err(|e| Error::io(&tmp, e))?;
    std::fs::rename(&tmp, &path).map_err(|e| Error::io(&path, e))?;
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
        blobs: Mutex<BTreeMap<BlobId, Vec<u8>>>,
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
            self.blobs.lock().unwrap().insert(id, sealed);
            Ok(id)
        }
        fn get_blob(&self, id: BlobId) -> Result<Vec<u8>> {
            let g = self.blobs.lock().unwrap();
            let v = g.get(&id).ok_or_else(|| Error::not_found("blob", id))?;
            self.cipher().open(&crate::store::blob_aad(id), v)
        }
        fn has_blob(&self, id: BlobId) -> Result<bool> {
            Ok(self.blobs.lock().unwrap().contains_key(&id))
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
                blob_bytes: blobs.values().map(|v| v.len() as u64).sum(),
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
        fn backend(&self) -> &'static str { self.0.backend() }
        fn capabilities(&self) -> Capabilities { self.0.capabilities() }
        fn list_journals(&self) -> Result<Vec<Journal>> { self.0.list_journals() }
        fn get_journal(&self, id: JournalId) -> Result<Journal> { self.0.get_journal(id) }
        fn put_journal(&self, j: &Journal) -> Result<()> { self.0.put_journal(j) }
        fn delete_journal(&self, id: JournalId) -> Result<()> { self.0.delete_journal(id) }
        fn list_entries(&self, q: &EntryQuery) -> Result<Vec<EntrySummary>> { self.0.list_entries(q) }
        fn get_entry(&self, id: EntryId) -> Result<Entry> { self.0.get_entry(id) }
        fn put_entry(&self, e: &Entry) -> Result<()> { self.0.put_entry(e) }
        fn delete_entry(&self, id: EntryId) -> Result<()> { self.0.delete_entry(id) }
        fn all_entries(&self) -> Result<Vec<Entry>> { self.0.all_entries() }
        fn put_blob(&self, b: &[u8]) -> Result<BlobId> { self.0.put_blob(b) }
        fn get_blob(&self, id: BlobId) -> Result<Vec<u8>> { self.0.get_blob(id) }
        fn has_blob(&self, id: BlobId) -> Result<bool> { self.0.has_blob(id) }
        fn delete_blob(&self, id: BlobId) -> Result<()> { self.0.delete_blob(id) }
        fn list_blobs(&self) -> Result<Vec<BlobId>> { self.0.list_blobs() }
        fn stats(&self) -> Result<StoreStats> { self.0.stats() }
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
            let store = g.entry(ctx.root.clone()).or_insert_with(|| {
                Arc::new(MemStore { cipher: None, ..Default::default() })
            });
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
            v.save_entry(&e).unwrap();
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

        assert_eq!(v.change_password(Some("nope"), Some("new pw")).unwrap_err().code(),
                   "bad_password");
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
        v.save_entry(&e).unwrap();
        assert_eq!(v.search("kingfisher", None, 10).unwrap().len(), 1);

        e.body = crate::RichDoc::from_plain_text("heron on the wire");
        v.save_entry(&e).unwrap();
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
        v.save_entry(&e).unwrap();

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
        assert_eq!(v.save_entry(&e).unwrap_err().code(), "invalid");
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
        assert_eq!(Vault::create(dir.path(), bad, registry()).unwrap_err().code(),
                   "unknown_backend");
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
            v.save_entry(&e).unwrap();
        }
        assert_eq!(v.entries(&EntryQuery::default()).unwrap().len(), 3);
        assert_eq!(v.entries(&EntryQuery::in_journal(a.id)).unwrap().len(), 2);
        let asc = EntryQuery { sort: SortOrder::DateAsc, limit: Some(1), ..Default::default() };
        assert_eq!(v.entries(&asc).unwrap()[0].local_date, jiff::civil::date(2025, 1, 1));
    }

    #[test]
    fn hex_helpers_round_trip_and_reject_junk() {
        let bytes = [0u8, 1, 15, 16, 255];
        assert_eq!(from_hex(&to_hex(&bytes)).unwrap(), bytes);
        assert!(from_hex("abc").is_err(), "odd length");
        assert!(from_hex("zz").is_err(), "non-hex digits");
    }
}
