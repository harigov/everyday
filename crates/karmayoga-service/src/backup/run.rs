//! Making one backup.
//!
//! In order: take the destination's lock; read which copies and chunks it
//! already has; copy the databases and list every file; cut each file into
//! chunks and send the ones the destination lacks; write the manifest that
//! makes it a copy; then let go of copies beyond the number kept, and of the
//! chunks only they used.
//!
//! The manifest is written only once every chunk it names is there, so a run
//! that fails or is stopped part way leaves no copy behind -- only chunks,
//! which the next run finds by listing and does not send again. Nothing is
//! deleted until the new copy is whole.
//!
//! Reading, naming and sealing happen on one blocking thread, which hands
//! sealed chunks to the sending side through a short queue; sending runs
//! [`Target::parallelism`] requests at once. The queue being short is what
//! keeps memory to a few dozen megabytes however large the vault is.

use bytes::Bytes;
use jiff::Timestamp;
use karmayoga_core::Vault;
use karmayoga_core::backup::{
    BackupKeys, BackupReport, Destination, S3Keys, StagedFile, list_files, parse_snapshot_name,
    snapshot_name,
};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tokio::task::JoinSet;

use super::manifest::{Area, FILES_PREFIX, FORMAT, FileEntry, Manifest};
use super::repo::{self, Lock, ManifestCache, chunk_object, snapshot_object};
use super::target::Target;
use super::{BackupError, BackupPhase, Progress};

/// Where a database is cut: sixteen of SQLite's 4 KiB pages.
pub const DATABASE_CHUNK: usize = 64 * 1024;
/// Where anything else is cut.
pub const FILE_CHUNK: usize = 1024 * 1024;

/// Everything one backup needs to know besides the vault.
#[derive(Debug, Clone)]
pub struct Plan {
    pub destination: Destination,
    /// How many copies to keep, this one included.
    pub keep: u32,
    /// This computer's settings folder, to copy too; `None` leaves it out.
    pub config_dir: Option<PathBuf>,
    /// Copy the files folder too, when the vault has one set up: its files
    /// go in the copy under `files/` (see [`FILES_PREFIX`]). While the
    /// folder is not usable the previous copy's are carried forward instead
    /// of the folder being recorded empty.
    pub files: bool,
    /// A backup has gone to this destination before -- see
    /// [`repo::prepare`].
    pub expect_existing: bool,
    /// The pasted S3 access key, when that is how the destination signs in.
    pub s3_keys: Option<S3Keys>,
    /// Where sealed manifests are kept between runs; `None` fetches them
    /// every time.
    pub cache_dir: Option<PathBuf>,
    pub host: String,
}

