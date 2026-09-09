//! The vault itself: its lock, its password, its housekeeping.
//!
//! What is deliberately not here is opening a vault, creating one, and
//! choosing which one this session is about. Those decide what the service
//! *is* and belong to whatever owns the process -- see [`crate::service`].

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult};
use crate::service::{Service, blocking};
use everyday_core::VaultStatus;
use everyday_core::profile::Profile;
use everyday_core::store::StoreStats;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct Nothing {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Unlock {
    pub password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangePassword {
    pub current: String,
    pub next: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoLock {
    pub seconds: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveProfile {
    pub profile: Profile,
}

/// Who this vault belongs to.
///
/// Under `Journals` rather than `Agent`, and read-write by hand rather than
/// by any tool. The assistant reads it into every prompt, but it is not the
/// assistant's: a birthday is a calendar's business and a location is the
/// weather's, when either of those exists. See `everyday_core::profile` for
/// why a fact that changes is a memory instead.
async fn profile(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<Profile> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.profile()?)).await
}

async fn save_profile(svc: Arc<Service>, _ctx: Ctx, args: SaveProfile) -> CommandResult<Profile> {
    let vault = svc.require()?;
    blocking(move || {
        vault.save_profile(&args.profile)?;
        // Read back rather than echoing what was sent: the vault stamps
        // `updated_at`, and a client that drew what it sent would show a
        // profile that had not been saved yet as though it had.
        Ok(vault.profile()?)
    })
    .await
}

async fn status(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<VaultStatus> {
    Ok(svc.require()?.status())
}

async fn unlock(svc: Arc<Service>, _ctx: Ctx, args: Unlock) -> CommandResult<VaultStatus> {
    let vault = svc.require()?;
    let v = vault.clone();
    blocking(move || v.unlock(Some(&args.password)).map_err(CommandError::from)).await?;
    svc.events().lock_state(false);
    Ok(vault.status())
}

/// Check a password without unlocking anything.
///
/// This is the other half of the split between a screen and a key. A client
/// that locked its own screen has not closed the vault -- the other windows
/// looking at it, and the assistant's scheduler, carried on -- so what it
/// needs on the way back in is proof of the person, not a second unlock.
///
/// It costs the same Argon2 derivation an unlock costs, deliberately, and
/// the server runs it behind the same rate limit for the same reason.
async fn verify_password(svc: Arc<Service>, _ctx: Ctx, args: Unlock) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || vault.verify_password(Some(&args.password)).map_err(CommandError::from)).await
}

async fn lock(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<VaultStatus> {
    let vault = svc.require()?;
    let status = blocking(move || {
        vault.lock();
        Ok(vault.status())
    })
    .await?;
    svc.events().lock_state(true);
    Ok(status)
}

async fn change_password(svc: Arc<Service>, _ctx: Ctx, args: ChangePassword) -> CommandResult<()> {
    everyday_vault::validate_password(&args.next)?;
    let vault = svc.require()?;
    blocking(move || {
        vault.change_password(Some(&args.current), Some(&args.next)).map_err(CommandError::from)
    })
    .await
}

async fn set_auto_lock(svc: Arc<Service>, _ctx: Ctx, args: AutoLock) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.set_auto_lock(args.seconds)?)).await
}

async fn set_forget_key(svc: Arc<Service>, _ctx: Ctx, args: AutoLock) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.set_forget_key(args.seconds)?)).await
}

