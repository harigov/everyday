//! Karma: what a day's deeds come to, for better and for worse.
//!
//! The application's points, and the thing its ranks will one day be built
//! on. Two decisions shape everything here, and both were the owner's:
//!
//! * **Each app decides what earns Karma**, and Karma must not be easy to
//!   earn across apps. So every source has its own cap, a day has a ceiling
//!   ([`DAY_CEILING`]: a hundred is a well-lived day), and the busywork that
//!   would otherwise farm points -- ten trivial tasks ticked in a minute --
//!   runs into its source's cap long before it reaches the day's.
//! * **Karma can be lost.** A habit somebody is trying to break -- a drink,
//!   a cigarette, an hour past the limit on a screen -- costs Karma on every
//!   day it happens, and a loss is never capped: a well-lived morning does
//!   not buy an evening's binge back.
//!
//! # Derived, not stored
//!
//! A day's Karma is worked out from records the vault already keeps --
//! tasks finished, trackers recorded, journal entries written, time filed
//! under a goal, things got to the end of -- at the moment it is asked for,
//! the way a derived tracker's readings are. Nothing is written per deed, so
//! nothing can go stale when a task is reopened or a reading corrected.
//!
//! What *is* stored is [`KarmaBook::since`]: the day Karma started. Karma
//! starts at zero (the owner's call -- no backfill from history), so a deed
//! dated before that day counts for nothing, however it got into the vault.
//! That also keeps an import of ten years of somebody else's journal from
//! arriving as ten years of Karma.
//!
//! The rules are pure functions of their inputs, here and tested here. The
//! vault's half -- reading the records and handing them over -- is
//! `Vault::karma`.

use crate::store::trackers::TrackerDay;
use crate::tracker::{Period, Standing, Tally, Tracker, TrackerKind, TrackerSource};
use jiff::civil::Date;
use serde::{Deserialize, Serialize};

/// Which version of the rules below a report was scored under. Bumped when a
/// rule changes what a day is worth, so a reader can tell two reports apart.
pub const RULES_VERSION: u32 = 1;

/// The most a day can earn, after every source's own cap. A hundred is a
/// well-lived day; there is no such thing as a two-hundred day.
pub const DAY_CEILING: i32 = 100;

/// A finished task: five, and five more if it was filed under a goal or an
/// area -- work towards something is worth more than work.
pub const TASK_POINTS: i32 = 5;
pub const TASK_PURPOSE_BONUS: i32 = 5;
/// At most this from tasks in one day.
pub const TASK_CAP: i32 = 40;

/// What a tracker earns or costs on a day it counts, when it does not say.
pub const HABIT_DEFAULT: i32 = 5;
/// At most this from habits in one day. Losses are never capped.
pub const HABIT_CAP: i32 = 40;
/// The most one tracker can be set to earn or cost in a day, either way.
pub const HABIT_LIMIT: i32 = 50;

/// Writing in the journal on the day: once a day, however many entries.
pub const JOURNAL_POINTS: i32 = 10;

/// Each whole hour of recorded time filed under a goal or an area.
pub const TIME_POINTS_PER_HOUR: i32 = 5;
pub const TIME_CAP: i32 = 20;

/// Getting to the end of something on a shelf: a book, a series, a game.
pub const FINISHED_POINTS: i32 = 10;
pub const FINISHED_CAP: i32 = 20;

/// Where a deed came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum DeedKind {
    /// A task finished that day.
    Task,
    /// A tracker kept -- or, for a habit being broken, a day it slipped.
    Habit,
    /// The journal written in.
    Journal,
    /// Time recorded against a goal or an area.
    Time,
    /// Something on a shelf got to the end of.
    Finished,
}

impl DeedKind {
    /// How much of this kind a day can earn. Losses are not capped.
    pub fn cap(self) -> i32 {
        match self {
            DeedKind::Task => TASK_CAP,
            DeedKind::Habit => HABIT_CAP,
            DeedKind::Journal => JOURNAL_POINTS,
            DeedKind::Time => TIME_CAP,
            DeedKind::Finished => FINISHED_CAP,
        }
    }
}

