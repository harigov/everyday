//! Backups on a schedule, whether or not a window is open.
//!
//! One supervised task, on the arrangement the calendars' refresher has:
//! started at unlock, stopped by the lock, so a backup is never made from a
//! locked vault. It looks every few minutes, and when woken by a settings
//! save, at whether one is due ([`super::next_due`]), and starts it.
//!
//! A backup that is due while the computer is asleep or the vault is locked
//! is made at the next unlock: due means "at or after", and a laptop opened
//! on Monday makes Sunday's.
//!
//! # Saying so
//!
//! A backup that fails once says nothing: a laptop on a train, a disk not
//! plugged in tonight. One that has been failing for longer than the
//! schedule's own interval (and at least an hour) is falling behind, and
//! that is said, once, as a notification that reaches somebody who is not
//! looking at Settings. The next one that works says it is working again,
//! under the same key, replacing the warning.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use jatayu_core::Vault;
use jatayu_core::backup::{BackupReport, BackupStatus};
use jiff::{SignedDuration, Timestamp};
use tokio::sync::{Notify, watch};

use super::{BackupError, Claim, Plan, Trigger};
use crate::events::{Change, Kind, Level, Notification, Op};
use crate::service::Service;
use crate::supervisor::{Outcome, TaskResult};

/// The supervisor's key for this task.
pub const TASK_KEY: &str = "backup";

/// How often to look whether a backup is due.
const LOOK_EVERY: Duration = Duration::from_secs(5 * 60);

/// How long after the vault unlocks before a backup that is due starts: long
/// enough for the windows to open and mail to have its first look, which is
/// what somebody unlocking is waiting for.
const SETTLE_FIRST: Duration = Duration::from_secs(90);

/// The notification's key, so a warning and the all-clear replace each
/// other.
const NOTICE_KEY: &str = "backup";

fn wake_signal() -> &'static Arc<Notify> {
    static WAKE: OnceLock<Arc<Notify>> = OnceLock::new();
    WAKE.get_or_init(|| Arc::new(Notify::new()))
}

/// Whether a failure has been said and not yet taken back.
static SAID: AtomicBool = AtomicBool::new(false);

/// Look again now: the settings changed.
pub fn wake() {
    wake_signal().notify_one();
}

/// Start the scheduler if it is not already running. Idempotent; called
/// from every unlock.
pub fn ensure_task(svc: &Arc<Service>, vault: &Arc<Vault>) {
    if !vault.is_writable() {
        return;
    }
    let svc_for_task = svc.clone();
    svc.supervisor().ensure(TASK_KEY, move |stop| {
        let svc = svc_for_task.clone();
        Box::pin(async move {
            match svc.get() {
                Some(vault) if vault.is_writable() => run(svc, vault, stop).await,
                _ => Ok(Outcome::Done),
            }
        })
    });
}

async fn run(svc: Arc<Service>, vault: Arc<Vault>, mut stop: watch::Receiver<bool>) -> TaskResult {
    let wake = wake_signal().clone();
    let mut look = tokio::time::Instant::now() + SETTLE_FIRST;
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
                // A save that turned backups on: look soon, not at once, so
                // somebody still choosing settings is not overtaken by a run.
                look = look.min(tokio::time::Instant::now() + Duration::from_secs(5));
            }
            () = tokio::time::sleep_until(look) => {
                look = tokio::time::Instant::now() + LOOK_EVERY;
                if !vault.is_unlocked() || super::running() {
                    continue;
                }
                let due = {
                    let vault = vault.clone();
                    tokio::task::spawn_blocking(move || is_due(&vault)).await.unwrap_or(false)
                };
                if due && let Err(why) = start(&svc, &vault, Trigger::Schedule) {
                    tracing::debug!("backup: not started: {why}");
                }
            }
        }
    }
}

fn is_due(vault: &Vault) -> bool {
    let Ok(settings) = vault.backup_settings() else { return false };
    let Some(destination) = &settings.destination else { return false };
    let Ok(status) = vault.backup_status() else { return false };
    let status = status.for_destination(destination);
    super::next_due(&settings, &status).is_some_and(|due| due <= Timestamp::now())
        && vault.supports_backup()
}

