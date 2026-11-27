//! The background task that keeps the files catalog in step with the files
//! folder.
//!
//! A pass ([`scan_now`]) walks the folder, compares it with the catalog
//! ([`jatayu_core::files::reconcile`]), hashes what is new or changed,
//! matches moves by hash, writes the outcome ([`Vault::apply_scan`]), then
//! reads metadata and draws thumbnails for what is new
//! ([`super::probe_and_store`]) and announces a [`Kind::File`] change.
//!
//! When: shortly after unlock, every [`EVERY`], and a few seconds after the
//! watcher ([`super::watch`]) reports a change. A pass never runs while the
//! folder is unavailable, and one pass at a time. Once an hour it also
//! empties the trash of what has been in it longer than the settings keep
//! it, and sweeps stale uploads and decrypted copies.
//!
//! # A first pass over a large folder
//!
//! Pointing Jatayu at a folder that already holds fifty thousand photos
//! is the heaviest thing this does, and it must not be one write after an
//! hour of hashing: nothing would appear until the end, and a vault that
//! locked halfway would have kept none of it. So what is new is hashed and
//! written [`BATCH`] files at a time, each batch announced and drawn before
//! the next is read, and the pass stops between files when the vault locks.
//!
//! [`Kind::File`]: crate::events::Kind::File

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use jatayu_core::Vault;
use jatayu_core::files::disk::{self, DiskFile, Folder};
use jatayu_core::files::reconcile;
use jatayu_core::files::{
    ChangedFile, FolderState, IndexRow, MovedFile, NewFile, ScanChanges, ScanReport,
};
use jatayu_core::id::FileId;
use jiff::Timestamp;
use tokio::sync::{Notify, watch};

use super::{probe_and_store, still_open};
use crate::events::{Change, Kind, Op};
use crate::service::Service;
use crate::supervisor::{Outcome, TaskResult};

/// The supervisor's key for this task.
pub const TASK_KEY: &str = "files";

/// How often a pass runs when nothing prompts one.
pub const EVERY: Duration = Duration::from_secs(5 * 60);

/// How long after the vault unlocks the first pass starts: long enough for
/// the windows to open and mail to have its first look, which is what
/// somebody unlocking is waiting for.
const FIRST: Duration = Duration::from_secs(10);

/// How long the folder must stay quiet before a pass that a change prompted
/// starts, so a sync tool's burst of writes is one pass and not twenty --
/// and so a file is not read while it is still being written.
const QUIET: Duration = Duration::from_secs(3);

/// The longest a steady stream of changes can hold a pass off: a download of
/// a large film writes for minutes, and the folder is not on that account
/// to go unread for as long.
const MOST_QUIET: Duration = Duration::from_secs(30);

/// How often the trash is emptied of what has been in it too long, and
/// stale uploads and decrypted copies are swept.
const HOUSEKEEPING_EVERY: Duration = Duration::from_secs(60 * 60);

/// How old a plain upload nobody finished must be before it is swept.
const STALE_UPLOADS: Duration = Duration::from_secs(24 * 60 * 60);

/// How old a decrypted copy of a sealed file handed to another program, or the
/// staging of a sealed upload nobody finished, must be before it is swept.
/// An hour, where an upload may wait a day: these are plaintext of files the
/// person chose to keep encrypted, they are also deleted when the vault locks,
/// and a copy is made afresh each time a file is opened. Also what
/// [`Service::set`] clears from the cache directory when a vault is opened.
pub(crate) const STALE_PLAINTEXT: Duration = Duration::from_secs(60 * 60);

/// How many files a pass hashes and writes before it announces what it has
/// and goes on. See the module docs.
const BATCH: usize = 200;

/// How many bytes of a new file's start are read to tell what it is.
const HEAD: u64 = 512;

/// The most errors one pass's report names; the rest are only logged.
const MAX_ERRORS: usize = 20;

/// One for the process, not one per vault: the watcher that rings it
/// belongs to the running task, which belongs to the vault open now, and a
/// ring left over from a vault just closed costs the next one a pass early,
/// nothing more.
fn wake_signal() -> &'static Arc<Notify> {
    static WAKE: OnceLock<Arc<Notify>> = OnceLock::new();
    WAKE.get_or_init(|| Arc::new(Notify::new()))
}

