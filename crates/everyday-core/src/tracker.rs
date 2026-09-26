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

use crate::id::{EntryId, JournalId, KindId, ReadingId, TrackerId};
use crate::store::trackers::TrackerDay;
use crate::timestamped::Timestamped;
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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

/// The calendar span a [`Target`] is counted over.
///
/// Calendar-aligned rather than rolling: "this week" starts on the week's
/// first day and "this year" on the first of January, because that is how
/// people plan, and a rolling seven days never says when it starts over.
/// Which day a week starts on is a setting of the interface, so anything
/// here that needs it takes it as a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum Period {
    #[default]
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl Period {
    pub const ALL: [Period; 5] =
        [Period::Day, Period::Week, Period::Month, Period::Quarter, Period::Year];

    pub fn as_str(self) -> &'static str {
        match self {
            Period::Day => "day",
            Period::Week => "week",
            Period::Month => "month",
            Period::Quarter => "quarter",
            Period::Year => "year",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }

    /// The first day of the period `day` falls in, with weeks starting on
    /// Monday.
    ///
    /// The interface answers the same question with the reader's own week
    /// start (`periodStart` in `ui/src/lib/habits.ts`), which the vault does
    /// not know: it is a property of a browser's locale, not of anything
    /// stored. This is for the core's own readers -- the assistant and a
    /// dream's digest -- which say which days they counted, so a Sunday-first
    /// reader is never left guessing what "this week" meant.
    pub fn start_of(self, day: Date) -> Date {
        match self {
            Period::Day => day,
            Period::Week => {
                let back = i64::from(day.weekday().to_monday_zero_offset());
                day.checked_sub(jiff::Span::new().days(back)).unwrap_or(day)
            }
            Period::Month => day.first_of_month(),
            Period::Quarter => {
                let first = (day.month() - 1) / 3 * 3 + 1;
                Date::new(day.year(), first, 1).unwrap_or(day)
            }
            Period::Year => Date::new(day.year(), 1, 1).unwrap_or(day),
        }
    }

    /// The last day of the period beginning at `start`, inclusive.
    pub fn end_from(self, start: Date) -> Date {
        match self {
            Period::Day => start,
            Period::Week => start.checked_add(jiff::Span::new().days(6)).unwrap_or(start),
            Period::Month => start.last_of_month(),
            Period::Quarter => Date::new(start.year(), start.month() + 2, 1)
                .map(|d| d.last_of_month())
                .unwrap_or(start),
            Period::Year => Date::new(start.year(), 12, 31).unwrap_or(start),
        }
    }

    /// Roughly how many days a period covers. For turning a window into a
    /// number of periods, never for deciding which period a day is in —
    /// [`start_of`](Period::start_of) is for that.
    pub fn days(self) -> u32 {
        match self {
            Period::Day => 1,
            Period::Week => 7,
            Period::Month => 30,
            Period::Quarter => 91,
            Period::Year => 365,
        }
    }
}

/// What a [`Target`] counts within its period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum Tally {
    /// The period's value under the tracker's [`Aggregate`]: minutes summed,
    /// doses summed, severity averaged. "At most an hour of TV a day."
    #[default]
    Value,
    /// Days in the period with something recorded. "Run three times a
    /// week" — where a 25-minute run and a 60-minute one are both a run.
    ///
    /// The only tally a [`Check`](TrackerKind::Check) has, since a tick has
    /// no value worth adding up.
    Days,
}

/// Where a period's number stands against a [`Target`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Standing {
    /// Below the minimum.
    Short,
    /// Inside the bounds, or there is only a maximum and it is not passed.
    Within,
    /// Past the maximum.
    Over,
}

/// How much, over which period: the intention a tracker is measured against.
///
/// One shape for what used to be two fields. A daily `target` ("30 minutes")
/// was `Target { min: 30, per: Day }`, and a `cadence` ("three times a
/// week") was `Target { min: 3, per: Week, tally: Days }` — and the two
/// between them could not say "at most an hour of TV a day", "between one
/// and two hours of piano a week" or "twelve books this year".
///
/// A bound is inclusive at both ends and at least one is required. A
/// maximum of zero is a real bound ("no TV at all"); a minimum of zero is
/// no bound and is dropped by [`normalize`](Target::normalize).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Target {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default)]
    pub per: Period,
    #[serde(default)]
    pub tally: Tally,
}

