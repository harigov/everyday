//! The assistant's standing work: routines, and the log of what they did.

use super::Vault;
use super::session::Domain;
use crate::error::Result;
use crate::id::{RoutineId, RoutineRunId};
use crate::routine::{Routine, RoutineRun, Trigger};
use crate::store::routines::{RoutineStore, RunQuery};

impl Vault {
    /// Does this vault's backend hold routines at all?
    pub fn supports_routines(&self) -> bool {
        self.with_routines(|_| Ok(())).is_ok()
    }

    fn with_routines<T>(&self, f: impl FnOnce(&dyn RoutineStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Routines, |s| s.routines().map(f))
    }

    pub fn routines(&self) -> Result<Vec<Routine>> {
        self.with_routines(|r| r.list_routines())
    }

    pub fn routine(&self, id: RoutineId) -> Result<Routine> {
        self.with_routines(|r| r.get_routine(id))
    }

    /// Save a routine.
    ///
    /// Validates the record, and then the one thing the schema deliberately
    /// cannot: a `BeforeEvent` trigger naming a role that does not exist would
    /// be a routine that silently never fires. There is no foreign key to
    /// `roles`, for the reason `goals` has none, so it is checked here.
    pub fn save_routine(&self, routine: &Routine) -> Result<()> {
        self.writable()?;
        routine.validate()?;
        if let Trigger::BeforeEvent { role_id: Some(role), .. } = &routine.trigger {
            self.role(*role)?;
        }
        self.with_routines(|r| r.put_routine(routine))
    }

    /// Delete a routine, its runs, and the transcripts behind them.
    ///
    /// The transcripts are the part a database cannot express: they live in
    /// the agent store, so the cascade is here. What is *not* touched is
    /// anything the routine ever made -- a task written by a routine that has
    /// since been deleted is still a task somebody has to do.
    pub fn delete_routine(&self, id: RoutineId) -> Result<()> {
        self.writable()?;
        let runs = self.with_routines(|r| r.list_runs(&RunQuery::for_routine(id)))?;
        for run in &runs {
            if let Some(conversation) = run.conversation_id {
                // Best effort: a transcript already gone is not a reason to
                // refuse to delete the routine.
                let _ = self.delete_conversation(conversation);
            }
        }
        self.with_routines(|r| r.delete_routine(id))
    }

    pub fn runs(&self, query: &RunQuery) -> Result<Vec<RoutineRun>> {
        self.with_routines(|r| r.list_runs(query))
    }

    pub fn run(&self, id: RoutineRunId) -> Result<RoutineRun> {
        self.with_routines(|r| r.get_run(id))
    }

    pub fn save_run(&self, run: &RoutineRun) -> Result<()> {
        self.writable()?;
        self.with_routines(|r| r.put_run(run))
    }

    pub fn delete_run(&self, id: RoutineRunId) -> Result<()> {
        self.writable()?;
        let run = self.run(id)?;
        if let Some(conversation) = run.conversation_id {
            let _ = self.delete_conversation(conversation);
        }
        self.with_routines(|r| r.delete_run(id))
    }

    /// How many runs nobody has looked at. The number on the app bar.
    pub fn unseen_runs(&self) -> Result<u64> {
        self.with_routines(|r| r.count_unseen_runs())
    }

    /// Mark runs as looked at. An empty list means all of them.
    pub fn mark_runs_seen(&self, ids: &[RoutineRunId]) -> Result<()> {
        self.writable()?;
        self.with_routines(|r| r.mark_runs_seen(ids))
    }
}
