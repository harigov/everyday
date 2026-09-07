//! Storage for the calendar domain: subscriptions and their events.
//!
//! # Why this is a third trait
//!
//! The same reason [`TaskStore`](super::tasks::TaskStore) is a second one. A
//! backend that stores journals as a tree of Markdown files has no opinion
//! about a cache of somebody else's meetings, and folding this into
//! [`JournalStore`](super::JournalStore) would oblige it to grow one. So the
//! calendar domain is reached through
//! [`JournalStore::calendars`](super::JournalStore::calendars), which returns
//! `None` by default, and the interface reads
//! [`Capabilities::calendars`](super::Capabilities::calendars) to know
//! whether to offer the app at all.
//!
//! Note what is *not* here: nothing about time blocks, tasks or projects.
//! The calendar view draws those, but it reads them through
//! [`TaskStore`](super::tasks::TaskStore), which has held them since the todo
//! app shipped. This trait owns exactly the part that is new — the feeds you
//! subscribed to, and the events read out of them.
//!
//! # Replace, never merge
//!
//! [`CalendarStore::replace_events`] is the only way events are written, and
//! it takes every event for one calendar at once. A sync is therefore atomic
//! and total: either the new set of the feed is there or the old one is,
//! never half of each. That is what makes refetching an hourly background
//! operation it is safe to run without asking. Nothing in this trait can
//! touch a record the user made.

use crate::calendar::{Calendar, Event};
use crate::error::Result;
use crate::id::{CalendarId, EventId};
use jiff::civil::Date;
use serde::{Deserialize, Serialize};

/// Filter for [`CalendarStore::list_events`]. This is the query the calendar
/// grid is made of: "every event touching this week, please".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EventQuery {
    /// Inclusive lower bound. An event matches if any day it covers is on or
    /// after this — not merely if it *starts* on or after it, or a fortnight
    /// in Lisbon would vanish from its own second week.
    pub from: Option<Date>,
    /// Inclusive upper bound, on the same "any day it covers" basis.
    pub to: Option<Date>,
    /// Restrict to one calendar. `None` means every subscription.
    pub calendar_id: Option<CalendarId>,
    /// Only calendars the user has left visible. The interface's default:
    /// unticking a calendar should cost nothing to draw.
    #[serde(default)]
    pub visible_only: bool,
    /// Case-insensitive substring of the title, description or location.
    #[serde(default)]
    pub text: String,
    pub limit: Option<u32>,
}

impl EventQuery {
    /// Everything touching the days from `from` to `to` inclusive.
    pub fn between(from: Date, to: Date) -> Self {
        Self { from: Some(from), to: Some(to), ..Default::default() }
    }

    pub fn matches(&self, e: &Event) -> bool {
        if !e.covers(self.from, self.to) {
            return false;
        }
        if let Some(cal) = self.calendar_id
            && e.calendar_id != cal
        {
            return false;
        }
        if !self.text.trim().is_empty()
            && !e.searchable_text().to_lowercase().contains(&self.text.trim().to_lowercase())
        {
            return false;
        }
        true
    }

    /// Filter and order a materialised list: chronological, all-day events
    /// first within a day, which is how every calendar in the world draws
    /// them and therefore where the eye looks for them.
    pub fn apply(&self, mut rows: Vec<Event>) -> Vec<Event> {
        rows.retain(|e| self.matches(e));
        rows.sort_by(|a, b| {
            b.all_day
                .cmp(&a.all_day)
                .then(a.start.cmp(&b.start))
                .then(a.end.cmp(&b.end))
                .then_with(|| a.uid.cmp(&b.uid))
        });
        if let Some(limit) = self.limit {
            rows.truncate(limit as usize);
        }
        rows
    }
}

/// The persistence contract for the calendar domain.
pub trait CalendarStore: Send + Sync {
    // ---- subscriptions --------------------------------------------------

    /// Every calendar, hidden ones included. There are a handful of these,
    /// so filtering is the caller's business.
    fn list_calendars(&self) -> Result<Vec<Calendar>>;

    fn get_calendar(&self, id: CalendarId) -> Result<Calendar>;

    /// Insert or replace. Implementations must be idempotent.
    fn put_calendar(&self, calendar: &Calendar) -> Result<()>;

    /// Delete the subscription and every event that came from it.
    fn delete_calendar(&self, id: CalendarId) -> Result<()>;

    // ---- events ---------------------------------------------------------

    fn list_events(&self, query: &EventQuery) -> Result<Vec<Event>>;

    fn get_event(&self, id: EventId) -> Result<Event>;

