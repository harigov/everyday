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
use everyday_core::agent::tools::Drafting;
use everyday_core::dream;
use everyday_core::meeting::{Recording, Stage, identify};
use everyday_core::proposal::{ProposalSource, max_per_run};
use everyday_core::routine::{DreamScope, Due, Outcome, Routine, RoutineRun, Trigger};
use everyday_core::store::calendars::EventQuery;
use everyday_core::store::meetings::RecordingQuery;
use everyday_core::store::routines::RunQuery;
use everyday_core::store::tasks::TaskQuery;
use everyday_core::task::TaskStatus;
use everyday_core::{
    Conversation, ConversationId, MemoryOrigin, MessageRole, ProposalQuery, RoutineRunId, Vault,
};
use jiff::Timestamp;
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
        service.locked().await;
        return;
    }
    // A second copy of the application holds the write claim. Reading is
    // fine and writing is not, and every routine writes something.
    if !vault.is_writable() {
        return;
    }

    // Snoozed threads whose moment has passed, back to the inbox -- the
    // minute scheduler's hook `docs/plans/mail.md`'s phase 3 section asks
    // for, independent of whether this vault has routines at all. Errors are
    // logged rather than propagated: a mail-less vault answers `Ok(0)`
    // immediately (`Service::get`/`is_writable` are the only reads it does),
    // and a real failure here should not also cost this tick its routines.
    if let Err(e) = crate::outbox::release_due_snoozes(service).await {
        tracing::warn!(error = %e, "could not release due snoozes");
    }

    // Proposals: the expiry sweep, beside the snooze release just above and
    // independent of whether this vault has routines at all, on the same
    // reasoning -- a vault without proposals answers `false` immediately
    // (`supports_proposals` is the only read it does), and a real failure
    // here should not also cost this tick its routines.
    {
        let vault = vault.clone();
        let service = service.clone();
        let now = service.now();
        let _ = blocking(move || {
            if vault.supports_proposals() && sweep_proposals(&vault, now) {
                service.events().changed(Change::new(Kind::Proposal, Op::Updated));
            }
            Ok(())
        })
        .await;
    }

    // The two model-assisted mail features that run unasked, on their own
    // schedule rather than a tool call's -- see `crate::mailai`'s module
    // docs for why neither is a tool, and why each keeps its own per-minute
    // budget rather than sharing `check_mail_rate_limit`'s. Both are no-ops,
    // cheaply, on every account that has not turned them on.
    crate::mailai::categorize_tick(service).await;
    crate::mailai::auto_draft_tick(service).await;

    // The meeting notes watcher -- "Take notes for Design sync?" -- and the
    // failed-spool expiry sweep. Neither is a routine and neither needs the
    // assistant, so both run here rather than waiting on
    // `supports_routines` below; both are no-ops, cheaply, on a vault with
    // meeting notes off. See `crate::meeting::watch` and
    // `crate::meeting::spool::expire_failed_tick`.
    {
        let vault = vault.clone();
        let service = service.clone();
        let _ = blocking(move || {
            crate::meeting::watch::tick(&service, &vault);
            crate::meeting::spool::expire_failed_tick(&service, &vault);
            Ok(())
        })
        .await;
    }

    if !vault.supports_routines() {
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
        // A dream asked for by hand is still a dream: the same digest, the
        // same drafting turn, the same bookkeeping. Run as an ordinary turn it
        // would have been handed an empty prompt and the whole catalogue
        // with nothing held back.
        if let Some(scope) = routine.kind.dream_scope() {
            run_dream(service, &vault, &routine, None, scope, Some(run)).await;
            continue;
        }
        let prompt = routine.instructions.clone();
        resume(service, &vault, &routine, run, prompt, None).await;
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
                .map(|e| {
                    let mut detail = describe_event(&e);
                    // "Last time with these people" -- see
                    // `meeting_prep_detail`'s own doc. Appended to every
                    // `BeforeEvent` subject, not only the "Meeting prep"
                    // template: any routine set to run before a meeting
                    // benefits from knowing there is a note from the last
                    // one with the same people, the same way it already
                    // gets the event's own attendees for free.
                    if let Some(prep) = meeting_prep_detail(vault, &e, now) {
                        detail.push_str("\n\n");
                        detail.push_str(&prep);
                    }
                    Subject { key: e.id.to_string(), label: e.title.clone(), detail }
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

/// How far back [`meeting_prep_detail`] looks for a note from the same
/// people. The same span `agent::tools::meetings::list_meeting_notes` uses
/// as its own default window, so a routine's unasked answer and a person's
/// own question agree on what "recently" means.
const MEETING_PREP_DAYS: i64 = 90;

/// Does any attendee in `a` name the same person as any attendee in `b`?
///
/// Pure, and the whole of the judgement call: matched by email when both
/// sides give one for a pair, by name otherwise. Two different addresses for
/// the same person are not found the same -- there is nothing in an
/// attendee string alone to say so -- which is the right way for this to be
/// wrong: a missed match costs a routine one unremarkable turn with no
/// "last time" section, and a wrong match would put words about a stranger
/// in front of somebody.
fn attendees_overlap(a: &[String], b: &[String]) -> bool {
    let a_keys: Vec<_> = a.iter().map(|s| identify::parse_attendee(s)).collect();
    let b_keys: Vec<_> = b.iter().map(|s| identify::parse_attendee(s)).collect();
    a_keys.iter().any(|(a_name, a_email)| {
        b_keys.iter().any(|(b_name, b_email)| match (a_email, b_email) {
            (Some(ae), Some(be)) => ae == be,
            _ => matches!((a_name, b_name), (Some(an), Some(bn)) if an.eq_ignore_ascii_case(bn)),
        })
    })
}

/// The most recent finished meeting note, within [`MEETING_PREP_DAYS`] of
/// `now`, whose own invited attendees overlap `attendees` -- or `None` when
/// nothing in `recordings` qualifies: too old, no note, or nobody in common.
///
/// Pure and independent of a vault, so the selection itself -- which of
/// several past calls counts as "with these people", and which loses to a
/// more recent one -- is a test with a handful of [`Recording`] values
/// rather than a fixture vault. [`meeting_prep_detail`] is the thin wrapper
/// that actually reads one out of the store.
fn recent_note_for_attendees<'a>(
    recordings: &'a [Recording],
    attendees: &[String],
    now: Timestamp,
    within_days: i64,
) -> Option<&'a Recording> {
    let cutoff = now.as_second() - within_days * 86_400;
    recordings
        .iter()
        .filter(|r| r.stage.is_finished() && r.note_id.is_some())
        .filter(|r| r.started_at.as_second() >= cutoff)
        .filter(|r| r.event.as_ref().is_some_and(|e| attendees_overlap(&e.attendees, attendees)))
        .max_by_key(|r| r.started_at)
}

/// "Last time with these people": a line for the prompt naming the most
/// recent meeting note that shared attendees with `event`, when there is
/// one to name.
///
/// Cheap on purpose -- this runs on every tick for every `BeforeEvent`
/// subject -- so the vault is asked once, bounded to
/// [`MEETING_PREP_DAYS`] and to finished recordings only, rather than
/// walked in full; [`RecordingQuery::from`] does the bounding at the store,
/// and [`recent_note_for_attendees`] does the rest in memory over whatever
/// that query already narrowed down to. `None` when meetings are not
/// supported at all, when the query itself fails, or when nothing
/// qualifies -- every one of those is "say nothing", not an error a routine
/// should stall over.
fn meeting_prep_detail(
    vault: &Vault,
    event: &everyday_core::Event,
    now: &jiff::Zoned,
) -> Option<String> {
    if !vault.supports_meetings() {
        return None;
    }
    let cutoff = Timestamp::from_second(now.timestamp().as_second() - MEETING_PREP_DAYS * 86_400)
        .unwrap_or(Timestamp::MIN);
    let query = RecordingQuery {
        stages: vec![Stage::Done.as_str().to_string()],
        from: Some(cutoff),
        // Belt and braces beside the date bound: a hard cap on rows read,
        // independent of how many calls somebody actually recorded in the
        // window -- the same reasoning `RECORDING_SCAN_CAP` gives in
        // `agent::tools::meetings`.
        limit: Some(200),
        ..Default::default()
    };
    let recordings = vault.recordings(&query).ok()?;
    let recording = recent_note_for_attendees(
        &recordings,
        &event.attendees,
        now.timestamp(),
        MEETING_PREP_DAYS,
    )?;
    let note_id = recording.note_id?;
    let note = vault.note(note_id).ok()?;
    let when = recording.started_at.to_zoned(now.time_zone().clone()).date();
    Some(format!(
        "Last time with these people: the note \"{}\", from {when}. Call get_transcript or \
         read the note itself for what was actually said.",
        note.display_title()
    ))
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
    let prompt = about.instructions.clone();
    resume(service, vault, &about, run, prompt, None).await;
}

/// Everything about one run, from the row to the notification.
async fn execute(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    routine: &Routine,
    slot: Option<jiff::Timestamp>,
) {
    if let Some(scope) = routine.kind.dream_scope() {
        run_dream(service, vault, routine, slot, scope, None).await;
        return;
    }

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
    resume(service, vault, routine, run, routine.instructions.clone(), None).await;
}

/// Everything about a dream's run: whether it may start at all, the digest
/// that becomes its first message, the drafting turn, and the bookkeeping
/// once it is done. Ordinary routines never reach this function.
///
/// # The idle guard
///
/// "No conversation turn in flight" -- [`crate::agent::turn_in_flight`] -- is
/// the cheap definition of idle, read exactly as the module doc for that
/// function states. A dream that finds one running does not stamp its slot,
/// so `Routine::is_due` says the same thing on the very next tick and it is
/// tried again inside its own grace, the same as a tick that found the vault
/// briefly unwritable. A queued run found busy stays queued, for the same
/// reason.
///
/// `queued` is the row "run now" already wrote, when that is how this dream
/// was asked for. A skip then closes that row rather than writing a second.
async fn run_dream(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    routine: &Routine,
    slot: Option<jiff::Timestamp>,
    scope: DreamScope,
    queued: Option<RoutineRun>,
) {
    let skip_this = |reason: &'static str| {
        let queued = queued.clone();
        async move {
            match queued {
                Some(run) => skip_queued(service, vault, run, reason).await,
                None => skip_dream(service, vault, routine, slot, reason).await,
            }
        }
    };
    let (settings, supports_proposals) = blocking({
        let vault = vault.clone();
        move || Ok((vault.agent_settings().unwrap_or_default(), vault.supports_proposals()))
    })
    .await
    .unwrap_or_default();

    if !settings.dreaming {
        skip_this("dreaming is switched off in Settings").await;
        return;
    }
    if !supports_proposals {
        skip_this("this vault's backend does not support proposals").await;
        return;
    }

    if crate::agent::turn_in_flight() {
        return;
    }

    let now = settings.now();
    let Ok(digest) = blocking({
        let vault = vault.clone();
        move || Ok(dream::digest(&vault, scope, &now)?)
    })
    .await
    else {
        skip_this("could not build tonight's digest").await;
        return;
    };

    // Only the day scope is ever skipped for having nothing to say: the
    // week and month digests read other dreams' own words, and a dream that
    // never runs the loop that reads its own outcomes.
    if scope == DreamScope::Day && digest.is_empty() {
        skip_this("nothing happened yesterday").await;
        return;
    }

    let saved = match queued {
        Some(run) => Ok(run),
        None => {
            blocking({
                let vault = vault.clone();
                let routine = routine.clone();
                move || {
                    let run = RoutineRun::new(&routine, slot);
                    vault.save_run(&run)?;
                    Ok(run)
                }
            })
            .await
        }
    };
    let Ok(run) = saved else { return };
    let run_id = run.id;
    let as_of = digest.as_of;

    // The contract the interface reads a dream's digest back out of a
    // transcript by: the app's own instructions, then the person's paragraph
    // if they added one, then the exact line `--- digest ---`, then the
    // digest itself. Nothing else in this application may emit that line.
    let mut prompt = dream::instructions(scope).to_string();
    let extra = routine.instructions.trim();
    if !extra.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(extra);
    }
    prompt.push_str("\n\n--- digest ---\n\n");
    prompt.push_str(&digest.to_markdown());

    let drafting = Drafting {
        source: Some(ProposalSource::Run { run_id }),
        direct: vec!["create_note"],
        max_proposals: Some(max_per_run(scope)),
    };

    resume(service, vault, routine, run, prompt, Some(drafting)).await;

    let vault = vault.clone();
    let _ = blocking(move || {
        finish_dream_bookkeeping(&vault, run_id, as_of);
        Ok(())
    })
    .await;
}

