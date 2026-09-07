//! Storage for the task domain: projects, tasks and time blocks.
//!
//! # Why this is a separate trait
//!
//! [`JournalStore`](super::JournalStore) is the journal's contract, and the
//! Markdown backend exists precisely because someone wants their journal to
//! be a tree of files they can grep. Folding a kanban board into that trait
//! would oblige every backend to grow a todo implementation it has no
//! opinion about, and would make "add a backend" a much larger promise than
//! it is today.
//!
//! So the task domain is its own trait, reached through
//! [`JournalStore::tasks`](super::JournalStore::tasks), which returns `None`
//! by default. A backend opts in by implementing this and overriding that
//! one method; SQLite does, Markdown does not, and the interface reads
//! [`Capabilities::tasks`](super::Capabilities::tasks) to know which it is
//! talking to rather than discovering it from an error at click time.
//!
//! The calendar will arrive the same way — through this trait, since
//! [`TimeBlock`] is already the record it needs.
//!
//! # Cascades
//!
//! Deleting is defined to take the subtree with it: a project takes its
//! tasks, a task takes its descendants, and both take the time blocks
//! pointing at what they removed. Leaving orphaned blocks behind would
//! quietly corrupt every "where did my time go" total afterwards, which is
//! the one question this domain exists to answer.

use crate::error::Result;
use crate::id::{BlockId, ProjectId, TaskId};
use crate::task::{BlockKind, Priority, Project, Task, TaskStats, TaskStatus, TimeBlock};
use jiff::civil::Date;
use serde::{Deserialize, Serialize};

/// Which project's tasks to look at.
///
/// Three states, spelled out, because the interesting one is easy to lose:
/// "the inbox" is *tasks with no project*, which `Option<ProjectId>` cannot
/// express without becoming `Option<Option<ProjectId>>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "camelCase")]
pub enum ProjectScope {
    /// Every task in the vault, filed or not.
    #[default]
    Any,
    /// Only unfiled tasks.
    Inbox,
    Project {
        id: ProjectId,
    },
}

/// Which level of the task tree to look at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "camelCase")]
pub enum ParentScope {
    /// Tasks and subtasks alike.
    #[default]
    Any,
    /// Only tasks with no parent. What a board draws as cards.
    TopLevel,
    /// The children of one task.
    Of { id: TaskId },
}

/// How a task list should be ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskSort {
    /// The order the user dragged things into: `sort_order`, then creation
    /// time so that two tasks added at the same position keep a stable order.
    #[default]
    Manual,
    /// Soonest deadline first. Tasks with no deadline sort last — they are
    /// not urgent, and putting them at the top would bury the ones that are.
    DueAsc,
    /// Most important first.
    PriorityDesc,
    CreatedDesc,
    UpdatedDesc,
    /// Most recently finished first. Undated tasks sort last.
    CompletedDesc,
    TitleAsc,
}

/// Filter + pagination for [`TaskStore::list_tasks`].
///
/// All filters are ANDed. An empty query matches every task in the vault.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TaskQuery {
    pub project: ProjectScope,
    pub parent: ParentScope,
    /// Task must be in one of these. Empty means any status.
    pub statuses: Vec<TaskStatus>,
    /// Task must carry *every* listed tag, compared case-insensitively.
    pub tags: Vec<String>,
    pub priority_at_least: Option<Priority>,
    /// Inclusive bounds on `due_date`. A task with no deadline fails either.
    pub due_from: Option<Date>,
    pub due_to: Option<Date>,
    /// `Some(true)` keeps only tasks with a deadline, `Some(false)` only
    /// those without.
    pub has_due: Option<bool>,
    /// Case-insensitive substring of the title, notes or tags.
    pub text: String,
    pub sort: TaskSort,
    pub offset: u32,
    /// `None` means no limit.
    pub limit: Option<u32>,
}

impl TaskQuery {
    /// Everything in one project, at every level.
    pub fn in_project(id: ProjectId) -> Self {
        Self { project: ProjectScope::Project { id }, ..Default::default() }
    }

