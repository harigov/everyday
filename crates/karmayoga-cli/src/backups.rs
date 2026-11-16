//! `karmayoga backups` and `karmayoga restore`: the backups Settings → Backup
//! makes, from a terminal. The work is `karmayoga_service::backup`'s; this
//! decides where, asks for a password when one is needed, and says what
//! happened.
//!
//! This binary is otherwise synchronous, so these build a runtime of their
//! own, as `serve` does, rather than `main` becoming async for everybody.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use karmayoga_core::backup::Destination;
use karmayoga_core::{Error, Result, Vault};
use karmayoga_service::backup::{
    self, BackupError, LocationOptions, Plan, Progress, RestoreOptions, target::Target,
};

use crate::args::{BackupsCmd, LocationArgs};
use crate::format::human_bytes;
use crate::verbs::prompt_password;

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Invalid(format!("could not start: {e}")))
}

fn options(location: &LocationArgs) -> LocationOptions {
    LocationOptions {
        region: location.region.clone(),
        endpoint: location.endpoint.clone(),
        profile: location.profile.clone(),
        credentials_file: location.credentials_file.clone(),
    }
}

fn failed(e: BackupError) -> Error {
    match e {
        BackupError::Vault(e) => e,
        other => Error::Invalid(other.to_string()),
    }
}

/// The destination named on the command line, or the one Settings chose,
/// with the access key the vault keeps for it.
fn destination(
    vault: &Vault,
    from: Option<&str>,
    location: &LocationArgs,
) -> Result<(Destination, Option<karmayoga_core::backup::S3Keys>)> {
    match from {
        Some(raw) => {
            let destination =
                backup::parse_location(raw, &options(location)).map_err(Error::Invalid)?;
            Ok((destination, None))
        }
        None => {
            let destination = vault.backup_settings()?.destination.ok_or_else(|| {
                Error::Invalid(
                    "no destination is set: choose one in Settings \u{2192} Backup, or name one \
                     with --to"
                        .into(),
                )
            })?;
            Ok((destination, vault.backup_s3_keys()?))
        }
    }
}

pub(crate) fn backups(vault: Vault, cmd: BackupsCmd) -> Result<()> {
    match cmd {
        BackupsCmd::Status => status(&vault),
        BackupsCmd::Run { to, keep, no_config, location } => {
            run(Arc::new(vault), to.as_deref(), keep, no_config, &location)
        }
        BackupsCmd::List { from, location } => {
            let (destination, keys) = destination(&vault, from.as_deref(), &location)?;
            let snapshots = runtime()?
                .block_on(backup::list_snapshots(&destination, keys))
                .map_err(|e| Error::Invalid(e.message))?;
            if snapshots.is_empty() {
                println!("No backups at {} yet.", destination.label());
            }
            for s in snapshots {
                let local = s.created.to_zoned(jiff::tz::TimeZone::system());
                println!("{}  {}", s.name, local.strftime("%a %d %b %Y, %H:%M"));
            }
            Ok(())
        }
        BackupsCmd::Verify { from, read, location } => {
            let (destination, keys) = destination(&vault, from.as_deref(), &location)?;
            let target = Target::open(&destination, keys).map_err(|e| Error::Invalid(e.message))?;
            let backup_keys = vault.backup_keys()?;
            let report =
                runtime()?.block_on(backup::verify(&target, &backup_keys, read)).map_err(failed)?;
            println!(
                "{} cop{} at {}; {} piece(s) named, {} missing{}",
                report.snapshots.len(),
                if report.snapshots.len() == 1 { "y" } else { "ies" },
                destination.label(),
                report.chunks_referenced,
                report.missing.len(),
                if read {
                    format!(
                        ", {} read back and {} damaged",
                        report.chunks_read,
                        report.damaged.len()
                    )
                } else {
                    String::new()
                }
            );
            for (name, why) in &report.unreadable {
                eprintln!("  {name}: {why}");
            }
            if report.sound() {
                Ok(())
            } else {
                Err(Error::Invalid("the backups are not whole".into()))
            }
        }
    }
}

