//! Storage for the assistant's standing work and its log.
//!
//! Two records and no cascade to anywhere else in the vault, which is the
//! point: a routine that made a task and was then deleted must leave the task
//! alone. What it does take with it is its own runs, and the conversations
//! behind them, because a transcript nothing points at is a transcript nobody
//! will ever open.
//!
//! In the clear: that a routine exists, and for a run, which routine it
//! belongs to, when it started and whether anybody has looked at it. That is
//! what the unseen count on the app bar and the log under each routine are
//! ordered by. Sealed: what the routine is for, and every word the assistant
//! said about it.

use crate::Result;
use crate::id::{RoutineId, RoutineRunId};
use crate::routine::{Outcome, Routine, RoutineRun};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Filter and pagination for [`RoutineStore::list_runs`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RunQuery {
    /// One routine's log. `None` is everything, newest first.
    pub routine_id: Option<RoutineId>,
    /// Keep only runs that ended this way. Empty means any.
    pub outcomes: Vec<Outcome>,
    /// Keep only runs nobody has looked at.
    pub unseen: Option<bool>,
    /// Runs started at or after this. What a "since you were away" list asks.
    pub since: Option<Timestamp>,
    pub limit: Option<u32>,
}

impl RunQuery {
    pub fn for_routine(id: RoutineId) -> Self {
        Self { routine_id: Some(id), ..Default::default() }
    }

    pub fn unseen(limit: u32) -> Self {
        Self { unseen: Some(true), limit: Some(limit), ..Default::default() }
    }

    /// Does this run pass the filters? The fallback for a backend that
    /// cannot express one natively, so behaviour is identical across them.
    pub fn matches(&self, run: &RoutineRun) -> bool {
        if let Some(id) = self.routine_id
            && run.routine_id != id
        {
            return false;
        }
        if let Some(unseen) = self.unseen
            && run.seen != !unseen
        {
            return false;
        }
        if let Some(since) = self.since
            && run.started_at < since
        {
            return false;
        }
        self.outcomes.is_empty() || self.outcomes.contains(&run.outcome)
    }

    /// Sort newest first and cap. Done here rather than in SQL so that two
    /// backends cannot disagree about where a tie goes.
    pub fn apply(&self, mut rows: Vec<RoutineRun>) -> Vec<RoutineRun> {
        rows.retain(|r| self.matches(r));
        rows.sort_by(|a, b| {
            b.started_at.cmp(&a.started_at).then_with(|| b.id.to_string().cmp(&a.id.to_string()))
        });
        if let Some(limit) = self.limit {
            rows.truncate(limit as usize);
        }
        rows
    }
}

/// What a backend must do to hold routines.
pub trait RoutineStore: Send + Sync {
    /// Every routine, in the order they were made. There are a handful of
    /// these, so filtering is the caller's business -- the same call
    /// `list_roles` makes.
    fn list_routines(&self) -> Result<Vec<Routine>>;
    fn get_routine(&self, id: RoutineId) -> Result<Routine>;
    /// Idempotent: saving the same routine twice leaves one.
    fn put_routine(&self, routine: &Routine) -> Result<()>;
    /// Takes its runs with it. Deleting one that is not there is not an error.
    ///
    /// The conversations behind those runs are *not* this trait's to remove:
    /// they belong to the agent store, and the vault deletes them either side
    /// of this call. See [`crate::Vault::delete_routine`].
    fn delete_routine(&self, id: RoutineId) -> Result<()>;

    fn list_runs(&self, query: &RunQuery) -> Result<Vec<RoutineRun>>;
    fn get_run(&self, id: RoutineRunId) -> Result<RoutineRun>;
    fn put_run(&self, run: &RoutineRun) -> Result<()>;
    fn delete_run(&self, id: RoutineRunId) -> Result<()>;

    /// How many runs nobody has looked at. One indexed count, because it is
    /// drawn on the app bar and reloads whenever anything changes.
    fn count_unseen_runs(&self) -> Result<u64>;

    /// Mark runs as looked at. Empty means all of them.
    fn mark_runs_seen(&self, ids: &[RoutineRunId]) -> Result<()>;
}

/// Additional authenticated data for a sealed routine payload.
pub fn routine_aad(id: RoutineId) -> Vec<u8> {
    format!("everyday.routine.v1:{id}").into_bytes()
}

/// The same, for a run.
pub fn run_aad(id: RoutineRunId) -> Vec<u8> {
    format!("everyday.run.v1:{id}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routine::Trigger;

    fn run(outcome: Outcome, seen: bool) -> RoutineRun {
        let routine = Routine::new("Brief", "Say what is due.", Trigger::Manual);
        let mut run = RoutineRun::new(&routine, None);
        run.outcome = outcome;
        run.seen = seen;
        run
    }

    #[test]
    fn the_unseen_filter_means_what_it_says() {
        let rows = vec![run(Outcome::Done, false), run(Outcome::Done, true)];
        assert_eq!(
            RunQuery { unseen: Some(true), ..Default::default() }.apply(rows.clone()).len(),
            1
        );
        assert_eq!(RunQuery { unseen: Some(false), ..Default::default() }.apply(rows).len(), 1);
    }

    #[test]
    fn an_outcome_filter_wants_any_of_them_rather_than_all() {
        let rows = vec![run(Outcome::Done, true), run(Outcome::Failed, true)];
        let q = RunQuery { outcomes: vec![Outcome::Failed], ..Default::default() };
        assert_eq!(q.apply(rows.clone()).len(), 1);
        let q = RunQuery { outcomes: vec![Outcome::Failed, Outcome::Done], ..Default::default() };
        assert_eq!(q.apply(rows).len(), 2);
    }

    #[test]
    fn the_two_kinds_of_record_are_sealed_under_different_labels() {
        let routine = RoutineId::new();
        assert_ne!(routine_aad(routine), run_aad(RoutineRunId::new()));
        assert!(String::from_utf8(routine_aad(routine)).unwrap().contains("routine"));
    }
}
