//! A repeat somebody *chose*, as opposed to one a feed handed us.
//!
//! [`crate::ics`] already reads every `RRULE` a subscribed calendar can
//! throw at it, and expands it into rows. That reader is deliberately
//! lenient and deliberately partial: it exists to draw somebody else's
//! calendar. This module is the other direction -- the handful of shapes a
//! person actually picks when they say "this repeats", held as a structure
//! rather than as `RRULE` text, because three different readers need it:
//!
//! - the interface, which draws a picker and a sentence ("Every week on
//!   Monday and Wednesday") and has no business parsing RFC 5545;
//! - the assistant, whose tool schema is written for a model, and a model
//!   asked for `{"frequency": "weekly", "weekdays": ["monday"]}` gets it
//!   right far more often than one asked to write `BYDAY=MO`;
//! - Microsoft Graph, which does not take `RRULE` at all and wants its own
//!   `patternedRecurrence` object instead.
//!
//! Google and CalDAV take `RRULE` text, which [`Recurrence::to_rrule`]
//! writes; a series read back from either goes through
//! [`Recurrence::from_rrule`], which answers `None` for a rule this shape
//! cannot hold rather than quietly simplifying it -- an editor that turned
//! "the last weekday of the month" into "monthly" on save would be
//! rewriting somebody's series behind their back.
//!
//! A repeating block of your own time is expanded through the same engine a
//! feed's recurrence is ([`crate::ics`]'s, via [`Recurrence::occurrences`]),
//! so "every other Tuesday" cannot mean one thing on a subscribed calendar
//! and another on yours.

use crate::error::{Error, Result};
use jiff::civil::{Date, DateTime};
use serde::{Deserialize, Serialize};

/// How often a repeat comes round, before [`Recurrence::interval`] stretches
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum Frequency {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Frequency {
    pub const ALL: [Frequency; 4] =
        [Frequency::Daily, Frequency::Weekly, Frequency::Monthly, Frequency::Yearly];

    pub fn as_str(self) -> &'static str {
        match self {
            Frequency::Daily => "daily",
            Frequency::Weekly => "weekly",
            Frequency::Monthly => "monthly",
            Frequency::Yearly => "yearly",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.as_str() == s)
    }

    fn rrule(self) -> &'static str {
        match self {
            Frequency::Daily => "DAILY",
            Frequency::Weekly => "WEEKLY",
            Frequency::Monthly => "MONTHLY",
            Frequency::Yearly => "YEARLY",
        }
    }

    /// The unit, for a sentence: "every 2 weeks".
    fn unit(self) -> &'static str {
        match self {
            Frequency::Daily => "day",
            Frequency::Weekly => "week",
            Frequency::Monthly => "month",
            Frequency::Yearly => "year",
        }
    }
}

