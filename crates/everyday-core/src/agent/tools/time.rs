//! Calendars read from elsewhere, and the time somebody sets aside or
//! records for themselves.
//!
//! The calendar domain is the smallest one on purpose: other people's time
//! is read-only here, and a person's own is already a [`TimeBlock`] --
//! [`crate::task`]'s domain, not a storage layer of this one's own. This
//! file is where both kinds of time meet a model.

use serde_json::{Value, json};

use super::{Args, Tool, ToolContext, Window, day, done, limit_arg, one_of, schema, text};
use crate::error::Result;
use crate::id::{BlockId, ProjectId, TaskId};
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
            ],
            &["date", "start_time", "end_time"]
        ),
        "Set aside time for a task, or record time that was spent. Give exactly one \
         of task_id, project_id or label to say what the time is for.",
        run_create_block
    ),
    tool!(
        "delete_time_block",
        Destructive,
        Tasks,
        schema(vec![("block_id", text("Id from list_time_blocks."))], &["block_id"]),
        "Permanently delete a block of time.",
        run_delete_block,
        Some(describe_delete_time_block)
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

fn run_create_block(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let date = args.date("date")?;
    let start = clock(args, "start_time")?;
    let end = clock(args, "end_time")?;

    // Exactly one subject, checked here rather than left to the model's
    // reading of the description: a block for both a task and a project is
    // not a thing this domain has, and silently preferring one would put the
    // time against something nobody chose.
    let task: Option<TaskId> = args.opt_id("task_id", "task")?;
    let project: Option<ProjectId> = args.opt_id("project_id", "project")?;
    let label = args.opt_str("label");
    let (subject, label) = match (task, project, label) {
        (Some(id), None, None) => (BlockSubject::Task { id }, None),
        (None, Some(id), None) => (BlockSubject::Project { id }, None),
        (None, None, Some(text)) => (BlockSubject::Adhoc, Some(text)),
        (None, None, None) => {
            return Err(args.bad("give one of `task_id`, `project_id` or `label`"));
        }
        _ => return Err(args.bad("give only one of `task_id`, `project_id` or `label`")),
    };

    // Named before the block is built, so the reply can say what the time is
    // for -- and so a block against a task that does not exist fails here
    // rather than becoming an hour against nothing.
    let name = match &subject {
        BlockSubject::Task { id } => ctx.vault.task(*id)?.title,
        BlockSubject::Project { id } => ctx.vault.project(*id)?.name,
        BlockSubject::Adhoc => label.unwrap_or_default().to_string(),
    };

    let kind = match args.opt_str("kind") {
        Some("actual") => BlockKind::Actual,
        Some("planned") | None => BlockKind::Planned,
        Some(other) => {
            return Err(args.bad(format!("`kind` must be planned or actual, got {other:?}")));
        }
    };

    // Local wall-clock times, resolved in the person's zone. A block is
    // stored as two absolute instants -- so that durations and overlaps are
    // unambiguous -- but "two till three" means two till three where they
    // are sitting, not in UTC.
    let zone = jiff::tz::TimeZone::get(ctx.tz).unwrap_or(jiff::tz::TimeZone::UTC);
    let start_ts = date
        .at(start.hour(), start.minute(), 0, 0)
        .to_zoned(zone.clone())
        .map_err(|e| args.bad(format!("{date} {start} does not exist in {}: {e}", ctx.tz)))?
        .timestamp();
    let end_ts = date
        .at(end.hour(), end.minute(), 0, 0)
        .to_zoned(zone)
        .map_err(|e| args.bad(format!("{date} {end} does not exist in {}: {e}", ctx.tz)))?
        .timestamp();
    if end_ts <= start_ts {
        return Err(args.bad("`end_time` must be after `start_time`"));
    }
    let minutes = u32::try_from((end_ts.as_second() - start_ts.as_second()) / 60).unwrap_or(0);

    let mut block = TimeBlock::new(subject, start_ts, minutes, ctx.tz);
    block.kind = kind;
    block.title = label.unwrap_or_default().to_string();
    block.notes = args.opt_str("notes").unwrap_or_default().to_string();

    ctx.vault.save_block(&block)?;
    done("created", "time block", &format!("{name} on {date}"), block.id.to_string())
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

fn run_delete_block(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: BlockId = args.id("block_id", "time block")?;
    let block = ctx.vault.block(id)?;
    ctx.vault.delete_block(id)?;
    done("deleted", "time block", &block.local_date.to_string(), id.to_string())
}
