//! The tracking domain: what you record *alongside* an entry.
//!
//! A journal is prose, and prose does not aggregate. "Slept badly again,
//! took the ibuprofen around eight" is the sentence you want to write and
//! exactly the sentence nobody can plot. This domain is the other half of
//! that page — the handful of numbers the day also produced.
//!
//! ```text
//!   Journal ── Tracker (a definition: what I decided to record)
//!                 │
//!                 └── Reading, Reading, Reading …   (one recorded value)
//! ```
//!
//! # One shape for four questions
//!
//! Habits, doses, symptoms and counts look like four different things and
//! are stored as one: a [`Reading`] is a tracker, an instant and a single
//! `f64`. [`TrackerKind`] changes how the interface *collects* that number
//! and how a chart should *aggregate* it, and changes nothing about how it
//! is stored.
//!
//! | Kind | `value` means | absent means |
//! |---|---|---|
//! | [`Check`](TrackerKind::Check) | `1` done, `0` deliberately not done | not recorded |
//! | [`Dose`](TrackerKind::Dose) | the dose taken, in the tracker's unit; one reading per dose | none taken |
//! | [`Scale`](TrackerKind::Scale) | severity, `0..=scale_max` | no symptom recorded |
//! | [`Amount`](TrackerKind::Amount) | the number, in the tracker's unit | nothing recorded |
//!
//! One numeric column and one timestamp is what makes "average pain by
//! weekday", "current streak" and "minutes exercised per week" ordinary
//! queries later, instead of four parallel schemas each needing their own.
//!
//! # Definitions live in the journal, readings live in the store
//!
//! [`Tracker`] is a field on [`Journal`](crate::model::Journal), sealed with
//! the rest of that record — so a tracker's *name* is as private as the
//! entries beside it. [`Reading`] is a row of its own in the backend, with
//! its date, its instant and its value in the clear, because a year of them
//! has to be scannable without decrypting a year of them. What the index
//! leaks is that tracker `7f3a…` was `500` at 08:12; what it never says is
//! that `7f3a…` is sertraline.
//!
//! # `at` is an `Option`, and that is the point
//!
//! A reading always knows its **day**. It only sometimes knows its
//! **minute**: ticking "flossed" while writing up yesterday evening records
//! a real fact about yesterday and nothing at all about 23:04. Storing a
//! default time in that case would put a pin on the calendar at an hour
//! nothing happened, and would quietly poison the first interesting question
//! anyone asks of this data — *when* do the migraines start? So `at` is
//! `None` there, timed readings are drawn on the grid, untimed ones in the
//! all-day band, and an hour-of-day query filters the unknowns out rather
//! than averaging them in.

use crate::id::{EntryId, JournalId, ReadingId, TrackerId};
use jiff::{Timestamp, civil::Date};
use serde::{Deserialize, Serialize};

/// What sort of thing is being recorded.
///
/// A closed set of four, chosen to cover what a day actually produces: a
/// thing you did or didn't do, a substance you took, a symptom you felt, and
/// a quantity you accumulated. User-defined kinds were considered and
/// rejected for the reason [`TaskStatus`](crate::task::TaskStatus) is closed
/// too — an open set makes cross-journal analysis unanswerable, and the
/// escape hatch for anything unusual is [`Amount`](TrackerKind::Amount) with
/// a unit of your choosing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TrackerKind {
    /// Done or not done. Habits: floss, walk the dog, no phone in bed.
    #[default]
    Check,
    /// Something taken, in a dose. Medication and supplements.
    ///
    /// Several a day is normal and each is its own reading, so "did I take
    /// the second one" is answerable and the day's total is a sum.
    Dose,
    /// Something felt, on a `0..=scale_max` severity scale. Pain, symptoms,
    /// mood. Several a day is normal here too — a headache that eases is two
    /// readings, not one overwritten.
    Scale,
    /// A quantity. Minutes exercised, pages read, glasses of water.
    Amount,
}

impl TrackerKind {
    pub const ALL: [TrackerKind; 4] =
        [TrackerKind::Check, TrackerKind::Dose, TrackerKind::Scale, TrackerKind::Amount];

