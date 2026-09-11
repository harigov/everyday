//! The tracking domain: what you decided to record, and every value of it.
//!
//! The clear/sealed split this crate makes everywhere goes furthest here,
//! and on purpose. `tracker_id`, `local_date`, `at_us` and `value` are all
//! in the clear, which means every question this domain exists to answer --
//! a monthly average, a streak, minutes per week -- is a `GROUP BY` over an
//! index rather than a decryption of the vault. What stays sealed is the
//! only part that identifies anything: the tracker's *name*, its unit and
//! its cadence, all inside the definition's payload, and the note attached
//! to a reading. The database says tracker `7f3a...` was `500` at 08:12 on
//! the 14th and never what `7f3a...` is.

use everyday_core::error::{Error, Result};
use everyday_core::id::{JournalId, ReadingId, TrackerId};
use everyday_core::purpose::Purpose;
use everyday_core::store::trackers::{
    ReadingQuery, TrackerDay, TrackerStore, reading_aad, tracker_aad,
};
use everyday_core::tracker::{Reading, Tracker};
use jiff::civil::Date;

use crate::conn::{Sql, SqlExt, ToValue, Value, Where};
use crate::purpose::{RecordKind, forget_purposes};
use crate::record::Record;
use crate::{SqlStore, from_us, id_str, to_us, vals};

impl Record for Tracker {
    const TABLE: &'static str = "trackers";
    const KIND: &'static str = "tracker";
    type Id = TrackerId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        tracker_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("archived", self.archived.to_value()),
            ("sort_order", self.sort_order.to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }

    fn purpose_kind() -> Option<RecordKind> {
        Some(RecordKind::Tracker)
    }

    fn purpose(&self) -> Option<&Purpose> {
        self.purpose.as_ref()
    }
}

