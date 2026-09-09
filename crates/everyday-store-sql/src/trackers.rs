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
use everyday_core::store::trackers::{
    ReadingQuery, TrackerDay, TrackerStore, reading_aad, tracker_aad,
};
use everyday_core::tracker::{Reading, Tracker};
use jiff::civil::Date;

use crate::conn::{SqlExt, Value};
use crate::purpose::{RecordKind, forget_purposes, set_purpose};
use crate::{SqlStore, from_us, id_str, to_us, vals};

impl TrackerStore for SqlStore {
    // ---- definitions ----------------------------------------------------

    fn list_trackers(&self) -> Result<Vec<Tracker>> {
        let rows = self
            .conn()
            .records("SELECT id, data FROM trackers ORDER BY sort_order, created_us", &[])?;
        self.collect(rows, tracker_aad)
    }

    fn get_tracker(&self, id: TrackerId) -> Result<Tracker> {
        let sealed = self
            .conn()
            .sealed("SELECT data FROM trackers WHERE id = ?1", &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found("tracker", id))?;
        self.unseal(&tracker_aad(id), &sealed)
    }

    fn put_tracker(&self, t: &Tracker) -> Result<()> {
        // The name, the unit, the icon and the cadence are all inside
        // `data`. A database whose trackers table said "sertraline" would
        // undo the whole point of sealing the readings.
        let data = self.seal(&tracker_aad(t.id), t)?;
        let mut conn = self.conn();
        let mut tx = conn.begin()?;
        tx.execute(
            "INSERT INTO trackers (id, archived, sort_order, created_us, updated_us, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (id) DO UPDATE SET
                archived = ?2, sort_order = ?3, created_us = ?4, updated_us = ?5, data = ?6",
            &vals![
                t.id.to_string(),
                t.archived,
                t.sort_order,
                to_us(t.created_at),
                to_us(t.updated_at),
                data,
            ],
        )?;
        set_purpose(tx.as_mut(), RecordKind::Tracker, &t.id.to_string(), t.purpose.as_ref())?;
        tx.commit()
    }

    fn delete_tracker(&self, id: TrackerId) -> Result<u64> {
        // Both halves in one transaction, which is the whole reason this is
        // one method rather than two calls the vault makes in order: the
        // survivable half-done state -- a definition whose history is gone,
        // and not a year of numbers nothing can name -- stops being a
        // question anyone has to reason about.
        let mut conn = self.conn();
        let mut tx = conn.begin()?;
        let removed =
            tx.execute("DELETE FROM readings WHERE tracker_id = ?1", &vals![id.to_string()])?;
        tx.execute("DELETE FROM trackers WHERE id = ?1", &vals![id.to_string()])?;
        forget_purposes(tx.as_mut(), RecordKind::Tracker, &[id.to_string()])?;
        tx.commit()?;
        Ok(removed)
    }

    fn merge_trackers(&self, from: TrackerId, into: TrackerId) -> Result<u64> {
        // A read-modify-reseal per reading, for the reason
        // `detach_readings_from` does one: `tracker_id` exists twice, as the
        // clear column the index is built on and inside the sealed payload,
        // and updating only the column would leave the record disagreeing
        // with itself. The sealed copy is the one a restore would believe.
        //
        // Affordable because of what it operates on: the history of one
        // tracker somebody is tidying up, once.
        let rows = self.conn().records(
            "SELECT id, data FROM readings WHERE tracker_id = ?1",
            &vals![from.to_string()],
        )?;
        let readings: Vec<Reading> = self.collect(rows, reading_aad)?;
        let resealed: Vec<(ReadingId, Vec<u8>)> = readings
            .into_iter()
            .map(|mut r| {
                r.tracker_id = into;
                let data = self.seal(&reading_aad(r.id), &r)?;
                Ok((r.id, data))
            })
            .collect::<Result<_>>()?;

        let moved = resealed.len() as u64;
        let mut conn = self.conn();
        let mut tx = conn.begin()?;
        for (id, data) in resealed {
            tx.execute(
                "UPDATE readings SET tracker_id = ?2, data = ?3 WHERE id = ?1",
                &vals![id.to_string(), into.to_string(), data],
            )?;
        }
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
        sql.push_str(" ORDER BY local_date ASC, at_us ASC NULLS FIRST, id ASC");
        if let Some(limit) = query.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        let rows = self.conn().records(&sql, &args)?;
        self.collect(rows, reading_aad)
    }

