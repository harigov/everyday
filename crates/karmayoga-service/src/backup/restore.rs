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
//! vault had when the copy was made -- but only when that is where this
//! computer keeps the folder by default -- or to the one named, by the same
//! rule: never over a file that is already there. A manifest is sealed, but
//! a copy made by an old build, or a repository somebody else could write
//! to, can still name any path, and a restore that laid files wherever it
//! was told would put them in `~/.config/autostart`. The folder is marked as
//! Karma Yoga's again, so a vault opened afterwards finds its files where its
//! catalog says they are. And [`restore_file`] brings back just one of them,
//! from the newest copy that holds it.
//!
//! What a restore writes is private. The vault's own files, and the settings
//! it was asked to put back -- a bearer token, a server's private key, the
//! list of paired devices -- are made readable by their owner alone from the
//! moment they exist, the folders they go in likewise when the restore makes
//! them. The files folder's files are the person's documents and are written
//! as any program writes them.
//!
//! Which copy is the newest is a matter of which ones open, not of what they
//! are called: the destination is a place somebody else may be able to write
//! to, and a file named for the year 2999 that does not open under the
//! vault's key is not a backup of anything.

use futures::{StreamExt, TryStreamExt};
use karmayoga_core::backup::{BackupKeys, UNENCRYPTED_BACKUP_REFUSAL};
use karmayoga_core::vault::{HEADER_BACKUP_FILENAME, HEADER_FILENAME, VaultHeader};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::manifest::{Area, FILES_PREFIX, FileEntry, Manifest};
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
    /// Where the files folder's files go. When `None`, the folder the vault
    /// had when the copy was made -- if that is this computer's default
    /// files folder, and otherwise nowhere: they are left out, and the
    /// report says so.
    pub files_into: Option<PathBuf>,
    /// The vault's password, when it has one.
    pub password: Option<String>,
    /// Go ahead although the backup's header says the vault is not
    /// encrypted. `false` unless the person said so (`--unencrypted`): that
    /// header is a plain file at the destination, "not encrypted" in it
    /// skips the password and puts the key at a value everybody knows, and
    /// so anybody who could write to the destination could have written it.
    /// See [`karmayoga_core::backup::BackupKeys::from_header`].
    pub allow_unencrypted: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreReport {
    pub snapshot: String,
    /// Newer backup files than `snapshot` that were passed over because they
    /// do not open with this vault's key -- damaged, from a newer Karma Yoga,
    /// or put there by somebody else. Empty when a copy was named.
    pub passed_over: Vec<String>,
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
    /// Files in the copy that had nowhere to go: none was named, and the
    /// folder the copy says they came from is not this computer's default
    /// files folder (or it does not say).
    pub files_folder_left_out: u64,
    /// The folder the copy says its files came from, when they were left
    /// out for that reason; what to name in `--files-to` is up to the
    /// person.
    pub files_folder_recorded: Option<String>,
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
///
/// A header that says the vault is not encrypted is refused unless
/// `allow_unencrypted`: see [`RestoreOptions::allow_unencrypted`].
pub async fn open_repository(
    target: &Target,
    password: Option<&str>,
    allow_unencrypted: bool,
) -> Result<(VaultHeader, BackupKeys), BackupError> {
    let header = read_header(target).await?;
    if !header.is_encrypted() && !allow_unencrypted {
        return Err(BackupError::Refused(UNENCRYPTED_BACKUP_REFUSAL.into()));
    }
    let keys = BackupKeys::from_header(&header, password, allow_unencrypted)?;
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
    let (header, keys) =
        open_repository(target, options.password.as_deref(), options.allow_unencrypted).await?;
    let names = repo::snapshots(target).await?;
    let (snapshot, manifest, passed_over) =
        choose_snapshot(target, &keys, options.snapshot.as_deref(), &names).await?;
    if !is_empty_or_absent(&options.into) {
        return Err(BackupError::Refused(format!(
            "{} is not empty; restore into a new folder",
            options.into.display()
        )));
    }
    if let Some(p) = progress {
        p.totals(manifest.files.len() as u64, manifest.bytes());
    }

    // Where the files folder's files go, settled before anything is
    // downloaded: a restore that finds the folder cannot be made -- a path
    // from another computer's user -- after gigabytes of the vault are laid
    // down has wasted them, and says so only at the end.
    let files: Vec<&FileEntry> = manifest.files.iter().filter(|f| f.in_files_folder()).collect();
    let default_folder = karmayoga_core::files::disk::default_folder();
    let files_root = match files_destination(
        !files.is_empty(),
        options.files_into.as_deref(),
        manifest.files_folder.as_deref(),
        default_folder.as_deref(),
    ) {
        FilesDestination::Nowhere | FilesDestination::NeedsFolder => None,
        FilesDestination::To(root) => {
            prepare_files_root(&root)?;
            Some(root)
        }
    };

    karmayoga_core::fsutil::create_private_dir(&options.into)?;
    let mut report =
        RestoreReport { snapshot: snapshot.clone(), passed_over, ..RestoreReport::default() };
    for file in manifest.files.iter().filter(|f| f.area == Area::Vault && !f.in_files_folder()) {
        if progress.is_some_and(Progress::cancelled) {
            return Err(BackupError::Cancelled);
        }
        write_file(target, &keys, file, &options.into, true, progress).await?;
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
        write_file(target, &keys, file, config, true, progress).await?;
        report.config_restored.push(file.path.clone());
    }

    match &files_root {
        Some(root) => put_back_files(target, &keys, &files, root, progress, &mut report).await?,
        None => {
            report.files_folder_left_out = files.len() as u64;
            if !files.is_empty() {
                report.files_folder_recorded = manifest.files_folder.clone();
            }
        }
    }

    // Last: from here on the folder is a vault.
    let header_json = serde_json::to_vec_pretty(&header).map_err(karmayoga_core::Error::from)?;
    for name in [HEADER_BACKUP_FILENAME, HEADER_FILENAME] {
        // Atomic, durable and private from its first byte: it is the one
        // file the password is guessed against.
        let path = options.into.join(name);
        karmayoga_core::fsutil::write_atomic_private(
            &path,
            &header_json,
            &karmayoga_core::fsutil::unique_tag(),
        )?;
    }
    karmayoga_core::fsutil::sync_dir(&options.into);
    Ok(report)
}

/// The copy to restore, and what it holds.
///
/// One named is that one or an error. Otherwise the newest whose manifest
/// opens under `keys`: the names in `snapshots/` are whatever somebody who
/// can write to the destination put there, and the newest by name may be a
/// file that is not a backup at all. A copy that does not open is passed
/// over, and the names of those that were are handed back for the report to
/// say so; but a destination that cannot be reached is not passed over: that
/// is an error, not a reason to hand back last month's copy without a word.
async fn choose_snapshot(
    target: &Target,
    keys: &BackupKeys,
    wanted: Option<&str>,
    names: &[String],
) -> Result<(String, Manifest, Vec<String>), BackupError> {
    let cache = ManifestCache::new(None);
    if let Some(wanted) = wanted {
        let name = names
            .iter()
            .find(|n| *n == wanted)
            .ok_or_else(|| BackupError::Refused(format!("there is no backup called {wanted}")))?;
        let manifest = repo::load_manifest(target, keys, name, &cache).await?;
        return Ok((name.clone(), manifest, Vec::new()));
    }
    if names.is_empty() {
        return Err(BackupError::Refused("there are no backups here yet".into()));
    }
    let mut passed_over = Vec::new();
    for name in names.iter().rev() {
        match repo::load_manifest(target, keys, name, &cache).await {
            Ok(manifest) => return Ok((name.clone(), manifest, passed_over)),
            Err(BackupError::Refused(why)) => {
                tracing::warn!(snapshot = %name, %why, "restore: passing over a copy that does not open");
                passed_over.push(name.clone());
            }
            Err(other) => return Err(other),
        }
    }
    Err(BackupError::Refused(format!(
        "none of the {} backup file(s) here opens with this vault's key, so there is nothing to \
         restore. They may be damaged, or may not be backups of this vault at all.",
        names.len()
    )))
}

/// Where the files folder's files go.
#[derive(Debug, PartialEq, Eq)]
enum FilesDestination {
    /// The copy holds none.
    Nowhere,
    To(PathBuf),
    /// It holds some, and nothing says where they may safely go.
    NeedsFolder,
}

/// Settle where the files folder's files go: the folder named, else the one
/// the copy records -- but only when that is `default`, this computer's own
/// default files folder.
///
/// A path read out of a manifest is somebody's say-so about where to write
/// on this computer. It is sealed, which keeps strangers from editing it,
/// and says nothing about a copy made on another machine, by an old build,
/// or in a repository somebody could write to; and what a restore writes
/// there can be anything the vault ever held. Only the folder this program
/// itself would offer is trusted. Anywhere else is the person's to name,
/// with `--files-to`, where they can see it.
fn files_destination(
    any_files: bool,
    named: Option<&Path>,
    recorded: Option<&str>,
    default: Option<&Path>,
) -> FilesDestination {
    if !any_files {
        return FilesDestination::Nowhere;
    }
    if let Some(named) = named {
        return FilesDestination::To(named.to_path_buf());
    }
    match (recorded, default) {
        (Some(recorded), Some(default)) if Path::new(recorded) == default => {
            FilesDestination::To(default.to_path_buf())
        }
        _ => FilesDestination::NeedsFolder,
    }
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
        write_file(target, keys, &entry, root, false, progress).await?;
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
        let placed = match write_file(target, keys, &entry, &stage, false, None).await {
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
///
/// With `private`, on unix, the file is created readable by its owner alone
/// -- the temporary name is, and so the finished file is, from its first
/// byte -- and every folder this makes on the way down is `0700`. For what a
/// vault and a computer's settings hold: a bearer token, a private key, the
/// store. A mode set afterwards leaves the secret open for as long as the
/// write takes. Otherwise the file is made as any program makes one.
async fn write_file(
    target: &Target,
    keys: &BackupKeys,
    file: &FileEntry,
    root: &Path,
    private: bool,
    progress: Option<&Progress>,
) -> Result<(), BackupError> {
    let dest = within(root, &file.path)?;
    if let Some(parent) = dest.parent() {
        make_dirs(root, parent, private)?;
    }
    let partial = dest.with_extension(format!(
        "{}restoring",
        dest.extension().map(|e| format!("{}.", e.to_string_lossy())).unwrap_or_default()
    ));
    let mut out =
        create_partial(&partial, private).map_err(|e| karmayoga_core::Error::io(&partial, e))?;
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

/// Make `parent`, which is `root` or a folder somewhere under it. When
/// `private`, each folder this call makes -- `root` included -- is `0700` on
/// unix, and one that was already there keeps the mode it had.
fn make_dirs(root: &Path, parent: &Path, private: bool) -> Result<(), BackupError> {
    if !private {
        std::fs::create_dir_all(parent).map_err(|e| karmayoga_core::Error::io(parent, e))?;
        return Ok(());
    }
    karmayoga_core::fsutil::create_private_dir(root)?;
    let mut here = root.to_path_buf();
    if let Ok(below) = parent.strip_prefix(root) {
        for part in below.components() {
            here.push(part);
            karmayoga_core::fsutil::create_private_dir(&here)?;
        }
    }
    Ok(())
}

/// A new file at `path`, for [`write_file`] to fill. Whatever is at that
/// name -- the leftover of a restore that was stopped, or something put there
/// -- is removed first and the file made with `create_new`, so it is this
/// call's own and its mode is the one asked for, not that of a file that was
/// already there, and a link that was left at the name is not followed.
fn create_partial(path: &Path, private: bool) -> std::io::Result<std::fs::File> {
    let _ = std::fs::remove_file(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if private {
            options.mode(0o600);
        }
    }
    #[cfg(not(unix))]
    let _ = private;
    options.open(path)
}

fn is_empty_or_absent(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_a_copy_names_is_used_only_when_it_is_this_computers_default() {
        let home = Path::new("/home/me/Karma Yoga");
        let to = |p: &str| FilesDestination::To(PathBuf::from(p));

        // Nothing to put anywhere.
        assert_eq!(
            files_destination(false, None, Some("/etc"), Some(home)),
            FilesDestination::Nowhere
        );
        // The default, as the copy says it.
        assert_eq!(
            files_destination(true, None, Some("/home/me/Karma Yoga"), Some(home)),
            to("/home/me/Karma Yoga")
        );
        assert_eq!(
            files_destination(true, None, Some("/home/me/Karma Yoga/"), Some(home)),
            to("/home/me/Karma Yoga")
        );
        // Anywhere else a copy may say -- another user's, or a place a
        // restore must never write -- needs somebody to name it.
        for recorded in [
            "/home/me/.config/autostart",
            "/home/you/Karma Yoga",
            "/home/me/Karma Yoga/../.config/autostart",
            "/home/me/Karma Yoga/Photos",
            "Karma Yoga",
            "",
        ] {
            assert_eq!(
                files_destination(true, None, Some(recorded), Some(home)),
                FilesDestination::NeedsFolder,
                "{recorded:?}"
            );
        }
        assert_eq!(files_destination(true, None, None, Some(home)), FilesDestination::NeedsFolder);
        assert_eq!(
            files_destination(true, None, Some("/home/me/Karma Yoga"), None),
            FilesDestination::NeedsFolder,
            "a computer with no home folder has no default to match"
        );
        // Named, it is the person's choice whatever the copy says.
        assert_eq!(
            files_destination(true, Some(Path::new("/mnt/new")), Some("/etc"), Some(home)),
            to("/mnt/new")
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_files_and_the_folders_made_for_them_are_owner_only_from_the_start() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");

        make_dirs(&root, &root.join("a/b"), true).unwrap();
        for made in [&root, &root.join("a"), &root.join("a/b")] {
            assert_eq!(mode(made), 0o700, "{}", made.display());
        }
        // A folder that was already there keeps its mode.
        let shared = dir.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
        make_dirs(&shared, &shared.join("c"), true).unwrap();
        assert_eq!((mode(&shared), mode(&shared.join("c"))), (0o755, 0o700));

        // And a leftover at the name -- readable by everybody, or a link
        // to somewhere else -- is not what the file is written through.
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, b"theirs").unwrap();
        let partial = root.join("x.restoring");
        std::os::unix::fs::symlink(&target, &partial).unwrap();
        let mut out = create_partial(&partial, true).unwrap();
        out.write_all(b"secret").unwrap();
        assert_eq!(mode(&partial), 0o600);
        assert_eq!(std::fs::read(&target).unwrap(), b"theirs");
    }
}