/// Make one backup of `vault` as `plan` says. `progress` is read by whoever
/// is watching, and stops the run when cancelled.
pub async fn run(
    vault: Arc<Vault>,
    plan: Plan,
    progress: Arc<Progress>,
) -> Result<BackupReport, BackupError> {
    let started = Instant::now();
    let keys = {
        let vault = vault.clone();
        Arc::new(blocking(move || Ok(vault.backup_keys()?)).await?)
    };
    let header = vault.backup_header()?;
    let target = Arc::new(Target::open(&plan.destination, plan.s3_keys.clone())?);

    progress.set_phase(BackupPhase::Listing);
    if !plan.expect_existing {
        target.create_root().await?;
    }
    repo::prepare(&target, &keys, plan.expect_existing).await?;

    // The staging folder before the lock: nothing between taking the lock
    // and letting it go may return early, or a lock nobody holds is left to
    // refuse every backup to this destination for an hour.
    let staging = repo::staging_dir(vault.path())
        .map_err(|e| BackupError::Vault(karmayoga_core::Error::io(vault.path(), e)))?;
    let result = match Lock::acquire(&target, &plan.host).await {
        Ok(mut lock) => {
            let result =
                run_locked(&vault, &plan, &keys, &target, header, &staging, &progress, &mut lock)
                    .await;
            lock.release(&target).await;
            result
        }
        Err(e) => Err(e),
    };
    let _ = std::fs::remove_dir_all(&staging);

    let mut report = result?;
    report.seconds = started.elapsed().as_secs_f64();
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
async fn run_locked(
    vault: &Arc<Vault>,
    plan: &Plan,
    keys: &Arc<BackupKeys>,
    target: &Arc<Target>,
    header: Vec<u8>,
    staging: &Path,
    progress: &Arc<Progress>,
    lock: &mut Lock,
) -> Result<BackupReport, BackupError> {
    // The header first: everything after it is sealed under the key it
    // opens, and a copy with no header beside it opens for nobody.
    target.put(repo::HEADER, Bytes::from(header)).await?;

    let cache = ManifestCache::new(plan.cache_dir.clone());
    let existing = repo::snapshots(target).await?;
    // This copy will be the newest (see `next_name`), so the copies that
    // stay are it and the newest `keep - 1` of these; the rest go.
    let keep = plan.keep.max(1) as usize;
    let stay = existing.len().min(keep - 1);
    let (gone, staying) = existing.split_at(existing.len() - stay);

    // Read only what is needed, one manifest at a time: the chunks every
    // copy that stays names, as one set, and the newest copy whole, for the
    // files that have not changed since. A manifest of a large vault is tens
    // of megabytes; holding all of them at once is not.
    let mut referenced: HashSet<String> = HashSet::new();
    let mut staying_readable = true;
    let mut previous: Option<Manifest> = None;
    for (i, name) in existing.iter().enumerate().rev() {
        let newest = i + 1 == existing.len();
        let stays = staying.contains(name);
        if !newest && !stays {
            continue;
        }
        check(progress)?;
        match repo::load_manifest(target, keys, name, &cache).await {
            Ok(m) => {
                if stays {
                    referenced.extend(m.chunks().map(str::to_string));
                }
                if previous.is_none() {
                    previous = Some(m);
                }
            }
            Err(e) => {
                tracing::warn!(snapshot = %name, error = %e, "backup: an earlier copy does not read");
                staying_readable &= !stays;
            }
        }
    }
    let present = repo::chunks(target).await?;
    let previous_files_folder = previous.as_ref().and_then(|m| m.files_folder.clone());
    let previous: HashMap<(Area, String), FileEntry> = previous
        .map(|m| m.files.into_iter().map(|f| ((f.area, f.path.clone()), f)).collect())
        .unwrap_or_default();

    progress.set_phase(BackupPhase::Preparing);
    let (files, files_folder) = {
        let (vault, staging, config, with_files) =
            (vault.clone(), staging.to_path_buf(), plan.config_dir.clone(), plan.files);
        let staged = tokio::task::spawn_blocking(move || -> Result<_, BackupError> {
            let mut files: Vec<(Area, StagedFile)> =
                vault.stage_for_backup(&staging)?.into_iter().map(|f| (Area::Vault, f)).collect();
            if let Some(config) = config.filter(|c| c.is_dir()) {
                files.extend(
                    config_files(&config, vault.path())?.into_iter().map(|f| (Area::Config, f)),
                );
            }
            let files_folder =
                if with_files { stage_files_folder(&vault, &mut files) } else { FilesFolder::Left };
            Ok((files, files_folder))
        });
        until_cancelled(staged, progress).await??
    };
    progress.totals(files.len() as u64, files.iter().map(|(_, f)| f.size).sum());
    check(progress)?;

    // A files folder that could not be read is not recorded empty: the last
    // copy's files go into this one as they were, each only if the
    // destination still has every chunk of it.
    let carried: Vec<FileEntry> = if matches!(files_folder, FilesFolder::Unavailable(_)) {
        let mut kept: Vec<FileEntry> = previous
            .values()
            .filter(|f| f.in_files_folder() && f.chunks.iter().all(|c| present.contains(c)))
            .cloned()
            .collect();
        kept.sort_by(|a, b| a.path.cmp(&b.path));
        tracing::warn!(
            carried = kept.len(),
            "backup: the files folder is not available, so the last copy's files are carried over"
        );
        kept
    } else {
        Vec::new()
    };

    progress.set_phase(BackupPhase::Copying);
    let parallelism = target.parallelism();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Sealed>(parallelism * 2);
    let abort = Arc::new(AtomicBool::new(false));
    let producer = {
        let (keys, progress, abort) = (keys.clone(), progress.clone(), abort.clone());
        tokio::task::spawn_blocking(move || {
            produce(files, previous, present, &keys, &tx, &progress, &abort)
        })
    };

    let mut sending: JoinSet<Result<u64, BackupError>> = JoinSet::new();
    let mut failure: Option<BackupError> = None;
    let mut touched = Instant::now();
    loop {
        // Room for one more, or a failure to stop on.
        while sending.len() >= parallelism {
            if let Some(done) = sending.join_next().await {
                settle(done, progress, &mut failure);
            }
        }
        while let Some(done) = sending.try_join_next() {
            settle(done, progress, &mut failure);
        }
        if failure.is_some() || progress.cancelled() {
            break;
        }
        if touched.elapsed() > repo::LOCK_TOUCH_EVERY {
            lock.touch(target).await;
            touched = Instant::now();
        }
        let Some(chunk) = rx.recv().await else { break };
        let target = target.clone();
        sending.spawn(async move {
            target.put(&chunk_object(&chunk.name), Bytes::from(chunk.sealed)).await?;
            Ok(chunk.plain)
        });
    }
    // Stop the reading side, then let what is in flight land: a chunk half
    // sent is not a problem, but a task left running past the lock is.
    abort.store(true, Ordering::Relaxed);
    drop(rx);
    while let Some(done) = sending.join_next().await {
        settle(done, progress, &mut failure);
    }
    let produced = producer
        .await
        .map_err(|e| BackupError::Refused(format!("reading the vault failed: {e}")))?;
    if let Some(failure) = failure {
        return Err(failure);
    }
    check(progress)?;
    let produced = produced?;

    // Every chunk is there: the copy can be named.
    let now = Timestamp::now();
    let snapshot = next_name(&existing, now);
    let mut entries = produced.entries;
    entries.extend(carried);
    let manifest = Manifest {
        format: FORMAT,
        created: now,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        host: plan.host.clone(),
        vault_name: vault.status().name,
        // Where the files folder was: as it says now, or -- when it cannot
        // be asked -- as the copy whose files this one carries said.
        files_folder: match &files_folder {
            FilesFolder::Unavailable(None) => previous_files_folder,
            other => other.path().map(str::to_string),
        },
        files: entries,
    };
    repo::store_manifest(target, keys, &snapshot, &manifest, &cache).await?;
    let snapshot_progress = progress.snapshot();
    let mut report = BackupReport {
        snapshot: snapshot.clone(),
        files: manifest.files.len() as u64,
        bytes: manifest.bytes(),
        uploaded_bytes: snapshot_progress.uploaded_bytes,
        uploaded_chunks: snapshot_progress.uploaded_chunks,
        skipped: produced.skipped,
        files_folder_unavailable: matches!(files_folder, FilesFolder::Unavailable(_)),
        ..BackupReport::default()
    };

    progress.set_phase(BackupPhase::Pruning);
    referenced.extend(manifest.chunks().map(str::to_string));
    drop(manifest);
    for name in gone {
        target.delete(&snapshot_object(name)).await?;
        cache.forget(name);
        report.removed_snapshots += 1;
    }
    // Chunks are let go of only when every copy that stays could be read:
    // one that could not might need any of them.
    if staying_readable {
        let garbage: Vec<String> =
            produced.known.into_iter().filter(|c| !referenced.contains(c)).collect();
        report.removed_chunks = delete_all(target, garbage, parallelism).await?;
    } else {
        tracing::warn!(
            "backup: an earlier copy could not be read, so no chunks were removed this time"
        );
    }
    Ok(report)
}

/// The name for a copy made at `now`: its date stamp -- or, when the clock
/// says a time no later than the newest copy already there, a second after
/// that one. A copy made under a clock that is behind must still sort as the
/// newest, or pruning would take it for the oldest and delete it the moment
/// it was written.
fn next_name(existing: &[String], now: Timestamp) -> String {
    let stamped = snapshot_name(now);
    match existing.iter().filter(|n| parse_snapshot_name(n).is_some()).max() {
        Some(newest) if *newest >= stamped => {
            let after = parse_snapshot_name(newest)
                .and_then(|t| t.checked_add(jiff::SignedDuration::from_secs(1)).ok())
                .unwrap_or(now);
            snapshot_name(after)
        }
        _ => stamped,
    }
}

/// Wait for `job`, unless the run is stopped first. Copying a database is
/// one step SQLite cannot be interrupted in; a stop, or the vault locking,
/// is not made to wait for it. The job is left to finish on its own -- it
/// holds no key, only paths -- and its copy goes with the staging folder.
async fn until_cancelled<T>(
    mut job: tokio::task::JoinHandle<T>,
    progress: &Progress,
) -> Result<T, BackupError> {
    let mut look = tokio::time::interval(std::time::Duration::from_millis(200));
    loop {
        tokio::select! {
            done = &mut job => {
                return done
                    .map_err(|e| BackupError::Refused(format!("a background task failed: {e}")));
            }
            _ = look.tick() => {
                if progress.cancelled() {
                    return Err(BackupError::Cancelled);
                }
            }
        }
    }
}

/// A chunk ready to send.
struct Sealed {
    name: String,
    sealed: Vec<u8>,
    plain: u64,
}

/// What the reading side came back with.
struct Produced {
    entries: Vec<FileEntry>,
    skipped: Vec<String>,
    /// Every chunk the destination has once this run's are sent: what was
    /// listed, and what this run added.
    known: HashSet<String>,
}

/// Read every file, cut it into chunks, and queue the chunks the destination
/// does not have. Runs on a blocking thread.
fn produce(
    files: Vec<(Area, StagedFile)>,
    previous: HashMap<(Area, String), FileEntry>,
    mut known: HashSet<String>,
    keys: &BackupKeys,
    tx: &tokio::sync::mpsc::Sender<Sealed>,
    progress: &Progress,
    abort: &AtomicBool,
) -> Result<Produced, BackupError> {
    let mut entries = Vec::with_capacity(files.len());
    let mut skipped = Vec::new();
    let mut buffer = vec![0u8; FILE_CHUNK];
    for (area, file) in files {
        if abort.load(Ordering::Relaxed) || progress.cancelled() {
            return Err(BackupError::Cancelled);
        }
        let chunk_size = if file.database { DATABASE_CHUNK } else { FILE_CHUNK };

        // Unchanged since the last copy: its chunks are the last copy's.
        if let Some(prev) = previous.get(&(area, file.path.clone()))
            && file.modified_ns.is_some()
            && prev.modified == file.modified_ns
            && prev.size == file.size
            && prev.chunk_size as usize == chunk_size
            && prev.chunks.iter().all(|c| known.contains(c))
        {
            entries.push(prev.clone());
            progress.read(file.size);
            progress.file_done();
            continue;
        }

        let opened = match std::fs::File::open(&file.source) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                skipped.push(file.path);
                progress.file_done();
                continue;
            }
            Err(e) => return Err(karmayoga_core::Error::io(&file.source, e).into()),
        };
        // No further than the size it had when it was listed: a mail pack
        // grows while it is read, and what was appended since the database
        // was copied is nothing the copy names.
        let mut reader = opened.take(file.size);
        let mut chunks = Vec::new();
        let mut size = 0u64;
        loop {
            let n = read_full(&mut reader, &mut buffer[..chunk_size])
                .map_err(|e| karmayoga_core::Error::io(&file.source, e))?;
            if n == 0 {
                break;
            }
            // Looked at for every chunk, not only every file: the database
            // copy alone is a hundred thousand of them, and a stop, or the
            // vault locking, should not wait for all of those to be read.
            if abort.load(Ordering::Relaxed) || progress.cancelled() {
                return Err(BackupError::Cancelled);
            }
            let piece = &buffer[..n];
            let name = keys.chunk_name(piece);
            if known.insert(name.clone()) {
                let sealed = keys.seal_chunk(&name, piece)?;
                // The sending side gone means the run is over.
                tx.blocking_send(Sealed { name: name.clone(), sealed, plain: n as u64 })
                    .map_err(|_| BackupError::Cancelled)?;
            }
            chunks.push(name);
            size += n as u64;
            progress.read(n as u64);
            if n < chunk_size {
                break;
            }
        }
        entries.push(FileEntry {
            area,
            path: file.path,
            size,
            modified: file.modified_ns,
            chunk_size: chunk_size as u32,
            chunks,
        });
        progress.file_done();
    }
    Ok(Produced { entries, skipped, known })
}

