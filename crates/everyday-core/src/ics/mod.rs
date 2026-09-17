//! An iCalendar ([RFC 5545](https://www.rfc-editor.org/rfc/rfc5545)) reader.
//!
//! Enough of the format to render somebody's real calendar, and no more.
//! This is the one piece of the calendar domain that is genuinely difficult,
//! so it is worth saying what it does and — more usefully — what it does not.
//!
//! # What it reads
//!
//! `VEVENT`s: summary, description, location, organiser, URL, status, and
//! the four shapes a start or end can take — an all-day `VALUE=DATE`, a
//! UTC `...Z` instant, a zoned `TZID=Europe/Berlin` local time, and a
//! floating local time with no zone at all. `DURATION` where `DTEND` is
//! absent. Recurrence: `RRULE` (`FREQ`, `INTERVAL`, `COUNT`, `UNTIL`,
//! `BYDAY` including ordinals like `-1FR`, `BYMONTHDAY`, `BYMONTH`),
//! `RDATE`, `EXDATE`, and `RECURRENCE-ID` overrides — the mechanism behind
//! "the Tuesday stand-up, except that one week it moved to Wednesday".
//!
//! # What it does not
//!
//! `VTODO` and `VJOURNAL` are skipped: this application has its own, better
//! answers for both, and importing a half-expressive copy of them would be a
//! worse todo list than the one next door. `VALARM` is skipped because the
//! app does not notify. `VFREEBUSY` is skipped. `BYSETPOS`, `BYWEEKNO` and
//! `BYYEARDAY` are not expanded; a rule using one produces its `DTSTART`
//! occurrence and no more, which is visibly wrong in a way that a silently
//! dropped event is not.
//!
//! `VTIMEZONE` blocks are parsed for their `TZID` but their offset rules are
//! *not* used. The zone is resolved through the system tz database instead,
//! which is both more correct — it knows about the change to the rules that
//! the feed was generated before — and how every other date in this
//! application is handled. Feeds that quote a Windows zone name go through
//! [`WINDOWS_ZONES`]; anything still unrecognised falls back to the
//! calendar's own default zone and then to the reader's.
//!
//! # Why occurrences are expanded here rather than at draw time
//!
//! A recurrence engine on the draw path is a recurrence engine that runs on
//! every scroll. Expanding once, at sync time, into rows the storage layer
//! can index by date makes the calendar grid a range scan — the same shape
//! as every other query in this application — at the cost of some rows.
//! [`EXPANSION_CAP`] bounds the damage a hostile or broken feed can do.
use crate::calendar::EventStatus;
use jiff::SignedDuration;
use jiff::civil::{DateTime, Weekday};

mod read;
mod rrule;
mod tz;
mod write;

pub use read::{events_for, parse};
pub use rrule::parse_rrule;
pub use tz::{WINDOWS_ZONES, resolve_tzid};
pub use write::Ics;

/// Most occurrences one recurrence rule may produce.
///
/// A daily event over the default window is about 1,100 rows; this is a
/// ceiling on the pathological case — `FREQ=SECONDLY`, or a rule whose
/// `UNTIL` is in the year 4000 — not a limit anyone should reach.
pub const EXPANSION_CAP: usize = 1_500;
/// How many attendees are kept from one event.
///
/// A meeting with more people than this in it is a broadcast, and the names
/// are of no use to anybody preparing for it -- while the text of them all
/// would go into a prompt and be paid for. Generous enough for every meeting
/// anybody actually prepares for.
pub const MAX_ATTENDEES: usize = 25;
/// One `VEVENT`, before recurrence is expanded.
#[derive(Debug, Clone, PartialEq)]
pub struct IcsEvent {
    pub uid: String,
    pub summary: String,
    pub description: String,
    pub location: String,
    pub organizer: String,
    /// Everybody else invited, as the feed gives them: a name where there is
    /// one, an address where there is not.
    pub attendees: Vec<String>,
    pub url: String,
    pub status: EventStatus,
    pub all_day: bool,
    /// Local wall-clock start, in `tz`.
    pub start: DateTime,
    pub end: DateTime,
    /// The zone `start` and `end` are quoted in, and therefore the zone they
    /// must be read in to arrive at an instant. `"UTC"` for a `...Z` stamp,
    /// the `TZID` parameter's zone where there is one, and empty for a
    /// floating time, which genuinely means "9am wherever you are".
    pub tz: String,
    /// The zone the record was *kept* in, from `X-EVERYDAY-TZ`, when this
    /// feed is one of ours.
    ///
    /// Deliberately not folded into `tz`: our own writer always quotes
    /// `DTSTART` as a UTC stamp and puts the record's zone here, so letting
    /// this one override `tz` would re-read an absolute instant as a
    /// wall-clock time somewhere else and move every timed event by that
    /// zone's offset -- once per export-and-import, compounding. It says how
    /// to *show* the event, never how to read it.
    pub display_tz: Option<String>,
    pub busy: bool,
    pub rrule: Option<Rrule>,
    pub rdates: Vec<DateTime>,
    pub exdates: Vec<DateTime>,
    /// Set when this `VEVENT` replaces one occurrence of a recurring event
    /// with the same `UID`.
    pub recurrence_id: Option<DateTime>,
    pub sequence: i64,
}

impl IcsEvent {
    /// How long the event runs, as a wall-clock span.
    fn span(&self) -> SignedDuration {
        self.end.duration_until(self.start).abs()
    }
}
/// A parsed feed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IcsCalendar {
    /// `X-WR-CALNAME`, the name the publisher gave the calendar. Google,
    /// Outlook and Apple all set it; it is what saves the subscriber from
    /// having to name a calendar they did not create.
    pub name: Option<String>,
    /// `X-WR-TIMEZONE`, the zone unzoned times in this feed are quoted in.
    pub tz: Option<String>,
    pub events: Vec<IcsEvent>,
}
// ── Recurrence ───────────────────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}
/// An `RRULE`, reduced to the parts this reader expands.
#[derive(Debug, Clone, PartialEq)]
pub struct Rrule {
    pub freq: Freq,
    pub interval: u32,
    pub count: Option<u32>,
    /// Inclusive last instant, in the event's own zone.
    pub until: Option<DateTime>,
    /// `(nth, weekday)`; `nth` is 0 for a plain `MO`, negative for `-1FR`.
    pub by_day: Vec<(i8, Weekday)>,
    pub by_month_day: Vec<i8>,
    pub by_month: Vec<i8>,
    /// True when the rule used a part this reader does not expand, so the
    /// caller can say "the first of these, and we could not work out the
    /// rest" instead of quietly showing one meeting a year.
    pub unsupported: bool,
}

impl Default for Rrule {
    fn default() -> Self {
        Self {
            freq: Freq::Daily,
            interval: 1,
            count: None,
            until: None,
            by_day: Vec::new(),
            by_month_day: Vec::new(),
            by_month: Vec::new(),
            unsupported: false,
        }
    }
}
