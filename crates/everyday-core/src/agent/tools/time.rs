//! Calendars read from elsewhere, and the time somebody sets aside or
//! records for themselves.
//!
//! The calendar domain is the smallest one on purpose: other people's time
//! is read-only here, and a person's own is already a [`TimeBlock`] --
//! [`crate::task`]'s domain, not a storage layer of this one's own. This
//! file is where both kinds of time meet a model.

use serde_json::{Value, json};

use super::{
    Args, Built, Tool, ToolContext, Window, day, done, limit_arg, load_by_id, one_of,
    resolve_purpose, run_delete, schema, text,
};
use crate::error::Result;
use crate::id::{BlockId, ProjectId, TaskId};
use crate::proposal::{About, AboutKind, Payload, ProposalKind, ProposedRecord};
use crate::store::calendars::EventQuery;
use crate::store::tasks::BlockQuery;
use crate::task::{BlockKind, BlockSubject, TimeBlock};

pub(super) static TOOLS: &[Tool] = &[
    tool!(
        "list_events",
        Read,
        Calendars,
        schema(
            vec![
                ("from", day("Start of the window, inclusive. Defaults to today.")),
                ("to", day("End of the window, inclusive. Defaults to a week out.")),
                limit_arg(),
            ],
            &[]
        ),
        "Calendar events in a date window, from subscribed calendars. These are \
         read-only: this app subscribes to other people's feeds and cannot write \
         to them. To schedule your own time, use create_time_block.",
        run_list_events
    ),
    tool!(
        "list_time_blocks",
        Read,
        Tasks,
        schema(
            vec![
                ("from", day("Start of the window, inclusive. Defaults to today.")),
                ("to", day("End of the window, inclusive. Defaults to a week out.")),
                ("task_id", text("Only blocks against this task.")),
                limit_arg(),
            ],
            &[]
        ),
        "Blocks of time in a window: planned ones (what you set aside) and actual \
         ones (what it took). The way to answer 'how long did that take'.",
        run_list_blocks
    ),
    tool!(
        "create_time_block",
        Write,
        Tasks,
        schema(
            vec![
                ("date", day("The day it falls on.")),
                ("start_time", text("Start, as HH:MM in 24-hour time.")),
                ("end_time", text("End, as HH:MM. Must be after the start.")),
                ("task_id", text("Which task this is time for.")),
                ("project_id", text("Or which project, if it is not one task.")),
                ("label", text("Or a free-text label, if it is neither.")),
                (
                    "kind",
                    one_of(
                        "planned (setting time aside) or actual (recording what it took). Defaults to planned.",
                        &["planned", "actual"]
                    )
                ),
                ("notes", text("Anything worth noting.")),
                ("goal_id", text("File it under this goal, from list_goals.")),
                ("role_id", text("Or under this role directly, from list_roles.")),
            ],
            &["date", "start_time", "end_time"]
        ),
        "Set aside time for a task, or record time that was spent. Give exactly one \
         of task_id, project_id or label to say what the time is for.",
        run_create_block,
        None,
        Some(build_create_block)
    ),
    tool!(
        "update_time_block",
        Write,
        Tasks,
        schema(
            vec![
                ("block_id", text("Id from list_time_blocks.")),
                ("date", day("Move it to this day. Omitted, it stays where it is.")),
                ("start_time", text("New start, as HH:MM. Omitted, it stays where it is.")),
                ("end_time", text("New end, as HH:MM. Must be after the start.")),
                ("task_id", text("Move it onto this task instead.")),
                ("project_id", text("Or onto this project instead.")),
                ("label", text("Or give it this free-text label instead.")),
                ("goal_id", text("File it under this goal, from list_goals.")),
                ("role_id", text("Or under this role directly, from list_roles.")),
            ],
            &["block_id"]
        ),
        "Move a block of time to a new day or time, or change what it is for -- the one call \
         instead of deleting it and creating another. Every field but block_id is optional \
         and anything left out stays as it is; give exactly one of task_id, project_id or \
         label if you are changing what the time is for.",
        run_update_block,
        None,
        Some(build_update_block)
    ),
    tool!(
        "delete_time_block",
        Destructive,
        Tasks,
        schema(vec![("block_id", text("Id from list_time_blocks."))], &["block_id"]),
        "Permanently delete a block of time.",
        run_delete_block,
        Some(describe_delete_time_block),
        Some(build_delete_time_block)
    ),
];