/// A day of the week, spelled out.
///
/// Its own type rather than `jiff`'s, because this one crosses the wire --
/// to the interface and to a model -- and `"monday"` is a better word to put
/// in front of either than whatever a time library's serde happens to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    pub const ALL: [Weekday; 7] = [
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
        Weekday::Saturday,
        Weekday::Sunday,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Weekday::Monday => "monday",
            Weekday::Tuesday => "tuesday",
            Weekday::Wednesday => "wednesday",
            Weekday::Thursday => "thursday",
            Weekday::Friday => "friday",
            Weekday::Saturday => "saturday",
            Weekday::Sunday => "sunday",
        }
    }

    /// Accepts the full name, its first three letters, or RFC 5545's two,
    /// in any case -- `"Monday"`, `"mon"`, `"MO"` -- because the people and
    /// models typing these are not all going to agree on one.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|d| {
            let name = d.as_str();
            s == name || s == name[..3] || s == name[..2]
        })
    }

    /// RFC 5545's two letters: `MO`, `TU`, ...
    pub fn code(self) -> &'static str {
        match self {
            Weekday::Monday => "MO",
            Weekday::Tuesday => "TU",
            Weekday::Wednesday => "WE",
            Weekday::Thursday => "TH",
            Weekday::Friday => "FR",
            Weekday::Saturday => "SA",
            Weekday::Sunday => "SU",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Weekday::Monday => "Monday",
            Weekday::Tuesday => "Tuesday",
            Weekday::Wednesday => "Wednesday",
            Weekday::Thursday => "Thursday",
            Weekday::Friday => "Friday",
            Weekday::Saturday => "Saturday",
            Weekday::Sunday => "Sunday",
        }
    }

    pub fn to_jiff(self) -> jiff::civil::Weekday {
        match self {
            Weekday::Monday => jiff::civil::Weekday::Monday,
            Weekday::Tuesday => jiff::civil::Weekday::Tuesday,
            Weekday::Wednesday => jiff::civil::Weekday::Wednesday,
            Weekday::Thursday => jiff::civil::Weekday::Thursday,
            Weekday::Friday => jiff::civil::Weekday::Friday,
            Weekday::Saturday => jiff::civil::Weekday::Saturday,
            Weekday::Sunday => jiff::civil::Weekday::Sunday,
        }
    }

    pub fn from_jiff(day: jiff::civil::Weekday) -> Self {
        match day {
            jiff::civil::Weekday::Monday => Weekday::Monday,
            jiff::civil::Weekday::Tuesday => Weekday::Tuesday,
            jiff::civil::Weekday::Wednesday => Weekday::Wednesday,
            jiff::civil::Weekday::Thursday => Weekday::Thursday,
            jiff::civil::Weekday::Friday => Weekday::Friday,
            jiff::civil::Weekday::Saturday => Weekday::Saturday,
            jiff::civil::Weekday::Sunday => Weekday::Sunday,
        }
    }
}

fn one() -> u32 {
    1
}

/// How an event repeats.
///
/// Four shapes cover what a person picks from a calendar's "Repeat" menu,
/// and this holds exactly those:
///
/// ```text
///   daily                          every N days
///   weekly   + weekdays            every N weeks, on these days (none: the start's own day)
///   monthly                        every N months, on the start's day of the month
///   monthly  + week_of_month + 1 weekday   "the second Tuesday", "the last Friday"
///   yearly                         every N years, on the start's date
/// ```
///
/// Ended by at most one of `count` or `until`; neither is forever.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Recurrence {
    pub frequency: Frequency,
    /// Every how many `frequency`s: 2 with `weekly` is every other week.
    #[serde(default = "one")]
    pub interval: u32,
    /// `weekly`: the days it falls on. `monthly` with `week_of_month`: the
    /// one day that week names. Empty otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub weekdays: Vec<Weekday>,
    /// `monthly` only: which week of the month -- 1 to 4, or -1 for the
    /// last -- with exactly one entry in `weekdays`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub week_of_month: Option<i8>,
    /// Stop after this many occurrences, the first included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// The last day an occurrence may fall on, inclusive, in the event's own
    /// zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<Date>,
}

/// The most occurrences a single rule may be asked for by count -- a
/// generous ceiling that still refuses a typo of 100000.
pub const MAX_COUNT: u32 = 1_000;

impl Recurrence {
    /// A plain repeat with nothing narrowing it: every day, every week on the
    /// start's own day, every month on its date, every year on its date.
    pub fn every(frequency: Frequency) -> Self {
        Self {
            frequency,
            interval: 1,
            weekdays: Vec::new(),
            week_of_month: None,
            count: None,
            until: None,
        }
    }

