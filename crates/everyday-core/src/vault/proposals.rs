//! Proposals: work the assistant prepared and did not do.
//!
//! The plumbing is here -- list, save, count, expire. Accepting and declining,
//! which are the part with rules in them, live beside it.

use super::Vault;
use super::session::Domain;
use crate::agent::MemoryOrigin;
use crate::error::{Error, Result};
use crate::id::{BlockId, MemoryId, NoteId, ProposalId, RoutineId, TaskId};
use crate::proposal::{
    DeclineReason, MAX_PENDING_PROPOSALS, Outcome, Payload, Proposal, ProposalKind, ProposedRecord,
};
use crate::purpose::Purpose;
use crate::record::RecordKind;
use crate::routine::RoutineKind;
use crate::store::proposals::{ProposalQuery, ProposalStore};
use crate::task::BlockSubject;
use jiff::Timestamp;

impl Vault {
    /// Does this vault's backend hold proposals at all?
    pub fn supports_proposals(&self) -> bool {
        self.with_proposals(|_| Ok(())).is_ok()
    }

    pub(super) fn with_proposals<T>(
        &self,
        f: impl FnOnce(&dyn ProposalStore) -> Result<T>,
    ) -> Result<T> {
        self.with_domain(Domain::Proposals, |s| s.proposals().map(f))
    }

    pub fn proposals(&self, query: &ProposalQuery) -> Result<Vec<Proposal>> {
        self.with_proposals(|p| p.list_proposals(query))
    }

    pub fn proposal(&self, id: ProposalId) -> Result<Proposal> {
        self.with_proposals(|p| p.get_proposal(id))
    }

    /// Save a proposal.
    ///
    /// Refuses a *new* pending proposal once [`MAX_PENDING_PROPOSALS`] are
    /// waiting: a person with forty unanswered proposals does not want a
    /// forty-first. Rewriting one that already exists is always allowed, so
    /// closing a proposal never trips the cap.
    pub fn save_proposal(&self, proposal: &Proposal) -> Result<()> {
        self.writable()?;
        proposal.validate()?;
        if proposal.is_pending() {
            let exists = self.with_proposals(|p| match p.get_proposal(proposal.id) {
                Ok(_) => Ok(true),
                Err(Error::NotFound { .. }) => Ok(false),
                Err(e) => Err(e),
            })?;
            if !exists && self.with_proposals(|p| p.count_pending())? >= MAX_PENDING_PROPOSALS {
                return Err(Error::Invalid(format!(
                    "{MAX_PENDING_PROPOSALS} proposals are already waiting for an answer; \
                     no more will be made until some are accepted or declined"
                )));
            }
        }
        self.with_proposals(|p| p.put_proposal(proposal))?;
        self.wrote(RecordKind::Proposal, proposal.id);
        Ok(())
    }

    pub fn delete_proposal(&self, id: ProposalId) -> Result<()> {
        self.writable()?;
        self.with_proposals(|p| p.delete_proposal(id))?;
        self.wrote(RecordKind::Proposal, id);
        Ok(())
    }

    /// How many proposals are waiting for an answer.
    pub fn pending_proposals(&self) -> Result<u64> {
        self.with_proposals(|p| p.count_pending())
    }

    /// How many waiting proposals nobody has looked at. Half the number on
    /// the app bar; unseen runs are the other half.
    pub fn unseen_proposals(&self) -> Result<u64> {
        self.with_proposals(|p| p.count_unseen_pending())
    }

    /// Mark proposals as looked at. An empty list means every pending one.
    pub fn mark_proposals_seen(&self, ids: &[ProposalId]) -> Result<()> {
        self.writable()?;
        self.with_proposals(|p| p.mark_proposals_seen(ids))?;
        for id in ids {
            self.wrote(RecordKind::Proposal, *id);
        }
        Ok(())
    }

    /// Close every pending proposal whose time has passed.
    pub fn expire_proposals(&self, now: Timestamp) -> Result<Vec<ProposalId>> {
        self.writable()?;
        let expired = self.with_proposals(|p| p.expire_proposals(now))?;
        for id in &expired {
            self.wrote(RecordKind::Proposal, *id);
        }
        Ok(expired)
    }

