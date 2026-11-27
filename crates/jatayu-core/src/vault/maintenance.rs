//! Search, blob storage, and the vault's upkeep: garbage collection, an
//! integrity check, backups, and rebuilding the search index.

use super::header::write_header;
use super::index::IndexSlot;
use super::{STORE_DIRNAME, Vault};
use crate::error::{Error, Result};
use crate::id::BlobId;
use crate::model::Entry;
use crate::search::{SearchHit, SearchIndex, SearchScope};
use crate::store::{IntegrityJob, JournalStore, StoreStats};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

/// How long after the first unlock the integrity check starts reading. The
/// minutes after a start are when the window loads and the night's mail
/// comes in; a check that reads the whole database then competes with both
/// for the disk. Nothing in a test waits that long.
const INTEGRITY_DELAY: Duration =
    if cfg!(test) { Duration::ZERO } else { Duration::from_secs(3 * 60) };

/// How long a clean check vouches for the store: a vault opened again within
/// it is not read through again.
const INTEGRITY_FRESH: Duration = Duration::from_secs(24 * 60 * 60);

/// Beside the header: when the last check that found nothing wrong
/// finished, in seconds since the epoch -- and, under this name, that no
/// process able to write the vault has had the store open since without
/// closing it again. See [`OpenStamp`].
pub(super) const INTEGRITY_STAMP_FILENAME: &str = "vault.checked";

/// The same stamp while a process that can write the vault has the store
/// open. Moved back to [`INTEGRITY_STAMP_FILENAME`] when the store closes;
/// a process that dies with it open -- an abort, a power cut -- leaves the
/// stamp under this name, which the next writable opening does not trust.
pub(super) const INTEGRITY_STAMP_OPEN_FILENAME: &str = "vault.checked.open";

/// A vault's integrity check, started by its first unlock. See
/// [`Vault::start_integrity_check`].
pub(super) struct IntegrityCheck {
    cancel: Arc<AtomicBool>,
    /// `None` when there was nothing to wait for: the store had nothing to
    /// check, a clean check ran recently, or the check could not be started.
    thread: Option<JoinHandle<()>>,
}

impl IntegrityCheck {
    /// Interrupt the check if it is still going -- or still waiting to
    /// start -- and wait for its thread.
    fn stop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

/// Whether the stamp at `stamp` says a clean check finished less than
/// [`INTEGRITY_FRESH`] ago. A stamp that cannot be read, or that is from the
/// future, says no.
fn checked_recently(stamp: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(stamp) else {
        return false;
    };
    let Ok(secs) = text.trim().parse::<u64>() else { return false };
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
    SystemTime::now().duration_since(at).is_ok_and(|age| age < INTEGRITY_FRESH)
}

/// Say that a check of `root` just finished and found nothing wrong.
///
/// Under the open name: the check runs beside a store this process opened,
/// and the stamp only takes its closed name when that store closes cleanly
/// -- see [`OpenStamp`]. A check that finishes after the last close leaves
/// it under the open name, which costs the next opening a check and nothing
/// worse.
fn stamp_checked(root: &Path) {
    let secs = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let path = root.join(INTEGRITY_STAMP_OPEN_FILENAME);
    if let Err(e) = crate::fsutil::write_atomic(&path, secs.to_string().as_bytes(), "check") {
        tracing::debug!(error = %e, "could not note the integrity check's time");
    }
}

/// The integrity stamp, kept under its open name for as long as this
/// process -- one that can write the vault -- has the store open.
///
/// A clean check vouches for the store for a day, but only for as long as
/// every process that wrote to it since closed it again: the opening after
/// an abort or a power cut is the one the check is for. So the stamp is
/// moved aside before the store opens, and moved back by this guard's drop,
/// which every clean way of closing the store runs -- locking, the vault
/// being dropped, an unlock that failed after opening -- and which an abort
/// (release builds are `panic = "abort"`) or a power cut does not. It is
/// [`Unlocked`](super::session::Unlocked)'s last field, so it drops after
/// the store has closed.
pub(super) struct OpenStamp {
    root: PathBuf,
}

impl OpenStamp {
    /// Move `root`'s stamp, if it has one, to its open name.
    fn hold(root: &Path) -> Self {
        let closed = root.join(INTEGRITY_STAMP_FILENAME);
        match std::fs::rename(&closed, root.join(INTEGRITY_STAMP_OPEN_FILENAME)) {
            // Made to last before the store opens: a rename a power cut
            // undoes would bring the stamp back for the opening after it.
            Ok(()) => crate::fsutil::sync_dir(root),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                // Not left under the name that vouches for the store either
                // way; losing it costs the next opening a check.
                tracing::debug!(error = %e, "could not set the integrity stamp aside");
                let _ = std::fs::remove_file(&closed);
            }
        }
        Self { root: root.to_path_buf() }
    }
}

