//! The assistant's standing work: the routines, and the log of what they did.
//!
//! Eleven commands, and the only one worth reading is `run_routine`: it does not
//! run anything. It writes a `Running` row with no slot and returns its id,
//! and the scheduler picks it up on the next tick. That is not laziness -- it
//! is what keeps "one run at a time" true. A command that ran the turn itself
//! would let three impatient clicks start three model calls that race each
//! other for the vault's single writer.

use super::Nothing;
use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::service::{Service, blocking};
use everyday_core::routine::{Routine, RoutineRun, Trigger, Weekday};
use everyday_core::store::routines::RunQuery;
use everyday_core::{RoutineId, RoutineRunId};
use jiff::civil::time;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineRef {
    pub id: RoutineId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveRoutine {
    pub routine: Routine,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Runs {
    #[serde(default)]
    pub query: RunQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRef {
    pub id: RoutineRunId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeenRuns {
    /// Which runs to mark. Empty means all of them.
    #[serde(default)]
    pub ids: Vec<RoutineRunId>,
}

/// A routine, and the two things a list has to say that the record does not.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineInfo {
    #[serde(flatten)]
    pub routine: Routine,
    /// The trigger in words. Derived in the core so that the interface and the
    /// assistant cannot spell "Weekdays at 07:00" two different ways.
    pub when: String,
    /// When it will next run, or absent for a trigger that is not a clock.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_due: Option<jiff::Timestamp>,
}

/// A routine somebody could start from, filled in.
///
/// Offered, never imposed: a template is only an editor with words already in
/// it, and every field can be changed before it is saved. They live here
/// rather than in the interface because the assistant offers them too -- asked
/// to "set up a morning brief", it should propose the same thing the plus
/// button does.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub name: String,
    pub instructions: String,
    pub trigger: Trigger,
    /// Why somebody would want this one. Drawn under the name.
    pub note: String,
    /// False for a template whose trigger needs something not built yet.
    pub available: bool,
}

async fn list_routines(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<RoutineInfo>> {
    let vault = svc.require()?;
    blocking(move || {
        let now = vault.agent_settings().unwrap_or_default().now();
        Ok(vault
            .routines()?
            .into_iter()
            .map(|routine| RoutineInfo {
                when: routine.trigger.describe(),
                next_due: routine.next_due(&now),
                routine,
            })
            .collect())
    })
    .await
}

/// Mint a routine without saving it.
///
/// The same shape as `new_entry` and `new_note`, and for the same reason: the
/// id and the timestamps are the core's to allocate, never the interface's. A
/// webview that minted its own would need `crypto.randomUUID`, which wants a
/// secure context the packaged shell does not always have -- and would mint
/// v4 where everything else in this vault is v7.
async fn new_routine(_svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<Routine> {
    Ok(Routine::new("", "", Trigger::Schedule { at: time(7, 0, 0, 0), days: Vec::new() }))
}

async fn save_routine(svc: Arc<Service>, _ctx: Ctx, args: SaveRoutine) -> CommandResult<Routine> {
    let vault = svc.require()?;
    blocking(move || {
        let mut routine = args.routine;
        routine.updated_at = jiff::Timestamp::now();
        vault.save_routine(&routine)?;
        Ok(routine)
    })
    .await
}

async fn delete_routine(svc: Arc<Service>, _ctx: Ctx, args: RoutineRef) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.delete_routine(args.id)?)).await
}

/// Ask for a run, and let the scheduler do it.
///
/// See the module note: the row is queued rather than executed here, which is
/// what keeps runs serial however many times somebody presses the button.
async fn run_routine(svc: Arc<Service>, _ctx: Ctx, args: RoutineRef) -> CommandResult<RoutineRun> {
    let vault = svc.require()?;
    blocking(move || {
        let routine = vault.routine(args.id)?;
        // Not twice. A second queued run of the same routine would be a second
        // model call for the same question, paid for twice.
        let waiting = vault.runs(&RunQuery::for_routine(routine.id))?;
        if let Some(already) = waiting.into_iter().find(|r| !r.outcome.is_finished()) {
            return Ok(already);
        }
        let run = RoutineRun::new(&routine, None);
        vault.save_run(&run)?;
        Ok(run)
    })
    .await
}

async fn list_runs(svc: Arc<Service>, _ctx: Ctx, args: Runs) -> CommandResult<Vec<RoutineRun>> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.runs(&args.query)?)).await
}

async fn get_run(svc: Arc<Service>, _ctx: Ctx, args: RunRef) -> CommandResult<RoutineRun> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.run(args.id)?)).await
}

async fn delete_run(svc: Arc<Service>, _ctx: Ctx, args: RunRef) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.delete_run(args.id)?)).await
}

async fn mark_runs_seen(svc: Arc<Service>, _ctx: Ctx, args: SeenRuns) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.mark_runs_seen(&args.ids)?)).await
}

async fn unseen_runs(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<u64> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.unseen_runs()?)).await
}

