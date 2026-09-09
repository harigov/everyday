//! The calendar domain: subscriptions and the events read from them.
//!
//! This domain goes further than the others in what it seals: a feed's
//! *address* is a bearer credential -- anyone holding one can read that
//! calendar until it is revoked -- so it never sits in a clear column, and
//! neither does the name of the calendar it points at. What stays clear is
//! only what an index needs: which calendar, which days, and when.

use everyday_core::calendar::{Calendar, Event};
use everyday_core::error::{Error, Result};
use everyday_core::id::{CalendarId, EventId};
use everyday_core::store::calendars::{CalendarStore, EventQuery, calendar_aad, event_aad};

use crate::conn::{SqlExt, Value};
use crate::purpose::{RecordKind, forget_purposes, set_purpose};
use crate::{SqlStore, to_us, vals};

impl CalendarStore for SqlStore {
    // ---- subscriptions --------------------------------------------------

    fn list_calendars(&self) -> Result<Vec<Calendar>> {
        let rows =
            self.read().records("SELECT id, data FROM calendars ORDER BY created_us", &[])?;
        self.collect(rows, calendar_aad)
    }

    fn get_calendar(&self, id: CalendarId) -> Result<Calendar> {
        let sealed = self
            .read()
            .sealed("SELECT data FROM calendars WHERE id = ?1", &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found("calendar", id))?;
        self.unseal(&calendar_aad(id), &sealed)
    }

    fn put_calendar(&self, c: &Calendar) -> Result<()> {
        // Note what is *not* in the clear columns: the name, and above all
        // the URL. A feed address is a bearer credential.
        let data = self.seal(&calendar_aad(c.id), c)?;
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        tx.execute(
            "INSERT INTO calendars (id, visible, created_us, updated_us, synced_us, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (id) DO UPDATE SET
                visible = ?2, created_us = ?3, updated_us = ?4, synced_us = ?5, data = ?6",
            &vals![
                c.id.to_string(),
                c.visible,
                to_us(c.created_at),
                to_us(c.updated_at),
                c.last_synced_at.map(to_us),
                data,
            ],
        )?;
        // A feed's role goes in the same pointer table as everything else,
        // always `role`-kinded: a calendar serves a role, and its forty
        // meetings are not each yours to file.
        let purpose = c.role_id.map(|id| everyday_core::purpose::Purpose::Role { id });
        set_purpose(tx.as_mut(), RecordKind::Calendar, &c.id.to_string(), purpose.as_ref())?;
        tx.commit()
    }

    fn delete_calendar(&self, id: CalendarId) -> Result<()> {
        // The events go with it by foreign key -- but only if the database is
        // enforcing them, which on SQLite is a connection pragma a future
        // refactor could quietly turn off. Deleting them explicitly costs one
        // indexed statement and does not depend on a setting staying put.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        tx.execute("DELETE FROM events WHERE calendar_id = ?1", &vals![id.to_string()])?;
        tx.execute("DELETE FROM calendars WHERE id = ?1", &vals![id.to_string()])?;
        forget_purposes(tx.as_mut(), RecordKind::Calendar, &[id.to_string()])?;
        tx.commit()
    }

    // ---- events ---------------------------------------------------------

    fn list_events(&self, query: &EventQuery) -> Result<Vec<Event>> {
        // Only the text filter needs the payload -- titles and locations are
        // sealed -- so it is the one thing that cannot be pushed into SQL.
        // Everything else is a clear column, and the window is an overlap
        // test on the two date columns rather than a bound on the start.
        let mut sql = String::from("SELECT id, data FROM events WHERE 1=1");
        let mut args: Vec<Value> = Vec::new();

        if let Some(from) = query.from {
            args.push(Value::Text(from.to_string()));
            sql.push_str(&format!(" AND end_date >= ?{}", args.len()));
        }
        if let Some(to) = query.to {
            args.push(Value::Text(to.to_string()));
            sql.push_str(&format!(" AND local_date <= ?{}", args.len()));
        }
        if let Some(cal) = query.calendar_id {
            args.push(Value::Text(cal.to_string()));
            sql.push_str(&format!(" AND calendar_id = ?{}", args.len()));
        }
        if query.visible_only {
            // `WHERE visible` rather than `WHERE visible = 1`: Postgres will
            // not compare a boolean to an integer, and both understand this.
            sql.push_str(" AND calendar_id IN (SELECT id FROM calendars WHERE visible)");
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

        let rows = self.read().records(&sql, &args)?;
        let out: Vec<Event> = self.collect(rows, event_aad)?;
        Ok(if in_memory_pass { query.apply(out) } else { out })
    }

    fn get_event(&self, id: EventId) -> Result<Event> {
        let sealed = self
            .read()
            .sealed("SELECT data FROM events WHERE id = ?1", &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found("event", id))?;
        self.unseal(&event_aad(id), &sealed)
    }

    fn replace_events(&self, calendar: CalendarId, events: &[Event]) -> Result<()> {
        // Sealing happens before the lock is taken: a feed can be thousands
        // of occurrences, and holding the connection across that many AEAD
        // seals would stall every other query for the duration of a sync
        // that is meant to be invisible.
        // An event claiming to belong to another calendar is filed where it
        // was asked to go -- otherwise it would survive this sync and be
        // deleted by that other calendar's next one. The correction is made
        // *before* sealing, so the payload and the clear column agree; doing
        // it only in the column left `get_event` returning an event whose own
        // `calendar_id` named a feed it was not stored under.
        let sealed: Vec<(String, Vec<u8>)> = events
            .iter()
            .map(|e| {
                let data = if e.calendar_id == calendar {
                    self.seal(&event_aad(e.id), e)?
                } else {
                    let mut filed = e.clone();
                    filed.calendar_id = calendar;
                    self.seal(&event_aad(e.id), &filed)?
                };
                Ok((e.id.to_string(), data))
            })
            .collect::<Result<_>>()?;

        let mut conn = self.write();
        let mut tx = conn.begin()?;
        tx.execute("DELETE FROM events WHERE calendar_id = ?1", &vals![calendar.to_string()])?;
        for (event, (id, data)) in events.iter().zip(&sealed) {
            // An upsert, because a feed is not obliged to be well formed.
            // Two occurrences deriving the same id -- a publisher repeating a
            // UID, a recurrence rule that lands twice on one instant -- broke
            // the primary key and aborted the whole transaction, so that feed
            // could never sync again. Last one wins is the right answer for a
            // cache of what a server said.
            tx.execute(
                "INSERT INTO events
                    (id, calendar_id, local_date, end_date, start_us, end_us, all_day, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (id) DO UPDATE SET
                    calendar_id = ?2, local_date = ?3, end_date = ?4, start_us = ?5,
                    end_us = ?6, all_day = ?7, data = ?8",
                &vals![
                    id,
                    calendar.to_string(),
                    event.local_date.to_string(),
                    event.end_date.to_string(),
                    to_us(event.start),
                    to_us(event.end),
                    event.all_day,
                    data,
                ],
            )?;
        }
        tx.commit()
    }

    fn count_events(&self, calendar: CalendarId) -> Result<u64> {
        let n = self.read().scalar_i64(
            "SELECT COUNT(*) FROM events WHERE calendar_id = ?1",
            &vals![calendar.to_string()],
        )?;
        Ok(n.max(0) as u64)
    }
}
