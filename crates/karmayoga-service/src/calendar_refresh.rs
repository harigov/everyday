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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use crate::domains::calendars::{SYNC_AT_ONCE, due_calendars, settle, sync_chosen};
use crate::error::codes;
use crate::events::{Change, Kind, Op};
use crate::service::{Service, blocking};
use crate::supervisor::{Outcome, TaskResult};
use karmayoga_core::Vault;
use karmayoga_core::id::CalendarId;
use tokio::sync::watch;
use tokio::task::JoinSet;

/// The supervisor's key for this task.
pub const TASK_KEY: &str = "calendars";

/// How often to look for a calendar that has come due, or that has a reminder
/// coming round in the next few minutes and has not been checked lately. A
/// minute, the reminder tick's own, so a meeting moved on somebody's phone is
/// fetched before its old alert goes off rather than after. Looking costs a
/// read of the calendar list; what is coming round is what the reminder tick
/// already read (see `Service::calendars_alerting_soon`), and a calendar that
/// failed is tried again only every few minutes (see `Service::feed_retry_due`).
const LOOK_EVERY: Duration = Duration::from_secs(60);

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

/// The supervised loop: every [`LOOK_EVERY`], start syncing what is due, a few
/// calendars at a time, each its own task; say what each came to as it lands;
/// tell an open window once nothing is left syncing. Returns when `stop` turns
/// `true`.
///
/// Each calendar is its own task so that one server that has stopped answering
/// -- held until its sync's ceiling, minutes -- holds up only itself: the next
/// look still comes a minute later, and still checks the calendar of a
/// reminder about to go out. A calendar still syncing is not started again.
///
/// Stopping drops whatever is still syncing, rather than waiting: a calendar
/// server that has stopped answering must not hold up a lock. Everything a
/// sync stores goes through the blocking pool, which sees its write through
/// whether or not anyone is still waiting for it.
pub async fn run(
    svc: Arc<Service>,
    vault: Arc<Vault>,
    mut stop: watch::Receiver<bool>,
) -> TaskResult {
    let mut syncing = JoinSet::new();
    let mut which: HashMap<tokio::task::Id, CalendarId> = HashMap::new();
    let mut changed = false;
    let mut look = tokio::time::Instant::now();
    loop {
        if *stop.borrow() {
            return Ok(Outcome::Done);
        }
        tokio::select! {
            stopping = stop.changed() => {
                // The sender is gone: nobody is left to ask this to stop, or
                // to want what it does.
                if stopping.is_err() {
                    return Ok(Outcome::Done);
                }
            }
            Some(landed) = syncing.join_next_with_id(), if !syncing.is_empty() => {
                let id = match &landed {
                    Ok((task, _)) => *task,
                    Err(e) => e.id(),
                };
                which.remove(&id);
                if let Ok((_, (calendar, result))) = landed {
                    let (svc, vault) = (svc.clone(), vault.clone());
                    changed |= blocking(move || Ok(settle(&svc, &vault, &calendar, &result)))
                        .await
                        .unwrap_or(false);
                }
                // A pass announces a change once, when it is over, and only
                // when an event came or went or a failure started or stopped:
                // a window reloading for it counts as somebody being there,
                // and every few minutes of that would keep the key in memory
                // for as long as a window was open.
                if changed && syncing.is_empty() {
                    changed = false;
                    svc.events().changed(Change::new(Kind::Event, Op::Updated));
                }
            }
            () = tokio::time::sleep_until(look) => {
                look = tokio::time::Instant::now() + LOOK_EVERY;
                // A sync held up past a whole look has kept its news long enough.
                if changed {
                    changed = false;
                    svc.events().changed(Change::new(Kind::Event, Op::Updated));
                }
                let soon = svc.calendars_alerting_soon(svc.now());
                let skip: HashSet<CalendarId> = which.values().copied().collect();
                let due = {
                    let (svc, vault) = (svc.clone(), vault.clone());
                    blocking(move || Ok(due_calendars(&svc, &vault, false, &soon, &skip)?)).await
                };
                let due = match due {
                    Ok(due) => due,
                    // Locked between one look and the next: nothing to read
                    // and nothing to write. The supervisor stops this task on
                    // the lock in any case; until it does, wait.
                    Err(e) if e.code == codes::LOCKED => Vec::new(),
                    Err(e) => return Err(e.into()),
                };
                for calendar in due.into_iter().take(SYNC_AT_ONCE.saturating_sub(which.len())) {
                    let id = calendar.id;
                    let task = syncing.spawn(sync_chosen(svc.clone(), vault.clone(), calendar));
                    which.insert(task.id(), id);
                }
            }
        }
    }
}
