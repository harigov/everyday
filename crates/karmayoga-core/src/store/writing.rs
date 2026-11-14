//! Storage for writing help: every time it was asked, and what came of it.
//!
//! One record, no cascade. In the clear: when it was asked, which is what
//! the history is ordered by. Sealed: everything else -- what was selected,
//! where, what was asked of it and what came back -- because a database that
//! said "rewrote a message in Slack at nine" would already be saying a lot,
//! and one that held the message would be saying everything.

use crate::Result;
use crate::id::WritingId;
use crate::writing::{WritingAction, WritingAssist, WritingOutcome};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Filter and pagination for [`WritingStore::list_writing_assists`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WritingQuery {
    /// Keep only these actions. Empty means any.
    pub actions: Vec<WritingAction>,
    /// Keep only these outcomes. Empty means any.
    pub outcomes: Vec<WritingOutcome>,
    /// Asked at or after this.
    pub since: Option<Timestamp>,
    pub limit: Option<u32>,
}

impl WritingQuery {
    /// Does this assist pass the filters? The fallback for a backend that
    /// cannot express one natively, so behaviour is identical across them.
    pub fn matches(&self, assist: &WritingAssist) -> bool {
        if let Some(since) = self.since
            && assist.created_at < since
        {
            return false;
        }
        (self.actions.is_empty() || self.actions.contains(&assist.action))
            && (self.outcomes.is_empty() || self.outcomes.contains(&assist.outcome))
    }

    /// Filter, sort newest first and cap. Done here rather than in SQL so
    /// that two backends cannot disagree about where a tie goes.
    pub fn apply(&self, mut rows: Vec<WritingAssist>) -> Vec<WritingAssist> {
        rows.retain(|a| self.matches(a));
        rows.sort_by(|a, b| {
            b.created_at.cmp(&a.created_at).then_with(|| b.id.to_string().cmp(&a.id.to_string()))
        });
        if let Some(limit) = self.limit {
            rows.truncate(limit as usize);
        }
        rows
    }
}

/// What a backend must do to hold writing help.
pub trait WritingStore: Send + Sync {
    fn list_writing_assists(&self, query: &WritingQuery) -> Result<Vec<WritingAssist>>;
    fn get_writing_assist(&self, id: WritingId) -> Result<WritingAssist>;
    /// Idempotent: saving the same assist twice leaves one.
    fn put_writing_assist(&self, assist: &WritingAssist) -> Result<()>;
    /// Deleting one that is not there is not an error.
    fn delete_writing_assist(&self, id: WritingId) -> Result<()>;
}

/// Additional authenticated data for a sealed writing-assist payload.
pub fn writing_assist_aad(id: WritingId) -> Vec<u8> {
    format!("everyday.writing_assist.v1:{id}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(1_700_000_000 + second).unwrap()
    }

    fn assist(second: i64, action: WritingAction, outcome: WritingOutcome) -> WritingAssist {
        let mut a = WritingAssist::new(action, format!("text {second}"), None);
        a.created_at = at(second);
        a.outcome = outcome;
        a
    }

    #[test]
    fn an_empty_query_keeps_everything_newest_first() {
        let rows = vec![
            assist(10, WritingAction::Improve, WritingOutcome::Replaced),
            assist(30, WritingAction::Grammar, WritingOutcome::Cancelled),
            assist(20, WritingAction::Concise, WritingOutcome::Failed),
        ];
        let got: Vec<_> =
            WritingQuery::default().apply(rows).into_iter().map(|a| a.original).collect();
        assert_eq!(got, ["text 30", "text 20", "text 10"]);
    }

    #[test]
    fn filters_and_limit_combine() {
        let rows = vec![
            assist(10, WritingAction::Grammar, WritingOutcome::Replaced),
            assist(20, WritingAction::Grammar, WritingOutcome::Cancelled),
            assist(30, WritingAction::Improve, WritingOutcome::Replaced),
            assist(40, WritingAction::Grammar, WritingOutcome::Replaced),
        ];
        let query = WritingQuery {
            actions: vec![WritingAction::Grammar],
            outcomes: vec![WritingOutcome::Replaced],
            since: Some(at(5)),
            limit: Some(1),
        };
        let got: Vec<_> = query.apply(rows).into_iter().map(|a| a.original).collect();
        assert_eq!(got, ["text 40"]);
    }
}