/// One thing done (or slipped into) on a day, and what it was worth before
/// any cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Deed {
    pub kind: DeedKind,
    /// What it was: a task's title, a tracker's name, "Wrote in the journal".
    pub label: String,
    /// Signed. A habit being broken is negative.
    pub points: i32,
}

/// One day's Karma.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KarmaDay {
    pub date: Date,
    /// What the good deeds came to, after every cap. Never negative.
    pub earned: i32,
    /// What the day's slips cost. Never positive, never capped.
    pub lost: i32,
    /// `earned + lost`: the day's Karma.
    pub net: i32,
    /// Every deed, at its own worth before the caps, gains first.
    pub deeds: Vec<Deed>,
}

impl KarmaDay {
    /// Score a day from its deeds.
    ///
    /// Gains are summed per kind and each kind is held to its own cap, then
    /// the whole to [`DAY_CEILING`]. Losses are summed as they are.
    pub fn score(date: Date, mut deeds: Vec<Deed>) -> Self {
        let mut gains: std::collections::BTreeMap<DeedKind, i32> = Default::default();
        let mut lost = 0i32;
        for deed in &deeds {
            if deed.points >= 0 {
                *gains.entry(deed.kind).or_default() += deed.points;
            } else {
                lost = lost.saturating_add(deed.points);
            }
        }
        let earned =
            gains.iter().map(|(kind, sum)| (*sum).min(kind.cap())).sum::<i32>().min(DAY_CEILING);
        deeds.sort_by(|a, b| (b.points >= 0).cmp(&(a.points >= 0)).then(b.points.cmp(&a.points)));
        KarmaDay { date, earned, lost, net: earned + lost, deeds }
    }
}

/// Karma over a window, and in all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KarmaReport {
    /// The day Karma started. Nothing before it counts.
    pub since: Date,
    /// Every day's net from `since` to today. Can be below zero.
    pub total: i64,
    /// Today's net, on its own -- what a day so far has come to.
    pub today: i32,
    /// The days asked for that have anything, oldest first. A day with no
    /// deeds is absent rather than a row of zeros.
    pub days: Vec<KarmaDay>,
    /// [`RULES_VERSION`].
    pub rules: u32,
}

/// What the vault keeps about Karma: when it started, and the days old
/// enough to be settled.
///
/// A day more than [`GRACE_DAYS`] old is **settled**: its earned, lost and
/// net are written down here and never worked out again. That bounds the
/// work a report does -- only the unsettled days and the window being drawn
/// are scored, however long Karma has been running -- and it means a later
/// change to the rules, or to what a tracker is worth, cannot rewrite a
/// month ago. A week's grace is room for a reading logged late or a task
/// ticked the next morning to still count where it belongs.
///
/// Written only by `Vault::settle_karma`, which runs after an unlock -- never
/// by a report, which is a read.
///
/// A sealed singleton of its own rather than a field on the profile or the
/// assistant's settings, because both of those are saved whole from a pane
/// that knows nothing about Karma, and a save that did not carry this day
/// back would restart Karma from zero without anybody noticing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct KarmaBook {
    pub since: Date,
    /// The last settled day; `None` until one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_through: Option<Date>,
    /// Every settled day that came to anything, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settled: Vec<SettledDay>,
}

impl KarmaBook {
    /// A book starting on `since`, with nothing settled.
    pub fn new(since: Date) -> Self {
        Self { since, settled_through: None, settled: Vec::new() }
    }

    /// The first day still worked out from the records.
    pub fn first_open_day(&self) -> Date {
        self.settled_through.and_then(|d| d.tomorrow().ok()).unwrap_or(self.since).max(self.since)
    }

    /// Every settled day's net, added up.
    pub fn settled_total(&self) -> i64 {
        self.settled.iter().map(|d| i64::from(d.net)).sum()
    }
}

/// A settled day: what it came to, without the deeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SettledDay {
    pub date: Date,
    pub earned: i32,
    pub lost: i32,
    pub net: i32,
}

/// How many days back are still open: a day older than this is settled.
pub const GRACE_DAYS: i32 = 7;

/// The associated data the [`KarmaBook`] is sealed under.
pub fn book_aad() -> Vec<u8> {
    b"karmayoga.karma-book.v1".to_vec()
}