impl Target {
    pub fn at_least(min: f64, per: Period) -> Self {
        Self { min: Some(min), max: None, per, tally: Tally::Value }
    }

    pub fn at_most(max: f64, per: Period) -> Self {
        Self { min: None, max: Some(max), per, tally: Tally::Value }
    }

    pub fn between(min: f64, max: f64, per: Period) -> Self {
        Self { min: Some(min), max: Some(max), per, tally: Tally::Value }
    }

    /// Something recorded on at least `times` days of every `per`.
    pub fn days(times: u32, per: Period) -> Self {
        Self { min: Some(f64::from(times)), max: None, per, tally: Tally::Days }
    }

    /// Pull the bounds back into sense, answering whether any are left.
    ///
    /// A bound that is not a finite, non-negative number goes; a minimum of
    /// zero goes, since everything is at least nothing; and bounds given the
    /// wrong way round are swapped, because "between 120 and 60" has only
    /// one thing it can have meant. A target with no bound left is no
    /// target, and the caller drops it.
    pub fn normalize(&mut self) -> bool {
        let sane = |b: Option<f64>| b.filter(|v| v.is_finite() && *v >= 0.0);
        self.min = sane(self.min).filter(|v| *v > 0.0);
        self.max = sane(self.max);
        if let (Some(lo), Some(hi)) = (self.min, self.max)
            && lo > hi
        {
            self.min = Some(hi);
            self.max = Some(lo);
        }
        self.min.is_some() || self.max.is_some()
    }

    /// Where `value` stands. Both bounds are inclusive.
    pub fn standing(&self, value: f64) -> Standing {
        if self.min.is_some_and(|min| value < min) {
            Standing::Short
        } else if self.max.is_some_and(|max| value > max) {
            Standing::Over
        } else {
            Standing::Within
        }
    }

    /// How it reads in a sentence: "at least 12 a year", "between 60 and
    /// 120 min a week", "at most 60 min a day", "at least 3 days a week".
    pub fn describe(&self, unit: &str) -> String {
        // The commonest habit of all, which "at least 1 day a day" is not
        // how anybody says.
        if self.tally == Tally::Days
            && self.per == Period::Day
            && self.min == Some(1.0)
            && self.max.is_none()
        {
            return "every day".into();
        }
        let quantity = |v: f64| match self.tally {
            Tally::Days => {
                let n = format_number(v);
                if (v - 1.0).abs() < f64::EPSILON {
                    format!("{n} day")
                } else {
                    format!("{n} days")
                }
            }
            Tally::Value if unit.is_empty() => format_number(v),
            Tally::Value => format!("{} {unit}", format_number(v)),
        };
        let bound = match (self.min, self.max) {
            (Some(lo), Some(hi)) if (lo - hi).abs() < f64::EPSILON => {
                format!("exactly {}", quantity(lo))
            }
            // The unit once, at the end: "between 60 and 120 min".
            (Some(lo), Some(hi)) => format!("between {} and {}", format_number(lo), quantity(hi)),
            (Some(lo), None) => format!("at least {}", quantity(lo)),
            (None, Some(hi)) => format!("at most {}", quantity(hi)),
            (None, None) => return "no target".into(),
        };
        format!("{bound} a {}", self.per.as_str())
    }
}

/// One target's current period, measured. See [`Tracker::progress`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetProgress {
    /// The period's first and last day. Weeks start on Monday.
    pub from: Date,
    pub to: Date,
    /// So far, up to and including the day asked about.
    pub value: f64,
    pub standing: Standing,
    /// What the minimum would come to by now at an even pace, for a period
    /// longer than a day. `None` where pace means nothing: a daily target,
    /// or one with no minimum. "5 of 12 books" means nothing without it.
    pub on_pace: Option<f64>,
}

