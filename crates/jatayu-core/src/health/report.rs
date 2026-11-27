//! Adding series up: buckets over local time, a day's value under a stat,
//! and the activity day the rings are drawn from.
//!
//! Everything here reads [`SeriesSummary`] quarter hours, never samples,
//! except [`raw_points`] -- which is why a year of resting heart rate costs
//! 365 small reads.
//!
//! A summary is one UTC day and its quarter hours are UTC ones, but the
//! person lives in a local zone: a local day is cut out of two UTC days, and
//! on a daylight-saving day it is 23 or 25 hours long. So every quarter hour
//! is placed by the *instant it starts*, in the zone asked for, and goes
//! wholly to the local hour, day, week or month that instant falls in. Zones
//! sit on whole quarter hours from UTC, so no local boundary cuts a quarter
//! hour in two and this is exact: the summary never needs the samples.

use super::chunk;
use super::metric::Metric;
use super::{
    ActivityDay, Bucket, BucketSize, MAX_SAMPLE_SECS, SamplePoint, SeriesDay, SeriesPoint,
    SeriesSummary, Stat, Workout,
};
use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};
use std::collections::BTreeMap;

/// Seconds in one quarter hour, a [`SeriesSummary`] bucket.
const QUARTER_SECS: i64 = 15 * 60;
/// Seconds in a UTC day.
const DAY_SECS: i64 = 86_400;
/// Quarter hours in a UTC day; a bucket numbered past it is corrupt.
const QUARTERS_PER_DAY: u8 = 96;

/// `stat`, with [`Stat::Auto`] resolved to the metric's own default.
pub fn resolve_stat(metric: Metric, stat: Stat) -> Stat {
    match stat {
        Stat::Auto => metric.info().stat,
        other => other,
    }
}

/// The UTC days whose summaries a local `from..=to` in `tz` needs: one
/// either side, since a local day straddles two UTC ones.
pub fn utc_span(from: Date, to: Date) -> (Date, Date) {
    (from.yesterday().unwrap_or(from), to.tomorrow().unwrap_or(to))
}

/// What the quarter hours that fall in one output point add up to.
struct Acc {
    count: u64,
    sum: f64,
    min: f64,
    max: f64,
    /// The `last` of the quarter that started last, and when it started.
    last: f64,
    last_start: i64,
    /// Seconds covered by samples, all of them and those in range.
    covered: u64,
    covered_in_range: u64,
    /// Readings in range, which a series with no durations (where `covered`
    /// stays 0) is judged by instead.
    count_in_range: u64,
}

impl Default for Acc {
    fn default() -> Self {
        Acc {
            count: 0,
            sum: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            last: 0.0,
            last_start: i64::MIN,
            covered: 0,
            covered_in_range: 0,
            count_in_range: 0,
        }
    }
}

impl Acc {
    /// Take in the quarter hour `b`, which starts at `start` (epoch
    /// seconds). `range` is the `low..=high` of an
    /// [`Stat::InRange`], judged on the quarter's own mean: a 15-minute
    /// approximation of "time in range", fine for a CGM reading every five.
    fn add(&mut self, b: &Bucket, start: i64, range: Option<(f64, f64)>) {
        self.count += u64::from(b.count);
        self.sum += b.sum;
        self.min = self.min.min(b.min);
        self.max = self.max.max(b.max);
        if start >= self.last_start {
            self.last = b.last;
            self.last_start = start;
        }
        self.covered += u64::from(b.covered);
        if let Some((low, high)) = range {
            let mean = b.sum / f64::from(b.count);
            if (low..=high).contains(&mean) {
                self.covered_in_range += u64::from(b.covered);
                self.count_in_range += u64::from(b.count);
            }
        }
    }

    /// The point's value under `stat`. [`Stat::Auto`] was resolved before
    /// anything was added; a catalogue that still said it would mean a mean.
    fn value(&self, stat: Stat) -> f64 {
        match stat {
            Stat::Sum => self.sum,
            Stat::Mean | Stat::Auto => self.sum / self.count as f64,
            Stat::Min => self.min,
            Stat::Max => self.max,
            Stat::Last => self.last,
            Stat::Count => self.count as f64,
            Stat::InRange { .. } if self.covered > 0 => {
                self.covered_in_range as f64 / self.covered as f64
            }
            Stat::InRange { .. } => self.count_in_range as f64 / self.count as f64,
        }
    }
}