    /// The children of one task.
    pub fn children_of(id: TaskId) -> Self {
        Self { parent: ParentScope::Of { id }, ..Default::default() }
    }

    /// Everything still outstanding.
    pub fn open() -> Self {
        Self {
            statuses: TaskStatus::ALL.into_iter().filter(|s| s.is_open()).collect(),
            ..Default::default()
        }
    }

    /// Open tasks due on or before `today`. The "Today" list.
    pub fn due_by(today: Date) -> Self {
        Self { due_to: Some(today), ..Self::open() }
    }

    /// Does this task pass the filters? Backends that cannot express a
    /// filter natively fall back to this, so behaviour stays identical
    /// across backends.
    pub fn matches(&self, t: &Task) -> bool {
        match self.project {
            ProjectScope::Any => {}
            ProjectScope::Inbox if t.project_id.is_some() => return false,
            ProjectScope::Project { id } if t.project_id != Some(id) => return false,
            _ => {}
        }
        match self.parent {
            ParentScope::Any => {}
            ParentScope::TopLevel if t.parent_id.is_some() => return false,
            ParentScope::Of { id } if t.parent_id != Some(id) => return false,
            _ => {}
        }
        if !self.statuses.is_empty() && !self.statuses.contains(&t.status) {
            return false;
        }
        if let Some(min) = self.priority_at_least
            && t.priority.rank() < min.rank()
        {
            return false;
        }
        if let Some(want) = self.has_due
            && t.due_date.is_some() != want
        {
            return false;
        }
        // A task with no deadline is outside every date window, rather than
        // inside all of them: "due this week" must not return the backlog.
        if self.due_from.is_some() || self.due_to.is_some() {
            let Some(due) = t.due_date else { return false };
            if self.due_from.is_some_and(|f| due < f) || self.due_to.is_some_and(|to| due > to) {
                return false;
            }
        }
        if !self.text.trim().is_empty()
            && !t.searchable_text().to_lowercase().contains(&self.text.trim().to_lowercase())
        {
            return false;
        }
        // Case-insensitive, like the journal's tag filter: "Work" and "work"
        // are the same tag to a person, so they should be to the filter too.
        self.tags.iter().all(|want| t.tags.iter().any(|have| have.eq_ignore_ascii_case(want)))
    }

    /// Filter, sort and paginate a fully-materialised list. Shared by every
    /// backend, so ordering cannot drift between them.
    pub fn apply(&self, mut rows: Vec<Task>) -> Vec<Task> {
        rows.retain(|t| self.matches(t));
        sort_tasks(&mut rows, self.sort);
        let start = (self.offset as usize).min(rows.len());
        rows.drain(..start);
        if let Some(limit) = self.limit {
            rows.truncate(limit as usize);
        }
        rows
    }
}

/// Order `rows` in place.
pub fn sort_tasks(rows: &mut [Task], sort: TaskSort) {
    rows.sort_by(|a, b| match sort {
        TaskSort::Manual => a.sort_order.cmp(&b.sort_order).then(a.created_at.cmp(&b.created_at)),
        // `None` is greater than every `Some`, which is exactly the "undated
        // things go last" behaviour wanted here -- but only because the
        // bound is a *deadline*. Spelled out rather than relied upon.
        TaskSort::DueAsc => match (a.due_date, b.due_date) {
            (Some(x), Some(y)) => x.cmp(&y).then(a.sort_order.cmp(&b.sort_order)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.sort_order.cmp(&b.sort_order),
        },
        TaskSort::PriorityDesc => {
            b.priority.rank().cmp(&a.priority.rank()).then(a.sort_order.cmp(&b.sort_order))
        }
        TaskSort::CreatedDesc => b.created_at.cmp(&a.created_at),
        TaskSort::UpdatedDesc => b.updated_at.cmp(&a.updated_at),
        TaskSort::CompletedDesc => match (a.completed_at, b.completed_at) {
            (Some(x), Some(y)) => y.cmp(&x),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => b.updated_at.cmp(&a.updated_at),
        },
        TaskSort::TitleAsc => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
    });
}

/// Filter for [`TaskStore::list_blocks`]. This is the query a calendar view
/// is made of: "every block in this week, please".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BlockQuery {
    /// Inclusive lower bound on `local_date`.
    pub from: Option<Date>,
    /// Inclusive upper bound on `local_date`.
    pub to: Option<Date>,
    pub task_id: Option<TaskId>,
    pub project_id: Option<ProjectId>,
    /// Plan or record. `None` returns both.
    pub kind: Option<BlockKind>,
    pub limit: Option<u32>,
}

