//! Backups: sealed, date-stamped copies of the vault and this computer's
//! settings, in a folder or a bucket, made on a schedule and pruned to a
//! number of copies.
//!
//! # A repository, not a pile of archives
//!
//! A destination holds one repository per vault:
//!
//! ```text
//!   <folder, or bucket/prefix>/
//!     jatayu-backup.json         what this is, and whose (a keyed id)
//!     vault-header.json          the vault's header: what the password opens
//!     README.txt                 for whoever finds it
//!     snapshots/
//!       2026-11-14T030512Z.snapshot     one sealed manifest per copy
//!     chunks/3f/3f9a…            sealed pieces of files, named by keyed hash
//!     lock.json                  while a backup is writing
//! ```
//!
//! A repository made while the application was called Karma Yoga has a
//! `karmayoga-backup.json` where this says `jatayu-backup.json`, and nothing
//! else different: it is found by that, backed up into and restored from as
//! it is, and never given a second marker (see `repo::marker`).
//!
//! Each manifest lists every file in that copy and the chunks it is made of,
//! so restoring one needs nothing from any other -- every copy is whole. But
//! two copies share every chunk that did not change between them, so a vault
//! of tens of gigabytes kept seven times over costs about one vault plus a
//! week of changes, and a nightly backup sends only what changed. That is the
//! whole reason this is not a zip per day: a mail vault is too large for that
//! to be something anybody would leave switched on.
//!
//! Sealing, naming and the key are `jatayu_core::backup`'s; see there for
//! why the key is the vault's own and the header is kept in the clear.
//!
//! # Chunks
//!
//! Every file is cut at fixed offsets: a database every 64 KiB, anything else
//! every megabyte. A database is copied page for page (see
//! `jatayu_store_sqlite`'s `backup_job`), so a day of mail changes only
//! the pages it touched, scattered through the file; small chunks keep each
//! of those from costing a megabyte. Everything else -- attachments, mail
//! packs -- is written once and appended to, never edited in the middle, so
//! large chunks cost nothing in what is shared and keep the number of
//! objects down. A file whose size and modification time match the last
//! copy's is not read at all: its chunks are the last copy's.
//!
//! # What a copy holds
//!
//! The vault -- its database (a consistent copy, made while the app keeps
//! writing), attachments, mail packs, a chat link's own store, recordings not
//! yet written up -- and, unless Settings says otherwise, this computer's
//! settings folder: the paired devices, the server's certificate and key, the
//! MCP token. Those travel sealed like everything else. Left out, because
//! each is either made again or not this vault's: downloaded models, the mail
//! search index, the pointer to the last vault opened, and the key kept in
//! the system keychain.
//!
//! And the files folder, when the vault has one set up: its files go in the
//! copy under `files/`, minus the folder Jatayu keeps for itself and the
//! names a scan of the folder leaves out. They are plain files on the disk
//! and sealed here like the rest.
//!
//! # A files folder that is not there
//!
//! The folder may be on an encrypted volume or a drive that is not mounted
//! tonight. A backup that took that for an empty folder would write a copy
//! without the files, and a few nights of it would push every copy that held
//! them out of "keep 7". So while the folder is unavailable -- the vault
//! says so, by the marker file its folder carries -- the previous copy's
//! `files/` entries are carried into the new manifest unchanged: the chunks
//! are already there, nothing is sent, and the run says it was done
//! ([`jatayu_core::backup::BackupReport::files_folder_unavailable`]).
//!
//! `files/` goes back to the folder the vault had, or one a restore is told
//! of, and one file can be brought back alone ([`restore::restore_file`]).

mod creds;
pub mod folder;
pub mod gcs;
mod manifest;
mod repo;
pub mod restore;
mod run;
pub mod s3;
pub mod target;
pub mod task;

pub use repo::{VerifyReport, verify};
pub use restore::{RestoreOptions, RestoreReport, restore};
pub use run::{Plan, run};

use jatayu_core::backup::{
    BackupKeys, BackupSettings, BackupStatus, Destination, S3Keys, SnapshotInfo,
};
use jiff::{SignedDuration, Timestamp};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use target::{Target, TargetError, TargetErrorKind};

/// What a backup is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BackupPhase {
    /// Copying the databases, listing files.
    Preparing,
    /// Reading what the destination already has.
    Listing,
    /// Sending what it does not.
    Copying,
    /// Letting go of copies beyond the number kept.
    Pruning,
}