    /// Accept a proposal: validate it, check what it refers to still exists,
    /// save the record through the domain's own save, and close it.
    ///
    /// `edited` is the record as the person changed it before accepting;
    /// `confirm` says the person accepted a memory as *true* (from the
    /// memory list) rather than merely "put that there".
    ///
    /// Mail is not accepted here: sending belongs to the service's outbox.
    ///
    /// A failure past the shape of `edited` -- a reference that has gone,
    /// the record's own validation, a `Replace` changed underneath it, a
    /// `Create` that already exists -- closes the proposal
    /// `Declined { reason: Other }` in the same call, in plain words, so
    /// nothing stale is ever written and the proposal does not sit there
    /// pretending it might still apply. See *Version safety* in
    /// `docs/plans/dreaming.md`.
    pub fn accept_proposal(
        &self,
        id: ProposalId,
        edited: Option<ProposedRecord>,
        confirm: bool,
        now: Timestamp,
    ) -> Result<Proposal> {
        self.writable()?;
        let mut proposal = self.with_proposals(|p| p.get_proposal(id))?;
        if !proposal.is_pending() {
            return Err(Error::Invalid("this proposal has already been answered".into()));
        }
        if proposal.expires_at <= now {
            proposal.close(Outcome::Expired { at: now }, now);
            self.save_proposal(&proposal)?;
            return Err(Error::Invalid("this proposal has expired".into()));
        }
        if matches!(proposal.payload, Payload::SendMail { .. }) {
            return Err(Error::Invalid(
                "a mail proposal is sent through the outbox, not accepted here".into(),
            ));
        }
        // The shape of `edited` is a caller question, not a "did the world
        // change under this proposal" one, so a mismatch here is answered
        // without touching the proposal at all -- the same door the
        // `SendMail` refusal above leaves it through.
        self.check_edit_shape(&proposal.payload, &edited)?;
        let edited_given = edited.is_some();

        match self.try_accept(&proposal.payload, edited, confirm, now) {
            Ok(saved_as) => {
                proposal.close(Outcome::Accepted { at: now, saved_as, edited: edited_given }, now);
                proposal.seen = true;
                self.save_proposal(&proposal)?;
                Ok(proposal)
            }
            Err(text) => {
                proposal.close(
                    Outcome::Declined {
                        at: now,
                        reason: Some(DeclineReason::Other { text: text.clone() }),
                    },
                    now,
                );
                self.save_proposal(&proposal)?;
                Err(Error::Invalid(text))
            }
        }
    }

    /// Decline a proposal, with an optional reason.
    ///
    /// A declined `Create` of a memory is kept, not merely closed: unless
    /// the reason is [`DeclineReason::NotNow`] -- timing, not truth -- the
    /// memory is saved anyway with [`MemoryOrigin::Rejected`], so a later
    /// dream reads it under "do not assume" rather than noticing the same
    /// thing again next week. A declined `SendMail` leaves the draft exactly
    /// as it was: declining is "do not send this", not "throw it away".
    pub fn decline_proposal(
        &self,
        id: ProposalId,
        reason: Option<DeclineReason>,
        now: Timestamp,
    ) -> Result<Proposal> {
        self.writable()?;
        let mut proposal = self.with_proposals(|p| p.get_proposal(id))?;
        if !proposal.is_pending() {
            return Err(Error::Invalid("this proposal has already been answered".into()));
        }
        if let Payload::Create { record: ProposedRecord::Memory(memory) } = &proposal.payload
            && !matches!(reason, Some(DeclineReason::NotNow))
        {
            let mut rejected = memory.clone();
            rejected.origin = MemoryOrigin::Rejected;
            rejected.updated_at = now;
            self.save_memory(&rejected)?;
        }
        proposal.close(Outcome::Declined { at: now, reason }, now);
        proposal.seen = true;
        self.save_proposal(&proposal)?;
        Ok(proposal)
    }

    /// `edited` must name the same record the proposal carries, and only
    /// applies to `Create` and `Replace` -- a `Delete` or a `SendMail` has no
    /// record to edit.
    fn check_edit_shape(&self, payload: &Payload, edited: &Option<ProposedRecord>) -> Result<()> {
        let Some(edited) = edited else { return Ok(()) };
        let Some(record) = payload.record() else {
            return Err(Error::Invalid(
                "an edit only applies to a proposal that creates or replaces a record".into(),
            ));
        };
        if edited.kind() != record.kind() || edited.id() != record.id() {
            return Err(Error::Invalid(
                "the edited record does not match what was proposed".into(),
            ));
        }
        Ok(())
    }

