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
use crate::calendar::{AccountSyncCursor, Calendar, CalendarOrigin, Event, SyncReport};
use crate::error::{Error, Result};
use crate::id::{AccountId, CalendarId, EventId};
use crate::record::RecordKind;
use crate::store::calendars::{CalendarStore, EventQuery};
use crate::timestamped::Timestamped;

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

    /// Save a calendar's settings -- its name, colour, visibility, role.
    ///
    /// Two fields are kept as stored rather than taken from `calendar`:
    /// [`Calendar::is_default`], which only [`Vault::set_default_calendar`]
    /// moves, and [`Calendar::read_only`], which only the account's server
    /// decides. Both are facts a caller holding a copy from a minute ago
    /// could otherwise put back the way they were -- a sidebar toggling a
    /// calendar's visibility from a list it loaded before the default moved
    /// would leave two calendars each claiming to be it.
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
        let mut calendar = calendar.clone();
        match self.calendar(calendar.id) {
            Ok(stored) => {
                calendar.is_default = stored.is_default;
                calendar.read_only = stored.read_only;
            }
            // A new calendar starts as neither: being made the default is a
            // separate choice, and read-only is the server's to say.
            Err(Error::NotFound { .. }) => calendar.is_default = false,
            Err(e) => return Err(e),
        }
        self.with_calendars(|c| c.put_calendar(&calendar))?;
        self.wrote(RecordKind::Calendar, calendar.id);
        Ok(())
    }

    /// The calendar new events go to when nobody names one, if it is one
    /// they can still go to. `None` means this computer's own time blocks --
    /// both when nothing was chosen and when the chosen calendar has since
    /// turned read-only or gone.
    pub fn default_calendar(&self) -> Result<Option<Calendar>> {
        Ok(self.calendars()?.into_iter().find(|c| c.is_default && c.takes_new_events()))
    }

    /// Make `id` the calendar new events go to, or -- with `None` -- send
    /// them to this computer's own time blocks. Clears the mark from every
    /// other calendar in the same pass, so there is never more than one.
    pub fn set_default_calendar(&self, id: Option<CalendarId>) -> Result<()> {
        self.writable()?;
        let calendars = self.calendars()?;
        if let Some(id) = id {
            let target = calendars
                .iter()
                .find(|c| c.id == id)
                .ok_or_else(|| Error::not_found("calendar", id))?;
            if !target.takes_new_events() {
                return Err(Error::Invalid(format!(
                    "{} cannot take new events, so it cannot be where they go",
                    target.name
                )));
            }
        }
        for mut calendar in calendars {
            let want = Some(calendar.id) == id;
            if calendar.is_default != want {
                calendar.is_default = want;
                calendar.touch();
                self.with_calendars(|c| c.put_calendar(&calendar))?;
                self.wrote(RecordKind::Calendar, calendar.id);
            }
        }
        Ok(())
    }

    /// Record what an account's server said about whether this account may
    /// write to the calendar. A no-op when nothing changed, so a sync that
    /// learns the same answer every hour writes nothing for it.
    pub fn set_calendar_read_only(&self, id: CalendarId, read_only: bool) -> Result<()> {
        self.writable()?;
        let mut calendar = self.calendar(id)?;
        if calendar.read_only == read_only {
            return Ok(());
        }
        calendar.read_only = read_only;
        // A calendar that can no longer take events cannot be where they
        // go; leaving the mark would only make `default_calendar` skip it.
        if read_only {
            calendar.is_default = false;
        }
        calendar.touch();
        self.with_calendars(|c| c.put_calendar(&calendar))?;
        self.wrote(RecordKind::Calendar, id);
        Ok(())
    }

    /// Unsubscribe: the calendar and every event that came from it. Only the
    /// calendar itself is recorded as touched -- the events it cascades away
    /// have no ids in hand here to name individually.
    pub fn delete_calendar(&self, id: CalendarId) -> Result<()> {
        self.writable()?;
        self.with_calendars(|c| c.delete_calendar(id))?;
        self.wrote(RecordKind::Calendar, id);
        Ok(())
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
        for event in &events {
            self.wrote(RecordKind::Event, event.id);
        }

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
        self.wrote(RecordKind::Calendar, id);

        Ok(SyncReport {
            calendar_id: Some(id),
            events: events.len() as u64,
            skipped,
            feed_name: feed.name,
            writable: None,
        })
    }

    /// Record that a sync failed, keeping the events that are already there.
    pub fn mark_calendar_failed(&self, id: CalendarId, why: &str) -> Result<()> {
        self.writable()?;
        let mut calendar = self.calendar(id)?;
        calendar.mark_failed(why);
        self.with_calendars(|c| c.put_calendar(&calendar))?;
        self.wrote(RecordKind::Calendar, id);
        Ok(())
    }

    /// Every calendar reading from `account`, whichever source it uses.
    ///
    /// A handful of rows per account, so a linear scan of the (already small)
    /// calendar list is simpler than a second index for a query nothing else
    /// needs to run often.
    pub fn account_calendars(&self, account: AccountId) -> Result<Vec<Calendar>> {
        Ok(self
            .calendars()?
            .into_iter()
            .filter(|c| c.origin.account_id() == Some(account))
            .collect())
    }

    /// Apply one account calendar's sync: upsert what changed, delete what
    /// vanished, and remember the cursor for next time.
    ///
    /// The counterpart to [`Vault::sync_calendar_from_ics`] for a calendar
    /// read from an account rather than a feed -- `accountcal` has already
    /// done the fetching, the parsing and the diffing by the time this runs,
    /// so this is purely the write: [`crate::store::calendars::CalendarStore::upsert_events`]
    /// touches only the rows named, `mark_synced` clears any previous
    /// failure, and the new cursor is what the next sync reads back to know
    /// what it can skip.
    pub fn sync_account_calendar(
        &self,
        id: CalendarId,
        upsert: &[Event],
        remove: &[EventId],
        cursor: AccountSyncCursor,
    ) -> Result<SyncReport> {
        self.writable()?;
        let mut calendar = self.calendar(id)?;
        if !matches!(calendar.origin, CalendarOrigin::Account { .. }) {
            return Err(Error::Invalid(
                "sync_account_calendar was asked to sync a calendar that is not an account's"
                    .into(),
            ));
        }
        self.with_calendars(|c| c.upsert_events(id, upsert, remove))?;
        for event in upsert {
            self.wrote(RecordKind::Event, event.id);
        }
        calendar.account_sync = cursor;
        calendar.mark_synced();
        self.with_calendars(|c| c.put_calendar(&calendar))?;
        self.wrote(RecordKind::Calendar, id);
        Ok(SyncReport {
            calendar_id: Some(id),
            events: upsert.len() as u64,
            skipped: 0,
            feed_name: None,
            writable: None,
        })
    }
}