async fn routine_templates(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<Template>> {
    let vault = svc.require()?;
    let calendars = vault.supports_calendars();
    Ok(templates(calendars))
}

/// The five a person is offered, and why each one.
///
/// A blank routine is a hard thing to write -- it is a prompt, and most people
/// have never written one -- so the empty state is these rather than an empty
/// box with a cursor in it.
fn templates(calendars: bool) -> Vec<Template> {
    vec![
        Template {
            name: "Morning brief".into(),
            instructions: "Look at what is due today and overdue, what is on the calendar, \
                 and any habit I am behind on. Write me a short note called \"Brief for \
                 <today's date>\" with the three or four things that actually matter, and \
                 say plainly if there is nothing much on."
                .into(),
            trigger: Trigger::Schedule { at: time(7, 0, 0, 0), days: Weekday::WEEKDAYS.to_vec() },
            note: "What is on today, before you open anything.".into(),
            available: true,
        },
        Template {
            name: "Weekly review".into(),
            instructions: "Look back over the last seven days: where the time went by role, \
                 which goals were touched and which were not, how the habits went against \
                 their cadence, and what is still open from last week. Write it up as a \
                 note. Be honest about the roles that got nothing."
                .into(),
            trigger: Trigger::Schedule { at: time(17, 0, 0, 0), days: vec![Weekday::Fri] },
            note: "An honest account of the week, written down.".into(),
            available: true,
        },
        Template {
            name: "Weekend planner".into(),
            instructions: "Look at the weekend: what is already on the calendar, what is due, \
                 and which parts of my life have had no time at all this week. Suggest a \
                 shape for Saturday and Sunday and block out time for two or three things \
                 worth doing. Leave plenty of the day unbooked."
                .into(),
            trigger: Trigger::Schedule { at: time(18, 0, 0, 0), days: vec![Weekday::Thu] },
            note: "Something planned for the weekend, before it arrives.".into(),
            available: true,
        },
        Template {
            name: "Something to read".into(),
            instructions: "Look at what is on my shelves, what I have finished lately and \
                 what I have said I mean to get to. Pick two or three things from what is \
                 already there that fit what I seem to be interested in at the moment, and \
                 write me a note saying why each one."
                .into(),
            trigger: Trigger::Schedule { at: time(20, 0, 0, 0), days: vec![Weekday::Sun] },
            note: "A nudge towards what is already on the shelf.".into(),
            available: true,
        },
        Template {
            name: "Meeting prep".into(),
            instructions: "Before this meeting, look through my journal, my tasks and my \
                 notes for anything about the people in it or the subject, and write me a \
                 short note: who they are, what we last said, and what is outstanding."
                .into(),
            trigger: Trigger::BeforeEvent { lead_minutes: 60, role_id: None },
            note: "Who is coming, and what you last said to them.".into(),
            // The trigger needs the calendar. Offered greyed rather than
            // hidden, so that "why is there no meeting prep" has an answer.
            available: calendars,
        },
    ]
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_routines", scope: Agent, effect: Read,
        args: Nothing, returns: "RoutineInfo[]", signature: &[],
        run: list_routines,
    },
    command! {
        name: "new_routine", scope: Agent, effect: Read,
        args: Nothing, returns: "Routine", signature: &[],
        run: new_routine,
    },
    command! {
        name: "save_routine", scope: Agent, effect: Write,
        change: Routine / Updated,
        args: SaveRoutine, returns: "Routine",
        signature: &[("routine", "Routine", true)],
        run: save_routine,
    },
    command! {
        name: "delete_routine", scope: Agent, effect: Destructive,
        change: Routine / Deleted,
        args: RoutineRef, returns: "void",
        signature: &[("id", "RoutineId", true)],
        run: delete_routine,
    },
    command! {
        name: "run_routine", scope: Agent, effect: Write,
        change: RoutineRun / Created,
        args: RoutineRef, returns: "RoutineRun",
        signature: &[("id", "RoutineId", true)],
        run: run_routine,
    },
    command! {
        name: "list_runs", scope: Agent, effect: Read,
        args: Runs, returns: "RoutineRun[]",
        signature: &[("query", "RunQuery", true)],
        run: list_runs,
    },
    command! {
        name: "get_run", scope: Agent, effect: Read,
        args: RunRef, returns: "RoutineRun",
        signature: &[("id", "RoutineRunId", true)],
        run: get_run,
    },
    command! {
        name: "delete_run", scope: Agent, effect: Destructive,
        change: RoutineRun / Deleted,
        args: RunRef, returns: "void",
        signature: &[("id", "RoutineRunId", true)],
        run: delete_run,
    },
    command! {
        name: "mark_runs_seen", scope: Agent, effect: Write,
        change: RoutineRun / Updated,
        args: SeenRuns, returns: "void",
        signature: &[("ids", "RoutineRunId[]", false)],
        run: mark_runs_seen,
    },
    command! {
        name: "unseen_runs", scope: Agent, effect: Read,
        args: Nothing, returns: "number", signature: &[],
        run: unseen_runs,
    },
    command! {
        name: "routine_templates", scope: Agent, effect: Read,
        args: Nothing, returns: "Template[]", signature: &[],
        run: routine_templates,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_is_a_routine_that_would_save() {
        for t in templates(true) {
            let routine = Routine::new(&t.name, &t.instructions, t.trigger.clone());
            routine.validate().unwrap_or_else(|e| panic!("{}: {e}", t.name));
            assert!(!t.note.trim().is_empty(), "{} needs a reason somebody would want it", t.name);
        }
    }

    #[test]
    fn meeting_prep_is_offered_greyed_when_there_is_no_calendar() {
        let without = templates(false);
        let prep = without.iter().find(|t| t.name == "Meeting prep").expect("still offered");
        assert!(!prep.available, "greyed rather than hidden, so the absence has an answer");
        assert!(templates(true).iter().all(|t| t.available));
    }
}
