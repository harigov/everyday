//! Projects, tasks and blocks of time.
//!
//! Every command here fails with `unsupported` on a vault whose backend stores
//! journals only; the interface reads `capabilities.tasks` from the vault
//! status and hides the app rather than letting that happen.
//!
//! Ids and timestamps are minted by the core, never by a client, for the same
//! reason journals are: they are UUIDv7, which storage relies on to sort
//! chronologically, and `crypto.randomUUID` is both a different ordering and
//! unavailable outside a secure context.

use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::service::{Service, blocking};
use everyday_core::model::{system_tz, today_local};
use everyday_core::store::tasks::{BlockQuery, TaskQuery};
use everyday_core::task::{
    BlockKind, BlockSubject, Project, Task, TaskStats, TaskStatus, TimeBlock,
};
use everyday_core::{BlockId, ProjectId, TaskId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::Nothing;
use super::journals::Named;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveProject {
    pub project: Project,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRef {
    pub id: ProjectId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tasks {
    pub query: TaskQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRef {
    pub id: TaskId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTask {
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    #[serde(default)]
    pub parent_id: Option<TaskId>,
    #[serde(default)]
    pub status: Option<TaskStatus>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveTask {
    pub task: Task,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveTasks {
    pub tasks: Vec<Task>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Blocks {
    pub query: BlockQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewBlock {
    pub subject: BlockSubject,
    pub start: jiff::Timestamp,
    pub minutes: u32,
    #[serde(default)]
    pub kind: Option<BlockKind>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveBlock {
    pub block: TimeBlock,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockRef {
    pub id: BlockId,
}

/// A tag and how often it is used. A named struct rather than a tuple so a
/// caller reads `t.count` instead of `t[1]`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagCount {
    pub tag: String,
    pub count: u32,
}

async fn list_projects(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<Project>> {
    svc.on_vault(move |vault| vault.projects()).await
}

async fn new_project(svc: Arc<Service>, _ctx: Ctx, args: Named) -> CommandResult<Project> {
    let _ = svc.require()?;
    Ok(Project::new(args.name))
}

async fn save_project(svc: Arc<Service>, _ctx: Ctx, args: SaveProject) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_project(&args.project)).await
}

/// Delete a project, its tasks and every block of time booked against them.
async fn delete_project(svc: Arc<Service>, _ctx: Ctx, args: ProjectRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_project(args.id)).await
}

async fn list_tasks(svc: Arc<Service>, _ctx: Ctx, args: Tasks) -> CommandResult<Vec<Task>> {
    svc.on_vault(move |vault| vault.tasks(&args.query)).await
}

async fn get_task(svc: Arc<Service>, _ctx: Ctx, args: TaskRef) -> CommandResult<Task> {
    svc.on_vault(move |vault| vault.task(args.id)).await
}

/// Mint a task, without saving it.
///
/// The caller fills in the title and whatever the quick-add line parsed out of
/// it, then calls `save_task`. Two round trips rather than one, in exchange for
/// one shape of task travelling in each direction.
async fn new_task(svc: Arc<Service>, _ctx: Ctx, args: NewTask) -> CommandResult<Task> {
    let _ = svc.require()?;
    let mut task = Task::new(String::new()).in_project(args.project_id).under(args.parent_id);
    if let Some(status) = args.status {
        task.status = status;
    }
    Ok(task)
}

async fn save_task(svc: Arc<Service>, _ctx: Ctx, args: SaveTask) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_task(&args.task)).await
}

/// Write several tasks at once. This is what dragging a card across a board is:
/// two columns renumbered, which must land as one change or not at all.
async fn save_tasks(svc: Arc<Service>, _ctx: Ctx, args: SaveTasks) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_tasks(&args.tasks)).await
}

/// Delete a task, its subtasks and their time blocks.
async fn delete_task(svc: Arc<Service>, _ctx: Ctx, args: TaskRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_task(args.id)).await
}

async fn list_blocks(svc: Arc<Service>, _ctx: Ctx, args: Blocks) -> CommandResult<Vec<TimeBlock>> {
    svc.on_vault(move |vault| vault.blocks(&args.query)).await
}

/// Mint a block of time, without saving it.
///
/// The time zone is the machine's, resolved here rather than in a webview, so
/// that `local_date` -- the column a calendar's week query scans -- is decided
/// by the same code that decides an entry's.
async fn new_block(svc: Arc<Service>, _ctx: Ctx, args: NewBlock) -> CommandResult<TimeBlock> {
    let _ = svc.require()?;
    let tz = system_tz();
    let mut block = TimeBlock::new(args.subject, args.start, args.minutes, &tz);
    if let Some(kind) = args.kind {
        block.kind = kind;
    }
    Ok(block)
}

async fn save_block(svc: Arc<Service>, _ctx: Ctx, args: SaveBlock) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_block(&args.block)).await
}

async fn delete_block(svc: Arc<Service>, _ctx: Ctx, args: BlockRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_block(args.id)).await
}

