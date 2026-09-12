//! Subscribed calendars, and the events read out of their feeds.
//!
//! The third domain, on exactly the terms of the second: everything goes
//! through [`Vault::with_calendars`], which fails with `unsupported` on a
//! backend that holds journals only.
//!
//! Note the shape of the sync entry point. This layer takes iCalendar
//! *text*, never a URL: fetching is the shell's job, because the core has
//! no async runtime, no TLS stack and -- deliberately -- no ability to
//! open a socket at all. What the core owns is everything that happens to
//! those bytes afterwards, which is the part worth testing.

use super::Vault;
use super::session::Domain;
use crate::calendar::{Calendar, Event, SyncReport};
use crate::error::{Error, Result};
use crate::id::{CalendarId, EventId};
use crate::store::calendars::{CalendarStore, EventQuery};

impl Vault {
    /// Does this vault's backend store subscribed calendars?
    pub fn supports_calendars(&self) -> bool {
        self.with_calendars(|_| Ok(())).is_ok()
    }

    fn with_calendars<T>(&self, f: impl FnOnce(&dyn CalendarStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Calendars, |s| s.calendars().map(f))
    }

    pub fn calendars(&self) -> Result<Vec<Calendar>> {
        self.with_calendars(|c| c.list_calendars())
    }

    pub fn calendar(&self, id: CalendarId) -> Result<Calendar> {
        self.with_calendars(|c| c.get_calendar(id))
    }

    pub fn save_calendar(&self, calendar: &Calendar) -> Result<()> {
        self.writable()?;
        if calendar.name.trim().is_empty() {
            return Err(Error::Invalid("a calendar needs a name".into()));
        }
        // Validate the address on the way in rather than at fetch time, so a
        // `file://` URL is refused where it was typed instead of quietly
        // stored and refused an hour later by a background sync nobody is
        // watching.
        if calendar.origin.url().is_some() {
            calendar.fetch_url()?;
        }
        self.with_calendars(|c| c.put_calendar(calendar))
    }

    /// Unsubscribe: the calendar and every event that came from it.
    pub fn delete_calendar(&self, id: CalendarId) -> Result<()> {
        self.writable()?;
        self.with_calendars(|c| c.delete_calendar(id))
    }

    pub fn events(&self, query: &EventQuery) -> Result<Vec<Event>> {
        self.with_calendars(|c| c.list_events(query))
    }

    pub fn event(&self, id: EventId) -> Result<Event> {
        self.with_calendars(|c| c.get_event(id))
    }

    /// How many events are held for one calendar.
    pub fn event_count(&self, id: CalendarId) -> Result<u64> {
        self.with_calendars(|c| c.count_events(id))
    }

    /// Parse `ics` and make it the whole of what `id` holds.
    ///
    /// The window is the days worth materialising: recurring events are
    /// expanded into it and no further, which is what keeps a decade-old
    /// daily stand-up from becoming four thousand rows. `default_tz` is the
    /// zone a floating time is read in — the reader's own.
    pub fn sync_calendar_from_ics(
        &self,
        id: CalendarId,
        ics: &str,
        window: (jiff::civil::Date, jiff::civil::Date),
        default_tz: &str,
    ) -> Result<SyncReport> {
        self.writable()?;
        // Refuse anything that is not an iCalendar document *before* it can
        // replace one, and before the store is touched at all. A captive
        // portal's login page, an expired link's HTML error, a truncated
        // download: all of them parse to zero events, and all of them would
        // otherwise empty a working calendar. A genuine VCALENDAR with no
        // VEVENTs in it is a different thing -- that is a real answer,
        // meaning "nothing on here" -- and it is written.
        if !ics.to_ascii_uppercase().contains("BEGIN:VCALENDAR") {
            return Err(Error::Invalid(
                "that address did not return a calendar; the events already here have been kept"
                    .into(),
            ));
        }
        let mut calendar = self.calendar(id)?;
        let feed = crate::ics::parse(ics);
        let (events, skipped) = crate::ics::events_for(&calendar, &feed, window, default_tz);

        self.with_calendars(|c| c.replace_events(id, &events))?;

        calendar.mark_synced();
        // A calendar that never had a name of its own takes the publisher's,
        // which spares the subscriber naming something they did not create.
        if let Some(name) = feed.name.as_deref()
            && !name.is_empty()
            && calendar.name.trim().is_empty()
        {
            calendar.name = name.to_string();
        }
        self.with_calendars(|c| c.put_calendar(&calendar))?;

        Ok(SyncReport {
            calendar_id: Some(id),
            events: events.len() as u64,
            skipped,
            feed_name: feed.name,
        })
    }

    /// Record that a sync failed, keeping the events that are already there.
    pub fn mark_calendar_failed(&self, id: CalendarId, why: &str) -> Result<()> {
        self.writable()?;
        let mut calendar = self.calendar(id)?;
        calendar.mark_failed(why);
        self.with_calendars(|c| c.put_calendar(&calendar))
    }
}