/// Defer the moment the key is dropped. Called on real interaction, so it
/// must be cheap.
///
/// Counted across every client: a vault being used from a phone is a vault
/// being used, and locking it out from under somebody because the machine
/// holding it has an idle keyboard would be a strange thing to do. Not
/// counted for the assistant, which is why [`crate::command::Command::invoke`]
/// calls this rather than the vault's own read and write paths.
async fn touch(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<()> {
    if let Some(v) = svc.get() {
        v.touch();
    }
    Ok(())
}

/// True if the vault just dropped its key for idleness.
///
/// Named for what it used to do. What it polls now is the *key* timeout, not
/// the screen one: a client's screen is its own business and is timed on its
/// own clock, and the vault behind it is shared. The name stays because
/// renaming it is a wire change that buys nothing.
///
/// A local window polls this. A remote client does not: it is told by the
/// `lockState` event instead, because a poll every few seconds is free over an
/// IPC bridge and is a round trip over a network. The assistant's scheduler
/// polls it too, so a machine serving a vault with no window attached still
/// honours the timeout.
async fn poll_auto_lock(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<bool> {
    let locked = svc.get().is_some_and(|v| v.forget_key_if_idle());
    if locked {
        svc.events().lock_state(true);
    }
    Ok(locked)
}

async fn vault_stats(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<StoreStats> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.stats()?)).await
}

async fn collect_garbage(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<u64> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.collect_garbage(everyday_core::store::GC_GRACE)?)).await
}

/// Push pending writes to durable storage.
///
/// Its caller is whoever is about to stop: the shell's close handshake, or a
/// server being shut down. Best-effort by nature -- the data is committed
/// either way, and a checkpoint that fails is not a reason to refuse to quit.
async fn flush(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || {
        let _ = vault.with_store(|s| s.flush());
        Ok(())
    })
    .await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "status", scope: Journals, effect: Read,
        args: Nothing, returns: "VaultStatus", signature: &[],
        run: status,
    },
    command! {
        name: "unlock", scope: Journals, effect: Write,
        args: Unlock, returns: "VaultStatus",
        signature: &[("password", "string", true)],
        run: unlock,
    },
    command! {
        name: "verify_password", scope: Journals, effect: Write,
        args: Unlock, returns: "void",
        signature: &[("password", "string", true)],
        run: verify_password,
    },
    command! {
        name: "lock", scope: Journals, effect: Write,
        args: Nothing, returns: "VaultStatus", signature: &[],
        run: lock,
    },
    command! {
        name: "change_password", scope: Journals, effect: Write,
        change: Settings / Updated,
        args: ChangePassword, returns: "void",
        signature: &[("current", "string", true), ("next", "string", true)],
        run: change_password,
    },
    command! {
        name: "set_auto_lock", scope: Journals, effect: Write,
        change: Settings / Updated,
        args: AutoLock, returns: "void",
        signature: &[("seconds", "number", true)],
        run: set_auto_lock,
    },
    command! {
        name: "set_forget_key", scope: Journals, effect: Write,
        change: Settings / Updated,
        args: AutoLock, returns: "void",
        signature: &[("seconds", "number", true)],
        run: set_forget_key,
    },
    command! {
        name: "touch", scope: Journals, effect: Write,
        args: Nothing, returns: "void", signature: &[],
        run: touch,
    },
    command! {
        name: "poll_auto_lock", scope: Journals, effect: Write,
        args: Nothing, returns: "boolean", signature: &[],
        run: poll_auto_lock,
    },
    command! {
        name: "profile", scope: Journals, effect: Read,
        args: Nothing, returns: "Profile", signature: &[],
        run: profile,
    },
    command! {
        name: "save_profile", scope: Journals, effect: Write,
        change: Settings / Updated,
        args: SaveProfile, returns: "Profile",
        signature: &[("profile", "Profile", true)],
        run: save_profile,
    },
    command! {
        name: "vault_stats", scope: Journals, effect: Read,
        args: Nothing, returns: "StoreStats", signature: &[],
        run: vault_stats,
    },
    command! {
        name: "collect_garbage", scope: Journals, effect: Destructive,
        args: Nothing, returns: "number", signature: &[],
        run: collect_garbage,
    },
    command! {
        name: "flush", scope: Journals, effect: Write,
        args: Nothing, returns: "void", signature: &[],
        run: flush,
    },
];
