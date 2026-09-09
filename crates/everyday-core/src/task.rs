//! The task domain: projects, tasks, and the blocks of time they occupy.
//!
//! This is the second domain in the vault, alongside [`crate::model`]'s
//! journals and entries, and it is deliberately shaped so that a calendar
//! can be built on it without a migration.
//!
//! ```text
//!   Project ──┬── Task ──┬── Task (subtask, and deeper if you insist)
//!             │          │
//!             └──────────┴── TimeBlock   when the work was planned,
//!                                        and when it actually happened
//! ```
//!
//! # Three decisions worth knowing about
//!
//! **Subtasks are tasks.** There is no separate `Subtask` type; a task
//! carries a `parent_id` and that is the whole of it. Two levels is what the
//! interface offers, but nothing in storage or the model cares how deep the
//! tree goes, so "sub-subtask" is a UI decision that can be revisited
//! without touching a schema.
//!
//! **Time is a first-class record, not a field.** A task does not have a
//! "scheduled at"; it has any number of [`TimeBlock`]s pointing at it, each
//! either [`BlockKind::Planned`] or [`BlockKind::Actual`]. That is what
//! makes "where did my time go" answerable — plan and reality are separate
//! rows to be compared, not one field overwriting the other — and it is what
//! lets a calendar hold events that are not tasks at all
//! ([`BlockSubject::Adhoc`]) without a second storage abstraction.
//!
//! **Descriptions are plain text.** Journal entries are rich documents
//! because that is what a journal is. A task description is a note to
//! yourself, and keeping it a `String` is what keeps quick capture quick,
//! keeps tasks out of the blob store, and keeps the attachment
//! garbage-collector's job unchanged.

use crate::id::{BlockId, ProjectId, TaskId};
use crate::purpose::Purpose;
use jiff::{
    Timestamp,
    civil::{Date, Time},
};
use serde::{Deserialize, Serialize};

/// Where a task is on its way to being done.
///
/// A closed set rather than user-defined columns. Board columns *are* these
/// statuses, which means "how long do things sit in Blocked" is a question
/// that can be asked across every project at once — the whole reason for
/// wanting analytics later. Per-project columns would make that
/// unanswerable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    /// Captured, not committed to.
    Backlog,
    /// Committed to, not started.
    #[default]
    Todo,
    /// In progress.
    Doing,
    /// Started and stuck on something external.
    Blocked,
    Done,
    /// Deliberately not doing this. Kept rather than deleted, because
    /// "decided against" and "never existed" are different answers.
    Cancelled,
}

impl TaskStatus {
    /// Left to right, as the board draws them.
    pub const ALL: [TaskStatus; 6] = [
        TaskStatus::Backlog,
        TaskStatus::Todo,
        TaskStatus::Doing,
        TaskStatus::Blocked,
        TaskStatus::Done,
        TaskStatus::Cancelled,
    ];

    /// Is there still work outstanding? `Cancelled` counts as closed.
    pub fn is_open(self) -> bool {
        !matches!(self, TaskStatus::Done | TaskStatus::Cancelled)
    }

    /// Stable wire name, matching the serde representation.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Backlog => "backlog",
            TaskStatus::Todo => "todo",
            TaskStatus::Doing => "doing",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Done => "done",
            TaskStatus::Cancelled => "cancelled",
        }
    }
}

/// How a project is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectStatus {
    #[default]
    Active,
    /// Real, but not being worked on now.
    Paused,
    Done,
    /// Out of the way. Still queryable, never listed by default.
    Archived,
}

impl ProjectStatus {
    pub fn is_open(self) -> bool {
        matches!(self, ProjectStatus::Active | ProjectStatus::Paused)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ProjectStatus::Active => "active",
            ProjectStatus::Paused => "paused",
            ProjectStatus::Done => "done",
            ProjectStatus::Archived => "archived",
        }
    }
}

