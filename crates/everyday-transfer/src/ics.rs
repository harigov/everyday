//! Writing iCalendar, which the core already reads.
//!
//! [`everyday_core::ics`] parses feeds because that is how other people's
//! calendars get in. This is the other direction, and it exists twice over:
//! a subscribed calendar exports as the `.ics` it arrived as, and the todo
//! app's blocks of time export as `.ics` too, because a block of time *is* a
//! calendar event -- the calendar app already draws them on the same grid.
//! Giving them the same file format means an exported week opens in Apple
//! Calendar, Google Calendar or Outlook without anybody being told which
//! folder holds the "real" appointments.
//!
//! # What survives that the standard has no field for
//!
//! The publisher's own `UID` for an event, and the zone it was quoted in, go
//! in `X-EVERYDAY-*` properties -- which is exactly what the extension
//! mechanism is for: every other calendar program ignores them, and this one
//! can read them back.
//!
//! What deliberately does *not* try to fit here is whether a block of time
//! was the plan or the record. There is no property for it that a calendar
//! would show, so the todo app writes its hours as a spreadsheet for coming
//! home and as an `.ics` for going out; see `parts::tasks`.
//!
//! # Times are written in UTC
//!
//! `DTSTART:20260910T113000Z` rather than `DTSTART;TZID=Europe/London:...`.
//! A `TZID` that names a zone the file does not define is the single most
//! common way an `.ics` is misread, and defining it properly means emitting a
//! `VTIMEZONE` with the transition rules for every zone mentioned. UTC is
//! unambiguous everywhere and needs nothing. The zone the record was written
//! in is not lost -- it rides along in `X-EVERYDAY-TZ`, which is where this
//! reads it back from.

use everyday_core::calendar::{Event, EventStatus};
use jiff::Timestamp;
use jiff::civil::Date;
use std::fmt::Write as _;

/// Builds one `VCALENDAR`.
pub struct Ics {
    out: String,
}

impl Ics {
    /// Start a calendar named `name`.
    pub fn new(name: &str) -> Self {
        let mut ics = Self { out: String::new() };
        ics.line("BEGIN", "VCALENDAR");
        ics.line("VERSION", "2.0");
        ics.line("PRODID", "-//Every Day//Export//EN");
        ics.line("CALSCALE", "GREGORIAN");
        // What a subscriber shows in its sidebar, and what `ics::parse` reads
        // back as the calendar's name, so a re-import does not ask for one.
        ics.line("X-WR-CALNAME", name);
        ics
    }

    /// An event, as the vault holds it.
    pub fn event(&mut self, event: &Event) -> &mut Self {
        self.begin(&event.id.to_string(), event.updated_at);
        self.line("SUMMARY", &event.title);
        self.text("DESCRIPTION", &event.description);
        self.text("LOCATION", &event.location);
        self.text("URL", &event.url);
        self.when(event.all_day, event.start, event.end, event.local_date, event.end_date);
        self.line(
            "STATUS",
            match event.status {
                EventStatus::Confirmed => "CONFIRMED",
                EventStatus::Tentative => "TENTATIVE",
                EventStatus::Cancelled => "CANCELLED",
            },
        );
        self.line("TRANSP", if event.busy { "OPAQUE" } else { "TRANSPARENT" });
        if !event.organizer.is_empty() {
            self.line("ORGANIZER", &format!("CN={}:", event.organizer));
        }
        for attendee in &event.attendees {
            self.line("ATTENDEE", &format!("CN={attendee}:"));
        }
        // The publisher's own id for this event. Ours is in `UID`, because an
        // expanded recurring series shares one id across every occurrence and
        // a file with a repeated `UID` is a file most readers deduplicate.
        self.text("X-EVERYDAY-UID", &event.uid);
        self.text("X-EVERYDAY-TZ", &event.tz);
        self.end()
    }

    /// Open a `VEVENT`. Public so a caller with its own properties to add --
    /// a time block -- can use the same framing.
    pub fn begin(&mut self, uid: &str, stamp: Timestamp) -> &mut Self {
        self.line("BEGIN", "VEVENT");
        self.line("UID", &format!("{uid}@everyday"));
        self.line("DTSTAMP", &utc(stamp));
        self
    }

    pub fn end(&mut self) -> &mut Self {
        self.line("END", "VEVENT");
        self
    }

    /// `DTSTART`/`DTEND`, in whichever of the two forms applies.
    ///
    /// An all-day event is a pair of dates and `DTEND` is exclusive -- a
    /// single day ends on the next one. Writing an all-day event as midnight
    /// to midnight in UTC is the bug that makes somebody's birthday show up
    /// on the wrong day west of Greenwich.
    pub fn when(
        &mut self,
        all_day: bool,
        start: Timestamp,
        end: Timestamp,
        local_date: Date,
        end_date: Date,
    ) -> &mut Self {
        if all_day {
            let last = end_date.tomorrow().unwrap_or(end_date);
            self.raw("DTSTART;VALUE=DATE", &date(local_date));
            self.raw("DTEND;VALUE=DATE", &date(last));
        } else {
            self.line("DTSTART", &utc(start));
            self.line("DTEND", &utc(end));
        }
        self
    }

