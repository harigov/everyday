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
use crate::events::{Change, Kind, Notification, Op};
use crate::service::{Service, blocking};
use everyday_core::routine::{Due, Outcome, Routine, RoutineRun, Trigger};
use everyday_core::store::calendars::EventQuery;
use everyday_core::store::routines::RunQuery;
use everyday_core::store::tasks::TaskQuery;
use everyday_core::task::TaskStatus;
use everyday_core::{Conversation, Vault};
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
        // Deliberately *not* inside the `select!`. A tick that was cancelled
        // half way through would drop the future carrying a live model turn,
        // which is the one thing this loop must never do: the run's row would
        // be left saying `Running` with nothing to finish it. Stopping is
        // therefore checked between ticks, and whoever asked for it waits.
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
        service.locked();
        return;
    }
    // A second copy of the application holds the write claim. Reading is
    // fine and writing is not, and every routine writes something.
    if !vault.is_writable() || !vault.supports_routines() {
        return;
    }

    // The two reads share one trip to the blocking pool: if the first fails
    // there is nothing due to compute anyway, and the order -- routines
    // first, then the settings that say what "now" is -- matches what the
    // two separate calls this replaced did.
    let Ok((routines, settings)) = blocking({
        let vault = vault.clone();
        move || Ok((vault.routines()?, vault.agent_settings().unwrap_or_default()))
    })
    .await
    else {
        return;
    };
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
                let reason = format!(
                    "its {} was {late} ago, past the {} minutes it allows",
                    clock_of(slot, &now),
                    routine.grace_minutes
                );
                // Grouped into one trip to the blocking pool: recording the
                // skip and stamping the slot are the same fact about the
                // same routine, and nothing between them needs the async
                // runtime.
                let _ = blocking({
                    let vault = vault.clone();
                    let routine = routine.clone();
                    move || {
                        skip(&vault, &routine, Some(slot), reason);
                        // Stamped even though nothing ran, or the same slot
                        // is reported missed on every tick for the rest of
                        // the day.
                        stamp(&vault, &routine, slot);
                        Ok(())
                    }
                })
                .await;
                service.events().changed(run_change());
            }
            Due::Now { slot } => {
                execute(service, &vault, &routine, Some(slot)).await;
            }
            Due::Later => {}
        }
    }

    // The triggers that are questions rather than moments. Evaluated as a
    // query on each tick rather than subscribed to, because the events they
    // are about arrive from somebody else's server on a refresh timer -- there
    // is no moment to compute in advance, and a subscription would have
    // nothing to fire it. Polling on the minute is also what makes a missed
    // window an honest nothing rather than a callback that never came.
    let routines_for_triggers = blocking({
        let vault = vault.clone();
        move || Ok(vault.routines().unwrap_or_default())
    })
    .await
    .unwrap_or_default();
    for routine in routines_for_triggers {
        if !routine.enabled || routine.trigger.is_clock() {
            continue;
        }
        // Read once for the routine rather than once per subject: the guard
        // is the same set for all of them, and a routine with a hundred
        // subjects in its window would otherwise read its whole log a
        // hundred times.
        let mut done = blocking({
            let vault = vault.clone();
            let routine = routine.clone();
            move || Ok(already_about(&vault, &routine))
        })
        .await
        .unwrap_or_default();
        let subjects = blocking({
            let vault = vault.clone();
            let routine = routine.clone();
            let now = now.clone();
            move || Ok(subjects_for(&vault, &routine, &now))
        })
        .await
        .unwrap_or_default();
        for subject in subjects {
            // Once per meeting, however many ticks it is in the window for.
            // The subject is the whole of that guard, which is why it is the
            // record's id rather than its title: two meetings called "Weekly"
            // are two meetings.
            if !done.insert(subject.key.clone()) {
                continue;
            }
            about(service, &vault, &routine, subject).await;
        }
    }

    // A row still `Running` that no live process claims is one a *previous*
    // process left behind: the app was quit mid-run, the machine slept, the
    // vault was switched. It cannot be resumed -- the turn it belonged to is
    // gone -- and it must not sit on the app bar looking like one that is
    // about to finish. So it is closed as failed, saying so.
    //
    // The claim is the whole test, and it is reliable in the direction that
    // matters: a run this process is carrying out is inside `resume`, which
    // has not returned, so `tick` cannot be here looking at it.
    //
    // The whole sweep is one trip to the blocking pool rather than one per
    // row: nothing in it ever awaited anything even when it ran inline on
    // this async fn, so grouping it changes when the events after it fire
    // relative to each other and not what any of them say -- `run_change`
    // carries no per-run identity for that to matter to.
    let abandoned = blocking({
        let vault = vault.clone();
        let service = service.clone();
        move || {
            let mut count = 0;
            for run in vault
                .runs(&RunQuery { outcomes: vec![Outcome::Running], ..Default::default() })
                .unwrap_or_default()
            {
                if service.claims_run(&run.id.to_string()) {
                    continue;
                }
                let mut abandoned = run.clone();
                abandoned.fail("it was still running when the application stopped");
                let _ = vault.save_run(&abandoned);
                if let (Some(slot), Ok(routine)) = (run.slot, vault.routine(run.routine_id)) {
                    stamp(&vault, &routine, slot);
                }
                count += 1;
            }
            Ok(count)
        }
    })
    .await
    .unwrap_or(0);
    for _ in 0..abandoned {
        service.events().changed(run_change());
    }

    // And whatever somebody asked for by hand. Queued rather than executed in
    // the command, so that "run now" is answered immediately and the work
    // still happens one at a time.
    let queued = blocking({
        let vault = vault.clone();
        move || {
            Ok(vault
                .runs(&RunQuery { outcomes: vec![Outcome::Queued], ..Default::default() })
                .unwrap_or_default())
        }
    })
    .await
    .unwrap_or_default();
    for run in queued {
        let routine_id = run.routine_id;
        let run_id = run.id;
        // The lookup and, on failure, the delete are the same trip: both are
        // about deciding whether this straggler still has a routine to run.
        let routine = blocking({
            let vault = vault.clone();
            move || {
                Ok(match vault.routine(routine_id) {
                    Ok(routine) => Some(routine),
                    // Its routine was deleted between the asking and now.
                    // The cascade normally takes the runs with it; this one
                    // is a straggler.
                    Err(_) => {
                        let _ = vault.delete_run(run_id);
                        None
                    }
                })
            }
        })
        .await
        .unwrap_or(None);
        let Some(routine) = routine else { continue };
        resume(service, &vault, &routine, run).await;
    }
}

