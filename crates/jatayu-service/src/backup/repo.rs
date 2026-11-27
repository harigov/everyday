//! The repository at a destination: the names of things in it, the marker
//! that says whose it is, the lock a backup holds while it writes, and
//! reading back what is there.

use bytes::Bytes;
use jatayu_core::backup::{BackupKeys, CHUNK_NAME_LEN, parse_snapshot_name};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::BackupError;
use super::manifest::Manifest;
use super::target::{Target, TargetResult};

/// What says a place holds a repository, and whose.
///
/// A repository a build under a former name made has that name's marker
/// instead ([`jatayu_core::legacy::Former::backup_marker`]) and is the same
/// repository: every other name in it is the same, and so are the keys. It is
/// read where it is and written to as it is, with its marker left alone --
/// see [`marker`].
pub const MARKER: &str = "jatayu-backup.json";
/// The format a marker this build writes names.
const FORMAT: &str = "jatayu-backup";
/// The vault's header, latest only. Not `vault.json`: a folder holding that
/// is taken for a vault, and a repository is not one.
pub const HEADER: &str = "vault-header.json";
pub const README: &str = "README.txt";
pub const LOCK: &str = "lock.json";
const SNAPSHOTS: &str = "snapshots/";
const SNAPSHOT_SUFFIX: &str = ".snapshot";
const CHUNKS: &str = "chunks/";

/// The repository format this build writes, and the newest it reads.
const REPOSITORY_VERSION: u32 = 1;

pub fn chunk_object(name: &str) -> String {
    format!("{CHUNKS}{}/{name}", &name[..2])
}

pub fn snapshot_object(snapshot: &str) -> String {
    format!("{SNAPSHOTS}{snapshot}{SNAPSHOT_SUFFIX}")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Marker {
    pub format: String,
    pub version: u32,
    /// [`BackupKeys::repository_id`] of the vault whose backups these are.
    pub repository: String,
    pub created: Timestamp,
}

/// Every name a marker may have, the current one first and then each former
/// name's, newest first.
fn marker_names() -> impl Iterator<Item = &'static str> {
    std::iter::once(MARKER).chain(
        jatayu_core::legacy::FORMERS
            .iter()
            .map(|former| former.backup_marker)
            .filter(|name| !name.is_empty()),
    )
}

/// Whether `format` is one this application wrote a marker in, under any of
/// its names.
fn known_format(format: &str) -> bool {
    format == FORMAT
        || jatayu_core::legacy::FORMERS
            .iter()
            .any(|former| !former.backup_format.is_empty() && former.backup_format == format)
}

/// The marker, if there is one, under the first of its names
/// ([`marker_names`]) the destination holds.
///
/// A repository a build under a former name made is found by its own marker
/// and is not given a second one. Writing the current marker beside it would
/// gain nothing -- nothing else in a repository carries a name -- and moving
/// it would be a write and a delete at a destination that might be a bucket
/// shared with a copy of the vault on another computer still running the
/// former build, which would then take it for a place whose backups had
/// gone. Left as it is, the repository is the same one to every build.
pub async fn marker(target: &Target) -> Result<Option<Marker>, BackupError> {
    for name in marker_names() {
        if let Some(bytes) = target.get(name).await? {
            return serde_json::from_slice(&bytes).map(Some).map_err(|_| {
                BackupError::Refused(format!(
                    "{name} at this destination is not one Jatayu wrote; choose somewhere else"
                ))
            });
        }
    }
    Ok(None)
}