/// Where a tracker's numbers come from.
///
/// Most trackers are [`Manual`](TrackerSource::Manual): somebody records a
/// reading. The others are *derived*: their readings are computed, at query
/// time, from records the vault already keeps, which is what lets "twelve
/// books this year" or "an hour or two of piano a week" be a tracker with a
/// target like any other — charted, streaked and asked about by the
/// assistant through the same calls — without anybody writing the same
/// fact down twice.
///
/// Derived readings are never stored, so they cannot go stale when a block
/// is moved or a finish date corrected, and they cannot be written: the
/// vault refuses a reading against a derived tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TrackerSource {
    /// Readings are recorded by hand. Everything before this existed.
    #[default]
    Manual,
    /// Minutes of actual time blocks filed under the tracker's own
    /// [`purpose`](Tracker::purpose), by the block's own pointer or its
    /// task's or project's — the chain the balance report follows. A
    /// tracker filed under a role counts that role's goals too.
    Time,
    /// Library items got to the end of — a finish or a re-read — counted on
    /// the day of the log entry. Every shelf when `kind_id` is `None`.
    ///
    /// Counted by shelf rather than by what is filed under the goal:
    /// "twelve books this year" means every book, and nobody should have to
    /// file each one to have it count.
    Finished {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind_id: Option<KindId>,
    },
}

impl TrackerSource {
    pub fn is_manual(&self) -> bool {
        matches!(self, TrackerSource::Manual)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            TrackerSource::Manual => "manual",
            TrackerSource::Time => "time",
            TrackerSource::Finished { .. } => "finished",
        }
    }
}

/// A thing you have decided to record.
///
/// A record of its own in the vault, like a library
/// [`Kind`](crate::library::Kind) — not a field inside one journal. It was
/// the latter until the purpose domain arrived, and the change is what lets
/// a habit be the *measure* of a goal: "meditate daily" cannot belong to
/// your work journal or your personal one, it belongs to you, and a habits
/// view had to reach through every journal to find it.
///
/// Which journals draw its chip is a per-journal choice, held in
/// [`Journal::shown_trackers`](crate::model::Journal::shown_trackers).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", from = "TrackerRecord")]
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
    /// What this measures, if it measures a goal.
    ///
    /// A run tracker under "run 10k without stopping" is evidence the goal
    /// is alive in a way no task can be: the goal has no work under it and
    /// never will, and the only thing that says it is being pursued is that
    /// the number keeps arriving.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<crate::purpose::Purpose>,
    /// Where the readings come from. See [`TrackerSource`].
    #[serde(default, skip_serializing_if = "TrackerSource::is_manual")]
    pub source: TrackerSource,
    /// What this is meant to come to, if anything: "at least 3 days a
    /// week", "at most 60 min a day", "at least 12 a year".
    ///
    /// Empty means nothing is asked of it — a dose is taken when it is
    /// taken and a symptom is felt when it is felt, and neither has a
    /// streak. More than one is allowed, because "run 30 minutes, three
    /// times a week" is two targets: an amount a day and days a week. The
    /// first is the one a view with room for one draws.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<Target>,
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