/// Fill `buf` from `reader`, or as much of it as there is before the end.
fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

fn settle(
    done: Result<Result<u64, BackupError>, tokio::task::JoinError>,
    progress: &Progress,
    failure: &mut Option<BackupError>,
) {
    match done {
        Ok(Ok(bytes)) => progress.uploaded(bytes),
        Ok(Err(e)) => {
            failure.get_or_insert(e);
        }
        Err(e) => {
            failure.get_or_insert(BackupError::Refused(format!("sending a chunk failed: {e}")));
        }
    }
}

fn check(progress: &Progress) -> Result<(), BackupError> {
    if progress.cancelled() { Err(BackupError::Cancelled) } else { Ok(()) }
}

/// Delete every chunk in `names`, `parallelism` at a time. Answers how many.
async fn delete_all(
    target: &Arc<Target>,
    names: Vec<String>,
    parallelism: usize,
) -> Result<u64, BackupError> {
    let mut deleting: JoinSet<Result<(), BackupError>> = JoinSet::new();
    let mut count = 0;
    for name in names {
        while deleting.len() >= parallelism {
            if let Some(done) = deleting.join_next().await {
                done.map_err(|e| BackupError::Refused(e.to_string()))??;
                count += 1;
            }
        }
        let target = target.clone();
        deleting.spawn(async move { Ok(target.delete(&chunk_object(&name)).await?) });
    }
    while let Some(done) = deleting.join_next().await {
        done.map_err(|e| BackupError::Refused(e.to_string()))??;
        count += 1;
    }
    Ok(count)
}