/// The instant the local day `date` starts in `tz`. Where midnight does not
/// exist (a zone that springs forward at midnight) the first instant of the
/// day, which is what a day "starts" meaning there.
fn day_start(tz: &TimeZone, date: Date) -> Option<Timestamp> {
    tz.to_timestamp(date.at(0, 0, 0, 0)).ok()
}

/// The Monday on or before `date`. Weeks start Monday: the core has no
/// locale, and every answer carries explicit dates, so a screen that wants
/// Sunday weeks can say so without asking for another bucket size.
fn monday_of(date: Date) -> Date {
    let back = i64::from(date.weekday().to_monday_zero_offset());
    date.saturating_sub(Span::new().days(back))
}

/// The instant (epoch seconds) of the bucket a quarter hour starting at
/// `start`, which is `local` there, belongs to. Hours are cut in instants
/// rather than local clock times, so the two 01:00 hours of a fall-back day
/// stay two buckets.
fn bucket_key(tz: &TimeZone, start: i64, local: DateTime, bucket: BucketSize) -> Option<i64> {
    match bucket {
        // `Raw` is `raw_points`' business; the finest this can say is a
        // quarter hour.
        BucketSize::Raw | BucketSize::QuarterHour => Some(start),
        BucketSize::Hour => {
            Some(start - i64::from(local.minute()) * 60 - i64::from(local.second()))
        }
        BucketSize::Day => day_start(tz, local.date()).map(|t| t.as_second()),
        BucketSize::Week => day_start(tz, monday_of(local.date())).map(|t| t.as_second()),
        BucketSize::Month => day_start(tz, local.date().first_of_month()).map(|t| t.as_second()),
    }
}

/// The instant a UTC day starts, in epoch seconds.
fn utc_midnight(day: Date) -> Option<i64> {
    TimeZone::UTC.to_timestamp(day.at(0, 0, 0, 0)).ok().map(|t| t.as_second())
}

/// Add summaries up into points of `bucket` width over the local dates
/// `from..=to` in `tz`, under `stat`. `bucket` must not be
/// [`BucketSize::Raw`] (that is [`raw_points`]).
///
/// Each quarter hour goes to the bucket its start falls in, and only
/// quarters starting on a local date in `from..=to` count, so a UTC day cut
/// by the range contributes just its part. Under [`Stat::Mean`] the value is
/// the sum of sums over the sum of counts (the mean of the readings, not of
/// the quarter means); under [`Stat::InRange`] it is the share of covered
/// seconds (of readings, where nothing carries a duration) whose quarter
/// hour's mean lies in `low..=high`. A week is Monday to Sunday and a
/// partial one at either end of the range holds only the days in it; its
/// `at` is still its Monday, and likewise a month's is its first.
///
/// One point per non-empty bucket, in time order; `min` and `max` are the
/// lowest and highest of the quarter hours' own.
pub fn bucket_series(
    metric: Metric,
    summaries: &[SeriesSummary],
    from: Date,
    to: Date,
    tz: &TimeZone,
    bucket: BucketSize,
    stat: Stat,
) -> Vec<SeriesPoint> {
    let stat = resolve_stat(metric, stat);
    let range = match stat {
        Stat::InRange { low, high } => Some((f64::from(low), f64::from(high))),
        _ => None,
    };
    let (first_day, last_day) = utc_span(from, to);
    let mut points: BTreeMap<i64, Acc> = BTreeMap::new();
    for summary in summaries {
        let Some(day) = summary.day else { continue };
        if day < first_day || day > last_day {
            continue;
        }
        let Some(midnight) = utc_midnight(day) else { continue };
        for b in &summary.buckets {
            if b.count == 0 || b.quarter >= QUARTERS_PER_DAY {
                continue;
            }
            let start = midnight + i64::from(b.quarter) * QUARTER_SECS;
            let Ok(instant) = Timestamp::from_second(start) else { continue };
            let local = tz.to_datetime(instant);
            if local.date() < from || local.date() > to {
                continue;
            }
            let Some(key) = bucket_key(tz, start, local, bucket) else { continue };
            points.entry(key).or_default().add(b, start, range);
        }
    }
    points
        .into_iter()
        .filter_map(|(key, acc)| {
            Some(SeriesPoint {
                at: Timestamp::from_second(key).ok()?,
                value: acc.value(stat),
                min: Some(acc.min),
                max: Some(acc.max),
                count: u32::try_from(acc.count).unwrap_or(u32::MAX),
            })
        })
        .collect()
}

