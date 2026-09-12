//! The task domain: projects, tasks and blocks of time.
//!
//! The clear/sealed split described at the top of the crate applies here in
//! its own way: `status`, `priority`, `due_date`, `project_id` and
//! `parent_id` are index columns in the clear, so a board filters and a
//! calendar scans a week without decrypting anything, while titles,
//! descriptions and tags stay sealed.

use everyday_core::error::{Error, Result};
use everyday_core::id::{BlockId, ProjectId, TaskId};
use everyday_core::purpose::Purpose;
use everyday_core::store::tasks::{
    BlockQuery, ParentScope, ProjectScope, TaskQuery, TaskSort, TaskStore, block_aad, project_aad,
    task_aad,
};
use everyday_core::task::{
    BlockKind, Project, ProjectTaskCount, Task, TaskStats, TaskStatus, TimeBlock,
};

use crate::conn::{Sql, SqlExt, ToValue, Value, Where};
use crate::purpose::{RecordKind, forget_purposes};
use crate::record::Record;
use crate::{SqlStore, date_str, id_str, to_us, vals};

impl Record for Project {
    const TABLE: &'static str = "projects";
    const KIND: &'static str = "project";
    type Id = ProjectId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        project_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("status", self.status.as_str().to_value()),
            ("priority", self.priority.rank().to_value()),
            ("due_date", date_str(self.due_date).to_value()),
            ("sort_order", self.sort_order.to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
            ("completed_us", self.completed_at.map(to_us).to_value()),
        ]
    }

    fn purpose_kind() -> Option<RecordKind> {
        Some(RecordKind::Project)
    }

    fn purpose(&self) -> Option<&Purpose> {
        self.purpose.as_ref()
    }
}

impl Record for Task {
    const TABLE: &'static str = "tasks";
    const KIND: &'static str = "task";
    type Id = TaskId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        task_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("project_id", id_str(self.project_id).to_value()),
            ("parent_id", id_str(self.parent_id).to_value()),
            ("status", self.status.as_str().to_value()),
            ("priority", self.priority.rank().to_value()),
            ("start_date", date_str(self.start_date).to_value()),
            ("due_date", date_str(self.due_date).to_value()),
            ("sort_order", self.sort_order.to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
            ("completed_us", self.completed_at.map(to_us).to_value()),
        ]
    }

    fn purpose_kind() -> Option<RecordKind> {
        Some(RecordKind::Task)
    }

    fn purpose(&self) -> Option<&Purpose> {
        self.purpose.as_ref()
    }
}

impl Record for TimeBlock {
    const TABLE: &'static str = "time_blocks";
    const KIND: &'static str = "block";
    type Id = BlockId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        block_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("task_id", id_str(self.subject.task_id()).to_value()),
            ("project_id", id_str(self.subject.project_id()).to_value()),
            ("local_date", self.local_date.to_string().to_value()),
            ("start_us", to_us(self.start).to_value()),
            ("end_us", to_us(self.end).to_value()),
            ("kind", self.kind.as_str().to_value()),
        ]
    }

    fn purpose_kind() -> Option<RecordKind> {
        Some(RecordKind::Block)
    }

    fn purpose(&self) -> Option<&Purpose> {
        self.purpose.as_ref()
    }
}

impl TaskStore for SqlStore {
    // ---- projects -------------------------------------------------------

    fn list_projects(&self) -> Result<Vec<Project>> {
        let rows = self
            .read()
            .records("SELECT id, data FROM projects ORDER BY sort_order, created_us", &[])?;
        self.collect(rows, project_aad)
    }

    fn get_project(&self, id: ProjectId) -> Result<Project> {
        self.get(id)
    }

    fn put_project(&self, p: &Project) -> Result<()> {
        self.upsert(p)
    }

    fn delete_project(&self, id: ProjectId) -> Result<()> {
        let mut conn = self.write();
        let mut tx = conn.begin()?;

        // Its tasks, plus anything nested under them -- a subtask filed into
        // a different project is still doomed with its parent.
        let roots =
            tx.query("SELECT id FROM tasks WHERE project_id = ?1", &vals![id.to_string()])?;
        let mut doomed: Vec<String> = Vec::new();
        for root in roots {
            let root = TaskId::parse(&root.text(0)?).map_err(|e| Error::Invalid(e.to_string()))?;
            for descendant in SqlStore::subtree(tx.as_mut(), root)? {
                if !doomed.contains(&descendant) {
                    doomed.push(descendant);
                }
            }
        }
        SqlStore::purge_tasks(tx.as_mut(), &doomed)?;

        // Time booked against the project itself, not against its tasks.
        tx.execute("DELETE FROM time_blocks WHERE project_id = ?1", &vals![id.to_string()])?;
        tx.execute("DELETE FROM projects WHERE id = ?1", &vals![id.to_string()])?;
        forget_purposes(tx.as_mut(), RecordKind::Project, &[id.to_string()])?;
        tx.commit()
    }