/// What became of the files folder when a copy was staged.
enum FilesFolder {
    /// Not asked for, or never set up, or a vault that keeps no files.
    Left,
    /// Listed, and its files added to the copy's; the folder's own path.
    Listed(String),
    /// Set up, and not usable now: not there, not readable, or there
    /// without its marker while the catalog expects files in it -- an
    /// encrypted volume that is locked, a drive that is not mounted. Its
    /// path, when it is known.
    ///
    /// Recording it as empty is the one wrong answer. A backup that ran
    /// every night through a week of a locked volume would then keep seven
    /// copies of nothing, and "keep 7" would have pushed every copy that held
    /// the files out. So the previous copy's files are carried forward.
    Unavailable(Option<String>),
}

impl FilesFolder {
    fn path(&self) -> Option<&str> {
        match self {
            Self::Listed(path) => Some(path.as_str()),
            Self::Unavailable(path) => path.as_deref(),
            Self::Left => None,
        }
    }
}

/// Add the files folder's files to `into`, named `files/…`, if the folder
/// is usable -- before the listing, and again after it, so a volume that
/// locked halfway through is not taken for a folder that has lost most of
/// its files.
fn stage_files_folder(vault: &Vault, into: &mut Vec<(Area, StagedFile)>) -> FilesFolder {
    if !vault.supports_files() {
        return FilesFolder::Left;
    }
    let folder = match vault.files_folder() {
        Ok(Some(folder)) => folder,
        Ok(None) => return FilesFolder::Left,
        Err(e) => {
            tracing::warn!(error = %e, "backup: the files folder could not be looked up");
            return FilesFolder::Unavailable(None);
        }
    };
    let root = folder.root().display().to_string();
    let usable = || matches!(vault.folder_state(), Ok(state) if state.is_available());
    if !usable() {
        return FilesFolder::Unavailable(Some(root));
    }
    let listed =
        match list_files(folder.root(), FILES_PREFIX.trim_end_matches('/'), &left_out_of_files) {
            Ok(listed) => listed,
            Err(e) => {
                tracing::warn!(error = %e, "backup: the files folder could not be read");
                return FilesFolder::Unavailable(Some(root));
            }
        };
    if !usable() {
        return FilesFolder::Unavailable(Some(root));
    }
    into.extend(listed.into_iter().map(|f| (Area::Vault, f)));
    FilesFolder::Listed(root)
}