/// One value per local day `from..=to` under `stat`, only for days with
/// something in them. What trackers, activity days and targets read.
pub fn daily_values(
    metric: Metric,
    summaries: &[SeriesSummary],
    from: Date,
    to: Date,
    tz: &TimeZone,
    stat: Stat,
) -> BTreeMap<Date, f64> {
    bucket_series(metric, summaries, from, to, tz, BucketSize::Day, stat)
        .into_iter()
        .map(|p| (tz.to_datetime(p.at).date(), p.value))
        .collect()
}

/// Every sample in `from..to`, in order: those that start in it, and one
/// that began earlier and was still going at `from` (a night's sleep stage
/// across midnight, a workout's heart-rate stretch). Days that cannot hold
/// one -- no sample lasts longer than [`MAX_SAMPLE_SECS`] -- are not opened.
pub fn raw_points(days: &[SeriesDay], from: Timestamp, to: Timestamp) -> Vec<SamplePoint> {
    let (from_s, to_s) = (from.as_second(), to.as_second());
    let mut out = Vec::new();
    for day in days {
        if let Some(midnight) = day.day.and_then(utc_midnight)
            && (midnight >= to_s || midnight + DAY_SECS + MAX_SAMPLE_SECS <= from_s)
        {
            continue;
        }
        let overlaps = |p: &SamplePoint| p.start < to && (p.start >= from || p.end > from);
        out.extend(chunk::points(day).into_iter().filter(overlaps));
    }
    out.sort_by_key(|p| (p.start, p.end));
    out
}

/// Activity days `from..=to` from each metric's daily values (as
/// [`daily_values`] answers them under the metric's own stat) and the
/// workouts in the range. A day with nothing in it is zeros and no resting
/// heart rate or HRV, so the screen can still draw it; `workouts` counts
/// those that started on that local date.
pub fn activity_days(
    daily: &BTreeMap<Metric, BTreeMap<Date, f64>>,
    workouts: &[Workout],
    from: Date,
    to: Date,
    tz: &TimeZone,
) -> Vec<ActivityDay> {
    let mut started: BTreeMap<Date, u32> = BTreeMap::new();
    for w in workouts {
        *started.entry(tz.to_datetime(w.start).date()).or_default() += 1;
    }
    let value =
        |metric: Metric, date: Date| daily.get(&metric).and_then(|days| days.get(&date)).copied();
    let mut out = Vec::new();
    let mut date = from;
    while date <= to {
        out.push(ActivityDay {
            date,
            active_kcal: value(Metric::ActiveEnergy, date).unwrap_or(0.0),
            exercise_min: value(Metric::ExerciseTime, date).unwrap_or(0.0),
            stand_hours: value(Metric::StandHour, date).unwrap_or(0.0),
            steps: value(Metric::Steps, date).unwrap_or(0.0),
            distance_m: value(Metric::WalkingRunningDistance, date).unwrap_or(0.0),
            flights: value(Metric::FlightsClimbed, date).unwrap_or(0.0),
            resting_hr: value(Metric::RestingHeartRate, date),
            hrv_ms: value(Metric::HeartRateVariability, date),
            workouts: started.get(&date).copied().unwrap_or(0),
        });
        let Ok(next) = date.tomorrow() else { break };
        date = next;
    }
    out
}

/// The metrics [`activity_days`] reads.
pub const ACTIVITY_METRICS: &[Metric] = &[
    Metric::ActiveEnergy,
    Metric::ExerciseTime,
    Metric::StandHour,
    Metric::Steps,
    Metric::WalkingRunningDistance,
    Metric::FlightsClimbed,
    Metric::RestingHeartRate,
    Metric::HeartRateVariability,
];

#[cfg(test)]
mod tests {
    use super::super::{Activity, Sample};
    use super::*;
    use crate::id::WorkoutId;
    use jiff::civil::date;

    fn la() -> TimeZone {
        TimeZone::get("America/Los_Angeles").unwrap()
    }

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn nov(day: i8) -> Date {
        date(2026, 11, day)
    }

    /// A quarter hour of `count` readings adding up to `sum`, none of them
    /// outside `min..=max`, the last of which was `last`.
    fn bucket(quarter: u8, count: u32, sum: f64, min: f64, max: f64, last: f64) -> Bucket {
        Bucket { quarter, count, sum, min, max, last, covered: 0 }
    }

