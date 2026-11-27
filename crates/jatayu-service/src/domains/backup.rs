//! Settings → Backup: where copies of the vault go, how often, how many are
//! kept, and a button that makes one now. The copying is
//! [`crate::backup`]'s; these are its doors.
//!
//! Scope `All`, as an export is: a backup is the whole vault, and where it
//! goes is a credential's worth of trust -- a client that could point
//! backups at a bucket of its own could take a copy of everything. A paired
//! desktop has `All`; nothing scoped narrower reaches these.

use std::sync::Arc;

use jatayu_core::Vault;
use jatayu_core::backup::{
    BackupSettings, BackupStatus, Destination, S3Credentials, S3Keys, SnapshotInfo,
};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::backup::{self, BackupProgress, DestinationCheck};
use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};

use super::Nothing;

/// What every command here answers with: the settings, and everything the
/// pane says about them that the record itself does not. Mirrors the TS
/// `BackupView`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupView {
    pub settings: BackupSettings,
    /// The last run's outcome -- about the destination chosen now. A status
    /// about somewhere else reads as nothing having happened yet.
    pub status: BackupStatus,
    /// A backup in progress, made by this process.
    pub running: Option<BackupProgress>,
    /// When the next backup is due on its own; `None` when they are off.
    pub next_due: Option<Timestamp>,
    /// Whether an S3 access key has been pasted in and is stored.
    pub has_keys: bool,
    /// Whether this vault can be backed up from here: false for a vault
    /// whose database is on a server.
    pub supported: bool,
    /// Whether the vault is encrypted. An unencrypted vault's backups are
    /// sealed under a key derived from nothing secret, and the pane says so.
    pub encrypted: bool,
    /// The folder "this computer's settings" means, to name in the pane.
    pub config_dir: String,
}

