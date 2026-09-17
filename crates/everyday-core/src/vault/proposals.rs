//! Proposals: work the assistant prepared and did not do.
//!
//! The plumbing is here -- list, save, count, expire. Accepting and declining,
//! which are the part with rules in them, live beside it.

use super::Vault;
use super::session::Domain;
use crate::error::{Error, Result};
use crate::id::ProposalId;
use crate::proposal::{MAX_PENDING_PROPOSALS, Proposal};
use crate::store::proposals::{ProposalQuery, ProposalStore};
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
        self.with_proposals(|p| p.put_proposal(proposal))
    }

    pub fn delete_proposal(&self, id: ProposalId) -> Result<()> {
        self.writable()?;
        self.with_proposals(|p| p.delete_proposal(id))
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
        self.with_proposals(|p| p.mark_proposals_seen(ids))
    }

    /// Close every pending proposal whose time has passed.
    pub fn expire_proposals(&self, now: Timestamp) -> Result<Vec<ProposalId>> {
        self.writable()?;
        self.with_proposals(|p| p.expire_proposals(now))
    }

    /// Accept a proposal: validate it, check what it refers to still exists,
    /// save the record through the domain's own save, and close it.
    ///
    /// `edited` is the record as the person changed it before accepting;
    /// `confirm` says the person accepted a memory as *true* (from the
    /// memory list) rather than merely "put that there".
    ///
    /// Mail is not accepted here: sending belongs to the service's outbox.
    pub fn accept_proposal(
        &self,
        id: ProposalId,
        edited: Option<crate::proposal::ProposedRecord>,
        confirm: bool,
        now: Timestamp,
    ) -> Result<Proposal> {
        let _ = (id, edited, confirm, now);
        Err(Error::Unsupported("accepting proposals (not built yet)"))
    }

    /// Decline a proposal, with an optional reason.
    pub fn decline_proposal(
        &self,
        id: ProposalId,
        reason: Option<crate::proposal::DeclineReason>,
        now: Timestamp,
    ) -> Result<Proposal> {
        let _ = (id, reason, now);
        Err(Error::Unsupported("declining proposals (not built yet)"))
    }
}