    /// The part that can fail in plain words: the references a record
    /// carries, its own validation, staleness against a `Replace`'s
    /// `expected_updated_at`, and whether a `Create` already exists. `Ok`
    /// carries the id of what was saved or removed, for
    /// `Outcome::Accepted::saved_as`.
    fn try_accept(
        &self,
        payload: &Payload,
        edited: Option<ProposedRecord>,
        confirm: bool,
        now: Timestamp,
    ) -> Result<String, String> {
        let edited_given = edited.is_some();
        match payload {
            Payload::SendMail { .. } => {
                Err("a mail proposal is sent through the outbox, not accepted here".into())
            }
            Payload::Delete { kind, id } => self.accept_delete(*kind, id),
            Payload::Create { record } => {
                let record = edited.unwrap_or_else(|| record.clone());
                self.check_references(&record)?;
                record.validate().map_err(|e| e.to_string())?;
                if self.record_exists(&record) {
                    return Err("this has already been created".into());
                }
                self.save_record(record, confirm, edited_given, now)
            }
            Payload::Replace { record, expected_updated_at } => {
                let record = edited.unwrap_or_else(|| record.clone());
                self.check_references(&record)?;
                record.validate().map_err(|e| e.to_string())?;
                let current = self.current_updated_at(&record)?;
                if current != *expected_updated_at {
                    return Err("it changed since this was proposed".into());
                }
                self.save_record(record, confirm, edited_given, now)
            }
        }
    }

    /// The references a record carries beyond itself -- the project a task
    /// is filed under, the task or project a block is for, the goal or role
    /// a purpose names -- checked here because they may have gone since the
    /// proposal was made and nothing else re-checks them before this saves.
    /// A routine's own reference (`Trigger::BeforeEvent`'s role) is
    /// [`Vault::save_routine`]'s to check, and it already does.
    fn check_references(&self, record: &ProposedRecord) -> Result<(), String> {
        match record {
            ProposedRecord::Task(t) => {
                if let Some(project_id) = t.project_id {
                    self.project(project_id)
                        .map_err(|_| "the project this was for has been deleted".to_string())?;
                }
                if let Some(parent_id) = t.parent_id {
                    self.task(parent_id)
                        .map_err(|_| "the task this was under has been deleted".to_string())?;
                }
                self.check_purpose(t.purpose)?;
            }
            ProposedRecord::Block(b) => {
                match b.subject {
                    BlockSubject::Task { id } => {
                        self.task(id).map_err(|_| {
                            "the task this time was for has been deleted".to_string()
                        })?;
                    }
                    BlockSubject::Project { id } => {
                        self.project(id).map_err(|_| {
                            "the project this time was for has been deleted".to_string()
                        })?;
                    }
                    BlockSubject::Adhoc => {}
                }
                self.check_purpose(b.purpose)?;
            }
            ProposedRecord::Note(n) => self.check_purpose(n.purpose)?,
            ProposedRecord::Memory(_) | ProposedRecord::Routine(_) => {}
        }
        Ok(())
    }

    fn check_purpose(&self, purpose: Option<Purpose>) -> Result<(), String> {
        match purpose {
            Some(Purpose::Goal { id }) => {
                self.goal(id).map_err(|_| "the goal this was for has been deleted".to_string())?;
            }
            Some(Purpose::Role { id }) => {
                self.role(id).map_err(|_| "the role this was for has been deleted".to_string())?;
            }
            None => {}
        }
        Ok(())
    }

    /// Whether a record with this id is already there -- the check a
    /// `Create` needs and none of the domain saves make for it, because most
    /// callers building a fresh record never collide with one.
    fn record_exists(&self, record: &ProposedRecord) -> bool {
        match record {
            ProposedRecord::Task(t) => self.task(t.id).is_ok(),
            ProposedRecord::Block(b) => self.block(b.id).is_ok(),
            ProposedRecord::Memory(m) => {
                self.memories().is_ok_and(|ms| ms.iter().any(|x| x.id == m.id))
            }
            ProposedRecord::Routine(r) => self.routine(r.id).is_ok(),
            ProposedRecord::Note(n) => self.note(n.id).is_ok(),
        }
    }

    /// The `updated_at` a `Replace` must still match, in plain words when
    /// the record is gone rather than merely stale.
    fn current_updated_at(&self, record: &ProposedRecord) -> Result<Timestamp, String> {
        let gone = || "it no longer exists".to_string();
        match record {
            ProposedRecord::Task(t) => self.task(t.id).map(|x| x.updated_at).map_err(|_| gone()),
            ProposedRecord::Block(b) => self.block(b.id).map(|x| x.updated_at).map_err(|_| gone()),
            ProposedRecord::Memory(m) => self
                .memories()
                .map_err(|_| gone())?
                .into_iter()
                .find(|x| x.id == m.id)
                .map(|x| x.updated_at)
                .ok_or_else(gone),
            ProposedRecord::Routine(r) => {
                self.routine(r.id).map(|x| x.updated_at).map_err(|_| gone())
            }
            ProposedRecord::Note(n) => self.note(n.id).map(|x| x.updated_at).map_err(|_| gone()),
        }
    }