fn describe_delete_time_block(ctx: &ToolContext<'_>, args: &Args<'_>) -> Option<String> {
    let id: BlockId = args.opt_id("block_id", "time block").ok()??;
    let block = ctx.vault.block(id).ok()?;
    // A block has no name of its own unless it is ad-hoc, so it is
    // described by what it is for and when -- which is what somebody
    // needs in order to recognise it.
    let subject = match &block.subject {
        BlockSubject::Task { id } => ctx.vault.task(*id).ok().map(|t| t.title),
        BlockSubject::Project { id } => ctx.vault.project(*id).ok().map(|p| p.name),
        BlockSubject::Adhoc => Some(block.title.clone()).filter(|t| !t.is_empty()),
    };
    Some(match subject {
        Some(what) => format!("{what} on {}", block.local_date),
        None => format!("the block on {}", block.local_date),
    })
}

fn run_list_events(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let (from, to) = args.window(ctx, Window::Ahead(7))?;
    let mut rows = ctx.vault.events(&EventQuery::between(from, to))?;
    rows.truncate(args.limit() as usize);
    Ok(json!({
        "count": rows.len(),
        "from": from.to_string(),
        "to": to.to_string(),
        "events": rows
            .iter()
            .map(|e| json!({
                "id": e.id.to_string(),
                "title": e.title,
                "date": e.local_date.to_string(),
                "all_day": e.all_day,
                "starts": e.start.to_string(),
                "ends": e.end.to_string(),
                "location": e.location,
            }))
            .collect::<Vec<_>>(),
    }))
}

fn run_list_blocks(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let (from, to) = args.window(ctx, Window::Ahead(7))?;
    let mut query = BlockQuery::between(from, to);
    if let Some(id) = args.opt_id::<TaskId>("task_id", "task")? {
        query.task_id = Some(id);
    }
    let mut rows = ctx.vault.blocks(&query)?;
    rows.truncate(args.limit() as usize);

    Ok(json!({
        "count": rows.len(),
        "blocks": rows
            .iter()
            .map(|b| json!({
                "id": b.id.to_string(),
                "date": b.local_date.to_string(),
                "starts": b.start.to_string(),
                "ends": b.end.to_string(),
                "minutes": b.minutes(),
                "kind": b.kind.as_str(),
                "for": block_subject_label(b),
            }))
            .collect::<Vec<_>>(),
    }))
}

fn block_subject_label(b: &TimeBlock) -> Value {
    match &b.subject {
        BlockSubject::Task { id } => json!({ "task_id": id.to_string() }),
        BlockSubject::Project { id } => json!({ "project_id": id.to_string() }),
        // Time that belongs to no record carries its own name in `title`,
        // which is the only thing that identifies it.
        BlockSubject::Adhoc => json!({ "label": b.title }),
    }
}

/// Exactly one of `task_id`, `project_id` or `label`, read off a call --
/// shared by `create_time_block`, which must be given one, and
/// `update_time_block`, which is free to give none. `Ok(None)` is "none of
/// the three was named", which the two tools read differently; anything
/// beyond one is always a mistake, whichever is asking, since a block for
/// both a task and a project is not a thing this domain has.
fn subject_from_args(args: &Args<'_>) -> Result<Option<(BlockSubject, Option<String>)>> {
    let task: Option<TaskId> = args.opt_id("task_id", "task")?;
    let project: Option<ProjectId> = args.opt_id("project_id", "project")?;
    let label = args.opt_str("label");
    match (task, project, label) {
        (Some(id), None, None) => Ok(Some((BlockSubject::Task { id }, None))),
        (None, Some(id), None) => Ok(Some((BlockSubject::Project { id }, None))),
        (None, None, Some(text)) => Ok(Some((BlockSubject::Adhoc, Some(text.to_string())))),
        (None, None, None) => Ok(None),
        _ => Err(args.bad("give only one of `task_id`, `project_id` or `label`")),
    }
}