/// What is known of one vault's passes.
///
/// Kept per vault, by its path, not once for the process: the desktop can
/// close one vault and open another, and one vault's pass must neither hold
/// up the other's nor be reported as the other's last.
#[derive(Default)]
struct Scans {
    /// Held for the whole of a pass: one at a time, whoever asks.
    pass: Arc<Mutex<()>>,
    scanning: Arc<AtomicBool>,
    last: Option<(Timestamp, ScanReport)>,
}

fn scans() -> std::sync::MutexGuard<'static, HashMap<PathBuf, Scans>> {
    static SCANS: OnceLock<Mutex<HashMap<PathBuf, Scans>>> = OnceLock::new();
    SCANS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// This vault's pass lock and its "scanning" flag.
fn handles(vault: &Vault) -> (Arc<Mutex<()>>, Arc<AtomicBool>) {
    let mut all = scans();
    let entry = all.entry(vault.path().to_path_buf()).or_default();
    (entry.pass.clone(), entry.scanning.clone())
}

/// Start the task if it is not already running. Idempotent; called from
/// every unlock ([`Service::start_refreshers`]).
///
/// Only a process that holds the vault's write claim runs it -- a second
/// window that merely reads must not move somebody's files into the catalog
/// twice -- and only for a vault that keeps a catalog at all.
pub fn ensure_task(svc: &Arc<Service>, vault: &Arc<Vault>) {
    if !vault.is_writable() || !vault.supports_files() {
        return;
    }
    // Looks the vault up each time it starts rather than keeping one: see
    // `calendar_refresh::ensure_task` for why a vault held by a factory the
    // supervisor keeps for good would be wrong.
    let svc_for_task = svc.clone();
    svc.supervisor().ensure(TASK_KEY, move |stop| {
        let svc = svc_for_task.clone();
        Box::pin(async move {
            match svc.get() {
                Some(vault) if vault.is_writable() && vault.supports_files() => {
                    run(svc, vault, stop).await
                }
                _ => Ok(Outcome::Done),
            }
        })
    });
}

/// Ask for a pass soon: after a change the watcher saw, or a write that
/// bypassed the catalog.
pub fn wake() {
    wake_signal().notify_one();
}

/// Whether a pass of this vault is running now.
pub fn scanning(vault: &Vault) -> bool {
    scans().get(vault.path()).is_some_and(|s| s.scanning.load(Ordering::Relaxed))
}

/// When this vault's last pass finished, and what it found.
pub fn last_scan(vault: &Vault) -> Option<(Timestamp, ScanReport)> {
    scans().get(vault.path()).and_then(|s| s.last.clone())
}

/// When the next pass is due after a wake at `now`, in a burst of wakes
/// that began at `first`: [`QUIET`] from now, but never later than
/// [`MOST_QUIET`] from the first of them.
fn after_wake(now: tokio::time::Instant, first: tokio::time::Instant) -> tokio::time::Instant {
    (now + QUIET).min(first + MOST_QUIET)
}

/// The supervised loop: a pass [`FIRST`] after it starts, then every
/// [`EVERY`], and [`QUIET`] after a wake. Returns when `stop` turns `true`.
async fn run(svc: Arc<Service>, vault: Arc<Vault>, mut stop: watch::Receiver<bool>) -> TaskResult {
    let wake = wake_signal().clone();
    let now = tokio::time::Instant::now();
    let mut pass_at = now + FIRST;
    let mut housekeeping_at = now + FIRST;
    // When the wakes the next pass answers began; `None` while the next pass
    // is only the periodic one.
    let mut burst: Option<tokio::time::Instant> = None;
    let mut watching = Watching::default();
    loop {
        if *stop.borrow() {
            return Ok(Outcome::Done);
        }
        tokio::select! {
            stopping = stop.changed() => {
                if stopping.is_err() {
                    return Ok(Outcome::Done);
                }
            }
            () = wake.notified() => {
                let now = tokio::time::Instant::now();
                let first = *burst.get_or_insert(now);
                pass_at = after_wake(now, first);
            }
            () = tokio::time::sleep_until(pass_at) => {
                let now = tokio::time::Instant::now();
                burst = None;
                pass_at = now + EVERY;
                if !vault.is_unlocked() {
                    continue;
                }
                // Watching starts before the pass, so what changes while it
                // runs is not missed.
                follow_folder(&vault, &mut watching).await;
                let housekeeping = housekeeping_at <= now;
                if housekeeping {
                    housekeeping_at = now + HOUSEKEEPING_EVERY;
                }
                let (svc, vault) = (svc.clone(), vault.clone());
                let _ = tokio::task::spawn_blocking(move || {
                    scan_now(&svc, &vault);
                    if housekeeping {
                        sweep(&svc, &vault);
                    }
                })
                .await;
            }
        }
    }
}

