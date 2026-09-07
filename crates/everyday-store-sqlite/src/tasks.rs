//! The task domain: projects, tasks and blocks of time.
//!
//! Optional -- a backend that returns `None` from `JournalStore::tasks` holds
//! journals and nothing else, which is what the Markdown backend does. This
//! one carries it, and that is why the todo app is offered on a SQLite vault.
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
use rusqlite::{OptionalExtension, params, params_from_iter};

use crate::{SqliteStore, date_str, id_str, to_us};

impl TaskStore for SqliteStore {
    // ---- projects -------------------------------------------------------

    fn list_projects(&self) -> Result<Vec<Project>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, data FROM projects ORDER BY sort_order, created_us")
            .map_err(Error::backend)?;
        let rows: Vec<(String, Vec<u8>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(Error::backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(Error::backend)?;
        drop(stmt);
        drop(conn);
        self.collect(rows, project_aad)
    }

    fn get_project(&self, id: ProjectId) -> Result<Project> {
        let conn = self.conn.lock().unwrap();
        let sealed: Option<Vec<u8>> = conn
            .query_row("SELECT data FROM projects WHERE id = ?1", params![id.to_string()], |r| {
                r.get(0)
            })
            .optional()
            .map_err(Error::backend)?;
        drop(conn);
        let sealed = sealed.ok_or_else(|| Error::not_found("project", id))?;
        self.unseal(&project_aad(id), &sealed)
    }

    fn put_project(&self, p: &Project) -> Result<()> {
        let data = self.seal(&project_aad(p.id), p)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO projects
                (id, status, priority, due_date, sort_order, created_us, updated_us,
                 completed_us, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                status = ?2, priority = ?3, due_date = ?4, sort_order = ?5,
                created_us = ?6, updated_us = ?7, completed_us = ?8, data = ?9",
            params![
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
        )
        .map_err(Error::backend)?;
        Ok(())
    }

    fn delete_project(&self, id: ProjectId) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(Error::backend)?;

        // Its tasks, plus anything nested under them -- a subtask filed into
        // a different project is still doomed with its parent.
        let roots: Vec<String> = {
            let mut stmt =
                tx.prepare("SELECT id FROM tasks WHERE project_id = ?1").map_err(Error::backend)?;
            stmt.query_map(params![id.to_string()], |r| r.get::<_, String>(0))
                .map_err(Error::backend)?
                .collect::<rusqlite::Result<_>>()
                .map_err(Error::backend)?
        };
        let mut doomed: Vec<String> = Vec::new();
        for root in roots {
            let root = TaskId::parse(&root).map_err(|e| Error::Invalid(e.to_string()))?;
            for descendant in Self::subtree(&tx, root)? {
                if !doomed.contains(&descendant) {
                    doomed.push(descendant);
                }
            }
        }
        Self::purge_tasks(&tx, &doomed)?;

        // Time booked against the project itself, not against its tasks.
        tx.execute("DELETE FROM time_blocks WHERE project_id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        tx.execute("DELETE FROM projects WHERE id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        tx.commit().map_err(Error::backend)?;
        Ok(())
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
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        match query.project {
            ProjectScope::Any => {}
            ProjectScope::Inbox => sql.push_str(" AND project_id IS NULL"),
            ProjectScope::Project { id } => {
                args.push(Box::new(id.to_string()));
                sql.push_str(&format!(" AND project_id = ?{}", args.len()));
            }
        }
        match query.parent {
            ParentScope::Any => {}
            ParentScope::TopLevel => sql.push_str(" AND parent_id IS NULL"),
            ParentScope::Of { id } => {
                args.push(Box::new(id.to_string()));
                sql.push_str(&format!(" AND parent_id = ?{}", args.len()));
            }
        }
        if !query.statuses.is_empty() {
            let mut holes = Vec::with_capacity(query.statuses.len());
            for status in &query.statuses {
                args.push(Box::new(status.as_str()));
                holes.push(format!("?{}", args.len()));
            }
            sql.push_str(&format!(" AND status IN ({})", holes.join(",")));
        }
        if let Some(min) = query.priority_at_least {
            args.push(Box::new(min.rank()));
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
            args.push(Box::new(from.to_string()));
            sql.push_str(&format!(" AND due_date >= ?{}", args.len()));
        }
        if let Some(to) = query.due_to {
            args.push(Box::new(to.to_string()));
            sql.push_str(&format!(" AND due_date <= ?{}", args.len()));
        }

        if !needs_memory_pass {
            sql.push_str(" ORDER BY ");
            sql.push_str(match query.sort {
                TaskSort::Manual => "sort_order ASC, created_us ASC",
                // "Undated last" has to be said out loud: SQLite sorts NULL
                // first on an ASC column, which would put the whole backlog
                // above the things actually due.
                TaskSort::DueAsc => "due_date IS NULL, due_date ASC, sort_order ASC",
                TaskSort::PriorityDesc => "priority DESC, sort_order ASC",
                TaskSort::CreatedDesc => "created_us DESC",
                TaskSort::UpdatedDesc => "updated_us DESC",
                TaskSort::CompletedDesc => "completed_us IS NULL, completed_us DESC",
                TaskSort::TitleAsc => unreachable!("handled by the in-memory pass"),
            });
            // SQLite needs an explicit LIMIT before it will honour OFFSET.
            sql.push_str(&format!(
                " LIMIT {} OFFSET {}",
                query.limit.map_or(-1i64, i64::from),
                query.offset
            ));
        }

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&sql).map_err(Error::backend)?;
        let rows: Vec<(String, Vec<u8>)> = stmt
            .query_map(params_from_iter(args.iter().map(|a| a.as_ref())), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(Error::backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(Error::backend)?;
        drop(stmt);
        drop(conn);

        let tasks: Vec<Task> = self.collect(rows, task_aad)?;
        Ok(if needs_memory_pass { query.apply(tasks) } else { tasks })
    }

    fn get_task(&self, id: TaskId) -> Result<Task> {
        let conn = self.conn.lock().unwrap();
        let sealed: Option<Vec<u8>> = conn
            .query_row("SELECT data FROM tasks WHERE id = ?1", params![id.to_string()], |r| {
                r.get(0)
            })
            .optional()
            .map_err(Error::backend)?;
        drop(conn);
        let sealed = sealed.ok_or_else(|| Error::not_found("task", id))?;
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

        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(Error::backend)?;
        for (t, data) in sealed {
            tx.execute(
                "INSERT INTO tasks
                    (id, project_id, parent_id, status, priority, start_date, due_date,
                     sort_order, created_us, updated_us, completed_us, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(id) DO UPDATE SET
                    project_id = ?2, parent_id = ?3, status = ?4, priority = ?5,
                    start_date = ?6, due_date = ?7, sort_order = ?8, created_us = ?9,
                    updated_us = ?10, completed_us = ?11, data = ?12",
                params![
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
            )
            .map_err(Error::backend)?;
        }
        tx.commit().map_err(Error::backend)?;
        Ok(())
    }

    fn delete_task(&self, id: TaskId) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(Error::backend)?;
        let doomed = Self::subtree(&tx, id)?;
        Self::purge_tasks(&tx, &doomed)?;
        tx.commit().map_err(Error::backend)?;
        Ok(())
    }

    // ---- time blocks ----------------------------------------------------

    fn list_blocks(&self, query: &BlockQuery) -> Result<Vec<TimeBlock>> {
        let mut sql = String::from("SELECT id, data FROM time_blocks WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(from) = query.from {
            args.push(Box::new(from.to_string()));
            sql.push_str(&format!(" AND local_date >= ?{}", args.len()));
        }
        if let Some(to) = query.to {
            args.push(Box::new(to.to_string()));
            sql.push_str(&format!(" AND local_date <= ?{}", args.len()));
        }
        if let Some(task) = query.task_id {
            args.push(Box::new(task.to_string()));
            sql.push_str(&format!(" AND task_id = ?{}", args.len()));
        }
        if let Some(project) = query.project_id {
            args.push(Box::new(project.to_string()));
            sql.push_str(&format!(" AND project_id = ?{}", args.len()));
        }
        if let Some(kind) = query.kind {
            args.push(Box::new(kind.as_str()));
            sql.push_str(&format!(" AND kind = ?{}", args.len()));
        }
        sql.push_str(" ORDER BY start_us ASC, end_us ASC");
        if let Some(limit) = query.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&sql).map_err(Error::backend)?;
        let rows: Vec<(String, Vec<u8>)> = stmt
            .query_map(params_from_iter(args.iter().map(|a| a.as_ref())), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(Error::backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(Error::backend)?;
        drop(stmt);
        drop(conn);
        self.collect(rows, block_aad)
    }

    fn get_block(&self, id: BlockId) -> Result<TimeBlock> {
        let conn = self.conn.lock().unwrap();
        let sealed: Option<Vec<u8>> = conn
            .query_row("SELECT data FROM time_blocks WHERE id = ?1", params![id.to_string()], |r| {
                r.get(0)
            })
            .optional()
            .map_err(Error::backend)?;
        drop(conn);
        let sealed = sealed.ok_or_else(|| Error::not_found("block", id))?;
        self.unseal(&block_aad(id), &sealed)
    }

    fn put_block(&self, b: &TimeBlock) -> Result<()> {
        let data = self.seal(&block_aad(b.id), b)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO time_blocks
                (id, task_id, project_id, local_date, start_us, end_us, kind, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                task_id = ?2, project_id = ?3, local_date = ?4, start_us = ?5,
                end_us = ?6, kind = ?7, data = ?8",
            params![
                b.id.to_string(),
                id_str(b.subject.task_id()),
                id_str(b.subject.project_id()),
                b.local_date.to_string(),
                to_us(b.start),
                to_us(b.end),
                b.kind.as_str(),
                data,
            ],
        )
        .map_err(Error::backend)?;
        Ok(())
    }

    fn delete_block(&self, id: BlockId) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM time_blocks WHERE id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        Ok(())
    }

    // ---- housekeeping ---------------------------------------------------

    fn task_stats(&self, today: jiff::civil::Date) -> Result<TaskStats> {
        // Every count here reads a clear column, so the whole panel costs
        // one pass over the indexes and decrypts nothing.
        let conn = self.conn.lock().unwrap();
        let count = |sql: &str| -> Result<u64> {
            let n: i64 = conn.query_row(sql, [], |r| r.get(0)).map_err(Error::backend)?;
            Ok(n as u64)
        };
        let minutes = |kind: BlockKind| -> Result<u64> {
            // Truncated to whole minutes per block before summing, so the
            // total agrees with the per-block figures the UI shows.
            let secs: i64 = conn
                .query_row(
                    "SELECT COALESCE(SUM(MAX(end_us - start_us, 0)), 0) / 1000000
                     FROM time_blocks WHERE kind = ?1",
                    params![kind.as_str()],
                    |r| r.get(0),
                )
                .map_err(Error::backend)?;
            Ok((secs / 60) as u64)
        };

        let open: Vec<&str> =
            TaskStatus::ALL.iter().filter(|s| s.is_open()).map(|s| s.as_str()).collect();
        let open_list = open.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(",");

        // One grouped scan of `tasks_by_project`, which is exactly the index
        // this is shaped like. `project_id` and `status` are both clear
        // columns, so the sidebar's counts cost no decryption at all.
        let mut open_by_project = Vec::new();
        {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT project_id, COUNT(*) FROM tasks
                     WHERE status IN ({open_list}) GROUP BY project_id"
                ))
                .map_err(Error::backend)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?)))
                .map_err(Error::backend)?;
            for row in rows {
                let (id, n) = row.map_err(Error::backend)?;
                let project_id = match id {
                    Some(id) => {
                        Some(ProjectId::parse(&id).map_err(|e| Error::Invalid(e.to_string()))?)
                    }
                    None => None,
                };
                open_by_project.push(ProjectTaskCount { project_id, open: n as u64 });
            }
        }

