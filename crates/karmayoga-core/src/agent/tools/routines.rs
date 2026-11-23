//! The assistant's own standing work, and the log of what it did.

use serde_json::{Value, json};

use super::{
    Args, Built, Tool, ToolContext, delete_built, describe_by_id, done, empty_schema, flag,
    limit_arg, list, load_by_id, number, one_of, run_delete, schema, text,
};
use crate::brief::{BriefKind, BriefSection};
use crate::error::{Error, Result};
use crate::id::RoutineId;
use crate::proposal::{About, AboutKind, Payload, ProposalKind, ProposedRecord};
use crate::routine::{Routine, RoutineRun, Trigger, Weekday};
use crate::store::routines::RunQuery;
use crate::timestamped::Timestamped;

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
                ("send_to_chat", flag(SEND_TO_CHAT)),
                ("ask_in_chat", flag(ASK_IN_CHAT)),
                ("call_me", flag(CALL_ME)),
                ("email_me", flag(EMAIL_ME)),
                ("email_account", text(EMAIL_ACCOUNT)),
                ("model", one_of(MODEL, &MODELS)),
                ("max_steps", number(MAX_STEPS)),
            ],
            &["name", "instructions", "at"]
        ),
        "Set up standing work: something you will do on a schedule, without being asked. \
         Use this when they say they want something to happen regularly \u{2014} \"every \
         Sunday evening, plan my week\". Say what you have set up and when it will first \
         run; they can see it and change it in the Assistant app.",
        run_create_routine,
        None,
        Some(build_create_routine)
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
                ("send_to_chat", flag(SEND_TO_CHAT)),
                ("ask_in_chat", flag(ASK_IN_CHAT)),
                ("call_me", flag(CALL_ME)),
                ("email_me", flag(EMAIL_ME)),
                ("email_account", text(EMAIL_ACCOUNT)),
                ("model", one_of(MODEL, &MODELS)),
                ("max_steps", number(MAX_STEPS)),
            ],
            &["routine_id"]
        ),
        "Change a routine. Every field is optional and omitted fields are left alone. To \
         stop one without losing it, set enabled to false.",
        run_update_routine,
        None,
        Some(build_update_routine)
    ),
    tool!(
        "delete_routine",
        Destructive,
        Routines,
        schema(vec![("routine_id", text("Id of the routine to delete."))], &["routine_id"]),
        "Permanently delete a routine, its run log and the transcripts of those runs. \
         Anything it made \u{2014} notes, tasks \u{2014} is left alone. There is no undo.",
        run_delete_routine,
        Some(describe_delete_routine),
        Some(build_delete_routine)
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
    tool!(
        "set_brief",
        Write,
        Routines,
        schema(
            vec![
                (
                    "kind",
                    one_of(
                        "Which brief: morning (the day ahead), evening (the day's close and \
                         tomorrow), weekly (Sunday mornings: today and the week ahead) or \
                         podcast (a daily script to listen to, made from what arrived in their \
                         Feed, spoken into audio and put first in Up Next).",
                        &["morning", "evening", "weekly", "podcast"]
                    )
                ),
                ("on", flag("True to switch it on, false to switch it off.")),
            ],
            &["kind", "on"]
        ),
        "Switch one of the person's daily briefs on or off. A brief is set up by the app, \
         not written as a routine: use this rather than create_routine when they ask for a \
         morning, evening or weekly brief, or a daily podcast. Its time, where it is sent and \
         its routine id come back; change the time or delivery with update_routine, and what \
         it covers (for the podcast: which feeds and how long) in Settings \u{2192} Briefs.",
        run_set_brief
    ),
    tool!(
        "get_brief",
        Read,
        Routines,
        schema(
            vec![(
                "kind",
                one_of(
                    "Which brief: the morning one, the evening one, the weekly one \
                     (Sundays: today and the week ahead) or the daily podcast. Omit for the \
                     newest of any.",
                    &["morning", "evening", "weekly", "podcast"]
                )
            )],
            &[]
        ),
        "The person's latest daily brief, as written: the headline and each section, with \
         the day it was made. Use it to answer \"what's on my brief\" or a question about \
         something in it. If it is from an earlier day, say so. To have it read out in a \
         spoken conversation, use read_brief instead.",
        run_get_brief
    ),
];