    /// A quarter hour of one reading of `sum`.
    fn one(quarter: u8, sum: f64) -> Bucket {
        bucket(quarter, 1, sum, sum, sum, sum)
    }

    fn day(on: Date, buckets: Vec<Bucket>) -> SeriesSummary {
        SeriesSummary { metric: None, day: Some(on), buckets, offset_min: 0 }
    }

    /// `bucket_series` over the local dates `range`. Every test gives its
    /// stat, so none depends on what the catalogue says.
    fn run(
        summaries: &[SeriesSummary],
        range: (Date, Date),
        tz: &TimeZone,
        bucket: BucketSize,
        stat: Stat,
    ) -> Vec<SeriesPoint> {
        bucket_series(Metric::Steps, summaries, range.0, range.1, tz, bucket, stat)
    }

    fn values(points: &[SeriesPoint]) -> Vec<f64> {
        points.iter().map(|p| p.value).collect()
    }

    fn starts(points: &[SeriesPoint]) -> Vec<Timestamp> {
        points.iter().map(|p| p.at).collect()
    }

    fn workout(start: &str) -> Workout {
        let start = at(start);
        Workout {
            id: WorkoutId::new(),
            activity: Activity::default(),
            name: String::new(),
            start,
            end: start,
            offset_min: 0,
            active_energy_kcal: None,
            total_energy_kcal: None,
            distance_m: None,
            elevation_up_m: None,
            avg_hr: None,
            max_hr: None,
            effort: None,
            indoor: None,
            source: String::new(),
            source_id: None,
            external_id: String::new(),
            route: None,
            note: String::new(),
            key: String::new(),
            hidden: false,
            created_at: start,
            updated_at: start,
        }
    }

    #[test]
    fn a_local_day_is_cut_from_two_utc_days() {
        // Pacific standard time, UTC-8: local 10 November runs from 08:00Z on
        // the 10th to 08:00Z on the 11th.
        let summaries = [
            day(nov(10), vec![one(31, 100.0), one(32, 10.0), one(95, 20.0)]),
            day(nov(11), vec![one(0, 30.0), one(31, 40.0), one(32, 1000.0)]),
        ];
        let tenth = run(&summaries, (nov(10), nov(10)), &la(), BucketSize::Day, Stat::Sum);
        assert_eq!(values(&tenth), vec![100.0]);
        assert_eq!(tenth[0].count, 4);
        assert_eq!(tenth[0].at, at("2026-11-10T08:00:00Z"));

        let three = run(&summaries, (nov(9), nov(11)), &la(), BucketSize::Day, Stat::Sum);
        assert_eq!(values(&three), vec![100.0, 100.0, 1000.0]);
    }

    #[test]
    fn a_daylight_saving_day_is_twenty_five_hours_long() {
        // Clocks go back at 09:00Z on 1 November 2026: local the 1st is
        // 07:00Z to 08:00Z the next day.
        let summaries = [
            day(nov(1), vec![one(27, 1.0), one(28, 10.0)]),
            day(nov(2), vec![one(31, 100.0), one(32, 1000.0)]),
        ];
        let from = date(2026, 10, 31);
        let by_day = daily_values(Metric::Steps, &summaries, from, nov(2), &la(), Stat::Sum);
        assert_eq!(by_day, BTreeMap::from([(from, 1.0), (nov(1), 110.0), (nov(2), 1000.0)]));
    }

    #[test]
    fn a_spring_forward_day_is_twenty_three_hours_long() {
        // Clocks go forward at 10:00Z on 8 March 2026: local the 8th is
        // 08:00Z to 07:00Z the next day.
        let summaries = [
            day(date(2026, 3, 8), vec![one(31, 1.0), one(32, 10.0)]),
            day(date(2026, 3, 9), vec![one(27, 100.0), one(28, 1000.0)]),
        ];
        let (seventh, eighth, ninth) = (date(2026, 3, 7), date(2026, 3, 8), date(2026, 3, 9));
        let by_day = daily_values(Metric::Steps, &summaries, seventh, ninth, &la(), Stat::Sum);
        assert_eq!(by_day, BTreeMap::from([(seventh, 1.0), (eighth, 110.0), (ninth, 1000.0)]));
    }