    /// A property, escaped. Skipped when the value is empty: a property with
    /// no value is legal and means something ("this event has no location"),
    /// and absence is the more honest statement.
    pub fn text(&mut self, key: &str, value: &str) -> &mut Self {
        if !value.is_empty() {
            self.line(key, value);
        }
        self
    }

    /// A property whose value needs escaping, folded to fit.
    pub fn line(&mut self, key: &str, value: &str) -> &mut Self {
        let escaped = value
            .replace('\\', "\\\\")
            .replace(';', "\\;")
            .replace(',', "\\,")
            .replace("\r\n", "\\n")
            .replace('\n', "\\n");
        self.raw(key, &escaped)
    }

    /// A property whose value is already in its final form.
    fn raw(&mut self, key: &str, value: &str) -> &mut Self {
        fold(&mut self.out, &format!("{key}:{value}"));
        self
    }

    pub fn finish(mut self) -> String {
        self.line("END", "VCALENDAR");
        self.out
    }
}

/// Wrap a content line at 75 octets, continuing with a leading space.
///
/// Counted in bytes, because that is what the specification counts, and split
/// on a character boundary, because a continuation that begins mid-codepoint
/// is a file that will not parse.
fn fold(out: &mut String, line: &str) {
    const LIMIT: usize = 75;
    let mut rest = line;
    let mut first = true;
    loop {
        let budget = if first { LIMIT } else { LIMIT - 1 };
        if rest.len() <= budget {
            if !first {
                out.push(' ');
            }
            out.push_str(rest);
            out.push_str("\r\n");
            return;
        }
        let mut cut = budget;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        if !first {
            out.push(' ');
        }
        out.push_str(&rest[..cut]);
        out.push_str("\r\n");
        rest = &rest[cut..];
        first = false;
    }
}

fn utc(at: Timestamp) -> String {
    let z = at.to_zoned(jiff::tz::TimeZone::UTC);
    let mut out = String::with_capacity(16);
    let _ = write!(
        out,
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        z.year(),
        z.month(),
        z.day(),
        z.hour(),
        z.minute(),
        z.second()
    );
    out
}

fn date(d: Date) -> String {
    format!("{:04}{:02}{:02}", d.year(), d.month(), d.day())
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::CalendarId;
    use everyday_core::calendar::Event;

    fn event() -> Event {
        let start: Timestamp = "2026-09-10T09:00:00Z".parse().unwrap();
        let end: Timestamp = "2026-09-10T09:15:00Z".parse().unwrap();
        Event {
            id: everyday_core::EventId::new(),
            calendar_id: CalendarId::new(),
            uid: "uid-1".into(),
            title: "Stand-up".into(),
            description: String::new(),
            location: "Room 2; the small one".into(),
            start,
            end,
            local_date: jiff::civil::date(2026, 9, 10),
            end_date: jiff::civil::date(2026, 9, 10),
            tz: "Europe/London".into(),
            all_day: false,
            status: EventStatus::Confirmed,
            organizer: String::new(),
            attendees: Vec::new(),
            url: String::new(),
            busy: true,
            updated_at: start,
        }
    }

    #[test]
    fn a_calendar_is_written_and_read_back() {
        let mut ics = Ics::new("Work");
        ics.event(&event());
        let text = ics.finish();

        let parsed = everyday_core::ics::parse(&text);
        assert_eq!(parsed.name.as_deref(), Some("Work"));
        assert_eq!(parsed.events.len(), 1);
        assert_eq!(parsed.events[0].summary, "Stand-up");
        assert_eq!(parsed.events[0].location, "Room 2; the small one");
    }

    #[test]
    fn an_all_day_event_ends_on_the_day_after_it() {
        let mut e = event();
        e.all_day = true;
        e.local_date = jiff::civil::date(2026, 9, 10);
        e.end_date = jiff::civil::date(2026, 9, 10);
        let mut ics = Ics::new("Birthdays");
        ics.event(&e);
        let text = ics.finish();
        assert!(text.contains("DTSTART;VALUE=DATE:20260910"), "{text}");
        assert!(text.contains("DTEND;VALUE=DATE:20260911"), "{text}");
    }

    #[test]
    fn a_long_line_is_folded_on_a_character_boundary() {
        let mut ics = Ics::new("x");
        ics.line("DESCRIPTION", &"\u{e9}".repeat(200));
        let text = ics.finish();
        for line in text.split("\r\n") {
            assert!(line.len() <= 75, "a line was {} octets: {line}", line.len());
        }
        // The proof that folding did not corrupt it: the parser gets the
        // whole string back.
        let mut ics = Ics::new("x");
        ics.event(&{
            let mut e = event();
            e.description = "\u{e9}".repeat(200);
            e
        });
        let parsed = everyday_core::ics::parse(&ics.finish());
        assert_eq!(parsed.events[0].description, "\u{e9}".repeat(200));
    }

    #[test]
    fn separators_in_the_text_do_not_end_the_property() {
        let mut e = event();
        e.description = "one, two; three\nfour".into();
        let mut ics = Ics::new("x");
        ics.event(&e);
        let parsed = everyday_core::ics::parse(&ics.finish());
        assert_eq!(parsed.events[0].description, "one, two; three\nfour");
    }
}
