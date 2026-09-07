//! The calendar domain: subscriptions and the events read from them.
//!
//! Optional on the same terms as the task domain. This one goes further than
//! the others in what it seals: a feed's *address* is a bearer credential --
//! anyone holding one can read that calendar until it is revoked -- so it
//! never sits in a clear column, and neither does the name of the calendar it
//! points at. What stays clear is only what an index needs: which calendar,
//! which days, and when.

use everyday_core::calendar::{Calendar, Event};
use everyday_core::error::{Error, Result};
use everyday_core::id::{CalendarId, EventId};
use everyday_core::store::calendars::{CalendarStore, EventQuery, calendar_aad, event_aad};
use rusqlite::{OptionalExtension, params, params_from_iter};

use crate::{SqliteStore, to_us};

impl CalendarStore for SqliteStore {
    // ---- subscriptions --------------------------------------------------

    fn list_calendars(&self) -> Result<Vec<Calendar>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, data FROM calendars ORDER BY created_us")
            .map_err(Error::backend)?;
        let rows: Vec<(String, Vec<u8>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(Error::backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(Error::backend)?;
        drop(stmt);
        drop(conn);
        self.collect(rows, calendar_aad)
    }

    fn get_calendar(&self, id: CalendarId) -> Result<Calendar> {
        let conn = self.conn.lock().unwrap();
        let sealed: Option<Vec<u8>> = conn
            .query_row("SELECT data FROM calendars WHERE id = ?1", params![id.to_string()], |r| {
                r.get(0)
            })
            .optional()
            .map_err(Error::backend)?;
        drop(conn);
        let sealed = sealed.ok_or_else(|| Error::not_found("calendar", id))?;
        self.unseal(&calendar_aad(id), &sealed)
    }

    fn put_calendar(&self, c: &Calendar) -> Result<()> {
        // Note what is *not* in the clear columns: the name, and above all
        // the URL. A feed address is a bearer credential.
        let data = self.seal(&calendar_aad(c.id), c)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO calendars (id, visible, created_us, updated_us, synced_us, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                visible = ?2, created_us = ?3, updated_us = ?4, synced_us = ?5, data = ?6",
            params![
                c.id.to_string(),
                c.visible,
                to_us(c.created_at),
                to_us(c.updated_at),
                c.last_synced_at.map(to_us),
                data,
            ],
        )
        .map_err(Error::backend)?;
        Ok(())
    }

    fn delete_calendar(&self, id: CalendarId) -> Result<()> {
        // The events go with it by foreign key -- but only if the pragma is
        // on, which `open` sets and which a future refactor could quietly
        // turn off. Deleting them explicitly costs one indexed statement and
        // does not depend on a connection setting staying put.
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(Error::backend)?;
        tx.execute("DELETE FROM events WHERE calendar_id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        tx.execute("DELETE FROM calendars WHERE id = ?1", params![id.to_string()])
            .map_err(Error::backend)?;
        tx.commit().map_err(Error::backend)?;
        Ok(())
    }

    // ---- events ---------------------------------------------------------

    fn list_events(&self, query: &EventQuery) -> Result<Vec<Event>> {
        // Only the text filter needs the payload -- titles and locations are
        // sealed -- so it is the one thing that cannot be pushed into SQL.
        // Everything else is a clear column, and the window is an overlap
        // test on the two date columns rather than a bound on the start.
        let mut sql = String::from("SELECT id, data FROM events WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(from) = query.from {
            args.push(Box::new(from.to_string()));
            sql.push_str(&format!(" AND end_date >= ?{}", args.len()));
        }
        if let Some(to) = query.to {
            args.push(Box::new(to.to_string()));
            sql.push_str(&format!(" AND local_date <= ?{}", args.len()));
        }
        if let Some(cal) = query.calendar_id {
            args.push(Box::new(cal.to_string()));
            sql.push_str(&format!(" AND calendar_id = ?{}", args.len()));
        }
        if query.visible_only {
            sql.push_str(" AND calendar_id IN (SELECT id FROM calendars WHERE visible = 1)");
        }
        // All-day first within a day, then chronological: what every
        // calendar draws, and therefore where the eye looks for them.
        sql.push_str(" ORDER BY all_day DESC, start_us ASC, end_us ASC");
        // A text filter cuts rows after the fact, so the limit cannot be
        // pushed down with it -- it would cap the wrong set.
        let in_memory_pass = !query.text.trim().is_empty();
        if let Some(limit) = query.limit
            && !in_memory_pass
        {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        let conn = self.conn.lock().unwrap();
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

        let out: Vec<Event> = self.collect(rows, event_aad)?;
        Ok(if in_memory_pass { query.apply(out) } else { out })
    }

    fn get_event(&self, id: EventId) -> Result<Event> {
        let conn = self.conn.lock().unwrap();
        let sealed: Option<Vec<u8>> = conn
            .query_row("SELECT data FROM events WHERE id = ?1", params![id.to_string()], |r| {
                r.get(0)
            })
            .optional()
            .map_err(Error::backend)?;
        drop(conn);
        let sealed = sealed.ok_or_else(|| Error::not_found("event", id))?;
        self.unseal(&event_aad(id), &sealed)
    }

    fn replace_events(&self, calendar: CalendarId, events: &[Event]) -> Result<()> {
        // Sealing happens before the lock is taken: a feed can be thousands
        // of occurrences, and holding the connection across that many AEAD
        // seals would stall every other query for the duration of a sync
        // that is meant to be invisible.
        let sealed: Vec<(String, Vec<u8>)> = events
            .iter()
            .map(|e| Ok((e.id.to_string(), self.seal(&event_aad(e.id), e)?)))
            .collect::<Result<_>>()?;

        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(Error::backend)?;
        tx.execute("DELETE FROM events WHERE calendar_id = ?1", params![calendar.to_string()])
            .map_err(Error::backend)?;
        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT INTO events
                        (id, calendar_id, local_date, end_date, start_us, end_us, all_day, data)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )
                .map_err(Error::backend)?;
            for (event, (id, data)) in events.iter().zip(&sealed) {
                // An event claiming to belong to another calendar would
                // survive this sync and be deleted by that calendar's next
                // one. File it where it was asked to go.
                stmt.execute(params![
                    id,
                    calendar.to_string(),
                    event.local_date.to_string(),
                    event.end_date.to_string(),
                    to_us(event.start),
                    to_us(event.end),
                    event.all_day,
                    data,
                ])
                .map_err(Error::backend)?;
            }
        }
        tx.commit().map_err(Error::backend)?;
        Ok(())
    }

    fn count_events(&self, calendar: CalendarId) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE calendar_id = ?1",
                params![calendar.to_string()],
                |r| r.get(0),
            )
            .map_err(Error::backend)?;
        Ok(n as u64)
    }
}