    #[test]
    fn the_repeated_hour_of_a_fall_back_stays_two_buckets() {
        // 01:15 PDT (08:15Z), then 01:00 and 01:15 PST (09:00Z, 09:15Z).
        let summaries = [day(nov(1), vec![one(33, 1.0), one(36, 2.0), one(37, 4.0)])];
        let hours = run(&summaries, (nov(1), nov(1)), &la(), BucketSize::Hour, Stat::Sum);
        assert_eq!(starts(&hours), vec![at("2026-11-01T08:00:00Z"), at("2026-11-01T09:00:00Z")]);
        assert_eq!(values(&hours), vec![1.0, 6.0]);
        assert_eq!(hours[1].count, 2);
    }

    #[test]
    fn stats_combine_the_quarter_hours() {
        let summaries = [day(
            nov(10),
            vec![
                bucket(8, 2, 10.0, 4.0, 6.0, 6.0),
                bucket(20, 1, 3.0, 3.0, 3.0, 3.0),
                bucket(40, 3, 30.0, 8.0, 12.0, 11.0),
            ],
        )];
        let one_day = |stat| {
            let points = run(&summaries, (nov(10), nov(10)), &TimeZone::UTC, BucketSize::Day, stat);
            assert_eq!(points.len(), 1);
            points[0].clone()
        };
        assert_eq!(one_day(Stat::Sum).value, 43.0);
        assert_eq!(one_day(Stat::Mean).value, 43.0 / 6.0);
        assert_eq!(one_day(Stat::Min).value, 3.0);
        assert_eq!(one_day(Stat::Max).value, 12.0);
        assert_eq!(one_day(Stat::Last).value, 11.0);
        assert_eq!(one_day(Stat::Count).value, 6.0);
        // Whatever the stat, the point carries the extremes and the count.
        let p = one_day(Stat::Mean);
        assert_eq!((p.min, p.max, p.count), (Some(3.0), Some(12.0), 6));
    }

    #[test]
    fn last_follows_the_start_time_not_the_order_given() {
        let summaries =
            [day(nov(11), vec![one(2, 7.0)]), day(nov(10), vec![one(90, 9.0), one(10, 5.0)])];
        let range = (nov(10), nov(11));
        let days = run(&summaries, range, &TimeZone::UTC, BucketSize::Day, Stat::Last);
        assert_eq!(values(&days), vec![9.0, 7.0]);
        let week = run(&summaries, range, &TimeZone::UTC, BucketSize::Week, Stat::Last);
        assert_eq!(values(&week), vec![7.0]);
    }

    #[test]
    fn in_range_is_the_share_of_covered_seconds_by_quarter_hour_mean() {
        let covered = |quarter, count, sum, covered| Bucket {
            covered,
            ..bucket(quarter, count, sum, sum, sum, sum)
        };
        let stat = Stat::InRange { low: 70, high: 180 };
        let share = |buckets| {
            let summaries = [day(nov(10), buckets)];
            run(&summaries, (nov(10), nov(10)), &TimeZone::UTC, BucketSize::Day, stat)[0].value
        };
        // Mean 100 over 300 s (in), mean 200 over 900 s (out), mean 180 over
        // 300 s (the bound, so in): 600 of 1500 seconds.
        let by_time =
            vec![covered(1, 3, 300.0, 300), covered(2, 1, 200.0, 900), covered(3, 2, 360.0, 300)];
        assert_eq!(share(by_time), 600.0 / 1500.0);
        // With no durations at all, the share of readings: three of four.
        let by_count = vec![bucket(1, 3, 300.0, 100.0, 100.0, 100.0), one(2, 200.0)];
        assert_eq!(share(by_count), 0.75);
        // Below the low bound is out, and the low bound itself is in.
        assert_eq!(share(vec![covered(1, 1, 69.0, 900), covered(2, 1, 70.0, 900)]), 0.5);
    }

