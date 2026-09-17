//! Proposals: work the assistant prepared and did not do.
//!
//! One table and no cascade. The only thing worth reading for is that every
//! change to a proposal's state is a read-modify-reseal, because the state is
//! in the clear column *and* the sealed payload -- the bug `mark_runs_seen`
//! records, not repeated here.

use everyday_core::error::Result;
use everyday_core::id::ProposalId;
use everyday_core::proposal::{Outcome, Proposal, ProposalState};
use everyday_core::store::proposals::{ProposalQuery, ProposalStore, proposal_aad};
use jiff::Timestamp;

use crate::conn::{SqlExt, ToValue, Value, Where};
use crate::record::Record;
use crate::{SqlStore, date_str, to_us, vals};

impl Record for Proposal {
    const TABLE: &'static str = "proposals";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("kind", self.kind.as_str().to_value()),
            ("target_date", date_str(self.target_date).to_value()),
            ("made_us", to_us(self.made_at).to_value()),
            ("expires_us", to_us(self.expires_at).to_value()),
            ("outcome", self.outcome.state().as_str().to_value()),
            ("seen", self.seen.to_value()),
        ]
    }
}

impl ProposalStore for SqlStore {
    fn list_proposals(&self, query: &ProposalQuery) -> Result<Vec<Proposal>> {
        // What an index can answer is pushed down; the rest -- what a
        // proposal is about, who made it -- is sealed, and is finished in
        // Rust by `ProposalQuery::apply`, the path every backend shares.
        let mut w = Where::new();
        if !query.states.is_empty() {
            w = w.in_list("outcome", query.states.iter().map(|s| s.as_str().to_string()));
        }
        if !query.kinds.is_empty() {
            w = w.in_list("kind", query.kinds.iter().map(|k| k.as_str().to_string()));
        }
        if let Some(from) = query.from {
            w = w.gte("target_date", from.to_string());
        }
        if let Some(to) = query.to {
            w = w.lte("target_date", to.to_string());
        }
        if let Some(since) = query.made_since {
            w = w.gte("made_us", to_us(since));
        }
        if let Some(unseen) = query.unseen {
            w = w.eq("seen", !unseen);
        }
        let (where_sql, args) = w.finish();
        let sql = format!("SELECT id, data FROM proposals WHERE {where_sql} ORDER BY made_us DESC");
        let rows = self.read().records(&sql, &args)?;
        Ok(query.apply(self.collect(rows, proposal_aad)?))
    }

    fn get_proposal(&self, id: ProposalId) -> Result<Proposal> {
        self.get(id)
    }

    fn put_proposal(&self, proposal: &Proposal) -> Result<()> {
        self.upsert(proposal)
    }

    fn delete_proposal(&self, id: ProposalId) -> Result<()> {
        self.delete_by_id::<Proposal>(id)?;
        Ok(())
    }

    fn expire_proposals(&self, now: Timestamp) -> Result<Vec<ProposalId>> {
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let mut closed = Vec::new();
        self.rewrite_each::<Proposal>(
            tx.as_mut(),
            "SELECT id, data FROM proposals WHERE outcome = ?1 AND expires_us <= ?2",
            &vals![ProposalState::Pending.as_str(), to_us(now)],
            |p| {
                // The column said pending; the payload is the authority.
                if p.is_pending() {
                    p.close(Outcome::Expired { at: now }, now);
                    closed.push(p.id);
                }
            },
        )?;
        tx.commit()?;
        Ok(closed)
    }

    fn count_pending(&self) -> Result<u64> {
        let n = self.read().scalar_i64(
            "SELECT COUNT(*) FROM proposals WHERE outcome = ?1",
            &vals![ProposalState::Pending.as_str()],
        )?;
        Ok(n.max(0) as u64)
    }

    fn count_unseen_pending(&self) -> Result<u64> {
        let n = self.read().scalar_i64(
            "SELECT COUNT(*) FROM proposals WHERE outcome = ?1 AND NOT seen",
            &vals![ProposalState::Pending.as_str()],
        )?;
        Ok(n.max(0) as u64)
    }

    fn mark_proposals_seen(&self, ids: &[ProposalId]) -> Result<()> {
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        if ids.is_empty() {
            self.rewrite_each::<Proposal>(
                tx.as_mut(),
                "SELECT id, data FROM proposals WHERE outcome = ?1 AND NOT seen",
                &vals![ProposalState::Pending.as_str()],
                |p| p.seen = true,
            )?;
        } else {
            for id in ids {
                self.rewrite_each::<Proposal>(
                    tx.as_mut(),
                    "SELECT id, data FROM proposals WHERE id = ?1 AND NOT seen",
                    &vals![id.to_string()],
                    |p| p.seen = true,
                )?;
            }
        }
        tx.commit()
    }
}