/// What a block's subject is called: the task's title, the project's name,
/// or -- for an ad-hoc block -- its own label. Resolved eagerly, before a
/// block is built or changed, so the reply can say what the time is for and
/// so a subject naming a task or project that does not exist fails here
/// rather than becoming an hour against nothing.
///
/// `describe_delete_time_block` asks the same question about a block that
/// may already be in that state and settles for silence; this always either
/// answers or fails, which is the right trade for a call still choosing
/// what to build.
fn subject_name(
    ctx: &ToolContext<'_>,
    subject: &BlockSubject,
    label: Option<&str>,
) -> Result<String> {
    Ok(match subject {
        BlockSubject::Task { id } => ctx.vault.task(*id)?.title,
        BlockSubject::Project { id } => ctx.vault.project(*id)?.name,
        BlockSubject::Adhoc => label.unwrap_or_default().to_string(),
    })
}

/// The absolute instants `start`/`end` resolve to on `date`, in zone `tz` --
/// shared by `block_from_args` and `apply_update_block_args` once each has
/// worked out which day and which two clock times actually apply.
///
/// Local wall-clock times, resolved in the person's zone. A block is stored
/// as two absolute instants -- so that durations and overlaps are
/// unambiguous -- but "two till three" means two till three where they are
/// sitting, not in UTC.
fn block_timestamps(
    args: &Args<'_>,
    tz: &str,
    date: jiff::civil::Date,
    start: jiff::civil::Time,
    end: jiff::civil::Time,
) -> Result<(jiff::Timestamp, jiff::Timestamp)> {
    let zone = jiff::tz::TimeZone::get(tz).unwrap_or(jiff::tz::TimeZone::UTC);
    let start_ts = date
        .at(start.hour(), start.minute(), 0, 0)
        .to_zoned(zone.clone())
        .map_err(|e| args.bad(format!("{date} {start} does not exist in {tz}: {e}")))?
        .timestamp();
    let end_ts = date
        .at(end.hour(), end.minute(), 0, 0)
        .to_zoned(zone)
        .map_err(|e| args.bad(format!("{date} {end} does not exist in {tz}: {e}")))?
        .timestamp();
    if end_ts <= start_ts {
        return Err(args.bad("`end_time` must be after `start_time`"));
    }
    Ok((start_ts, end_ts))
}

