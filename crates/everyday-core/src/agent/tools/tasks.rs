//! Projects, tasks and the time spent on them: the todo app, and the
//! records the calendar draws.

use serde_json::{Value, json};

use super::{
    Args, Tool, ToolContext, day, done, flag, limit_arg, list, number, one_of, schema, text,
};
use crate::error::{Error, Result};
use crate::id::{ProjectId, TaskId};
use crate::store::tasks::{ParentScope, ProjectScope, TaskQuery};
use crate::task::{Priority, Project, ProjectStatus, Task, TaskStatus};
use jiff::Timestamp;

// `pub(super)` rather than private: `agent::tools`'s own argument-parsing
// tests exercise a real enum's error message, and a task's status is the
// one on hand.
pub(super) const STATUSES: &[&str] = &["backlog", "todo", "doing", "blocked", "done", "cancelled"];
const PRIORITIES: &[&str] = &["none", "low", "medium", "high", "urgent"];
const PROJECT_STATUSES: &[&str] = &["active", "paused", "done", "archived"];

pub(super) static TOOLS: &[Tool] = &[
    tool!(
        "list_projects",
        Read,
        Tasks,
        schema(
            vec![
                ("status", one_of("Restrict to one status.", PROJECT_STATUSES)),
                ("include_archived", flag("Include archived projects. Off by default.")),
            ],
            &[]
        ),
        "Every project, with its id, name, status, deadline and how many tasks it \
         holds open.",
        run_list_projects
    ),
    tool!(
        "create_project",
        Write,
        Tasks,
        schema(
            vec![
                ("name", text("What to call it.")),
                ("notes", text("A description.")),
                ("status", one_of("Defaults to active.", PROJECT_STATUSES)),
                ("priority", one_of("Defaults to none.", PRIORITIES)),
                ("due_date", day("Deadline for the whole project.")),
                ("start_date", day("Earliest sensible start.")),
                ("tags", list("Tags to attach.")),
            ],
            &["name"]
        ),
        "Create a project \u{2014} a named body of work that tasks belong to.",
        run_create_project
    ),
    tool!(
        "update_project",
        Write,
        Tasks,
        schema(
            vec![
                ("project_id", text("Id of the project to change.")),
                ("name", text("Rename it.")),
                ("notes", text("Replace the description.")),
                ("status", one_of("Move it to this status.", PROJECT_STATUSES)),
                ("priority", one_of("Change its priority.", PRIORITIES)),
                ("due_date", day("Set the deadline.")),
                ("start_date", day("Set the start date.")),
                ("tags", list("Replaces the tags entirely.")),
            ],
            &["project_id"]
        ),
        "Change a project. Omitted fields are left alone. Marking a project done \
         does not touch its tasks.",
        run_update_project
    ),
    tool!(
        "delete_project",
        Destructive,
        Tasks,
        schema(vec![("project_id", text("Id of the project to delete."))], &["project_id"]),
        "Permanently delete a project. Its tasks are NOT deleted \u{2014} they fall back \
         to the inbox. Prefer update_project with status archived unless deletion \
         was actually asked for.",
        run_delete_project,
        Some(describe_delete_project)
    ),
    tool!(
        "list_tasks",
        Read,
        Tasks,
        schema(
            vec![
                ("project_id", text("Only tasks in this project.")),
                ("inbox_only", flag("Only tasks in no project.")),
                ("parent_id", text("Only the subtasks of this task.")),
                ("top_level_only", flag("Exclude subtasks.")),
                ("statuses", list("Keep only these statuses. Defaults to every status.")),
                ("open_only", flag("Shorthand for the statuses that are not done or cancelled.")),
                ("tags", list("Keep only tasks carrying every one of these tags.")),
                ("priority_at_least", one_of("Keep only tasks at or above this.", PRIORITIES)),
                ("due_from", day("Earliest deadline, inclusive.")),
                ("due_to", day("Latest deadline, inclusive. Use today's date for 'due now'.")),
                ("text", text("Case-insensitive substring of the title, notes or tags.")),
                limit_arg(),
            ],
            &[]
        ),
        "Tasks matching a filter. The main way to find a task before changing it. \
         For 'what is due today', pass due_to as today's date with open_only true.",
        run_list_tasks
    ),
    tool!(
        "get_task",
        Read,
        Tasks,
        schema(vec![("task_id", text("Id from list_tasks."))], &["task_id"]),
        "One task in full, including its notes and its subtasks.",
        run_get_task
    ),
    tool!(
        "create_task",
        Write,
        Tasks,
        schema(
            vec![
                ("title", text("What to do. A verb phrase reads best.")),
                ("project_id", text("Which project it belongs to. Omit for the inbox.")),
                ("parent_id", text("Make it a subtask of this task.")),
                ("notes", text("Detail that does not fit in the title.")),
                ("status", one_of("Defaults to todo.", STATUSES)),
                ("priority", one_of("Defaults to none.", PRIORITIES)),
                ("due_date", day("Deadline.")),
                ("start_date", day("Earliest sensible start.")),
                ("estimate_minutes", number("Expected effort in minutes.")),
                ("tags", list("Tags to attach.")),
            ],
            &["title"]
        ),
        "Create a task. This is the tool for 'add a task', 'remind me to', \
         'I need to'. Create several by calling it several times.",
        run_create_task
    ),
    tool!(
        "update_task",
        Write,
        Tasks,
        schema(
            vec![
                ("task_id", text("Id of the task to change.")),
                ("title", text("Rename it.")),
                ("notes", text("Replace the notes.")),
                ("status", one_of("Move it. Use done to complete a task.", STATUSES)),
                ("priority", one_of("Change its priority.", PRIORITIES)),
                ("project_id", text("Move it to this project.")),
                ("clear_project", flag("Move it back to the inbox.")),
                ("due_date", day("Set the deadline.")),
                ("clear_due_date", flag("Remove the deadline.")),
                ("start_date", day("Set the start date.")),
                ("estimate_minutes", number("Expected effort in minutes.")),
                ("tags", list("Replaces the tags entirely.")),
            ],
            &["task_id"]
        ),
        "Change a task. Omitted fields are left alone. This is how a task is \
         completed \u{2014} status done \u{2014} rescheduled, reprioritised or moved. \
         Never delete a task to mark it finished.",
        run_update_task
    ),
    tool!(
        "delete_task",
        Destructive,
        Tasks,
        schema(vec![("task_id", text("Id of the task to delete."))], &["task_id"]),
        "Permanently delete a task and its subtasks. There is no undo. To finish \
         a task use update_task with status done; to drop one you decided against, \
         status cancelled. Delete only what was never real.",
        run_delete_task,
        Some(describe_delete_task)
    ),
];

