//! The assistant's standing work, and its log.
//!
//! Two straightforward tables. The only thing worth reading for is the
//! cascade: deleting a routine takes its runs, in one transaction, because a
//! run whose routine is gone is a row nothing can draw. What it does *not*
//! take is anything the routine ever made -- a task written by a routine that
//! has since been deleted is still a task somebody has to do.

use everyday_core::error::Result;
use everyday_core::id::{RoutineId, RoutineRunId};
use everyday_core::routine::{Routine, RoutineRun};
use everyday_core::store::routines::{RoutineStore, RunQuery, routine_aad, run_aad};

use crate::conn::{SqlExt, ToValue, Value, Where};
use crate::record::Record;
use crate::{SqlStore, to_us, vals};

impl Record for Routine {
    const TABLE: &'static str = "routines";
    const KIND: &'static str = "routine";
    type Id = RoutineId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        routine_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl Record for RoutineRun {
    const TABLE: &'static str = "routine_runs";
    const KIND: &'static str = "run";
    type Id = RoutineRunId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        run_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("routine_id", self.routine_id.to_string().to_value()),
            ("started_us", to_us(self.started_at).to_value()),
            ("seen", self.seen.to_value()),
        ]
    }
}

impl RoutineStore for SqlStore {
    fn list_routines(&self) -> Result<Vec<Routine>> {
        let rows = self.read().records("SELECT id, data FROM routines ORDER BY created_us", &[])?;
        self.collect(rows, routine_aad)
    }

    fn get_routine(&self, id: RoutineId) -> Result<Routine> {
        self.get(id)
    }

    fn put_routine(&self, routine: &Routine) -> Result<()> {
        // The name, the trigger and the instructions are all inside `data`. A
        // database whose routines table said "Prepare for the oncology
        // appointment" would be telling somebody a great deal.
        self.upsert(routine)
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
        let mut w = Where::new();
        if let Some(id) = query.routine_id {
            w = w.eq("routine_id", id.to_string());
        }
        if let Some(unseen) = query.unseen {
            w = w.eq("seen", !unseen);
        }
        if let Some(since) = query.since {
            w = w.gte("started_us", to_us(since));
        }
        let (where_sql, args) = w.finish();
        let mut sql = format!("SELECT id, data FROM routine_runs WHERE {where_sql}");
        sql.push_str(" ORDER BY started_us DESC");
        let rows = self.read().records(&sql, &args)?;
        Ok(query.apply(self.collect(rows, run_aad)?))
    }

    fn get_run(&self, id: RoutineRunId) -> Result<RoutineRun> {
        self.get(id)
    }

    fn put_run(&self, run: &RoutineRun) -> Result<()> {
        self.upsert(run)
    }

    fn delete_run(&self, id: RoutineRunId) -> Result<()> {
        self.delete_by_id::<RoutineRun>(id)?;
        Ok(())
    }

    fn count_unseen_runs(&self) -> Result<u64> {
        let n = self.read().scalar_i64("SELECT COUNT(*) FROM routine_runs WHERE NOT seen", &[])?;
        Ok(n.max(0) as u64)
    }

    /// Mark runs as looked at.
    ///
    /// Read-modify-write via [`SqlStore::rewrite_each`] rather than one
    /// `UPDATE`, and that is not an oversight: `seen` lives in the clear
    /// column *and* inside the sealed payload, and a statement that moved
    /// only the column would leave a run whose record still said `false` --
    /// so the next client to read it back would put the number straight back
    /// on the app bar. There is no way to rewrite a sealed payload in SQL, so
    /// the rows are opened, changed and written, in one transaction on the
    /// writer for the reason `rewrite_each` gives.
    ///
    /// The whole-list case resolves to "every unseen id" and goes down the
    /// same path. It is a handful of rows, and a fast statement that lies is
    /// not a trade worth making.
    fn mark_runs_seen(&self, ids: &[RoutineRunId]) -> Result<()> {
        let mut conn = self.write();
        let mut tx = conn.begin()?;

        if ids.is_empty() {
            self.rewrite_each::<RoutineRun>(
                tx.as_mut(),
                "SELECT id, data FROM routine_runs WHERE NOT seen",
                &[],
                |run| run.seen = true,
            )?;
        } else {
            for id in ids {
                self.rewrite_each::<RoutineRun>(
                    tx.as_mut(),
                    "SELECT id, data FROM routine_runs WHERE id = ?1 AND NOT seen",
                    &vals![id.to_string()],
                    |run| run.seen = true,
                )?;
            }
        }
        tx.commit()
    }
}