fn status(vault: &Vault) -> Result<()> {
    let settings = vault.backup_settings()?;
    let Some(destination) = &settings.destination else {
        println!("Backups are not set up. Choose where they go in Settings \u{2192} Backup.");
        return Ok(());
    };
    let status = vault.backup_status()?.for_destination(destination);
    println!("Destination: {}", destination.label());
    println!(
        "Schedule:    {}, every {} hour(s), keeping {} cop{}{}",
        if settings.enabled { "on" } else { "off" },
        settings.every_hours,
        settings.keep,
        if settings.keep == 1 { "y" } else { "ies" },
        if settings.include_config { ", with this computer's settings" } else { "" },
    );
    match (&status.last_success, &status.last_report) {
        (Some(at), Some(report)) => println!(
            "Last backup: {} ({}, {} sent)",
            at.to_zoned(jiff::tz::TimeZone::system()).strftime("%a %d %b %Y, %H:%M"),
            human_bytes(report.bytes),
            human_bytes(report.uploaded_bytes)
        ),
        _ => println!("Last backup: none yet"),
    }
    if let Some(error) = &status.last_error {
        println!("Last try:    failed: {error}");
    }
    if let Some(due) = backup::next_due(&settings, &status) {
        println!(
            "Next:        {}",
            due.max(jiff::Timestamp::now())
                .to_zoned(jiff::tz::TimeZone::system())
                .strftime("%a %d %b %Y, %H:%M")
        );
    }
    Ok(())
}

fn run(
    vault: Arc<Vault>,
    to: Option<&str>,
    keep: Option<u32>,
    no_config: bool,
    location: &LocationArgs,
) -> Result<()> {
    if !vault.supports_backup() {
        return Err(Error::Invalid(
            "this vault's database is on a server, which is backed up by whoever runs it".into(),
        ));
    }
    let settings = vault.backup_settings()?;
    let (destination, s3_keys) = destination(&vault, to, location)?;
    let saved = to.is_none();
    let keys = vault.backup_keys()?;
    let plan = Plan {
        destination: destination.clone(),
        keep: keep.unwrap_or(settings.keep).clamp(1, karmayoga_core::backup::MAX_KEEP),
        config_dir: (!no_config && settings.include_config).then(karmayoga_vault::config_dir),
        files: vault.supports_files(),
        // Somebody at a terminal is looking at the destination they asked
        // for, and may have emptied it on purpose; only the schedule, which
        // nobody watches, takes an empty place that held backups for one
        // that is not plugged in. See `backup::repo::prepare`.
        expect_existing: false,
        s3_keys,
        cache_dir: Some(backup::task::cache_dir(&keys.repository_id())),
        host: backup::host_name(),
    };
    drop(keys);
    let Some(claim) = backup::claim(true) else {
        return Err(Error::Invalid("a backup is already running".into()));
    };
    let progress = claim.progress();
    let attempt = jiff::Timestamp::now();
    println!("Backing up to {}…", destination.label());
    let result = runtime()?.block_on(async {
        let watching = tokio::spawn(report_progress(progress.clone()));
        let result = backup::run(vault.clone(), plan, progress).await;
        watching.abort();
        result
    });
    drop(claim);
    // Recorded for the destination Settings chose, so the app and `backups
    // status` see it; one named here is a one-off.
    if saved && vault.is_writable() {
        backup::task::save_outcome(&vault, &destination, attempt, &result, false);
    }
    let report = result.map_err(failed)?;
    println!(
        "Made {}: {} file(s), {}; sent {} in {} new piece(s){}, in {:.0}s.",
        report.snapshot,
        report.files,
        human_bytes(report.bytes),
        human_bytes(report.uploaded_bytes),
        report.uploaded_chunks,
        if report.removed_snapshots > 0 {
            format!("; let go of {} older cop(ies)", report.removed_snapshots)
        } else {
            String::new()
        },
        report.seconds
    );
    for path in &report.skipped {
        eprintln!("  went away before it was read: {path}");
    }
    if report.files_folder_unavailable {
        eprintln!(
            "  The files folder was not available, so this copy carries the last copy's files \
             as they were."
        );
    }
    Ok(())
}