/// What a tracker earns (positive) or costs (negative) on a day it counts,
/// when its own [`Tracker::karma`] is not set.
///
/// * A tracker shaped like a limit -- a maximum on some target and a minimum
///   on none, "at most an hour of TV", "at most seven drinks a week" --
///   costs [`HABIT_DEFAULT`] on a day it slipped (see [`habit_points`]).
/// * A habit recorded by hand -- a tick, an amount -- earns it on a day it
///   is kept.
/// * A dose or a symptom earns nothing: a pill taken is not a deed, and a
///   migraine is certainly not one.
/// * Derived trackers earn nothing by default, except screen time or a
///   Health measure with a target: time on a goal and things finished are
///   already deeds of their own, and counting them twice would be farming.
pub fn default_points(tracker: &Tracker) -> i32 {
    let limit = limit_shaped(tracker);
    let has_min = tracker.targets.iter().any(|t| t.min.is_some());
    match &tracker.source {
        TrackerSource::Manual => match tracker.kind {
            TrackerKind::Check | TrackerKind::Amount if limit => -HABIT_DEFAULT,
            TrackerKind::Check | TrackerKind::Amount => HABIT_DEFAULT,
            TrackerKind::Dose | TrackerKind::Scale => 0,
        },
        TrackerSource::ScreenTime { .. } | TrackerSource::Health { .. } if limit => -HABIT_DEFAULT,
        TrackerSource::ScreenTime { .. } | TrackerSource::Health { .. } if has_min => HABIT_DEFAULT,
        _ => 0,
    }
}

/// What a tracker is worth on a day it counts: its own setting, or
/// [`default_points`], held to [`HABIT_LIMIT`] either way.
pub fn points_for(tracker: &Tracker) -> i32 {
    tracker.karma.unwrap_or_else(|| default_points(tracker)).clamp(-HABIT_LIMIT, HABIT_LIMIT)
}

/// The tracker's daily target on its value, if it has one -- the only kind a
/// single day can be judged against.
fn daily_target(tracker: &Tracker) -> Option<&crate::tracker::Target> {
    tracker.targets.iter().find(|t| t.per == Period::Day && t.tally == Tally::Value)
}

/// Is this a habit being kept down rather than built up: a maximum on some
/// target and a minimum on none? "At most an hour of TV a day", "at most
/// seven drinks a week" and "smoked on at most two days a week" all are,
/// whatever their period -- a day with any of it recorded is a day it
/// happened, which is what a habit being broken costs on.
fn limit_shaped(tracker: &Tracker) -> bool {
    tracker.targets.iter().any(|t| t.max.is_some())
        && tracker.targets.iter().all(|t| t.min.is_none())
}

/// What one tracker's day is worth: `0` when the day does not count.
///
/// A tracker that earns counts on a day it was **kept**: something was
/// recorded (a tick of "done", not a deliberate "not done") and, if there is
/// a daily target, the day did not fall short of it.
///
/// A tracker that costs counts on a day it **slipped**: something was
/// recorded and, if there is a daily target, the day went over it. A
/// weekly or yearly limit cannot be judged a day at a time, so there every
/// day it happens costs. "A
/// drink" with no target costs on every day one is recorded; "at most an
/// hour of TV" costs only on a day past the hour; and logging "no alcohol
/// today" as a deliberate zero costs nothing.
pub fn habit_points(tracker: &Tracker, day: Option<&TrackerDay>) -> i32 {
    let points = points_for(tracker);
    let Some(day) = day.filter(|d| d.count > 0) else { return 0 };
    if points == 0 {
        return 0;
    }
    let value = day.value_for(tracker.kind.aggregate());
    // A check stores 1 for done and 0 for deliberately not done, and every
    // other kind reads the same way: nought drinks, nought minutes, a
    // severity of nought is a day it did not happen, recorded on purpose.
    if day.sum <= 0.0 {
        return 0;
    }
    let standing = daily_target(tracker).map(|t| t.standing(value));
    let counts = if points > 0 {
        standing != Some(Standing::Short) && standing != Some(Standing::Over)
    } else {
        // A cost with no target is a cost on every day it happens.
        standing.is_none_or(|s| s == Standing::Over)
    };
    if counts { points } else { 0 }
}

