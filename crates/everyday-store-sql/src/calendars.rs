//! The calendar domain: subscriptions and the events read from them.
//!
//! This domain goes further than the others in what it seals: a feed's
//! *address* is a bearer credential -- anyone holding one can read that
//! calendar until it is revoked -- so it never sits in a clear column, and
//! neither does the name of the calendar it points at. What stays clear is
//! only what an index needs: which calendar, which days, and when.

use everyday_core::calendar::{Calendar, Event};
use everyday_core::error::Result;
use everyday_core::id::{CalendarId, EventId};
use everyday_core::store::calendars::{CalendarStore, EventQuery, calendar_aad, event_aad};

use crate::conn::{SqlExt, ToValue, Value, Where};
use crate::purpose::{RecordKind, forget_purposes, set_purpose};
use crate::record::{Record, upsert_stmt};
use crate::{SqlStore, to_us, vals};

impl Record for Calendar {
    const TABLE: &'static str = "calendars";
    const KIND: &'static str = "calendar";
    type Id = CalendarId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        calendar_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("visible", self.visible.to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
            ("synced_us", self.last_synced_at.map(to_us).to_value()),
        ]
    }

    // A calendar's purpose is derived from `role_id` rather than carried as
    // a `Purpose` field of its own, so it cannot be handed back as a
    // borrowed `&Purpose` the way every other record's can. `put_calendar`
    // computes it and calls `set_purpose` itself instead of going through
    // `SqlStore::upsert_many`; see there.
}

impl Record for Event {
    const TABLE: &'static str = "events";
    const KIND: &'static str = "event";
    type Id = EventId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        event_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("calendar_id", self.calendar_id.to_string().to_value()),
            ("local_date", self.local_date.to_string().to_value()),
            ("end_date", self.end_date.to_string().to_value()),
            ("start_us", to_us(self.start).to_value()),
            ("end_us", to_us(self.end).to_value()),
            ("all_day", self.all_day.to_value()),
        ]
    }
}

impl CalendarStore for SqlStore {
    // ---- subscriptions --------------------------------------------------

    fn list_calendars(&self) -> Result<Vec<Calendar>> {
        let rows =
            self.read().records("SELECT id, data FROM calendars ORDER BY created_us", &[])?;
        self.collect(rows, calendar_aad)
    }

    fn get_calendar(&self, id: CalendarId) -> Result<Calendar> {
        self.get(id)
    }

    fn put_calendar(&self, c: &Calendar) -> Result<()> {
        // Note what is *not* in the clear columns: the name, and above all
        // the URL. A feed address is a bearer credential.
        let data = self.seal(&calendar_aad(c.id), c)?;
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let (sql, args) = upsert_stmt(c, data);
        tx.execute(&sql, &args)?;
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
        let mut w = Where::new();
        if let Some(from) = query.from {
            w = w.gte("end_date", from.to_string());
        }
        if let Some(to) = query.to {
            w = w.lte("local_date", to.to_string());
        }
        if let Some(cal) = query.calendar_id {
            w = w.eq("calendar_id", cal.to_string());
        }
        let (where_sql, args) = w.finish();
        let mut sql = format!("SELECT id, data FROM events WHERE {where_sql}");
        if query.visible_only {
            // `WHERE visible` rather than `WHERE visible = 1`: Postgres will
            // not compare a boolean to an integer, and both understand this.
            // A fixed, argument-free subquery, so it is appended directly
            // rather than through `Where`, which exists for conditions that
            // carry their own placeholder.
            sql.push_str(" AND calendar_id IN (SELECT id FROM calendars WHERE visible)");
        }
        // All-day first within a day, then chronological: what every
        // calendar draws, and therefore where the eye looks for them.
        //
        // A text filter cuts rows after the fact, so the limit cannot be
        // pushed down with it -- it would cap the wrong set.
        let in_memory_pass = !query.text.trim().is_empty();
        if in_memory_pass {
            sql.push_str(" ORDER BY all_day DESC, start_us ASC, end_us ASC");
        } else {
            self.page(&mut sql, "all_day DESC, start_us ASC, end_us ASC", query.limit, 0);
        }

        let rows = self.read().records(&sql, &args)?;
        let out: Vec<Event> = self.collect(rows, event_aad)?;
        Ok(if in_memory_pass { query.apply(out) } else { out })
    }

    fn get_event(&self, id: EventId) -> Result<Event> {
        self.get(id)
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
        let filed: Vec<(Event, Vec<u8>)> = events
            .iter()
            .map(|e| {
                if e.calendar_id == calendar {
                    let data = self.seal(&event_aad(e.id), e)?;
                    Ok((e.clone(), data))
                } else {
                    let mut filed = e.clone();
                    filed.calendar_id = calendar;
                    let data = self.seal(&event_aad(filed.id), &filed)?;
                    Ok((filed, data))
                }
            })
            .collect::<Result<_>>()?;

        let mut conn = self.write();
        let mut tx = conn.begin()?;
        tx.execute("DELETE FROM events WHERE calendar_id = ?1", &vals![calendar.to_string()])?;
        for (event, data) in filed {
            // An upsert, because a feed is not obliged to be well formed.
            // Two occurrences deriving the same id -- a publisher repeating a
            // UID, a recurrence rule that lands twice on one instant -- broke
            // the primary key and aborted the whole transaction, so that feed
            // could never sync again. Last one wins is the right answer for a
            // cache of what a server said. Built with `upsert_stmt` rather
            // than `SqlStore::upsert`, which owns its own transaction, so the
            // whole sync stays the one transaction the delete opened.
            let (sql, args) = upsert_stmt(&event, data);
            tx.execute(&sql, &args)?;
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