/// Start a backup of `vault`, in the background. Fails at once, with why,
/// if one is already running.
pub fn start(svc: &Arc<Service>, vault: &Arc<Vault>, trigger: Trigger) -> Result<(), String> {
    let claim = super::claim(trigger == Trigger::Now)
        .ok_or_else(|| "a backup is already running".to_string())?;
    let (svc, vault) = (svc.clone(), vault.clone());
    tokio::spawn(async move { run_and_record(svc, vault, claim, trigger).await });
    Ok(())
}

async fn run_and_record(svc: Arc<Service>, vault: Arc<Vault>, claim: Claim, trigger: Trigger) {
    let progress = claim.progress();
    let attempt = Timestamp::now();
    let planned = {
        let vault = vault.clone();
        tokio::task::spawn_blocking(move || plan(&vault, trigger))
            .await
            .unwrap_or_else(|e| (None, Err(BackupError::Refused(e.to_string()))))
    };
    // A plan that failed after the destination was known -- a key that
    // would not open, a status that would not read -- is recorded against
    // it like any other failure, so "Back up now" never ends in silence.
    let (destination, result) = match planned {
        (destination, Ok(plan)) => {
            (destination, super::run(vault.clone(), plan, progress.clone()).await)
        }
        (destination, Err(e)) => (destination, Err(e)),
    };
    let stopped_by_user = progress.stopped_by_user();
    drop(claim);
    match &result {
        Ok(report) => tracing::info!(
            snapshot = %report.snapshot,
            files = report.files,
            uploaded_bytes = report.uploaded_bytes,
            seconds = report.seconds,
            "backup made"
        ),
        Err(BackupError::Cancelled) => tracing::info!("backup stopped"),
        Err(e) => tracing::warn!(error = %e, "backup failed"),
    }
    if let Some(destination) = destination {
        let vault_for_record = vault.clone();
        let svc_for_record = svc.clone();
        let _ = tokio::task::spawn_blocking(move || {
            record(
                &svc_for_record,
                &vault_for_record,
                &destination,
                attempt,
                &result,
                trigger,
                stopped_by_user,
            )
        })
        .await;
    }
    svc.events().changed(Change::new(Kind::Settings, Op::Updated));
}

/// Why a backup cannot start right now, said before it is started: no
/// destination chosen, or a vault this computer cannot back up. `None` when
/// it can. For "Back up now", so the button answers with the reason rather
/// than starting something that stops at once.
pub fn refusal(vault: &Vault) -> Option<String> {
    match vault.backup_settings() {
        Ok(settings) if settings.destination.is_none() => {
            Some("choose where backups go first".into())
        }
        Ok(_) if !vault.supports_backup() => Some(UNSUPPORTED.into()),
        Ok(_) => None,
        Err(e) => Some(e.to_string()),
    }
}

const UNSUPPORTED: &str =
    "this vault's database is on a server, which is backed up by whoever runs it";

/// The plan for a backup of `vault` as its settings stand, beside the
/// destination it is for -- known as soon as the settings are read, so a
/// failure after that can be recorded against it.
fn plan(
    vault: &Vault,
    trigger: Trigger,
) -> (Option<jatayu_core::backup::Destination>, Result<Plan, BackupError>) {
    let settings = match vault.backup_settings() {
        Ok(s) => s,
        Err(e) => return (None, Err(e.into())),
    };
    let Some(destination) = settings.destination.clone() else {
        return (None, Err(BackupError::Refused("choose where backups go first".into())));
    };
    let planned = (|| {
        if !vault.supports_backup() {
            return Err(BackupError::Refused(UNSUPPORTED.into()));
        }
        let status = vault.backup_status()?.for_destination(&destination);
        let keys = vault.backup_keys()?;
        Ok(Plan {
            destination: destination.clone(),
            keep: settings.keep,
            config_dir: settings.include_config.then(jatayu_vault::config_dir),
            // The files folder goes in every copy once there is one: it is
            // the person's own files, and the vault's catalog of them is
            // worth nothing without them. See `run::stage_files_folder`.
            files: vault.supports_files(),
            // Only a backup nobody is watching treats an empty destination
            // that has held backups as one that is not there. Somebody
            // pressing the button is looking at the place, and may have
            // emptied it on purpose; see `repo::prepare`.
            expect_existing: trigger == Trigger::Schedule && status.has_used(&destination),
            s3_keys: vault.backup_s3_keys()?,
            cache_dir: Some(cache_dir(&keys.repository_id())),
            host: super::host_name(),
        })
    })();
    (Some(destination), planned)
}