    /// Save the accepted record through the same domain save its own tool
    /// would use, stamping `updated_at` and, for a memory, its `origin`.
    ///
    /// A task and a block go through their own `save_*`, which validate
    /// them again -- cheap, and one less place that can drift from what a
    /// direct save enforces. A routine goes through [`Vault::save_routine`],
    /// which also checks the one reference this module does not:
    /// `Trigger::BeforeEvent`'s role. A note goes through
    /// [`Vault::overwrite_note`], the force path: staleness was already
    /// checked, against the proposal's own `expected_updated_at`, so a
    /// second conflict check against whatever `updated_at` a concurrent
    /// autosave left would be answering a question nobody asked twice.
    ///
    /// A memory's `origin` is `Confirmed` when the person said this is true
    /// (`confirm`) or rewrote it (`edited_given`), and `Inferred` otherwise;
    /// `last_supported` is left as the proposal carried it. A routine
    /// proposal is always saved [`RoutineKind::Custom`]: nothing this vault
    /// proposes is one of the application's own dream routines.
    fn save_record(
        &self,
        mut record: ProposedRecord,
        confirm: bool,
        edited_given: bool,
        now: Timestamp,
    ) -> Result<String, String> {
        let id = record.id();
        let result = match &mut record {
            ProposedRecord::Task(t) => {
                t.updated_at = now;
                self.save_task(t)
            }
            ProposedRecord::Block(b) => {
                b.updated_at = now;
                self.save_block(b)
            }
            ProposedRecord::Memory(m) => {
                m.updated_at = now;
                m.origin = if confirm || edited_given {
                    MemoryOrigin::Confirmed
                } else {
                    MemoryOrigin::Inferred
                };
                self.save_memory(m).map(|_| ())
            }
            ProposedRecord::Routine(r) => {
                r.updated_at = now;
                r.kind = RoutineKind::Custom;
                self.save_routine(r)
            }
            ProposedRecord::Note(n) => {
                n.updated_at = now;
                self.overwrite_note(n)
            }
        };
        result.map(|()| id).map_err(|e| e.to_string())
    }

    /// Remove a record by kind and id, in plain words when it is not there
    /// to remove. A dream routine refuses here as well as in
    /// [`Vault::delete_routine`]'s own guard, because a proposal that wanted
    /// one gone should say so rather than let the delete silently fail
    /// underneath it.
    fn accept_delete(&self, kind: ProposalKind, id: &str) -> Result<String, String> {
        let gone = || "it was already gone".to_string();
        match kind {
            ProposalKind::Task => {
                let task_id = TaskId::parse(id).map_err(|_| gone())?;
                self.task(task_id).map_err(|_| gone())?;
                self.delete_task(task_id).map_err(|e| e.to_string())?;
            }
            ProposalKind::Block => {
                let block_id = BlockId::parse(id).map_err(|_| gone())?;
                self.block(block_id).map_err(|_| gone())?;
                self.delete_block(block_id).map_err(|e| e.to_string())?;
            }
            ProposalKind::Memory => {
                let memory_id = MemoryId::parse(id).map_err(|_| gone())?;
                if !self.memories().is_ok_and(|ms| ms.iter().any(|m| m.id == memory_id)) {
                    return Err(gone());
                }
                self.delete_memory(memory_id).map_err(|e| e.to_string())?;
            }
            ProposalKind::Routine => {
                let routine_id = RoutineId::parse(id).map_err(|_| gone())?;
                let routine = self.routine(routine_id).map_err(|_| gone())?;
                if routine.kind.is_dream() {
                    return Err("a dream routine cannot be deleted".into());
                }
                self.delete_routine(routine_id).map_err(|e| e.to_string())?;
            }
            ProposalKind::Note => {
                let note_id = NoteId::parse(id).map_err(|_| gone())?;
                self.note(note_id).map_err(|_| gone())?;
                self.delete_note(note_id).map_err(|e| e.to_string())?;
            }
            ProposalKind::Mail => return Err("a mail proposal cannot delete anything".into()),
        }
        Ok(id.to_string())
    }
}
