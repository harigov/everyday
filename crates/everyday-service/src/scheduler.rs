//! The loop that runs the assistant's routines.
//!
//! The first background task this crate has ever owned, and the reason is
//! worth stating because the crate has been arguing the other way until now.
//! Every periodic thing in the application has been polled by a *window*: the
//! calendar refreshes on a five-minute `setInterval`, the lock timer on a
//! five-second one. The comment on `sync_due_calendars` gives the argument --
//! *"so that a locked vault is never fetched into and a window nobody is
//! looking at is never the reason a laptop wakes its radio"* -- and it was
//! right for a feature whose only consumer was the window in front of you.
//!
//! It cannot be right for this. A routine set for seven in the morning has to
//! run at seven whether or not anybody has opened a window, which means the
//! thing that decides it is due has to live where the vault lives. So: one
//! task, on the runtime the process already has, ticking once a minute.
//!
//! # Rules it keeps
//!
//! **One at a time.** Runs are serial. Two model calls at once would double
//! the bill and race each other for the vault's single writer, and nothing
//! about a morning brief is urgent enough to want either.
//!
//! **Nothing while locked.** A locked vault has no key, so there is nothing to
//! read and no credential to spend. The tick does nothing at all -- not even
//! record a skip, because a vault that is locked at seven and unlocked at nine
//! should produce one honest "missed" row when somebody is there to be told,
//! rather than a row written by a process nobody asked.
//!
//! **A missed slot is recorded.** Past its grace, a routine's moment becomes a
//! skipped run with the reason in it. The question somebody asks the next
//! morning is "why did I not get my brief", and a log line on a machine under
//! a desk is not an answer.
//!
//! **A hung model ends the run.** The provider is reached through rig's own
//! HTTP client rather than `http.rs`, so it does not share that module's
//! timeouts. The whole turn is wrapped in one here instead.

use crate::agent::{AgentEvent, Turn};
use crate::ctx::{Caller, Ctx, Scope};
use crate::events::{Change, Kind, Notification, Op};
use crate::service::Service;
use everyday_core::routine::{Due, Outcome, Routine, RoutineRun};
use everyday_core::store::routines::RunQuery;
use everyday_core::{Conversation, RoutineRunId, Vault};
use std::sync::Arc;
use std::time::Duration;

/// How often the loop looks at the clock.
///
/// A minute, because a routine's moment is a minute of the day and there is
/// nothing to gain from finer. It is also the granularity the query triggers
/// will want: an event an hour away is still an hour away in thirty seconds.
const TICK: Duration = Duration::from_secs(60);

/// Longest one run may take before it is abandoned.
///
/// Fifteen minutes, the number `RemoteClient` already uses for a streamed
/// turn. Long enough for a real multi-step job on a slow endpoint, and short
/// enough that a wedged provider does not hold the loop until the process
/// ends.
const RUN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Run the scheduler until `stop` says otherwise.
///
/// Must be spawned on the runtime the rest of the process uses. Starting one
/// here would double the blocking pool and, worse, would mean the vault's
/// single-writer rule was being kept by two sets of threads that know nothing
/// about each other -- the rule `everyday_server::start` already states.
pub async fn run(service: Arc<Service>, mut stop: tokio::sync::watch::Receiver<bool>) {
    tracing::info!("the assistant's scheduler is awake");
    loop {
        tokio::select! {
            _ = tokio::time::sleep(TICK) => {}
            _ = stop.changed() => break,
        }
        if *stop.borrow() {
            break;
        }
        tick(&service).await;
    }
    tracing::info!("the assistant's scheduler has stopped");
}