/// Where one repository's manifests are kept between runs: beside the
/// downloaded models, never inside anything a backup copies.
pub fn cache_dir(repository: &str) -> std::path::PathBuf {
    jatayu_vault::data_dir().join("backup-cache").join(repository)
}

/// Write down in the vault what became of a backup to `destination` that
/// started at `attempt`, and answer the status as it now stands -- `None`
/// when nothing changed: a backup stopped by the vault locking. One stopped
/// by somebody is recorded as stopped, so the schedule waits a whole
/// interval before trying again (see [`super::next_due`]). For the scheduler
/// and for the command line alike.
pub fn save_outcome(
    vault: &Vault,
    destination: &jatayu_core::backup::Destination,
    attempt: Timestamp,
    result: &Result<BackupReport, BackupError>,
    stopped_by_user: bool,
) -> Option<BackupStatus> {
    let mut status: BackupStatus = vault.backup_status().ok()?.for_destination(destination);
    match result {
        Ok(report) => {
            status.last_attempt = Some(attempt);
            status.last_success = Some(Timestamp::now());
            status.last_error = None;
            status.failing_since = None;
            status.last_report = Some(report.clone());
            status.mark_used(destination);
        }
        Err(BackupError::Cancelled) if stopped_by_user => {
            status.last_attempt = Some(attempt);
            status.stopped_at = Some(Timestamp::now());
        }
        Err(BackupError::Cancelled) => return None,
        Err(e) => {
            status.last_attempt = Some(attempt);
            status.last_error = Some(e.to_string());
            status.failing_since.get_or_insert(attempt);
        }
    }
    if let Err(e) = vault.save_backup_status(&status) {
        tracing::warn!(error = %e, "backup: could not record how it went");
    }
    Some(status)
}

/// Write down what became of a backup, and say so if it is news.
fn record(
    svc: &Service,
    vault: &Vault,
    destination: &jatayu_core::backup::Destination,
    attempt: Timestamp,
    result: &Result<BackupReport, BackupError>,
    trigger: Trigger,
    stopped_by_user: bool,
) {
    let Some(status) = save_outcome(vault, destination, attempt, result, stopped_by_user) else {
        return;
    };
    match result {
        Ok(_) => {
            if SAID.swap(false, Ordering::Relaxed) {
                svc.events().notify(
                    Notification::new(Level::Success, "Backups are working again")
                        .body(format!("A backup was made to {}.", destination.label()))
                        .for_user()
                        .key(NOTICE_KEY),
                );
            }
        }
        Err(BackupError::Cancelled) => {}
        Err(e) => {
            let since = status.failing_since.unwrap_or(attempt);
            if trigger == Trigger::Schedule
                && falling_behind(vault, since)
                && !SAID.swap(true, Ordering::Relaxed)
            {
                let last = status
                    .last_success
                    .map(|t| format!("The last one that worked was on {}.", t.strftime("%a %d %b")))
                    .unwrap_or_else(|| "None has worked yet.".into());
                svc.events().notify(
                    Notification::warning("Backups are not being made")
                        .body(format!("{e} {last} See Settings \u{2192} Backup."))
                        .for_user()
                        .key(NOTICE_KEY),
                );
            }
        }
    }
}

/// Whether failures since `since` have gone on longer than the schedule's
/// own interval, and at least an hour.
fn falling_behind(vault: &Vault, since: Timestamp) -> bool {
    let every = vault.backup_settings().map(|s| s.every_hours).unwrap_or(24);
    let span = SignedDuration::from_hours(i64::from(every)).max(SignedDuration::from_hours(1));
    Timestamp::now().duration_since(since) >= span
}