    /// Refuse a rule that could not be written to any calendar, in words a
    /// person -- or a model -- can act on.
    pub fn validate(&self) -> Result<()> {
        if self.interval == 0 || self.interval > 999 {
            return Err(Error::Invalid("a repeat's interval must be between 1 and 999".into()));
        }
        if self.count.is_some() && self.until.is_some() {
            return Err(Error::Invalid(
                "a repeat ends after a number of times or on a date, not both".into(),
            ));
        }
        if let Some(count) = self.count
            && (count == 0 || count > MAX_COUNT)
        {
            return Err(Error::Invalid(format!(
                "a repeat may run between 1 and {MAX_COUNT} times"
            )));
        }
        match (self.frequency, self.week_of_month) {
            (Frequency::Monthly, Some(week)) => {
                if !matches!(week, -1 | 1..=4) {
                    return Err(Error::Invalid(
                        "week_of_month is 1 to 4, or -1 for the last week".into(),
                    ));
                }
                if self.weekdays.len() != 1 {
                    return Err(Error::Invalid(
                        "a monthly repeat by week names exactly one weekday, as in \"the \
                         second Tuesday\""
                            .into(),
                    ));
                }
            }
            (_, Some(_)) => {
                return Err(Error::Invalid(
                    "week_of_month only applies to a monthly repeat".into(),
                ));
            }
            (Frequency::Weekly, None) => {}
            (_, None) if !self.weekdays.is_empty() => {
                return Err(Error::Invalid(
                    "weekdays apply to a weekly repeat, or a monthly one with week_of_month".into(),
                ));
            }
            _ => {}
        }
        Ok(())
    }

    /// The days a weekly rule falls on, with an empty list read the way RFC
    /// 5545 reads it: the start's own day. Sorted Monday first, without
    /// repeats. Only meaningful for `weekly`; every other frequency answers
    /// `weekdays` as given.
    pub fn days_for(&self, start: Date) -> Vec<Weekday> {
        let mut days = self.weekdays.clone();
        if self.frequency == Frequency::Weekly && days.is_empty() {
            days.push(Weekday::from_jiff(start.weekday()));
        }
        days.sort();
        days.dedup();
        days
    }

    /// This rule as an RFC 5545 `RRULE` value -- no `RRULE:` prefix.
    ///
    /// `until` is a *day* here but RFC 5545 wants it in the same form as
    /// `DTSTART`: a bare `DATE` for an all-day event, and a UTC instant for
    /// a timed one (§3.3.10: "if the DTSTART property is specified as a date
    /// with UTC time or a date with local time and time zone reference,
    /// then the UNTIL rule part MUST be specified as a date with UTC time").
    /// So a timed event's last day is written as the last second of that day
    /// in `tz`, converted to UTC -- which still admits an occurrence late on
    /// that day and nothing on the next.
    pub fn to_rrule(&self, all_day: bool, tz: &str) -> String {
        let mut parts = vec![format!("FREQ={}", self.frequency.rrule())];
        if self.interval > 1 {
            parts.push(format!("INTERVAL={}", self.interval));
        }
        match (self.frequency, self.week_of_month) {
            (Frequency::Monthly, Some(week)) => {
                if let Some(day) = self.weekdays.first() {
                    parts.push(format!("BYDAY={week}{}", day.code()));
                }
            }
            (Frequency::Weekly, _) if !self.weekdays.is_empty() => {
                let mut days = self.weekdays.clone();
                days.sort();
                days.dedup();
                let codes: Vec<&str> = days.iter().map(|d| d.code()).collect();
                parts.push(format!("BYDAY={}", codes.join(",")));
            }
            _ => {}
        }
        if let Some(count) = self.count {
            parts.push(format!("COUNT={count}"));
        } else if let Some(until) = self.until {
            if all_day {
                parts.push(format!("UNTIL={}", until.strftime("%Y%m%d")));
            } else {
                let zone = jiff::tz::TimeZone::get(tz).unwrap_or(jiff::tz::TimeZone::UTC);
                let last = until.to_datetime(jiff::civil::time(23, 59, 59, 0));
                let instant = zone
                    .to_ambiguous_zoned(last)
                    .later()
                    .map(|z| z.timestamp())
                    .unwrap_or_else(|_| jiff::Timestamp::UNIX_EPOCH);
                parts.push(format!("UNTIL={}", instant.strftime("%Y%m%dT%H%M%SZ")));
            }
        }
        parts.join(";")
    }

