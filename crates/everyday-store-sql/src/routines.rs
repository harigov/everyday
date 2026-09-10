//! The assistant's standing work, and its log.
//!
//! Two straightforward tables. The only thing worth reading for is the
//! cascade: deleting a routine takes its runs, in one transaction, because a
//! run whose routine is gone is a row nothing can draw. What it does *not*
//! take is anything the routine ever made -- a task written by a routine that
//! has since been deleted is still a task somebody has to do.

use everyday_core::error::{Error, Result};
use everyday_core::id::{RoutineId, RoutineRunId};
use everyday_core::routine::{Routine, RoutineRun};
use everyday_core::store::routines::{RoutineStore, RunQuery, routine_aad, run_aad};

use crate::conn::SqlExt;
use crate::{SqlStore, to_us, vals};

impl RoutineStore for SqlStore {
    fn list_routines(&self) -> Result<Vec<Routine>> {
        let rows = self.read().records("SELECT id, data FROM routines ORDER BY created_us", &[])?;
        self.collect(rows, routine_aad)
    }

    fn get_routine(&self, id: RoutineId) -> Result<Routine> {
        let sealed = self
            .read()
            .sealed("SELECT data FROM routines WHERE id = ?1", &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found("routine", id))?;
        self.unseal(&routine_aad(id), &sealed)
    }

    fn put_routine(&self, routine: &Routine) -> Result<()> {
        // The name, the trigger and the instructions are all inside `data`. A
        // database whose routines table said "Prepare for the oncology
        // appointment" would be telling somebody a great deal.
        let data = self.seal(&routine_aad(routine.id), routine)?;
        self.write().execute(
            "INSERT INTO routines (id, created_us, updated_us, data) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (id) DO UPDATE SET created_us = ?2, updated_us = ?3, data = ?4",
            &vals![
                routine.id.to_string(),
                to_us(routine.created_at),
                to_us(routine.updated_at),
                data,
            ],
        )?;
        Ok(())
    }

    fn delete_routine(&self, id: RoutineId) -> Result<()> {
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        // Runs first: a run whose routine is gone is a row nothing can draw,
        // and doing it the other way round would leave them behind if the
        // second statement failed.
        tx.execute("DELETE FROM routine_runs WHERE routine_id = ?1", &vals![id.to_string()])?;
        tx.execute("DELETE FROM routines WHERE id = ?1", &vals![id.to_string()])?;
        tx.commit()
    }

    fn list_runs(&self, query: &RunQuery) -> Result<Vec<RoutineRun>> {
        // What an index can answer is pushed down; the rest -- the outcome,
        // which is inside the sealed payload -- is finished in Rust by
        // `RunQuery::apply`, which is the same code path every backend uses.
        let mut sql = String::from("SELECT id, data FROM routine_runs WHERE 1=1");
        let mut args: Vec<crate::conn::Value> = Vec::new();
        if let Some(id) = query.routine_id {
            args.push(crate::conn::Value::Text(id.to_string()));
            sql.push_str(&format!(" AND routine_id = ?{}", args.len()));
        }
        if let Some(unseen) = query.unseen {
            args.push(crate::conn::Value::Bool(!unseen));
            sql.push_str(&format!(" AND seen = ?{}", args.len()));
        }
        if let Some(since) = query.since {
            args.push(crate::conn::Value::Int(to_us(since)));
            sql.push_str(&format!(" AND started_us >= ?{}", args.len()));
        }
        sql.push_str(" ORDER BY started_us DESC");
        let rows = self.read().records(&sql, &args)?;
        Ok(query.apply(self.collect(rows, run_aad)?))
    }

    fn get_run(&self, id: RoutineRunId) -> Result<RoutineRun> {
        let sealed = self
            .read()
            .sealed("SELECT data FROM routine_runs WHERE id = ?1", &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found("run", id))?;
        self.unseal(&run_aad(id), &sealed)
    }

    fn put_run(&self, run: &RoutineRun) -> Result<()> {
        let data = self.seal(&run_aad(run.id), run)?;
        self.write().execute(
            "INSERT INTO routine_runs (id, routine_id, started_us, seen, data)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET
                routine_id = ?2, started_us = ?3, seen = ?4, data = ?5",
            &vals![
                run.id.to_string(),
                run.routine_id.to_string(),
                to_us(run.started_at),
                run.seen,
                data,
            ],
        )?;
        Ok(())
    }

    fn delete_run(&self, id: RoutineRunId) -> Result<()> {
        self.write().execute("DELETE FROM routine_runs WHERE id = ?1", &vals![id.to_string()])?;
        Ok(())
    }

    fn count_unseen_runs(&self) -> Result<u64> {
        let n = self.read().scalar_i64("SELECT COUNT(*) FROM routine_runs WHERE NOT seen", &[])?;
        Ok(n.max(0) as u64)
    }

    /// Mark runs as looked at.
    ///
    /// Read-modify-write rather than one `UPDATE`, and that is not an
    /// oversight: `seen` lives in the clear column *and* inside the sealed
    /// payload, and a statement that moved only the column would leave a run
    /// whose record still said `false` -- so the next client to read it back
    /// would put the number straight back on the app bar. There is no way to
    /// rewrite a sealed payload in SQL, so the rows are opened, changed and
    /// written, in one transaction on the writer for the reason
    /// `merge_trackers` gives.
    ///
    /// The whole-list case resolves to "every unseen id" and goes down the
    /// same path. It is a handful of rows, and a fast statement that lies is
    /// not a trade worth making.
    fn mark_runs_seen(&self, ids: &[RoutineRunId]) -> Result<()> {
        let mut conn = self.write();
        let mut tx = conn.begin()?;

        let targets: Vec<RoutineRunId> = if ids.is_empty() {
            tx.query("SELECT id FROM routine_runs WHERE NOT seen", &[])?
                .into_iter()
                .map(|row| {
                    RoutineRunId::parse(&row.text(0)?).map_err(|e| Error::Invalid(e.to_string()))
                })
                .collect::<Result<_>>()?
        } else {
            ids.to_vec()
        };

        for id in targets {
            let Some(sealed) =
                tx.sealed("SELECT data FROM routine_runs WHERE id = ?1", &vals![id.to_string()])?
            else {
                continue;
            };
            let mut run: RoutineRun = self.unseal(&run_aad(id), &sealed)?;
            if run.seen {
                continue;
            }
            run.seen = true;
            let data = self.seal(&run_aad(id), &run)?;
            tx.execute(
                "UPDATE routine_runs SET seen = ?2, data = ?3 WHERE id = ?1",
                &vals![id.to_string(), true, data],
            )?;
        }
        tx.commit()
    }
}