impl Record for Reading {
    const TABLE: &'static str = "readings";
    const KIND: &'static str = "reading";
    type Id = ReadingId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        reading_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("journal_id", id_str(self.journal_id).to_value()),
            ("tracker_id", self.tracker_id.to_string().to_value()),
            ("entry_id", id_str(self.entry_id).to_value()),
            ("local_date", self.local_date.to_string().to_value()),
            ("at_us", self.at.map(to_us).to_value()),
            ("value", self.value.to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl TrackerStore for SqlStore {
    // ---- definitions ----------------------------------------------------

    fn list_trackers(&self) -> Result<Vec<Tracker>> {
        let rows = self
            .read()
            .records("SELECT id, data FROM trackers ORDER BY sort_order, created_us", &[])?;
        self.collect(rows, tracker_aad)
    }

    fn get_tracker(&self, id: TrackerId) -> Result<Tracker> {
        self.get(id)
    }

    fn put_tracker(&self, t: &Tracker) -> Result<()> {
        self.upsert(t)
    }

    fn delete_tracker(&self, id: TrackerId) -> Result<u64> {
        // Both halves in one transaction, which is the whole reason this is
        // one method rather than two calls the vault makes in order: the
        // survivable half-done state -- a definition whose history is gone,
        // and not a year of numbers nothing can name -- stops being a
        // question anyone has to reason about.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let removed =
            tx.execute("DELETE FROM readings WHERE tracker_id = ?1", &vals![id.to_string()])?;
        tx.execute("DELETE FROM trackers WHERE id = ?1", &vals![id.to_string()])?;
        forget_purposes(tx.as_mut(), RecordKind::Tracker, &[id.to_string()])?;
        tx.commit()?;
        Ok(removed)
    }

    fn merge_trackers(&self, from: TrackerId, into: TrackerId) -> Result<u64> {
        // A read-modify-reseal per reading, via `rewrite_each`, for the
        // reason its own docs give: `tracker_id` exists twice, as the clear
        // column the index is built on and inside the sealed payload, and
        // updating only the column would leave the record disagreeing with
        // itself. The sealed copy is the one a restore would believe.
        //
        // Affordable because of what it operates on: the history of one
        // tracker somebody is tidying up, once.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let moved = self.rewrite_each::<Reading>(
            tx.as_mut(),
            "SELECT id, data FROM readings WHERE tracker_id = ?1",
            &vals![from.to_string()],
            |r| r.tracker_id = into,
        )?;
        tx.execute("DELETE FROM trackers WHERE id = ?1", &vals![from.to_string()])?;
        forget_purposes(tx.as_mut(), RecordKind::Tracker, &[from.to_string()])?;
        tx.commit()?;
        Ok(moved)
    }

    // ---- readings -------------------------------------------------------

    fn list_readings(&self, query: &ReadingQuery) -> Result<Vec<Reading>> {
        let (where_sql, args) = where_clause(query);
        let mut sql = format!("SELECT id, data FROM readings WHERE {where_sql}");
        // Untimed readings first within a day, matching `ReadingQuery::apply`
        // so the two orderings cannot drift. `NULLS FIRST` is spelled out
        // because the two databases have opposite defaults for it -- SQLite
        // sorts NULL before any value on an ASC column, Postgres after -- and
        // a reading with no minute landing at the wrong end of a day would be
        // a plausible-looking wrong answer rather than an error.
        self.page(&mut sql, "local_date ASC, at_us ASC NULLS FIRST, id ASC", query.limit, 0);

        let rows = self.read().records(&sql, &args)?;
        self.collect(rows, reading_aad)
    }

    fn get_reading(&self, id: ReadingId) -> Result<Reading> {
        self.get(id)
    }

    fn put_reading(&self, r: &Reading) -> Result<()> {
        self.upsert(r)
    }

    fn delete_reading(&self, id: ReadingId) -> Result<()> {
        self.delete_by_id::<Reading>(id)?;
        Ok(())
    }

    fn delete_readings_of(&self, tracker: TrackerId) -> Result<u64> {
        self.write()
            .execute("DELETE FROM readings WHERE tracker_id = ?1", &vals![tracker.to_string()])
    }

    fn detach_readings_in(&self, journal: JournalId) -> Result<u64> {
        // The same read-modify-reseal `detach_readings_from` does for an
        // entry, and for the same reason: `journal_id` exists as a clear
        // column and inside the sealed payload, and a row where the two
        // disagree is a row a restore would read differently.
        //
        // `delete_journal` calls `detach_readings_in_tx` directly, inside
        // its own transaction, so an entry's readings and the journal it
        // names disappear together; this trait method is the standalone
        // path -- the one `Vault::delete_journal` reaches for when there is
        // no larger transaction to share -- and opens one of its own.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let n = self.detach_readings_in_tx(tx.as_mut(), journal)?;
        tx.commit()?;
        Ok(n)
    }

    /// The aggregate, done where the data is.
    ///
    /// Nothing is decrypted here: every column this reads is a clear one, so
    /// a year of one tracker costs an index range scan and returns 365 rows
    /// instead of 3,000 sealed payloads for the caller to open and add up.
    /// That difference is the reason `value` is not sealed -- and it is worth
    /// more, not less, on a backend where those payloads would come over a
    /// network.
    ///
    /// No `LIMIT`, matching `ReadingQuery::totals` -- see that query's
    /// `limit` field for why an aggregate does not paginate.
    fn tracker_days(&self, query: &ReadingQuery) -> Result<Vec<TrackerDay>> {
        let (where_sql, args) = where_clause(query);
        let sql = format!(
            "SELECT tracker_id, local_date, COUNT(*), SUM(value), MAX(value),
                    MIN(at_us), MAX(at_us)
             FROM readings WHERE {where_sql}
             GROUP BY tracker_id, local_date
             ORDER BY local_date ASC, tracker_id ASC"
        );

        let rows = self.read().query(&sql, &args)?;
        rows.into_iter()
            .map(|row| {
                Ok(TrackerDay {
                    tracker_id: TrackerId::parse(&row.text(0)?)
                        .map_err(|e| Error::Invalid(e.to_string()))?,
                    date: row
                        .text(1)?
                        .parse::<Date>()
                        .map_err(|e| Error::Invalid(e.to_string()))?,
                    count: u32::try_from(row.i64(2)?).unwrap_or(u32::MAX),
                    sum: row.opt_f64(3)?.unwrap_or(0.0),
                    max: row.opt_f64(4)?.unwrap_or(0.0),
                    first_at: row.opt_i64(5)?.map(from_us),
                    last_at: row.opt_i64(6)?.map(from_us),
                })
            })
            .collect()
    }
}

impl SqlStore {
    /// The actual detach, inside whatever transaction the caller is
    /// running. [`TrackerStore::detach_readings_in`] wraps this in a
    /// transaction of its own for a caller with no transaction already open;
    /// `delete_journal` passes its own instead, so the journal's row and its
    /// readings' pointers go in one commit rather than two.
    pub(crate) fn detach_readings_in_tx(
        &self,
        tx: &mut dyn Sql,
        journal: JournalId,
    ) -> Result<u64> {
        self.rewrite_each::<Reading>(
            tx,
            "SELECT id, data FROM readings WHERE journal_id = ?1",
            &vals![journal.to_string()],
            |r| r.journal_id = None,
        )
    }
}

/// The filters both queries share, as SQL and its arguments.
///
/// Written once because `list_readings` and `tracker_days` must agree about
/// what "this window" means: a chart whose totals covered a different set of
/// rows than the list beneath it would be a bug nobody could see.
fn where_clause(query: &ReadingQuery) -> (String, Vec<Value>) {
    let mut w = Where::new();
    if let Some(j) = query.journal_id {
        w = w.eq("journal_id", j.to_string());
    }
    if let Some(e) = query.entry_id {
        w = w.eq("entry_id", e.to_string());
    }
    if let Some(from) = query.from {
        w = w.gte("local_date", from.to_string());
    }
    if let Some(to) = query.to {
        w = w.lte("local_date", to.to_string());
    }
    if query.timed_only {
        w = w.not_null("at_us");
    }
    if !query.tracker_ids.is_empty() {
        w = w.in_list("tracker_id", query.tracker_ids.iter().map(|id| id.to_string()));
    }
    w.finish()
}