impl BlockQuery {
    /// Everything filed under the days from `from` to `to` inclusive.
    pub fn between(from: Date, to: Date) -> Self {
        Self { from: Some(from), to: Some(to), ..Default::default() }
    }

    pub fn for_task(id: TaskId) -> Self {
        Self { task_id: Some(id), ..Default::default() }
    }

    pub fn matches(&self, b: &TimeBlock) -> bool {
        if self.from.is_some_and(|f| b.local_date < f) || self.to.is_some_and(|t| b.local_date > t)
        {
            return false;
        }
        if let Some(task) = self.task_id
            && b.subject.task_id() != Some(task)
        {
            return false;
        }
        // A block on a project matches that project; so does a block on a
        // task *in* it, but the store resolves that, not this — here we can
        // only see what the subject says.
        if let Some(project) = self.project_id
            && b.subject.project_id() != Some(project)
        {
            return false;
        }
        if let Some(kind) = self.kind
            && b.kind != kind
        {
            return false;
        }
        true
    }

    /// Filter and order a materialised list: chronological, which is the
    /// only order a calendar or a timesheet ever wants.
    pub fn apply(&self, mut rows: Vec<TimeBlock>) -> Vec<TimeBlock> {
        rows.retain(|b| self.matches(b));
        rows.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
        if let Some(limit) = self.limit {
            rows.truncate(limit as usize);
        }
        rows
    }
}

/// The persistence contract for the task domain.
pub trait TaskStore: Send + Sync {
    // ---- projects -------------------------------------------------------

    /// Every project, archived ones included. There are tens of these, not
    /// thousands, so filtering is the caller's business.
    fn list_projects(&self) -> Result<Vec<Project>>;

    fn get_project(&self, id: ProjectId) -> Result<Project>;

    /// Insert or replace. Implementations must be idempotent.
    fn put_project(&self, project: &Project) -> Result<()>;

    /// Delete the project, every task in it, and every time block that
    /// pointed at either.
    fn delete_project(&self, id: ProjectId) -> Result<()>;

    // ---- tasks ----------------------------------------------------------

    fn list_tasks(&self, query: &TaskQuery) -> Result<Vec<Task>>;

    fn get_task(&self, id: TaskId) -> Result<Task>;

    fn put_task(&self, task: &Task) -> Result<()>;

    /// Write several tasks at once.
    ///
    /// This is not a convenience: dragging a card across a board renumbers
    /// every task in two columns, and doing that as N separate writes is
    /// both slow and a window in which the board is half-reordered on disk.
    /// Backends that can be transactional should override this and be so.
    fn put_tasks(&self, tasks: &[Task]) -> Result<()> {
        for t in tasks {
            self.put_task(t)?;
        }
        Ok(())
    }

    /// Delete the task, its descendants, and the time blocks pointing at any
    /// of them.
    fn delete_task(&self, id: TaskId) -> Result<()>;

    // ---- time blocks ----------------------------------------------------

    fn list_blocks(&self, query: &BlockQuery) -> Result<Vec<TimeBlock>>;

    fn get_block(&self, id: BlockId) -> Result<TimeBlock>;

    fn put_block(&self, block: &TimeBlock) -> Result<()>;

    fn delete_block(&self, id: BlockId) -> Result<()>;

    // ---- housekeeping ---------------------------------------------------