/// A tracker as it is read, which is not quite how it is written.
///
/// Every field of [`Tracker`], plus the two a tracker had before
/// [`targets`](Tracker::targets): a daily `target` number and a `cadence`
/// of times per period. A record written by an older build still has them
/// in its sealed payload, and [`From`] folds them into targets — the
/// cadence first, since it was what drove the habit's streak, then the
/// daily amount — so nothing an older build recorded is lost, and nothing
/// downstream of deserialisation ever sees the old shape. They are never
/// written back.
///
/// Deserialisation is the one place every reader passes through: the
/// vault, a backend's own listing, an archive import, a command from the
/// interface. Folding anywhere later would leave one of those behind.
///
/// Built field by field in [`From`], so a field added to [`Tracker`] and
/// forgotten here is a compile error rather than a field that silently
/// never loads.
#[derive(Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
struct TrackerRecord {
    id: TrackerId,
    name: String,
    kind: TrackerKind,
    #[serde(default)]
    icon: String,
    #[serde(default)]
    color: String,
    #[serde(default)]
    unit: String,
    #[serde(default = "one")]
    default_value: f64,
    #[serde(default = "default_scale_max")]
    scale_max: f64,
    #[serde(default)]
    on_calendar: bool,
    #[serde(default)]
    purpose: Option<crate::purpose::Purpose>,
    #[serde(default)]
    source: TrackerSource,
    #[serde(default)]
    targets: Vec<Target>,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    sort_order: i32,
    created_at: Timestamp,
    updated_at: Timestamp,
    /// Before targets: a daily amount.
    #[serde(default)]
    #[cfg_attr(feature = "schema", schemars(skip))]
    target: Option<f64>,
    /// Before targets: `{ times, per }`, a count of days with a reading.
    #[serde(default)]
    #[cfg_attr(feature = "schema", schemars(skip))]
    cadence: Option<LegacyCadence>,
}

#[derive(Deserialize)]
struct LegacyCadence {
    times: u32,
    #[serde(default)]
    per: Period,
}