    /// Read an `RRULE` value back, with or without its `RRULE:` prefix.
    ///
    /// `None` for anything this shape cannot hold exactly -- several days
    /// of the month, `BYSETPOS`, an hourly rule -- so a caller can say "this
    /// series repeats in a way that can only be changed where it was made"
    /// rather than offer an editor that would flatten it on save. A
    /// `BYMONTHDAY` or `BYMONTH` naming a single value is accepted and
    /// dropped: it is what every calendar writes for "monthly on the 12th",
    /// and the series' own start already says the 12th.
    pub fn from_rrule(value: &str) -> Option<Self> {
        let value = value.trim();
        let value =
            value.strip_prefix("RRULE:").or_else(|| value.strip_prefix("rrule:")).unwrap_or(value);
        let rule = crate::ics::parse_rrule(value);
        if rule.unsupported || rule.by_month_day.len() > 1 || rule.by_month.len() > 1 {
            return None;
        }
        let frequency = match rule.freq {
            crate::ics::Freq::Daily => Frequency::Daily,
            crate::ics::Freq::Weekly => Frequency::Weekly,
            crate::ics::Freq::Monthly => Frequency::Monthly,
            crate::ics::Freq::Yearly => Frequency::Yearly,
        };
        let mut out = Recurrence {
            frequency,
            interval: rule.interval.max(1),
            weekdays: Vec::new(),
            week_of_month: None,
            count: rule.count,
            until: rule.until.map(|u| u.date()),
        };
        match frequency {
            Frequency::Weekly => {
                if rule.by_day.iter().any(|(nth, _)| *nth != 0) {
                    return None;
                }
                out.weekdays = rule.by_day.iter().map(|(_, d)| Weekday::from_jiff(*d)).collect();
                out.weekdays.sort();
                out.weekdays.dedup();
            }
            Frequency::Monthly => match rule.by_day.as_slice() {
                [] => {}
                [(nth, day)] if matches!(*nth, -1 | 1..=4) && rule.by_month_day.is_empty() => {
                    out.week_of_month = Some(*nth);
                    out.weekdays = vec![Weekday::from_jiff(*day)];
                }
                _ => return None,
            },
            Frequency::Daily | Frequency::Yearly => {
                if !rule.by_day.is_empty() {
                    return None;
                }
            }
        }
        // `COUNT` and `UNTIL` together are illegal per RFC 5545; a server
        // that sent both gets the count, which is the stricter of the two
        // for the purposes of drawing it.
        if out.count.is_some() {
            out.until = None;
        }
        out.validate().ok()?;
        Some(out)
    }

    /// [`Recurrence::from_rrule`], for a series quoted in zone `tz`.
    ///
    /// The difference is `UNTIL`. A timed series' `UNTIL` is a UTC instant
    /// (RFC 5545 insists), and its *day* is the day that instant falls on
    /// where the series is held -- the last second of 31 December in New
    /// York is already 1 January in UTC, and reading the UTC date would give
    /// every series west of Greenwich one day too many. A bare date, or a
    /// floating time, already is the series' own day and is read as it is.
    pub fn from_rrule_in(value: &str, tz: &str) -> Option<Self> {
        let mut rule = Self::from_rrule(value)?;
        let until = value
            .split(';')
            .filter_map(|part| part.split_once('='))
            .find(|(key, _)| key.trim().eq_ignore_ascii_case("UNTIL"))
            .map(|(_, raw)| raw.trim());
        if let Some(raw) = until
            && raw.len() == 16
            && raw.ends_with(['Z', 'z'])
            && let Ok(at) = jiff::civil::DateTime::strptime("%Y%m%dT%H%M%S", &raw[..15])
            && let Ok(zone) = jiff::tz::TimeZone::get(tz)
            && let Ok(instant) = jiff::tz::TimeZone::UTC.to_ambiguous_zoned(at).compatible()
        {
            rule.until = Some(instant.timestamp().to_zoned(zone).date());
        }
        Some(rule)
    }

