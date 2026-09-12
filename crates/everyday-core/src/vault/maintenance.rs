//! Search, blob storage, and the vault's upkeep: garbage collection, an
//! integrity check, backups, and rebuilding the search index.

use super::header::write_header;
use super::{STORE_DIRNAME, Vault};
use crate::error::{Error, Result};
use crate::id::BlobId;
use crate::model::Entry;
use crate::search::{SearchHit, SearchIndex, SearchScope};
use crate::store::{JournalStore, StoreStats};
use std::path::Path;

impl Vault {
    /// Search everything this vault can be searched for.
    ///
    /// One index over entries and notes both, so a half-remembered phrase is
    /// found wherever it was written down. [`SearchScope`] narrows it when
    /// the asking is from inside one app.
    pub fn search(&self, query: &str, scope: SearchScope, limit: usize) -> Result<Vec<SearchHit>> {
        self.read(|u| Ok(u.index.search_in(query, scope, limit)))
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
            let notes = match u.store.notes() {
                Some(n) => n.all_notes()?,
                None => Vec::new(),
            };
            u.index = SearchIndex::build(&u.store.all_entries()?, &notes);
            Ok(u.index.len())
        })
    }

    /// Escape hatch for operations that need the raw backend, e.g. import.
    pub fn with_store<T>(&self, f: impl FnOnce(&dyn JournalStore) -> Result<T>) -> Result<T> {
        self.read(|u| f(u.store.as_ref()))
    }
}
