//! The tracking domain: the readings a journal's trackers produced.
//!
//! Optional, like the two domains before it -- a backend returning `None`
//! from `JournalStore::trackers` holds journals and nothing else, which is
//! what the Markdown backend does.
//!
//! The clear/sealed split this crate makes everywhere goes furthest here,
//! and on purpose. `tracker_id`, `local_date`, `at_us` and `value` are all
//! in the clear, which means every question this domain exists to answer --
//! a monthly average, a streak, minutes per week -- is a `GROUP BY` over an
//! index rather than a decryption of the vault. What stays sealed is the
//! only part that identifies anything: the tracker's *name*, which is not
//! in this table at all but in the journal record, and the note attached to
//! a reading. The file says tracker `7f3a...` was `500` at 08:12 on the 14th
//! and never what `7f3a...` is.

use everyday_core::error::{Error, Result};
use everyday_core::id::{JournalId, ReadingId, TrackerId};
use everyday_core::store::trackers::{ReadingQuery, TrackerDay, TrackerStore, reading_aad};
use everyday_core::tracker::Reading;
use jiff::civil::Date;
use rusqlite::{OptionalExtension, params, params_from_iter};

use crate::{SqliteStore, id_str, to_us};

impl TrackerStore for SqliteStore {
    fn list_readings(&self, query: &ReadingQuery) -> Result<Vec<Reading>> {
        let (where_sql, args) = where_clause(query);
        let mut sql = format!("SELECT id, data FROM readings WHERE {where_sql}");
        // Untimed readings first within a day: `at_us IS NULL` sorts before
        // any instant, which matches `ReadingQuery::apply` so the two
        // orderings cannot drift.
        sql.push_str(" ORDER BY local_date ASC, at_us ASC, id ASC");
        if let Some(limit) = query.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        let conn = self.conn();
        let mut stmt = conn.prepare(&sql).map_err(Error::backend)?;
        let rows: Vec<(String, Vec<u8>)> = stmt
            .query_map(params_from_iter(args.iter().map(|a| a.as_ref())), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(Error::backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(Error::backend)?;
        drop(stmt);
        drop(conn);
        self.collect(rows, reading_aad)
    }

    fn get_reading(&self, id: ReadingId) -> Result<Reading> {
        let conn = self.conn();
        let sealed: Option<Vec<u8>> = conn
            .query_row("SELECT data FROM readings WHERE id = ?1", params![id.to_string()], |r| {
                r.get(0)
            })
            .optional()
            .map_err(Error::backend)?;
        drop(conn);
        let sealed = sealed.ok_or_else(|| Error::not_found("reading", id))?;
        self.unseal(&reading_aad(id), &sealed)
    }

    fn put_reading(&self, r: &Reading) -> Result<()> {
        let data = self.seal(&reading_aad(r.id), r)?;
        let conn = self.conn();
        conn.execute(
            "INSERT INTO readings
                (id, journal_id, tracker_id, entry_id, local_date, at_us, value,
                 created_us, updated_us, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                journal_id = ?2, tracker_id = ?3, entry_id = ?4, local_date = ?5,
                at_us = ?6, value = ?7, created_us = ?8, updated_us = ?9, data = ?10",
            params![
                r.id.to_string(),
                r.journal_id.to_string(),
                r.tracker_id.to_string(),
                id_str(r.entry_id),
                r.local_date.to_string(),
                r.at.map(to_us),
                r.value,
                to_us(r.created_at),
                to_us(r.updated_at),
                data,
            ],
        )
        .map_err(Error::backend)?;
        Ok(())
    }

    fn delete_reading(&self, id: ReadingId) -> Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM readings WHERE id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        Ok(())
    }

    fn delete_readings_of(&self, tracker: TrackerId) -> Result<u64> {
        let conn = self.conn();
        let n = conn
            .execute("DELETE FROM readings WHERE tracker_id = ?1", params![tracker.to_string()])
            .map_err(Error::backend)?;
        Ok(n as u64)
    }

    fn delete_readings_in(&self, journal: JournalId) -> Result<u64> {
        let conn = self.conn();
        let n = conn
            .execute("DELETE FROM readings WHERE journal_id = ?1", params![journal.to_string()])
            .map_err(Error::backend)?;
        Ok(n as u64)
    }

    /// The aggregate, done where the data is.
    ///
    /// Nothing is decrypted here: every column this reads is a clear one, so
    /// a year of one tracker costs an index range scan and returns 365 rows
    /// instead of 3,000 sealed payloads for the caller to open and add up.
    /// That difference is the reason `value` is not sealed.
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

        let conn = self.conn();
        let mut stmt = conn.prepare(&sql).map_err(Error::backend)?;
        let rows = stmt
            .query_map(params_from_iter(args.iter().map(|a| a.as_ref())), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<f64>>(3)?,
                    r.get::<_, Option<f64>>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                ))
            })
            .map_err(Error::backend)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Error::backend)?;
        drop(stmt);
        drop(conn);

        rows.into_iter()
            .map(|(tracker, date, count, sum, max, first, last)| {
                Ok(TrackerDay {
                    tracker_id: TrackerId::parse(&tracker)
                        .map_err(|e| Error::Invalid(e.to_string()))?,
                    date: date.parse::<Date>().map_err(|e| Error::Invalid(e.to_string()))?,
                    count: u32::try_from(count).unwrap_or(u32::MAX),
                    sum: sum.unwrap_or(0.0),
                    max: max.unwrap_or(0.0),
                    first_at: first.map(from_us),
                    last_at: last.map(from_us),
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
fn where_clause(query: &ReadingQuery) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut sql = String::from("1=1");
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(j) = query.journal_id {
        args.push(Box::new(j.to_string()));
        sql.push_str(&format!(" AND journal_id = ?{}", args.len()));
    }
    if let Some(e) = query.entry_id {
        args.push(Box::new(e.to_string()));
        sql.push_str(&format!(" AND entry_id = ?{}", args.len()));
    }
    if let Some(from) = query.from {
        args.push(Box::new(from.to_string()));
        sql.push_str(&format!(" AND local_date >= ?{}", args.len()));
    }
    if let Some(to) = query.to {
        args.push(Box::new(to.to_string()));
        sql.push_str(&format!(" AND local_date <= ?{}", args.len()));
    }
    if query.timed_only {
        sql.push_str(" AND at_us IS NOT NULL");
    }
    if !query.tracker_ids.is_empty() {
        let mut holes = Vec::new();
        for id in &query.tracker_ids {
            args.push(Box::new(id.to_string()));
            holes.push(format!("?{}", args.len()));
        }
        sql.push_str(&format!(" AND tracker_id IN ({})", holes.join(",")));
    }
    (sql, args)
}

fn from_us(us: i64) -> jiff::Timestamp {
    jiff::Timestamp::from_microsecond(us).unwrap_or(jiff::Timestamp::UNIX_EPOCH)
}
