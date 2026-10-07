//! Search, blob storage, and the vault's upkeep: garbage collection, an
//! integrity check, backups, and rebuilding the search index.

use super::header::write_header;
use super::{STORE_DIRNAME, Vault};
use crate::error::{Error, Result};
use crate::id::BlobId;
use crate::model::Entry;
use crate::search::{SearchHit, SearchIndex, SearchScope};
use crate::store::{IntegrityJob, JournalStore, StoreStats};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

/// A vault's integrity check, started by its first unlock. See
/// [`Vault::start_integrity_check`].
pub(super) struct IntegrityCheck {
    cancel: Arc<AtomicBool>,
    /// `None` when there was nothing to wait for: the store had nothing to
    /// check, or the check could not be started.
    thread: Option<JoinHandle<()>>,
}

impl IntegrityCheck {
    /// Interrupt the check if it is still going, and wait for its thread.
    fn stop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

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

    /// Check `store`'s structure on a thread of its own, the first time this
    /// vault is unlocked.
    ///
    /// This used to run inside the unlock, and an unlock waited for it. That
    /// was free for a journal and is not for a vault that keeps mail: the
    /// check reads every page, and on a 5.6 GB database from a cold disk
    /// cache that is over twenty seconds of "Unlocking…" for an answer
    /// nothing was waiting on. The check never decided whether the vault
    /// opened -- see below -- so the unlock has no reason to wait for it.
    ///
    /// Once per opening rather than once per unlock. What it looks for is
    /// the damage a bad shutdown leaves, and between this process locking a
    /// vault and unlocking it again there has been no shutdown; reading
    /// gigabytes again every time the idle timer drops the key would only
    /// push everything else out of the disk cache.
    ///
    /// A bad answer is said loudly and does not refuse anything. A damaged
    /// vault is precisely the one someone needs to get into -- to export
    /// what still reads, or to see how much of it survived -- and locking
    /// them out would turn recoverable damage into total loss. `everyday
    /// check` reports the same findings on demand.
    ///
    /// Locking leaves the check running, since it holds no key and reads
    /// only the database's structure. Dropping the vault stops it, and so
    /// does [`Vault::flush`].
    pub(super) fn start_integrity_check(&self, store: &dyn JournalStore) {
        let mut slot = self.integrity_check.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let thread = match store.background_integrity_check(cancel.clone()) {
            Ok(Some(job)) => spawn_integrity_check(job, cancel.clone()),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(error = %e, "could not run the integrity check");
                None
            }
        };
        *slot = Some(IntegrityCheck { cancel, thread });
    }

    /// Stop the integrity check if it is still running, and wait for it.
    /// Not started again until the vault is next opened.
    pub(super) fn stop_integrity_check(&self) {
        let mut slot = self.integrity_check.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(check) = slot.as_mut() {
            check.stop();
        }
    }

    /// Move what has been written out of the write-ahead log and into the
    /// database itself. For a process about to stop -- the shell's close
    /// handshake, a server shutting down -- and for a backup.
    ///
    /// Stops the integrity check first. A checkpoint waits for every reader
    /// to catch up with the last write, and a check part way through the
    /// database will not until it has read the rest: the checkpoint sits out
    /// the store's five-second busy timeout and then gives up without having
    /// done anything. Whoever is flushing is about to stop, or about to read
    /// every page for a backup, so the check is not missed.
    pub fn flush(&self) -> Result<()> {
        self.stop_integrity_check();
        self.read(|u| u.store.flush())
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
            self.flush()?;
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
            // See `super::meetings::rebuild_meeting_index`: a transcript is
            // not reachable from `all_notes` alone.
            super::meetings::rebuild_meeting_index(u.store.as_ref(), &mut u.index)?;
            Ok(u.index.len())
        })
    }

    /// Escape hatch for operations that need the raw backend, e.g. import.
    pub fn with_store<T>(&self, f: impl FnOnce(&dyn JournalStore) -> Result<T>) -> Result<T> {
        self.read(|u| f(u.store.as_ref()))
    }
}

/// Run `job` on a thread of its own and log what it finds.
fn spawn_integrity_check(job: IntegrityJob, cancel: Arc<AtomicBool>) -> Option<JoinHandle<()>> {
    let started = Instant::now();
    let spawned = std::thread::Builder::new().name("integrity-check".into()).spawn(move || {
        let result = job();
        // Stopped because the vault was closed. Whatever came back is about
        // the interruption, not about the database.
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        match result {
            Ok(problems) if !problems.is_empty() => {
                tracing::error!(
                    count = problems.len(),
                    first = %problems[0],
                    "storage integrity check failed -- restore from a backup"
                );
            }
            Ok(_) => tracing::debug!(
                elapsed_ms = started.elapsed().as_millis() as u64,
                "storage integrity check passed"
            ),
            Err(e) => tracing::warn!(error = %e, "could not run the integrity check"),
        }
    });
    match spawned {
        Ok(thread) => Some(thread),
        Err(e) => {
            tracing::warn!(error = %e, "could not start the integrity check");
            None
        }
    }
}