    pub fn as_str(self) -> &'static str {
        match self {
            TrackerKind::Check => "check",
            TrackerKind::Dose => "dose",
            TrackerKind::Scale => "scale",
            TrackerKind::Amount => "amount",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// How a day's — or a week's — readings should be combined.
    ///
    /// This is the one piece of per-kind knowledge a chart cannot guess.
    /// Summing severities would be nonsense (a 3 in the morning and a 3 at
    /// night is not a 6), and averaging doses would hide that you took two.
    pub fn aggregate(self) -> Aggregate {
        match self {
            TrackerKind::Check => Aggregate::Count,
            TrackerKind::Dose | TrackerKind::Amount => Aggregate::Sum,
            TrackerKind::Scale => Aggregate::Mean,
        }
    }

    /// Whether a reading of this kind carries a number worth showing. A
    /// check's `1` is a tick, not a quantity.
    pub fn has_value(self) -> bool {
        self != TrackerKind::Check
    }
}

/// How readings combine over a period. See [`TrackerKind::aggregate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Aggregate {
    /// How many times it happened.
    Count,
    /// Added up: 400 mg + 400 mg = 800 mg.
    Sum,
    /// Averaged: two headaches of 3 and 7 is a day averaging 5.
    Mean,
}

/// Top of a [`Scale`](TrackerKind::Scale) when the tracker does not say.
/// Zero to ten is the scale every clinician already asks in.
pub const DEFAULT_SCALE_MAX: f64 = 10.0;

/// A thing you have decided to record in a journal.
///
/// Lives in [`Journal::trackers`](crate::model::Journal::trackers), so
/// "which journal" and "what does it track" are one record and one save.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tracker {
    pub id: TrackerId,
    pub name: String,
    pub kind: TrackerKind,
    /// A name from the interface's tracker icon set, e.g. `pill`. Unknown
    /// names fall back to a dot rather than to nothing, so a vault written
    /// by a later build with more icons still draws.
    #[serde(default)]
    pub icon: String,
    /// `#rrggbb`. Unlike the calendar — where colour is spoken for — a
    /// tracker's colour is its own identity in a row of chips.
    #[serde(default)]
    pub color: String,
    /// Shown after the value: `mg`, `min`, `pages`. Empty for a check.
    #[serde(default)]
    pub unit: String,
    /// What the input prefills, and the step its plus/minus buttons take:
    /// the usual dose, the usual session. Ignored by
    /// [`Check`](TrackerKind::Check).
    #[serde(default = "one")]
    pub default_value: f64,
    /// A daily goal, if there is one. Drives the progress ring on the chip
    /// and nothing else — missing it is information, not an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<f64>,
    /// Top of the severity range for a [`Scale`](TrackerKind::Scale).
    #[serde(default = "default_scale_max")]
    pub scale_max: f64,
    /// Draw this tracker's readings on the calendar.
    ///
    /// Off by default, and per tracker rather than per kind, because the
    /// question it answers is "is the *time* of this real?". A migraine at
    /// 14:20 belongs on a grid. "Flossed", ticked at bedtime for the whole
    /// day, is a pin at an hour that means nothing.
    #[serde(default)]
    pub on_calendar: bool,
    /// Retired: kept for its history, gone from the day's chips.
    ///
    /// The alternative — deleting the tracker — would take a year of
    /// readings with it, which is the wrong answer to "I stopped taking
    /// this in March".
    #[serde(default)]
    pub archived: bool,
    /// Manual ordering within the journal.
    #[serde(default)]
    pub sort_order: i32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

fn one() -> f64 {
    1.0
}

fn default_scale_max() -> f64 {
    DEFAULT_SCALE_MAX
}

/// The palette tracker chips are coloured from. Deliberately not
/// [`DEFAULT_JOURNAL_COLORS`](crate::model::DEFAULT_JOURNAL_COLORS): these
/// sit several to a row at small sizes and need to stay apart from each
/// other at a glance, so they are spaced around the wheel rather than
/// chosen to be sober.
pub const TRACKER_COLORS: &[&str] = &[
    "#e11d48", // rose
    "#f97316", // orange
    "#f59e0b", // amber
    "#16a34a", // green
    "#0d9488", // teal
    "#0284c7", // sky
    "#4f46e5", // indigo
    "#9333ea", // violet
    "#db2777", // pink
    "#78716c", // stone
];

impl Tracker {
    pub fn new(name: impl Into<String>, kind: TrackerKind) -> Self {
        let now = Timestamp::now();
        Self {
            id: TrackerId::new(),
            name: name.into(),
            kind,
            icon: "dot".into(),
            color: TRACKER_COLORS[0].to_string(),
            unit: String::new(),
            default_value: 1.0,
            target: None,
            scale_max: DEFAULT_SCALE_MAX,
            on_calendar: false,
            archived: false,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = unit.into();
        self
    }

    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = icon.into();
        self
    }