        // Both bounds compare against a clear `due_date` column, and NULL
        // compares as neither -- so an undated task is correctly outside
        // both, exactly as it is outside `TaskQuery`'s date windows.
        let today = today.to_string();
        let due_today: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM tasks
                     WHERE status IN ({open_list}) AND due_date <= ?1"
                ),
                params![today],
                |r| r.get(0),
            )
            .map_err(Error::backend)?;
        let overdue: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM tasks
                     WHERE status IN ({open_list}) AND due_date < ?1"
                ),
                params![today],
                |r| r.get(0),
            )
            .map_err(Error::backend)?;

        Ok(TaskStats {
            open_by_project,
            due_today: due_today as u64,
            overdue: overdue as u64,
            projects: count("SELECT COUNT(*) FROM projects")?,
            active_projects: count(
                "SELECT COUNT(*) FROM projects WHERE status IN ('active', 'paused')",
            )?,
            tasks: count("SELECT COUNT(*) FROM tasks")?,
            open_tasks: count(&format!(
                "SELECT COUNT(*) FROM tasks WHERE status IN ({open_list})"
            ))?,
            done_tasks: count("SELECT COUNT(*) FROM tasks WHERE status = 'done'")?,
            blocks: count("SELECT COUNT(*) FROM time_blocks")?,
            planned_minutes: minutes(BlockKind::Planned)?,
            logged_minutes: minutes(BlockKind::Actual)?,
        })
    }
}