    fn get_reading(&self, id: ReadingId) -> Result<Reading> {
        let sealed = self
            .conn()
            .sealed("SELECT data FROM readings WHERE id = ?1", &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found("reading", id))?;
        self.unseal(&reading_aad(id), &sealed)
    }

    fn put_reading(&self, r: &Reading) -> Result<()> {
        let data = self.seal(&reading_aad(r.id), r)?;
        self.conn().execute(
            "INSERT INTO readings
                (id, journal_id, tracker_id, entry_id, local_date, at_us, value,
                 created_us, updated_us, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (id) DO UPDATE SET
                journal_id = ?2, tracker_id = ?3, entry_id = ?4, local_date = ?5,
                at_us = ?6, value = ?7, created_us = ?8, updated_us = ?9, data = ?10",
            &vals![
                r.id.to_string(),
                id_str(r.journal_id),
                r.tracker_id.to_string(),
                id_str(r.entry_id),
                r.local_date.to_string(),
                r.at.map(to_us),
                r.value,
                to_us(r.created_at),
                to_us(r.updated_at),
                data,
            ],
        )?;
        Ok(())
    }

    fn delete_reading(&self, id: ReadingId) -> Result<()> {
        self.conn().execute("DELETE FROM readings WHERE id = ?1", &vals![id.to_string()])?;
        Ok(())
    }

    fn delete_readings_of(&self, tracker: TrackerId) -> Result<u64> {
        self.conn()
            .execute("DELETE FROM readings WHERE tracker_id = ?1", &vals![tracker.to_string()])
    }

    fn detach_readings_in(&self, journal: JournalId) -> Result<u64> {
        // The same read-modify-reseal `detach_readings_from` does for an
        // entry, and for the same reason: `journal_id` exists as a clear
        // column and inside the sealed payload, and a row where the two
        // disagree is a row a restore would read differently.
        let rows = self.conn().records(
            "SELECT id, data FROM readings WHERE journal_id = ?1",
            &vals![journal.to_string()],
        )?;
        let readings: Vec<Reading> = self.collect(rows, reading_aad)?;
        let resealed: Vec<(ReadingId, Vec<u8>)> = readings
            .into_iter()
            .map(|mut r| {
                r.journal_id = None;
                let data = self.seal(&reading_aad(r.id), &r)?;
                Ok((r.id, data))
            })
            .collect::<Result<_>>()?;

        let changed = resealed.len() as u64;
        let mut conn = self.conn();
        let mut tx = conn.begin()?;
        for (id, data) in resealed {
            tx.execute(
                "UPDATE readings SET journal_id = NULL, data = ?2 WHERE id = ?1",
                &vals![id.to_string(), data],
            )?;
        }
        tx.commit()?;
        Ok(changed)
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

        let rows = self.conn().query(&sql, &args)?;
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

/// The filters both queries share, as SQL and its arguments.
///
/// Written once because `list_readings` and `tracker_days` must agree about
/// what "this window" means: a chart whose totals covered a different set of
/// rows than the list beneath it would be a bug nobody could see.
fn where_clause(query: &ReadingQuery) -> (String, Vec<Value>) {
    let mut sql = String::from("1=1");
    let mut args: Vec<Value> = Vec::new();

    if let Some(j) = query.journal_id {
        args.push(Value::Text(j.to_string()));
        sql.push_str(&format!(" AND journal_id = ?{}", args.len()));
    }
    if let Some(e) = query.entry_id {
        args.push(Value::Text(e.to_string()));
        sql.push_str(&format!(" AND entry_id = ?{}", args.len()));
    }
    if let Some(from) = query.from {
        args.push(Value::Text(from.to_string()));
        sql.push_str(&format!(" AND local_date >= ?{}", args.len()));
    }
    if let Some(to) = query.to {
        args.push(Value::Text(to.to_string()));
        sql.push_str(&format!(" AND local_date <= ?{}", args.len()));
    }
    if query.timed_only {
        sql.push_str(" AND at_us IS NOT NULL");
    }
    if !query.tracker_ids.is_empty() {
        let mut holes = Vec::new();
        for id in &query.tracker_ids {
            args.push(Value::Text(id.to_string()));
            holes.push(format!("?{}", args.len()));
        }
        sql.push_str(&format!(" AND tracker_id IN ({})", holes.join(",")));
    }
    (sql, args)
}