    /// Trim the strings and pull the numbers back into their ranges.
    ///
    /// Called on every save rather than trusted from the interface, because
    /// "the value is finite and the scale is at least 1" is the assumption
    /// every reader downstream makes — a `NaN` target would propagate
    /// silently into a chart axis and take the whole view with it.
    pub fn normalize(&mut self) {
        self.name = self.name.trim().to_string();
        self.unit = self.unit.trim().to_string();
        self.icon = self.icon.trim().to_string();
        if self.icon.is_empty() {
            self.icon = "dot".into();
        }
        if !self.default_value.is_finite() || self.default_value <= 0.0 {
            self.default_value = 1.0;
        }
        if !self.scale_max.is_finite() || self.scale_max < 1.0 {
            self.scale_max = DEFAULT_SCALE_MAX;
        }
        self.scale_max = self.scale_max.min(100.0);
        self.target = self.target.filter(|t| t.is_finite() && *t > 0.0);
        if self.kind == TrackerKind::Check {
            self.unit = String::new();
        }
    }

    /// Clamp a value to what this tracker can mean.
    pub fn clamp(&self, value: f64) -> f64 {
        if !value.is_finite() {
            return 0.0;
        }
        match self.kind {
            TrackerKind::Check => {
                if value > 0.0 {
                    1.0
                } else {
                    0.0
                }
            }
            TrackerKind::Scale => value.clamp(0.0, self.scale_max),
            _ => value.max(0.0),
        }
    }

    /// How the interface writes one reading of this tracker: `500 mg`,
    /// `6/10`, `Done`.
    pub fn format_value(&self, value: f64) -> String {
        match self.kind {
            TrackerKind::Check => {
                if value > 0.0 {
                    "Done".into()
                } else {
                    "Not done".into()
                }
            }
            TrackerKind::Scale => {
                format!("{}/{}", format_number(value), format_number(self.scale_max))
            }
            _ if self.unit.is_empty() => format_number(value),
            _ => format!("{} {}", format_number(value), self.unit),
        }
    }

    /// Minutes this tracker's value represents, if its unit is a time.
    ///
    /// This is what lets "45 min run, at 07:00" draw as 07:00–07:45 on the
    /// calendar instead of a dot. Every other tracker is a moment, because
    /// every other tracker *is* one: a 500 mg dose has no length.
    pub fn duration_minutes(&self, value: f64) -> Option<f64> {
        if self.kind != TrackerKind::Amount {
            return None;
        }
        let minutes = match self.unit.trim().to_lowercase().as_str() {
            "min" | "mins" | "minute" | "minutes" => value,
            "h" | "hr" | "hrs" | "hour" | "hours" => value * 60.0,
            _ => return None,
        };
        (minutes.is_finite() && minutes > 0.0).then_some(minutes)
    }
}

/// One recorded value: this tracker, this much, then.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reading {
    pub id: ReadingId,
    /// The journal whose settings define [`tracker_id`](Reading::tracker_id).
    pub journal_id: JournalId,
    pub tracker_id: TrackerId,
    /// The entry it was recorded beside, if there was one. Readings do not
    /// need an entry — logging a supplement should not oblige anyone to
    /// write a paragraph — so this is how the two are linked when they were
    /// in fact recorded together, and nothing more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<EntryId>,
    /// The day this is filed under, in the author's local time. Always
    /// known; it is what every date-range query is built on.
    pub local_date: Date,
    /// When it actually happened. `None` means "that day, time unknown" —
    /// see the module docs for why this is not defaulted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
    /// IANA time zone `at` was recorded in, e.g. `Europe/Berlin`.
    #[serde(default = "utc")]
    pub tz: String,
    /// See [`TrackerKind`] for what this means per kind.
    pub value: f64,
    #[serde(default)]
    pub note: String,
    /// When it was written down, as opposed to when it happened. "Took it at
    /// eight, remembered to log it at eleven" survives as both.
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

fn utc() -> String {
    "UTC".to_string()
}