impl Drop for OpenStamp {
    fn drop(&mut self) {
        let open = self.root.join(INTEGRITY_STAMP_OPEN_FILENAME);
        match std::fs::rename(&open, self.root.join(INTEGRITY_STAMP_FILENAME)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::debug!(error = %e, "could not put the integrity stamp back"),
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
        self.index_ready()?;
        self.read(|u| Ok(u.index.read(|i| i.search_in(query, scope, limit)).unwrap_or_default()))
    }

    pub fn suggest_terms(&self, prefix: &str, limit: usize) -> Result<Vec<String>> {
        self.index_ready()?;
        self.read(|u| Ok(u.index.read(|i| i.terms_with_prefix(prefix, limit)).unwrap_or_default()))
    }

    /// Wait for a search index still being built after the unlock -- see
    /// `super::index` -- without holding the vault's lock while waiting, so
    /// nothing else queues behind a search. Bounded: past it, the search
    /// answers nothing rather than hang.
    fn index_ready(&self) -> Result<()> {
        let waiter = self.read(|u| Ok(u.index.waiter()))?;
        waiter.wait(super::index::READY_WAIT);
        Ok(())
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
        self.read(|u| {
            let mut stats = u.store.stats()?;
            stats.assistant_entries = super::assistant_entries(u.store.as_ref());
            Ok(stats)
        })
    }

    /// Reclaim attachments no entry references any more.
    ///
    /// Takes the *write* lock, not the read lock it used to. Collection is
    /// two passes -- walk the entries for live blob ids, then delete
    /// everything else -- and under a read lock a save landing between them
    /// stores a blob the first pass could not have seen and the second pass
    /// deletes. The write lock closes that window inside this process;
    /// `grace` is what covers the rest, including a picture pasted into a
    /// draft that has not been saved yet and a second process holding one
    /// open. A draft that *has* been saved is not left to `grace`: its
    /// attachments and pasted pictures are read from the draft itself, so a
    /// message written on Friday and scheduled for Monday still has them.
    /// See [`JournalStore::collect_garbage`].
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
    /// And not on every opening either: a check that found nothing is
    /// noted beside the header, and an opening within a day of it is not
    /// checked again -- an app restarted ten times an afternoon would
    /// otherwise read the whole database ten times -- unless a process died
    /// with the store open since, which is when the check matters most (see
    /// [`OpenStamp`]). When it does run, it waits a few minutes first, for
    /// the start-up's own reading to finish.
    ///
    /// A bad answer is said loudly and does not refuse anything. A damaged
    /// vault is precisely the one someone needs to get into -- to export
    /// what still reads, or to see how much of it survived -- and locking
    /// them out would turn recoverable damage into total loss. `jatayu
    /// check` reports the same findings on demand.
    ///
    /// Locking leaves the check running, since it holds no key and reads
    /// only the database's structure. Dropping the vault stops it, and so
    /// does [`Vault::flush`].
    ///
    /// `vouched` is [`Vault::hold_integrity_stamp`]'s answer, read before
    /// the store was opened.
    pub(super) fn start_integrity_check(&self, store: &dyn JournalStore, vouched: bool) {
        let mut slot = self.integrity_check.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        if vouched {
            tracing::debug!("storage integrity check skipped: a clean one ran within the day");
            *slot = Some(IntegrityCheck { cancel, thread: None });
            return;
        }
        let thread = match store.background_integrity_check(cancel.clone()) {
            Ok(Some(job)) => {
                // Only the process that may write the vault notes the time:
                // a read-only one keeps its hands off the folder.
                let stamp = self.is_writable().then(|| self.root.clone());
                spawn_integrity_check(job, cancel.clone(), stamp)
            }
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(error = %e, "could not run the integrity check");
                None
            }
        };
        *slot = Some(IntegrityCheck { cancel, thread });
    }