/// How much this matters.
///
/// `None` is a real value and the default: most tasks do not deserve a
/// priority, and a scheme where everything must be triaged on capture is a
/// scheme people stop capturing into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Priority {
    #[default]
    None,
    Low,
    Medium,
    High,
    Urgent,
}

impl Priority {
    pub const ALL: [Priority; 5] =
        [Priority::None, Priority::Low, Priority::Medium, Priority::High, Priority::Urgent];

    /// Sort key. Higher is more urgent, so a backend can order on an integer
    /// column without knowing what the names mean.
    pub fn rank(self) -> i64 {
        match self {
            Priority::None => 0,
            Priority::Low => 1,
            Priority::Medium => 2,
            Priority::High => 3,
            Priority::Urgent => 4,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Priority::None => "none",
            Priority::Low => "low",
            Priority::Medium => "medium",
            Priority::High => "high",
            Priority::Urgent => "urgent",
        }
    }
}

/// A body of work with tasks under it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    /// Free-text description. Plain, deliberately — see the module docs.
    #[serde(default)]
    pub notes: String,
    /// `#rrggbb`, drives the project's accent the way a journal's does.
    pub color: String,
    /// A short emoji or glyph shown next to the name.
    pub icon: String,
    pub status: ProjectStatus,
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_date: Option<Date>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<Date>,
    /// Budgeted effort in minutes. Compare against the sum of the project's
    /// [`BlockKind::Actual`] blocks to learn how wrong you were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate_minutes: Option<u32>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// What this body of work is *for*. Inherited by every task and every
    /// block under it that does not say otherwise, which is what makes
    /// attribution one decision rather than a hundred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<Purpose>,
    /// Manual ordering in the sidebar; ties broken by `name`.
    #[serde(default)]
    pub sort_order: i32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<Timestamp>,
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: ProjectId::new(),
            name: name.into(),
            notes: String::new(),
            color: DEFAULT_PROJECT_COLORS[0].to_string(),
            icon: "\u{1f5c2}\u{fe0f}".into(), // card index dividers
            status: ProjectStatus::Active,
            priority: Priority::None,
            start_date: None,
            due_date: None,
            estimate_minutes: None,
            tags: Vec::new(),
            purpose: None,
            sort_order: 0,
            created_at: now,
            updated_at: now,
            completed_at: None,
        }
    }

    pub fn with_color(mut self, color: impl Into<String>) -> Self {
        self.color = color.into();
        self
    }

    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = icon.into();
        self
    }

    /// Move to `status`, keeping `completed_at` honest.
    pub fn set_status(&mut self, status: ProjectStatus) {
        self.status = status;
        self.completed_at = match status {
            ProjectStatus::Done => self.completed_at.or_else(|| Some(Timestamp::now())),
            _ => None,
        };
        self.updated_at = Timestamp::now();
    }

    /// Everything a text filter should look at.
    pub fn searchable_text(&self) -> String {
        let mut out = String::with_capacity(64);
        out.push_str(&self.name);
        out.push('\n');
        out.push_str(&self.notes);
        for tag in &self.tags {
            out.push('\n');
            out.push_str(tag);
        }
        out
    }
}

/// Project accents. The journal palette, so one vault has one set of colours.
pub const DEFAULT_PROJECT_COLORS: &[&str] = crate::model::DEFAULT_JOURNAL_COLORS;