    /// Swap one calendar's entire set of events for `events`.
    ///
    /// The only write. See the module docs: a sync that merged would have to
    /// decide what to do about an event the feed no longer mentions, and
    /// every answer to that is worse than starting again from what the
    /// server currently says.
    fn replace_events(&self, calendar: CalendarId, events: &[Event]) -> Result<()>;

    /// How many events are held for one calendar. For the line under its
    /// name in the sidebar; must not decrypt anything to answer.
    fn count_events(&self, calendar: CalendarId) -> Result<u64>;
}

/// Associated data bound into a calendar's ciphertext. See
/// [`entry_aad`](super::entry_aad) for why records are bound to their id.
pub fn calendar_aad(id: CalendarId) -> Vec<u8> {
    format!("everyday.calendar.v1:{id}").into_bytes()
}

pub fn event_aad(id: EventId) -> Vec<u8> {
    format!("everyday.event.v1:{id}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::EventStatus;
    use jiff::Timestamp;
    use jiff::civil::date;

    fn event(cal: CalendarId, from: Date, to: Date, title: &str) -> Event {
        Event {
            id: EventId::new(),
            calendar_id: cal,
            uid: format!("{title}@{from}"),
            title: title.into(),
            description: String::new(),
            location: String::new(),
            start: Timestamp::now(),
            end: Timestamp::now(),
            local_date: from,
            end_date: to,
            tz: "UTC".into(),
            all_day: false,
            status: EventStatus::Confirmed,
            organizer: String::new(),
            url: String::new(),
            busy: true,
            updated_at: Timestamp::now(),
        }
    }

    #[test]
    fn an_empty_query_matches_everything() {
        let cal = CalendarId::new();
        assert!(EventQuery::default().matches(&event(
            cal,
            date(2026, 3, 1),
            date(2026, 3, 1),
            "x"
        )));
    }

    #[test]
    fn a_week_query_finds_an_event_that_started_before_it() {
        // The bug this guards: keying the window on the start day alone, so
        // a fortnight's holiday disappears for its own second week.
        let cal = CalendarId::new();
        let trip = event(cal, date(2026, 7, 10), date(2026, 7, 20), "Lisbon");
        let week = EventQuery::between(date(2026, 7, 13), date(2026, 7, 19));
        assert!(week.matches(&trip));

        let after = EventQuery::between(date(2026, 7, 21), date(2026, 7, 27));
        assert!(!after.matches(&trip));
    }

    #[test]
    fn the_calendar_filter_narrows_to_one_subscription() {
        let mine = CalendarId::new();
        let theirs = CalendarId::new();
        let q = EventQuery { calendar_id: Some(mine), ..Default::default() };
        assert!(q.matches(&event(mine, date(2026, 3, 1), date(2026, 3, 1), "a")));
        assert!(!q.matches(&event(theirs, date(2026, 3, 1), date(2026, 3, 1), "b")));
    }

    #[test]
    fn text_search_covers_the_location_as_well_as_the_title() {
        let cal = CalendarId::new();
        let mut e = event(cal, date(2026, 3, 1), date(2026, 3, 1), "Review");
        e.location = "Meeting room 4".into();
        let q = |s: &str| EventQuery { text: s.into(), ..Default::default() };
        assert!(q("REVIEW").matches(&e), "matching should ignore case");
        assert!(q("room 4").matches(&e));
        assert!(!q("canteen").matches(&e));
    }

    #[test]
    fn all_day_events_sort_above_the_timed_ones_that_share_their_day() {
        let cal = CalendarId::new();
        let mut holiday = event(cal, date(2026, 3, 1), date(2026, 3, 1), "Holiday");
        holiday.all_day = true;
        holiday.start = Timestamp::from_second(1_800_000_000).unwrap();
        let mut meeting = event(cal, date(2026, 3, 1), date(2026, 3, 1), "Meeting");
        meeting.start = holiday.start - jiff::SignedDuration::from_hours(2);

        let out = EventQuery::default().apply(vec![meeting, holiday]);
        assert_eq!(out[0].title, "Holiday", "the banner belongs above the grid");
    }

    #[test]
    fn aad_is_distinct_per_record_and_per_kind() {
        let same = uuid::Uuid::now_v7();
        assert_ne!(calendar_aad(CalendarId(same)), event_aad(EventId(same)));
        // And distinct from the other two domains, which share the id space.
        assert_ne!(event_aad(EventId(same)), super::super::tasks::block_aad(crate::BlockId(same)),);
        assert_ne!(calendar_aad(CalendarId(same)), super::super::entry_aad(crate::EntryId(same)));
    }
}