fn describe_delete_project(ctx: &ToolContext<'_>, args: &Args<'_>) -> Option<String> {
    let id: ProjectId = args.opt_id("project_id", "project").ok()??;
    ctx.vault.project(id).ok().map(|p| p.name)
}

fn describe_delete_task(ctx: &ToolContext<'_>, args: &Args<'_>) -> Option<String> {
    let id: TaskId = args.opt_id("task_id", "task").ok()??;
    ctx.vault.task(id).ok().map(|t| t.title)
}

fn task_json(t: &Task) -> Value {
    let mut v = json!({
        "id": t.id.to_string(),
        "title": t.title,
        "status": t.status.as_str(),
    });
    let m = v.as_object_mut().unwrap();
    if t.priority != Priority::None {
        m.insert("priority".into(), json!(t.priority.as_str()));
    }
    if let Some(p) = t.project_id {
        m.insert("project_id".into(), json!(p.to_string()));
    }
    if let Some(p) = t.parent_id {
        m.insert("parent_id".into(), json!(p.to_string()));
    }
    if let Some(d) = t.due_date {
        m.insert("due_date".into(), json!(d.to_string()));
    }
    if let Some(d) = t.start_date {
        m.insert("start_date".into(), json!(d.to_string()));
    }
    if let Some(e) = t.estimate_minutes {
        m.insert("estimate_minutes".into(), json!(e));
    }
    if !t.tags.is_empty() {
        m.insert("tags".into(), json!(t.tags));
    }
    if !t.notes.trim().is_empty() {
        m.insert("notes".into(), json!(t.notes));
    }
    v
}

fn project_json(p: &Project, open_tasks: Option<u64>) -> Value {
    let mut v = json!({
        "id": p.id.to_string(),
        "name": p.name,
        "status": p.status.as_str(),
    });
    let m = v.as_object_mut().unwrap();
    if p.priority != Priority::None {
        m.insert("priority".into(), json!(p.priority.as_str()));
    }
    if let Some(d) = p.due_date {
        m.insert("due_date".into(), json!(d.to_string()));
    }
    if !p.notes.trim().is_empty() {
        m.insert("notes".into(), json!(p.notes));
    }
    if !p.tags.is_empty() {
        m.insert("tags".into(), json!(p.tags));
    }
    if let Some(n) = open_tasks {
        m.insert("open_tasks".into(), json!(n));
    }
    v
}