/// Every tag used anywhere in the task domain, most used first.
async fn task_tags(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<Vec<TagCount>> {
    let vault = svc.require()?;
    blocking(move || {
        Ok(vault.task_tags()?.into_iter().map(|(tag, count)| TagCount { tag, count }).collect())
    })
    .await
}

/// Counts for the sidebar, as of the machine's own calendar day.
///
/// The day is resolved here rather than in the core, and here rather than in a
/// webview, so that "overdue" is decided by the same code that decides which
/// day an entry is filed under.
async fn task_stats(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<TaskStats> {
    svc.on_vault(move |vault| vault.task_stats(today_local())).await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_projects", scope: Tasks, effect: Read,
        args: Nothing, returns: "Project[]", signature: &[],
        run: list_projects,
    },
    command! {
        name: "new_project", scope: Tasks, effect: Read,
        args: Named, returns: "Project",
        signature: &[("name", "string", true)],
        run: new_project,
    },
    command! {
        name: "save_project", scope: Tasks, effect: Write,
        change: Project / Updated,
        args: SaveProject, returns: "void",
        signature: &[("project", "Project", true)],
        run: save_project,
    },
    command! {
        name: "delete_project", scope: Tasks, effect: Destructive,
        change: Project / Deleted,
        args: ProjectRef, returns: "void",
        signature: &[("id", "ProjectId", true)],
        run: delete_project,
    },
    command! {
        name: "list_tasks", scope: Tasks, effect: Read,
        args: Tasks, returns: "Task[]",
        signature: &[("query", "TaskQuery", true)],
        run: list_tasks,
    },
    command! {
        name: "get_task", scope: Tasks, effect: Read,
        args: TaskRef, returns: "Task",
        signature: &[("id", "TaskId", true)],
        run: get_task,
    },
    command! {
        name: "new_task", scope: Tasks, effect: Read,
        args: NewTask, returns: "Task",
        signature: &[
            ("projectId", "ProjectId | null", false),
            ("parentId", "TaskId | null", false),
            ("status", "TaskStatus | null", false),
        ],
        run: new_task,
    },
    command! {
        name: "save_task", scope: Tasks, effect: Write,
        change: Task / Updated,
        args: SaveTask, returns: "void",
        signature: &[("task", "Task", true)],
        run: save_task,
    },
    command! {
        name: "save_tasks", scope: Tasks, effect: Write,
        change: Task / Updated,
        args: SaveTasks, returns: "void",
        signature: &[("tasks", "Task[]", true)],
        run: save_tasks,
    },
    command! {
        name: "delete_task", scope: Tasks, effect: Destructive,
        change: Task / Deleted,
        args: TaskRef, returns: "void",
        signature: &[("id", "TaskId", true)],
        run: delete_task,
    },
    command! {
        name: "list_blocks", scope: Tasks, effect: Read,
        args: Blocks, returns: "TimeBlock[]",
        signature: &[("query", "BlockQuery", true)],
        run: list_blocks,
    },
    command! {
        name: "new_block", scope: Tasks, effect: Read,
        args: NewBlock, returns: "TimeBlock",
        signature: &[
            ("subject", "BlockSubject", true),
            ("start", "string", true),
            ("minutes", "number", true),
            ("kind", "BlockKind | null", false),
        ],
        run: new_block,
    },
    command! {
        name: "save_block", scope: Tasks, effect: Write,
        change: Block / Updated,
        args: SaveBlock, returns: "void",
        signature: &[("block", "TimeBlock", true)],
        run: save_block,
    },
    command! {
        name: "delete_block", scope: Tasks, effect: Destructive,
        change: Block / Deleted,
        args: BlockRef, returns: "void",
        signature: &[("id", "BlockId", true)],
        run: delete_block,
    },
    command! {
        name: "task_tags", scope: Tasks, effect: Read,
        args: Nothing, returns: "TagCount[]", signature: &[],
        run: task_tags,
    },
    command! {
        name: "task_stats", scope: Tasks, effect: Read,
        args: Nothing, returns: "TaskStats", signature: &[],
        run: task_stats,
    },
];