    /// Read the integrity stamp before the store is opened, and -- in a
    /// process that can write the vault -- set it aside for as long as the
    /// store is open. Answers whether a clean check vouches for the store,
    /// for [`Vault::start_integrity_check`], and the guard that puts the
    /// stamp back when the store closes. See [`OpenStamp`].
    pub(super) fn hold_integrity_stamp(&self) -> (bool, Option<OpenStamp>) {
        let closed = self.root.join(INTEGRITY_STAMP_FILENAME);
        let open = self.root.join(INTEGRITY_STAMP_OPEN_FILENAME);
        if !self.is_writable() {
            // Keeps its hands off the folder. It is read-only because
            // another process holds the vault, and that one's stamp, under
            // its open name, is as good as it was when that process opened
            // the store; if that process dies, the next one to write checks.
            return (checked_recently(&closed) || checked_recently(&open), None);
        }
        let vouched = checked_recently(&closed);
        let first = self.integrity_check.lock().unwrap_or_else(|e| e.into_inner()).is_none();
        if first && !vouched {
            // Left by a process that died with the store open. It vouches
            // for nothing now, and must not come back under the closed name
            // when this process closes the store before its own check has
            // finished. Later in the same opening, a stamp under the open
            // name is this process's own: a check that finished while the
            // vault was locked.
            let _ = std::fs::remove_file(&open);
        }
        (vouched, Some(OpenStamp::hold(&self.root)))
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
            let entries = u.store.all_entries()?;
            // See `super::meetings::transcript_texts`: a transcript is not
            // reachable from `all_notes` alone.
            let extras = super::meetings::transcript_texts(u.store.as_ref(), &notes)?;
            let index = SearchIndex::build_with_extras(&entries, &notes, &extras);
            let len = index.len();
            // Built here, not on a thread: whoever asked for this waits for
            // it. Replacing the slot stops a build the unlock left running.
            u.index = IndexSlot::ready(index);
            Ok(len)
        })
    }

    /// Escape hatch for operations that need the raw backend, e.g. export,
    /// or reading an attachment. Under the shared lock alone, so it never
    /// waits on a write; for writing through the backend, see
    /// [`Vault::with_store_writing`].
    pub fn with_store<T>(&self, f: impl FnOnce(&dyn JournalStore) -> Result<T>) -> Result<T> {
        self.read(|u| f(u.store.as_ref()))
    }

    /// [`Vault::with_store`] for a caller that writes through the raw
    /// backend -- an import, into every domain at once.
    ///
    /// Holds every domain gate as well as the shared lock, so the writes that
    /// take a gate instead of the exclusive lock wait for it, as they did
    /// when they were exclusive, and cannot land between one of its reads
    /// and the write that follows.
    pub fn with_store_writing<T>(
        &self,
        f: impl FnOnce(&dyn JournalStore) -> Result<T>,
    ) -> Result<T> {
        self.with_store_quiet(f)
    }

    /// [`Vault::with_store`] for a caller whose reads must agree with each
    /// other -- an export, which lists a domain's records in several reads
    /// (the accounts, then their transactions) and writes what it found as
    /// one archive.
    ///
    /// Holds every domain gate as well as the shared lock: the writers that
    /// take a gate instead of the exclusive lock wait for it, as they did
    /// when they were exclusive, so a bank sync cannot add an account
    /// between the read of the accounts and the read of its transactions.
    /// Readers still do not wait.
    pub fn with_store_quiet<T>(&self, f: impl FnOnce(&dyn JournalStore) -> Result<T>) -> Result<T> {
        self.read(|u| {
            let held = self.gates.all();
            let out = f(u.store.as_ref());
            drop(held);
            out
        })
    }
}