    // ---- tasks ----------------------------------------------------------

    fn list_tasks(&self, query: &TaskQuery) -> Result<Vec<Task>> {
        // The same split the entry list makes: push down what the indexes
        // can answer, and finish in memory when a filter or an ordering
        // depends on something that only exists once decrypted -- here the
        // tags, the free-text match and the title ordering.
        let needs_memory_pass = !query.tags.is_empty()
            || !query.text.trim().is_empty()
            || query.sort == TaskSort::TitleAsc;

        let mut w = Where::new();
        match query.project {
            ProjectScope::Any => {}
            ProjectScope::Inbox => w = w.is_null("project_id"),
            ProjectScope::Project { id } => w = w.eq("project_id", id.to_string()),
        }
        match query.parent {
            ParentScope::Any => {}
            ParentScope::TopLevel => w = w.is_null("parent_id"),
            ParentScope::Of { id } => w = w.eq("parent_id", id.to_string()),
        }
        if !query.statuses.is_empty() {
            w = w.in_list("status", query.statuses.iter().map(|s| s.as_str()));
        }
        if let Some(min) = query.priority_at_least {
            w = w.gte("priority", min.rank());
        }
        if let Some(want) = query.has_due {
            w = if want { w.not_null("due_date") } else { w.is_null("due_date") };
        }
        // NULL compares as neither >= nor <=, so an undated task falls out
        // of a date window here for the same reason it does in
        // `TaskQuery::matches`. That agreement is what the conformance suite
        // pins down.
        if let Some(from) = query.due_from {
            w = w.gte("due_date", from.to_string());
        }
        if let Some(to) = query.due_to {
            w = w.lte("due_date", to.to_string());
        }
        let (where_sql, args) = w.finish();
        let mut sql = format!("SELECT id, data FROM tasks WHERE {where_sql}");

        if !needs_memory_pass {
            self.page(
                &mut sql,
                match query.sort {
                    TaskSort::Manual => "sort_order ASC, created_us ASC",
                    // "Undated last" has to be said out loud. The two
                    // databases put NULLs at opposite ends of an ASC sort,
                    // and neither default is the one wanted -- so the
                    // leading boolean decides it and they agree.
                    TaskSort::DueAsc => "due_date IS NULL, due_date ASC, sort_order ASC",
                    TaskSort::PriorityDesc => "priority DESC, sort_order ASC",
                    TaskSort::CreatedDesc => "created_us DESC",
                    TaskSort::UpdatedDesc => "updated_us DESC",
                    TaskSort::CompletedDesc => "completed_us IS NULL, completed_us DESC",
                    TaskSort::TitleAsc => unreachable!("handled by the in-memory pass"),
                },
                query.limit,
                query.offset,
            );
        }

        let rows = self.read().records(&sql, &args)?;
        let tasks: Vec<Task> = self.collect(rows, task_aad)?;
        Ok(if needs_memory_pass { query.apply(tasks) } else { tasks })
    }

    fn get_task(&self, id: TaskId) -> Result<Task> {
        self.get(id)
    }

    fn put_task(&self, t: &Task) -> Result<()> {
        self.put_tasks(std::slice::from_ref(t))
    }

    fn put_tasks(&self, tasks: &[Task]) -> Result<()> {
        self.upsert_many(tasks)
    }

    fn delete_task(&self, id: TaskId) -> Result<()> {
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let doomed = SqlStore::subtree(tx.as_mut(), id)?;
        SqlStore::purge_tasks(tx.as_mut(), &doomed)?;
        tx.commit()
    }

    // ---- time blocks ----------------------------------------------------

    fn list_blocks(&self, query: &BlockQuery) -> Result<Vec<TimeBlock>> {
        let mut w = Where::new();
        if let Some(from) = query.from {
            w = w.gte("local_date", from.to_string());
        }
        if let Some(to) = query.to {
            w = w.lte("local_date", to.to_string());
        }
        if let Some(task) = query.task_id {
            w = w.eq("task_id", task.to_string());
        }
        if let Some(project) = query.project_id {
            w = w.eq("project_id", project.to_string());
        }
        if let Some(kind) = query.kind {
            w = w.eq("kind", kind.as_str());
        }
        let (where_sql, args) = w.finish();
        let mut sql = format!("SELECT id, data FROM time_blocks WHERE {where_sql}");
        self.page(&mut sql, "start_us ASC, end_us ASC", query.limit, 0);

        let rows = self.read().records(&sql, &args)?;
        self.collect(rows, block_aad)
    }

