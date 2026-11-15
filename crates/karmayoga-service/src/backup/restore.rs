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

use futures::{StreamExt, TryStreamExt};
use karmayoga_core::backup::BackupKeys;
use karmayoga_core::vault::{HEADER_BACKUP_FILENAME, HEADER_FILENAME, VaultHeader};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::manifest::{Area, FileEntry};
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

    std::fs::create_dir_all(&options.into)
        .map_err(|e| karmayoga_core::Error::io(&options.into, e))?;
    let mut report = RestoreReport { snapshot: snapshot.clone(), ..RestoreReport::default() };
    for file in manifest.files.iter().filter(|f| f.area == Area::Vault) {
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

    // Last: from here on the folder is a vault.
    let header_json = serde_json::to_vec_pretty(&header).map_err(karmayoga_core::Error::from)?;
    for name in [HEADER_BACKUP_FILENAME, HEADER_FILENAME] {
        let path = options.into.join(name);
        write_durably(&path, &header_json).map_err(|e| karmayoga_core::Error::io(&path, e))?;
    }
    karmayoga_core::fsutil::sync_dir(&options.into);
    Ok(report)
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
    let mut pieces = futures::stream::iter(file.chunks.iter().map(|name| async move {
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
