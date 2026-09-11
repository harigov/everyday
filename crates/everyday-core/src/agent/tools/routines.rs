//! The assistant's own standing work, and the log of what it did.

use serde_json::{Value, json};

use super::{
    Args, Tool, ToolContext, done, empty_schema, flag, limit_arg, list, number, schema, text,
};
use crate::error::{Error, Result};
use crate::id::RoutineId;
use crate::routine::{Routine, RoutineRun, Trigger, Weekday};
use crate::store::routines::RunQuery;

pub(super) static TOOLS: &[Tool] = &[
    tool!(
        "list_routines",
        Read,
        Routines,
        empty_schema(),
        "Your own standing work: what runs without being asked, when, and when \
         each last ran.",
        run_list_routines
    ),
    tool!(
        "create_routine",
        Write,
        Routines,
        schema(
            vec![
                ("name", text("What to call it. Short: it goes in a list.")),
                (
                    "instructions",
                    text(
                        "What to do when it runs, addressed to yourself, in their words \
                         where you have them. This is the whole prompt you will be given: \
                         nothing else about the conversation you are having now survives, \
                         so say what to look at and what to produce."
                    )
                ),
                ("at", text("Time of day, 24-hour, as HH:MM. For example 07:00.")),
                (
                    "days",
                    list(
                        "Days it runs: mon, tue, wed, thu, fri, sat, sun. Omit for every \
                         day."
                    )
                ),
                (
                    "grace_minutes",
                    number(
                        "How long after its time it will still run before the run is \
                         recorded as missed. Default 60."
                    )
                ),
            ],
            &["name", "instructions", "at"]
        ),
        "Set up standing work: something you will do on a schedule, without being asked. \
         Use this when they say they want something to happen regularly \u{2014} \"every \
         Sunday evening, plan my week\". Say what you have set up and when it will first \
         run; they can see it and change it in the Assistant app.",
        run_create_routine
    ),
    tool!(
        "update_routine",
        Write,
        Routines,
        schema(
            vec![
                ("routine_id", text("Id from list_routines.")),
                ("name", text("Replaces the name. Omit to leave it alone.")),
                ("instructions", text("Replaces the instructions. Omit to leave them alone.")),
                ("at", text("New time of day as HH:MM.")),
                ("days", list("Replaces the days entirely.")),
                ("grace_minutes", number("New grace, in minutes.")),
                ("enabled", flag("Switch it on or off without deleting it.")),
            ],
            &["routine_id"]
        ),
        "Change a routine. Every field is optional and omitted fields are left alone. To \
         stop one without losing it, set enabled to false.",
        run_update_routine
    ),
    tool!(
        "delete_routine",
        Destructive,
        Routines,
        schema(vec![("routine_id", text("Id of the routine to delete."))], &["routine_id"]),
        "Permanently delete a routine, its run log and the transcripts of those runs. \
         Anything it made \u{2014} notes, tasks \u{2014} is left alone. There is no undo.",
        run_delete_routine,
        Some(describe_delete_routine)
    ),
    tool!(
        "run_routine",
        Write,
        Routines,
        schema(vec![("routine_id", text("Id from list_routines."))], &["routine_id"]),
        "Ask for a routine to run now, as well as on its schedule. It is queued rather \
         than run this instant \u{2014} runs happen one at a time \u{2014} so it will start \
         within a minute. Say that rather than describing what it found, which you will \
         not know yet.",
        run_run_routine
    ),
    tool!(
        "list_runs",
        Read,
        Routines,
        schema(
            vec![
                ("routine_id", text("Only this routine's runs. Omit for all of them.")),
                limit_arg(),
            ],
            &[]
        ),
        "What your routines have done lately, newest first: when, whether it worked, and \
         what you said at the end of it. Use this to answer \"what did you do this \
         morning\".",
        run_list_runs
    ),
];

