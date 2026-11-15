//! Putting a copy back: a vault directory, laid down from a repository, that
//! opens with the same password.
//!
//! Only into a folder that is empty or not there yet. A restore is for the
//! day something went wrong, and the last thing that day needs is a restore
//! that wrote half a vault over what was left of the real one. The header
//! goes down last, so until the restore has finished the folder is not a
//! vault and nothing will open it as one.
//!
//! This computer's settings come back only when asked, into the folder named,
//! and never over a file that is already there: on a new computer there is
//! nothing to overwrite, and on this one somebody restoring an old copy of
//! their journal most likely does not want last month's paired devices back.
//!
//! The files folder's files (`files/` in the copy) go back to the folder the
//! vault had when the copy was made, or to the one named, by the same rule:
//! never over a file that is already there. The folder is marked as Karma
//! Yoga's again, so a vault opened afterwards finds its files where its
//! catalog says they are. And [`restore_file`] brings back just one of them,
//! from the newest copy that holds it.

use futures::{StreamExt, TryStreamExt};
use karmayoga_core::backup::BackupKeys;
use karmayoga_core::vault::{HEADER_BACKUP_FILENAME, HEADER_FILENAME, VaultHeader};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::manifest::{Area, FILES_PREFIX, FileEntry};
use super::repo::{self, ManifestCache, chunk_object};
use super::target::Target;
use super::{BackupError, Progress};

#[derive(Debug, Clone)]
pub struct RestoreOptions {
    /// Which copy; `None` for the newest.
    pub snapshot: Option<String>,
    /// Where the vault goes: a folder that is empty or not there.
    pub into: PathBuf,
    /// Where this computer's settings go, if they should come back at all.
    pub config_into: Option<PathBuf>,
    /// Where the files folder's files go; the folder the vault had when the
    /// copy was made when `None`.
    pub files_into: Option<PathBuf>,
    /// The vault's password, when it has one.
    pub password: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreReport {
    pub snapshot: String,
    pub files: u64,
    pub bytes: u64,
    /// Settings files put back.
    pub config_restored: Vec<String>,
    /// Settings files left as they were, because one was already there.
    pub config_kept: Vec<String>,
    /// Settings files in the copy that were not asked for.
    pub config_left_out: u64,
    /// Where the files folder's files were put back, if there were any and
    /// they had somewhere to go.
    pub files_folder: Option<String>,
    pub files_folder_restored: u64,
    pub files_folder_bytes: u64,
    /// Files left as they were, because one was already there.
    pub files_folder_kept: u64,
    /// Files in the copy that had nowhere to go: it does not say which
    /// folder they came from, and none was named.
    pub files_folder_left_out: u64,
}

/// The vault header kept at `target`: whether a password is needed, and
/// what it opens.
pub async fn read_header(target: &Target) -> Result<VaultHeader, BackupError> {
    let header = target.get(repo::HEADER).await?.ok_or_else(|| {
        BackupError::Refused("there are no Karma Yoga backups here (no vault-header.json)".into())
    })?;
    serde_json::from_slice(&header)
        .map_err(|e| BackupError::Refused(format!("vault-header.json is damaged: {e}")))
}

/// The header kept at `target`, and the keys it gives with `password`. A
/// wrong password is [`karmayoga_core::Error::BadPassword`].
pub async fn open_repository(
    target: &Target,
    password: Option<&str>,
) -> Result<(VaultHeader, BackupKeys), BackupError> {
    let header = read_header(target).await?;
    let keys = BackupKeys::from_header(&header, password)?;
    match repo::marker(target).await? {
        Some(m) if m.repository == keys.repository_id() => Ok((header, keys)),
        Some(_) => Err(BackupError::Refused(
            "the header here does not belong to these backups; they cannot be opened".into(),
        )),
        None => Err(BackupError::Refused("there are no Karma Yoga backups here".into())),
    }
}

/// Restore a copy from `target`.
pub async fn restore(
    target: &Target,
    options: RestoreOptions,
    progress: Option<&Progress>,
) -> Result<RestoreReport, BackupError> {
    let (header, keys) = open_repository(target, options.password.as_deref()).await?;
    let names = repo::snapshots(target).await?;
    let snapshot =
        match &options.snapshot {
            Some(wanted) => names.iter().find(|n| *n == wanted).cloned().ok_or_else(|| {
                BackupError::Refused(format!("there is no backup called {wanted}"))
            })?,
            None => names
                .last()
                .cloned()
                .ok_or_else(|| BackupError::Refused("there are no backups here yet".into()))?,
        };
    if !is_empty_or_absent(&options.into) {
        return Err(BackupError::Refused(format!(
            "{} is not empty; restore into a new folder",
            options.into.display()
        )));
    }
    let manifest = repo::load_manifest(target, &keys, &snapshot, &ManifestCache::new(None)).await?;
    if let Some(p) = progress {
        p.totals(manifest.files.len() as u64, manifest.bytes());
    }

    // Where the files folder's files go, settled before anything is
    // downloaded: a restore that finds the folder cannot be made -- a path
    // from another computer's user -- after gigabytes of the vault are laid
    // down has wasted them, and says so only at the end.
    let files: Vec<&FileEntry> = manifest.files.iter().filter(|f| f.in_files_folder()).collect();
    let files_root = options
        .files_into
        .clone()
        .or_else(|| manifest.files_folder.as_deref().map(PathBuf::from))
        .filter(|_| !files.is_empty());
    if let Some(root) = &files_root {
        prepare_files_root(root)?;
    }

    std::fs::create_dir_all(&options.into)
        .map_err(|e| karmayoga_core::Error::io(&options.into, e))?;
    let mut report = RestoreReport { snapshot: snapshot.clone(), ..RestoreReport::default() };
    for file in manifest.files.iter().filter(|f| f.area == Area::Vault && !f.in_files_folder()) {
        if progress.is_some_and(Progress::cancelled) {
            return Err(BackupError::Cancelled);
        }
        write_file(target, &keys, file, &options.into, progress).await?;
        report.files += 1;
        report.bytes += file.size;
    }
    for file in manifest.files.iter().filter(|f| f.area == Area::Config) {
        let Some(config) = &options.config_into else {
            report.config_left_out += 1;
            continue;
        };
        let dest = within(config, &file.path)?;
        if dest.exists() {
            report.config_kept.push(file.path.clone());
            continue;
        }
        write_file(target, &keys, file, config, progress).await?;
        report.config_restored.push(file.path.clone());
    }

    match &files_root {
        Some(root) => put_back_files(target, &keys, &files, root, progress, &mut report).await?,
        None => report.files_folder_left_out = files.len() as u64,
    }

    // Last: from here on the folder is a vault.
    let header_json = serde_json::to_vec_pretty(&header).map_err(karmayoga_core::Error::from)?;
    for name in [HEADER_BACKUP_FILENAME, HEADER_FILENAME] {
        let path = options.into.join(name);
        write_durably(&path, &header_json).map_err(|e| karmayoga_core::Error::io(&path, e))?;
    }
    karmayoga_core::fsutil::sync_dir(&options.into);
    Ok(report)
}

/// Make sure the files can be put somewhere before any are: an absolute
/// path, made if it is not there.
fn prepare_files_root(root: &Path) -> Result<(), BackupError> {
    if !root.is_absolute() {
        return Err(BackupError::Refused(format!(
            "{} is not a folder this restore can put files in: it is not an absolute path",
            root.display()
        )));
    }
    std::fs::create_dir_all(root).map_err(|e| karmayoga_core::Error::io(root, e))?;
    Ok(())
}

/// Put the files folder's files back under `root`, never over one that is
/// already there, and mark the folder as Karma Yoga's.
async fn put_back_files(
    target: &Target,
    keys: &BackupKeys,
    files: &[&FileEntry],
    root: &Path,
    progress: Option<&Progress>,
    report: &mut RestoreReport,
) -> Result<(), BackupError> {
    for file in files {
        if progress.is_some_and(Progress::cancelled) {
            return Err(BackupError::Cancelled);
        }
        let relative = file.path.strip_prefix(FILES_PREFIX).unwrap_or(&file.path);
        if within(root, relative)?.symlink_metadata().is_ok() {
            report.files_folder_kept += 1;
            continue;
        }
        let entry = FileEntry { path: relative.to_string(), ..(*file).clone() };
        write_file(target, keys, &entry, root, progress).await?;
        report.files_folder_restored += 1;
        report.files_folder_bytes += file.size;
    }
    karmayoga_core::files::disk::Folder::new(root).set_up()?;
    report.files_folder = Some(root.display().to_string());
    Ok(())
}

/// A file brought back from a copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredFile {
    /// The copy it came from.
    pub snapshot: String,
    pub bytes: u64,
}