/// Close a dream somebody queued by hand as skipped, with the reason. The
/// row already exists, so it is this one that says why -- not a second.
async fn skip_queued(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    mut run: RoutineRun,
    reason: &str,
) {
    run.outcome = Outcome::Skipped;
    run.reason = reason.to_string();
    run.finished_at = Some(service.now());
    run.seen = true;
    save_run(vault, &run).await;
    service.events().changed(run_change());
}

/// Record a dream as skipped, stamp its slot so it is not reported again on
/// the next tick, and raise the change every other skip raises.
async fn skip_dream(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    routine: &Routine,
    slot: Option<jiff::Timestamp>,
    reason: &str,
) {
    let reason = reason.to_string();
    let _ = blocking({
        let vault = vault.clone();
        let routine = routine.clone();
        move || {
            skip(&vault, &routine, slot, reason);
            if let Some(slot) = slot {
                stamp(&vault, &routine, slot);
            }
            Ok(())
        }
    })
    .await;
    service.events().changed(run_change());
}

/// After a dream's turn finishes: prefix its summary with what it actually
/// did, and apply any memories its final message confirmed. A no-op for a
/// run that did not finish `Done` -- a failed or timed-out dream has nothing
/// to count and nothing to confirm.
fn finish_dream_bookkeeping(vault: &Vault, run_id: RoutineRunId, as_of: jiff::civil::Date) {
    let Ok(mut run) = vault.run(run_id) else { return };
    if run.outcome != Outcome::Done {
        return;
    }

    let proposals = vault
        .proposals(&ProposalQuery { run_id: Some(run_id), ..Default::default() })
        .unwrap_or_default();
    let (notes, memories) = tool_counts(vault, run.conversation_id);
    // Read from the model's own text, before it is prefixed below -- the
    // confirmation section is part of that text, not of the counts this
    // function is about to add in front of it.
    let confirmed = dream::parse_confirmed_memory_ids(&run.summary);

    let counts = counts_sentence(proposals.len(), memories, notes);
    let original = run.summary.trim();
    run.summary =
        if original.is_empty() { format!("{counts}.") } else { format!("{counts}. {original}") };
    let _ = vault.save_run(&run);

    for id in confirmed {
        let Ok(all) = vault.memories() else { continue };
        let Some(mut memory) = all.into_iter().find(|m| m.id == id) else { continue };
        // Confirming is not the same act as a person agreeing something is
        // true from the memory list -- that sets `Confirmed` through a
        // different door -- so `origin` is left exactly as it was; only the
        // evidence date moves.
        if matches!(memory.origin, MemoryOrigin::Inferred | MemoryOrigin::Confirmed) {
            memory.last_supported = Some(as_of);
            let _ = vault.save_memory(&memory);
        }
    }
}