    /// Every local start this rule produces, from `seed` -- the first
    /// occurrence, which is always included -- up to and including the day
    /// `through`, and never more than `cap` of them.
    ///
    /// Runs on [`crate::ics`]'s own expander, so a repeat made here and one
    /// read from a feed agree to the day.
    pub fn occurrences(&self, seed: DateTime, through: Date, cap: usize) -> Vec<DateTime> {
        let mut rule = crate::ics::Rrule {
            freq: match self.frequency {
                Frequency::Daily => crate::ics::Freq::Daily,
                Frequency::Weekly => crate::ics::Freq::Weekly,
                Frequency::Monthly => crate::ics::Freq::Monthly,
                Frequency::Yearly => crate::ics::Freq::Yearly,
            },
            interval: self.interval.max(1),
            count: self.count,
            until: self.until.map(|d| d.to_datetime(jiff::civil::time(23, 59, 59, 0))),
            ..Default::default()
        };
        match (self.frequency, self.week_of_month) {
            (Frequency::Monthly, Some(week)) => {
                rule.by_day = self.weekdays.iter().map(|d| (week, d.to_jiff())).collect();
            }
            (Frequency::Weekly, _) => {
                rule.by_day = self.weekdays.iter().map(|d| (0, d.to_jiff())).collect();
            }
            _ => {}
        }
        let end = through.to_datetime(jiff::civil::time(23, 59, 59, 0));
        let mut out = crate::ics::occurrences(seed, &rule, (seed, end));
        // A weekly rule naming days that do not include the start's own
        // still begins on the start, the way every calendar app draws it:
        // the event somebody just made is on the day they made it.
        if out.first() != Some(&seed) && seed <= end {
            out.insert(0, seed);
            if let Some(count) = self.count
                && out.len() > count as usize
            {
                out.truncate(count as usize);
            }
        }
        out.truncate(cap);
        out
    }

    /// The rule in a sentence, for a confirmation card or a tool's answer:
    /// "every week on Monday and Wednesday, until 2026-12-31".
    pub fn describe(&self) -> String {
        let mut out = if self.interval == 1 {
            format!("every {}", self.frequency.unit())
        } else {
            format!("every {} {}s", self.interval, self.frequency.unit())
        };
        match (self.frequency, self.week_of_month) {
            (Frequency::Monthly, Some(week)) => {
                let nth = match week {
                    1 => "first",
                    2 => "second",
                    3 => "third",
                    4 => "fourth",
                    _ => "last",
                };
                if let Some(day) = self.weekdays.first() {
                    out.push_str(&format!(" on the {nth} {}", day.title()));
                }
            }
            (Frequency::Weekly, _) if !self.weekdays.is_empty() => {
                let names: Vec<&str> = self.weekdays.iter().map(|d| d.title()).collect();
                out.push_str(" on ");
                out.push_str(&join_and(&names));
            }
            _ => {}
        }
        if let Some(count) = self.count {
            out.push_str(&format!(", {count} times"));
        } else if let Some(until) = self.until {
            out.push_str(&format!(", until {until}"));
        }
        out
    }
}