/// The watch on the folder, and which folder it is on.
#[derive(Default)]
struct Watching {
    /// The folder the last look found usable; `None` when there was none.
    folder: Option<String>,
    /// `None` when the folder cannot be watched -- or there is none.
    watch: Option<super::watch::Watch>,
}

/// Watch the folder the settings name while it is usable, and a new one
/// when they name another; let go of the watch while there is none. A
/// folder that could not be watched is not tried again until it changes: a
/// tree too large for the system's watches would fail the same way every
/// five minutes.
async fn follow_folder(vault: &Arc<Vault>, watching: &mut Watching) {
    let reading = vault.clone();
    let wanted = tokio::task::spawn_blocking(move || match reading.folder_state() {
        Ok(FolderState::Available { path }) => Some(path),
        _ => None,
    })
    .await
    .ok()
    .flatten();
    if wanted == watching.folder {
        return;
    }
    watching.watch = None;
    watching.folder = wanted.clone();
    if let Some(path) = wanted {
        watching.watch = tokio::task::spawn_blocking(move || super::watch::start(Path::new(&path)))
            .await
            .ok()
            .flatten();
    }
}

/// Resets a vault's "scanning" flag however a pass ends.
struct Scanning(Arc<AtomicBool>);

impl Drop for Scanning {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Relaxed);
    }
}

/// Run a pass now, on this thread (blocking), and answer what it did.
/// Waits for a pass already running to finish rather than running two.
pub fn scan_now(svc: &Arc<Service>, vault: &Arc<Vault>) -> ScanReport {
    let (pass_lock, flag) = handles(vault);
    let _turn = pass_lock.lock().unwrap_or_else(|e| e.into_inner());
    flag.store(true, Ordering::Relaxed);
    let _scanning = Scanning(flag);
    let (report, worth_recording) = pass(svc, vault);
    if worth_recording {
        scans().entry(vault.path().to_path_buf()).or_default().last =
            Some((Timestamp::now(), report.clone()));
    }
    // What the documents' index is built from has changed: it has a pass to
    // make. A pass that found nothing new does not ring it.
    if report.added + report.changed + report.moved + report.missing + report.returned > 0 {
        crate::documents::task::wake();
    }
    report
}

