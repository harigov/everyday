//! Tasks, projects and time blocks.
//!
//! The second domain. Every method here goes through [`Vault::with_tasks`],
//! which fails with `unsupported` on a backend that holds journals only, so a
//! Markdown vault reports the absence structurally rather than panicking or
//! silently returning nothing.

use super::Vault;
use super::session::Domain;
use crate::error::{Error, Result};
use crate::id::{BlockId, ProjectId, TaskId};
use crate::store::EntryQuery;
use crate::store::tasks::{BlockQuery, TaskQuery, TaskStore};
use crate::task::{Project, Task, TaskStats, TimeBlock};

impl Vault {
    /// Does this vault's backend store tasks at all?
    pub fn supports_tasks(&self) -> bool {
        self.with_tasks(|_| Ok(())).is_ok()
    }

    /// Run `f` against the task store, or explain that there isn't one.
    fn with_tasks<T>(&self, f: impl FnOnce(&dyn TaskStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Tasks, |s| s.tasks().map(f))
    }

    pub fn projects(&self) -> Result<Vec<Project>> {
        self.with_tasks(|t| {
            let mut ps = t.list_projects()?;
            ps.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)));
            Ok(ps)
        })
    }

    pub fn project(&self, id: ProjectId) -> Result<Project> {
        self.with_tasks(|t| t.get_project(id))
    }

    pub fn save_project(&self, project: &Project) -> Result<()> {
        self.writable()?;
        self.with_tasks(|t| t.put_project(project))
    }

    /// Delete a project, its tasks and their time blocks.
    pub fn delete_project(&self, id: ProjectId) -> Result<()> {
        self.writable()?;
        self.with_tasks(|t| t.delete_project(id))
    }

    pub fn tasks(&self, query: &TaskQuery) -> Result<Vec<Task>> {
        self.with_tasks(|t| t.list_tasks(query))
    }

    pub fn task(&self, id: TaskId) -> Result<Task> {
        self.with_tasks(|t| t.get_task(id))
    }

    pub fn save_task(&self, task: &Task) -> Result<()> {
        self.writable()?;
        if task.title.trim().is_empty() {
            return Err(Error::Invalid("a task needs a title".into()));
        }
        if task.parent_id == Some(task.id) {
            return Err(Error::Invalid("a task cannot be its own subtask".into()));
        }
        self.with_tasks(|t| t.put_task(task))
    }

    /// Write several tasks as one operation. This is what a board reorder
    /// is: dragging one card renumbers everything below it in two columns.
    pub fn save_tasks(&self, tasks: &[Task]) -> Result<()> {
        self.writable()?;
        for t in tasks {
            if t.title.trim().is_empty() {
                return Err(Error::Invalid("a task needs a title".into()));
            }
        }
        self.with_tasks(|s| s.put_tasks(tasks))
    }

    /// Delete a task, its subtasks and their time blocks.
    pub fn delete_task(&self, id: TaskId) -> Result<()> {
        self.writable()?;
        self.with_tasks(|t| t.delete_task(id))
    }

    pub fn blocks(&self, query: &BlockQuery) -> Result<Vec<TimeBlock>> {
        self.with_tasks(|t| t.list_blocks(query))
    }

    pub fn block(&self, id: BlockId) -> Result<TimeBlock> {
        self.with_tasks(|t| t.get_block(id))
    }

    pub fn save_block(&self, block: &TimeBlock) -> Result<()> {
        self.writable()?;
        block.validate()?;
        self.with_tasks(|t| t.put_block(block))
    }

    pub fn delete_block(&self, id: BlockId) -> Result<()> {
        self.writable()?;
        self.with_tasks(|t| t.delete_block(id))
    }

    /// Counts for the sidebar, as of the calendar day `today`.
    pub fn task_stats(&self, today: jiff::civil::Date) -> Result<TaskStats> {
        self.with_tasks(|t| t.task_stats(today))
    }

    /// Every tag used on an entry, most used first, ties broken
    /// alphabetically.
    ///
    /// The journal-domain twin of [`Vault::task_tags`], and it lives here for
    /// the same reason that one does: which tags exist and how often they are
    /// used is a question about the vault's contents, not about the shell
    /// asking. It had been a loop in the desktop shell's command handler,
    /// which meant the CLI could not answer it and the two front ends were
    /// one copy-paste away from sorting the list differently.
    pub fn entry_tags(&self) -> Result<Vec<(String, u32)>> {
        let mut counts: std::collections::BTreeMap<String, u32> = Default::default();
        for e in self.entries(&EntryQuery::default())? {
            for tag in e.tags {
                *counts.entry(tag).or_default() += 1;
            }
        }
        let mut out: Vec<(String, u32)> = counts.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(out)
    }

    /// Every tag in the task domain with how often it is used, most used
    /// first, ties broken alphabetically.
    ///
    /// Tags are inside the sealed payloads -- the same trade the journal
    /// makes -- so this is a decrypt-and-count pass rather than an index
    /// lookup. That is affordable at the scale a person's todo list reaches,
    /// and it is what keeps the database file from listing what someone is
    /// working on to anyone who opens it.
    pub fn task_tags(&self) -> Result<Vec<(String, u32)>> {
        self.with_tasks(|t| {
            let mut counts: std::collections::BTreeMap<String, u32> = Default::default();
            let mut bump = |tags: &[String]| {
                for tag in tags {
                    *counts.entry(tag.clone()).or_default() += 1;
                }
            };
            for p in t.list_projects()? {
                bump(&p.tags);
            }
            for task in t.list_tasks(&TaskQuery::default())? {
                bump(&task.tags);
            }
            for b in t.list_blocks(&BlockQuery::default())? {
                bump(&b.tags);
            }
            let mut out: Vec<(String, u32)> = counts.into_iter().collect();
            out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            Ok(out)
        })
    }
}
