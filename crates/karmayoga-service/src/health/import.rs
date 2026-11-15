//! Importing Apple Health's `export.zip`: one at a time, on a thread of its
//! own, batch by batch through `Vault::ingest_health`, with progress the
//! interface polls and a cancel that stops at the next batch.
//!
//! The status is process-wide -- one vault is open, one import runs -- and
//! answered by [`status`] to `apple_import_status`. A change of
//! `HealthSample` is raised after each batch so the screens fill in as it
//! goes.
//!
//! # Why a thread, and not a task
//!
//! The reader is synchronous and the export is gigabytes: it would hold a
//! blocking-pool thread for the better part of an hour, and the pool is what
//! every command in the application waits on. A thread of its own costs the
//! same stack and leaves the pool alone.
//!
//! # What the command does not touch
//!
//! The source an import files under -- an existing Apple export source, or a
//! new one named "Apple Health export" -- is found or made on the import's
//! own thread, not in [`start`]. Creating it there means `import_apple_health`
//! itself writes no record, so what it declares (samples changed) is all it
//! did; the source's creation is announced by the import as it happens.

use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Change, Kind, Op};
use crate::service::Service;
use karmayoga_core::Vault;
use karmayoga_core::error::Error;
use karmayoga_core::health::connect::apple::{self, Progress};
use karmayoga_core::health::{
    Connector, HealthBatch, HealthIngestReport, HealthSource, ImportStatus, SourceStatus,
};
use karmayoga_core::id::HealthSourceId;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// What the source an import creates is called.
const SOURCE_NAME: &str = "Apple Health export";

/// What the reader answers, as `Error::Invalid`, when [`CANCEL`] stopped it.
const CANCELLED: &str = "cancelled";

const RUNNING: &str = "running";

/// Where the current (or last) import is. `None` until the first starts.
static STATUS: Mutex<Option<ImportStatus>> = Mutex::new(None);

/// Set by [`cancel`], read by the reader at each batch boundary.
static CANCEL: AtomicBool = AtomicBool::new(false);

fn current() -> MutexGuard<'static, Option<ImportStatus>> {
    // A panic while the status was held could only have been in the one-line
    // closures below, which leave it whole; carry on with what is there.
    STATUS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Change the status, if there is one.
fn update(change: impl FnOnce(&mut ImportStatus)) {
    if let Some(status) = current().as_mut() {
        change(status);
    }
}

/// Start importing the export at `path` into `source` (an Apple export
/// source; one is created, named "Apple Health export", if `None`).
/// Refused while another import runs.
/// What to do once an import has ended, on its own thread: told whether it
/// finished (rather than failing or being stopped). How an import read from
/// a file in the files catalog lets go of the decrypted copy it read, and
/// puts an export uploaded only to be imported in the trash.
pub type Finished = Box<dyn FnOnce(&Vault, bool) + Send>;

pub fn start(
    svc: &Arc<Service>,
    vault: Arc<Vault>,
    path: PathBuf,
    source: Option<HealthSourceId>,
    finished: Option<Finished>,
) -> CommandResult<ImportStatus> {
    let meta = std::fs::metadata(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            CommandError::new(codes::NOT_FOUND, "that file is not there")
        }
        _ => CommandError::new(codes::IO, "that file cannot be read"),
    })?;
    if !meta.is_file() {
        return Err(CommandError::new(codes::INVALID, "that is not a file"));
    }
    if let Some(id) = source {
        let found = vault.health_source(id)?;
        if found.connector != Connector::AppleExport {
            return Err(CommandError::new(
                codes::INVALID,
                "that connection does not take an Apple Health export",
            ));
        }
    }

    let started = ImportStatus {
        state: RUNNING.into(),
        file_name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        started_at: Some(svc.now()),
        ..ImportStatus::default()
    };
    {
        let mut status = current();
        if status.as_ref().is_some_and(|s| s.state == RUNNING) {
            return Err(CommandError::new(
                codes::BUSY,
                "an Apple Health import is already running",
            ));
        }
        CANCEL.store(false, Ordering::SeqCst);
        *status = Some(started.clone());
    }

    let worker = {
        let svc = svc.clone();
        std::thread::Builder::new()
            .name("health-import".into())
            .spawn(move || run(&svc, &vault, &path, source, finished))
    };
    if let Err(e) = worker {
        tracing::warn!(error = %e, "could not start the Apple Health import's thread");
        let now = svc.now();
        update(|s| {
            s.state = "failed".into();
            s.error = "the import could not be started".into();
            s.finished_at = Some(now);
        });
        return Err(CommandError::new(codes::INTERNAL, "the import could not be started"));
    }
    Ok(started)
}

/// Where the current (or last) import is.
pub fn status() -> ImportStatus {
    let idle = || ImportStatus { state: "idle".into(), ..ImportStatus::default() };
    current().clone().unwrap_or_else(idle)
}

/// Ask the running import to stop. Answers whether one was running.
pub fn cancel() -> bool {
    let running = current().as_ref().is_some_and(|s| s.state == RUNNING);
    if running {
        CANCEL.store(true, Ordering::SeqCst);
    }
    running
}