impl From<TrackerRecord> for Tracker {
    fn from(r: TrackerRecord) -> Self {
        let mut targets = r.targets;
        // Only when the record has no targets of its own: a record with both
        // was written by something that knew about targets, and its targets
        // are the ones it meant.
        if targets.is_empty() {
            if let Some(c) = r.cadence.filter(|c| c.times > 0) {
                targets.push(Target::days(c.times, c.per));
            }
            if let Some(t) = r.target.filter(|t| t.is_finite() && *t > 0.0) {
                targets.push(Target::at_least(t, Period::Day));
            }
        }
        Tracker {
            id: r.id,
            name: r.name,
            kind: r.kind,
            icon: r.icon,
            color: r.color,
            unit: r.unit,
            default_value: r.default_value,
            scale_max: r.scale_max,
            on_calendar: r.on_calendar,
            purpose: r.purpose,
            source: r.source,
            targets,
            archived: r.archived,
            sort_order: r.sort_order,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
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
            scale_max: DEFAULT_SCALE_MAX,
            on_calendar: false,
            purpose: None,
            source: TrackerSource::Manual,
            targets: Vec::new(),
            archived: false,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// Make this a habit, recorded on `times` days of every `per`.
    pub fn every(mut self, times: u32, per: Period) -> Self {
        if times > 0 {
            self.targets.push(Target::days(times, per));
        }
        self
    }

    /// Ask something of it. See [`Target`].
    pub fn aiming(mut self, target: Target) -> Self {
        self.targets.push(target);
        self
    }

    /// Is this a tracker somebody records readings of?
    pub fn is_manual(&self) -> bool {
        self.source.is_manual()
    }

    /// Did this day hold a real value?
    ///
    /// A severity of 0 is a real answer -- "no headache today" -- so a scale
    /// counts any reading. Everywhere else a zero is a check ticked as not
    /// done, and that is not a day it was done. Mirrors `recorded` in
    /// `ui/src/lib/habits.ts`.
    pub fn recorded(&self, day: &TrackerDay) -> bool {
        if self.kind == TrackerKind::Scale { day.count > 0 } else { day.sum > 0.0 }
    }

    /// The number some days come to under a target's tally: days with
    /// something recorded, or the tracker's own aggregate over them -- a
    /// sum, or for a severity the mean of every reading. A check always
    /// counts days. Mirrors `periodValue` in `ui/src/lib/habits.ts`.
    pub fn period_value<'a>(
        &self,
        days: impl IntoIterator<Item = &'a TrackerDay>,
        tally: Tally,
    ) -> f64 {
        let days: Vec<&TrackerDay> = days.into_iter().collect();
        if tally == Tally::Days || self.kind == TrackerKind::Check {
            let mut hit: Vec<Date> =
                days.iter().filter(|d| self.recorded(d)).map(|d| d.date).collect();
            hit.sort_unstable();
            hit.dedup();
            return hit.len() as f64;
        }
        let sum: f64 = days.iter().map(|d| d.sum).sum();
        if self.kind.aggregate() != Aggregate::Mean {
            return sum;
        }
        let count: u32 = days.iter().map(|d| d.count).sum();
        if count == 0 { 0.0 } else { sum / f64::from(count) }
    }

    /// How `target`'s current period stands on `today`, from this tracker's
    /// days. `days` must reach back at least to the period's first day.
    /// Mirrors `progress` in `ui/src/lib/habits.ts`, bar the week start.
    pub fn progress(&self, target: &Target, days: &[TrackerDay], today: Date) -> TargetProgress {
        let from = target.per.start_of(today);
        let to = target.per.end_from(from);
        let value = self.period_value(
            days.iter().filter(|d| d.tracker_id == self.id && d.date >= from && d.date <= today),
            target.tally,
        );
        let span = |a: Date, b: Date| ((b - a).get_days() + 1).max(1) as f64;
        let on_pace = match (target.per, target.min) {
            (Period::Day, _) | (_, None) => None,
            (_, Some(min)) => Some(min * span(from, today) / span(from, to)),
        };
        TargetProgress { from, to, value, standing: target.standing(value), on_pace }
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
        // A derived tracker's readings are a sum of something: minutes, or
        // things finished. Nothing is ever recorded at a minute, so there is
        // nothing to put on the calendar either.
        if !self.is_manual() {
            self.kind = TrackerKind::Amount;
            self.on_calendar = false;
        }
        match self.source {
            TrackerSource::Time => self.unit = "min".into(),
            TrackerSource::Finished { .. } => self.unit = String::new(),
            TrackerSource::Manual => {}
        }
        if self.kind == TrackerKind::Check {
            self.unit = String::new();
        }
        // A tick has no value worth adding up, so a check counts days. A
        // derived tracker is the other way round: one reading a day, which
        // is its whole value, so "days with something" is not a separate
        // question -- and "60 days a week" is a target nobody can meet.
        let forced = match (self.kind, self.is_manual()) {
            (_, false) => Some(Tally::Value),
            (TrackerKind::Check, true) => Some(Tally::Days),
            _ => None,
        };
        self.targets.retain_mut(|t| {
            if let Some(tally) = forced {
                t.tally = tally;
            }
            t.normalize()
        });
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

impl Timestamped for Tracker {
    fn touch(&mut self) {
        self.updated_at = Timestamp::now();
    }
}

/// One recorded value: this tracker, this much, then.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Reading {
    pub id: ReadingId,
    /// The journal whose page this was ticked on, if it was ticked on one.
    ///
    /// `None` for a reading logged from the Overview, or from the tray,
    /// where there is no journal in view. It used to be required, because a
    /// tracker was a field inside one journal and so every reading had one;
    /// the tracker is a vault record now and this is provenance rather than
    /// ownership — exactly what [`entry_id`](Reading::entry_id) already is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_id: Option<JournalId>,
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
    /// A reading filed under `date`, with no time of day and no journal.
    ///
    /// Use [`in_journal`](Reading::in_journal) to say where it was ticked,
    /// the way [`with_entry`](Reading::with_entry) says which entry it sat
    /// beside. Neither is required.
    pub fn on(tracker_id: TrackerId, date: Date, value: f64) -> Self {
        let now = Timestamp::now();
        Self {
            id: ReadingId::new(),
            journal_id: None,
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
    pub fn at(tracker_id: TrackerId, at: Timestamp, tz: &str, value: f64) -> Self {
        let date = crate::model::local_date_in(at, tz);
        Self { at: Some(at), tz: tz.to_string(), ..Self::on(tracker_id, date, value) }
    }

    /// Record which journal's page this was ticked on.
    pub fn in_journal(mut self, journal: JournalId) -> Self {
        self.journal_id = Some(journal);
        self
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

impl Timestamped for Reading {
    fn touch(&mut self) {
        self.updated_at = Timestamp::now();
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
    fn a_target_with_no_bound_left_is_no_target() {
        // Everything is at least nothing, so a minimum of zero says nothing
        // -- and a target that says nothing would count every period as met.
        let mut nothing = Target::at_least(0.0, Period::Week);
        assert!(!nothing.normalize());
        let mut nonsense = Target::at_most(f64::NAN, Period::Day);
        assert!(!nonsense.normalize());

        // A maximum of zero is a real bound: "no TV at all".
        let mut none_at_all = Target::at_most(0.0, Period::Day);
        assert!(none_at_all.normalize());
        assert_eq!(none_at_all.max, Some(0.0));
    }

    #[test]
    fn bounds_the_wrong_way_round_are_swapped_not_refused() {
        let mut t = Target::between(120.0, 60.0, Period::Week);
        assert!(t.normalize());
        assert_eq!((t.min, t.max), (Some(60.0), Some(120.0)));
    }

    #[test]
    fn a_value_stands_short_within_or_over_and_the_bounds_are_inclusive() {
        let piano = Target::between(60.0, 120.0, Period::Week);
        assert_eq!(piano.standing(59.0), Standing::Short);
        assert_eq!(piano.standing(60.0), Standing::Within);
        assert_eq!(piano.standing(120.0), Standing::Within);
        assert_eq!(piano.standing(121.0), Standing::Over);

        // A limit alone is never short: no TV is well within an hour.
        let tv = Target::at_most(60.0, Period::Day);
        assert_eq!(tv.standing(0.0), Standing::Within);
        assert_eq!(tv.standing(61.0), Standing::Over);

        // More than asked for is still met: three runs a week does not
        // become a failure because you went four times.
        assert_eq!(Target::days(3, Period::Week).standing(4.0), Standing::Within);
    }

    #[test]
    fn a_target_reads_as_a_sentence() {
        assert_eq!(Target::at_least(12.0, Period::Year).describe(""), "at least 12 a year");
        assert_eq!(
            Target::between(60.0, 120.0, Period::Week).describe("min"),
            "between 60 and 120 min a week"
        );
        assert_eq!(Target::at_most(60.0, Period::Day).describe("min"), "at most 60 min a day");
        assert_eq!(Target::days(3, Period::Week).describe("min"), "at least 3 days a week");
        assert_eq!(Target::days(1, Period::Day).describe(""), "every day");
    }

    #[test]
    fn an_old_record_keeps_its_cadence_and_its_daily_goal_as_targets() {
        // What a tracker looked like before targets: a daily amount and
        // three times a week, both in its sealed payload. Neither may be
        // lost, and the cadence comes first because it drove the streak.
        let old = serde_json::json!({
            "id": TrackerId::new(),
            "name": "Run",
            "kind": "amount",
            "unit": "min",
            "target": 30.0,
            "cadence": { "times": 3, "per": "week" },
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z",
        });
        let t: Tracker = serde_json::from_value(old).unwrap();
        assert_eq!(
            t.targets,
            vec![Target::days(3, Period::Week), Target::at_least(30.0, Period::Day)]
        );

        // ...and the old names are never written back.
        let back = serde_json::to_value(&t).unwrap();
        assert!(back.get("target").is_none() && back.get("cadence").is_none());
        let again: Tracker = serde_json::from_value(back).unwrap();
        assert_eq!(again, t, "a second round trip changes nothing");
    }

    #[test]
    fn a_record_with_targets_ignores_whatever_old_fields_it_still_carries() {
        let json = serde_json::json!({
            "id": TrackerId::new(),
            "name": "TV",
            "kind": "amount",
            "target": 30.0,
            "targets": [{ "max": 60.0, "per": "day", "tally": "value" }],
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z",
        });
        let t: Tracker = serde_json::from_value(json).unwrap();
        assert_eq!(t.targets, vec![Target::at_most(60.0, Period::Day)]);
    }

    #[test]
    fn a_manual_tracker_with_no_targets_is_written_as_it_always_was() {
        // The frozen vault's golden dump holds a tracker like this one, and
        // it must not change: nothing new is written for a tracker that uses
        // nothing new.
        let t = Tracker::new("Floss", TrackerKind::Check);
        let json = serde_json::to_value(&t).unwrap();
        assert!(json.get("source").is_none());
        assert!(json.get("targets").is_none());
    }

    #[test]
    fn a_derived_tracker_is_an_uncalendared_amount_whatever_it_was_saved_as() {
        let mut t = Tracker::new("Time on piano", TrackerKind::Check);
        t.source = TrackerSource::Time;
        t.on_calendar = true;
        t.normalize();
        assert_eq!(t.kind, TrackerKind::Amount);
        assert_eq!(t.unit, "min");
        assert!(!t.on_calendar);

        let mut books = Tracker::new("Books", TrackerKind::Amount).with_unit("pages");
        books.source = TrackerSource::Finished { kind_id: None };
        books.normalize();
        assert_eq!(books.unit, "", "a count of things finished has no unit");

        // Counting days of a derived tracker is its value by another name,
        // and "60 days a week" of time would be a target nobody can meet.
        let mut time = Tracker::new("Time on piano", TrackerKind::Amount)
            .aiming(Target { tally: Tally::Days, ..Target::at_least(60.0, Period::Week) });
        time.source = TrackerSource::Time;
        time.normalize();
        assert_eq!(time.targets[0].tally, Tally::Value);
    }

    #[test]
    fn a_check_counts_days_and_drops_targets_with_nothing_in_them() {
        let mut t = Tracker::new("Floss", TrackerKind::Check)
            .aiming(Target::at_least(5.0, Period::Week))
            .aiming(Target::at_least(0.0, Period::Day));
        t.normalize();
        assert_eq!(t.targets, vec![Target::days(5, Period::Week)]);
    }

    #[test]
    fn a_source_reads_the_way_it_is_written() {
        let kind = KindId::new();
        for source in [
            TrackerSource::Manual,
            TrackerSource::Time,
            TrackerSource::Finished { kind_id: Some(kind) },
        ] {
            let json = serde_json::to_value(source).unwrap();
            assert_eq!(serde_json::from_value::<TrackerSource>(json).unwrap(), source);
        }
        assert_eq!(
            serde_json::to_value(TrackerSource::Finished { kind_id: Some(kind) }).unwrap(),
            serde_json::json!({ "type": "finished", "kindId": kind }),
        );
    }

    #[test]
    fn a_period_is_the_calendar_one_the_day_falls_in() {
        // 2026-09-09 is a Wednesday. The same cases `overview.test.mjs`
        // pins for the interface, with a Monday week.
        let wed = Date::constant(2026, 9, 9);
        let span = |p: Period| (p.start_of(wed), p.end_from(p.start_of(wed)));
        assert_eq!(span(Period::Day), (wed, wed));
        assert_eq!(span(Period::Week), (Date::constant(2026, 9, 7), Date::constant(2026, 9, 13)));
        assert_eq!(span(Period::Month), (Date::constant(2026, 9, 1), Date::constant(2026, 9, 30)));
        assert_eq!(
            span(Period::Quarter),
            (Date::constant(2026, 7, 1), Date::constant(2026, 9, 30))
        );
        assert_eq!(span(Period::Year), (Date::constant(2026, 1, 1), Date::constant(2026, 12, 31)));
        // A Monday is the start of its own week, and a Sunday the end.
        assert_eq!(Period::Week.start_of(Date::constant(2026, 9, 7)), Date::constant(2026, 9, 7));
        assert_eq!(Period::Week.start_of(Date::constant(2026, 9, 13)), Date::constant(2026, 9, 7));
        assert_eq!(
            Period::Quarter.end_from(Date::constant(2027, 1, 1)),
            Date::constant(2027, 3, 31)
        );
    }

    fn day(tracker: TrackerId, d: i8, count: u32, sum: f64) -> TrackerDay {
        TrackerDay {
            tracker_id: tracker,
            date: Date::constant(2026, 9, d),
            count,
            sum,
            max: sum,
            first_at: None,
            last_at: None,
        }
    }

    #[test]
    fn progress_counts_the_current_period_so_far_and_says_what_pace_would_be() {
        let mut books = Tracker::new("Books", TrackerKind::Amount);
        books.source = TrackerSource::Finished { kind_id: None };
        let year = Target::at_least(12.0, Period::Year);
        let p = books.progress(&year, &[day(books.id, 2, 1, 1.0)], Date::constant(2026, 9, 26));
        assert_eq!((p.from, p.to), (Date::constant(2026, 1, 1), Date::constant(2026, 12, 31)));
        assert_eq!(p.value, 1.0);
        assert_eq!(p.standing, Standing::Short);
        // 269 of 365 days gone: nearly nine would be on pace.
        assert_eq!((p.on_pace.unwrap() * 10.0).round() / 10.0, 8.8);

        let tv = Tracker::new("TV", TrackerKind::Amount).with_unit("min");
        let limit = Target::at_most(60.0, Period::Day);
        let over = tv.progress(&limit, &[day(tv.id, 26, 2, 80.0)], Date::constant(2026, 9, 26));
        assert_eq!(over.standing, Standing::Over);
        assert_eq!(over.on_pace, None, "a day has no pace");
    }

    #[test]
    fn a_week_of_days_counts_days_not_readings_and_a_not_done_tick_is_no_day() {
        let floss = Tracker::new("Floss", TrackerKind::Check);
        let days = [day(floss.id, 7, 3, 3.0), day(floss.id, 8, 1, 0.0), day(floss.id, 9, 1, 1.0)];
        let p = floss.progress(&Target::days(3, Period::Week), &days, Date::constant(2026, 9, 9));
        assert_eq!(p.value, 2.0, "three ticks on Monday are one day; Tuesday's was a no");
        assert_eq!(p.standing, Standing::Short);
    }

    #[test]
    fn a_severity_averages_every_reading_in_the_period() {
        let pain = Tracker::new("Headache", TrackerKind::Scale);
        let days = [day(pain.id, 7, 2, 10.0), day(pain.id, 8, 1, 2.0)];
        assert_eq!(pain.period_value(&days, Tally::Value), 4.0);
    }

    #[test]
    fn periods_round_trip_through_their_names() {
        for p in Period::ALL {
            assert_eq!(Period::parse(p.as_str()), Some(p));
        }
        assert_eq!(Period::parse("fortnight"), None);
    }

    #[test]
    fn a_reading_need_not_name_a_journal() {
        // What the Overview's quick-track produces: a real reading of a
        // vault-level tracker, logged from no journal page at all.
        let date = jiff::civil::date(2026, 3, 1);
        let r = Reading::on(TrackerId::new(), date, 60.0);
        assert_eq!(r.journal_id, None);
        assert_eq!(r.entry_id, None, "and no entry either");

        let filed = r.clone().in_journal(JournalId::new());
        assert!(filed.journal_id.is_some());
        assert_eq!(filed.value, r.value, "saying where it was ticked changes nothing else");
    }

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
        t.targets = vec![Target::at_least(f64::INFINITY, Period::Day)];
        t.icon = String::new();
        t.normalize();
        assert_eq!(t.name, "Water");
        assert_eq!(t.unit, "glasses");
        assert_eq!(t.default_value, 1.0);
        assert_eq!(t.scale_max, DEFAULT_SCALE_MAX);
        assert!(t.targets.is_empty());
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
        let t = TrackerId::new();
        let date = Date::constant(2026, 3, 14);
        assert_eq!(Reading::on(t, date, 1.0).minute_of_day(), None);

        // 07:30 in Berlin is 06:30 UTC; the reading knows which it means.
        let at: Timestamp = "2026-03-14T06:30:00Z".parse().unwrap();
        let timed = Reading::at(t, at, "Europe/Berlin", 45.0);
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