    fn get_block(&self, id: BlockId) -> Result<TimeBlock> {
        self.get(id)
    }

    fn put_block(&self, b: &TimeBlock) -> Result<()> {
        self.upsert(b)
    }

    fn delete_block(&self, id: BlockId) -> Result<()> {
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        tx.execute("DELETE FROM time_blocks WHERE id = ?1", &vals![id.to_string()])?;
        forget_purposes(tx.as_mut(), RecordKind::Block, &[id.to_string()])?;
        tx.commit()?;
        Ok(())
    }

    // ---- housekeeping ---------------------------------------------------

    fn task_stats(&self, today: jiff::civil::Date) -> Result<TaskStats> {
        // Every count here reads a clear column, so the whole panel costs
        // one pass over the indexes and decrypts nothing.
        let mut conn = self.read();

        let open: Vec<&str> =
            TaskStatus::ALL.iter().filter(|s| s.is_open()).map(|s| s.as_str()).collect();
        let open_list = open.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(",");

        // One grouped scan of `tasks_by_project`, which is exactly the index
        // this is shaped like. `project_id` and `status` are both clear
        // columns, so the sidebar's counts cost no decryption at all.
        let mut open_by_project = Vec::new();
        let rows = conn.query(
            &format!(
                "SELECT project_id, COUNT(*) FROM tasks
                 WHERE status IN ({open_list}) GROUP BY project_id"
            ),
            &[],
        )?;
        for row in rows {
            let project_id = match row.opt_text(0)? {
                Some(id) => Some(ProjectId::parse(&id).map_err(|e| Error::Invalid(e.to_string()))?),
                None => None,
            };
            open_by_project.push(ProjectTaskCount { project_id, open: row.u64(1)? });
        }

        // Both bounds compare against a clear `due_date` column, and NULL
        // compares as neither -- so an undated task is correctly outside
        // both, exactly as it is outside `TaskQuery`'s date windows.
        let today = vals![today.to_string()];
        let due_today = conn.scalar_i64(
            &format!("SELECT COUNT(*) FROM tasks WHERE status IN ({open_list}) AND due_date <= ?1"),
            &today,
        )?;
        let overdue = conn.scalar_i64(
            &format!("SELECT COUNT(*) FROM tasks WHERE status IN ({open_list}) AND due_date < ?1"),
            &today,
        )?;

        // Summed in microseconds and divided here rather than in SQL. `SUM`
        // over an integer column is exact in SQLite and a `numeric` in
        // Postgres, and dividing a `numeric` does not truncate -- so the
        // arithmetic moves to Rust, where it means one thing. The `CAST` is
        // the other half of that: it brings the sum back to an integer both
        // drivers can read, and the value is exact so nothing is rounded.
        let mut minutes = |kind: BlockKind| -> Result<u64> {
            let us = conn.scalar_i64(
                &format!(
                    "SELECT CAST(COALESCE(SUM({greatest}(end_us - start_us, 0)), 0) AS BIGINT)
                     FROM time_blocks WHERE kind = ?1",
                    greatest = self.dialect.greatest(),
                ),
                &vals![kind.as_str()],
            )?;
            Ok((us.max(0) / 1_000_000 / 60) as u64)
        };
        let planned_minutes = minutes(BlockKind::Planned)?;
        let logged_minutes = minutes(BlockKind::Actual)?;

        let mut count =
            |sql: String| -> Result<u64> { Ok(conn.scalar_i64(&sql, &[])?.max(0) as u64) };
        Ok(TaskStats {
            open_by_project,
            due_today: due_today.max(0) as u64,
            overdue: overdue.max(0) as u64,
            projects: count("SELECT COUNT(*) FROM projects".into())?,
            active_projects: count(
                "SELECT COUNT(*) FROM projects WHERE status IN ('active', 'paused')".into(),
            )?,
            tasks: count("SELECT COUNT(*) FROM tasks".into())?,
            open_tasks: count(format!("SELECT COUNT(*) FROM tasks WHERE status IN ({open_list})"))?,
            done_tasks: count("SELECT COUNT(*) FROM tasks WHERE status = 'done'".into())?,
            blocks: count("SELECT COUNT(*) FROM time_blocks".into())?,
            planned_minutes,
            logged_minutes,
        })
    }
}