/// One thing to do.
///
/// A task with no `project_id` is in the inbox — captured but not filed —
/// which is what makes capture cheap enough to actually do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: TaskId,
    /// `None` means the inbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    /// `None` means top level. Set, and this is a subtask of that task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<TaskId>,
    pub title: String,
    #[serde(default)]
    pub notes: String,
    pub status: TaskStatus,
    pub priority: Priority,
    /// Earliest sensible start. A task is not "upcoming" before this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_date: Option<Date>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<Date>,
    /// Time of day the deadline bites. Meaningless without `due_date`, and
    /// separate from it so that "due Friday" does not have to invent an hour
    /// — an all-day deadline and a 4pm one are genuinely different things.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_time: Option<Time>,
    /// Expected effort in minutes. Also the default length of a time block
    /// scheduled from this task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate_minutes: Option<u32>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// What this task is *for*. `None` means "whatever the project is for",
    /// which is the usual case and the reason the field is cheap to leave
    /// alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<Purpose>,
    /// Manual ordering within its board column / list section.
    #[serde(default)]
    pub sort_order: i32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// Set when the task reached [`TaskStatus::Done`], cleared if it comes
    /// back out. The "what did I finish this week" column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<Timestamp>,
}

impl Task {
    pub fn new(title: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: TaskId::new(),
            project_id: None,
            parent_id: None,
            title: title.into(),
            notes: String::new(),
            status: TaskStatus::Todo,
            priority: Priority::None,
            start_date: None,
            due_date: None,
            due_time: None,
            estimate_minutes: None,
            tags: Vec::new(),
            purpose: None,
            sort_order: 0,
            created_at: now,
            updated_at: now,
            completed_at: None,
        }
    }

    pub fn in_project(mut self, project: impl Into<Option<ProjectId>>) -> Self {
        self.project_id = project.into();
        self
    }

    pub fn under(mut self, parent: impl Into<Option<TaskId>>) -> Self {
        self.parent_id = parent.into();
        self
    }

    pub fn is_subtask(&self) -> bool {
        self.parent_id.is_some()
    }

    /// Move to `status`, keeping `completed_at` honest.
    ///
    /// Reopening a finished task clears the completion stamp rather than
    /// leaving a date that would make the analytics count it twice.
    pub fn set_status(&mut self, status: TaskStatus) {
        self.status = status;
        self.completed_at = match status {
            TaskStatus::Done => self.completed_at.or_else(|| Some(Timestamp::now())),
            _ => None,
        };
        self.updated_at = Timestamp::now();
    }

    /// Is the deadline in the past, as of `today`? Open tasks only: a
    /// finished task is never overdue, whenever it was due.
    pub fn is_overdue(&self, today: Date) -> bool {
        self.status.is_open() && self.due_date.is_some_and(|d| d < today)
    }

    /// Should this appear in "Today"? Anything open that is due on or before
    /// today, and has started.
    pub fn is_due_by(&self, today: Date) -> bool {
        self.status.is_open()
            && self.due_date.is_some_and(|d| d <= today)
            && self.start_date.is_none_or(|s| s <= today)
    }

    /// Everything a text filter should look at.
    pub fn searchable_text(&self) -> String {
        let mut out = String::with_capacity(64);
        out.push_str(&self.title);
        out.push('\n');
        out.push_str(&self.notes);
        for tag in &self.tags {
            out.push('\n');
            out.push_str(tag);
        }
        out
    }
}

/// What a block of time was spent on.
///
/// `Adhoc` is not a placeholder: a calendar has to hold the dentist
/// appointment as well as the work, and giving it a home here means the
/// calendar does not need its own storage layer when it arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BlockSubject {
    Task {
        id: TaskId,
    },
    Project {
        id: ProjectId,
    },
    /// Time that belongs to no task: a meeting, lunch, the commute.
    Adhoc,
}

impl BlockSubject {
    pub fn task_id(&self) -> Option<TaskId> {
        match self {
            BlockSubject::Task { id } => Some(*id),
            _ => None,
        }
    }

    pub fn project_id(&self) -> Option<ProjectId> {
        match self {
            BlockSubject::Project { id } => Some(*id),
            _ => None,
        }
    }
}

/// Intention or record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlockKind {
    /// "I mean to do this then." Lives on the calendar ahead of time.
    #[default]
    Planned,
    /// "I did this then." The row the time-spent reports add up.
    Actual,
}

impl BlockKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BlockKind::Planned => "planned",
            BlockKind::Actual => "actual",
        }
    }
}