/// Make sure `target` holds this vault's repository, making one if it holds
/// nothing yet.
///
/// `expect_existing` is set once a backup has gone there before: a marker
/// missing then is not a new place but a place that is not there -- a disk
/// not plugged in, whose empty mount point would otherwise be filled with a
/// fresh backup on the computer's own disk.
pub async fn prepare(
    target: &Target,
    keys: &BackupKeys,
    expect_existing: bool,
) -> Result<(), BackupError> {
    match marker(target).await? {
        Some(m) => {
            if !known_format(&m.format) || m.version > REPOSITORY_VERSION {
                return Err(BackupError::Refused(
                    "the backups here were made by a newer Jatayu; update it, or choose \
                     somewhere else"
                        .into(),
                ));
            }
            if m.repository != keys.repository_id() {
                return Err(BackupError::Refused(
                    "this destination holds another vault's backups; choose somewhere else".into(),
                ));
            }
            Ok(())
        }
        None if expect_existing => Err(BackupError::Target(super::target::TargetError::new(
            super::target::TargetErrorKind::Missing,
            "the backups that were made here are not there any more -- if it is a disk, is it \
             plugged in? If they were removed on purpose, press Back up now to start again there.",
        ))),
        None => {
            let marker = Marker {
                format: FORMAT.into(),
                version: REPOSITORY_VERSION,
                repository: keys.repository_id(),
                created: Timestamp::now(),
            };
            let json = serde_json::to_vec_pretty(&marker).expect("a marker serialises");
            target.put(README, Bytes::from_static(README_TEXT.as_bytes())).await?;
            target.put(MARKER, Bytes::from(json)).await?;
            Ok(())
        }
    }
}

const README_TEXT: &str = "\
These are backups of a Jatayu vault.

Every file is sealed (XChaCha20-Poly1305) under a key derived from the
vault's own, so they open with the vault's password and nothing else.
`vault-header.json` is what turns that password into the key; it is safe to
keep beside the backups, exactly as `vault.json` is kept beside a vault.

snapshots/   one sealed list per backup, named by the date it was made (UTC)
chunks/      the pieces every file is cut into, shared between backups

To restore the newest backup into a folder:

    jatayu restore <this folder, or s3://bucket/prefix, or gs://bucket/prefix> <new folder>

and add `--snapshot <name>` for an older one, `--config` to put this
computer's settings back too. The folder that comes out is a vault: open it
in Jatayu with the same password.

Do not edit or move what is in here by hand. Deleting a snapshot file is
safe; the chunks only it used are removed by the next backup.
";

/// The copies at `target`, oldest first. Only names this build made; anything
/// else in `snapshots/` is left alone.
pub async fn snapshots(target: &Target) -> TargetResult<Vec<String>> {
    let mut names: Vec<String> = target
        .list(SNAPSHOTS)
        .await?
        .into_iter()
        .filter_map(|object| {
            let name = object.strip_prefix(SNAPSHOTS)?.strip_suffix(SNAPSHOT_SUFFIX)?;
            parse_snapshot_name(name).map(|_| name.to_string())
        })
        .collect();
    names.sort();
    Ok(names)
}

/// Every chunk at `target`, by name.
pub async fn chunks(target: &Target) -> TargetResult<HashSet<String>> {
    Ok(target
        .list(CHUNKS)
        .await?
        .into_iter()
        .filter_map(|object| {
            let name = object.rsplit('/').next()?;
            (name.len() == CHUNK_NAME_LEN && name.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| name.to_string())
        })
        .collect())
}

/// Sealed manifests kept on this computer between backups, so each run
/// fetches only the ones it has not seen. Sealed as they are at the
/// destination; a damaged or stale one is fetched again.
pub struct ManifestCache {
    dir: Option<PathBuf>,
}