/// How many of this run's tool calls were a successful `create_note` or a
/// successful `remember` -- the two things a dream's summary line counts
/// besides proposals. Read from the transcript rather than from `Turned`,
/// which `resume` does not thread this far: the run is reloaded from the
/// vault by the time this runs, and the transcript is the one record of
/// what actually happened that survives that.
fn tool_counts(vault: &Vault, conversation_id: Option<ConversationId>) -> (usize, usize) {
    let Some(conversation_id) = conversation_id else { return (0, 0) };
    let Ok(messages) = vault.messages(conversation_id) else { return (0, 0) };
    let mut names = std::collections::HashMap::new();
    for m in &messages {
        for call in &m.tool_calls {
            names.insert(call.id.clone(), call.name.clone());
        }
    }
    let mut notes = 0;
    let mut memories = 0;
    for m in &messages {
        if m.role != MessageRole::Tool || m.failed {
            continue;
        }
        let Some(id) = &m.tool_call_id else { continue };
        match names.get(id).map(String::as_str) {
            Some("create_note") => notes += 1,
            Some("remember") => memories += 1,
            _ => {}
        }
    }
    (notes, memories)
}

fn counts_sentence(proposals: usize, memories: usize, notes: usize) -> String {
    format!(
        "{proposals} proposal{}, {memories} memor{}, {notes} note{}",
        plural(proposals),
        if memories == 1 { "y" } else { "ies" },
        plural(notes),
    )
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
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
    prompt: String,
    drafting: Option<Drafting>,
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
        run.finished_at = Some(service.now());
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
        service: service.clone(),
        vault: vault.clone(),
        pending: service.pending(),
        conversation: conversation.id,
        prompt,
        context: None,
        // Nothing is listening: there is no window on the other end of a
        // scheduled run. The events are dropped rather than buffered, and what
        // is kept is the transcript in the vault.
        channel: Arc::new(|_: AgentEvent| {}),
        unattended: Some(run.id),
        drafting,
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
    for (kind, ids) in wrote {
        // One or the other, never both -- see `Change`'s own contract.
        let (id, ids) = match ids.len() {
            1 => (Some(ids.into_iter().next().expect("len 1")), Vec::new()),
            _ => (None, ids),
        };
        service.events().changed(Change {
            kind,
            op: Op::Updated,
            id,
            ids,
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
        ids: Vec::new(),
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

/// Close whatever a pending proposal's own clock, or an event only mail can
/// raise, has already decided -- called from `tick`, beside
/// `release_due_snoozes`. Returns whether anything closed, so the caller
/// knows whether to raise a `Change`.
///
/// [`everyday_core::Vault::expire_proposals`] handles every kind's own
/// deadline. A `SendMail` proposal has a second way to go stale that no
/// deadline alone can see -- its draft sent by hand, discarded, or gone
/// altogether -- so that is swept here too, in the same pass. Every failure
/// is logged and skipped rather than propagated: one bad row must not stop
/// the rest of the sweep, the way one routine's trouble does not stop
/// another's in the loop above.
fn sweep_proposals(vault: &Vault, now: Timestamp) -> bool {
    use everyday_core::proposal::{Payload, ProposalKind};
    use everyday_core::store::proposals::ProposalQuery;

    let mut closed = false;
    match vault.expire_proposals(now) {
        Ok(ids) => closed |= !ids.is_empty(),
        Err(e) => tracing::warn!(error = %e, "could not expire due proposals"),
    }

    let pending = match vault.proposals(&ProposalQuery::pending_of(ProposalKind::Mail)) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "could not list pending mail proposals");
            return closed;
        }
    };
    for mut proposal in pending {
        let Payload::SendMail { draft_id } = &proposal.payload else { continue };
        let draft_id = *draft_id;
        let stale = match vault.draft(draft_id) {
            Ok(draft) => !matches!(draft.state, everyday_core::mail::DraftState::Editing),
            // Gone altogether reads the same as no longer editing: either
            // way there is nothing left to send.
            Err(_) => true,
        };
        if !stale {
            continue;
        }
        proposal.close(everyday_core::proposal::Outcome::Expired { at: now }, now);
        match vault.save_proposal(&proposal) {
            Ok(()) => closed = true,
            Err(e) => tracing::warn!(error = %e, "could not close a stale mail proposal"),
        }
    }
    closed
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

    // ---- meeting prep: "last time with these people" ---------------------

    use everyday_core::id::{CalendarId, NoteId, TemplateId};
    use everyday_core::meeting::EventRef;

    const NOW_SECS: i64 = 1_700_000_000;

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// A finished, noted recording `days_ago` before [`NOW_SECS`], with
    /// `attendees` on its event. `stage`/`note_id` are the defaults a real
    /// finished recording would have; tests that need something else
    /// override the field they care about.
    fn recording(attendees: &[&str], days_ago: i64) -> Recording {
        let event = EventRef {
            calendar_id: CalendarId::new(),
            uid: "call-1".into(),
            title: "Design sync".into(),
            start: Timestamp::from_second(NOW_SECS).unwrap(),
            end: Timestamp::from_second(NOW_SECS + 3_600).unwrap(),
            tz: "UTC".into(),
            organizer: String::new(),
            attendees: strings(attendees),
            join_url: String::new(),
            calendar_name: "Work".into(),
            series: None,
        };
        let mut r = Recording::new("Design sync", Some(event), TemplateId::new());
        r.started_at = Timestamp::from_second(NOW_SECS - days_ago * 86_400).unwrap();
        r.stage = Stage::Done;
        r.note_id = Some(NoteId::new());
        r
    }

    #[test]
    fn attendees_overlap_matches_by_email_before_name() {
        // Same address, spelled differently in case -- the address is what
        // actually identifies the person, so this must match.
        assert!(attendees_overlap(
            &strings(&["Priya Raman <priya@x.com>"]),
            &strings(&["P. Raman <PRIYA@X.COM>"])
        ));
        // No address on either side: falls back to the name, still
        // case-insensitively.
        assert!(attendees_overlap(&strings(&["Priya Raman"]), &strings(&["priya raman"])));
        // Different people entirely.
        assert!(!attendees_overlap(
            &strings(&["priya@x.com"]),
            &strings(&["sam@x.com", "Sam Okafor"])
        ));
    }

    #[test]
    fn recent_note_for_attendees_picks_the_newest_qualifying_one() {
        let now = Timestamp::from_second(NOW_SECS).unwrap();
        let older = recording(&["priya@x.com"], 10);
        let newer = recording(&["priya@x.com"], 2);
        let recordings = vec![older, newer.clone()];
        let found = recent_note_for_attendees(&recordings, &strings(&["priya@x.com"]), now, 90);
        assert_eq!(found.map(|r| r.started_at), Some(newer.started_at));
    }

    #[test]
    fn recent_note_for_attendees_ignores_what_it_should() {
        let now = Timestamp::from_second(NOW_SECS).unwrap();

        let mut not_done = recording(&["priya@x.com"], 1);
        not_done.stage = Stage::Recording;

        let mut no_note = recording(&["priya@x.com"], 1);
        no_note.note_id = None;

        let too_old = recording(&["priya@x.com"], 200);
        let different_people = recording(&["sam@x.com"], 1);

        let recordings = vec![not_done, no_note, too_old, different_people];
        let found = recent_note_for_attendees(&recordings, &strings(&["priya@x.com"]), now, 90);
        assert!(found.is_none(), "{found:?}");
    }
}