/// Everything `create_time_block` does to build the record, without saving
/// it -- the half `run_create_block` and `build_create_block` share. Returns
/// the block and the name of what it is for, which both the plain reply and
/// a proposal's caption need.
fn block_from_args(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<(TimeBlock, String)> {
    let date = args.date("date")?;
    let start = clock(args, "start_time")?;
    let end = clock(args, "end_time")?;

    // Exactly one subject, checked here rather than left to the model's
    // reading of the description: a block for both a task and a project is
    // not a thing this domain has, and silently preferring one would put the
    // time against something nobody chose.
    let (subject, label) = subject_from_args(args)?
        .ok_or_else(|| args.bad("give one of `task_id`, `project_id` or `label`"))?;
    let name = subject_name(ctx, &subject, label.as_deref())?;

    let kind = match args.opt_str("kind") {
        Some("actual") => BlockKind::Actual,
        Some("planned") | None => BlockKind::Planned,
        Some(other) => {
            return Err(args.bad(format!("`kind` must be planned or actual, got {other:?}")));
        }
    };

    let (start_ts, end_ts) = block_timestamps(args, ctx.tz, date, start, end)?;
    let minutes = u32::try_from((end_ts.as_second() - start_ts.as_second()) / 60).unwrap_or(0);

    let mut block = TimeBlock::new(subject, start_ts, minutes, ctx.tz);
    block.kind = kind;
    block.title = label.unwrap_or_default().to_string();
    block.notes = args.opt_str("notes").unwrap_or_default().to_string();
    block.purpose = resolve_purpose(ctx, args)?;

    Ok((block, name))
}

/// What a task-linked block's proposal is "about" -- the task, not the
/// block, since the task is what a person recognises and what the ghost
/// should sit beside. `None` for a block with no such subject.
fn task_about(block: &TimeBlock) -> Option<About> {
    match &block.subject {
        BlockSubject::Task { id } => Some(About { kind: AboutKind::Task, id: id.to_string() }),
        BlockSubject::Project { .. } | BlockSubject::Adhoc => None,
    }
}

/// "Thu" for the weekday a caption reads for a proposed block -- a plain
/// match rather than a format string, since [`jiff::civil::Weekday`] has no
/// short `Display` of its own and this file has no other use for one.
fn weekday_abbrev(date: jiff::civil::Date) -> &'static str {
    use jiff::civil::Weekday;
    match date.weekday() {
        Weekday::Monday => "Mon",
        Weekday::Tuesday => "Tue",
        Weekday::Wednesday => "Wed",
        Weekday::Thursday => "Thu",
        Weekday::Friday => "Fri",
        Weekday::Saturday => "Sat",
        Weekday::Sunday => "Sun",
    }
}

fn run_create_block(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let (block, name) = block_from_args(ctx, args)?;
    let date = block.local_date;
    ctx.vault.save_block(&block)?;
    done("created", "time block", &format!("{name} on {date}"), block.id.to_string())
}

