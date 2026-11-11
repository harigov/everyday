//! Keeping subscribed calendars current, whether or not a window is open.
//!
//! One supervised task, the arrangement the Feed's refresher has (see
//! [`crate::news::ensure_task`]): started at unlock, stopped by the lock, so a
//! locked vault is never fetched into.
//!
//! This used to be a timer in the calendar screen, which ran only once that
//! screen had been opened in a session. That stopped being enough when things
//! that run with no window at all began reading the events it brings in: the
//! meeting notices in [`crate::scheduler::tick`] and the tray's next meeting would
//! otherwise work from whatever the last visit to the calendar left behind.
//!
//! The pass itself, and its rules about failure, are
//! `domains::calendars::sync_due`'s: this only decides when. Calling it directly
//! rather than through the command means the pass does not count as somebody
//! being at the keyboard, and so does not hold off the auto-lock. A window that
//! reloads for what a pass announces does count, which is the other reason a pass
//! announces only what changed: one that says something every few minutes would
//! keep the key in memory for as long as a window was open.

use crate::domains::calendars::sync_due;
use crate::error::codes;
use crate::events::{Change, Kind, Op};
use crate::service::{Service, blocking};
use crate::supervisor::{Outcome, TaskResult};
use everyday_core::Vault;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// The supervisor's key for this task.
pub const TASK_KEY: &str = "calendars";

/// How often to look for a calendar that has come due, or that has a reminder
/// coming round in the next few minutes and has not been checked lately. A
/// minute, the reminder tick's own, so a meeting moved on somebody's phone is
/// fetched before its old alert goes off rather than after. Looking costs a
/// read of the calendar list and the next two days' events; a calendar that
/// failed is tried again only every few minutes (see `Service::feed_retry_due`).
const LOOK_EVERY: Duration = Duration::from_secs(60);

/// How far ahead to look for a reminder whose calendar should be checked
/// first. More than one look, so a reminder never falls between two; few
/// enough minutes that a meeting moved just before is still caught.
const CHECK_AHEAD_SECS: i64 = 3 * 60;

/// Start the refresher if it is not already running. Idempotent -- see
/// [`crate::supervisor::Supervisor::ensure`] -- so it is called from every
/// unlock and costs nothing when the task is already there.
pub fn ensure_task(svc: &Arc<Service>, vault: &Arc<Vault>) {
    if !vault.supports_calendars() {
        return;
    }
    // Looks the vault up each time it starts rather than keeping one: the
    // supervisor keeps this for as long as the process runs, and a vault held
    // here would keep its write lock through a close, so opening it again would
    // come up read-only, and a restart after switching would be refreshing the
    // wrong vault.
    let svc_for_task = svc.clone();
    svc.supervisor().ensure(TASK_KEY, move |stop| {
        let svc = svc_for_task.clone();
        Box::pin(async move {
            match svc.get() {
                Some(vault) if vault.supports_calendars() => run(svc, vault, stop).await,
                _ => Ok(Outcome::Done),
            }
        })
    });
}

/// The supervised loop: refresh what is due, say so if that changed anything,
/// and look again in [`LOOK_EVERY`]. Returns when `stop` turns `true`.
pub async fn run(
    svc: Arc<Service>,
    vault: Arc<Vault>,
    mut stop: watch::Receiver<bool>,
) -> TaskResult {
    loop {
        if *stop.borrow() {
            return Ok(Outcome::Done);
        }
        // Raced with the signal rather than left to finish: a calendar server
        // that has stopped answering must not hold up a lock. Everything a sync
        // stores goes through the blocking pool, which sees its write through
        // whether or not anyone is still waiting for it.
        let soon = {
            let (vault, now) = (vault.clone(), svc.now());
            blocking(move || {
                Ok(crate::arrivals::calendars_alerting_soon(&vault, now, CHECK_AHEAD_SECS)
                    .unwrap_or_default())
            })
            .await
            .unwrap_or_default()
        };
        tokio::select! {
            pass = sync_due(&svc, &vault, false, &soon) => match pass {
                Ok(pass) => {
                    if pass.changed {
                        svc.events().changed(Change::new(Kind::Event, Op::Updated));
                    }
                }
                // Locked between one turn and the next: nothing to read and
                // nothing to write. The supervisor stops this task on the lock
                // in any case; until it does, wait.
                Err(e) if e.code == codes::LOCKED => {}
                Err(e) => return Err(e.into()),
            },
            changed = stop.changed() => {
                if changed.is_err() {
                    return Ok(Outcome::Done);
                }
                continue;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(LOOK_EVERY) => {}
            changed = stop.changed() => {
                // The sender is gone: nobody is left to ask this to stop, or
                // to want what it does.
                if changed.is_err() {
                    return Ok(Outcome::Done);
                }
            }
        }
    }
}
