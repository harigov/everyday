//! Writing help: every time it was asked, and what came of it.
//!
//! One straightforward table. Everything an assist is -- the selected text,
//! the application it came from, what was asked and what came back -- is
//! sealed; the one clear column is when it was asked, which is what the
//! history is ordered by.

use everyday_core::error::Result;
use everyday_core::id::WritingId;
use everyday_core::store::writing::{WritingQuery, WritingStore, writing_assist_aad};
use everyday_core::writing::WritingAssist;

use crate::conn::{SqlExt, ToValue, Value, Where};
use crate::record::Record;
use crate::{SqlStore, to_us};

impl Record for WritingAssist {
    const TABLE: &'static str = "writing_assists";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![("created_us", to_us(self.created_at).to_value())]
    }
}

impl WritingStore for SqlStore {
    fn list_writing_assists(&self, query: &WritingQuery) -> Result<Vec<WritingAssist>> {
        // Only the time is in the clear, so only `since` is pushed down; the
        // action and outcome are sealed and are finished in Rust by
        // `WritingQuery::apply`, the same code path every backend uses.
        let mut w = Where::new();
        if let Some(since) = query.since {
            w = w.gte("created_us", to_us(since));
        }
        let (where_sql, args) = w.finish();
        let sql = format!(
            "SELECT id, data FROM writing_assists WHERE {where_sql} ORDER BY created_us DESC"
        );
        let rows = self.read().records(&sql, &args)?;
        Ok(query.apply(self.collect(rows, writing_assist_aad)?))
    }

    fn get_writing_assist(&self, id: WritingId) -> Result<WritingAssist> {
        self.get(id)
    }

    fn put_writing_assist(&self, assist: &WritingAssist) -> Result<()> {
        self.upsert(assist)
    }

    fn delete_writing_assist(&self, id: WritingId) -> Result<()> {
        self.delete_by_id::<WritingAssist>(id)?;
        Ok(())
    }
}