impl ManifestCache {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self { dir }
    }

    fn path(&self, snapshot: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(format!("{snapshot}{SNAPSHOT_SUFFIX}")))
    }

    fn read(&self, snapshot: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path(snapshot)?).ok()
    }

    fn write(&self, snapshot: &str, sealed: &[u8]) {
        let (Some(dir), Some(path)) = (&self.dir, self.path(snapshot)) else { return };
        if jatayu_core::fsutil::create_private_dir(dir).is_ok() {
            let _ = std::fs::write(path, sealed);
        }
    }

    pub fn forget(&self, snapshot: &str) {
        if let Some(path) = self.path(snapshot) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The manifest of `snapshot`, from the cache if it is there and opens.
pub async fn load_manifest(
    target: &Target,
    keys: &BackupKeys,
    snapshot: &str,
    cache: &ManifestCache,
) -> Result<Manifest, BackupError> {
    if let Some(sealed) = cache.read(snapshot)
        && let Ok(m) = Manifest::decode(&sealed, keys, snapshot)
    {
        return Ok(m);
    }
    let sealed = target
        .get(&snapshot_object(snapshot))
        .await?
        .ok_or_else(|| BackupError::Refused(format!("there is no backup called {snapshot}")))?;
    let manifest = Manifest::decode(&sealed, keys, snapshot).map_err(BackupError::Refused)?;
    cache.write(snapshot, &sealed);
    Ok(manifest)
}

/// Store `manifest` as `snapshot`, and keep a copy in the cache.
pub async fn store_manifest(
    target: &Target,
    keys: &BackupKeys,
    snapshot: &str,
    manifest: &Manifest,
    cache: &ManifestCache,
) -> Result<(), BackupError> {
    let sealed = manifest.encode(keys, snapshot).map_err(BackupError::Refused)?;
    target.put(&snapshot_object(snapshot), Bytes::from(sealed.clone())).await?;
    cache.write(snapshot, &sealed);
    Ok(())
}

/// Who is writing to a repository.
///
/// A second backup writing at once -- the same vault restored onto another
/// computer and left backing up to the same place -- would delete chunks the
/// first is about to name. So a backup takes this first, and a lock that is
/// still fresh, from anybody else, refuses the run. Fresh means touched in the
/// last hour: a backup refreshes it as it goes, so one left by a crash is
/// stale soon enough.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lock {
    pub host: String,
    pub pid: u32,
    pub started: Timestamp,
    pub touched: Timestamp,
}

const LOCK_FRESH_FOR: jiff::SignedDuration = jiff::SignedDuration::from_hours(1);
/// How often a running backup touches its lock.
pub const LOCK_TOUCH_EVERY: std::time::Duration = std::time::Duration::from_secs(10 * 60);

impl Lock {
    pub async fn acquire(target: &Target, host: &str) -> Result<Self, BackupError> {
        if let Some(bytes) = target.get(LOCK).await?
            && let Ok(held) = serde_json::from_slice::<Lock>(&bytes)
        {
            let ours = held.host == host && held.pid == std::process::id();
            let fresh =
                held.touched.checked_add(LOCK_FRESH_FOR).is_ok_and(|t| t > Timestamp::now());
            if fresh && !ours {
                return Err(BackupError::Refused(format!(
                    "another backup to this destination is running, from {} since {}; try again \
                     once it has finished",
                    held.host,
                    held.started.strftime("%Y-%m-%d %H:%M UTC")
                )));
            }
        }
        let now = Timestamp::now();
        let lock =
            Self { host: host.to_string(), pid: std::process::id(), started: now, touched: now };
        lock.write(target).await?;
        Ok(lock)
    }

    async fn write(&self, target: &Target) -> TargetResult<()> {
        let json = serde_json::to_vec_pretty(self).expect("a lock serialises");
        target.put(LOCK, Bytes::from(json)).await
    }

    pub async fn touch(&mut self, target: &Target) {
        self.touched = Timestamp::now();
        if let Err(e) = self.write(target).await {
            tracing::warn!(error = %e, "backup: could not refresh the lock");
        }
    }

    pub async fn release(self, target: &Target) {
        if let Err(e) = target.delete(LOCK).await {
            tracing::warn!(error = %e, "backup: could not remove the lock");
        }
    }
}

/// What [`verify`] found.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReport {
    pub snapshots: Vec<String>,
    /// Snapshots whose manifest could not be read, with why.
    pub unreadable: Vec<(String, String)>,
    pub chunks_referenced: u64,
    /// Chunks a manifest names that are not there.
    pub missing: Vec<String>,
    /// Chunks read back that did not open, when they were read.
    pub damaged: Vec<String>,
    pub chunks_read: u64,
}