const SEND_TO_CHAT: &str = "Also send the result to them in their chat (Matrix, through the bot \
     they reach you by), as well as keeping it in the app. Only if they have a chat link set \
     up. False stops sending it there.";

const ASK_IN_CHAT: &str = "While it runs, put questions to them in that chat: approvals for \
     anything that deletes or sends, and a question when it needs a fact or a decision only \
     they have. Sends the result there too. False stops asking.";

const CALL_ME: &str = "Call them with the result through that chat instead of writing it: you \
     say it, and stay on the line for anything they ask about it. Written in the chat only if \
     they do not pick up. Turns sending to the chat on. For a brief they asked to hear, or to \
     be called with. False stops calling.";

const EMAIL_ME: &str = "Also email the result to them, from one of their mail accounts to its \
     own address. False stops emailing it.";

const EMAIL_ACCOUNT: &str = "Which of their mail accounts to email from, by address. Omit for \
     the first one.";

const MODEL: &str = "Which of their models it runs on, as set up in Settings \u{2192} Models: \
     large for work that needs the most thought, medium (the assistant's own, and the default) \
     for most, small for something quick and simple. Only if they ask.";

const MODELS: [&str; 3] = ["large", "medium", "small"];

const MAX_STEPS: &str = "The most steps a run may take, 1 to 100 -- each step is one turn of \
     yours, which may call tools. Omit for the assistant's own limit; 0 goes back to it. Only \
     if they ask, or the work plainly needs many reads.";