/// A line every few seconds while a backup runs, on stderr, so a long first
/// backup is not a silent terminal.
async fn report_progress(progress: Arc<Progress>) {
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    tick.tick().await;
    loop {
        tick.tick().await;
        let p = progress.snapshot();
        eprintln!(
            "  {:?}: {} of {} read, {} sent",
            p.phase,
            human_bytes(p.bytes_done),
            human_bytes(p.bytes_total),
            human_bytes(p.uploaded_bytes)
        );
    }
}

/// `karmayoga restore`: no vault needed, and none opened.
#[allow(clippy::too_many_arguments)]
pub(crate) fn restore(
    from: &str,
    into: &Path,
    snapshot: Option<String>,
    config: bool,
    files_to: Option<&Path>,
    allow_unencrypted: bool,
    location: &LocationArgs,
    password: Option<String>,
) -> Result<()> {
    let destination = backup::parse_location(from, &options(location)).map_err(Error::Invalid)?;
    let target = Target::open(&destination, None).map_err(|e| Error::Invalid(e.message))?;
    let rt = runtime()?;
    let header = rt.block_on(backup::restore::read_header(&target)).map_err(failed)?;
    // Before any prompt, and before anything is read: a header that says
    // "not encrypted" asks for no password, which is exactly what somebody
    // who can write to the destination would want it to say. See
    // `BackupKeys::from_header`.
    if !header.is_encrypted() && !allow_unencrypted {
        return Err(Error::Invalid(karmayoga_core::backup::UNENCRYPTED_BACKUP_REFUSAL.into()));
    }
    let password = if header.is_encrypted() {
        Some(match password {
            Some(p) => p,
            None => prompt_password(&format!("Password for \"{}\": ", header.name))?,
        })
    } else {
        None
    };
    let config_into = config.then(karmayoga_vault::config_dir);
    let progress = Progress::new(true);
    println!("Restoring from {} into {}…", destination.label(), into.display());
    let report = rt
        .block_on(async {
            let watching = tokio::spawn(report_progress(progress.clone()));
            let result = backup::restore(
                &target,
                RestoreOptions {
                    snapshot,
                    into: into.to_path_buf(),
                    config_into,
                    files_into: files_to.map(Path::to_path_buf),
                    password,
                    allow_unencrypted,
                },
                Some(&progress),
            )
            .await;
            watching.abort();
            result
        })
        .map_err(failed)?;
    if !report.passed_over.is_empty() {
        eprintln!(
            "Passed over {} newer backup file(s) that do not open with this vault's key, and \
             restored the newest that does: {}",
            report.passed_over.len(),
            report.passed_over.join(", ")
        );
    }
    println!(
        "Restored {}: {} file(s), {}. Open {} in Karma Yoga with the same password.",
        report.snapshot,
        report.files,
        human_bytes(report.bytes),
        into.display()
    );
    if !report.config_restored.is_empty() {
        println!("Put back {} settings file(s).", report.config_restored.len());
    }
    if !report.config_kept.is_empty() {
        println!("Left alone, because one was already there: {}", report.config_kept.join(", "));
    }
    if report.config_left_out > 0 {
        println!(
            "{} settings file(s) were not put back; add --config to bring them too.",
            report.config_left_out
        );
    }
    if let Some(folder) = &report.files_folder {
        println!(
            "Put back {} file(s), {}, into {folder}{}.",
            report.files_folder_restored,
            human_bytes(report.files_folder_bytes),
            if report.files_folder_kept > 0 {
                format!("; left {} alone, because one was already there", report.files_folder_kept)
            } else {
                String::new()
            }
        );
        if files_to.is_some() {
            println!(
                "The vault still names its old files folder: choose {folder} in Settings \u{2192} \
                 Files when you open it."
            );
        }
    }
    if report.files_folder_left_out > 0 {
        let was = match &report.files_folder_recorded {
            Some(folder) => format!("the copy says that folder was {folder}, which is not"),
            None => "the copy does not say where that folder was, and it is not".into(),
        };
        println!(
            "{} file(s) from the files folder were not put back: {was} this computer's own \
             Karma Yoga folder, and a restore does not write wherever a backup says. Name the \
             folder you want them in with --files-to.",
            report.files_folder_left_out
        );
    }
    Ok(())
}