impl VerifyReport {
    pub fn sound(&self) -> bool {
        self.unreadable.is_empty() && self.missing.is_empty() && self.damaged.is_empty()
    }
}

/// Check that every copy at `target` is whole: each manifest opens and every
/// chunk it names is there. With `read`, the newest copy's chunks are also
/// downloaded and opened, which is the only check that a byte has not
/// changed -- and costs a download of the vault.
pub async fn verify(
    target: &Target,
    keys: &BackupKeys,
    read: bool,
) -> Result<VerifyReport, BackupError> {
    let mut report = VerifyReport { snapshots: snapshots(target).await?, ..Default::default() };
    let present = chunks(target).await?;
    let cache = ManifestCache::new(None);
    let mut referenced = HashSet::new();
    let mut newest = None;
    for name in report.snapshots.clone() {
        match load_manifest(target, keys, &name, &cache).await {
            Ok(m) => {
                referenced.extend(m.chunks().map(str::to_string));
                newest = Some(m);
            }
            Err(e) => report.unreadable.push((name, e.to_string())),
        }
    }
    report.chunks_referenced = referenced.len() as u64;
    report.missing = referenced.iter().filter(|c| !present.contains(*c)).cloned().collect();
    report.missing.sort();
    if read && let Some(newest) = newest {
        let wanted: HashSet<&str> = newest.chunks().collect();
        for name in wanted {
            if !present.contains(name) {
                continue;
            }
            report.chunks_read += 1;
            let ok = match target.get(&chunk_object(name)).await? {
                Some(sealed) => keys.open_chunk(name, &sealed).is_ok(),
                None => false,
            };
            if !ok {
                report.damaged.push(name.to_string());
            }
        }
        report.damaged.sort();
    }
    Ok(report)
}

/// The staging folder for one run: under the vault's, named for this
/// process (`<pid>-<random>`) so a second process backing up the same vault
/// cannot empty it, and so other processes can tell it is in use -- see
/// [`staging_in_use`]. Leftovers from a run that crashed, or an earlier run
/// of this same process that could not finish removing its own, are cleared
/// on the way.
pub fn staging_dir(vault_root: &Path) -> std::io::Result<PathBuf> {
    let parent = vault_root.join(jatayu_core::vault::BACKUP_STAGING_DIRNAME);
    let me = std::process::id();
    if let Ok(entries) = std::fs::read_dir(&parent) {
        for entry in entries.flatten() {
            // A process makes one backup at a time, so any folder of ours is
            // an earlier one's; anybody else's is left alone while they live.
            let ours = owner(&entry.file_name()) == Some(me);
            if ours || !in_use(&entry) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    let dir = parent.join(format!("{me}-{:08x}", rand::random::<u32>()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Whether some process -- this one or another -- is making a backup of the
/// vault at `vault_root` right now: it has a staging folder there and is
/// still alive.
pub fn staging_in_use(vault_root: &Path) -> bool {
    let parent = vault_root.join(jatayu_core::vault::BACKUP_STAGING_DIRNAME);
    std::fs::read_dir(parent).is_ok_and(|entries| entries.flatten().any(|e| in_use(&e)))
}

/// The process a staging folder's name says made it.
fn owner(name: &std::ffi::OsStr) -> Option<u32> {
    name.to_str()?.split_once('-')?.0.parse().ok()
}

/// Whether a staging folder belongs to a backup still going. On Linux, by
/// whether the process that made it is alive -- a backup's first copy up a
/// slow line can take a day, and must not be swept away from under it. Where
/// that cannot be asked, by age: younger than two days.
fn in_use(entry: &std::fs::DirEntry) -> bool {
    #[cfg(target_os = "linux")]
    if let Some(pid) = owner(&entry.file_name()) {
        return Path::new("/proc").join(pid.to_string()).exists();
    }
    entry
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|age| age < std::time::Duration::from_secs(2 * 24 * 60 * 60))
}