fn describe_delete_routine(ctx: &ToolContext<'_>, args: &Args<'_>) -> Option<String> {
    describe_by_id::<RoutineId, Routine>(
        args,
        "routine_id",
        "routine",
        |id| ctx.vault.routine(id),
        |r| r.name,
    )
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
    if let Some(tier) = r.model {
        map.insert("model".into(), json!(tier));
    }
    if let Some(steps) = r.max_steps {
        map.insert("max_steps".into(), json!(steps));
    }
    if !r.delivery.is_empty() {
        map.insert(
            "delivery".into(),
            json!({
                "chat": r.delivery.chat.is_some(),
                "asks_in_chat": r.delivery.asks(),
                "calls": r.delivery.calls(),
                "email": r.delivery.email.is_some(),
            }),
        );
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
    Ok(Trigger::Schedule { at, days: parse_days(args)?, day_of_month: None })
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

/// Everything `create_routine` does to build the record, without saving it
/// or checking who is asking -- the half `run_create_routine` and
/// `build_create_routine` share. The unattended refusal lives in
/// `run_create_routine` alone: drafting mode exists precisely to bypass it,
/// the way `docs/plans/dreaming.md` says a proposal is the safe form of the
/// call it stands in for.
fn routine_from_create_args(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Routine> {
    let trigger = schedule_from(args)?;
    let mut routine = Routine::new(args.str("name")?, args.str("instructions")?, trigger);
    if let Some(grace) = args.opt_u32("grace_minutes") {
        routine.grace_minutes = grace;
    }
    apply_delivery_args(ctx, args, &mut routine.delivery)?;
    apply_how_args(args, &mut routine)?;
    routine.validate()?;
    Ok(routine)
}

/// `model` and `max_steps`, applied to a routine. Omitted, each is left
/// alone; `max_steps` of nought puts the assistant's own limit back.
fn apply_how_args(args: &Args<'_>, routine: &mut Routine) -> Result<()> {
    if let Some(raw) = args.opt_str("model") {
        let tier = match raw.trim().to_ascii_lowercase().as_str() {
            "large" => crate::llm::ModelTier::Large,
            "medium" => crate::llm::ModelTier::Medium,
            "small" => crate::llm::ModelTier::Small,
            _ => {
                return Err(args.bad(format!("{raw:?} is not a model: use large, medium or small")));
            }
        };
        routine.model = Some(tier);
    }
    if let Some(steps) = args.opt_u32("max_steps") {
        routine.max_steps = (steps > 0).then_some(steps);
    }
    Ok(())
}

/// `send_to_chat`, `ask_in_chat`, `call_me`, `email_me` and `email_account`, applied to
/// what a routine already has. An omitted flag leaves its part alone, so the
/// same function serves a new routine (which has nothing) and a change.
///
/// The model names no ids: the chat is their first switched-on link, and the
/// account is found by its address. Either missing is said plainly, so the
/// reply can tell them what to set up rather than claiming it is done.
fn apply_delivery_args(
    ctx: &ToolContext<'_>,
    args: &Args<'_>,
    delivery: &mut crate::routine::Delivery,
) -> Result<()> {
    let ask = args.opt_bool("ask_in_chat");
    let call = args.opt_bool("call_me");
    let chat = match (args.opt_bool("send_to_chat"), ask, call) {
        // Asking and calling happen through the chat, so asking for either is
        // asking for the chat.
        (_, Some(true), _) | (_, _, Some(true)) | (Some(true), _, _) => Some(true),
        (Some(false), _, _) => Some(false),
        (None, _, _) => None,
    };
    match chat {
        Some(true) if delivery.chat.is_none() => {
            let link = ctx
                .vault
                .chat_settings()
                .ok()
                .and_then(|s| s.links.into_iter().find(|l| l.enabled))
                .ok_or_else(|| {
                    Error::Invalid(
                        "they have no chat link switched on, so there is nowhere to send it. \
                         One is set up in Settings, under Chat."
                            .into(),
                    )
                })?;
            delivery.chat = Some(link.id);
        }
        Some(false) => {
            delivery.chat = None;
            delivery.ask = false;
            delivery.call = false;
        }
        _ => {}
    }
    if let Some(ask) = ask {
        delivery.ask = ask;
    }
    if let Some(call) = call {
        delivery.call = call;
    }

    let account = args.opt_str("email_account").map(str::trim).filter(|a| !a.is_empty());
    match (args.opt_bool("email_me"), account) {
        (Some(false), _) => delivery.email = None,
        (Some(true), _) | (None, Some(_)) => {
            let accounts: Vec<_> =
                ctx.vault.accounts()?.into_iter().filter(|a| a.services.mail).collect();
            let chosen = match account {
                Some(address) => accounts.iter().find(|a| {
                    a.address.eq_ignore_ascii_case(address)
                        || a.identities.iter().any(|i| i.address.eq_ignore_ascii_case(address))
                }),
                None => accounts.first(),
            };
            let chosen = chosen.ok_or_else(|| {
                Error::Invalid(match account {
                    Some(address) => format!(
                        "{address:?} is not one of their mail accounts. Leave email_account \
                         out to use the first."
                    ),
                    None => "they have no mail account to send it from.".into(),
                })
            })?;
            delivery.email = Some(chosen.id);
        }
        (None, None) => {}
    }
    Ok(())
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
    let routine = routine_from_create_args(ctx, args)?;
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

fn build_create_routine(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Built> {
    let routine = routine_from_create_args(ctx, args)?;
    let caption = format!("Create routine: {}", routine.name);
    Ok(Built {
        payload: Payload::Create { record: ProposedRecord::Routine(routine) },
        caption,
        about: None,
    })
}

/// Everything `update_routine` does to the loaded record, without saving it
/// -- the half `run_update_routine` and `build_update_routine` share.
fn apply_update_routine_args(
    ctx: &ToolContext<'_>,
    args: &Args<'_>,
    mut routine: Routine,
) -> Result<Routine> {
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
            (
                Trigger::Schedule { at, days, .. },
                Trigger::Schedule { days: was, day_of_month, .. },
            ) if args.get("days").is_none() => Trigger::Schedule {
                at,
                days: if days.is_empty() { was.clone() } else { days },
                day_of_month: *day_of_month,
            },
            (next, _) => next,
        };
    } else if args.get("days").is_some() {
        if let Trigger::Schedule { at, day_of_month, .. } = routine.trigger {
            routine.trigger = Trigger::Schedule { at, days: parse_days(args)?, day_of_month };
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
    apply_delivery_args(ctx, args, &mut routine.delivery)?;
    apply_how_args(args, &mut routine)?;
    routine.touch();
    routine.validate()?;
    Ok(routine)
}

/// The arguments that change what a routine does, where its results go, or
/// what it spends doing it, rather than when it runs.
const WHAT_AND_WHERE: [&str; 8] = [
    "instructions",
    "send_to_chat",
    "ask_in_chat",
    "call_me",
    "email_me",
    "email_account",
    "model",
    "max_steps",
];

fn run_update_routine(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    // `create_routine`'s refusal, for the half of an update that amounts to
    // writing a new routine. A run's instructions are what the next run takes
    // as the person's own words, addresses and all, so a scheduled run that
    // has read a stranger's page or message must not be able to write them
    // -- that would hand the next run whatever the stranger asked for, with
    // nobody having seen it. Nor what it runs on and for how long, which is
    // the person's money. Moving it, pausing it or renaming it is still fine.
    if ctx.unattended && WHAT_AND_WHERE.iter().any(|field| args.get(field).is_some()) {
        return Err(Error::Invalid(
            "a scheduled run may not change a routine's instructions, where its results go, or \
             the model and steps it runs with. If it should change, say so in your reply and \
             they can change it."
                .into(),
        ));
    }
    let routine: Routine = load_by_id(args, "routine_id", "routine", |id| ctx.vault.routine(id))?;
    let routine = apply_update_routine_args(ctx, args, routine)?;
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

/// A dream proposing to change itself is refused rather than parked as a
/// proposal: its schedule and grace are the person's to edit directly (see
/// `docs/plans/dreaming.md`'s Phase 3), and a routine's own run is not the
/// person who should be asked to approve a change to it.
fn refuse_if_dream(routine: &Routine) -> Result<()> {
    if routine.kind.is_dream() {
        return Err(Error::Invalid(
            "a dream's schedule and grace can be changed directly in Settings; it cannot be \
             proposed as a change for somebody to accept."
                .into(),
        ));
    }
    Ok(())
}

fn build_update_routine(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Built> {
    let original: Routine = load_by_id(args, "routine_id", "routine", |id| {
        let r = ctx.vault.routine(id)?;
        refuse_if_dream(&r)?;
        Ok(r)
    })?;
    let expected_updated_at = original.updated_at;
    let routine = apply_update_routine_args(ctx, args, original)?;
    let caption = format!("Change routine: {}", routine.name);
    let about = About { kind: AboutKind::Routine, id: routine.id.to_string() };
    Ok(Built {
        payload: Payload::Replace { record: ProposedRecord::Routine(routine), expected_updated_at },
        caption,
        about: Some(about),
    })
}

/// A dream cannot be deleted by hand, in either mode: it is set up by the
/// application when dreaming is switched on, and the way to remove it is to
/// switch dreaming off, not to delete one of the three routines it made.
///
/// A brief likewise: it is switched on and off in Settings, under Briefs.
fn refuse_delete_if_dream(routine: &Routine) -> Result<()> {
    if routine.kind.is_dream() {
        return Err(Error::Invalid(
            "a dream is set up by the application when dreaming is switched on and cannot be \
             deleted on its own; turn dreaming off in Settings to remove it."
                .into(),
        ));
    }
    if routine.kind.brief().is_some() {
        return Err(Error::Invalid(
            "a brief is set up by the application and cannot be deleted; switch it off in \
             Settings \u{2192} Briefs instead."
                .into(),
        ));
    }
    Ok(())
}

fn run_delete_routine(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    // Read it first, so the confirmation card and the reply can name what went.
    run_delete::<RoutineId, Routine>(
        args,
        "routine_id",
        "routine",
        |id| {
            let r = ctx.vault.routine(id)?;
            refuse_delete_if_dream(&r)?;
            Ok(r)
        },
        |id| ctx.vault.delete_routine(id),
        |routine| routine.name,
    )
}

fn build_delete_routine(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Built> {
    delete_built::<RoutineId, Routine>(
        args,
        "routine_id",
        "routine",
        ProposalKind::Routine,
        AboutKind::Routine,
        |id| {
            let r = ctx.vault.routine(id)?;
            refuse_delete_if_dream(&r)?;
            Ok(r)
        },
        |routine: Routine| format!("Delete routine: {}", routine.name),
    )
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

fn run_set_brief(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let kind = args.opt_str("kind").and_then(BriefKind::parse).ok_or_else(|| {
        Error::Invalid("set_brief: kind is morning, evening, weekly or podcast.".into())
    })?;
    let on = args.opt_bool("on").ok_or_else(|| Error::Invalid("set_brief: say on.".into()))?;
    let routine = ctx.vault.set_brief(kind, on)?;
    let mut out = done(
        if on { "switched on" } else { "switched off" },
        "routine",
        &routine.name,
        routine.id.to_string(),
    )?;
    if let Some(map) = out.as_object_mut() {
        map.insert("when".into(), json!(routine.trigger.describe()));
        if kind == BriefKind::Weekly && on {
            map.insert(
                "note".into(),
                json!("on its day it takes the morning brief's place, so there is one brief"),
            );
        }
        if kind == BriefKind::Podcast && on {
            map.insert(
                "note".into(),
                json!(
                    "it is made from what has arrived in their Feed since the last one, \
                     spoken into audio and put first in Up Next; which feeds and how long it \
                     runs are set in Settings \u{2192} Briefs"
                ),
            );
        }
    }
    Ok(out)
}

/// The newest brief, or the newest of one kind. Withheld from a caller kept
/// from any private domain, as a run's own words are in `list_runs`: a brief
/// quotes mail, meetings and targets.
fn run_get_brief(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    if !ctx.withheld.is_empty() {
        return Err(Error::Invalid(
            "get_brief: the brief is not available to background work.".into(),
        ));
    }
    let kind = match args.opt_str("kind") {
        Some(k) => Some(BriefKind::parse(k).ok_or_else(|| {
            Error::Invalid(
                "get_brief: kind is \"morning\", \"evening\", \"weekly\" or \"podcast\".".into(),
            )
        })?),
        None => None,
    };
    let Some(run) = crate::brief::newest(ctx.vault, kind)? else {
        let which = kind.map(|k| k.name().to_lowercase()).unwrap_or_else(|| "brief".into());
        return Err(Error::Invalid(format!(
            "get_brief: there is no {which} yet. Briefs are switched on in Settings \u{2192} \
             Briefs; one that is on can be made now with run_routine."
        )));
    };
    let mut brief = run.brief.clone().expect("briefs() keeps only runs with one");
    // Its mail section names who wrote and about what, so it reaches no
    // caller that could not read that mail itself -- a model the person has
    // not shared their mail with, or an MCP client without mail.
    let may_read_mail = super::mail::offered_to(
        ctx.vault,
        "read_thread",
        ctx.caller.as_ref(),
        ctx.assistant_provider.as_deref(),
    );
    let mut note = None;
    if !may_read_mail && brief.parts.iter().any(|p| p.section == Some(BriefSection::Mail)) {
        brief.parts.retain(|p| p.section != Some(BriefSection::Mail));
        note = Some("Its mail section is left out: mail is not shared with you.");
    }
    // The podcast has no mail section: its newsletters are told among the
    // rest, so a caller that may not read mail gets its headline and nothing
    // of what it says when any of it came from there.
    if !may_read_mail
        && brief.digest.podcast.as_ref().is_some_and(|p| {
            p.stories.iter().any(|s| s.from == crate::brief::ExcerptFrom::Newsletter)
        })
    {
        brief.parts.clear();
        note = Some(
            "Its topics are left out: it draws on newsletters from mail, which is not shared with you.",
        );
    }
    let mut out = json!({
        "kind": brief.kind.as_str(),
        "day": brief.day.to_string(),
        "made_at": run.finished_at.unwrap_or(run.started_at).to_string(),
        "today": brief.day == ctx.today,
        "headline": brief.headline,
        "brief": brief.markdown(),
    });
    if let (Some(note), Some(map)) = (note, out.as_object_mut()) {
        map.insert("note".into(), json!(note));
    }
    Ok(out)
}

fn run_list_runs(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let query = RunQuery {
        routine_id: args.opt_id("routine_id", "routine")?,
        limit: Some(args.limit()),
        ..Default::default()
    };
    let runs = ctx.vault.runs(&query)?;
    // What a run said is the assistant's own writing, run as the person with
    // every tool there is: it may quote their money, their body, their files.
    // A caller that may not see one of those gets that a run happened, when
    // and how it ended, and nothing of what it said.
    let withheld = !ctx.withheld.is_empty();
    let mut out = json!({
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
                if !withheld && !r.summary.trim().is_empty() {
                    map.insert("said".into(), json!(r.summary));
                }
                if !withheld && !r.reason.trim().is_empty() {
                    map.insert("reason".into(), json!(r.reason));
                }
                out
            })
            .collect::<Vec<_>>(),
    });
    if withheld {
        out["withheld"] = json!(ctx.withheld_note());
    }
    Ok(out)
}