/// One pass. The report, and whether it is a pass worth showing as the last
/// one: not when there was no folder to look at, which is nobody's scan.
fn pass(svc: &Arc<Service>, vault: &Arc<Vault>) -> (ScanReport, bool) {
    let mut report = ScanReport::default();
    let folder = match vault.files_folder() {
        Ok(Some(folder)) => folder,
        Ok(None) => return (report, false),
        Err(e) => {
            note(&mut report, e);
            return (report, false);
        }
    };
    match vault.folder_state() {
        Ok(state) if state.is_available() => {
            // A `.stignore` written or replaced since the folder was set up
            // must still keep the plaintext in the internal folder from being
            // shared.
            if let Err(e) = folder.ensure_ignored() {
                tracing::warn!(error = %e, "files: could not keep the internal folder out of sync");
            }
        }
        // Nothing is compared with a folder that is not there: a locked
        // volume is not a person who deleted every file.
        Ok(_) => {
            report.unavailable = true;
            return (report, true);
        }
        Err(e) => {
            note(&mut report, e);
            return (report, false);
        }
    }
    let (index, listing) = match (vault.file_index(), folder.walk()) {
        (Ok(index), Ok(listing)) => (index, listing),
        (Err(e), _) | (_, Err(e)) => {
            note(&mut report, e);
            return (report, true);
        }
    };

    let plan = reconcile::plan(&index, &listing);
    report.unchanged = u32::try_from(plan.unchanged.len()).unwrap_or(u32::MAX);
    let rows: HashMap<FileId, &IndexRow> = index.iter().map(|r| (r.id, r)).collect();

    // What moved while nobody watched. A file keeps its size when it moves,
    // so only what is new and as large as something that vanished is hashed
    // to find out -- the rest is hashed in its own turn below.
    let vanished_sizes: HashSet<u64> =
        plan.vanished.iter().filter_map(|id| rows.get(id)).map(|r| r.size).collect();
    let vanished_known: Vec<(FileId, String, String)> = plan
        .vanished
        .iter()
        .filter_map(|id| rows.get(id))
        .filter(|row| !row.hash.is_empty())
        .map(|row| (row.id, row.hash.clone(), row.path.clone()))
        .collect();
    let mut hashes: HashMap<String, String> = HashMap::new();
    if !vanished_known.is_empty() {
        for file in plan.new.iter().filter(|f| vanished_sizes.contains(&f.size)) {
            if !still_open(svc, vault) {
                note(&mut report, "stopped: the vault locked");
                return (report, true);
            }
            match folder.hash(&file.path) {
                Ok(hash) => {
                    hashes.insert(file.path.clone(), hash);
                }
                Err(e) => note(&mut report, format!("{}: {e}", file.path)),
            }
        }
    }
    let candidates: Vec<(String, String)> =
        hashes.iter().map(|(path, hash)| (path.clone(), hash.clone())).collect();
    let matched = reconcile::match_moves(&vanished_known, &candidates);

    let on_disk: HashMap<&str, &DiskFile> = listing.iter().map(|f| (f.path.as_str(), f)).collect();
    let moved: Vec<MovedFile> = matched
        .iter()
        .filter_map(|(id, path)| {
            let file = on_disk.get(path.as_str())?;
            Some(MovedFile {
                id: *id,
                path: path.clone(),
                size: file.size,
                modified_us: file.modified_us,
            })
        })
        .collect();
    let moved_ids: HashSet<FileId> = moved.iter().map(|m| m.id).collect();
    let moved_paths: HashSet<&str> = moved.iter().map(|m| m.path.as_str()).collect();

    let mut vanished: Vec<FileId> =
        plan.vanished.iter().copied().filter(|id| !moved_ids.contains(id)).collect();
    // So many gone at once that the folder is probably not what it was:
    // nothing is marked missing, and the next pass looks again.
    let present = index.iter().filter(|r| !r.missing).count();
    if reconcile::held_back(present, vanished.len()) {
        report.held_back = true;
        tracing::warn!(
            vanished = vanished.len(),
            present,
            "files: so many files are gone that none is marked missing"
        );
        vanished.clear();
    }

    let mut changed_work: VecDeque<&(FileId, DiskFile)> = plan.changed.iter().collect();
    let mut new_work: VecDeque<&DiskFile> =
        plan.new.iter().filter(|f| !moved_paths.contains(f.path.as_str())).collect();
    let mut changes = ScanChanges {
        moved,
        vanished,
        returned: plan.returned.iter().map(|(id, _)| *id).collect(),
        ..ScanChanges::default()
    };

    loop {
        let mut room = BATCH;
        while room > 0 {
            let Some((id, file)) = changed_work.pop_front() else { break };
            if !still_open(svc, vault) {
                note(&mut report, "stopped: the vault locked");
                return (report, true);
            }
            match folder.hash(&file.path) {
                Ok(hash) => changes.changed.push(ChangedFile {
                    id: *id,
                    size: file.size,
                    modified_us: file.modified_us,
                    hash,
                }),
                Err(e) => note(&mut report, format!("{}: {e}", file.path)),
            }
            room -= 1;
        }
        while room > 0 {
            let Some(file) = new_work.pop_front() else { break };
            if !still_open(svc, vault) {
                note(&mut report, "stopped: the vault locked");
                return (report, true);
            }
            match new_file(&folder, file, &mut hashes) {
                Ok(new) => changes.new.push(new),
                Err(e) => note(&mut report, format!("{}: {e}", file.path)),
            }
            room -= 1;
        }
        let last = changed_work.is_empty() && new_work.is_empty();
        if !is_empty(&changes) && !apply(svc, vault, &mut report, std::mem::take(&mut changes)) {
            return (report, true);
        }
        if last {
            break;
        }
    }
    (report, true)
}