/// A span of time attached to a subject.
///
/// The instants are absolute ([`Timestamp`]) so that arithmetic — durations,
/// overlaps, totals — is unambiguous, and `local_date` is carried alongside
/// as the day the block is *filed under* in the author's zone. That is the
/// same split [`crate::model::Entry`] makes between `created_at` and
/// `local_date`, and for the same reason: a backend can answer "show me this
/// week" as an index scan without knowing anything about time zones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeBlock {
    pub id: BlockId,
    pub subject: BlockSubject,
    /// Label for the block. Empty means "use the subject's own name", which
    /// is the usual case for a block scheduled from a task.
    #[serde(default)]
    pub title: String,
    pub start: Timestamp,
    pub end: Timestamp,
    /// The calendar day this is filed under, in `tz`.
    pub local_date: Date,
    /// IANA time zone, e.g. `Europe/Berlin`.
    pub tz: String,
    /// Occupies the whole day rather than a slot in it. `start` and `end`
    /// still bound it, so totals and range queries need no special case.
    #[serde(default)]
    pub all_day: bool,
    pub kind: BlockKind,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// What this hour was *for*. `None` falls through to the task's, then
    /// the project's — see [`crate::purpose`] for the chain. An `Adhoc`
    /// block with a purpose set directly is how the dentist appointment
    /// lands under looking after yourself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<Purpose>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl TimeBlock {
    /// A block running `minutes` from `start`, filed under the local day
    /// `start` falls on in `tz`.
    pub fn new(subject: BlockSubject, start: Timestamp, minutes: u32, tz: &str) -> Self {
        let now = Timestamp::now();
        let end = start + jiff::SignedDuration::from_mins(i64::from(minutes));
        Self {
            id: BlockId::new(),
            subject,
            title: String::new(),
            start,
            end,
            local_date: crate::model::local_date_in(start, tz),
            tz: tz.to_string(),
            all_day: false,
            kind: BlockKind::Planned,
            notes: String::new(),
            tags: Vec::new(),
            purpose: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// File this block under a goal or a role directly, overriding whatever
    /// it would otherwise inherit.
    pub fn for_purpose(mut self, purpose: Purpose) -> Self {
        self.purpose = Some(purpose);
        self
    }

    pub fn of_kind(mut self, kind: BlockKind) -> Self {
        self.kind = kind;
        self
    }

    /// Length in whole minutes, floored, and never negative — an end before
    /// its start is nonsense that should read as zero rather than underflow
    /// a total.
    pub fn minutes(&self) -> u32 {
        let secs = self.end.as_second() - self.start.as_second();
        u32::try_from(secs.max(0) / 60).unwrap_or(u32::MAX)
    }

    /// Do two blocks cover any of the same instant? Touching at an endpoint
    /// does not count, so back-to-back blocks do not read as a clash.
    pub fn overlaps(&self, other: &TimeBlock) -> bool {
        self.start < other.end && other.start < self.end
    }

    /// Reject a block that could not be drawn or totalled.
    pub fn validate(&self) -> crate::Result<()> {
        if self.end < self.start {
            return Err(crate::Error::Invalid("a time block cannot end before it starts".into()));
        }
        Ok(())
    }
}

/// How much is outstanding in one project.
///
/// A separate roll-up rather than something the interface counts for itself,
/// because the interface only ever holds the tasks currently on screen -- so
/// its own count would say "1" for a project with forty tasks in it, purely
/// because one of them happens to be due today. Every column this reads is
/// in the clear, so it decrypts nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTaskCount {
    /// `None` is the inbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    /// Tasks neither done nor cancelled, at every level.
    pub open: u64,
}