    #[test]
    fn weeks_start_on_monday_and_partial_weeks_hold_only_their_days_in_range() {
        // 2 November 2026 is a Monday.
        let summaries = [
            day(nov(8), vec![one(10, 1.0)]),
            day(nov(9), vec![one(10, 2.0)]),
            day(nov(15), vec![one(10, 4.0)]),
            day(nov(16), vec![one(10, 8.0)]),
        ];
        let weeks = run(&summaries, (nov(8), nov(16)), &TimeZone::UTC, BucketSize::Week, Stat::Sum);
        assert_eq!(
            starts(&weeks),
            vec![
                at("2026-11-02T00:00:00Z"),
                at("2026-11-09T00:00:00Z"),
                at("2026-11-16T00:00:00Z")
            ]
        );
        assert_eq!(values(&weeks), vec![1.0, 6.0, 8.0]);
        // Cut the range at the Sunday: the 16th is out.
        let cut = run(&summaries, (nov(9), nov(15)), &TimeZone::UTC, BucketSize::Week, Stat::Sum);
        assert_eq!(values(&cut), vec![6.0]);
    }

    #[test]
    fn months_are_filed_under_their_first_day() {
        let summaries = [
            day(date(2026, 10, 31), vec![one(10, 1.0)]),
            day(nov(1), vec![one(10, 2.0)]),
            day(nov(30), vec![one(10, 4.0)]),
        ];
        let range = (date(2026, 10, 31), nov(30));
        let months = run(&summaries, range, &TimeZone::UTC, BucketSize::Month, Stat::Sum);
        assert_eq!(starts(&months), vec![at("2026-10-01T00:00:00Z"), at("2026-11-01T00:00:00Z")]);
        assert_eq!(values(&months), vec![1.0, 6.0]);
    }

    #[test]
    fn quarter_hours_and_hours_keep_their_own_starts() {
        let summaries = [day(nov(10), vec![one(5, 1.0), one(6, 2.0), one(8, 4.0)])];
        let range = (nov(10), nov(10));
        let quarters = run(&summaries, range, &TimeZone::UTC, BucketSize::QuarterHour, Stat::Sum);
        assert_eq!(
            starts(&quarters),
            vec![
                at("2026-11-10T01:15:00Z"),
                at("2026-11-10T01:30:00Z"),
                at("2026-11-10T02:00:00Z")
            ]
        );
        let hours = run(&summaries, range, &TimeZone::UTC, BucketSize::Hour, Stat::Sum);
        assert_eq!(starts(&hours), vec![at("2026-11-10T01:00:00Z"), at("2026-11-10T02:00:00Z")]);
        assert_eq!(values(&hours), vec![3.0, 4.0]);
    }

    #[test]
    fn a_half_hour_zone_cuts_its_days_on_its_own_midnight() {
        // India, UTC+5:30: local 10 November starts at 18:30Z on the 9th.
        let india = TimeZone::get("Asia/Kolkata").unwrap();
        let summaries = [
            day(nov(9), vec![one(73, 1.0), one(74, 10.0)]),
            day(nov(10), vec![one(73, 100.0), one(74, 1000.0)]),
        ];
        let by_day = daily_values(Metric::Steps, &summaries, nov(9), nov(11), &india, Stat::Sum);
        assert_eq!(by_day, BTreeMap::from([(nov(9), 1.0), (nov(10), 110.0), (nov(11), 1000.0)]));
    }

    #[test]
    fn nothing_in_range_is_no_points() {
        let summaries = [day(nov(10), vec![one(5, 1.0)])];
        let utc = TimeZone::UTC;
        let days = |from, to| run(&summaries, (from, to), &utc, BucketSize::Day, Stat::Sum);
        assert!(days(nov(11), nov(12)).is_empty());
        assert!(days(nov(12), nov(10)).is_empty());
        assert!(run(&[], (nov(1), nov(30)), &utc, BucketSize::Day, Stat::Sum).is_empty());
        // An empty quarter (count 0) makes no point either.
        let hollow = [day(nov(10), vec![bucket(5, 0, 0.0, 0.0, 0.0, 0.0)])];
        let by_day = daily_values(Metric::Steps, &hollow, nov(10), nov(10), &utc, Stat::Sum);
        assert!(by_day.is_empty());
    }

    #[test]
    fn daily_values_only_has_days_with_something() {
        let summaries = [
            day(nov(10), vec![bucket(5, 2, 120.0, 55.0, 65.0, 65.0)]),
            day(nov(12), vec![one(5, 70.0)]),
        ];
        let metric = Metric::RestingHeartRate;
        let by_day = daily_values(metric, &summaries, nov(9), nov(13), &TimeZone::UTC, Stat::Mean);
        assert_eq!(by_day, BTreeMap::from([(nov(10), 60.0), (nov(12), 70.0)]));
    }

    fn sample(start: &str, secs: u32, value: f64, source: u16) -> Sample {
        Sample { start: at(start).as_second(), secs, value, source, offset_min: 0 }
    }

