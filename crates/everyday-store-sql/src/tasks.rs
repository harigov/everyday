//! The task domain: projects, tasks and blocks of time.
//!
//! The clear/sealed split described at the top of the crate applies here in
//! its own way: `status`, `priority`, `due_date`, `project_id` and
//! `parent_id` are index columns in the clear, so a board filters and a
//! calendar scans a week without decrypting anything, while titles,
//! descriptions and tags stay sealed.

use everyday_core::error::{Error, Result};
use everyday_core::id::{BlockId, ProjectId, TaskId};
use everyday_core::store::tasks::{
    BlockQuery, ParentScope, ProjectScope, TaskQuery, TaskSort, TaskStore, block_aad, project_aad,
    task_aad,
};
use everyday_core::task::{
    BlockKind, Project, ProjectTaskCount, Task, TaskStats, TaskStatus, TimeBlock,
};

use crate::conn::{SqlExt, Value};
use crate::purpose::{RecordKind, forget_purposes, set_purpose};
use crate::{SqlStore, date_str, id_str, to_us, vals};

impl TaskStore for SqlStore {
    // ---- projects -------------------------------------------------------

    fn list_projects(&self) -> Result<Vec<Project>> {
        let rows = self
            .conn()
            .records("SELECT id, data FROM projects ORDER BY sort_order, created_us", &[])?;
        self.collect(rows, project_aad)
    }

    fn get_project(&self, id: ProjectId) -> Result<Project> {
        let sealed = self
            .conn()
            .sealed("SELECT data FROM projects WHERE id = ?1", &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found("project", id))?;
        self.unseal(&project_aad(id), &sealed)
    }