fn run_list_projects(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let want: Option<ProjectStatus> = args.opt_enum("status", PROJECT_STATUSES)?;
    let include_archived = args.bool_or("include_archived", false);

    let counts = ctx.vault.task_stats(ctx.today)?.open_by_project;
    let rows: Vec<Value> = ctx
        .vault
        .projects()?
        .into_iter()
        .filter(|p| match want {
            Some(s) => p.status == s,
            None => include_archived || p.status != ProjectStatus::Archived,
        })
        .map(|p| {
            // Projects with nothing open are omitted from the counts rather
            // than listed as zero, so an absent row means none rather than
            // unknown.
            let open = counts.iter().find(|c| c.project_id == Some(p.id)).map_or(0, |c| c.open);
            project_json(&p, Some(open))
        })
        .collect();

    Ok(json!({ "count": rows.len(), "projects": rows }))
}

fn run_create_project(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let mut p = Project::new(args.str("name")?);
    p.notes = args.opt_str("notes").unwrap_or_default().to_string();
    if let Some(s) = args.opt_enum("status", PROJECT_STATUSES)? {
        p.set_status(s);
    }
    if let Some(pr) = args.opt_enum("priority", PRIORITIES)? {
        p.priority = pr;
    }
    p.due_date = args.opt_date("due_date")?;
    p.start_date = args.opt_date("start_date")?;
    p.tags = args.strings("tags");

    ctx.vault.save_project(&p)?;
    done("created", "project", &p.name, p.id.to_string())
}

fn run_update_project(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: ProjectId = args.id("project_id", "project")?;
    let mut p = ctx.vault.project(id)?;

    if let Some(name) = args.opt_str("name") {
        p.name = name.to_string();
    }
    if let Some(notes) = args.opt_str("notes") {
        p.notes = notes.to_string();
    }
    // Through `set_status`, never by assignment: the helper is what keeps
    // `completed_at` honest -- set when a project is finished, cleared when
    // it is reopened. Assigning the field directly leaves a reopened project
    // wearing the date it was closed.
    if let Some(s) = args.opt_enum("status", PROJECT_STATUSES)? {
        p.set_status(s);
    }
    if let Some(pr) = args.opt_enum("priority", PRIORITIES)? {
        p.priority = pr;
    }
    if let Some(d) = args.opt_date("due_date")? {
        p.due_date = Some(d);
    }
    if let Some(d) = args.opt_date("start_date")? {
        p.start_date = Some(d);
    }
    if args.get("tags").is_some() {
        p.tags = args.strings("tags");
    }
    p.updated_at = Timestamp::now();

    ctx.vault.save_project(&p)?;
    done("updated", "project", &p.name, p.id.to_string())
}

fn run_delete_project(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: ProjectId = args.id("project_id", "project")?;
    let p = ctx.vault.project(id)?;
    ctx.vault.delete_project(id)?;
    done("deleted", "project", &p.name, id.to_string())
}

fn run_list_tasks(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let mut query = TaskQuery {
        tags: args.strings("tags"),
        due_from: args.opt_date("due_from")?,
        due_to: args.opt_date("due_to")?,
        text: args.opt_str("text").unwrap_or_default().to_string(),
        limit: Some(args.limit()),
        ..Default::default()
    };

    if let Some(id) = args.opt_id::<ProjectId>("project_id", "project")? {
        query.project = ProjectScope::Project { id };
    } else if args.bool_or("inbox_only", false) {
        query.project = ProjectScope::Inbox;
    }

    if let Some(id) = args.opt_id::<TaskId>("parent_id", "task")? {
        query.parent = ParentScope::Of { id };
    } else if args.bool_or("top_level_only", false) {
        query.parent = ParentScope::TopLevel;
    }

    if let Some(p) = args.opt_enum::<Priority>("priority_at_least", PRIORITIES)? {
        query.priority_at_least = Some(p);
    }

    // `open_only` and an explicit list of statuses are two ways to say the
    // same kind of thing, and a model will sometimes send both. The explicit
    // list wins, because it is the more specific request.
    let named = args.strings("statuses");
    if !named.is_empty() {
        let mut statuses = Vec::new();
        for raw in &named {
            let parsed: TaskStatus =
                serde_json::from_value(json!(raw.to_lowercase())).map_err(|_| {
                    args.bad(format!(
                        "`statuses` must contain only {}, got {raw:?}",
                        STATUSES.join(", ")
                    ))
                })?;
            statuses.push(parsed);
        }
        query.statuses = statuses;
    } else if args.bool_or("open_only", false) {
        query.statuses = TaskStatus::ALL.into_iter().filter(|s| s.is_open()).collect();
    }

    let rows = ctx.vault.tasks(&query)?;
    Ok(json!({
        "count": rows.len(),
        "tasks": rows.iter().map(task_json).collect::<Vec<_>>(),
    }))
}

