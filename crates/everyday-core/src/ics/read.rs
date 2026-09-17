use crate::calendar::{Calendar, Event, EventStatus};
use crate::id::EventId;
use jiff::civil::{Date, DateTime, Time};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

use super::rrule::{occurrences, parse_rrule};
use super::tz::resolve_tzid;
use super::{EXPANSION_CAP, IcsCalendar, IcsEvent, MAX_ATTENDEES, Rrule};

// ── Line parsing ─────────────────────────────────────────────────────────
/// One `NAME;PARAM=VALUE:VALUE` line, unfolded.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Line<'a> {
    pub(super) name: String,
    pub(super) params: Vec<(String, String)>,
    pub(super) value: &'a str,
}

impl Line<'_> {
    fn param(&self, key: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}
/// Undo RFC 5545 line folding: a CRLF followed by a space or tab is a
/// continuation, not a break. Feeds in the wild fold at exactly 75 octets,
/// which lands mid-word and mid-UTF-8-sequence about as often as you would
/// expect, so this must join before anything else looks at the text.
fn unfold(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        match line.strip_prefix([' ', '\t']) {
            Some(rest) if !out.is_empty() => out.last_mut().unwrap().push_str(rest),
            _ => out.push(line.to_string()),
        }
    }
    out
}
/// Split a content line into its name, parameters and value.
///
/// The colon that ends the name-and-parameters section is the first one
/// *outside* double quotes: `ORGANIZER;CN="Smith, J:r":mailto:j@x` is one
/// property with one parameter, and a naive `split(':')` makes three of it.
fn parse_line(line: &str) -> Option<Line<'_>> {
    let mut in_quotes = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let (head, value) = line.split_at(colon);
    let value = &value[1..];

    let mut parts = split_unquoted(head, ';');
    let name = parts.next()?.trim().to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    let params = parts
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            let v = v.trim().trim_matches('"');
            Some((k.trim().to_ascii_uppercase(), v.to_string()))
        })
        .collect();
    Some(Line { name, params, value })
}
fn split_unquoted(s: &str, sep: char) -> impl Iterator<Item = &str> {
    let mut in_quotes = false;
    let mut start = 0;
    let mut out = Vec::new();
    for (i, c) in s.char_indices() {
        if c == '"' {
            in_quotes = !in_quotes;
        } else if c == sep && !in_quotes {
            out.push(&s[start..i]);
            start = i + c.len_utf8();
        }
    }
    out.push(&s[start..]);
    out.into_iter()
}
/// Undo the escaping RFC 5545 applies to TEXT values.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') | Some('N') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(',') => out.push(','),
            Some(';') => out.push(';'),
            // An escape this reader does not know is more likely a literal
            // backslash in a description than a format the writer invented.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}