pub(crate) fn view(vault: &Vault) -> jatayu_core::Result<BackupView> {
    let settings = vault.backup_settings()?;
    let status = match &settings.destination {
        Some(d) => vault.backup_status()?.for_destination(d),
        None => BackupStatus::default(),
    };
    let next_due = backup::next_due(&settings, &status);
    Ok(BackupView {
        has_keys: vault.has_backup_s3_keys().unwrap_or(false),
        supported: vault.supports_backup(),
        encrypted: vault.is_encrypted(),
        config_dir: jatayu_vault::config_dir().display().to_string(),
        running: backup::progress(),
        next_due,
        status,
        settings,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveBackupSettings {
    pub settings: BackupSettings,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetBackupKeys {
    pub access_key_id: String,
    pub secret_access_key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckDestination {
    pub destination: Destination,
}

async fn backup_settings(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<BackupView> {
    svc.on_vault(view).await
}

async fn save_backup_settings(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SaveBackupSettings,
) -> CommandResult<BackupView> {
    let view = svc
        .on_vault(move |vault| {
            vault.save_backup_settings(&args.settings)?;
            view(vault)
        })
        .await?;
    // Turning backups on, or changing how often, can make one due now.
    backup::task::wake();
    Ok(view)
}

async fn set_backup_keys(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SetBackupKeys,
) -> CommandResult<BackupView> {
    svc.on_vault(move |vault| {
        vault.set_backup_s3_keys(Some(&S3Keys {
            access_key_id: args.access_key_id,
            secret_access_key: args.secret_access_key,
        }))?;
        view(vault)
    })
    .await
}

async fn clear_backup_keys(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<BackupView> {
    svc.on_vault(move |vault| {
        vault.set_backup_s3_keys(None)?;
        view(vault)
    })
    .await
}

/// Start a backup now, whether or not they are switched on, and answer at
/// once: the pane follows it through `backup_settings`' `running`.
async fn run_backup(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<BackupView> {
    let vault = svc.require_unlocked()?;
    let refusal = {
        let vault = vault.clone();
        blocking(move || Ok(backup::task::refusal(&vault))).await?
    };
    if let Some(why) = refusal {
        return Err(CommandError::new(codes::INVALID, why));
    }
    backup::task::start(&svc, &vault, backup::Trigger::Now)
        .map_err(|why| CommandError::new(codes::BUSY, why))?;
    blocking(move || Ok(view(&vault)?)).await
}

/// Stop the backup in progress. What it had sent stays where it is and is
/// used by the next one; no copy is recorded for it, and the schedule waits
/// a whole interval before trying again rather than starting it over at its
/// next look.
async fn cancel_backup(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<BackupView> {
    backup::stop();
    svc.on_vault(view).await
}

/// The copies kept at the chosen destination, newest first. Asks the
/// destination, so it can fail the way a backup would.
async fn backup_snapshots(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<SnapshotInfo>> {
    let vault = svc.require_unlocked()?;
    let (destination, keys) = blocking(move || {
        let settings = vault.backup_settings()?;
        let keys = vault.backup_s3_keys()?;
        Ok((settings.destination, keys))
    })
    .await?;
    let Some(destination) = destination else { return Ok(Vec::new()) };
    backup::list_snapshots(&destination, keys)
        .await
        .map_err(|e| CommandError::new(target_code(&e), e.message))
}

/// Try a destination before saving it: write a small object there, read it
/// back and delete it. Answers with what happened rather than failing, so the
/// pane can say it beside the fields.
async fn check_backup_destination(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: CheckDestination,
) -> CommandResult<DestinationCheck> {
    let vault = svc.require_unlocked()?;
    let destination = args.destination.normalized();
    if let Err(e) = destination.validate() {
        return Ok(DestinationCheck { ok: false, message: e.to_string() });
    }
    let stored = matches!(&destination, Destination::S3 { credentials: S3Credentials::Stored, .. });
    let (s3_keys, keys) = blocking(move || {
        let s3_keys = if stored { vault.backup_s3_keys()? } else { None };
        Ok((s3_keys, vault.backup_keys()?))
    })
    .await?;
    Ok(backup::check_destination(&destination, s3_keys, Some(&keys)).await)
}

fn target_code(e: &crate::backup::target::TargetError) -> &'static str {
    use crate::backup::target::TargetErrorKind::*;
    match e.kind {
        Unreachable => codes::UNREACHABLE,
        Denied | Credentials => codes::FORBIDDEN,
        Missing => codes::NOT_FOUND,
        Other => codes::INVALID,
    }
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "backup_settings", scope: All, effect: Read,
        args: Nothing, returns: "BackupView", signature: &[],
        run: backup_settings,
    },
    command! {
        name: "save_backup_settings", scope: All, effect: Write,
        change: Settings / Updated,
        args: SaveBackupSettings, returns: "BackupView",
        signature: &[("settings", "BackupSettings", true)],
        run: save_backup_settings,
    },
    command! {
        name: "set_backup_keys", scope: All, effect: Write,
        change: Settings / Updated,
        args: SetBackupKeys, returns: "BackupView",
        signature: &[("accessKeyId", "string", true), ("secretAccessKey", "string", true)],
        run: set_backup_keys,
    },
    command! {
        name: "clear_backup_keys", scope: All, effect: Write,
        change: Settings / Updated,
        args: Nothing, returns: "BackupView", signature: &[],
        run: clear_backup_keys,
    },
    command! {
        name: "run_backup", scope: All, effect: Write,
        args: Nothing, returns: "BackupView", signature: &[],
        run: run_backup,
    },
    command! {
        name: "cancel_backup", scope: All, effect: Write,
        args: Nothing, returns: "BackupView", signature: &[],
        run: cancel_backup,
    },
    command! {
        name: "backup_snapshots", scope: All, effect: Read,
        args: Nothing, returns: "SnapshotInfo[]", signature: &[],
        run: backup_snapshots,
    },
    command! {
        name: "check_backup_destination", scope: All, effect: Read,
        args: CheckDestination, returns: "DestinationCheck",
        signature: &[("destination", "Destination", true)],
        run: check_backup_destination,
    },
];