/// Whole hours of time on purpose, as points. Part hours earn nothing: the
/// fifty-minute block is a block, the five-minute one is not a deed.
pub fn time_points(minutes: u64) -> i32 {
    i32::try_from(minutes / 60).unwrap_or(i32::MAX).saturating_mul(TIME_POINTS_PER_HOUR)
}

/// What a finished task is worth.
pub fn task_points(purposed: bool) -> i32 {
    TASK_POINTS + if purposed { TASK_PURPOSE_BONUS } else { 0 }
}

/// Did somebody write this entry on (or the morning after) the day it is
/// for? An entry written up a week later is a fine entry and no deed of
/// that day's: otherwise a quiet evening could write a month of Karma.
pub fn written_on_the_day(entry_date: Date, created: Date) -> bool {
    let next = entry_date.tomorrow().unwrap_or(entry_date);
    created == entry_date || created == next
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::TrackerId;
    use crate::tracker::Target;

    fn d(s: &str) -> Date {
        s.parse().unwrap()
    }

    fn tracker(kind: TrackerKind) -> Tracker {
        Tracker::new("t", kind)
    }

    fn day(count: u32, sum: f64) -> TrackerDay {
        TrackerDay {
            tracker_id: TrackerId::new(),
            date: d("2026-11-16"),
            count,
            sum,
            max: sum,
            first_at: None,
            last_at: None,
        }
    }

    fn deed(kind: DeedKind, points: i32) -> Deed {
        Deed { kind, label: String::new(), points }
    }

    #[test]
    fn a_kept_habit_earns_and_a_missed_one_does_not() {
        let t = tracker(TrackerKind::Check);
        assert_eq!(habit_points(&t, Some(&day(1, 1.0))), HABIT_DEFAULT);
        assert_eq!(habit_points(&t, None), 0);
        // A deliberate "not done" is not a kept day.
        assert_eq!(habit_points(&t, Some(&day(1, 0.0))), 0);
    }

    #[test]
    fn a_habit_being_broken_costs_on_every_day_it_happens() {
        let mut drink = tracker(TrackerKind::Check);
        drink.karma = Some(-20);
        assert_eq!(habit_points(&drink, Some(&day(1, 1.0))), -20);
        assert_eq!(habit_points(&drink, Some(&day(1, 0.0))), 0, "\"none today\" costs nothing");
        assert_eq!(habit_points(&drink, None), 0);
    }

    #[test]
    fn a_limit_costs_only_past_it() {
        let mut tv = tracker(TrackerKind::Amount);
        tv.targets = vec![Target::at_most(60.0, Period::Day)];
        assert_eq!(default_points(&tv), -HABIT_DEFAULT);
        assert_eq!(habit_points(&tv, Some(&day(1, 45.0))), 0);
        assert_eq!(habit_points(&tv, Some(&day(1, 60.0))), 0, "the bound is inclusive");
        assert_eq!(habit_points(&tv, Some(&day(2, 90.0))), -HABIT_DEFAULT);
    }

    #[test]
    fn a_deliberate_zero_is_a_day_it_did_not_happen_for_every_kind() {
        let mut drinks = tracker(TrackerKind::Amount);
        drinks.karma = Some(-20);
        assert_eq!(habit_points(&drinks, Some(&day(1, 0.0))), 0);
        assert_eq!(habit_points(&drinks, Some(&day(1, 2.0))), -20);
        let run = tracker(TrackerKind::Amount);
        assert_eq!(habit_points(&run, Some(&day(1, 0.0))), 0, "nought minutes is no run");
    }

    #[test]
    fn a_limit_over_any_period_is_a_habit_being_broken() {
        let mut drinks = tracker(TrackerKind::Amount);
        drinks.targets = vec![Target::at_most(7.0, Period::Week)];
        assert_eq!(default_points(&drinks), -HABIT_DEFAULT);
        assert_eq!(habit_points(&drinks, Some(&day(1, 1.0))), -HABIT_DEFAULT);
        let mut smoked = tracker(TrackerKind::Check);
        smoked.targets =
            vec![Target { min: None, max: Some(2.0), per: Period::Week, tally: Tally::Days }];
        assert_eq!(default_points(&smoked), -HABIT_DEFAULT);
        // A minimum anywhere makes it a habit being built.
        let mut piano = tracker(TrackerKind::Amount);
        piano.targets = vec![Target::between(60.0, 120.0, Period::Week)];
        assert_eq!(default_points(&piano), HABIT_DEFAULT);
    }

    #[test]
    fn the_open_days_start_after_the_settled_ones() {
        let mut book = KarmaBook::new(d("2026-11-01"));
        assert_eq!(book.first_open_day(), d("2026-11-01"));
        book.settled_through = Some(d("2026-11-09"));
        assert_eq!(book.first_open_day(), d("2026-11-10"));
    }

    #[test]
    fn a_daily_minimum_has_to_be_reached_to_earn() {
        let mut read = tracker(TrackerKind::Amount);
        read.targets = vec![Target::at_least(30.0, Period::Day)];
        assert_eq!(habit_points(&read, Some(&day(1, 10.0))), 0);
        assert_eq!(habit_points(&read, Some(&day(1, 30.0))), HABIT_DEFAULT);
    }

    #[test]
    fn doses_symptoms_and_double_counted_sources_earn_nothing_by_default() {
        assert_eq!(default_points(&tracker(TrackerKind::Dose)), 0);
        assert_eq!(default_points(&tracker(TrackerKind::Scale)), 0);
        let mut time = tracker(TrackerKind::Amount);
        time.source = TrackerSource::Time;
        assert_eq!(default_points(&time), 0);
        let mut finished = tracker(TrackerKind::Amount);
        finished.source = TrackerSource::Finished { kind_id: None };
        assert_eq!(default_points(&finished), 0);
    }

    #[test]
    fn a_tracker_cannot_be_set_past_the_limit() {
        let mut t = tracker(TrackerKind::Check);
        t.karma = Some(1000);
        assert_eq!(points_for(&t), HABIT_LIMIT);
        t.karma = Some(-1000);
        assert_eq!(points_for(&t), -HABIT_LIMIT);
    }

    #[test]
    fn each_kind_is_capped_then_the_day() {
        // Twenty tasks: 200 raw, 40 after the cap.
        let tasks = (0..20).map(|_| deed(DeedKind::Task, 10)).collect::<Vec<_>>();
        let day = KarmaDay::score(d("2026-11-16"), tasks);
        assert_eq!(day.earned, TASK_CAP);

        // Every source at its cap still stops at the ceiling.
        let all = vec![
            deed(DeedKind::Task, 100),
            deed(DeedKind::Habit, 100),
            deed(DeedKind::Journal, 10),
            deed(DeedKind::Time, 100),
            deed(DeedKind::Finished, 100),
        ];
        assert_eq!(KarmaDay::score(d("2026-11-16"), all).earned, DAY_CEILING);
    }

    #[test]
    fn losses_are_never_capped_and_can_take_a_day_below_zero() {
        let deeds =
            vec![deed(DeedKind::Task, 10), deed(DeedKind::Habit, -50), deed(DeedKind::Habit, -50)];
        let day = KarmaDay::score(d("2026-11-16"), deeds);
        assert_eq!((day.earned, day.lost, day.net), (10, -100, -90));
        // Gains first, then losses.
        assert!(day.deeds[0].points > 0);
    }

    #[test]
    fn time_counts_in_whole_hours() {
        assert_eq!(time_points(59), 0);
        assert_eq!(time_points(60), TIME_POINTS_PER_HOUR);
        assert_eq!(time_points(150), 2 * TIME_POINTS_PER_HOUR);
    }

    #[test]
    fn an_entry_counts_only_when_written_on_the_day_or_the_morning_after() {
        assert!(written_on_the_day(d("2026-11-16"), d("2026-11-16")));
        assert!(written_on_the_day(d("2026-11-16"), d("2026-11-17")));
        assert!(!written_on_the_day(d("2026-11-16"), d("2026-11-20")));
        assert!(!written_on_the_day(d("2026-11-16"), d("2026-11-15")));
    }
}