fn describe_delete_routine(ctx: &ToolContext<'_>, args: &Args<'_>) -> Option<String> {
    let id: RoutineId = args.opt_id("routine_id", "routine").ok()??;
    ctx.vault.routine(id).ok().map(|r| r.name)
}

fn routine_json(r: &Routine, now: &jiff::Zoned) -> Value {
    let mut out = json!({
        "id": r.id.to_string(),
        "name": r.name,
        "when": r.trigger.describe(),
        "enabled": r.enabled,
    });
    let map = out.as_object_mut().expect("built as an object");
    if let Some(last) = r.last_run_at {
        map.insert("last_run".into(), json!(last.to_string()));
    } else {
        map.insert("last_run".into(), json!("never"));
    }
    if let Some(next) = r.next_due(now) {
        map.insert("next_run".into(), json!(next.to_string()));
    }
    out
}

fn run_list_routines(ctx: &ToolContext<'_>, _args: &Args<'_>) -> Result<Value> {
    let now = ctx.now();
    let routines = ctx.vault.routines()?;
    Ok(json!({
        "count": routines.len(),
        "routines": routines.iter().map(|r| routine_json(r, &now)).collect::<Vec<_>>(),
    }))
}

/// Read `at` and `days` into a schedule.
///
/// The time is parsed rather than taken as two numbers, because "07:00" is
/// what a model produces when asked for a time of day and splitting it into
/// fields invites one of them being forgotten.
fn schedule_from(args: &Args<'_>) -> Result<Trigger> {
    let raw = args.str("at")?;
    let at = raw.trim().parse::<jiff::civil::Time>().map_err(|_| {
        args.bad(format!("{raw:?} is not a time of day. Use 24-hour HH:MM, for example 07:00."))
    })?;
    Ok(Trigger::Schedule { at, days: parse_days(args)? })
}

/// The `days` argument, deduplicated in the order they were given.
///
/// Shared by [`schedule_from`], where an absent list means every day, and by
/// `update_routine`'s days-only branch, where a schedule is already known to
/// exist and only its days are changing -- the two places this used to be
/// typed out separately, with the same error text re-derived each time.
fn parse_days(args: &Args<'_>) -> Result<Vec<Weekday>> {
    let mut days = Vec::new();
    for word in args.strings("days") {
        let day = Weekday::parse(&word).ok_or_else(|| {
            args.bad(format!("{word:?} is not a day. Use mon, tue, wed, thu, fri, sat or sun."))
        })?;
        if !days.contains(&day) {
            days.push(day);
        }
    }
    Ok(days)
}

fn run_create_routine(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    // The one recursion this refuses. A routine that makes routines, running
    // unattended every morning, is a way to wake up owning forty of them --
    // and nobody asked for any of them, which is the opposite of what a
    // routine is. Asked for in a conversation, it is exactly right.
    if ctx.unattended {
        return Err(Error::Invalid(
            "a scheduled run may not create routines. If they should have one, say so in \
             your reply and they can set it up."
                .into(),
        ));
    }
    let trigger = schedule_from(args)?;
    let mut routine = Routine::new(args.str("name")?, args.str("instructions")?, trigger);
    if let Some(grace) = args.opt_u32("grace_minutes") {
        routine.grace_minutes = grace;
    }
    routine.validate()?;
    ctx.vault.save_routine(&routine)?;

    let now = ctx.now();
    let mut out = done("created", "routine", &routine.name, routine.id.to_string())?;
    // What it will actually do, so the reply can say when rather than
    // promising something vague.
    if let Some(map) = out.as_object_mut() {
        map.insert("when".into(), json!(routine.trigger.describe()));
        if let Some(next) = routine.next_due(&now) {
            map.insert("first_run".into(), json!(next.to_string()));
        }
    }
    Ok(out)
}