    fn heart_day(on: Date, sources: &[&str], samples: Vec<Sample>) -> SeriesDay {
        SeriesDay {
            metric: Some(Metric::HeartRate),
            day: Some(on),
            sources: sources.iter().map(|s| s.to_string()).collect(),
            samples,
            tombstones: Vec::new(),
        }
    }

    #[test]
    fn raw_points_are_the_samples_in_range_in_order() {
        let tenth = heart_day(
            nov(10),
            &["Watch", "iPhone"],
            vec![
                sample("2026-11-10T22:00:00Z", 0, 1.0, 0),
                sample("2026-11-10T23:00:00Z", 7200, 2.0, 1),
                sample("2026-11-10T23:50:00Z", 0, 3.0, 0),
            ],
        );
        let eleventh = heart_day(
            nov(11),
            &["Watch"],
            vec![
                sample("2026-11-11T00:30:00Z", 0, 4.0, 0),
                sample("2026-11-11T01:00:00Z", 0, 5.0, 0),
            ],
        );
        // Given out of order, read from 23:30 (inside the two-hour stretch)
        // to 01:00 (exclusive).
        let points =
            raw_points(&[eleventh, tenth], at("2026-11-10T23:30:00Z"), at("2026-11-11T01:00:00Z"));
        assert_eq!(points.iter().map(|p| p.value).collect::<Vec<_>>(), vec![2.0, 3.0, 4.0]);
        assert_eq!(points[0].source, "iPhone");
        assert_eq!(points[0].end, at("2026-11-11T01:00:00Z"));
    }

    #[test]
    fn raw_points_skip_days_too_far_off_to_hold_a_sample() {
        let old = heart_day(
            date(2026, 10, 1),
            &["Watch"],
            vec![sample("2026-10-01T12:00:00Z", 0, 1.0, 0)],
        );
        let (from, to) = (at("2026-11-10T00:00:00Z"), at("2026-11-11T00:00:00Z"));
        assert!(raw_points(&[old], from, to).is_empty());
    }

    #[test]
    fn activity_days_fill_every_date_and_count_workouts_by_local_date() {
        let daily = BTreeMap::from([
            (Metric::ActiveEnergy, BTreeMap::from([(nov(10), 540.0)])),
            (Metric::Steps, BTreeMap::from([(nov(10), 9000.0), (nov(12), 100.0)])),
            (Metric::RestingHeartRate, BTreeMap::from([(nov(10), 58.0)])),
            (Metric::HeartRateVariability, BTreeMap::from([(nov(11), 41.0)])),
        ]);
        // 22:00 PST on the 10th, then midnight and 01:00 on the 11th.
        let workouts = [
            workout("2026-11-11T06:00:00Z"),
            workout("2026-11-11T08:00:00Z"),
            workout("2026-11-11T09:00:00Z"),
        ];
        let days = activity_days(&daily, &workouts, nov(10), nov(12), &la());
        assert_eq!(days.len(), 3);
        assert_eq!(days[0].date, nov(10));
        assert_eq!((days[0].active_kcal, days[0].steps), (540.0, 9000.0));
        assert_eq!((days[0].resting_hr, days[0].workouts), (Some(58.0), 1));
        // Nothing but two workouts and an HRV on the 11th.
        assert_eq!((days[1].active_kcal, days[1].steps), (0.0, 0.0));
        assert_eq!((days[1].resting_hr, days[1].hrv_ms, days[1].workouts), (None, Some(41.0), 2));
        assert_eq!((days[2].steps, days[2].workouts), (100.0, 0));
    }

    #[test]
    fn activity_days_of_nothing_are_zeros_and_a_backwards_range_is_empty() {
        let none = BTreeMap::new();
        let days = activity_days(&none, &[], nov(10), nov(11), &TimeZone::UTC);
        assert_eq!(days.len(), 2);
        for d in &days {
            assert_eq!((d.active_kcal, d.exercise_min, d.stand_hours), (0.0, 0.0, 0.0));
            assert_eq!((d.steps, d.distance_m, d.flights, d.workouts), (0.0, 0.0, 0.0, 0));
            assert_eq!((d.resting_hr, d.hrv_ms), (None, None));
        }
        assert!(activity_days(&none, &[], nov(11), nov(10), &TimeZone::UTC).is_empty());
    }
}