/// A `DATE` or `DATE-TIME` value, and the zone it should be read in.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Moment {
    pub(super) at: DateTime,
    /// `None` for a floating time: one with no zone, which means "whatever
    /// the clock on the wall says wherever the reader is".
    tz: Option<String>,
    date_only: bool,
}
/// Parse `20260906`, `20260906T140000` or `20260906T140000Z`.
pub(super) fn parse_moment(line: &Line<'_>, value: &str) -> Option<Moment> {
    let v = value.trim();
    let date_only = line.param("VALUE") == Some("DATE") || (v.len() == 8 && !v.contains('T'));

    let (digits, utc) = match v.strip_suffix(['Z', 'z']) {
        Some(rest) => (rest, true),
        None => (v, false),
    };
    let bytes = digits.as_bytes();
    if bytes.len() < 8 || !bytes[..8].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let num = |a: usize, b: usize| digits[a..b].parse::<i16>().ok();
    let date = Date::new(num(0, 4)?, num(4, 6)? as i8, num(6, 8)? as i8).ok()?;

    let at = if date_only || bytes.len() < 15 {
        date.to_datetime(Time::midnight())
    } else {
        if bytes[8] != b'T' {
            return None;
        }
        let time = Time::new(num(9, 11)? as i8, num(11, 13)? as i8, num(13, 15)? as i8, 0).ok()?;
        date.to_datetime(time)
    };

    let tz = if utc { Some("UTC".to_string()) } else { line.param("TZID").and_then(resolve_tzid) };
    Some(Moment { at, tz, date_only })
}
// ── The parser ───────────────────────────────────────────────────────────
/// Read a feed. Never fails on one bad event: a calendar with a malformed
/// `VEVENT` in it should show the other three hundred, not nothing.
pub fn parse(text: &str) -> IcsCalendar {
    let mut out = IcsCalendar::default();
    // A `VEVENT` under construction, and the stack of components around it,
    // so a `DTSTART` inside a `VTIMEZONE` or a `VALARM` is not mistaken for
    // the event's own.
    let mut stack: Vec<String> = Vec::new();
    let mut current: Option<Draft> = None;

    for raw in unfold(text) {
        let Some(line) = parse_line(&raw) else {
            continue;
        };
        match line.name.as_str() {
            "BEGIN" => {
                let component = line.value.trim().to_ascii_uppercase();
                if component == "VEVENT" && stack.iter().all(|c| c == "VCALENDAR") {
                    current = Some(Draft::default());
                }
                stack.push(component);
                continue;
            }
            "END" => {
                let component = line.value.trim().to_ascii_uppercase();
                if component == "VEVENT"
                    && let Some(draft) = current.take()
                    && let Some(event) = draft.finish()
                {
                    out.events.push(event);
                }
                stack.pop();
                continue;
            }
            _ => {}
        }

        // Only ever the outermost VEVENT: a VALARM's TRIGGER and a
        // VTIMEZONE's DTSTART are not the event's.
        let in_event = stack.last().map(String::as_str) == Some("VEVENT");
        if let Some(draft) = current.as_mut()
            && in_event
        {
            draft.apply(&line);
            continue;
        }
        if stack.last().map(String::as_str) == Some("VCALENDAR") {
            match line.name.as_str() {
                "X-WR-CALNAME" => out.name = Some(unescape(line.value).trim().to_string()),
                "X-WR-TIMEZONE" => out.tz = resolve_tzid(line.value),
                _ => {}
            }
        }
    }
    out
}
/// A `VEVENT` being assembled.
#[derive(Debug, Default)]
struct Draft {
    uid: String,
    summary: String,
    description: String,
    location: String,
    organizer: String,
    attendees: Vec<String>,
    url: String,
    status: Option<EventStatus>,
    transparent: bool,
    start: Option<Moment>,
    end: Option<Moment>,
    duration: Option<SignedDuration>,
    rrule: Option<Rrule>,
    rdates: Vec<Moment>,
    exdates: Vec<Moment>,
    recurrence_id: Option<Moment>,
    sequence: i64,
    /// The publisher's own id and quoted zone, when this feed is one of
    /// ours: see the module docs on `X-EVERYDAY-UID`/`X-EVERYDAY-TZ` in
    /// `everyday-transfer`'s writer. Every other calendar program leaves
    /// these two fields `None`, exactly as the extension mechanism intends.
    everyday_uid: Option<String>,
    everyday_tz: Option<String>,
}
impl Draft {
    fn apply(&mut self, line: &Line<'_>) {
        match line.name.as_str() {
            "UID" => self.uid = line.value.trim().to_string(),
            "SUMMARY" => self.summary = unescape(line.value).trim().to_string(),
            "DESCRIPTION" => self.description = unescape(line.value).trim().to_string(),
            "LOCATION" => self.location = unescape(line.value).trim().to_string(),
            "URL" => self.url = line.value.trim().to_string(),
            "SEQUENCE" => self.sequence = line.value.trim().parse().unwrap_or(0),
            "ORGANIZER" => {
                // `CN` is the human name; the value is a mailto: URI. Prefer
                // the name, because "Priya Raman" is what you want on a card
                // and "mailto:praman@example.com" is not.
                self.organizer = line
                    .param("CN")
                    .map(|cn| unescape(cn).trim().to_string())
                    .filter(|cn| !cn.is_empty())
                    .unwrap_or_else(|| line.value.trim().trim_start_matches("mailto:").to_string());
            }
            // One line per person, so this appends rather than assigns. The
            // same `CN`-then-address rule `ORGANIZER` follows, for the same
            // reason: "Priya Raman" is what belongs on a card.
            //
            // Duplicates are dropped, because a feed that lists somebody as
            // both an attendee and a delegate is not saying they are two
            // people. The cap is there because an all-hands with four hundred
            // invitees is a payload nobody's brief needs -- and this is text
            // that goes into a prompt.
            "ATTENDEE" => {
                if self.attendees.len() >= MAX_ATTENDEES {
                    return;
                }
                let who = line
                    .param("CN")
                    .map(|cn| unescape(cn).trim().to_string())
                    .filter(|cn| !cn.is_empty())
                    .unwrap_or_else(|| line.value.trim().trim_start_matches("mailto:").to_string());
                if !who.is_empty() && !self.attendees.iter().any(|a| a.eq_ignore_ascii_case(&who)) {
                    self.attendees.push(who);
                }
            }
            "STATUS" => {
                self.status = match line.value.trim().to_ascii_uppercase().as_str() {
                    "TENTATIVE" => Some(EventStatus::Tentative),
                    "CANCELLED" => Some(EventStatus::Cancelled),
                    _ => Some(EventStatus::Confirmed),
                }
            }
            "TRANSP" => self.transparent = line.value.trim().eq_ignore_ascii_case("TRANSPARENT"),
            "DTSTART" => self.start = parse_moment(line, line.value),
            "DTEND" => self.end = parse_moment(line, line.value),
            "DURATION" => self.duration = parse_duration(line.value),
            "RRULE" => self.rrule = Some(parse_rrule(line.value)),
            "RECURRENCE-ID" => self.recurrence_id = parse_moment(line, line.value),
            // Both are comma-separated lists, and both can appear more than
            // once on the same event.
            "RDATE" => self.rdates.extend(split_moments(line)),
            "EXDATE" => self.exdates.extend(split_moments(line)),
            "X-EVERYDAY-UID" => {
                self.everyday_uid =
                    Some(unescape(line.value).trim().to_string()).filter(|v| !v.is_empty())
            }
            "X-EVERYDAY-TZ" => {
                self.everyday_tz =
                    Some(unescape(line.value).trim().to_string()).filter(|v| !v.is_empty())
            }
            _ => {}
        }
    }