/// Run `job` on a thread of its own, after [`INTEGRITY_DELAY`], and log what
/// it finds. A clean answer is noted in `stamp`'s folder, when given one.
fn spawn_integrity_check(
    job: IntegrityJob,
    cancel: Arc<AtomicBool>,
    stamp: Option<PathBuf>,
) -> Option<JoinHandle<()>> {
    let spawned = std::thread::Builder::new().name("integrity-check".into()).spawn(move || {
        // Parked rather than slept, so stopping the check -- the vault
        // closing, a flush -- wakes it at once.
        if !INTEGRITY_DELAY.is_zero() {
            let until = Instant::now() + INTEGRITY_DELAY;
            while !cancel.load(Ordering::Relaxed) {
                let Some(left) = until.checked_duration_since(Instant::now()) else { break };
                std::thread::park_timeout(left);
            }
            if cancel.load(Ordering::Relaxed) {
                return;
            }
        }
        let started = Instant::now();
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
            Ok(_) => {
                tracing::debug!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "storage integrity check passed"
                );
                if let Some(root) = &stamp {
                    stamp_checked(root);
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::KdfParams;
    use crate::model::{Entry, Journal};
    use crate::store::BackendSettings;
    use crate::vault::VaultConfig;

    fn cfg() -> VaultConfig {
        VaultConfig {
            name: "Test".into(),
            backend: "memory".into(),
            settings: BackendSettings::default(),
            password: Some("pw".into()),
            kdf: KdfParams::insecure_fast(),
            auto_lock_seconds: 0,
            forget_key_seconds: 0,
        }
    }

    fn secs_ago(ago: u64) -> String {
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
        (now.as_secs() - ago).to_string()
    }

    #[test]
    fn a_gated_write_waits_for_a_quiet_read_of_the_store() {
        // An export lists a domain in several reads; a gated writer landing
        // between two of them makes the archive contradict itself.
        let dir = tempfile::tempdir().unwrap();
        let v = Arc::new(Vault::create(dir.path(), cfg(), crate::testing::registry()).unwrap());
        let j = Journal::new("Daily");
        v.save_journal(&j).unwrap();
        let e = Entry::new(j.id, "UTC");

        let (writer, during) = v
            .with_store_quiet(|store| {
                let writer = {
                    let (v, e) = (v.clone(), e.clone());
                    std::thread::spawn(move || v.save_entry(&e, None))
                };
                // Long enough for a save that did not wait to have landed.
                std::thread::sleep(Duration::from_millis(200));
                Ok((writer, store.get_entry(e.id).is_ok()))
            })
            .unwrap();
        assert!(!during, "the save waited for the quiet read to finish");
        writer.join().unwrap().unwrap();
        assert!(v.with_store(|s| s.get_entry(e.id)).is_ok(), "and landed after it");
    }

    #[test]
    fn the_stamp_is_set_aside_while_the_store_is_open() {
        let dir = tempfile::tempdir().unwrap();
        let (reg, probe) = crate::testing::registry_with_integrity_probe();
        drop(Vault::create(dir.path(), cfg(), reg.clone()).unwrap());
        assert_eq!(probe.started(), 1);
        let closed = dir.path().join(INTEGRITY_STAMP_FILENAME);
        let open = dir.path().join(INTEGRITY_STAMP_OPEN_FILENAME);
        std::fs::write(&closed, secs_ago(60 * 60)).unwrap();

        let v = Vault::open(dir.path(), reg.clone()).unwrap();
        v.unlock(Some("pw")).unwrap();
        assert_eq!(probe.started(), 1, "checked clean an hour ago");
        assert!(!closed.exists() && open.exists(), "set aside while the store is open");
        v.lock();
        assert!(closed.exists() && !open.exists(), "put back when the store closes");
        v.unlock(Some("pw")).unwrap();
        assert!(!closed.exists() && open.exists());

        // A process that finds the vault held reads it, and leaves the
        // stamp where the holder put it.
        let viewer = Vault::open(dir.path(), reg.clone()).unwrap();
        assert!(!viewer.is_writable());
        viewer.unlock(Some("pw")).unwrap();
        assert_eq!(probe.started(), 1, "the holder's stamp vouches for a viewer");
        drop(viewer);
        assert!(!closed.exists() && open.exists());

        drop(v);
        assert!(closed.exists() && !open.exists(), "put back when the vault is dropped");
    }

    #[test]
    fn a_process_that_died_with_the_store_open_leaves_it_to_be_checked() {
        let dir = tempfile::tempdir().unwrap();
        let (reg, probe) = crate::testing::registry_with_integrity_probe();
        drop(Vault::create(dir.path(), cfg(), reg.clone()).unwrap());
        let closed = dir.path().join(INTEGRITY_STAMP_FILENAME);
        let open = dir.path().join(INTEGRITY_STAMP_OPEN_FILENAME);

        // What an abort or a power cut an hour after a clean check leaves:
        // the stamp under its open name, never put back.
        std::fs::write(&open, secs_ago(60 * 60)).unwrap();
        let v = Vault::open(dir.path(), reg.clone()).unwrap();
        v.unlock(Some("pw")).unwrap();
        assert_eq!(probe.started(), 2, "checked again after a process died with it open");

        // Closed before that check finished: the dead process's stamp does
        // not come back as if it had.
        drop(v);
        assert!(!closed.exists() && !open.exists());
        let v = Vault::open(dir.path(), reg).unwrap();
        v.unlock(Some("pw")).unwrap();
        assert_eq!(probe.started(), 3);
    }

    #[test]
    fn a_clean_check_is_noted_under_the_open_name() {
        let dir = tempfile::tempdir().unwrap();
        stamp_checked(dir.path());
        assert!(checked_recently(&dir.path().join(INTEGRITY_STAMP_OPEN_FILENAME)));
        assert!(!dir.path().join(INTEGRITY_STAMP_FILENAME).exists());
        drop(OpenStamp { root: dir.path().to_path_buf() });
        assert!(checked_recently(&dir.path().join(INTEGRITY_STAMP_FILENAME)));
    }
}