/// A backup in progress, as Settings draws it. Mirrors the TS
/// `BackupProgress`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupProgress {
    pub phase: BackupPhase,
    pub manual: bool,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub uploaded_bytes: u64,
    pub uploaded_chunks: u64,
    pub started: Timestamp,
}

/// What trying a destination came to. Mirrors the TS `DestinationCheck`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationCheck {
    pub ok: bool,
    pub message: String,
}

/// Why a backup is being made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Its time came round.
    Schedule,
    /// Somebody pressed "Back up now".
    Now,
}

/// Why a backup or a restore stopped.
#[derive(Debug)]
pub enum BackupError {
    /// The destination failed.
    Target(TargetError),
    /// The vault did: locked, unreadable, a disk full while staging.
    Vault(jatayu_core::Error),
    /// Somebody stopped it, or the vault locked.
    Cancelled,
    /// Something about the repository or the request that is nobody's
    /// network: another vault's backups, a copy that is not there, a folder
    /// to restore into that is not empty.
    Refused(String),
}

impl std::fmt::Display for BackupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Target(e) => f.write_str(&e.message),
            Self::Vault(e) => write!(f, "{e}"),
            Self::Cancelled => f.write_str("the backup was stopped"),
            Self::Refused(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for BackupError {}

impl From<TargetError> for BackupError {
    fn from(e: TargetError) -> Self {
        Self::Target(e)
    }
}

impl From<jatayu_core::Error> for BackupError {
    fn from(e: jatayu_core::Error) -> Self {
        Self::Vault(e)
    }
}

impl BackupError {
    /// Whether this is the destination not being reachable at all -- a
    /// laptop offline, a disk not plugged in -- rather than anything that
    /// needs somebody to act.
    pub fn unreachable(&self) -> bool {
        matches!(
            self,
            Self::Target(TargetError {
                kind: TargetErrorKind::Unreachable | TargetErrorKind::Missing,
                ..
            })
        )
    }
}

/// One backup's progress, shared between the run and whoever asks after it.
pub struct Progress {
    manual: bool,
    started: Timestamp,
    phase: Mutex<BackupPhase>,
    files_done: AtomicU64,
    files_total: AtomicU64,
    bytes_done: AtomicU64,
    bytes_total: AtomicU64,
    uploaded_bytes: AtomicU64,
    uploaded_chunks: AtomicU64,
    cancelled: AtomicBool,
    /// Stopped by somebody, rather than by the vault locking.
    by_user: AtomicBool,
}

impl Progress {
    pub fn new(manual: bool) -> Arc<Self> {
        Arc::new(Self {
            manual,
            started: Timestamp::now(),
            phase: Mutex::new(BackupPhase::Preparing),
            files_done: AtomicU64::new(0),
            files_total: AtomicU64::new(0),
            bytes_done: AtomicU64::new(0),
            bytes_total: AtomicU64::new(0),
            uploaded_bytes: AtomicU64::new(0),
            uploaded_chunks: AtomicU64::new(0),
            cancelled: AtomicBool::new(false),
            by_user: AtomicBool::new(false),
        })
    }

    fn set_phase(&self, phase: BackupPhase) {
        *self.phase.lock().unwrap_or_else(|e| e.into_inner()) = phase;
    }

    fn totals(&self, files: u64, bytes: u64) {
        self.files_total.store(files, Ordering::Relaxed);
        self.bytes_total.store(bytes, Ordering::Relaxed);
    }

    fn file_done(&self) {
        self.files_done.fetch_add(1, Ordering::Relaxed);
    }

    fn read(&self, bytes: u64) {
        self.bytes_done.fetch_add(bytes, Ordering::Relaxed);
    }

    fn uploaded(&self, bytes: u64) {
        self.uploaded_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.uploaded_chunks.fetch_add(1, Ordering::Relaxed);
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Stopped because somebody asked: [`stop`] rather than [`cancel`].
    pub fn stopped_by_user(&self) -> bool {
        self.by_user.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> BackupProgress {
        BackupProgress {
            phase: *self.phase.lock().unwrap_or_else(|e| e.into_inner()),
            manual: self.manual,
            files_done: self.files_done.load(Ordering::Relaxed),
            files_total: self.files_total.load(Ordering::Relaxed),
            bytes_done: self.bytes_done.load(Ordering::Relaxed),
            bytes_total: self.bytes_total.load(Ordering::Relaxed),
            uploaded_bytes: self.uploaded_bytes.load(Ordering::Relaxed),
            uploaded_chunks: self.uploaded_chunks.load(Ordering::Relaxed),
            started: self.started,
        }
    }
}

/// The one backup this process may have going. One at a time, because two
/// would race for the destination's lock and stage the same databases.
static CURRENT: Mutex<Option<Arc<Progress>>> = Mutex::new(None);

/// Holding the slot. Dropping it frees the slot.
pub struct Claim(Arc<Progress>);

impl Claim {
    pub fn progress(&self) -> Arc<Progress> {
        self.0.clone()
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut current = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
        if current.as_ref().is_some_and(|p| Arc::ptr_eq(p, &self.0)) {
            *current = None;
        }
    }
}

/// Take the slot for a backup, or `None` if one is already running.
pub fn claim(manual: bool) -> Option<Claim> {
    let mut current = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    if current.is_some() {
        return None;
    }
    let progress = Progress::new(manual);
    *current = Some(progress.clone());
    Some(Claim(progress))
}

/// The backup this process is making, if it is making one.
pub fn progress() -> Option<BackupProgress> {
    CURRENT.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|p| p.snapshot())
}

/// Whether a backup is running in this process. Mail's compaction waits for
/// it: a pack rewritten and dropped between the database being copied and
/// the packs being read would be messages the copy names and does not hold.
pub fn running() -> bool {
    CURRENT.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// Stop the backup in progress, if there is one, because the vault is
/// locking: a backup holds keys derived from the vault's, and the lock
/// promises none are left in memory. Nothing is recorded; the backup is
/// still due, and is made after the next unlock.
pub fn cancel() {
    if let Some(p) = CURRENT.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        p.cancel();
    }
}

/// Stop the backup in progress because somebody pressed Stop. Recorded, so
/// the schedule waits a whole interval instead of starting it again at its
/// next look.
pub fn stop() {
    if let Some(p) = CURRENT.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        p.by_user.store(true, Ordering::Relaxed);
        p.cancel();
    }
}

/// Whether a backup is running of the vault at `vault_root`, in this process
/// or any other -- `jatayu backups run` on the command line while the app
/// is open. Mail's compaction asks, before it drops a pack a backup may
/// still be about to read. Another process is seen by its staging folder;
/// see [`repo::staging_in_use`].
pub fn running_on(vault_root: &std::path::Path) -> bool {
    running() || repo::staging_in_use(vault_root)
}

/// How long after a failed backup the next is tried: half an hour, or the
/// schedule's own interval if that is shorter.
const RETRY_AFTER: SignedDuration = SignedDuration::from_mins(30);

/// When the next backup is due on its own, or `None` when they are off.
///
/// Due `every_hours` after the last one that worked -- and at once if none
/// ever has. After a failure, not before [`RETRY_AFTER`] has passed, so a
/// destination that keeps refusing is not asked every few minutes. After
/// somebody pressed Stop, not before a whole interval has passed.
pub fn next_due(settings: &BackupSettings, status: &BackupStatus) -> Option<Timestamp> {
    if !settings.enabled || settings.destination.is_none() {
        return None;
    }
    let every = SignedDuration::from_hours(i64::from(settings.every_hours));
    let scheduled = status
        .last_success
        .and_then(|t| t.checked_add(every).ok())
        .unwrap_or(Timestamp::UNIX_EPOCH);
    let mut due = match (status.last_error.is_some(), status.last_attempt) {
        (true, Some(attempt)) => {
            let retry = attempt.checked_add(RETRY_AFTER.min(every)).unwrap_or(attempt);
            scheduled.max(retry)
        }
        _ => scheduled,
    };
    // Stopped by somebody since the last one that worked: that backup is
    // skipped, and the next is a whole interval later.
    if let Some(stopped) = status.stopped_at
        && status.last_success.is_none_or(|ok| stopped > ok)
    {
        due = due.max(stopped.checked_add(every).unwrap_or(stopped));
    }
    Some(due)
}

/// The copies kept at `destination`, newest first. Needs no key: the names
/// are the dates.
pub async fn list_snapshots(
    destination: &Destination,
    s3_keys: Option<S3Keys>,
) -> Result<Vec<SnapshotInfo>, TargetError> {
    let target = Target::open(destination, s3_keys)?;
    let mut names = repo::snapshots(&target).await?;
    names.reverse();
    Ok(names
        .into_iter()
        .filter_map(|name| {
            let created = jatayu_core::backup::parse_snapshot_name(&name)?;
            Some(SnapshotInfo { name, created })
        })
        .collect())
}

/// Try `destination`: write a small object, read it back, delete it, and
/// look at what is already there -- answering in words either way.
///
/// A folder is made if it is not there yet, since trying it is somebody
/// choosing it.
pub async fn check_destination(
    destination: &Destination,
    s3_keys: Option<S3Keys>,
    keys: Option<&BackupKeys>,
) -> DestinationCheck {
    match try_destination(destination, s3_keys, keys).await {
        Ok(message) => DestinationCheck { ok: true, message },
        Err(e) => DestinationCheck { ok: false, message: e.to_string() },
    }
}

async fn try_destination(
    destination: &Destination,
    s3_keys: Option<S3Keys>,
    keys: Option<&BackupKeys>,
) -> Result<String, BackupError> {
    let target = Target::open(destination, s3_keys)?;
    target.create_root().await?;
    let probe = format!("probe-{:016x}", rand::random::<u64>());
    let body = bytes::Bytes::from_static(b"jatayu backup probe");
    target.put(&probe, body.clone()).await?;
    let back = target.get(&probe).await;
    target.delete(&probe).await?;
    if back?.as_deref() != Some(&body[..]) {
        return Err(BackupError::Refused(
            "what was written there did not read back the same".into(),
        ));
    }
    let snapshots = repo::snapshots(&target).await?.len();
    match (repo::marker(&target).await?, keys) {
        (None, _) => Ok(format!("Works. Nothing is kept at {} yet.", destination.label())),
        (Some(marker), Some(keys)) if marker.repository != keys.repository_id() => {
            Err(BackupError::Refused(format!(
                "{} holds another vault's backups; choose somewhere else",
                destination.label()
            )))
        }
        (Some(_), _) => Ok(match snapshots {
            1 => "Works. One backup of this vault is kept there already.".to_string(),
            n => format!("Works. {n} backups of this vault are kept there already."),
        }),
    }
}

/// How to reach a bucket named on a command line, beyond its name.
#[derive(Debug, Clone, Default)]
pub struct LocationOptions {
    pub region: Option<String>,
    pub endpoint: Option<String>,
    pub profile: Option<String>,
    pub credentials_file: Option<String>,
}

/// A destination written as a person would type it: `s3://bucket/prefix`,
/// `gs://bucket/prefix`, or a folder. A relative folder is taken from where
/// the command was run. A bucket's credentials are always this computer's;
/// a pasted access key lives in a vault, and a restore may have none.
pub fn parse_location(raw: &str, options: &LocationOptions) -> Result<Destination, String> {
    fn split(rest: &str) -> (String, String) {
        let rest = rest.trim_matches('/');
        match rest.split_once('/') {
            Some((bucket, prefix)) => (bucket.to_string(), prefix.to_string()),
            None => (rest.to_string(), String::new()),
        }
    }
    let destination = if let Some(rest) = raw.strip_prefix("s3://") {
        let (bucket, prefix) = split(rest);
        Destination::S3 {
            bucket,
            prefix,
            region: options.region.clone(),
            endpoint: options.endpoint.clone(),
            profile: options.profile.clone(),
            credentials: jatayu_core::backup::S3Credentials::ThisComputer,
        }
    } else if let Some(rest) = raw.strip_prefix("gs://") {
        let (bucket, prefix) = split(rest);
        Destination::Gcs { bucket, prefix, credentials_file: options.credentials_file.clone() }
    } else if raw.contains("://") {
        return Err(format!("{raw} is not a folder, an s3:// bucket or a gs:// bucket"));
    } else {
        let path = std::path::Path::new(raw);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().map_err(|e| e.to_string())?.join(path)
        };
        Destination::Folder { path: path.display().to_string() }
    }
    .normalized();
    destination.validate().map_err(|e| e.to_string())?;
    Ok(destination)
}

/// This computer's name, for a manifest and a lock: which machine made a
/// copy, or is making one now.
pub fn host_name() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests;
