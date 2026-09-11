//! Roles and goals: the shape of a life, and what any other record was
//! *for*.

use serde_json::{Value, json};
use std::collections::BTreeMap;

use super::{
    Args, Tool, ToolContext, Window, day, done, empty_schema, flag, limit_arg, one_of, schema, text,
};
use crate::error::Result;
use crate::id::{GoalId, RoleId};
use crate::purpose::{Goal, GoalActivity, GoalStatus, Purpose};
use crate::store::purpose::{GoalQuery, PurposeWindow};

const GOAL_STATUSES: [&str; 4] = ["active", "paused", "done", "dropped"];

pub(super) static TOOLS: &[Tool] = &[
    tool!(
        "list_roles",
        Read,
        Purpose,
        empty_schema(),
        "The parts of a life this vault is organised around \u{2014} parent, work, \
         yourself \u{2014} with how many goals sit under each.",
        run_list_roles
    ),
    tool!(
        "list_goals",
        Read,
        Purpose,
        schema(
            vec![
                ("role_id", text("From list_roles. Omit for every role.")),
                ("status", one_of("Only goals in this state.", &GOAL_STATUSES)),
                ("include_activity", flag("Also say what has been recorded against each.")),
                limit_arg(),
            ],
            &[]
        ),
        "What somebody has said they want, under which part of their life. With \
         include_activity, each goal also reports its hours, its open tasks and \
         when it was last touched \u{2014} which is how to find the ones that have \
         gone quiet.",
        run_list_goals
    ),
    tool!(
        "create_goal",
        Write,
        Purpose,
        schema(
            vec![
                ("role_id", text("From list_roles. Required: every goal sits under one.")),
                ("title", text("What is wanted.")),
                ("notes", text("What done would look like.")),
                ("horizon", day("When it would ideally be true by. Soft; nothing is notified.")),
            ],
            &["role_id", "title"]
        ),
        "Add a goal under a role.",
        run_create_goal
    ),
    tool!(
        "update_goal",
        Write,
        Purpose,
        schema(
            vec![
                ("goal_id", text("From list_goals.")),
                ("title", text("A new title.")),
                ("notes", text("What done would look like.")),
                ("status", one_of("Where it has got to.", &GOAL_STATUSES)),
                ("horizon", day("When it would ideally be true by.")),
                ("role_id", text("Move it under a different role.")),
            ],
            &["goal_id"]
        ),
        "Change a goal. Only the fields given are touched. Setting status to done \
         stamps when it was finished; dropped does not, because giving up on \
         something is not finishing it.",
        run_update_goal
    ),
    tool!(
        "delete_goal",
        Destructive,
        Purpose,
        schema(vec![("goal_id", text("From list_goals."))], &["goal_id"]),
        "Permanently delete a goal. Whatever was filed under it is kept but stops \
         being counted towards it. To record giving up on something, use \
         update_goal with status dropped.",
        run_delete_goal,
        Some(describe_delete_goal)
    ),
    tool!(
        "set_purpose",
        Write,
        Purpose,
        schema(
            vec![
                (
                    "kind",
                    one_of(
                        "What sort of record to file.",
                        &["project", "task", "block", "entry", "item", "tracker"]
                    )
                ),
                ("id", text("The record's id, from whichever list tool found it.")),
                ("goal_id", text("File it under this goal. From list_goals.")),
                ("role_id", text("Or under this role directly. From list_roles.")),
                ("clear", flag("Unfile it instead.")),
            ],
            &["kind", "id"]
        ),
        "Say what a record is for: a goal, or a part of a life directly. Filing a \
         project is worth more than filing its tasks \u{2014} everything under it \
         inherits, so one call attributes the lot. Pass exactly one of goal_id, \
         role_id or clear.",
        run_set_purpose
    ),
    tool!(
        "time_by_role",
        Read,
        Purpose,
        schema(
            vec![
                ("from", day("Start of the window, inclusive. Defaults to 7 days back.")),
                ("to", day("End of the window, inclusive. Defaults to today.")),
            ],
            &[]
        ),
        "Where the hours went over a window, grouped by the part of a life they \
         served, planned beside recorded. Time filed against nothing is reported \
         as its own row rather than left out.",
        run_time_by_role
    ),
];

fn describe_delete_goal(ctx: &ToolContext<'_>, args: &Args<'_>) -> Option<String> {
    let id: GoalId = args.opt_id("goal_id", "goal").ok()??;
    ctx.vault.goal(id).ok().map(|g| g.title)
}

fn run_list_roles(ctx: &ToolContext<'_>, _args: &Args<'_>) -> Result<Value> {
    let mut rows = Vec::new();
    for role in ctx.vault.roles()? {
        if role.archived {
            continue;
        }
        let (goals, open) = ctx.vault.count_goals(role.id).unwrap_or((0, 0));
        rows.push(json!({
            "id": role.id.to_string(),
            "name": role.name,
            "goals": goals,
            "open_goals": open,
        }));
    }
    Ok(json!({ "count": rows.len(), "roles": rows }))
}

