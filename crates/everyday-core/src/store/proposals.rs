//! Storage for proposals: work the assistant prepared and did not do.
//!
//! One table, no cascade. In the clear: what kind of record a proposal is,
//! the day it would be drawn on, when it was made and when it expires,
//! whether it is still pending, and whether anybody has looked at it. That is
//! what the ghosts in each app, the expiry sweep and the count on the app bar
//! are queried by. Sealed: the record itself, the caption, the reason, and
//! what it was about. A database can therefore say that three tasks were
//! proposed for Thursday and one was declined -- never what any of them was.
//!
//! See [`crate::proposal`] for the record and `docs/plans/dreaming.md` for
//! the design.

use crate::Result;
use crate::id::{ProposalId, RoutineRunId};
use crate::proposal::{AboutKind, Proposal, ProposalKind, ProposalSource, ProposalState};
use jiff::Timestamp;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};

/// Filter and pagination for [`ProposalStore::list_proposals`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProposalQuery {
    /// Keep only these kinds. Empty means any.
    pub kinds: Vec<ProposalKind>,
    /// Keep only these states. Empty means any.
    pub states: Vec<ProposalState>,
    /// Drawn on or after this day. A proposal with no day is excluded when
    /// either bound is set.
    pub from: Option<Date>,
    /// Drawn on or before this day.
    pub to: Option<Date>,
    /// Made at or after this instant. What "the outcomes this week" asks.
    pub made_since: Option<Timestamp>,
    /// About this kind of record.
    pub about_kind: Option<AboutKind>,
    /// About this record.
    pub about_id: Option<String>,
    /// Made by this run.
    pub run_id: Option<RoutineRunId>,
    /// Keep only proposals nobody has looked at.
    pub unseen: Option<bool>,
    pub limit: Option<u32>,
}

impl ProposalQuery {
    /// Everything still waiting for an answer.
    pub fn pending() -> Self {
        Self { states: vec![ProposalState::Pending], ..Default::default() }
    }

    /// Pending proposals of one kind.
    pub fn pending_of(kind: ProposalKind) -> Self {
        Self { kinds: vec![kind], ..Self::pending() }
    }

    /// Does this proposal pass the filters? The fallback for a backend that
    /// cannot express one natively, so behaviour is identical across them.
    pub fn matches(&self, p: &Proposal) -> bool {
        if !self.kinds.is_empty() && !self.kinds.contains(&p.kind) {
            return false;
        }
        if !self.states.is_empty() && !self.states.contains(&p.outcome.state()) {
            return false;
        }
        if self.from.is_some() || self.to.is_some() {
            let Some(day) = p.target_date else { return false };
            if self.from.is_some_and(|from| day < from) || self.to.is_some_and(|to| day > to) {
                return false;
            }
        }
        if self.made_since.is_some_and(|since| p.made_at < since) {
            return false;
        }
        if let Some(kind) = self.about_kind
            && p.about.as_ref().is_none_or(|a| a.kind != kind)
        {
            return false;
        }
        if let Some(id) = &self.about_id
            && p.about.as_ref().is_none_or(|a| &a.id != id)
        {
            return false;
        }
        if let Some(run) = self.run_id
            && p.made_by != Some(ProposalSource::Run { run_id: run })
        {
            return false;
        }
        if let Some(unseen) = self.unseen
            && p.seen == unseen
        {
            return false;
        }
        true
    }

    /// Filter, sort newest first, and cap. Done here rather than in SQL so
    /// that two backends cannot disagree about where a tie goes.
    pub fn apply(&self, mut rows: Vec<Proposal>) -> Vec<Proposal> {
        rows.retain(|p| self.matches(p));
        rows.sort_by(|a, b| {
            b.made_at.cmp(&a.made_at).then_with(|| b.id.to_string().cmp(&a.id.to_string()))
        });
        if let Some(limit) = self.limit {
            rows.truncate(limit as usize);
        }
        rows
    }
}

/// What a backend must do to hold proposals.
pub trait ProposalStore: Send + Sync {
    fn list_proposals(&self, query: &ProposalQuery) -> Result<Vec<Proposal>>;
    fn get_proposal(&self, id: ProposalId) -> Result<Proposal>;
    /// Idempotent: saving the same proposal twice leaves one.
    fn put_proposal(&self, proposal: &Proposal) -> Result<()>;
    /// Deleting one that is not there is not an error.
    fn delete_proposal(&self, id: ProposalId) -> Result<()>;

    /// Close every pending proposal whose `expires_at` is at or before
    /// `now`, as [`Outcome::Expired`](crate::proposal::Outcome::Expired) at
    /// `now`. Returns the ids closed.
    ///
    /// A read-modify-reseal, not one `UPDATE`: the state lives in the clear
    /// column *and* in the sealed payload, and moving one without the other
    /// is the bug `mark_runs_seen` already records.
    fn expire_proposals(&self, now: Timestamp) -> Result<Vec<ProposalId>>;

    /// How many proposals are pending. One indexed count.
    fn count_pending(&self) -> Result<u64>;

    /// How many pending proposals nobody has looked at. The app bar's half.
    fn count_unseen_pending(&self) -> Result<u64>;

    /// Mark proposals as looked at. Empty means every pending one.
    fn mark_proposals_seen(&self, ids: &[ProposalId]) -> Result<()>;
}

/// Additional authenticated data for a sealed proposal.
pub fn proposal_aad(id: ProposalId) -> Vec<u8> {
    format!("everyday.proposal.v1:{id}").into_bytes()
}