    /// Counts for the sidebar, as of the calendar day `today`.
    ///
    /// The day is a parameter rather than something the backend reads off
    /// the clock: "overdue" depends on the author's time zone, which is a
    /// question for the layer that knows what platform it is on.
    fn task_stats(&self, today: Date) -> Result<TaskStats>;
}

/// Associated data bound into a project's ciphertext. See
/// [`entry_aad`](super::entry_aad) for why records are bound to their id.
pub fn project_aad(id: ProjectId) -> Vec<u8> {
    format!("everyday.project.v1:{id}").into_bytes()
}

pub fn task_aad(id: TaskId) -> Vec<u8> {
    format!("everyday.task.v1:{id}").into_bytes()
}

pub fn block_aad(id: BlockId) -> Vec<u8> {
    format!("everyday.block.v1:{id}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::BlockSubject;
    use jiff::Timestamp;
    use jiff::civil::date;

    fn task(title: &str) -> Task {
        Task::new(title)
    }

    #[test]
    fn an_empty_query_matches_everything() {
        assert!(TaskQuery::default().matches(&task("anything")));
    }

    #[test]
    fn the_inbox_is_tasks_with_no_project() {
        let filed = task("filed").in_project(ProjectId::new());
        let loose = task("loose");

        let inbox = TaskQuery { project: ProjectScope::Inbox, ..Default::default() };
        assert!(inbox.matches(&loose));
        assert!(!inbox.matches(&filed));

        let any = TaskQuery::default();
        assert!(any.matches(&loose) && any.matches(&filed));
    }

    #[test]
    fn top_level_excludes_subtasks() {
        let parent = task("parent");
        let child = task("child").under(parent.id);

        let top = TaskQuery { parent: ParentScope::TopLevel, ..Default::default() };
        assert!(top.matches(&parent));
        assert!(!top.matches(&child));

        let kids = TaskQuery::children_of(parent.id);
        assert!(kids.matches(&child));
        assert!(!kids.matches(&parent));
    }

    #[test]
    fn a_task_with_no_deadline_is_outside_every_date_window() {
        // The bug this guards: "due this week" quietly returning the whole
        // backlog because an absent date compared as in-range.
        let undated = task("someday");
        let q = TaskQuery { due_to: Some(date(2026, 3, 31)), ..Default::default() };
        assert!(!q.matches(&undated));

        let mut dated = task("friday");
        dated.due_date = Some(date(2026, 3, 27));
        assert!(q.matches(&dated));
    }

    #[test]
    fn date_bounds_are_inclusive() {
        let q = TaskQuery {
            due_from: Some(date(2026, 3, 10)),
            due_to: Some(date(2026, 3, 20)),
            ..Default::default()
        };
        let on = |d: i8| {
            let mut t = task("x");
            t.due_date = Some(date(2026, 3, d));
            t
        };
        assert!(!q.matches(&on(9)));
        assert!(q.matches(&on(10)));
        assert!(q.matches(&on(20)));
        assert!(!q.matches(&on(21)));
    }

    #[test]
    fn status_and_priority_filters_narrow_rather_than_reorder() {
        let mut doing = task("doing");
        doing.status = TaskStatus::Doing;
        doing.priority = Priority::Low;

        let open = TaskQuery::open();
        assert!(open.matches(&doing));

        let mut done = task("done");
        done.set_status(TaskStatus::Done);
        assert!(!open.matches(&done));

        let important = TaskQuery { priority_at_least: Some(Priority::High), ..Default::default() };
        assert!(!important.matches(&doing));
        doing.priority = Priority::Urgent;
        assert!(important.matches(&doing));
    }

    #[test]
    fn tag_filter_requires_all_tags_and_ignores_case() {
        let mut t = task("x");
        t.tags = vec!["Work".into(), "deep".into()];
        let q = |tags: &[&str]| TaskQuery {
            tags: tags.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        assert!(q(&["work"]).matches(&t));
        assert!(q(&["WORK", "Deep"]).matches(&t));
        assert!(!q(&["work", "errand"]).matches(&t));
    }

    #[test]
    fn text_search_covers_notes_as_well_as_titles() {
        let mut t = task("book flights");
        t.notes = "aisle seat".into();
        let q = |s: &str| TaskQuery { text: s.into(), ..Default::default() };
        assert!(q("FLIGHT").matches(&t), "matching should ignore case");
        assert!(q("aisle").matches(&t));
        assert!(!q("hotel").matches(&t));
    }

    #[test]
    fn undated_tasks_sort_after_dated_ones() {
        let mut rows = vec![task("no date"), task("late"), task("soon")];
        rows[1].due_date = Some(date(2026, 4, 1));
        rows[2].due_date = Some(date(2026, 3, 1));
        sort_tasks(&mut rows, TaskSort::DueAsc);
        assert_eq!(
            rows.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
            ["soon", "late", "no date"],
        );
    }

    #[test]
    fn manual_order_follows_sort_order() {
        let mut rows = vec![task("c"), task("a"), task("b")];
        rows[0].sort_order = 2;
        rows[1].sort_order = 0;
        rows[2].sort_order = 1;
        sort_tasks(&mut rows, TaskSort::Manual);
        assert_eq!(rows.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
    }

    #[test]
    fn apply_filters_then_sorts_then_paginates() {
        let rows: Vec<Task> = (1..=5)
            .map(|d| {
                let mut t = task(&format!("t{d}"));
                t.due_date = Some(date(2026, 3, d));
                t
            })
            .collect();
        let q =
            TaskQuery { sort: TaskSort::DueAsc, offset: 1, limit: Some(2), ..Default::default() };
        let out = q.apply(rows);
        assert_eq!(out.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["t2", "t3"]);
    }

    #[test]
    fn pagination_past_the_end_yields_nothing_rather_than_panicking() {
        let q = TaskQuery { offset: 99, ..Default::default() };
        assert!(q.apply(vec![task("only")]).is_empty());
    }

    #[test]
    fn block_queries_bound_by_the_local_day_and_order_chronologically() {
        let t0 = Timestamp::from_second(1_800_000_000).unwrap();
        let make = |offset_mins: i64, kind: BlockKind| {
            TimeBlock::new(
                BlockSubject::Adhoc,
                t0 + jiff::SignedDuration::from_mins(offset_mins),
                30,
                "UTC",
            )
            .of_kind(kind)
        };
        let rows = vec![make(120, BlockKind::Actual), make(0, BlockKind::Planned)];
        let day = rows[0].local_date;

        let all = BlockQuery::between(day, day).apply(rows.clone());
        assert_eq!(all.len(), 2);
        assert!(all[0].start < all[1].start, "blocks come back in time order");

        let logged = BlockQuery { kind: Some(BlockKind::Actual), ..Default::default() };
        assert_eq!(logged.apply(rows.clone()).len(), 1);

        let elsewhere = BlockQuery::between(day.tomorrow().unwrap(), day.tomorrow().unwrap());
        assert!(elsewhere.apply(rows).is_empty());
    }

    #[test]
    fn a_block_query_for_one_task_ignores_other_subjects() {
        let t0 = Timestamp::from_second(1_800_000_000).unwrap();
        let mine = TaskId::new();
        let rows = vec![
            TimeBlock::new(BlockSubject::Task { id: mine }, t0, 30, "UTC"),
            TimeBlock::new(BlockSubject::Task { id: TaskId::new() }, t0, 30, "UTC"),
            TimeBlock::new(BlockSubject::Adhoc, t0, 30, "UTC"),
        ];
        assert_eq!(BlockQuery::for_task(mine).apply(rows).len(), 1);
    }

    #[test]
    fn aad_is_distinct_per_record_and_per_kind() {
        let same = uuid::Uuid::now_v7();
        assert_ne!(task_aad(TaskId(same)), project_aad(ProjectId(same)));
        assert_ne!(task_aad(TaskId(same)), block_aad(BlockId(same)));
        // And distinct from the journal domain, which shares the id space.
        assert_ne!(task_aad(TaskId(same)), super::super::entry_aad(crate::EntryId(same)));
    }
}
