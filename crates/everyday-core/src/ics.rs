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

use crate::calendar::{Calendar, Event, EventStatus};
use crate::id::EventId;
use jiff::civil::{Date, DateTime, Time, Weekday};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};

/// Most occurrences one recurrence rule may produce.
///
/// A daily event over the default window is about 1,100 rows; this is a
/// ceiling on the pathological case — `FREQ=SECONDLY`, or a rule whose
/// `UNTIL` is in the year 4000 — not a limit anyone should reach.
pub const EXPANSION_CAP: usize = 1_500;

/// One `VEVENT`, before recurrence is expanded.
#[derive(Debug, Clone, PartialEq)]
pub struct IcsEvent {
    pub uid: String,
    pub summary: String,
    pub description: String,
    pub location: String,
    pub organizer: String,
    pub url: String,
    pub status: EventStatus,
    pub all_day: bool,
    /// Local wall-clock start, in `tz`.
    pub start: DateTime,
    pub end: DateTime,
    pub tz: String,
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

// ── Line parsing ─────────────────────────────────────────────────────────

/// One `NAME;PARAM=VALUE:VALUE` line, unfolded.
#[derive(Debug, Clone, PartialEq)]
struct Line<'a> {
    name: String,
    params: Vec<(String, String)>,
    value: &'a str,
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

// ── Time zones ───────────────────────────────────────────────────────────

/// Windows time-zone names, as Outlook and Exchange write them, mapped to
/// the IANA names the system database understands.
///
/// A subset — the CLDR mapping has several hundred entries and most name
/// zones nobody's calendar is quoted in. An unlisted name is not an error:
/// it falls back to the feed's own default zone, and then to the reader's,
/// which is at worst the behaviour of a floating time.
pub const WINDOWS_ZONES: &[(&str, &str)] = &[
    ("AUS Eastern Standard Time", "Australia/Sydney"),
    ("AUS Central Standard Time", "Australia/Darwin"),
    ("Arabian Standard Time", "Asia/Dubai"),
    ("Argentina Standard Time", "America/Argentina/Buenos_Aires"),
    ("Atlantic Standard Time", "America/Halifax"),
    ("Canada Central Standard Time", "America/Regina"),
    ("Cen. Australia Standard Time", "Australia/Adelaide"),
    ("Central America Standard Time", "America/Guatemala"),
    ("Central Brazilian Standard Time", "America/Cuiaba"),
    ("Central Europe Standard Time", "Europe/Budapest"),
    ("Central European Standard Time", "Europe/Warsaw"),
    ("Central Standard Time", "America/Chicago"),
    ("Central Standard Time (Mexico)", "America/Mexico_City"),
    ("China Standard Time", "Asia/Shanghai"),
    ("E. Africa Standard Time", "Africa/Nairobi"),
    ("E. Australia Standard Time", "Australia/Brisbane"),
    ("E. South America Standard Time", "America/Sao_Paulo"),
    ("Eastern Standard Time", "America/New_York"),
    ("Egypt Standard Time", "Africa/Cairo"),
    ("FLE Standard Time", "Europe/Kiev"),
    ("GMT Standard Time", "Europe/London"),
    ("GTB Standard Time", "Europe/Bucharest"),
    ("Greenwich Standard Time", "Atlantic/Reykjavik"),
    ("Hawaiian Standard Time", "Pacific/Honolulu"),
    ("India Standard Time", "Asia/Kolkata"),
    ("Iran Standard Time", "Asia/Tehran"),
    ("Israel Standard Time", "Asia/Jerusalem"),
    ("Korea Standard Time", "Asia/Seoul"),
    ("Mountain Standard Time", "America/Denver"),
    ("Mountain Standard Time (Mexico)", "America/Chihuahua"),
    ("New Zealand Standard Time", "Pacific/Auckland"),
    ("Pacific SA Standard Time", "America/Santiago"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("Romance Standard Time", "Europe/Paris"),
    ("Russian Standard Time", "Europe/Moscow"),
    ("SA Pacific Standard Time", "America/Bogota"),
    ("SE Asia Standard Time", "Asia/Bangkok"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("South Africa Standard Time", "Africa/Johannesburg"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("Turkey Standard Time", "Europe/Istanbul"),
    ("US Eastern Standard Time", "America/Indianapolis"),
    ("US Mountain Standard Time", "America/Phoenix"),
    ("UTC", "UTC"),
    ("W. Australia Standard Time", "Australia/Perth"),
    ("W. Central Africa Standard Time", "Africa/Lagos"),
    ("W. Europe Standard Time", "Europe/Berlin"),
];

/// Turn a feed's `TZID` into an IANA name the tz database will accept.
///
/// Three shapes turn up in real feeds: a plain IANA name, a Windows display
/// name, and Mozilla's `/mozilla.org/20050126_1/Europe/Berlin` — a prefixed
/// IANA name, which is why the last two path segments are tried.
pub fn resolve_tzid(tzid: &str) -> Option<String> {
    let tzid = tzid.trim().trim_matches('"');
    if tzid.is_empty() {
        return None;
    }
    if TimeZone::get(tzid).is_ok() {
        return Some(tzid.to_string());
    }
    if let Some((_, iana)) = WINDOWS_ZONES.iter().find(|(win, _)| win.eq_ignore_ascii_case(tzid)) {
        return Some((*iana).to_string());
    }
    // `/mozilla.org/20050126_1/Europe/Berlin` -> `Europe/Berlin`.
    let segs: Vec<&str> = tzid.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() >= 2 {
        let tail = format!("{}/{}", segs[segs.len() - 2], segs[segs.len() - 1]);
        if TimeZone::get(&tail).is_ok() {
            return Some(tail);
        }
    }
    if let Some(last) = segs.last()
        && TimeZone::get(last).is_ok()
    {
        return Some((*last).to_string());
    }
    None
}

/// A `DATE` or `DATE-TIME` value, and the zone it should be read in.
#[derive(Debug, Clone, PartialEq)]
struct Moment {
    at: DateTime,
    /// `None` for a floating time: one with no zone, which means "whatever
    /// the clock on the wall says wherever the reader is".
    tz: Option<String>,
    date_only: bool,
}

/// Parse `20260906`, `20260906T140000` or `20260906T140000Z`.
fn parse_moment(line: &Line<'_>, value: &str) -> Option<Moment> {
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
            uid: if self.uid.is_empty() {
                // A feed with no UID cannot be reconciled across syncs, but
                // it can still be drawn. Key it on when it happens.
                format!("no-uid-{}-{}", start.at, self.summary)
            } else {
                self.uid
            },
            summary: self.summary,
            description: self.description,
            location: self.location,
            organizer: self.organizer,
            url: self.url,
            status: self.status.unwrap_or_default(),
            all_day,
            start: start.at,
            end,
            tz: tz.unwrap_or_default(),
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

fn parse_weekday(code: &str) -> Option<Weekday> {
    Some(match code {
        "MO" => Weekday::Monday,
        "TU" => Weekday::Tuesday,
        "WE" => Weekday::Wednesday,
        "TH" => Weekday::Thursday,
        "FR" => Weekday::Friday,
        "SA" => Weekday::Saturday,
        "SU" => Weekday::Sunday,
        _ => return None,
    })
}

/// Parse an `RRULE` value into the parts this reader expands.
pub fn parse_rrule(value: &str) -> Rrule {
    let mut rule = Rrule::default();
    let mut saw_freq = false;
    for part in value.split(';') {
        let Some((key, val)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_uppercase();
        let val = val.trim();
        match key.as_str() {
            "FREQ" => {
                saw_freq = true;
                match val.to_ascii_uppercase().as_str() {
                    "DAILY" => rule.freq = Freq::Daily,
                    "WEEKLY" => rule.freq = Freq::Weekly,
                    "MONTHLY" => rule.freq = Freq::Monthly,
                    "YEARLY" => rule.freq = Freq::Yearly,
                    // HOURLY, MINUTELY, SECONDLY. Legal, and not a thing a
                    // human calendar contains; expanding one at the cap
                    // would be 1,500 rows of noise.
                    _ => rule.unsupported = true,
                }
            }
            "INTERVAL" => rule.interval = val.parse().unwrap_or(1).max(1),
            "COUNT" => rule.count = val.parse().ok(),
            "UNTIL" => {
                let line = Line { name: "UNTIL".into(), params: Vec::new(), value: val };
                rule.until = parse_moment(&line, val).map(|m| m.at);
            }
            "BYDAY" => {
                for token in val.split(',') {
                    let token = token.trim();
                    let split = token.len().saturating_sub(2);
                    let (ord, code) = token.split_at(split);
                    let Some(weekday) = parse_weekday(&code.to_ascii_uppercase()) else {
                        continue;
                    };
                    let nth = if ord.is_empty() { 0 } else { ord.parse::<i8>().unwrap_or(0) };
                    rule.by_day.push((nth, weekday));
                }
            }
            "BYMONTHDAY" => {
                rule.by_month_day.extend(val.split(',').filter_map(|d| d.trim().parse::<i8>().ok()))
            }
            "BYMONTH" => {
                rule.by_month.extend(val.split(',').filter_map(|d| d.trim().parse::<i8>().ok()))
            }
            // Legal parts this reader cannot expand faithfully. Flagged
            // rather than ignored, so the caller can be honest about it.
            "BYSETPOS" | "BYWEEKNO" | "BYYEARDAY" | "BYHOUR" | "BYMINUTE" | "BYSECOND" => {
                rule.unsupported = true
            }
            _ => {}
        }
    }
    if !saw_freq {
        rule.unsupported = true;
    }
    rule
}

// ── Expansion ────────────────────────────────────────────────────────────

/// Every local start this rule produces, from `seed`, clipped to `window`.
///
/// Walks candidate days forward from the seed rather than generating a
/// closed form. That is slower and very much clearer, and the cost is
/// bounded twice over: by the window, and by [`EXPANSION_CAP`].
fn occurrences(seed: DateTime, rule: &Rrule, window: (DateTime, DateTime)) -> Vec<DateTime> {
    let mut out = Vec::new();
    if rule.unsupported {
        return vec![seed];
    }
    let rule = &seeded(seed, rule);
    let (from, to) = window;
    let stop = match rule.until {
        Some(until) if until < to => until,
        _ => to,
    };
    let time = seed.time();
    let interval = i32::try_from(rule.interval).unwrap_or(1).max(1);

    // A rule with no COUNT and no UNTIL runs forever, so the window is the
    // only bound; with a COUNT, occurrences before the window still consume
    // it, so counting has to start at the seed either way.
    let mut emitted = 0u32;
    let mut cursor = seed.date();
    let mut steps = 0usize;

    let matches_by_day = |d: Date| -> bool {
        if rule.by_day.is_empty() {
            return true;
        }
        rule.by_day.iter().any(|(nth, weekday)| {
            if d.weekday() != *weekday {
                return false;
            }
            match nth {
                0 => true,
                n if *n > 0 => (d.day() - 1) / 7 + 1 == *n,
                n => {
                    let last = d.last_of_month().day();
                    -((last - d.day()) / 7 + 1) == *n
                }
            }
        })
    };
    let matches_month_day = |d: Date| -> bool {
        rule.by_month_day.is_empty()
            || rule.by_month_day.iter().any(|n| {
                let last = d.last_of_month().day();
                let want = if *n < 0 { last + n + 1 } else { *n };
                want == d.day()
            })
    };
    let matches_month =
        |d: Date| -> bool { rule.by_month.is_empty() || rule.by_month.contains(&d.month()) };

    // Days the cursor advances by between candidate windows, and how wide a
    // window each step opens.
    while steps < EXPANSION_CAP * 8 && out.len() < EXPANSION_CAP {
        steps += 1;
        if cursor.to_datetime(time) > stop {
            break;
        }
        if let Some(max) = rule.count
            && emitted >= max
        {
            break;
        }

        // Candidate days within this period of the rule.
        let candidates: Vec<Date> = match rule.freq {
            Freq::Daily => vec![cursor],
            Freq::Weekly => {
                if rule.by_day.is_empty() {
                    vec![cursor]
                } else {
                    // The whole week the cursor sits in, Monday first, so a
                    // `BYDAY=MO,WE,FR` rule emits in calendar order.
                    let back = i32::from(cursor.weekday().to_monday_zero_offset());
                    let Ok(monday) = cursor.checked_add(days(-back)) else {
                        break;
                    };
                    (0..7).filter_map(|i| monday.checked_add(days(i)).ok()).collect()
                }
            }
            Freq::Monthly => {
                let first = cursor.first_of_month();
                (0..first.days_in_month())
                    .filter_map(|i| first.checked_add(days(i.into())).ok())
                    .collect()
            }
            Freq::Yearly => {
                let first = cursor.first_of_year();
                (0..first.days_in_year())
                    .filter_map(|i| first.checked_add(days(i.into())).ok())
                    .collect()
            }
        };

        for day in candidates {
            if day < seed.date() {
                continue;
            }
            let at = day.to_datetime(time);
            if at > stop {
                break;
            }
            if !(matches_by_day(day) && matches_month_day(day) && matches_month(day)) {
                continue;
            }
            emitted += 1;
            if let Some(max) = rule.count
                && emitted > max
            {
                break;
            }
            if at >= from {
                out.push(at);
            }
            if out.len() >= EXPANSION_CAP {
                break;
            }
        }

        // Advance one period.
        let next = match rule.freq {
            Freq::Daily => cursor.checked_add(days(interval)),
            Freq::Weekly => cursor.checked_add(days(interval * 7)),
            Freq::Monthly => {
                cursor.first_of_month().checked_add(jiff::Span::new().months(interval))
            }
            Freq::Yearly => cursor.first_of_year().checked_add(jiff::Span::new().years(interval)),
        };
        let Ok(next) = next else { break };
        cursor = next;
    }

    out.sort();
    out.dedup();
    out
}

/// Fill in the parts of a rule that RFC 5545 says come from `DTSTART`.
///
/// "`FREQ=MONTHLY`" alone means *this day of every month*, not *every day of
/// every month*, and "`FREQ=YEARLY`" means *this date every year*, not *the
/// first of January*. Both read as "no constraint" if the missing parts are
/// left empty, which is how a monthly one-to-one turns into thirty meetings.
/// The defaults are applied once, here, rather than in each arm of the
/// expansion, so there is one place to check them against the spec.
fn seeded(seed: DateTime, rule: &Rrule) -> Rrule {
    let mut rule = rule.clone();
    let day = seed.date().day();
    match rule.freq {
        Freq::Monthly if rule.by_day.is_empty() && rule.by_month_day.is_empty() => {
            rule.by_month_day = vec![day];
        }
        Freq::Yearly if rule.by_day.is_empty() && rule.by_month_day.is_empty() => {
            rule.by_month_day = vec![day];
            if rule.by_month.is_empty() {
                rule.by_month = vec![seed.date().month()];
            }
        }
        // DAILY and WEEKLY already walk day by day and week by week from the
        // seed, so an absent BYDAY genuinely does mean "no constraint".
        _ => {}
    }
    rule
}

fn days(n: i32) -> jiff::Span {
    jiff::Span::new().days(n)
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
        local_date: local_start.date(),
        end_date,
        tz: tz.to_string(),
        all_day: source.all_day,
        status: source.status,
        organizer: source.organizer.clone(),
        url: source.url.clone(),
        busy: source.busy && source.status != EventStatus::Cancelled,
        updated_at: Timestamp::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::{date, datetime};

    fn feed(body: &str) -> IcsCalendar {
        parse(&format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{body}\r\nEND:VCALENDAR\r\n"))
    }

    fn one(body: &str) -> IcsEvent {
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
    fn a_windows_zone_name_resolves_to_an_iana_one() {
        assert_eq!(resolve_tzid("W. Europe Standard Time").as_deref(), Some("Europe/Berlin"));
        assert_eq!(resolve_tzid("Europe/Berlin").as_deref(), Some("Europe/Berlin"));
        assert_eq!(
            resolve_tzid("/mozilla.org/20050126_1/Europe/Berlin").as_deref(),
            Some("Europe/Berlin"),
        );
        assert_eq!(resolve_tzid("Middle-earth Standard Time"), None);
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

    // ── recurrence ───────────────────────────────────────────────────────

    fn expand(rrule: &str, seed: DateTime, from: Date, to: Date) -> Vec<Date> {
        let rule = parse_rrule(rrule);
        occurrences(seed, &rule, (from.to_datetime(Time::midnight()), to.to_datetime(Time::MAX)))
            .into_iter()
            .map(|d| d.date())
            .collect()
    }

    #[test]
    fn a_weekly_rule_lands_on_the_days_it_names() {
        let out = expand(
            "FREQ=WEEKLY;BYDAY=MO,WE,FR",
            datetime(2026, 9, 7, 9, 0, 0, 0), // a Monday
            date(2026, 9, 7),
            date(2026, 9, 20),
        );
        assert_eq!(
            out,
            [
                date(2026, 9, 7),
                date(2026, 9, 9),
                date(2026, 9, 11),
                date(2026, 9, 14),
                date(2026, 9, 16),
                date(2026, 9, 18),
            ],
        );
    }

    #[test]
    fn interval_skips_periods_rather_than_occurrences() {
        let out = expand(
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=TU",
            datetime(2026, 9, 8, 9, 0, 0, 0), // a Tuesday
            date(2026, 9, 1),
            date(2026, 10, 15),
        );
        assert_eq!(out, [date(2026, 9, 8), date(2026, 9, 22), date(2026, 10, 6)]);
    }

    #[test]
    fn an_ordinal_byday_finds_the_third_thursday_and_the_last_friday() {
        let third = expand(
            "FREQ=MONTHLY;BYDAY=3TH",
            datetime(2026, 9, 17, 18, 0, 0, 0),
            date(2026, 9, 1),
            date(2026, 12, 31),
        );
        assert_eq!(
            third,
            [date(2026, 9, 17), date(2026, 10, 15), date(2026, 11, 19), date(2026, 12, 17)]
        );

        let last = expand(
            "FREQ=MONTHLY;BYDAY=-1FR",
            datetime(2026, 9, 25, 16, 0, 0, 0),
            date(2026, 9, 1),
            date(2026, 11, 30),
        );
        assert_eq!(last, [date(2026, 9, 25), date(2026, 10, 30), date(2026, 11, 27)]);
    }

    #[test]
    fn count_is_consumed_by_occurrences_before_the_window_too() {
        // The bug this guards: asking for October and getting five more
        // dailies out of a rule that was only ever meant to fire three times
        // in September.
        let out = expand(
            "FREQ=DAILY;COUNT=3",
            datetime(2026, 9, 1, 9, 0, 0, 0),
            date(2026, 9, 3),
            date(2026, 12, 31),
        );
        assert_eq!(out, [date(2026, 9, 3)]);
    }

    #[test]
    fn until_is_inclusive_and_stops_the_series() {
        let out = expand(
            "FREQ=DAILY;UNTIL=20260904T090000Z",
            datetime(2026, 9, 1, 9, 0, 0, 0),
            date(2026, 9, 1),
            date(2026, 12, 31),
        );
        assert_eq!(out, [date(2026, 9, 1), date(2026, 9, 2), date(2026, 9, 3), date(2026, 9, 4)]);
    }

    #[test]
    fn a_yearly_rule_keeps_its_day() {
        let out = expand(
            "FREQ=YEARLY",
            datetime(2026, 3, 14, 0, 0, 0, 0),
            date(2026, 1, 1),
            date(2029, 12, 31),
        );
        assert_eq!(
            out,
            [date(2026, 3, 14), date(2027, 3, 14), date(2028, 3, 14), date(2029, 3, 14)]
        );
    }

    #[test]
    fn a_bare_monthly_rule_means_this_day_of_the_month_not_every_day_of_it() {
        // The bug this guards: `FREQ=MONTHLY` with no BY parts reading as
        // "no constraint", which turns a monthly one-to-one into thirty
        // meetings.
        let out = expand(
            "FREQ=MONTHLY",
            datetime(2026, 9, 15, 11, 0, 0, 0),
            date(2026, 9, 1),
            date(2026, 12, 31),
        );
        assert_eq!(
            out,
            [date(2026, 9, 15), date(2026, 10, 15), date(2026, 11, 15), date(2026, 12, 15)],
        );
    }

    #[test]
    fn a_monthly_rule_on_the_31st_skips_the_months_that_have_no_31st() {
        let out = expand(
            "FREQ=MONTHLY",
            datetime(2026, 1, 31, 9, 0, 0, 0),
            date(2026, 1, 1),
            date(2026, 5, 31),
        );
        assert_eq!(
            out,
            [date(2026, 1, 31), date(2026, 3, 31), date(2026, 5, 31)],
            "February and April have no 31st, and inventing one is worse than skipping it",
        );
    }

    #[test]
    fn a_bare_yearly_rule_keeps_the_month_as_well_as_the_day() {
        // The bug this guards: the cursor jumping to the first of January
        // and the rule then matching it, so a birthday moves to New Year.
        let out = expand(
            "FREQ=YEARLY",
            datetime(2026, 3, 14, 0, 0, 0, 0),
            date(2026, 1, 1),
            date(2028, 12, 31),
        );
        assert_eq!(out, [date(2026, 3, 14), date(2027, 3, 14), date(2028, 3, 14)]);
    }

    #[test]
    fn a_rule_this_reader_cannot_expand_yields_its_seed_and_says_so() {
        let rule = parse_rrule("FREQ=MONTHLY;BYSETPOS=-1;BYDAY=MO,TU,WE,TH,FR");
        assert!(rule.unsupported, "BYSETPOS is not expanded and must be flagged");
        let out = expand(
            "FREQ=MONTHLY;BYSETPOS=-1;BYDAY=MO",
            datetime(2026, 9, 28, 9, 0, 0, 0),
            date(2026, 1, 1),
            date(2026, 12, 31),
        );
        assert_eq!(out, [date(2026, 9, 28)], "one honest occurrence beats twelve invented ones");
    }

    #[test]
    fn an_endless_daily_rule_is_bounded_by_the_window_not_by_time() {
        let out = expand(
            "FREQ=DAILY",
            datetime(2026, 1, 1, 9, 0, 0, 0),
            date(2026, 3, 1),
            date(2026, 3, 31),
        );
        assert_eq!(out.len(), 31);
        assert_eq!(out[0], date(2026, 3, 1));
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
}