fn run_get_task(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: TaskId = args.id("task_id", "task")?;
    let task = ctx.vault.task(id)?;
    let children = ctx.vault.tasks(&TaskQuery::children_of(id))?;
    let mut out = task_json(&task);
    if !children.is_empty() {
        out.as_object_mut()
            .unwrap()
            .insert("subtasks".into(), json!(children.iter().map(task_json).collect::<Vec<_>>()));
    }
    Ok(out)
}

fn run_create_task(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let mut t = Task::new(args.str("title")?);
    t.project_id = resolve_project(ctx, args, "project_id")?;
    t.parent_id = resolve_parent(ctx, args)?;
    t.notes = args.opt_str("notes").unwrap_or_default().to_string();
    if let Some(s) = args.opt_enum("status", STATUSES)? {
        t.set_status(s);
    }
    if let Some(p) = args.opt_enum("priority", PRIORITIES)? {
        t.priority = p;
    }
    t.due_date = args.opt_date("due_date")?;
    t.start_date = args.opt_date("start_date")?;
    t.estimate_minutes = args.opt_u32("estimate_minutes");
    t.tags = args.strings("tags");

    ctx.vault.save_task(&t)?;
    done("created", "task", &t.title, t.id.to_string())
}

fn run_update_task(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: TaskId = args.id("task_id", "task")?;
    let mut t = ctx.vault.task(id)?;

    if let Some(title) = args.opt_str("title") {
        t.title = title.to_string();
    }
    if let Some(notes) = args.opt_str("notes") {
        t.notes = notes.to_string();
    }
    // See `run_update_project`: `set_status` owns `completed_at`. A task
    // finished by assignment has no completion date, so "what did I get done
    // this week" never sees it; one reopened by assignment keeps the old one
    // and renders as a todo that was finished on Tuesday.
    if let Some(s) = args.opt_enum("status", STATUSES)? {
        t.set_status(s);
    }
    if let Some(p) = args.opt_enum("priority", PRIORITIES)? {
        t.priority = p;
    }
    // Clearing needs its own flag: a model has no way to send "no project"
    // in a field typed as a string, and `""` would have to be guessed at.
    if args.bool_or("clear_project", false) {
        t.project_id = None;
    } else if let Some(p) = resolve_project(ctx, args, "project_id")? {
        t.project_id = Some(p);
    }
    if args.bool_or("clear_due_date", false) {
        t.due_date = None;
    } else if let Some(d) = args.opt_date("due_date")? {
        t.due_date = Some(d);
    }
    if let Some(d) = args.opt_date("start_date")? {
        t.start_date = Some(d);
    }
    if let Some(e) = args.opt_u32("estimate_minutes") {
        t.estimate_minutes = Some(e);
    }
    if args.get("tags").is_some() {
        t.tags = args.strings("tags");
    }
    t.updated_at = Timestamp::now();

    ctx.vault.save_task(&t)?;
    done("updated", "task", &t.title, t.id.to_string())
}

/// Read the project an argument names, so a task cannot be filed into one
/// that does not exist.
///
/// Ids are untagged UUIDs and there is no foreign key on `project_id` -- the
/// column is a denormalised copy beside a sealed payload -- so an id the
/// model half-remembered is stored without complaint and produces a task
/// that is in neither the inbox nor on any board, while the tool reports
/// success. One read closes that, and its error tells the model how to get
/// a real one.
fn resolve_project(ctx: &ToolContext<'_>, args: &Args<'_>, key: &str) -> Result<Option<ProjectId>> {
    let Some(id) = args.opt_id::<ProjectId>(key, "project")? else { return Ok(None) };
    ctx.vault.project(id).map_err(|_| {
        Error::Invalid(format!(
            "no project with id {id}. Call list_projects and use an id from it."
        ))
    })?;
    Ok(Some(id))
}

/// The same, for a parent task. See [`resolve_project`].
fn resolve_parent(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Option<TaskId>> {
    let Some(id) = args.opt_id::<TaskId>("parent_id", "task")? else { return Ok(None) };
    ctx.vault.task(id).map_err(|_| {
        Error::Invalid(format!("no task with id {id}. Call list_tasks and use an id from it."))
    })?;
    Ok(Some(id))
}

fn run_delete_task(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: TaskId = args.id("task_id", "task")?;
    let t = ctx.vault.task(id)?;
    ctx.vault.delete_task(id)?;
    done("deleted", "task", &t.title, id.to_string())
}