/// Bring one file back into the files folder, from the newest copy at the
/// vault's backup destination that holds it. `rel` is its path in the
/// folder. `None` when no copy does. Refuses to write over anything.
///
/// For a catalogued file that has gone missing: the file goes back at the
/// path the catalog has it under, and the next pass of the files task finds
/// it there again with everything the person added.
pub async fn restore_file(
    vault: &std::sync::Arc<karmayoga_core::Vault>,
    rel: &str,
) -> Result<Option<RestoredFile>, BackupError> {
    let (reading, wanted) = (vault.clone(), rel.to_string());
    let (destination, s3_keys, keys, dest, staging) =
        tokio::task::spawn_blocking(move || -> Result<_, BackupError> {
            let destination = reading.backup_settings()?.destination.ok_or_else(|| {
                BackupError::Refused(
                    "choose where backups go in Settings \u{2192} Backup first".into(),
                )
            })?;
            let folder = reading
                .files_folder()?
                .ok_or_else(|| BackupError::Refused("the files folder is not set up".into()))?;
            let dest = folder.resolve(&wanted)?;
            let staging = folder.root().join(karmayoga_core::files::INTERNAL_DIR).join("tmp");
            Ok((destination, reading.backup_s3_keys()?, reading.backup_keys()?, dest, staging))
        })
        .await
        .map_err(|e| BackupError::Refused(format!("a background task failed: {e}")))??;
    let target = Target::open(&destination, s3_keys)?;
    let cache = super::task::cache_dir(&keys.repository_id());
    restore_one(&target, &keys, rel, &dest, &staging, Some(cache)).await
}

