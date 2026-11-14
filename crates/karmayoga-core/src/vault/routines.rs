//! The assistant's standing work: routines, and the log of what they did.

use super::Vault;
use super::session::Domain;
use crate::error::{Error, Result};
use crate::id::{RoutineId, RoutineRunId};
use crate::record::RecordKind;
use crate::routine::{DreamScope, Routine, RoutineRun, Trigger};
use crate::store::routines::{RoutineStore, RunQuery};
use crate::timestamped::Timestamped;

/// What `Vault::save_routine` says when somebody tries to make or unmake a
/// dream by hand. One sentence, used from both directions -- a new routine
/// whose kind is `Dream`, and an existing one whose kind would change either
/// way -- because the person's fix is the same either way: the switch in
/// Settings, not this form.
const DREAM_IS_THE_APPLICATIONS: &str =
    "a dream belongs to the application; turn dreaming off in Settings instead";

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
    ///
    /// Refuses a *new* routine whose kind is [`RoutineKind::Dream`](crate::routine::RoutineKind::Dream)
    /// -- those three are made only by [`Vault::set_dreaming`] -- and refuses
    /// changing an existing routine's kind in either direction. A dream's
    /// schedule, grace and instructions (the person's own paragraph, added to
    /// the app's prompt) are ordinary edits and go through unchanged.
    pub fn save_routine(&self, routine: &Routine) -> Result<()> {
        self.writable()?;
        let previous_kind = self.routine(routine.id).ok().map(|r| r.kind);
        match previous_kind {
            None if routine.kind.is_dream() => {
                return Err(Error::Invalid(DREAM_IS_THE_APPLICATIONS.into()));
            }
            Some(previous) if previous != routine.kind => {
                return Err(Error::Invalid(DREAM_IS_THE_APPLICATIONS.into()));
            }
            _ => {}
        }
        self.put_routine_unchecked(routine)
    }

    /// The write [`Vault::save_routine`] does, without the dream-kind
    /// refusal -- the one path [`Vault::set_dreaming`] uses to create and
    /// enable the three routines that refusal exists to stop anybody else
    /// from making.
    fn put_routine_unchecked(&self, routine: &Routine) -> Result<()> {
        routine.validate()?;
        if let Trigger::BeforeEvent { role_id: Some(role), .. } = &routine.trigger {
            self.role(*role)?;
        }
        self.with_routines(|r| r.put_routine(routine))?;
        self.wrote(RecordKind::Routine, routine.id);
        Ok(())
    }

    /// Delete a routine, its runs, and the transcripts behind them.
    ///
    /// The transcripts are the part a database cannot express: they live in
    /// the agent store, so the cascade is here. What is *not* touched is
    /// anything the routine ever made -- a task written by a routine that has
    /// since been deleted is still a task somebody has to do.
    ///
    /// Refuses a dream: it is not somebody's to delete by hand, only to turn
    /// off.
    pub fn delete_routine(&self, id: RoutineId) -> Result<()> {
        self.writable()?;
        let routine = self.routine(id)?;
        if routine.kind.is_dream() {
            return Err(Error::Invalid(DREAM_IS_THE_APPLICATIONS.into()));
        }
        let runs = self.with_routines(|r| r.list_runs(&RunQuery::for_routine(id)))?;
        for run in &runs {
            if let Some(conversation) = run.conversation_id {
                // Best effort: a transcript already gone is not a reason to
                // refuse to delete the routine.
                let _ = self.delete_conversation(conversation);
            }
        }
        self.with_routines(|r| r.delete_routine(id))?;
        self.wrote(RecordKind::Routine, id);
        Ok(())
    }

    /// Switch dreaming on or off.
    ///
    /// On first enable, creates the three routines the application owns --
    /// see [`Routine::dream`] for their schedules and graces -- through
    /// [`Vault::put_routine_unchecked`], the one path that may give a routine
    /// a `Dream` kind. Thereafter, and on every call, this only flips
    /// `enabled` on the three: their schedule, grace and instructions are
    /// the person's to edit once they exist, through the ordinary
    /// [`Vault::save_routine`].
    ///
    /// Idempotent either way -- switching on twice creates nothing a second
    /// time, and switching off twice just leaves them off -- and returns the
    /// three routines as saved, so a caller can draw them without a second
    /// read.
    pub fn set_dreaming(&self, on: bool) -> Result<Vec<Routine>> {
        self.writable()?;
        let existing = self.routines()?;
        let mut out = Vec::with_capacity(DreamScope::ALL.len());
        for scope in DreamScope::ALL {
            let mut routine = existing
                .iter()
                .find(|r| r.kind.dream_scope() == Some(scope))
                .cloned()
                .unwrap_or_else(|| Routine::dream(scope));
            routine.enabled = on;
            routine.touch();
            self.put_routine_unchecked(&routine)?;
            out.push(routine);
        }
        Ok(out)
    }

    pub fn runs(&self, query: &RunQuery) -> Result<Vec<RoutineRun>> {
        self.with_routines(|r| r.list_runs(query))
    }

    pub fn run(&self, id: RoutineRunId) -> Result<RoutineRun> {
        self.with_routines(|r| r.get_run(id))
    }

    pub fn save_run(&self, run: &RoutineRun) -> Result<()> {
        self.writable()?;
        self.with_routines(|r| r.put_run(run))?;
        self.wrote(RecordKind::RoutineRun, run.id);
        Ok(())
    }

    pub fn delete_run(&self, id: RoutineRunId) -> Result<()> {
        self.writable()?;
        let run = self.run(id)?;
        if let Some(conversation) = run.conversation_id {
            let _ = self.delete_conversation(conversation);
        }
        self.with_routines(|r| r.delete_run(id))?;
        self.wrote(RecordKind::RoutineRun, id);
        Ok(())
    }

    /// How many runs nobody has looked at. The number on the app bar.
    pub fn unseen_runs(&self) -> Result<u64> {
        self.with_routines(|r| r.count_unseen_runs())
    }

    /// Mark runs as looked at. An empty list means all of them.
    ///
    /// Only the ids named record a touch -- "all of them" has no ids of its
    /// own to name here any more than `mark_runs_seen`'s own `change:` row
    /// does, since both read the same empty `ids` the caller sent.
    pub fn mark_runs_seen(&self, ids: &[RoutineRunId]) -> Result<()> {
        self.writable()?;
        self.with_routines(|r| r.mark_runs_seen(ids))?;
        for id in ids {
            self.wrote(RecordKind::RoutineRun, *id);
        }
        Ok(())
    }
}
