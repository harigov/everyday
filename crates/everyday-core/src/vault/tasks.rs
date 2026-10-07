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
use crate::record::RecordKind;
use crate::recurrence::Recurrence;
use crate::store::EntryQuery;
use crate::store::tasks::{BlockQuery, TaskQuery, TaskStore};
use crate::task::{BlockKind, BlockSeries, Project, SeriesScope, Task, TaskStats, TimeBlock};

/// How far ahead of its first block a repeating block of your own is
/// written out -- the same two years an account calendar's sync reaches
/// forward, so "this repeats" means the same distance on every calendar the
/// grid draws.
pub const BLOCK_SERIES_DAYS: i64 = 730;
/// The most blocks one series writes, whatever its rule -- a daily repeat
/// over [`BLOCK_SERIES_DAYS`] fits, with room.
pub const BLOCK_SERIES_CAP: usize = 750;

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
        self.with_tasks(|t| t.put_project(project))?;
        self.wrote(RecordKind::Project, project.id);
        Ok(())
    }

    /// Delete a project, its tasks and their time blocks. Only the project
    /// itself is recorded as touched -- the cascade has no ids in hand here
    /// to name individually.
    pub fn delete_project(&self, id: ProjectId) -> Result<()> {
        self.writable()?;
        self.with_tasks(|t| t.delete_project(id))?;
        self.wrote(RecordKind::Project, id);
        Ok(())
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
        self.with_tasks(|t| t.put_task(task))?;
        self.wrote(RecordKind::Task, task.id);
        Ok(())
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
        self.with_tasks(|s| s.put_tasks(tasks))?;
        for t in tasks {
            self.wrote(RecordKind::Task, t.id);
        }
        Ok(())
    }

    /// Delete a task, its subtasks and their time blocks. Only the task
    /// itself is recorded as touched -- the cascade has no ids in hand here
    /// to name individually.
    pub fn delete_task(&self, id: TaskId) -> Result<()> {
        self.writable()?;
        self.with_tasks(|t| t.delete_task(id))?;
        self.wrote(RecordKind::Task, id);
        Ok(())
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
        self.with_tasks(|t| t.put_block(block))?;
        self.wrote(RecordKind::Block, block.id);
        Ok(())
    }

    pub fn delete_block(&self, id: BlockId) -> Result<()> {
        self.writable()?;
        self.with_tasks(|t| t.delete_block(id))?;
        self.wrote(RecordKind::Block, id);
        Ok(())
    }

    /// Save `block` as the start of a repeating series -- or, with `rule`
    /// `None`, as a block that no longer repeats -- and write out the rest
    /// of the series after it.
    ///
    /// What "the rest" replaces is every *later* block of the series `block`
    /// already belonged to, so the same call makes a series, changes how one
    /// repeats from here on, carries an edit to this block forward onto the
    /// ones after it, and stops one repeating. Earlier blocks are left alone,
    /// the way every calendar's "this and following" leaves them: they keep
    /// their old series, and that series simply ends where this one begins.
    ///
    /// Occurrences are written [`BLOCK_SERIES_DAYS`] ahead of `block` and no
    /// further -- see [`crate::task::BlockSeries`] for why a repeat of your
    /// own is rows rather than a rule. Every one of them is a plan: an hour
    /// that has not happened yet cannot be a record of what did.
    ///
    /// Answers every block written, `block` first.
    pub fn save_block_series(
        &self,
        block: &TimeBlock,
        rule: Option<&Recurrence>,
    ) -> Result<Vec<TimeBlock>> {
        self.writable()?;
        block.validate()?;
        if let Some(rule) = rule {
            rule.validate()?;
            if block.kind != BlockKind::Planned {
                return Err(Error::Invalid(
                    "only a plan can repeat -- a record of time spent happened once".into(),
                ));
            }
        }

        // Whatever series this block is in *on disk* is the one whose later
        // blocks are replaced -- not whatever the caller's copy says, which
        // may be one it is in the middle of changing.
        let stored = match self.block(block.id) {
            Ok(b) => Some(b),
            Err(Error::NotFound { .. }) => None,
            Err(e) => return Err(e),
        };
        let old_series = stored.as_ref().and_then(|b| b.series.clone()).or(block.series.clone());
        let from = stored.as_ref().map_or(block.start, |b| b.start.min(block.start));
        let doomed: Vec<BlockId> = match &old_series {
            Some(series) => self
                .series_members(series.id, crate::model::local_date_in(from, &block.tz), None)?
                .into_iter()
                .filter(|b| b.id != block.id && b.start >= from)
                .map(|b| b.id)
                .collect(),
            None => Vec::new(),
        };

        let now = jiff::Timestamp::now();
        let mut head = block.clone();
        head.updated_at = now;
        head.series = rule.map(|rule| BlockSeries { id: block.id, rule: rule.clone() });
        let mut written = vec![head.clone()];
        if let Some(series) = &head.series {
            let zone = jiff::tz::TimeZone::get(&head.tz).unwrap_or(jiff::tz::TimeZone::UTC);
            let seed = head.start.to_zoned(zone.clone()).datetime();
            let length = head.end.duration_since(head.start);
            let through = head
                .local_date
                .checked_add(jiff::Span::new().days(BLOCK_SERIES_DAYS))
                .unwrap_or(head.local_date);
            for at in series.rule.occurrences(seed, through, BLOCK_SERIES_CAP).into_iter().skip(1) {
                // The wall-clock time holds across a change of clocks:
                // "nine every Monday" is nine in October and in November.
                let Ok(start) = zone.to_ambiguous_zoned(at).later().map(|z| z.timestamp()) else {
                    continue;
                };
                let mut copy = head.clone();
                copy.id = BlockId::new();
                copy.start = start;
                copy.end = start + length;
                copy.local_date = at.date();
                copy.kind = BlockKind::Planned;
                copy.created_at = now;
                copy.updated_at = now;
                written.push(copy);
            }
        }

        self.with_tasks(|t| {
            t.delete_blocks(&doomed)?;
            t.put_blocks(&written)
        })?;
        for id in &doomed {
            self.wrote(RecordKind::Block, *id);
        }
        for b in &written {
            self.wrote(RecordKind::Block, b.id);
        }
        Ok(written)
    }

    /// Delete `id`'s series: every block of it from `id` on, or all of it.
    /// Answers the ids deleted. A block that does not repeat is refused
    /// rather than deleted alone -- that is [`Vault::delete_block`]'s job,
    /// and a caller that thought it was deleting a series should hear that
    /// it was not.
    pub fn delete_block_series(&self, id: BlockId, scope: SeriesScope) -> Result<Vec<BlockId>> {
        self.writable()?;
        let block = self.block(id)?;
        let series = block
            .series
            .clone()
            .ok_or_else(|| Error::Invalid("that block does not repeat".into()))?;
        let from = match scope {
            SeriesScope::Following => block.local_date,
            // The first block's day, when it is still there to ask; a series
            // never reaches further back than it reaches forward otherwise.
            SeriesScope::All => match self.block(series.id) {
                Ok(first) => first.local_date.min(block.local_date),
                Err(_) => block
                    .local_date
                    .checked_sub(jiff::Span::new().days(BLOCK_SERIES_DAYS))
                    .unwrap_or(block.local_date),
            },
        };
        let doomed: Vec<BlockId> = self
            .series_members(series.id, from, Some(block.local_date))?
            .into_iter()
            .filter(|b| scope == SeriesScope::All || b.start >= block.start || b.id == block.id)
            .map(|b| b.id)
            .collect();
        self.with_tasks(|t| t.delete_blocks(&doomed))?;
        for id in &doomed {
            self.wrote(RecordKind::Block, *id);
        }
        Ok(doomed)
    }

    /// Every block of series `series` filed on or after `from`, as far
    /// ahead as a series is ever written from `anchor` (or from `from`).
    fn series_members(
        &self,
        series: BlockId,
        from: jiff::civil::Date,
        anchor: Option<jiff::civil::Date>,
    ) -> Result<Vec<TimeBlock>> {
        let reach = anchor.unwrap_or(from).max(from);
        let to = reach.checked_add(jiff::Span::new().days(BLOCK_SERIES_DAYS + 31)).unwrap_or(reach);
        Ok(self
            .blocks(&BlockQuery::between(from, to))?
            .into_iter()
            .filter(|b| b.series.as_ref().is_some_and(|s| s.id == series))
            .collect())
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