fn build_create_block(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Built> {
    let (block, name) = block_from_args(ctx, args)?;
    let about = task_about(&block);
    let zone = jiff::tz::TimeZone::get(ctx.tz).unwrap_or(jiff::tz::TimeZone::UTC);
    let start = block.start.to_zoned(zone.clone()).time();
    let end = block.end.to_zoned(zone).time();
    let caption = format!(
        "Plan time {} {:02}:{:02}\u{2013}{:02}:{:02}: {name}",
        weekday_abbrev(block.local_date),
        start.hour(),
        start.minute(),
        end.hour(),
        end.minute(),
    );
    Ok(Built { payload: Payload::Create { record: ProposedRecord::Block(block) }, caption, about })
}

fn clock(args: &Args<'_>, key: &str) -> Result<jiff::civil::Time> {
    let raw = args.str(key)?;
    raw.trim()
        .parse::<jiff::civil::Time>()
        // `09:00` is what the schema asks for and what models send; the
        // parser wants seconds, so the common form is tried with them added.
        .or_else(|_| format!("{}:00", raw.trim()).parse::<jiff::civil::Time>())
        .map_err(|_| args.bad(format!("`{key}` must be a time like 09:30, got {raw:?}")))
}

/// Everything `update_time_block` does to the loaded record, without saving
/// it -- the half `run_update_block` and `build_update_block` share, on the
/// same terms `apply_update_routine_args` already keeps for a routine. Only
/// a field actually named in `args` changes; a schedule is one pair of
/// instants, though, so naming any one of `date`, `start_time` or
/// `end_time` means re-deriving both -- the two left unsaid are read back
/// off the block exactly as it already was, so omitting all three leaves it
/// exactly where it was. Returns the block and the name of what it is for:
/// the new one, when the subject changed, otherwise the one it already had.
fn apply_update_block_args(
    ctx: &ToolContext<'_>,
    args: &Args<'_>,
    mut block: TimeBlock,
) -> Result<(TimeBlock, String)> {
    let name = match subject_from_args(args)? {
        Some((subject, label)) => {
            let name = subject_name(ctx, &subject, label.as_deref())?;
            block.subject = subject;
            block.title = label.unwrap_or_default();
            name
        }
        None => subject_name(ctx, &block.subject, Some(block.title.as_str()))?,
    };

    // Only a call that names a day or a time moves the block. Recomputing
    // the instants from unchanged inputs would not be a no-op for a block
    // filed in another zone -- one planned while travelling -- since its
    // wall-clock times would be re-read in the person's zone today.
    let retimed = ["date", "start_time", "end_time"].iter().any(|k| args.opt_str(k).is_some());
    if retimed {
        let zone = jiff::tz::TimeZone::get(ctx.tz).unwrap_or(jiff::tz::TimeZone::UTC);
        let date = args.opt_date("date")?.unwrap_or(block.local_date);
        let start = match args.opt_str("start_time") {
            Some(_) => clock(args, "start_time")?,
            None => block.start.to_zoned(zone.clone()).time(),
        };
        let end = match args.opt_str("end_time") {
            Some(_) => clock(args, "end_time")?,
            None => block.end.to_zoned(zone).time(),
        };
        let (start_ts, end_ts) = block_timestamps(args, ctx.tz, date, start, end)?;
        block.start = start_ts;
        block.end = end_ts;
        block.local_date = date;
        block.tz = ctx.tz.to_string();
    }

    if let Some(purpose) = resolve_purpose(ctx, args)? {
        block.purpose = Some(purpose);
    }

    // `TimeBlock` carries no `Timestamped` impl of its own to `touch()` --
    // unlike a task or an item, nothing else stamps this for a plain
    // `save_block`, so it is done by hand here.
    block.updated_at = jiff::Timestamp::now();
    Ok((block, name))
}

fn run_update_block(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let block: TimeBlock = load_by_id(args, "block_id", "time block", |id| ctx.vault.block(id))?;
    let (block, name) = apply_update_block_args(ctx, args, block)?;
    let date = block.local_date;
    ctx.vault.save_block(&block)?;
    done("updated", "time block", &format!("{name} on {date}"), block.id.to_string())
}

fn build_update_block(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Built> {
    let block: TimeBlock = load_by_id(args, "block_id", "time block", |id| ctx.vault.block(id))?;
    let expected_updated_at = block.updated_at;
    let (block, name) = apply_update_block_args(ctx, args, block)?;
    let id = block.id;
    let about = task_about(&block).or(Some(About { kind: AboutKind::Block, id: id.to_string() }));
    let zone = jiff::tz::TimeZone::get(ctx.tz).unwrap_or(jiff::tz::TimeZone::UTC);
    let start = block.start.to_zoned(zone.clone()).time();
    let end = block.end.to_zoned(zone).time();
    let caption = format!(
        "Move block to {} {:02}:{:02}\u{2013}{:02}:{:02}: {name}",
        weekday_abbrev(block.local_date),
        start.hour(),
        start.minute(),
        end.hour(),
        end.minute(),
    );
    Ok(Built {
        payload: Payload::Replace { record: ProposedRecord::Block(block), expected_updated_at },
        caption,
        about,
    })
}

fn run_delete_block(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    run_delete::<BlockId, TimeBlock>(
        args,
        "block_id",
        "time block",
        |id| ctx.vault.block(id),
        |id| ctx.vault.delete_block(id),
        |block| block.local_date.to_string(),
    )
}

fn build_delete_time_block(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Built> {
    let id: BlockId = args.id("block_id", "time block")?;
    let block = ctx.vault.block(id)?;
    let desc = describe_delete_time_block(ctx, args).unwrap_or_else(|| "the block".to_string());
    let about = task_about(&block).or(Some(About { kind: AboutKind::Block, id: id.to_string() }));
    Ok(Built {
        payload: Payload::Delete { kind: ProposalKind::Block, id: id.to_string() },
        caption: format!("Delete block: {desc}"),
        about,
    })
}