fn run_update_routine(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: RoutineId = args.id("routine_id", "routine")?;
    let mut routine = ctx.vault.routine(id)?;

    if let Some(name) = args.opt_str("name") {
        routine.name = name.to_string();
    }
    if let Some(instructions) = args.opt_str("instructions") {
        routine.instructions = instructions.to_string();
    }
    // A schedule is one value, so naming either half means reading both --
    // but *omitting* one must leave it alone. Moving a 07:00 weekday brief to
    // 08:00 with `at` alone used to reset it to every day, because
    // `schedule_from` reads an absent `days` as "no days named, so all of
    // them". Only a `days` the caller actually sent replaces the days.
    if args.get("at").is_some() {
        let named = schedule_from(args)?;
        routine.trigger = match (named, &routine.trigger) {
            (Trigger::Schedule { at, days }, Trigger::Schedule { days: was, .. })
                if args.get("days").is_none() =>
            {
                Trigger::Schedule { at, days: if days.is_empty() { was.clone() } else { days } }
            }
            (next, _) => next,
        };
    } else if args.get("days").is_some() {
        if let Trigger::Schedule { at, .. } = routine.trigger {
            routine.trigger = Trigger::Schedule { at, days: parse_days(args)? };
        } else {
            return Err(Error::Invalid(
                "update_routine: this routine does not run on a schedule, so it has no \
                 days. Give `at` as well to move it onto one."
                    .into(),
            ));
        }
    }
    if let Some(grace) = args.opt_u32("grace_minutes") {
        routine.grace_minutes = grace;
    }
    if let Some(enabled) = args.opt_bool("enabled") {
        routine.enabled = enabled;
    }
    routine.updated_at = jiff::Timestamp::now();
    routine.validate()?;
    ctx.vault.save_routine(&routine)?;

    let now = ctx.now();
    let mut out = done("updated", "routine", &routine.name, routine.id.to_string())?;
    if let Some(map) = out.as_object_mut() {
        map.insert("when".into(), json!(routine.trigger.describe()));
        if let Some(next) = routine.next_due(&now) {
            map.insert("next_run".into(), json!(next.to_string()));
        }
    }
    Ok(out)
}

fn run_delete_routine(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: RoutineId = args.id("routine_id", "routine")?;
    // Read it first, so the confirmation card and the reply can name what went.
    let routine = ctx.vault.routine(id)?;
    ctx.vault.delete_routine(id)?;
    done("deleted", "routine", &routine.name, id.to_string())
}

fn run_run_routine(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: RoutineId = args.id("routine_id", "routine")?;
    let routine = ctx.vault.routine(id)?;
    // Queued, not run. Runs happen one at a time, and a tool that blocked on a
    // second model call would be a turn waiting on a turn.
    let queued = ctx.vault.runs(&RunQuery::for_routine(id))?;
    let run = match queued.into_iter().find(|r| !r.outcome.is_finished()) {
        Some(already) => already,
        None => {
            let run = RoutineRun::new(&routine, None);
            ctx.vault.save_run(&run)?;
            run
        }
    };
    Ok(json!({
        "ok": true,
        "action": "queued",
        "kind": "run",
        "name": routine.name,
        "id": run.id.to_string(),
        "note": "it will start within a minute; you will not see the result in this reply",
    }))
}

fn run_list_runs(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let query = RunQuery {
        routine_id: args.opt_id("routine_id", "routine")?,
        limit: Some(args.limit()),
        ..Default::default()
    };
    let runs = ctx.vault.runs(&query)?;
    Ok(json!({
        "count": runs.len(),
        "runs": runs
            .iter()
            .map(|r| {
                let mut out = json!({
                    "id": r.id.to_string(),
                    "routine": r.routine_name,
                    "started": r.started_at.to_string(),
                    "outcome": r.outcome.as_str(),
                });
                let map = out.as_object_mut().expect("built as an object");
                if !r.summary.trim().is_empty() {
                    map.insert("said".into(), json!(r.summary));
                }
                if !r.reason.trim().is_empty() {
                    map.insert("reason".into(), json!(r.reason));
                }
                out
            })
            .collect::<Vec<_>>(),
    }))
}