/// [`restore_file`], given where the copies are, the file's path in the
/// folder, where it is to go (`dest`) and a folder to stage it in first --
/// somewhere nothing watches, on the same disk, so what appears at `dest`
/// is whole or absent.
pub async fn restore_one(
    target: &Target,
    keys: &BackupKeys,
    rel: &str,
    dest: &Path,
    staging: &Path,
    cache_dir: Option<PathBuf>,
) -> Result<Option<RestoredFile>, BackupError> {
    let wanted = format!("{FILES_PREFIX}{rel}");
    let cache = ManifestCache::new(cache_dir);
    let names = repo::snapshots(target).await?;
    for name in names.iter().rev() {
        let manifest = match repo::load_manifest(target, keys, name, &cache).await {
            Ok(manifest) => manifest,
            Err(e) => {
                tracing::warn!(snapshot = %name, error = %e, "backup: a copy does not read while looking for a file");
                continue;
            }
        };
        // Owned, not a borrow into the manifest: a reference found through a
        // closure and then held across the awaits below makes this future
        // too particular about lifetimes to be sent between threads.
        let Some(entry) =
            manifest.files.into_iter().find(|f| f.in_files_folder() && f.path == wanted)
        else {
            continue;
        };
        if dest.symlink_metadata().is_ok() {
            return Err(BackupError::Refused(format!("{rel} is already there")));
        }
        let stage = staging.join(format!("restore-{:016x}", rand::random::<u64>()));
        let placed = match write_file(target, keys, &entry, &stage, None).await {
            Ok(()) => place(&within(&stage, &entry.path)?, dest),
            Err(e) => Err(e),
        };
        let _ = std::fs::remove_dir_all(&stage);
        placed?;
        return Ok(Some(RestoredFile { snapshot: name.clone(), bytes: entry.size }));
    }
    Ok(None)
}

/// Move a restored file into its place, making the folders it is in.
fn place(staged: &Path, dest: &Path) -> Result<(), BackupError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| karmayoga_core::Error::io(parent, e))?;
    }
    std::fs::rename(staged, dest).map_err(|e| karmayoga_core::Error::io(dest, e))?;
    Ok(())
}

/// Lay one file down under `root`: its chunks fetched a few at a time, opened
/// and checked, and written in order to a temporary name that becomes the
/// real one once it is whole.
async fn write_file(
    target: &Target,
    keys: &BackupKeys,
    file: &FileEntry,
    root: &Path,
    progress: Option<&Progress>,
) -> Result<(), BackupError> {
    let dest = within(root, &file.path)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| karmayoga_core::Error::io(parent, e))?;
    }
    let partial = dest.with_extension(format!(
        "{}restoring",
        dest.extension().map(|e| format!("{}.", e.to_string_lossy())).unwrap_or_default()
    ));
    let mut out =
        std::fs::File::create(&partial).map_err(|e| karmayoga_core::Error::io(&partial, e))?;
    let mut written = 0u64;
    // Each chunk's name owned by its fetch, not borrowed from `file`: a
    // closure over `&String` returning a future is not general enough over
    // lifetimes for the future to be sent between threads, and a single
    // file's restore runs inside a command, which must be.
    let mut pieces = futures::stream::iter(file.chunks.iter().cloned().map(|name| async move {
        let name = name.as_str();
        let sealed = target.get(&chunk_object(name)).await?.ok_or_else(|| {
            BackupError::Refused(format!(
                "a piece of {} is missing from the backup (chunk {name})",
                file.path
            ))
        })?;
        keys.open_chunk(name, &sealed).map_err(|_| {
            BackupError::Refused(format!("a piece of {} is damaged (chunk {name})", file.path))
        })
    }))
    .buffered(target.parallelism());
    while let Some(piece) = pieces.try_next().await? {
        out.write_all(&piece).map_err(|e| karmayoga_core::Error::io(&partial, e))?;
        written += piece.len() as u64;
        if let Some(p) = progress {
            p.read(piece.len() as u64);
        }
    }
    if written != file.size {
        let _ = std::fs::remove_file(&partial);
        return Err(BackupError::Refused(format!(
            "{} came back {written} bytes long, not {}",
            file.path, file.size
        )));
    }
    out.sync_all().map_err(|e| karmayoga_core::Error::io(&partial, e))?;
    drop(out);
    std::fs::rename(&partial, &dest).map_err(|e| karmayoga_core::Error::io(&dest, e))?;
    if let Some(p) = progress {
        p.file_done();
    }
    Ok(())
}

/// `path` under `root`, refusing one that would reach outside it. A manifest
/// is sealed, so this guards against a bug rather than against anybody.
fn within(root: &Path, path: &str) -> Result<PathBuf, BackupError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.split('/').any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(BackupError::Refused(format!("{path:?} is not a path a backup holds")));
    }
    Ok(path.split('/').fold(root.to_path_buf(), |p, part| p.join(part)))
}

fn is_empty_or_absent(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

fn write_durably(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}