/// A file the catalog has not seen, as `apply_scan` wants it: hashed (unless
/// the matching of moves already did), and typed from its first bytes and
/// its name.
fn new_file(
    folder: &Folder,
    file: &DiskFile,
    hashes: &mut HashMap<String, String>,
) -> jatayu_core::Result<NewFile> {
    let hash = match hashes.remove(&file.path) {
        Some(hash) => hash,
        None => folder.hash(&file.path)?,
    };
    let head = folder.read_range(&file.path, 0, HEAD)?;
    Ok(NewFile {
        path: file.path.clone(),
        size: file.size,
        modified_us: file.modified_us,
        hash,
        mime: disk::guess_mime(disk::split(&file.path).1, &head),
    })
}

fn is_empty(changes: &ScanChanges) -> bool {
    changes.new.is_empty()
        && changes.changed.is_empty()
        && changes.moved.is_empty()
        && changes.vanished.is_empty()
        && changes.returned.is_empty()
}

/// Write one batch, announce it, read and draw what is new in it, and
/// announce that. `false` when the catalog could not be written and the
/// pass should stop.
fn apply(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    report: &mut ScanReport,
    changes: ScanChanges,
) -> bool {
    let applied = match vault.apply_scan(&changes) {
        Ok(applied) => applied,
        Err(e) => {
            note(report, e);
            return false;
        }
    };
    let r = &applied.report;
    report.added += r.added;
    report.changed += r.changed;
    report.moved += r.moved;
    report.missing += r.missing;
    report.returned += r.returned;
    for error in &r.errors {
        note(report, error);
    }
    if r.added + r.changed + r.moved + r.missing + r.returned == 0 {
        return true;
    }
    // Once the records are there, so the new files appear at once, and again
    // once their thumbnails are.
    svc.events().changed(Change::new(Kind::File, Op::Updated));
    if applied.to_probe.is_empty() {
        return true;
    }
    for id in applied.to_probe {
        if !still_open(svc, vault) {
            return false;
        }
        if let Err(e) = probe_and_store(vault, id) {
            tracing::debug!(%id, error = %e, "files: could not read a file the pass found");
        }
    }
    svc.events().changed(Change::new(Kind::File, Op::Updated));
    true
}

fn note(report: &mut ScanReport, error: impl std::fmt::Display) {
    tracing::debug!(%error, "files: a pass met a problem");
    if report.errors.len() < MAX_ERRORS {
        report.errors.push(error.to_string());
    }
}