    fn finish(self) -> Option<IcsEvent> {
        let start = self.start?;
        let all_day = start.date_only;

        // DTEND, or DTSTART + DURATION, or a default: RFC 5545 says an
        // all-day event with neither lasts one day and a timed one is
        // instantaneous. A zero-length event cannot be drawn, so it is given
        // a nominal half hour rather than a hairline.
        let end = match (&self.end, self.duration) {
            (Some(e), _) => e.at,
            (None, Some(d)) => start.at.checked_add(d).unwrap_or(start.at),
            (None, None) if all_day => start.at.checked_add(SignedDuration::from_hours(24)).ok()?,
            (None, None) => start.at.checked_add(SignedDuration::from_mins(30)).ok()?,
        };
        // An end before its start is corrupt; treat it as unset rather than
        // letting it underflow every total downstream.
        let end = if end < start.at { start.at } else { end };

        let tz = start.tz.clone();
        Some(IcsEvent {
            // `X-EVERYDAY-UID` wins when it is there: it is the id the
            // publisher actually used, and `UID` on one of our own exports is
            // a wrapper around our own record id rather than theirs (see
            // `Ics::begin` in `everyday-transfer`).
            uid: match self.everyday_uid {
                Some(uid) => uid,
                None if self.uid.is_empty() => {
                    // A feed with no UID cannot be reconciled across syncs,
                    // but it can still be drawn. Key it on when it happens.
                    format!("no-uid-{}-{}", start.at, self.summary)
                }
                None => self.uid,
            },
            summary: self.summary,
            description: self.description,
            location: self.location,
            organizer: self.organizer,
            attendees: self.attendees,
            url: self.url,
            status: self.status.unwrap_or_default(),
            all_day,
            start: start.at,
            end,
            tz: tz.unwrap_or_default(),
            display_tz: self.everyday_tz,
            busy: !self.transparent,
            rrule: self.rrule,
            rdates: self.rdates.into_iter().map(|m| m.at).collect(),
            exdates: self.exdates.into_iter().map(|m| m.at).collect(),
            recurrence_id: self.recurrence_id.map(|m| m.at),
            sequence: self.sequence,
        })
    }
}
fn split_moments(line: &Line<'_>) -> Vec<Moment> {
    line.value.split(',').filter_map(|v| parse_moment(line, v)).collect()
}
/// Parse an RFC 5545 duration: `P1DT2H30M`, `-PT15M`, `P2W`.
fn parse_duration(value: &str) -> Option<SignedDuration> {
    let v = value.trim();
    let (sign, v) = match v.strip_prefix(['-', '+']) {
        Some(rest) if v.starts_with('-') => (-1i64, rest),
        Some(rest) => (1, rest),
        None => (1, v),
    };
    let v = v.strip_prefix(['P', 'p'])?;
    let mut secs: i64 = 0;
    let mut digits = String::new();
    let mut in_time = false;
    for c in v.chars() {
        match c {
            'T' | 't' => in_time = true,
            '0'..='9' => digits.push(c),
            _ => {
                let n: i64 = digits.parse().ok()?;
                digits.clear();
                secs += match c.to_ascii_uppercase() {
                    'W' => n * 7 * 86_400,
                    'D' => n * 86_400,
                    'H' if in_time => n * 3_600,
                    'M' if in_time => n * 60,
                    'S' if in_time => n,
                    _ => return None,
                };
            }
        }
    }
    Some(SignedDuration::from_secs(sign * secs))
}
/// Turn a parsed feed into storable [`Event`]s for one calendar.
///
/// `window` bounds the expansion — a feed's recurring events are only
/// materialised for the days that could be drawn — and `default_tz` is what
/// a floating time is read in, which is the reader's own zone.
///
/// Returns the events and how many occurrences fell outside the window, so
/// the interface can say "3,412 events, and this calendar goes further back
/// than we fetched" rather than implying the feed was empty before 2025.
pub fn events_for(
    calendar: &Calendar,
    feed: &IcsCalendar,
    window: (Date, Date),
    default_tz: &str,
) -> (Vec<Event>, u64) {
    let feed_tz = feed.tz.as_deref().unwrap_or(default_tz);
    let from = window.0.to_datetime(Time::midnight());
    let to = window.1.to_datetime(Time::MAX);

    // Overrides first: a VEVENT carrying RECURRENCE-ID replaces exactly one
    // occurrence of the series with the same UID, and it must win over the
    // rule that would otherwise have produced that slot.
    let mut overrides: std::collections::HashMap<(&str, DateTime), &IcsEvent> = Default::default();
    for e in &feed.events {
        if let Some(rid) = e.recurrence_id {
            overrides
                .entry((e.uid.as_str(), rid))
                // A feed may carry two revisions of the same override; the
                // higher SEQUENCE is the current one.
                .and_modify(|held| {
                    if e.sequence >= held.sequence {
                        *held = e;
                    }
                })
                .or_insert(e);
        }
    }

    let mut out = Vec::new();
    let mut skipped = 0u64;

    for e in &feed.events {
        if e.recurrence_id.is_some() {
            continue; // emitted below, in its series' place
        }
        let zone = zone_for(&e.tz, feed_tz, default_tz);
        let span = e.span();

        let mut starts = match &e.rrule {
            Some(rule) => occurrences(e.start, rule, (from, to)),
            None => {
                if e.start > to || e.start.checked_add(span).unwrap_or(e.start) < from {
                    skipped += 1;
                    Vec::new()
                } else {
                    vec![e.start]
                }
            }
        };
        starts.extend(e.rdates.iter().copied().filter(|d| *d >= from && *d <= to));
        starts.retain(|d| !e.exdates.contains(d));
        starts.sort();
        starts.dedup();

        for start in starts {
            let source = overrides.get(&(e.uid.as_str(), start)).copied().unwrap_or(e);
            // An override carries its own start and end; the rule only said
            // which occurrence it replaces.
            let (local_start, local_end) = if std::ptr::eq(source, e) {
                (start, start.checked_add(span).unwrap_or(start))
            } else {
                (source.start, source.end)
            };
            if local_start > to || local_end < from {
                skipped += 1;
                continue;
            }
            let zone = if std::ptr::eq(source, e) {
                zone.clone()
            } else {
                zone_for(&source.tz, feed_tz, default_tz)
            };
            let Some(event) = materialise(calendar, source, local_start, local_end, &zone) else {
                continue;
            };
            out.push(event);
            if out.len() >= EXPANSION_CAP * 4 {
                // A single feed is not allowed to become the whole vault.
                skipped += 1;
                break;
            }
        }
    }

    out.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.uid.cmp(&b.uid)));
    (out, skipped)
}
/// Which zone a time should be read in: its own, then the feed's, then the
/// reader's. A floating time genuinely means "9am wherever you are", so
/// falling back to the local zone is correct rather than a guess.
fn zone_for(event_tz: &str, feed_tz: &str, default_tz: &str) -> String {
    for candidate in [event_tz, feed_tz, default_tz] {
        if !candidate.is_empty() && TimeZone::get(candidate).is_ok() {
            return candidate.to_string();
        }
    }
    "UTC".to_string()
}
fn materialise(
    calendar: &Calendar,
    source: &IcsEvent,
    local_start: DateTime,
    local_end: DateTime,
    tz: &str,
) -> Option<Event> {
    let zone = TimeZone::get(tz).unwrap_or(TimeZone::UTC);
    // `.first()` rather than an unwrap: on the morning the clocks go
    // forward, 02:30 does not exist, and a meeting nominally at that time
    // should land at the first instant that does rather than vanish.
    let start = local_start
        .to_zoned(zone.clone())
        .ok()
        .map(|z| z.timestamp())
        .or_else(|| zone.to_ambiguous_timestamp(local_start).compatible().ok())?;
    let end = local_end
        .to_zoned(zone.clone())
        .ok()
        .map(|z| z.timestamp())
        .or_else(|| zone.to_ambiguous_timestamp(local_end).compatible().ok())
        .unwrap_or(start);

    // An all-day event's DTEND is *exclusive* — a one-day holiday is
    // `DTSTART;VALUE=DATE:20260710` / `DTEND;VALUE=DATE:20260711` — so the
    // last day it covers is the day before the end.
    let end_date = if source.all_day && local_end.time() == Time::midnight() {
        local_end.date().yesterday().ok().unwrap_or(local_start.date()).max(local_start.date())
    } else {
        local_end.date()
    };

    // The zone to *show* this in, which is not always the one it was read
    // in: our own archives quote every `DTSTART` as a UTC stamp and carry
    // the record's real zone in `X-EVERYDAY-TZ`, so that a day written in
    // Lisbon still reads as a Lisbon day after leaving and coming back. The
    // instant above is already settled and does not move; only the wall
    // clock this is filed under does. An all-day event is a date rather than
    // a time and has no zone to disagree about, so it keeps the dates it
    // arrived with.
    let (tz, local_date, end_date) = match source.display_tz.as_deref() {
        Some(shown) if !source.all_day => match TimeZone::get(shown) {
            Ok(zone) => (
                shown.to_string(),
                start.to_zoned(zone.clone()).date(),
                end.max(start).to_zoned(zone).date(),
            ),
            Err(_) => (tz.to_string(), local_start.date(), end_date),
        },
        _ => (tz.to_string(), local_start.date(), end_date),
    };

    Some(Event {
        id: EventId::new(),
        calendar_id: calendar.id,
        // The occurrence's own start makes the key: two rows of a weekly
        // stand-up share a UID and must not share an identity.
        uid: format!("{}@{}", source.uid, local_start),
        title: if source.summary.is_empty() { "(no title)".into() } else { source.summary.clone() },
        description: source.description.clone(),
        location: source.location.clone(),
        start,
        end: end.max(start),
        local_date,
        end_date,
        tz,
        all_day: source.all_day,
        status: source.status,
        organizer: source.organizer.clone(),
        attendees: source.attendees.clone(),
        url: source.url.clone(),
        busy: source.busy && source.status != EventStatus::Cancelled,
        // Not set: this `uid` already carries the series' own base (see the
        // comment above) and `detect::series_key`'s suffix-stripping
        // fallback already recovers it, the same way it does for CalDAV.
        series: None,
        updated_at: Timestamp::now(),
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use jiff::civil::{date, datetime};

    pub(super) fn feed(body: &str) -> IcsCalendar {
        parse(&format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{body}\r\nEND:VCALENDAR\r\n"))
    }

    pub(super) fn one(body: &str) -> IcsEvent {
        let f = feed(body);
        assert_eq!(f.events.len(), 1, "expected exactly one event in {body:?}");
        f.events.into_iter().next().unwrap()
    }

    #[test]
    fn folded_lines_are_rejoined_before_anything_reads_them() {
        // Every real feed folds at 75 octets, which lands mid-word.
        let e = one("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20260906T090000Z\r\n\
             SUMMARY:A meeting with a very long tit\r\n le that had to be folded\r\n\
             END:VEVENT");
        assert_eq!(e.summary, "A meeting with a very long title that had to be folded");
    }

    #[test]
    fn a_colon_inside_a_quoted_parameter_does_not_end_the_name() {
        let e = one("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20260906T090000Z\r\n\
             ORGANIZER;CN=\"Smith, J:r\":mailto:j@example.com\r\nEND:VEVENT");
        assert_eq!(e.organizer, "Smith, J:r");
    }

    #[test]
    fn text_escapes_are_undone() {
        let e = one("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20260906T090000Z\r\n\
             DESCRIPTION:Bring\\, if you can\\, the notes\\nand a pen\\; thanks\r\nEND:VEVENT");
        assert_eq!(e.description, "Bring, if you can, the notes\nand a pen; thanks");
    }

    #[test]
    fn the_three_shapes_of_a_start_are_all_understood() {
        let utc = one("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20260906T143000Z\r\nEND:VEVENT");
        assert_eq!(utc.start, datetime(2026, 9, 6, 14, 30, 0, 0));
        assert_eq!(utc.tz, "UTC");
        assert!(!utc.all_day);

        let zoned = one(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART;TZID=Europe/Berlin:20260906T143000\r\nEND:VEVENT",
        );
        assert_eq!(zoned.tz, "Europe/Berlin");

        let all_day = one("BEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20260906\r\nEND:VEVENT");
        assert!(all_day.all_day);
        assert_eq!(all_day.start, datetime(2026, 9, 6, 0, 0, 0, 0));

        let floating = one("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20260906T143000\r\nEND:VEVENT");
        assert_eq!(floating.tz, "", "a floating time names no zone");
    }

    #[test]
    fn duration_stands_in_for_a_missing_end() {
        let e = one(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20260906T090000Z\r\nDURATION:PT1H30M\r\nEND:VEVENT",
        );
        assert_eq!(e.end, datetime(2026, 9, 6, 10, 30, 0, 0));
        assert_eq!(parse_duration("P1DT2H"), Some(SignedDuration::from_hours(26)));
        assert_eq!(parse_duration("P2W"), Some(SignedDuration::from_hours(24 * 14)));
    }

    #[test]
    fn an_event_with_neither_end_nor_duration_still_has_a_length() {
        // Legal, and drawn as a hairline if taken at face value.
        let timed = one("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20260906T090000Z\r\nEND:VEVENT");
        assert_eq!(timed.end, datetime(2026, 9, 6, 9, 30, 0, 0));

        let all_day = one("BEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20260906\r\nEND:VEVENT");
        assert_eq!(all_day.end, datetime(2026, 9, 7, 0, 0, 0, 0));
    }

    #[test]
    fn a_dtstart_inside_a_valarm_is_not_the_events_own() {
        let e = one("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20260906T090000Z\r\n\
             BEGIN:VALARM\r\nTRIGGER:-PT15M\r\nDTSTART:19700101T000000Z\r\nEND:VALARM\r\n\
             END:VEVENT");
        assert_eq!(e.start, datetime(2026, 9, 6, 9, 0, 0, 0));
    }

    #[test]
    fn a_vtimezone_block_does_not_become_an_event() {
        let f = feed(
            "BEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\n\
             BEGIN:DAYLIGHT\r\nDTSTART:19700329T020000\r\nTZOFFSETFROM:+0100\r\n\
             TZOFFSETTO:+0200\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n\
             BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20260906T090000Z\r\nEND:VEVENT",
        );
        assert_eq!(f.events.len(), 1);
    }

    #[test]
    fn the_publishers_own_name_for_the_calendar_is_read() {
        let f = feed("X-WR-CALNAME:Priya — Work\r\nX-WR-TIMEZONE:Europe/Berlin");
        assert_eq!(f.name.as_deref(), Some("Priya — Work"));
        assert_eq!(f.tz.as_deref(), Some("Europe/Berlin"));
    }

    #[test]
    fn one_broken_event_does_not_lose_the_rest() {
        let f = feed(
            "BEGIN:VEVENT\r\nUID:broken\r\nDTSTART:not-a-date\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:fine\r\nDTSTART:20260906T090000Z\r\nEND:VEVENT",
        );
        assert_eq!(f.events.len(), 1);
        assert_eq!(f.events[0].uid, "fine");
    }
    // ── materialising ────────────────────────────────────────────────────

    fn calendar() -> Calendar {
        Calendar::subscribed("Work", "https://example.com/w.ics")
    }

    #[test]
    fn exdate_removes_the_occurrence_it_names() {
        let f = feed(
            "BEGIN:VEVENT\r\nUID:standup\r\nSUMMARY:Stand-up\r\n\
             DTSTART:20260907T090000Z\r\nDTEND:20260907T091500Z\r\n\
             RRULE:FREQ=DAILY;COUNT=5\r\nEXDATE:20260909T090000Z\r\nEND:VEVENT",
        );
        let (events, _) = events_for(&calendar(), &f, (date(2026, 9, 1), date(2026, 9, 30)), "UTC");
        let dates: Vec<Date> = events.iter().map(|e| e.local_date).collect();
        assert_eq!(
            dates,
            [date(2026, 9, 7), date(2026, 9, 8), date(2026, 9, 10), date(2026, 9, 11)],
        );
    }

    #[test]
    fn a_recurrence_id_override_replaces_exactly_one_occurrence() {
        // The week the Tuesday stand-up moved to Wednesday afternoon.
        let f = feed(
            "BEGIN:VEVENT\r\nUID:standup\r\nSUMMARY:Stand-up\r\n\
             DTSTART:20260907T090000Z\r\nDTEND:20260907T091500Z\r\n\
             RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=3\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:standup\r\nSUMMARY:Stand-up (moved)\r\n\
             RECURRENCE-ID:20260914T090000Z\r\n\
             DTSTART:20260916T150000Z\r\nDTEND:20260916T153000Z\r\nEND:VEVENT",
        );
        let (events, _) = events_for(&calendar(), &f, (date(2026, 9, 1), date(2026, 9, 30)), "UTC");
        assert_eq!(events.len(), 3);
        let moved = events.iter().find(|e| e.title.contains("moved")).expect("the override");
        assert_eq!(moved.local_date, date(2026, 9, 16));
        assert_eq!(moved.minutes(), 30);
        assert!(
            !events.iter().any(|e| e.local_date == date(2026, 9, 14)),
            "the occurrence it replaced must not also be drawn",
        );
    }

    #[test]
    fn an_all_day_events_exclusive_end_is_the_day_before() {
        // A one-day holiday, as every feed writes one.
        let f = feed(
            "BEGIN:VEVENT\r\nUID:h\r\nSUMMARY:Public holiday\r\n\
             DTSTART;VALUE=DATE:20260710\r\nDTEND;VALUE=DATE:20260711\r\nEND:VEVENT",
        );
        let (events, _) = events_for(&calendar(), &f, (date(2026, 7, 1), date(2026, 7, 31)), "UTC");
        assert_eq!(events[0].local_date, date(2026, 7, 10));
        assert_eq!(events[0].end_date, date(2026, 7, 10), "one day, not two");
        assert!(events[0].all_day);
    }

    #[test]
    fn a_multi_day_event_records_every_day_it_covers() {
        let f = feed(
            "BEGIN:VEVENT\r\nUID:t\r\nSUMMARY:Lisbon\r\n\
             DTSTART;VALUE=DATE:20260710\r\nDTEND;VALUE=DATE:20260721\r\nEND:VEVENT",
        );
        let (events, _) = events_for(&calendar(), &f, (date(2026, 7, 1), date(2026, 7, 31)), "UTC");
        assert_eq!(events[0].local_date, date(2026, 7, 10));
        assert_eq!(events[0].end_date, date(2026, 7, 20));
        assert!(events[0].covers(Some(date(2026, 7, 15)), Some(date(2026, 7, 15))));
    }

    #[test]
    fn a_floating_time_is_read_in_the_readers_own_zone() {
        let f = feed(
            "BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:Lunch\r\nDTSTART:20260906T120000\r\n\
             DTEND:20260906T130000\r\nEND:VEVENT",
        );
        let (events, _) =
            events_for(&calendar(), &f, (date(2026, 9, 1), date(2026, 9, 30)), "Asia/Kolkata");
        assert_eq!(events[0].tz, "Asia/Kolkata");
        // Noon in Kolkata is 06:30 UTC.
        assert_eq!(events[0].start.to_string(), "2026-09-06T06:30:00Z");
    }

    #[test]
    fn the_feeds_own_zone_wins_over_the_readers() {
        let f = feed(
            "X-WR-TIMEZONE:Europe/Berlin\r\n\
             BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20260906T120000\r\nDTEND:20260906T130000\r\nEND:VEVENT",
        );
        let (events, _) =
            events_for(&calendar(), &f, (date(2026, 9, 1), date(2026, 9, 30)), "Asia/Kolkata");
        assert_eq!(events[0].tz, "Europe/Berlin");
    }

    #[test]
    fn occurrences_of_one_series_get_distinct_identities() {
        let f = feed(
            "BEGIN:VEVENT\r\nUID:standup\r\nSUMMARY:Stand-up\r\n\
             DTSTART:20260907T090000Z\r\nDTEND:20260907T091500Z\r\n\
             RRULE:FREQ=DAILY;COUNT=4\r\nEND:VEVENT",
        );
        let (events, _) = events_for(&calendar(), &f, (date(2026, 9, 1), date(2026, 9, 30)), "UTC");
        let uids: std::collections::BTreeSet<&str> =
            events.iter().map(|e| e.uid.as_str()).collect();
        assert_eq!(uids.len(), 4, "a shared UID must not mean a shared identity");
    }

    #[test]
    fn a_cancelled_event_is_kept_and_marked_free() {
        let f = feed(
            "BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:Off\r\nSTATUS:CANCELLED\r\n\
             DTSTART:20260906T090000Z\r\nDTEND:20260906T100000Z\r\nEND:VEVENT",
        );
        let (events, _) = events_for(&calendar(), &f, (date(2026, 9, 1), date(2026, 9, 30)), "UTC");
        assert_eq!(events[0].status, EventStatus::Cancelled);
        assert!(!events[0].busy, "a cancelled meeting does not occupy the slot");
    }

    #[test]
    fn a_transparent_event_does_not_read_as_busy() {
        let f = feed(
            "BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:Birthday\r\nTRANSP:TRANSPARENT\r\n\
             DTSTART;VALUE=DATE:20260906\r\nEND:VEVENT",
        );
        let (events, _) = events_for(&calendar(), &f, (date(2026, 9, 1), date(2026, 9, 30)), "UTC");
        assert!(!events[0].busy);
    }

    #[test]
    fn events_outside_the_window_are_counted_rather_than_silently_lost() {
        let f = feed(
            "BEGIN:VEVENT\r\nUID:old\r\nDTSTART:20200906T090000Z\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:now\r\nDTSTART:20260906T090000Z\r\nEND:VEVENT",
        );
        let (events, skipped) =
            events_for(&calendar(), &f, (date(2026, 1, 1), date(2026, 12, 31)), "UTC");
        assert_eq!(events.len(), 1);
        assert_eq!(skipped, 1);
    }

    #[test]
    fn an_empty_or_junk_feed_is_an_empty_calendar_not_an_error() {
        assert!(parse("").events.is_empty());
        assert!(parse("this is not a calendar at all").events.is_empty());
        // A truncated download: BEGIN with no END.
        assert!(parse("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\n").events.is_empty());
    }
    #[test]
    fn attendees_are_read_by_name_where_the_feed_gives_one() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:m1\r\n\
             DTSTART:20260914T140000Z\r\nDTEND:20260914T150000Z\r\nSUMMARY:Review\r\n\
             ORGANIZER;CN=Priya Raman:mailto:priya@example.com\r\n\
             ATTENDEE;CN=Sam Weatherby;PARTSTAT=ACCEPTED:mailto:sam@example.com\r\n\
             ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:noname@example.com\r\n\
             ATTENDEE;CN=Sam Weatherby:mailto:sam.weatherby@example.com\r\n\
             END:VEVENT\r\nEND:VCALENDAR";
        let parsed = parse(ics);
        let event = &parsed.events[0];
        assert_eq!(event.organizer, "Priya Raman");
        assert_eq!(
            event.attendees,
            vec!["Sam Weatherby".to_string(), "noname@example.com".to_string()],
            "a name where there is one, an address where there is not, and each person once"
        );
    }

    #[test]
    fn an_event_with_nobody_on_it_has_no_attendees_rather_than_an_empty_name() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:m2\r\n\
             DTSTART:20260914T140000Z\r\nDTEND:20260914T150000Z\r\nSUMMARY:Dentist\r\n\
             END:VEVENT\r\nEND:VCALENDAR";
        let parsed = parse(ics);
        assert!(parsed.events[0].attendees.is_empty());
    }

    #[test]
    fn an_all_hands_is_capped_rather_than_carried_into_a_prompt() {
        let mut ics = String::from(
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:m3\r\n\
             DTSTART:20260914T140000Z\r\nDTEND:20260914T150000Z\r\nSUMMARY:All hands\r\n",
        );
        for i in 0..(MAX_ATTENDEES + 20) {
            ics.push_str(&format!("ATTENDEE;CN=Person {i}:mailto:p{i}@example.com\r\n"));
        }
        ics.push_str("END:VEVENT\r\nEND:VCALENDAR");
        let parsed = parse(&ics);
        assert_eq!(parsed.events[0].attendees.len(), MAX_ATTENDEES);
    }
}