/// Counts for the todo header and, later, the analytics view.
///
/// Everything here is *as of a day*, because that is what every number a
/// todo app shows actually means: "outstanding" and "overdue" are questions
/// about a calendar date, and which date is the caller's to decide -- the
/// core has no business guessing a time zone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStats {
    pub projects: u64,
    /// Projects that are neither done nor archived.
    pub active_projects: u64,
    pub tasks: u64,
    /// Tasks that are neither done nor cancelled.
    pub open_tasks: u64,
    pub done_tasks: u64,
    pub blocks: u64,
    /// Total minutes across every [`BlockKind::Actual`] block.
    pub logged_minutes: u64,
    /// Total minutes across every [`BlockKind::Planned`] block.
    pub planned_minutes: u64,
    /// Open tasks due on or before the day the stats were taken. What the
    /// "Today" list holds.
    #[serde(default)]
    pub due_today: u64,
    /// Open tasks whose deadline has already passed. A subset of
    /// `due_today`, counted separately because it is the more alarming half.
    #[serde(default)]
    pub overdue: u64,
    /// Outstanding work per project, for the sidebar. Projects with nothing
    /// open are omitted rather than listed as zero.
    #[serde(default)]
    pub open_by_project: Vec<ProjectTaskCount>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::{date, time};

    #[test]
    fn a_new_task_is_open_and_unfiled() {
        let t = Task::new("write the thing");
        assert!(t.status.is_open());
        assert_eq!(t.project_id, None, "capture should not demand a project");
        assert_eq!(t.parent_id, None);
        assert_eq!(t.completed_at, None);
    }

    #[test]
    fn finishing_a_task_stamps_it_and_reopening_clears_the_stamp() {
        let mut t = Task::new("x");
        t.set_status(TaskStatus::Done);
        let stamped = t.completed_at.expect("done tasks carry a completion time");

        // Marking it done twice must not move the stamp: "finished on
        // Tuesday" should not become "finished on Friday" because a board
        // re-saved the card.
        t.set_status(TaskStatus::Done);
        assert_eq!(t.completed_at, Some(stamped));

        t.set_status(TaskStatus::Doing);
        assert_eq!(t.completed_at, None, "a reopened task is not a finished one");
    }

    #[test]
    fn cancelled_is_closed_but_not_done() {
        let mut t = Task::new("x");
        t.set_status(TaskStatus::Cancelled);
        assert!(!t.status.is_open());
        assert_eq!(t.completed_at, None, "cancelled work was never completed");
    }

    #[test]
    fn overdue_only_applies_to_open_tasks() {
        let today = date(2026, 3, 10);
        let mut t = Task::new("late");
        t.due_date = Some(date(2026, 3, 9));
        assert!(t.is_overdue(today));

        t.set_status(TaskStatus::Done);
        assert!(!t.is_overdue(today), "a finished task is never overdue");
    }

    #[test]
    fn a_task_that_has_not_started_is_not_due_yet() {
        let today = date(2026, 3, 10);
        let mut t = Task::new("deferred");
        t.due_date = Some(date(2026, 3, 10));
        t.start_date = Some(date(2026, 3, 15));
        assert!(!t.is_due_by(today), "a deferred task should not surface early");

        t.start_date = Some(date(2026, 3, 1));
        assert!(t.is_due_by(today));
    }

    #[test]
    fn due_time_is_optional_and_independent_of_the_date() {
        let mut t = Task::new("call the bank");
        t.due_date = Some(date(2026, 3, 10));
        assert_eq!(t.due_time, None, "an all-day deadline invents no hour");
        t.due_time = Some(time(16, 30, 0, 0));
        let round: Task = serde_json::from_slice(&serde_json::to_vec(&t).unwrap()).unwrap();
        assert_eq!(round, t);
    }

    #[test]
    fn block_length_is_minutes_and_never_negative() {
        let start = Timestamp::from_second(1_700_000_000).unwrap();
        let b = TimeBlock::new(BlockSubject::Adhoc, start, 90, "UTC");
        assert_eq!(b.minutes(), 90);
        assert!(b.validate().is_ok());

        let mut backwards = b.clone();
        backwards.end = start - jiff::SignedDuration::from_mins(30);
        assert_eq!(backwards.minutes(), 0, "a backwards block must not underflow a total");
        assert!(backwards.validate().is_err());
    }

    #[test]
    fn blocks_that_merely_touch_do_not_overlap() {
        let start = Timestamp::from_second(1_700_000_000).unwrap();
        let first = TimeBlock::new(BlockSubject::Adhoc, start, 60, "UTC");
        let next = TimeBlock::new(BlockSubject::Adhoc, first.end, 60, "UTC");
        assert!(!first.overlaps(&next), "back to back is not a clash");

        let clashing = TimeBlock::new(
            BlockSubject::Adhoc,
            start + jiff::SignedDuration::from_mins(30),
            60,
            "UTC",
        );
        assert!(first.overlaps(&clashing));
        assert!(clashing.overlaps(&first), "overlap is symmetric");
    }

    #[test]
    fn a_block_is_filed_under_the_local_day_not_the_utc_one() {
        // 23:30 on 9 March in New York is already 10 March in UTC.
        let start = "2026-03-10T03:30:00Z".parse::<Timestamp>().unwrap();
        let b = TimeBlock::new(BlockSubject::Adhoc, start, 30, "America/New_York");
        assert_eq!(b.local_date, date(2026, 3, 9));
    }

    #[test]
    fn the_subject_says_what_the_time_went_to() {
        let task = TaskId::new();
        let on_task = BlockSubject::Task { id: task };
        assert_eq!(on_task.task_id(), Some(task));
        assert_eq!(on_task.project_id(), None);
        assert_eq!(BlockSubject::Adhoc.task_id(), None);
    }

    #[test]
    fn priorities_rank_in_the_order_they_are_written() {
        let mut ps = vec![Priority::Urgent, Priority::None, Priority::High];
        ps.sort_by_key(|p| p.rank());
        assert_eq!(ps, [Priority::None, Priority::High, Priority::Urgent]);
        assert!(Priority::default() == Priority::None, "capture must not demand triage");
    }

    #[test]
    fn wire_names_match_the_serde_representation() {
        // The interface switches on these strings and the SQLite backend
        // indexes them, so a rename that only touched one of the two would
        // be a silent break.
        for s in TaskStatus::ALL {
            assert_eq!(serde_json::to_string(&s).unwrap(), format!("\"{}\"", s.as_str()));
        }
        for p in Priority::ALL {
            assert_eq!(serde_json::to_string(&p).unwrap(), format!("\"{}\"", p.as_str()));
        }
        for k in [BlockKind::Planned, BlockKind::Actual] {
            assert_eq!(serde_json::to_string(&k).unwrap(), format!("\"{}\"", k.as_str()));
        }
        for s in [
            ProjectStatus::Active,
            ProjectStatus::Paused,
            ProjectStatus::Done,
            ProjectStatus::Archived,
        ] {
            assert_eq!(serde_json::to_string(&s).unwrap(), format!("\"{}\"", s.as_str()));
        }
    }

    #[test]
    fn an_adhoc_block_survives_a_json_round_trip() {
        let start = Timestamp::from_second(1_700_000_000).unwrap();
        for subject in [
            BlockSubject::Adhoc,
            BlockSubject::Task { id: TaskId::new() },
            BlockSubject::Project { id: ProjectId::new() },
        ] {
            let b = TimeBlock::new(subject, start, 45, "Europe/Berlin");
            let round: TimeBlock =
                serde_json::from_slice(&serde_json::to_vec(&b).unwrap()).unwrap();
            assert_eq!(round, b);
        }
    }

    #[test]
    fn searchable_text_covers_notes_and_tags() {
        let mut t = Task::new("book flights");
        t.notes = "aisle seat if possible".into();
        t.tags = vec!["travel".into()];
        let text = t.searchable_text();
        assert!(text.contains("aisle"));
        assert!(text.contains("travel"));
    }
}