/// The import's thread: read, then settle the status however it ended, so
/// that nothing is left saying `running` for ever.
fn run(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    path: &Path,
    source: Option<HealthSourceId>,
    finished: Option<Finished>,
) {
    let outcome =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read(svc, vault, path, source)));
    let (state, error) = match outcome {
        Ok(result) => settle(result),
        Err(_) => {
            tracing::warn!("an Apple Health import panicked");
            ("failed", "the import stopped unexpectedly".to_string())
        }
    };
    if let Some(finished) = finished {
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            finished(vault, state == "done")
        }));
        if ran.is_err() {
            tracing::warn!("tidying up after an Apple Health import panicked");
        }
    }
    let now = svc.now();
    update(|s| {
        s.state = state.into();
        s.error = error;
        s.finished_at = Some(now);
    });
    let events = svc.events();
    events.changed(Change::new(Kind::HealthSource, Op::Updated));
    events.changed(Change::new(Kind::HealthSample, Op::Updated));
}

/// How an import ended, as the state and the error the status shows. Only
/// the kind of a failure is logged: what it names is somebody's health.
fn settle(result: karmayoga_core::Result<()>) -> (&'static str, String) {
    match result {
        Ok(()) => ("done", String::new()),
        Err(Error::Invalid(why)) if why == CANCELLED => ("cancelled", String::new()),
        Err(e) => {
            tracing::warn!(code = e.code(), "an Apple Health import failed");
            ("failed", CommandError::from(e).message)
        }
    }
}

/// Read the whole export into the vault.
fn read(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    path: &Path,
    source: Option<HealthSourceId>,
) -> karmayoga_core::Result<()> {
    // Opened first: a file that cannot be read must not leave a source behind.
    let file = File::open(path)?;
    let source = source_for(svc, vault, source)?;
    let mut sink = |batch: HealthBatch| -> karmayoga_core::Result<()> {
        let report = vault.ingest_health(source, batch, None)?;
        announce(svc, &report);
        update(|s| s.report.absorb(report));
        Ok(())
    };
    let mut progress = |p: Progress| {
        update(|s| {
            s.read = p.read;
            s.total = p.total;
        })
    };
    apple::read_export(BufReader::new(file), &mut sink, &mut progress, &CANCEL)
}

/// The source to file under: the one asked for, else an Apple export source
/// already there, else a new one.
fn source_for(
    svc: &Arc<Service>,
    vault: &Vault,
    wanted: Option<HealthSourceId>,
) -> karmayoga_core::Result<HealthSourceId> {
    if let Some(id) = wanted {
        return Ok(id);
    }
    let sources = vault.health_sources()?;
    if let Some(found) = sources.iter().find(|s| s.connector == Connector::AppleExport) {
        return Ok(found.id);
    }
    let now = svc.now();
    let source = HealthSource {
        id: HealthSourceId::new(),
        connector: Connector::AppleExport,
        name: SOURCE_NAME.into(),
        paused: false,
        status: SourceStatus::default(),
        token_id: String::new(),
        created_at: now,
        updated_at: now,
    };
    let saved = vault.save_health_source(&source)?;
    svc.events().changed(Change {
        id: Some(saved.id.to_string()),
        ..Change::new(Kind::HealthSource, Op::Created)
    });
    Ok(saved.id)
}

/// Tell the windows what one batch brought: samples always, and the other
/// lists when it carried something for them.
fn announce(svc: &Arc<Service>, report: &HealthIngestReport) {
    let events = svc.events();
    events.changed(Change::new(Kind::HealthSample, Op::Updated));
    if report.workouts_added + report.workouts_updated > 0 {
        events.changed(Change::new(Kind::Workout, Op::Updated));
    }
    if report.doses_added > 0 {
        events.changed(Change::new(Kind::Dose, Op::Updated));
    }
    if report.documents_added > 0 {
        events.changed(Change::new(Kind::HealthDocument, Op::Created));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finished_read_is_done_with_nothing_to_say() {
        assert_eq!(settle(Ok(())), ("done", String::new()));
    }

    #[test]
    fn a_read_the_person_stopped_is_cancelled_and_not_an_error() {
        let stopped = Err(Error::Invalid(CANCELLED.into()));
        assert_eq!(settle(stopped), ("cancelled", String::new()));
    }

    #[test]
    fn a_read_that_broke_is_failed_and_says_why() {
        let (state, error) = settle(Err(Error::Invalid("not a zip".into())));
        assert_eq!(state, "failed");
        assert!(error.contains("not a zip"), "{error}");
    }

    #[test]
    fn a_read_refused_for_a_paused_source_is_failed_not_cancelled() {
        let (state, _) = settle(Err(Error::Invalid("paused".into())));
        assert_eq!(state, "failed");
    }

    #[test]
    fn what_has_never_run_is_idle_and_cannot_be_cancelled() {
        // Nothing in this binary starts an import, so the status is as the
        // process began.
        assert_eq!(status().state, "idle");
        assert!(!cancel());
        assert!(!CANCEL.load(Ordering::SeqCst), "cancelling nothing must not leave a flag set");
    }
}