fn goal_json(goal: &Goal, role: Option<&str>, activity: Option<&GoalActivity>) -> Value {
    let mut out = json!({
        "id": goal.id.to_string(),
        "title": goal.title,
        "status": goal.status.as_str(),
        "role_id": goal.role_id.to_string(),
    });
    let m = out.as_object_mut().expect("just built an object");
    if let Some(role) = role {
        m.insert("role".into(), json!(role));
    }
    if !goal.notes.is_empty() {
        m.insert("notes".into(), json!(goal.notes));
    }
    if let Some(horizon) = goal.horizon {
        m.insert("horizon".into(), json!(horizon.to_string()));
    }
    if let Some(a) = activity {
        // Only what happened. A row of seven zeroes tells a model nothing
        // and costs it the tokens to read them.
        let mut had = serde_json::Map::new();
        if a.actual_minutes > 0 {
            had.insert("minutes".into(), json!(a.actual_minutes));
        }
        if a.open_tasks > 0 {
            had.insert("open_tasks".into(), json!(a.open_tasks));
        }
        if a.done_tasks > 0 {
            had.insert("done_tasks".into(), json!(a.done_tasks));
        }
        if a.entries > 0 {
            had.insert("entries".into(), json!(a.entries));
        }
        if a.readings > 0 {
            had.insert("readings".into(), json!(a.readings));
        }
        if a.items > 0 {
            had.insert("shelf_items".into(), json!(a.items));
        }
        match a.last_touched {
            Some(at) => {
                had.insert("last_touched".into(), json!(at.to_string()));
            }
            // Said rather than omitted: "never touched" is the answer
            // somebody asking about a goal most wants to hear.
            None => {
                had.insert("last_touched".into(), json!("never"));
            }
        }
        m.insert("activity".into(), Value::Object(had));
    }
    out
}

fn run_list_goals(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let mut query = GoalQuery { limit: Some(args.limit()), ..Default::default() };
    query.role_id = args.opt_id("role_id", "role")?;
    if let Some(status) = args.opt_enum::<GoalStatus>("status", &GOAL_STATUSES)? {
        query.statuses = vec![status];
    }

    let roles = ctx.vault.roles()?;
    let with_activity = args.bool_or("include_activity", false);
    let rows: Vec<Value> = ctx
        .vault
        .goals(&query)?
        .into_iter()
        .map(|goal| {
            let role = roles.iter().find(|r| r.id == goal.role_id).map(|r| r.name.as_str());
            let activity = if with_activity { ctx.vault.goal_activity(goal.id).ok() } else { None };
            goal_json(&goal, role, activity.as_ref())
        })
        .collect();
    Ok(json!({ "count": rows.len(), "goals": rows }))
}

fn run_create_goal(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let role_id: RoleId = args.id("role_id", "role")?;
    let title = args.str("title")?.trim().to_string();
    if title.is_empty() {
        return Err(args.bad("`title` cannot be empty"));
    }
    let mut goal = Goal::new(role_id, title);
    goal.notes = args.opt_str("notes").unwrap_or_default().to_string();
    goal.horizon = args.opt_date("horizon")?;
    ctx.vault.save_goal(&goal)?;
    done("created", "goal", &goal.title, goal.id.to_string())
}

fn run_update_goal(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: GoalId = args.id("goal_id", "goal")?;
    let mut goal = ctx.vault.goal(id)?;

    if let Some(title) = args.opt_str("title") {
        let title = title.trim();
        if title.is_empty() {
            return Err(args.bad("`title` cannot be emptied"));
        }
        goal.title = title.to_string();
    }
    if let Some(notes) = args.opt_str("notes") {
        goal.notes = notes.to_string();
    }
    if let Some(role_id) = args.opt_id::<RoleId>("role_id", "role")? {
        goal.role_id = role_id;
    }
    if let Some(horizon) = args.opt_date("horizon")? {
        goal.horizon = Some(horizon);
    }
    // Through `set_status`, not by assignment: it is what keeps the finished
    // stamp honest, and dropping a goal must not leave one behind.
    if let Some(status) = args.opt_enum::<GoalStatus>("status", &GOAL_STATUSES)? {
        goal.set_status(status);
    }
    goal.updated_at = jiff::Timestamp::now();
    ctx.vault.save_goal(&goal)?;
    done("updated", "goal", &goal.title, id.to_string())
}

fn run_delete_goal(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: GoalId = args.id("goal_id", "goal")?;
    let goal = ctx.vault.goal(id)?;
    ctx.vault.delete_goal(id)?;
    done("deleted", "goal", &goal.title, id.to_string())
}