/// One pass. Public so a test can drive it without waiting a minute.
pub async fn tick(service: &Arc<Service>) {
    let Some(vault) = service.get() else { return };

    // A locked vault has no key: nothing to read, nothing to spend. Also the
    // moment to honour the key timeout, which is what lets a machine serving
    // a vault with no window attached forget its key on schedule -- until
    // this existed, nothing polled it.
    if !vault.is_unlocked() {
        return;
    }
    if vault.forget_key_if_idle() {
        service.events().lock_state(true);
        return;
    }
    // A second copy of the application holds the write claim. Reading is
    // fine and writing is not, and every routine writes something.
    if !vault.is_writable() || !vault.supports_routines() {
        return;
    }

    let Ok(routines) = vault.routines() else { return };
    let settings = vault.agent_settings().unwrap_or_default();
    let now = settings.now();

    // Oldest first, so a morning where two routines are both due runs them in
    // the order they last ran rather than the order they were created.
    let mut due: Vec<(Routine, Due)> = routines
        .into_iter()
        .map(|r| (r.is_due(&now), r))
        .filter(|(d, _)| !matches!(d, Due::Later))
        .map(|(d, r)| (r, d))
        .collect();
    due.sort_by_key(|(r, _)| r.last_run_at.unwrap_or(r.created_at));

    for (routine, verdict) in due {
        match verdict {
            Due::Missed { slot } => {
                let late = pretty_minutes(now.timestamp().as_second() - slot.as_second());
                skip(
                    &vault,
                    &routine,
                    Some(slot),
                    format!(
                        "its {} was {late} ago, past the {} minutes it allows",
                        clock_of(slot, &now),
                        routine.grace_minutes
                    ),
                );
                // Stamped even though nothing ran, or the same slot is
                // reported missed on every tick for the rest of the day.
                stamp(&vault, &routine, slot);
                service.events().changed(run_change());
            }
            Due::Now { slot } => {
                execute(service, &vault, &routine, Some(slot)).await;
            }
            Due::Later => {}
        }
    }

    // And whatever somebody asked for by hand. Queued as a `Running` row with
    // no slot rather than executed in the command, so that "run now" is
    // answered immediately and the work still happens one at a time.
    let queued = vault
        .runs(&RunQuery { outcomes: vec![Outcome::Running], ..Default::default() })
        .unwrap_or_default();
    for run in queued {
        // A row left `Running` by a process that died is not work to do. It is
        // swept, because a run that cannot finish should not sit on the app bar
        // looking like one that is about to.
        let Ok(routine) = vault.routine(run.routine_id) else {
            let _ = vault.delete_run(run.id);
            continue;
        };
        if run.slot.is_some() {
            // Ours, mid-flight or abandoned. Left alone: `execute` owns it.
            continue;
        }
        resume(service, &vault, &routine, run).await;
    }
}

/// Everything about one run, from the row to the notification.
async fn execute(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    routine: &Routine,
    slot: Option<jiff::Timestamp>,
) {
    let run = RoutineRun::new(routine, slot);
    if vault.save_run(&run).is_err() {
        return;
    }
    resume(service, vault, routine, run).await;
}

/// Carry a `Running` row through to an outcome.
///
/// Split from [`execute`] so that a run somebody queued by hand and a run the
/// clock started go down exactly the same path -- including the sweep of a row
/// whose process died, which arrives here as a row that is already saved.
async fn resume(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    routine: &Routine,
    mut run: RoutineRun,
) {
    // The credential check is first and is a *skip*, not a failure: an
    // assistant that is switched off has not failed at anything, and the
    // reason is the useful part.
    if let Err(e) = vault.agent_credentials() {
        run.outcome = Outcome::Skipped;
        run.reason = e.to_string();
        run.finished_at = Some(jiff::Timestamp::now());
        run.seen = true;
        let _ = vault.save_run(&run);
        if let Some(slot) = run.slot {
            stamp(vault, routine, slot);
        }
        service.events().changed(run_change());
        return;
    }

    // The transcript. Named after the routine so the rail's thread list reads,
    // and carrying the run so `chats_only` keeps it out of that list.
    let conversation = Conversation::for_run(run.id, routine.name.clone());
    if vault.save_conversation(&conversation).is_ok() {
        run.conversation_id = Some(conversation.id);
        let _ = vault.save_run(&run);
    }

    tracing::info!(routine = %routine.name, "running a routine");
    // What the tray says while this is going on. Cleared below whatever
    // happens, including a timeout.
    service.set_running_routine(Some(routine.name.clone()));
    let turn = Turn {
        vault: vault.clone(),
        pending: service.pending(),
        conversation: conversation.id,
        prompt: routine.instructions.clone(),
        context: None,
        // Nothing is listening: there is no window on the other end of a
        // scheduled run. The events are dropped rather than buffered, and what
        // is kept is the transcript in the vault.
        channel: Arc::new(|_: AgentEvent| {}),
        unattended: Some(run.id),
    };

    match tokio::time::timeout(RUN_TIMEOUT, crate::agent::run_turn(turn)).await {
        Ok(Ok(turned)) => {
            run.steps = turned.steps;
            run.finish(Outcome::Done, turned.text);
        }
        Ok(Err(e)) => run.fail(e.to_string()),
        Err(_) => run.fail(format!(
            "it was still going after {} minutes and was stopped",
            RUN_TIMEOUT.as_secs() / 60
        )),
    }

    service.set_running_routine(None);
    let _ = vault.save_run(&run);
    if let Some(slot) = run.slot {
        stamp(vault, routine, slot);
    }
    announce(service, routine, &run);
    service.events().changed(run_change());
}