impl Reading {
    /// A reading filed under `date`, with no time of day.
    pub fn on(journal_id: JournalId, tracker_id: TrackerId, date: Date, value: f64) -> Self {
        let now = Timestamp::now();
        Self {
            id: ReadingId::new(),
            journal_id,
            tracker_id,
            entry_id: None,
            local_date: date,
            at: None,
            tz: "UTC".into(),
            value,
            note: String::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// A reading of something that happened at `at`, filed under the day
    /// `at` falls on in `tz`.
    pub fn at(
        journal_id: JournalId,
        tracker_id: TrackerId,
        at: Timestamp,
        tz: &str,
        value: f64,
    ) -> Self {
        let date = crate::model::local_date_in(at, tz);
        Self { at: Some(at), tz: tz.to_string(), ..Self::on(journal_id, tracker_id, date, value) }
    }

    pub fn with_entry(mut self, entry: EntryId) -> Self {
        self.entry_id = Some(entry);
        self
    }

    /// Minutes from midnight local, or `None` when the time is unknown.
    /// What the calendar grid positions by.
    pub fn minute_of_day(&self) -> Option<i32> {
        let at = self.at?;
        let zoned = at.in_tz(&self.tz).unwrap_or_else(|_| at.in_tz("UTC").expect("UTC exists"));
        Some(i32::from(zoned.hour()) * 60 + i32::from(zoned.minute()))
    }
}

/// A number without a pointless `.0`, and without eleven decimal places
/// either. Doses are written `0.5`, minutes are written `45`.
pub fn format_number(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    if (v - v.round()).abs() < f64::EPSILON {
        return format!("{}", v.round() as i64);
    }
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_lose_their_trailing_zeroes() {
        assert_eq!(format_number(45.0), "45");
        assert_eq!(format_number(0.5), "0.5");
        assert_eq!(format_number(1.25), "1.25");
        assert_eq!(format_number(f64::NAN), "0");
    }

    #[test]
    fn values_are_formatted_per_kind() {
        let check = Tracker::new("Floss", TrackerKind::Check);
        assert_eq!(check.format_value(1.0), "Done");
        assert_eq!(check.format_value(0.0), "Not done");

        let dose = Tracker::new("Ibuprofen", TrackerKind::Dose).with_unit("mg");
        assert_eq!(dose.format_value(400.0), "400 mg");

        let mut pain = Tracker::new("Headache", TrackerKind::Scale);
        pain.scale_max = 10.0;
        assert_eq!(pain.format_value(6.0), "6/10");
    }

    #[test]
    fn normalize_pulls_nonsense_back_into_range() {
        let mut t = Tracker::new("  Water  ", TrackerKind::Amount);
        t.unit = " glasses ".into();
        t.default_value = -3.0;
        t.scale_max = f64::NAN;
        t.target = Some(f64::INFINITY);
        t.icon = String::new();
        t.normalize();
        assert_eq!(t.name, "Water");
        assert_eq!(t.unit, "glasses");
        assert_eq!(t.default_value, 1.0);
        assert_eq!(t.scale_max, DEFAULT_SCALE_MAX);
        assert_eq!(t.target, None);
        assert_eq!(t.icon, "dot");
    }

    #[test]
    fn a_check_has_no_unit_however_hard_you_try() {
        let mut t = Tracker::new("Floss", TrackerKind::Check).with_unit("mg");
        t.normalize();
        assert_eq!(t.unit, "");
    }

    #[test]
    fn values_are_clamped_to_what_the_kind_can_mean() {
        let check = Tracker::new("Floss", TrackerKind::Check);
        assert_eq!(check.clamp(7.0), 1.0);
        assert_eq!(check.clamp(-1.0), 0.0);

        let mut pain = Tracker::new("Headache", TrackerKind::Scale);
        pain.scale_max = 10.0;
        assert_eq!(pain.clamp(99.0), 10.0);
        assert_eq!(pain.clamp(f64::NAN), 0.0);
    }

    #[test]
    fn only_time_units_become_calendar_spans() {
        let run = Tracker::new("Run", TrackerKind::Amount).with_unit("min");
        assert_eq!(run.duration_minutes(45.0), Some(45.0));

        let long = Tracker::new("Sleep", TrackerKind::Amount).with_unit("hours");
        assert_eq!(long.duration_minutes(7.5), Some(450.0));

        let pages = Tracker::new("Reading", TrackerKind::Amount).with_unit("pages");
        assert_eq!(pages.duration_minutes(30.0), None);

        // A dose is a moment however it is measured.
        let dose = Tracker::new("Melatonin", TrackerKind::Dose).with_unit("min");
        assert_eq!(dose.duration_minutes(45.0), None);
    }

    #[test]
    fn an_undated_reading_has_no_minute_and_says_so() {
        let j = JournalId::new();
        let t = TrackerId::new();
        let date = Date::constant(2026, 3, 14);
        assert_eq!(Reading::on(j, t, date, 1.0).minute_of_day(), None);

        // 07:30 in Berlin is 06:30 UTC; the reading knows which it means.
        let at: Timestamp = "2026-03-14T06:30:00Z".parse().unwrap();
        let timed = Reading::at(j, t, at, "Europe/Berlin", 45.0);
        assert_eq!(timed.minute_of_day(), Some(7 * 60 + 30));
        assert_eq!(timed.local_date, date);
    }

    #[test]
    fn kinds_round_trip_through_their_names() {
        for kind in TrackerKind::ALL {
            assert_eq!(TrackerKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(TrackerKind::parse("nonsense"), None);
    }
}