fn run_set_purpose(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let kind = args.str("kind")?;
    let id = args.str("id")?;
    let clear = args.bool_or("clear", false);
    let goal_id = args.opt_id::<GoalId>("goal_id", "goal")?;
    let role_id = args.opt_id::<RoleId>("role_id", "role")?;

    // Exactly one. Two would be a silent choice between them, and none
    // would be a call that looks like it worked and did nothing.
    let given =
        usize::from(clear) + usize::from(goal_id.is_some()) + usize::from(role_id.is_some());
    if given != 1 {
        return Err(args.bad("pass exactly one of `goal_id`, `role_id` or `clear`"));
    }

    let purpose = match (goal_id, role_id) {
        (Some(id), _) => {
            // Checked before anything is written, so a bad id is a refusal
            // rather than a record filed under nothing.
            ctx.vault.goal(id)?;
            Some(Purpose::Goal { id })
        }
        (_, Some(id)) => {
            ctx.vault.role(id)?;
            Some(Purpose::Role { id })
        }
        _ => None,
    };

    let name = match kind {
        "project" => {
            let mut p = ctx.vault.project(parse_id(args, "id", "project", id)?)?;
            p.purpose = purpose;
            let name = p.name.clone();
            ctx.vault.save_project(&p)?;
            name
        }
        "task" => {
            let mut t = ctx.vault.task(parse_id(args, "id", "task", id)?)?;
            t.purpose = purpose;
            let name = t.title.clone();
            ctx.vault.save_task(&t)?;
            name
        }
        "block" => {
            let mut b = ctx.vault.block(parse_id(args, "id", "block", id)?)?;
            b.purpose = purpose;
            let name = if b.title.is_empty() { "that hour".to_string() } else { b.title.clone() };
            ctx.vault.save_block(&b)?;
            name
        }
        "entry" => {
            let mut e = ctx.vault.entry(parse_id(args, "id", "entry", id)?)?;
            e.purpose = purpose;
            let name = e.display_title();
            // The version it was read at, so a filing that raced an edit in
            // the window loses rather than silently overwriting it.
            let expect = Some(e.updated_at);
            ctx.vault.save_entry(&e, expect)?;
            name
        }
        "item" => {
            let mut i = ctx.vault.item(parse_id(args, "id", "item", id)?)?;
            i.purpose = purpose;
            let name = i.title.clone();
            ctx.vault.save_item(&i)?;
            name
        }
        "tracker" => {
            let mut t = ctx.vault.tracker(parse_id(args, "id", "tracker", id)?)?;
            t.purpose = purpose;
            let name = t.name.clone();
            ctx.vault.save_tracker(&t)?;
            name
        }
        other => {
            return Err(args.bad(format!(
                "`kind` must be one of project, task, block, entry, item, tracker \u{2014} not {other:?}"
            )));
        }
    };

    Ok(json!({
        "ok": true,
        "action": if clear { "unfiled" } else { "filed" },
        "kind": kind,
        "name": name,
        "id": id,
    }))
}

/// Parse an id of a named kind, reporting it the way `Args` reports its own.
fn parse_id<T: std::str::FromStr>(
    args: &Args<'_>,
    field: &str,
    kind: &str,
    raw: &str,
) -> Result<T> {
    raw.parse::<T>().map_err(|_| args.bad(format!("`{field}` is not a {kind} id: {raw:?}")))
}

fn run_time_by_role(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let (from, to) = args.window(ctx, Window::Back(7))?;

    let window = PurposeWindow::new(from, to);
    let roles = ctx.vault.roles()?;
    let goals = ctx.vault.goals(&GoalQuery::default())?;

    /// Recorded, planned, blocks, and somebody else's meetings.
    #[derive(Default)]
    struct Row {
        actual: u64,
        planned: u64,
        blocks: u64,
        meetings: u64,
    }

    // Folded to one row per role here rather than in SQL, for the reason the
    // interface folds it too: a goal's role is inside a sealed payload, and
    // asking the database to open every one of them to group a report would
    // undo the whole point of the pointer being a clear column.
    let mut rows: BTreeMap<Option<String>, Row> = BTreeMap::new();
    let name_of = |role: Option<RoleId>| -> Option<String> {
        role.and_then(|id| roles.iter().find(|r| r.id == id)).map(|r| r.name.clone())
    };

    for entry in ctx.vault.time_by_purpose(window)? {
        let role = match entry.purpose {
            Some(Purpose::Role { id }) => Some(id),
            Some(Purpose::Goal { id }) => goals.iter().find(|g| g.id == id).map(|g| g.role_id),
            None => None,
        };
        let slot = rows.entry(name_of(role)).or_default();
        slot.actual += entry.actual_minutes;
        slot.planned += entry.planned_minutes;
        slot.blocks += entry.blocks;
    }

    // Meetings are counted apart from the hours and never summed into them:
    // an event is somebody else's claim on an hour and a block is your own
    // record of one, and adding them double-counts every meeting you logged.
    for entry in ctx.vault.events_by_role(window).unwrap_or_default() {
        rows.entry(name_of(entry.role_id)).or_default().meetings += entry.minutes;
    }

    let out: Vec<Value> = rows
        .into_iter()
        .map(|(name, row)| {
            json!({
                // The unattributed row is named rather than left as null:
                // most of a life is not booked against anything, and a
                // report that dropped that share would be flattering.
                "role": name.unwrap_or_else(|| "not filed".into()),
                "minutes": row.actual,
                "planned_minutes": row.planned,
                "blocks": row.blocks,
                "meeting_minutes": row.meetings,
            })
        })
        .collect();

    Ok(json!({
        "from": from.to_string(),
        "to": to.to_string(),
        "roles": out,
    }))
}