    fn put_project(&self, p: &Project) -> Result<()> {
        let data = self.seal(&project_aad(p.id), p)?;
        let mut conn = self.conn();
        let mut tx = conn.begin()?;
        tx.execute(
            "INSERT INTO projects
                (id, status, priority, due_date, sort_order, created_us, updated_us,
                 completed_us, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (id) DO UPDATE SET
                status = ?2, priority = ?3, due_date = ?4, sort_order = ?5,
                created_us = ?6, updated_us = ?7, completed_us = ?8, data = ?9",
            &vals![
                p.id.to_string(),
                p.status.as_str(),
                p.priority.rank(),
                date_str(p.due_date),
                p.sort_order,
                to_us(p.created_at),
                to_us(p.updated_at),
                p.completed_at.map(to_us),
                data,
            ],
        )?;
        set_purpose(tx.as_mut(), RecordKind::Project, &p.id.to_string(), p.purpose.as_ref())?;
        tx.commit()
    }

    fn delete_project(&self, id: ProjectId) -> Result<()> {
        let mut conn = self.conn();
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

        let mut sql = String::from("SELECT id, data FROM tasks WHERE 1=1");
        let mut args: Vec<Value> = Vec::new();

        match query.project {
            ProjectScope::Any => {}
            ProjectScope::Inbox => sql.push_str(" AND project_id IS NULL"),
            ProjectScope::Project { id } => {
                args.push(Value::Text(id.to_string()));
                sql.push_str(&format!(" AND project_id = ?{}", args.len()));
            }
        }
        match query.parent {
            ParentScope::Any => {}
            ParentScope::TopLevel => sql.push_str(" AND parent_id IS NULL"),
            ParentScope::Of { id } => {
                args.push(Value::Text(id.to_string()));
                sql.push_str(&format!(" AND parent_id = ?{}", args.len()));
            }
        }
        if !query.statuses.is_empty() {
            let mut holes = Vec::with_capacity(query.statuses.len());
            for status in &query.statuses {
                args.push(Value::Text(status.as_str().to_string()));
                holes.push(format!("?{}", args.len()));
            }
            sql.push_str(&format!(" AND status IN ({})", holes.join(",")));
        }
        if let Some(min) = query.priority_at_least {
            args.push(Value::Int(min.rank()));
            sql.push_str(&format!(" AND priority >= ?{}", args.len()));
        }
        if let Some(want) = query.has_due {
            sql.push_str(if want { " AND due_date IS NOT NULL" } else { " AND due_date IS NULL" });
        }
        // NULL compares as neither >= nor <=, so an undated task falls out
        // of a date window here for the same reason it does in
        // `TaskQuery::matches`. That agreement is what the conformance suite
        // pins down.
        if let Some(from) = query.due_from {
            args.push(Value::Text(from.to_string()));
            sql.push_str(&format!(" AND due_date >= ?{}", args.len()));
        }
        if let Some(to) = query.due_to {
            args.push(Value::Text(to.to_string()));
            sql.push_str(&format!(" AND due_date <= ?{}", args.len()));
        }

        if !needs_memory_pass {
            sql.push_str(" ORDER BY ");
            sql.push_str(match query.sort {
                TaskSort::Manual => "sort_order ASC, created_us ASC",
                // "Undated last" has to be said out loud. The two databases
                // put NULLs at opposite ends of an ASC sort, and neither
                // default is the one wanted -- so the leading boolean decides
                // it and they agree.
                TaskSort::DueAsc => "due_date IS NULL, due_date ASC, sort_order ASC",
                TaskSort::PriorityDesc => "priority DESC, sort_order ASC",
                TaskSort::CreatedDesc => "created_us DESC",
                TaskSort::UpdatedDesc => "updated_us DESC",
                TaskSort::CompletedDesc => "completed_us IS NULL, completed_us DESC",
                TaskSort::TitleAsc => unreachable!("handled by the in-memory pass"),
            });
            sql.push_str(&self.dialect.limit_offset(query.limit, query.offset));
        }

        let rows = self.conn().records(&sql, &args)?;
        let tasks: Vec<Task> = self.collect(rows, task_aad)?;
        Ok(if needs_memory_pass { query.apply(tasks) } else { tasks })
    }

    fn get_task(&self, id: TaskId) -> Result<Task> {
        let sealed = self
            .conn()
            .sealed("SELECT data FROM tasks WHERE id = ?1", &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found("task", id))?;
        self.unseal(&task_aad(id), &sealed)
    }

    fn put_task(&self, t: &Task) -> Result<()> {
        self.put_tasks(std::slice::from_ref(t))
    }

    fn put_tasks(&self, tasks: &[Task]) -> Result<()> {
        if tasks.is_empty() {
            return Ok(());
        }
        // Sealed before the lock is taken: encryption is the expensive part
        // and there is no reason to hold the connection through it.
        let sealed: Vec<(&Task, Vec<u8>)> =
            tasks.iter().map(|t| Ok((t, self.seal(&task_aad(t.id), t)?))).collect::<Result<_>>()?;

        let mut conn = self.conn();
        let mut tx = conn.begin()?;
        for (t, data) in sealed {
            tx.execute(
                "INSERT INTO tasks
                    (id, project_id, parent_id, status, priority, start_date, due_date,
                     sort_order, created_us, updated_us, completed_us, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT (id) DO UPDATE SET
                    project_id = ?2, parent_id = ?3, status = ?4, priority = ?5,
                    start_date = ?6, due_date = ?7, sort_order = ?8, created_us = ?9,
                    updated_us = ?10, completed_us = ?11, data = ?12",
                &vals![
                    t.id.to_string(),
                    id_str(t.project_id),
                    id_str(t.parent_id),
                    t.status.as_str(),
                    t.priority.rank(),
                    date_str(t.start_date),
                    date_str(t.due_date),
                    t.sort_order,
                    to_us(t.created_at),
                    to_us(t.updated_at),
                    t.completed_at.map(to_us),
                    data,
                ],
            )?;
            set_purpose(tx.as_mut(), RecordKind::Task, &t.id.to_string(), t.purpose.as_ref())?;
        }
        tx.commit()
    }

    fn delete_task(&self, id: TaskId) -> Result<()> {
        let mut conn = self.conn();
        let mut tx = conn.begin()?;
        let doomed = SqlStore::subtree(tx.as_mut(), id)?;
        SqlStore::purge_tasks(tx.as_mut(), &doomed)?;
        tx.commit()
    }

    // ---- time blocks ----------------------------------------------------

    fn list_blocks(&self, query: &BlockQuery) -> Result<Vec<TimeBlock>> {
        let mut sql = String::from("SELECT id, data FROM time_blocks WHERE 1=1");
        let mut args: Vec<Value> = Vec::new();

        if let Some(from) = query.from {
            args.push(Value::Text(from.to_string()));
            sql.push_str(&format!(" AND local_date >= ?{}", args.len()));
        }
        if let Some(to) = query.to {
            args.push(Value::Text(to.to_string()));
            sql.push_str(&format!(" AND local_date <= ?{}", args.len()));
        }
        if let Some(task) = query.task_id {
            args.push(Value::Text(task.to_string()));
            sql.push_str(&format!(" AND task_id = ?{}", args.len()));
        }
        if let Some(project) = query.project_id {
            args.push(Value::Text(project.to_string()));
            sql.push_str(&format!(" AND project_id = ?{}", args.len()));
        }
        if let Some(kind) = query.kind {
            args.push(Value::Text(kind.as_str().to_string()));
            sql.push_str(&format!(" AND kind = ?{}", args.len()));
        }
        sql.push_str(" ORDER BY start_us ASC, end_us ASC");
        if let Some(limit) = query.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        let rows = self.conn().records(&sql, &args)?;
        self.collect(rows, block_aad)
    }

    fn get_block(&self, id: BlockId) -> Result<TimeBlock> {
        let sealed = self
            .conn()
            .sealed("SELECT data FROM time_blocks WHERE id = ?1", &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found("block", id))?;
        self.unseal(&block_aad(id), &sealed)
    }

    fn put_block(&self, b: &TimeBlock) -> Result<()> {
        let data = self.seal(&block_aad(b.id), b)?;
        let mut conn = self.conn();
        let mut tx = conn.begin()?;
        tx.execute(
            "INSERT INTO time_blocks
                (id, task_id, project_id, local_date, start_us, end_us, kind, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (id) DO UPDATE SET
                task_id = ?2, project_id = ?3, local_date = ?4, start_us = ?5,
                end_us = ?6, kind = ?7, data = ?8",
            &vals![
                b.id.to_string(),
                id_str(b.subject.task_id()),
                id_str(b.subject.project_id()),
                b.local_date.to_string(),
                to_us(b.start),
                to_us(b.end),
                b.kind.as_str(),
                data,
            ],
        )?;
        set_purpose(tx.as_mut(), RecordKind::Block, &b.id.to_string(), b.purpose.as_ref())?;
        tx.commit()
    }

    fn delete_block(&self, id: BlockId) -> Result<()> {
        let mut conn = self.conn();
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
        let mut conn = self.conn();

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