/// Something a query trigger found: what it is, and how to say so.
struct Subject {
    /// The record's id. What a second run for the same thing is refused by.
    key: String,
    /// A line naming it, for the run log and the notification.
    label: String,
    /// Everything the model is told about it, appended to the instructions.
    detail: String,
}

/// What a query trigger has found, right now.
///
/// Bounded by the trigger's own window: an event an hour away is not found
/// two hours out, so a routine set to run an hour before a meeting runs an
/// hour before it and not at breakfast.
fn subjects_for(vault: &Arc<Vault>, routine: &Routine, now: &jiff::Zoned) -> Vec<Subject> {
    match &routine.trigger {
        Trigger::BeforeEvent { lead_minutes, role_id } => {
            let lead = i64::from(*lead_minutes) * 60;
            let window_end = now.timestamp().as_second() + lead;
            // Two days of events, filtered by the instant. The query is by
            // day, and a lead of a day and a half would otherwise miss its
            // own window at the end of the range.
            let Ok(events) = vault.events(&EventQuery {
                from: Some(now.date()),
                to: now.date().checked_add(jiff::Span::new().days(2)).ok(),
                visible_only: true,
                ..Default::default()
            }) else {
                return Vec::new();
            };
            // A role narrows by *calendar*, because that is where a role is
            // filed: a work feed is work, and its events do not each carry
            // their own answer.
            let allowed: Option<std::collections::HashSet<_>> = role_id.map(|role| {
                vault
                    .calendars()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| c.role_id == Some(role))
                    .map(|c| c.id)
                    .collect()
            });
            events
                .into_iter()
                .filter(|e| {
                    let starts = e.start.as_second();
                    starts > now.timestamp().as_second() && starts <= window_end
                })
                .filter(|e| e.status != everyday_core::EventStatus::Cancelled && e.busy)
                .filter(|e| allowed.as_ref().is_none_or(|ids| ids.contains(&e.calendar_id)))
                .map(|e| Subject {
                    key: e.id.to_string(),
                    label: e.title.clone(),
                    detail: describe_event(&e),
                })
                .collect()
        }
        Trigger::TaskDue { lead_days } => {
            // Dated tasks only, falling due between today and the lead. A
            // task with no deadline never falls due, so it never triggers
            // this -- which is the whole difference between a deadline and a
            // wish.
            let Ok(tasks) = vault.tasks(&TaskQuery {
                due_from: Some(now.date()),
                due_to: now.date().checked_add(jiff::Span::new().days(i64::from(*lead_days))).ok(),
                statuses: TaskStatus::ALL.iter().copied().filter(|s| s.is_open()).collect(),
                ..Default::default()
            }) else {
                return Vec::new();
            };
            tasks
                .into_iter()
                .map(|t| Subject {
                    key: t.id.to_string(),
                    label: t.title.clone(),
                    detail: describe_task(&t),
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// What the model is told about a meeting, beside the routine's instructions.
fn describe_event(event: &everyday_core::Event) -> String {
    let mut out = format!("The meeting is \"{}\", starting {}", event.title, event.start);
    if !event.location.trim().is_empty() {
        out.push_str(&format!(", at {}", event.location.trim()));
    }
    out.push('.');
    if !event.organizer.trim().is_empty() {
        out.push_str(&format!(" It was called by {}.", event.organizer.trim()));
    }
    if !event.attendees.is_empty() {
        out.push_str(&format!(" Also invited: {}.", event.attendees.join(", ")));
    }
    if !event.description.trim().is_empty() {
        out.push_str(&format!("\n\nThe invitation says:\n{}", event.description.trim()));
    }
    out
}

fn describe_task(task: &everyday_core::Task) -> String {
    let mut out = format!("The task is \"{}\"", task.title);
    if let Some(due) = task.due_date {
        out.push_str(&format!(", due {due}"));
    }
    out.push('.');
    if !task.notes.trim().is_empty() {
        out.push_str(&format!(" It says: {}", task.notes.trim()));
    }
    out
}

/// Everything this routine has already been about.
///
/// The run log is the record, so a meeting the laptop was awake for at nine
/// and again at nine-oh-one is prepared for once. Read whole rather than
/// capped: a capped read is worse than no guard at all, because once a
/// routine has more subjects in its window than the cap, the oldest fall out
/// of the log's newest-first window and are run again every single minute --
/// one paid model call each, for ever. The log is bounded by what a routine
/// has actually done, which is what `collect_garbage` is for.
fn already_about(vault: &Arc<Vault>, routine: &Routine) -> std::collections::HashSet<String> {
    vault
        .runs(&RunQuery { routine_id: Some(routine.id), ..Default::default() })
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| r.subject)
        .collect()
}

/// Run a routine about one thing it found.
async fn about(service: &Arc<Service>, vault: &Arc<Vault>, routine: &Routine, subject: Subject) {
    let mut about = routine.clone();
    // The subject goes *after* the instructions, so the routine's own words
    // are what the model reads first and this is the detail they apply to.
    about.instructions = format!("{}\n\n{}", routine.instructions.trim(), subject.detail);
    // Named for what it is about, so a log of five meeting preparations reads
    // as five meetings rather than five identical rows.
    about.name = format!("{} \u{2014} {}", routine.name, subject.label);

    let routine_id = routine.id;
    let saved = blocking({
        let vault = vault.clone();
        let about = about.clone();
        move || {
            let mut run = RoutineRun::new(&about, None);
            // The run belongs to the *routine*, whatever the run is called:
            // its log has to find it.
            run.routine_id = routine_id;
            // Written before the turn, so a second tick during a long run
            // sees it and does not start the same preparation again.
            run.subject = Some(subject.key);
            vault.save_run(&run)?;
            Ok(run)
        }
    })
    .await;
    let Ok(run) = saved else { return };
    resume(service, vault, &about, run).await;
}

/// Everything about one run, from the row to the notification.
async fn execute(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    routine: &Routine,
    slot: Option<jiff::Timestamp>,
) {
    let saved = blocking({
        let vault = vault.clone();
        let routine = routine.clone();
        move || {
            let run = RoutineRun::new(&routine, slot);
            vault.save_run(&run)?;
            Ok(run)
        }
    })
    .await;
    let Ok(run) = saved else { return };
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
    // Claimed for as long as this call is on the stack, so the sweep above can
    // tell a run this process is carrying out from one a dead process left.
    let _claim = service.claim_run(run.id.to_string());
    run.begin();
    save_run(vault, &run).await;
    // The credential check is first and is a *skip*, not a failure: an
    // assistant that is switched off has not failed at anything, and the
    // reason is the useful part. Read as `core::Result<Result<_, String>>`
    // rather than converted through `?` -- the message a person sees is
    // `everyday_core::Error`'s own text, and running this through
    // `CommandError` first would prefix it with a code nobody asked to read.
    let unusable = blocking({
        let vault = vault.clone();
        move || Ok(vault.agent_credentials().map(|_| ()).map_err(|e| e.to_string()))
    })
    .await;
    if let Some(reason) = unusable.unwrap_or_else(|e| Err(e.to_string())).err() {
        run.outcome = Outcome::Skipped;
        run.reason = reason;
        run.finished_at = Some(jiff::Timestamp::now());
        run.seen = true;
        save_run(vault, &run).await;
        if let Some(slot) = run.slot {
            stamp_async(vault, routine, slot).await;
        }
        service.events().changed(run_change());
        return;
    }

    // The transcript. Named after the routine so the rail's thread list reads,
    // and carrying the run so `chats_only` keeps it out of that list.
    let conversation = Conversation::for_run(run.id, routine.name.clone());
    let saved_conversation = blocking({
        let vault = vault.clone();
        let conversation = conversation.clone();
        move || Ok(vault.save_conversation(&conversation)?)
    })
    .await;
    if saved_conversation.is_ok() {
        run.conversation_id = Some(conversation.id);
        save_run(vault, &run).await;
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

    let mut wrote = Vec::new();
    match tokio::time::timeout(RUN_TIMEOUT, crate::agent::run_turn(turn)).await {
        Ok(Ok(turned)) => {
            run.steps = turned.steps;
            wrote = turned.wrote;
            run.finish(Outcome::Done, turned.text);
        }
        Ok(Err(e)) => run.fail(e.to_string()),
        Err(_) => run.fail(format!(
            "it was still going after {} minutes and was stopped",
            RUN_TIMEOUT.as_secs() / 60
        )),
    }

    service.set_running_routine(None);
    save_run(vault, &run).await;
    if let Some(slot) = run.slot {
        stamp_async(vault, routine, slot).await;
    }
    announce(service, routine, &run);
    // What the run *wrote*, and then the run itself. Only the second of these
    // used to be raised, so a window with the Notes app open went on showing
    // yesterday's list after the morning brief had written into it: the run
    // appeared in the Assistant app and the note it made appeared nowhere
    // until somebody switched apps.
    for kind in wrote {
        service.events().changed(Change {
            kind,
            op: Op::Updated,
            id: None,
            origin: Some("assistant".to_string()),
        });
    }
    service.events().changed(run_change());
}

/// Say something, once.
///
/// Keyed so that a repetition replaces its predecessor rather than stacking:
/// a routine whose endpoint has been unreachable every morning for a week says
/// so once rather than seven times -- the same courtesy `feed_failed` extends
/// to a calendar that has stopped answering. The title names the routine and
/// never carries what it found: this is the one notification in the
/// application that will routinely be drawn over a lock screen.
///
/// The *subject* is part of the key on the way out, and is not on the way in.
/// A query trigger produces one run, and one notification, per thing it
/// matched -- two meetings in the next hour are two briefs -- and keyed on the
/// routine alone the second silently replaced the first, so one of the two
/// pieces of work was never seen. A failure is still keyed on the routine
/// alone, because a routine that cannot reach its model fails once per subject
/// for the same single reason, and saying so ten times is the noise the key
/// exists to prevent.
fn announce(service: &Arc<Service>, routine: &Routine, run: &RoutineRun) {
    let routine_key = format!("routine:{}", routine.id);
    match run.outcome {
        Outcome::Done => {
            service.routine_recovered(routine.id.to_string());
            if run.summary.trim().is_empty() && run.steps == 0 {
                return;
            }
            let key = match &run.subject {
                Some(subject) => format!("{routine_key}:{subject}"),
                None => routine_key,
            };
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
                        .key(routine_key),
                );
            }
        }
        // A skip is not news. It is a row somebody will see when they look.
        Outcome::Skipped | Outcome::Queued | Outcome::Running => {}
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
///
/// Re-read rather than written back from the copy the run started with. A run
/// may take a quarter of an hour, and in that time somebody can switch the
/// routine off, rewrite its instructions or delete it -- and `put_routine` is
/// an upsert, so writing back the old copy would undo the edit or resurrect
/// the routine with an empty log. What this is allowed to change is one field.
fn stamp(vault: &Arc<Vault>, routine: &Routine, slot: jiff::Timestamp) {
    // Gone while it ran. Nothing to stamp, and nothing to bring back.
    let Ok(mut current) = vault.routine(routine.id) else { return };
    current.last_run_at = Some(slot);
    let _ = vault.save_routine(&current);
}

/// Run [`stamp`] off the async runtime.
///
/// The Missed arm and the abandoned-run sweep call `stamp` directly, because
/// each is already inside a `blocking` closure doing everything it needs in
/// one trip; `resume` is not -- it calls this between two `.await`s of its
/// own -- so it gets a version that takes the hop itself.
async fn stamp_async(vault: &Arc<Vault>, routine: &Routine, slot: jiff::Timestamp) {
    let vault = vault.clone();
    let routine = routine.clone();
    let _ = blocking(move || {
        stamp(&vault, &routine, slot);
        Ok(())
    })
    .await;
}

/// Save a run's row off the async runtime.
///
/// `resume` saves the row at every one of its transitions -- begun, skipped,
/// connected to its transcript, finished -- and every one of those points is
/// separated from the next by something that awaits, most of all the model
/// turn itself, so the four saves cannot be grouped into one trip to the
/// blocking pool the way the sweeps above group theirs.
async fn save_run(vault: &Arc<Vault>, run: &RoutineRun) {
    let vault = vault.clone();
    let run = run.clone();
    let _ = blocking(move || Ok(vault.save_run(&run)?)).await;
}

fn run_change() -> Change {
    Change {
        kind: Kind::RoutineRun,
        op: Op::Updated,
        id: None,
        origin: Some("assistant".to_string()),
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