/// Left out of a copy of the files folder, by path as the copy names it
/// (`files/…`): the folder Karma Yoga keeps for itself, and every name a
/// scan of the folder leaves out too -- a sync tool's scratch files, an
/// operating system's litter -- so what a backup holds is what the catalog
/// does.
fn left_out_of_files(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name == karmayoga_core::files::INTERNAL_DIR || karmayoga_core::files::disk::is_skipped(name)
}

/// What is copied from this computer's settings folder: everything but what
/// is made again, is a pointer to somewhere on this machine, is a store
/// something else has open -- or is not settings at all.
///
/// On macOS the settings folder and the data folder are one folder, and that
/// folder is where the vault lives by default, with the downloaded models
/// beside it. So the vault and the machine's data are left out by name, and
/// anything under the vault's own folder by path: a backup copies the vault
/// once, through the store, and copying it again as "settings" would read a
/// live database's log as bytes and carry gigabytes of models along.
fn config_files(dir: &Path, vault_root: &Path) -> karmayoga_core::Result<Vec<StagedFile>> {
    let inside_vault = |source: &Path| {
        vault_root != dir && vault_root.starts_with(dir) && source.starts_with(vault_root)
    };
    Ok(list_files(dir, "", &leave_out_of_config)?
        .into_iter()
        .filter(|f| !inside_vault(&f.source))
        .filter(|f| {
            !(karmayoga_core::backup::might_be_database(&f.path)
                && karmayoga_core::backup::is_sqlite_file(&f.source))
        })
        .collect())
}