/// What is done once an hour: empty the trash of what has been in it longer
/// than the settings keep it, and sweep what nobody finished or came back
/// for.
fn sweep(svc: &Arc<Service>, vault: &Arc<Vault>) {
    match vault.purge_files_trash(svc.now()) {
        Ok(0) => {}
        Ok(purged) => {
            tracing::info!(files = purged, "files: emptied what had been in the trash too long");
            svc.events().changed(Change::new(Kind::File, Op::Deleted));
        }
        Err(e) => tracing::debug!(error = %e, "files: could not empty the trash"),
    }
    // Decrypted copies of sealed files, and the staging of sealed uploads, are
    // not in the folder (they are under the user's cache directory, so a sync
    // tool never sees them), and they are swept whether or not the folder is
    // there: a drive that is unplugged is no reason to leave a passport lying
    // decrypted. This also clears what an older build left in the folder's
    // own `open` folder. Locking the vault deletes them all at once; this is
    // for the ones that outlive a vault that stays unlocked.
    if let Err(e) = vault.sweep_open_copies(STALE_PLAINTEXT) {
        tracing::debug!(error = %e, "files: could not sweep decrypted copies");
    }
    let Ok(Some(folder)) = vault.files_folder() else { return };
    // Nothing is swept from a folder that is not there.
    if !matches!(vault.folder_state(), Ok(state) if state.is_available()) {
        return;
    }
    if let Err(e) = folder.sweep_uploads(STALE_UPLOADS) {
        tracing::debug!(error = %e, "files: could not sweep stale uploads");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jatayu_core::VaultConfig;
    use jatayu_core::files::disk::UploadStaging;
    use jatayu_core::files::{FileFilter, FileOrigin, FileQuery, ImportOptions, StatusFilter};
    use tokio::time::Instant;

    fn open(home: &Path) -> (Arc<Service>, Arc<Vault>) {
        let config = VaultConfig { password: None, ..Default::default() };
        let vault = jatayu_vault::create(home, config).unwrap();
        let svc = Arc::new(Service::new());
        let vault = svc.set(vault);
        (svc, vault)
    }

    /// Options for a file kept sealed in the vault.
    fn sealed() -> ImportOptions {
        ImportOptions { encrypted: Some(true), ..ImportOptions::new(FileOrigin::Files) }
    }

    fn rows(vault: &Vault) -> Vec<IndexRow> {
        let mut rows = vault.file_index().unwrap();
        rows.sort_by(|a, b| a.path.cmp(&b.path));
        rows
    }

    #[test]
    fn a_pass_catalogues_what_it_finds_and_follows_a_move_by_its_contents() {
        let (home, folder) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (svc, vault) = open(home.path());
        vault.set_up_files_folder(Some(folder.path())).unwrap();
        std::fs::write(folder.path().join("a.txt"), b"hello").unwrap();

        let report = scan_now(&svc, &vault);
        assert_eq!(report.added, 1, "{report:?}");
        let before = rows(&vault);
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].path, "a.txt");
        assert!(!before[0].hash.is_empty(), "what is catalogued is hashed");

        // Nothing changed: nothing is written.
        let report = scan_now(&svc, &vault);
        assert_eq!((report.added, report.changed, report.moved, report.missing), (0, 0, 0, 0));
        assert_eq!(report.unchanged, 1);

        // Moved and renamed while nobody watched: the same file, the same id.
        std::fs::create_dir(folder.path().join("Moved")).unwrap();
        std::fs::rename(folder.path().join("a.txt"), folder.path().join("Moved/b.txt")).unwrap();
        let report = scan_now(&svc, &vault);
        assert_eq!(report.moved, 1, "{report:?}");
        assert_eq!(report.added, 0);
        let after = rows(&vault);
        assert_eq!(
            (after.len(), after[0].id, after[0].path.as_str()),
            (1, before[0].id, "Moved/b.txt")
        );

        // The record of the last pass is this vault's own, whatever other
        // tests run beside this one.
        let (when, last) = last_scan(&vault).expect("a pass was recorded");
        assert!(when <= Timestamp::now());
        assert_eq!(last, report);
        assert!(!scanning(&vault));
    }

    #[test]
    fn a_file_that_is_gone_is_missing_and_a_folder_that_is_gone_is_not_every_file_deleted() {
        let (home, folder) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (svc, vault) = open(home.path());
        vault.set_up_files_folder(Some(folder.path())).unwrap();
        std::fs::write(folder.path().join("keep.txt"), b"kept").unwrap();
        std::fs::write(folder.path().join("gone.txt"), b"gone").unwrap();
        assert_eq!(scan_now(&svc, &vault).added, 2);

        std::fs::remove_file(folder.path().join("gone.txt")).unwrap();
        let report = scan_now(&svc, &vault);
        assert_eq!(report.missing, 1, "{report:?}");
        let missing = vault
            .files(&FileQuery {
                filter: FileFilter { status: StatusFilter::Missing, ..FileFilter::default() },
                ..FileQuery::default()
            })
            .unwrap();
        assert_eq!(missing.files.len(), 1);
        assert_eq!(missing.files[0].name, "gone.txt");

        // The whole folder away -- a drive that is not mounted: unavailable,
        // and nothing more is marked missing.
        let away = folder.path().with_extension("away");
        std::fs::rename(folder.path(), &away).unwrap();
        let report = scan_now(&svc, &vault);
        assert!(report.unavailable, "{report:?}");
        assert_eq!(report.missing, 0);
        std::fs::rename(&away, folder.path()).unwrap();
        let report = scan_now(&svc, &vault);
        assert_eq!((report.unavailable, report.missing), (false, 0), "{report:?}");
    }

    #[test]
    fn most_of_the_files_vanishing_at_once_is_held_back_not_recorded() {
        let (home, folder) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (svc, vault) = open(home.path());
        vault.set_up_files_folder(Some(folder.path())).unwrap();
        for n in 0..30 {
            std::fs::write(folder.path().join(format!("{n}.txt")), format!("file {n}")).unwrap();
        }
        assert_eq!(scan_now(&svc, &vault).added, 30);
        for n in 0..30 {
            std::fs::remove_file(folder.path().join(format!("{n}.txt"))).unwrap();
        }
        let report = scan_now(&svc, &vault);
        assert!(report.held_back, "{report:?}");
        assert_eq!(report.missing, 0);
        assert!(rows(&vault).iter().all(|r| !r.missing));
    }

    #[test]
    fn a_pass_with_no_folder_set_up_does_nothing() {
        let home = tempfile::tempdir().unwrap();
        let (svc, vault) = open(home.path());
        let report = scan_now(&svc, &vault);
        assert_eq!(report, ScanReport::default());
    }

    #[test]
    fn a_burst_of_changes_is_one_pass_a_few_seconds_after_the_last_but_not_never() {
        let start = Instant::now();
        // One change: a pass `QUIET` later.
        assert_eq!(after_wake(start, start), start + QUIET);
        // A change every second for a minute pushes it back each time, until
        // the longest a pass may be held off.
        let later = start + Duration::from_secs(28);
        assert_eq!(after_wake(later, start), start + MOST_QUIET);
        let much_later = start + Duration::from_secs(55);
        assert_eq!(after_wake(much_later, start), start + MOST_QUIET);
    }

    #[tokio::test]
    async fn locking_the_service_and_closing_it_delete_the_decrypted_copies() {
        let home = tempfile::tempdir().unwrap();
        let (svc, vault) = open(home.path());
        let passport = vault.import_bytes(b"passport", "passport.pdf", &sealed()).unwrap();

        // The lock screen's and the idle timer's way: the vault drops its key
        // and the service is told. The copy handed to another program goes
        // with the key, not an hour later when a sweep next runs.
        let copy = vault.open_file_copy(passport.id).unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), b"passport");
        vault.lock();
        svc.locked().await;
        assert!(!copy.exists(), "a decrypted copy outlived the lock");

        // Closing the vault to open another, or quitting.
        vault.unlock(None).unwrap();
        let copy = vault.open_file_copy(passport.id).unwrap();
        svc.close().await;
        assert!(!copy.exists(), "a decrypted copy outlived the close");
    }

    #[test]
    fn opening_a_vault_clears_the_plaintext_a_run_that_is_gone_left() {
        let home = tempfile::tempdir().unwrap();
        let (copy, staging) = {
            // A run that quit without locking: everything dropped, the write
            // lock released, nothing deleted.
            let (_svc, vault) = open(home.path());
            let passport = vault.import_bytes(b"passport", "passport.pdf", &sealed()).unwrap();
            let ticket = vault.begin_upload("scan.bin", 3, &sealed()).unwrap();
            vault.upload_chunk(&ticket.upload, 0, b"abc").unwrap();
            (vault.open_file_copy(passport.id).unwrap(), UploadStaging::for_vault(vault.path()))
        };
        assert!(copy.exists());
        assert_eq!(std::fs::read_dir(staging.root()).unwrap().count(), 1);

        // The next run, before anything is unlocked or swept.
        let svc = Arc::new(Service::new());
        svc.set(jatayu_vault::open(home.path()).unwrap());
        assert!(!copy.exists(), "a decrypted copy survived the next start");
        assert_eq!(std::fs::read_dir(staging.root()).unwrap().count(), 0);
    }
}