fn join_and(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::{date, datetime};

    fn weekly(days: &[Weekday]) -> Recurrence {
        Recurrence { weekdays: days.to_vec(), ..Recurrence::every(Frequency::Weekly) }
    }

    #[test]
    fn weekdays_are_read_however_they_are_spelled() {
        for s in ["monday", "Monday", "MON", "mo", "MO"] {
            assert_eq!(Weekday::parse(s), Some(Weekday::Monday), "{s}");
        }
        assert_eq!(Weekday::parse("thu"), Some(Weekday::Thursday));
        assert_eq!(Weekday::parse("someday"), None);
    }

    #[test]
    fn a_rule_is_written_as_the_rrule_every_calendar_reads() {
        assert_eq!(Recurrence::every(Frequency::Daily).to_rrule(false, "UTC"), "FREQ=DAILY");
        assert_eq!(
            Recurrence { interval: 2, ..weekly(&[Weekday::Wednesday, Weekday::Monday]) }
                .to_rrule(false, "UTC"),
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE",
        );
        let second_tuesday = Recurrence {
            weekdays: vec![Weekday::Tuesday],
            week_of_month: Some(2),
            count: Some(6),
            ..Recurrence::every(Frequency::Monthly)
        };
        assert_eq!(second_tuesday.to_rrule(false, "UTC"), "FREQ=MONTHLY;BYDAY=2TU;COUNT=6");
        let last_friday = Recurrence {
            weekdays: vec![Weekday::Friday],
            week_of_month: Some(-1),
            ..Recurrence::every(Frequency::Monthly)
        };
        assert_eq!(last_friday.to_rrule(false, "UTC"), "FREQ=MONTHLY;BYDAY=-1FR");
    }

    #[test]
    fn until_is_a_date_for_an_all_day_event_and_a_utc_instant_for_a_timed_one() {
        let rule =
            Recurrence { until: Some(date(2026, 12, 31)), ..Recurrence::every(Frequency::Weekly) };
        assert_eq!(rule.to_rrule(true, "Europe/Berlin"), "FREQ=WEEKLY;UNTIL=20261231");
        // 23:59:59 in Berlin, in winter (UTC+1), is 22:59:59 UTC.
        assert_eq!(rule.to_rrule(false, "Europe/Berlin"), "FREQ=WEEKLY;UNTIL=20261231T225959Z");
    }

    #[test]
    fn a_rule_survives_the_round_trip_through_rrule_text() {
        let rules = [
            Recurrence::every(Frequency::Daily),
            Recurrence { interval: 3, ..weekly(&[Weekday::Monday, Weekday::Friday]) },
            Recurrence {
                weekdays: vec![Weekday::Thursday],
                week_of_month: Some(-1),
                count: Some(12),
                ..Recurrence::every(Frequency::Monthly)
            },
            Recurrence { until: Some(date(2027, 6, 1)), ..Recurrence::every(Frequency::Yearly) },
        ];
        for rule in rules {
            let text = rule.to_rrule(true, "UTC");
            assert_eq!(Recurrence::from_rrule(&text), Some(rule.clone()), "{text}");
            assert_eq!(Recurrence::from_rrule(&format!("RRULE:{text}")), Some(rule), "{text}");
        }
    }

    #[test]
    fn a_utc_until_is_read_as_the_day_it_falls_on_where_the_series_is_held() {
        // What `to_rrule` writes for a timed series ending 31 December in
        // New York: the last second of that day there, in UTC.
        let rule =
            Recurrence { until: Some(date(2026, 12, 31)), ..Recurrence::every(Frequency::Weekly) };
        let text = rule.to_rrule(false, "America/New_York");
        assert_eq!(text, "FREQ=WEEKLY;UNTIL=20270101T045959Z");
        assert_eq!(Recurrence::from_rrule(&text).unwrap().until, Some(date(2027, 1, 1)));
        assert_eq!(
            Recurrence::from_rrule_in(&text, "America/New_York").unwrap().until,
            Some(date(2026, 12, 31)),
        );
        // A bare date is already the series' own day.
        assert_eq!(
            Recurrence::from_rrule_in("FREQ=DAILY;UNTIL=20261231", "America/New_York")
                .unwrap()
                .until,
            Some(date(2026, 12, 31)),
        );
    }

    #[test]
    fn a_rule_this_shape_cannot_hold_is_refused_rather_than_flattened() {
        for text in [
            "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1",
            "FREQ=MONTHLY;BYMONTHDAY=1,15",
            "FREQ=HOURLY",
            "FREQ=WEEKLY;BYDAY=1MO",
            "FREQ=DAILY;BYDAY=MO",
        ] {
            assert_eq!(Recurrence::from_rrule(text), None, "{text}");
        }
        // ...but "monthly on the 12th", spelled out, is just monthly.
        assert_eq!(
            Recurrence::from_rrule("FREQ=MONTHLY;BYMONTHDAY=12"),
            Some(Recurrence::every(Frequency::Monthly)),
        );
    }

    #[test]
    fn nonsense_is_refused_in_words() {
        let both = Recurrence {
            count: Some(3),
            until: Some(date(2026, 1, 1)),
            ..Recurrence::every(Frequency::Daily)
        };
        assert!(both.validate().is_err());
        assert!(
            Recurrence { interval: 0, ..Recurrence::every(Frequency::Daily) }.validate().is_err()
        );
        assert!(
            Recurrence { week_of_month: Some(2), ..Recurrence::every(Frequency::Weekly) }
                .validate()
                .is_err()
        );
        assert!(
            Recurrence {
                week_of_month: Some(5),
                weekdays: vec![Weekday::Monday],
                ..Recurrence::every(Frequency::Monthly)
            }
            .validate()
            .is_err()
        );
        assert!(
            Recurrence { weekdays: vec![Weekday::Monday], ..Recurrence::every(Frequency::Daily) }
                .validate()
                .is_err()
        );
        assert!(weekly(&[Weekday::Monday, Weekday::Thursday]).validate().is_ok());
    }

    #[test]
    fn occurrences_start_on_the_seed_and_stop_where_they_are_told() {
        let seed = datetime(2026, 10, 5, 9, 0, 0, 0); // a Monday
        let rule = weekly(&[Weekday::Monday, Weekday::Wednesday]);
        let got = rule.occurrences(seed, date(2026, 10, 18), 100);
        assert_eq!(
            got,
            vec![
                datetime(2026, 10, 5, 9, 0, 0, 0),
                datetime(2026, 10, 7, 9, 0, 0, 0),
                datetime(2026, 10, 12, 9, 0, 0, 0),
                datetime(2026, 10, 14, 9, 0, 0, 0),
            ],
        );

        let counted = Recurrence { count: Some(3), ..Recurrence::every(Frequency::Daily) };
        assert_eq!(counted.occurrences(seed, date(2027, 1, 1), 100).len(), 3);

        let until =
            Recurrence { until: Some(date(2026, 10, 7)), ..Recurrence::every(Frequency::Daily) };
        assert_eq!(until.occurrences(seed, date(2027, 1, 1), 100).len(), 3);

        assert_eq!(
            Recurrence::every(Frequency::Daily).occurrences(seed, date(2030, 1, 1), 10).len(),
            10
        );
    }

    #[test]
    fn a_weekly_rule_that_skips_the_start_day_still_starts_on_it() {
        // Made on a Monday, repeating Tuesdays and Thursdays.
        let seed = datetime(2026, 10, 5, 9, 0, 0, 0);
        let got = weekly(&[Weekday::Tuesday, Weekday::Thursday]).occurrences(
            seed,
            date(2026, 10, 9),
            100,
        );
        assert_eq!(
            got,
            vec![
                datetime(2026, 10, 5, 9, 0, 0, 0),
                datetime(2026, 10, 6, 9, 0, 0, 0),
                datetime(2026, 10, 8, 9, 0, 0, 0),
            ],
        );
    }

    #[test]
    fn the_last_friday_of_the_month_is_found_in_every_month() {
        let seed = datetime(2026, 10, 30, 16, 0, 0, 0);
        let rule = Recurrence {
            weekdays: vec![Weekday::Friday],
            week_of_month: Some(-1),
            ..Recurrence::every(Frequency::Monthly)
        };
        let got = rule.occurrences(seed, date(2027, 1, 31), 100);
        let days: Vec<Date> = got.iter().map(|d| d.date()).collect();
        assert_eq!(
            days,
            vec![date(2026, 10, 30), date(2026, 11, 27), date(2026, 12, 25), date(2027, 1, 29)],
        );
    }

    #[test]
    fn a_rule_reads_as_a_sentence() {
        assert_eq!(Recurrence::every(Frequency::Daily).describe(), "every day");
        assert_eq!(
            Recurrence { interval: 2, ..weekly(&[Weekday::Monday, Weekday::Wednesday]) }.describe(),
            "every 2 weeks on Monday and Wednesday",
        );
        assert_eq!(
            Recurrence {
                weekdays: vec![Weekday::Tuesday],
                week_of_month: Some(2),
                count: Some(5),
                ..Recurrence::every(Frequency::Monthly)
            }
            .describe(),
            "every month on the second Tuesday, 5 times",
        );
    }

    #[test]
    fn wire_names_match_the_serde_representation() {
        for f in Frequency::ALL {
            assert_eq!(serde_json::to_string(&f).unwrap(), format!("\"{}\"", f.as_str()));
        }
        for d in Weekday::ALL {
            assert_eq!(serde_json::to_string(&d).unwrap(), format!("\"{}\"", d.as_str()));
        }
        let rule: Recurrence =
            serde_json::from_value(serde_json::json!({ "frequency": "weekly" })).unwrap();
        assert_eq!(rule, Recurrence::every(Frequency::Weekly), "interval defaults to 1");
    }
}