/// Left out of a copy of the settings folder, by path relative to it.
fn leave_out_of_config(path: &str) -> bool {
    const FILES: &[&str] = &[
        "last-vault",
        "webview-carried-over",
        // A vault's own, when it shares the folder (macOS's default).
        "vault.json",
        "vault.json.bak",
        "vault.lock",
    ];
    const FOLDERS: &[&str] = &[
        // Made again from the mail.
        "mail-index",
        // A Postgres vault's chat store: a database something else has open.
        "chat",
        // The backup's own manifests are the destination's.
        "backup-cache",
        // When the vault shares the folder: its store, its recordings in
        // progress, a backup's staging copies, and the downloaded models.
        "store",
        "spool",
        ".backup-staging",
        "models",
    ];
    let name = path.rsplit('/').next().unwrap_or(path);
    FILES.contains(&path)
        || FOLDERS.iter().any(|f| path == *f || path.starts_with(&format!("{f}/")))
        || name.ends_with(".lock")
        || name.ends_with(".tmp")
        || name.ends_with(".partial")
        || name.ends_with("-wal")
        || name.ends_with("-shm")
        || name.ends_with("-journal")
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, BackupError> + Send + 'static,
) -> Result<T, BackupError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| BackupError::Refused(format!("a background task failed: {e}")))?
}

#[cfg(test)]
pub(super) fn leaves_out_of_config(path: &str) -> bool {
    leave_out_of_config(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_copy_made_under_a_clock_that_is_behind_still_sorts_newest() {
        let now: Timestamp = "2026-11-14T03:00:00Z".parse().unwrap();
        assert_eq!(next_name(&[], now), "2026-11-14T030000Z");
        let older = vec!["2026-11-13T030000Z".to_string()];
        assert_eq!(next_name(&older, now), "2026-11-14T030000Z");
        // The newest copy is from a clock that ran ahead; this one follows it.
        let ahead = vec!["2026-11-13T030000Z".to_string(), "2027-01-01T000000Z".to_string()];
        assert_eq!(next_name(&ahead, now), "2027-01-01T000001Z");
        // Two in one second.
        let same = vec!["2026-11-14T030000Z".to_string()];
        assert_eq!(next_name(&same, now), "2026-11-14T030001Z");
    }

    #[test]
    fn staging_does_not_hold_up_a_stop() {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let progress = Progress::new(true);
            let job = tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_secs(5)));
            let stopper = progress.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                stopper.cancel();
            });
            let started = Instant::now();
            let result = until_cancelled(job, &progress).await;
            assert!(matches!(result, Err(BackupError::Cancelled)));
            assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        });
    }

    #[test]
    fn a_settings_folder_shared_with_the_vault_copies_only_the_settings() {
        // macOS: one folder for settings, data, the default vault and models.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (path, body) in [
            ("devices.json", "{}"),
            ("mcp.json", "{}"),
            ("vault.json", "{}"),
            ("vault.json.bak", "{}"),
            ("store/media/ab/ab.blob", "sealed"),
            ("store/karmayoga.db-wal", "log"),
            ("spool/recordings/r/mic-0.chunk", "audio"),
            ("models/speechKit/vad.onnx", "weights"),
            ("backup-cache/repo/x.snapshot", "sealed"),
            ("mail-index/abc/seg", "index"),
        ] {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, body).unwrap();
        }
        let mut copied: Vec<String> =
            config_files(root, root).unwrap().into_iter().map(|f| f.path).collect();
        copied.sort();
        assert_eq!(copied, ["devices.json", "mcp.json"]);

        // A vault inside the settings folder is left out by path too.
        let nested = root.join("Vaults/Mine");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("journal.json"), "{}").unwrap();
        let copied: Vec<String> =
            config_files(root, &nested).unwrap().into_iter().map(|f| f.path).collect();
        assert!(!copied.iter().any(|p| p.starts_with("Vaults/")), "{copied:?}");
    }
}