/// Say something, once.
///
/// Keyed by the routine, so a routine whose endpoint has been unreachable
/// every morning for a week says so once rather than seven times -- the same
/// courtesy `feed_failed` extends to a calendar that has stopped answering.
/// The title names the routine and never carries what it found: this is the
/// one notification in the application that will routinely be drawn over a
/// lock screen.
fn announce(service: &Arc<Service>, routine: &Routine, run: &RoutineRun) {
    let key = format!("routine:{}", routine.id);
    match run.outcome {
        Outcome::Done => {
            service.routine_recovered(routine.id.to_string());
            if run.summary.trim().is_empty() && run.steps == 0 {
                return;
            }
            service.events().notify(
                Notification::new(crate::events::Level::Info, format!("{} is ready", routine.name))
                    .for_user()
                    .key(key),
            );
        }
        Outcome::Failed => {
            if service.routine_failed(routine.id.to_string()) {
                service.events().notify(
                    Notification::warning(format!("{} could not run", routine.name))
                        .body(run.reason.clone())
                        .for_user()
                        .key(key),
                );
            }
        }
        // A skip is not news. It is a row somebody will see when they look.
        Outcome::Skipped | Outcome::Running => {}
    }
}

fn skip(vault: &Arc<Vault>, routine: &Routine, slot: Option<jiff::Timestamp>, reason: String) {
    tracing::info!(routine = %routine.name, %reason, "skipping a routine");
    let _ = vault.save_run(&RoutineRun::skipped(routine, slot, reason));
}

/// Record that this routine has now dealt with `slot`.
///
/// Stamped for a skip as well as a run, or the same missed moment is reported
/// again on every tick for the rest of the day.
fn stamp(vault: &Arc<Vault>, routine: &Routine, slot: jiff::Timestamp) {
    let mut next = routine.clone();
    next.last_run_at = Some(slot);
    let _ = vault.save_routine(&next);
}

fn run_change() -> Change {
    Change {
        kind: Kind::RoutineRun,
        op: Op::Updated,
        id: None,
        origin: Some("assistant".to_string()),
    }
}

/// The context a run's own commands would be issued under.
///
/// Not used by the turn -- the tools reach the vault directly, as they do in a
/// chat -- but this is the shape a run has, and it is the reason
/// `Caller::Assistant` exists: everything it writes is stamped `assistant`, so
/// a window draws it instead of mistaking it for its own.
pub fn context_for(run: RoutineRunId) -> Ctx {
    Ctx {
        caller: Caller::Assistant(run.to_string()),
        scopes: vec![Scope::All],
        proved_at: None,
        request_id: None,
    }
}

/// "07:00", in the person's own zone.
fn clock_of(slot: jiff::Timestamp, now: &jiff::Zoned) -> String {
    let local = slot.to_zoned(now.time_zone().clone());
    format!("{:02}:{:02}", local.hour(), local.minute())
}

/// "40 minutes", "3 hours". Rounded, because this goes in a sentence.
fn pretty_minutes(seconds: i64) -> String {
    let minutes = (seconds / 60).max(0);
    if minutes < 90 {
        return format!("{minutes} minutes");
    }
    let hours = (minutes + 30) / 60;
    if hours < 36 {
        return format!("{hours} hours");
    }
    format!("{} days", (hours + 12) / 24)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lateness_reads_as_a_person_would_say_it() {
        assert_eq!(pretty_minutes(0), "0 minutes");
        assert_eq!(pretty_minutes(40 * 60), "40 minutes");
        assert_eq!(pretty_minutes(3 * 3600), "3 hours");
        assert_eq!(pretty_minutes(3 * 86_400), "3 days");
        assert_eq!(pretty_minutes(-5), "0 minutes", "a clock that went backwards");
    }
}
