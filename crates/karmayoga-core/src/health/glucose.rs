//! Glucose: the standard CGM report and what a meal did.
//!
//! Ranges and formulas from the international consensus on time in range
//! (Battelino et al., Diabetes Care 2019) and the GMI paper (Bergenstal et
//! al., 2018). All in mg/dL, the canonical unit.

use super::{AgpSlot, GlucoseEvent, GlucoseReport, Meal, MealResponse, SamplePoint};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

pub const VERY_LOW: f64 = 54.0;
pub const LOW: f64 = 70.0;
pub const HIGH: f64 = 180.0;
pub const VERY_HIGH: f64 = 250.0;

/// A reading stands for the time until the next one, but no more than this:
/// a longer silence is a gap in the data, not a long reading.
const MAX_WEIGHT_SECS: i64 = 15 * 60;
/// What the last reading stands for, there being no next one: the usual
/// interval of a continuous monitor.
const LAST_WEIGHT_SECS: i64 = 5 * 60;
/// The shortest stretch below or above a threshold that is an event.
const EVENT_SECS: i64 = 15 * 60;
/// Width of an AGP slot in minutes, and how many there are in a day.
const SLOT_MINUTES: u32 = 15;
const SLOTS: usize = 96;

/// How far before a meal its baseline reading may be, and how far after it
/// one is taken when there is none before.
const BASELINE_BEFORE_SECS: i64 = 30 * 60;
const BASELINE_AFTER_SECS: i64 = 10 * 60;
/// What a meal's response covers, and the part of it the area is measured
/// over.
const RESPONSE_SECS: i64 = 3 * 3600;
const AREA_SECS: i64 = 2 * 3600;
/// Back within this of baseline is recovered.
const RECOVERED_WITHIN: f64 = 10.0;

/// Glucose management indicator, %, from a mean in mg/dL.
pub fn gmi(mean: f64) -> f64 {
    3.31 + 0.02392 * mean
}

/// Which side of the target range a reading is on, if either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Low,
    High,
}

fn side_of(value: f64) -> Option<Side> {
    if value < LOW {
        Some(Side::Low)
    } else if value > HIGH {
        Some(Side::High)
    } else {
        None
    }
}

/// The five bands of the consensus, as an index into five shares: below 54,
/// 54 to 69, 70 to 180, 181 to 250, above 250. The target range is inclusive.
fn band_of(value: f64) -> usize {
    if value < VERY_LOW {
        0
    } else if value < LOW {
        1
    } else if value <= HIGH {
        2
    } else if value <= VERY_HIGH {
        3
    } else {
        4
    }
}

/// `points` in time order, by reference.
fn sorted(points: &[SamplePoint]) -> Vec<&SamplePoint> {
    let mut out: Vec<&SamplePoint> = points.iter().collect();
    out.sort_by_key(|p| p.start);
    out
}

/// Seconds each of `readings` (in time order) stands for: until the next one,
/// at most [`MAX_WEIGHT_SECS`]; the last one [`LAST_WEIGHT_SECS`].
fn weights_of(readings: &[&SamplePoint]) -> Vec<i64> {
    let mut weights: Vec<i64> = readings
        .windows(2)
        .map(|pair| (pair[1].start.as_second() - pair[0].start.as_second()).min(MAX_WEIGHT_SECS))
        .collect();
    if !readings.is_empty() {
        weights.push(LAST_WEIGHT_SECS);
    }
    weights
}

/// The epoch seconds the local dates `from..=to` in `tz` begin and end at,
/// when that is a span at all.
fn local_span(from: Date, to: Date, tz: &TimeZone) -> Option<(i64, i64)> {
    let start = tz.to_timestamp(from.at(0, 0, 0, 0)).ok()?.as_second();
    let end = tz.to_timestamp(to.tomorrow().ok()?.at(0, 0, 0, 0)).ok()?.as_second();
    (end > start).then_some((start, end))
}

/// The report over local dates `from..=to` in `tz`. A reading stands for
/// the time until the next one, capped at 15 minutes, which is what
/// coverage and the time-in-range shares are counted in.
///
/// Readings outside the dates are ignored. `coverage` is the weight of the
/// readings over the minutes the dates hold (a daylight-saving day is 23 or
/// 25 hours), the five shares are of the weight there is, `mean`, `cv` (the
/// population standard deviation over the mean) and `gmi` are of the
/// readings themselves, and the AGP is the 5th, 25th, 50th, 75th and 95th
/// percentiles of the readings by the quarter hour of local time they were
/// taken in.
pub fn report(points: &[SamplePoint], from: Date, to: Date, tz: &TimeZone) -> GlucoseReport {
    let mut out = GlucoseReport { from: Some(from), to: Some(to), ..GlucoseReport::default() };
    let Some((start, end)) = local_span(from, to, tz) else { return out };
    let mut readings = sorted(points);
    readings.retain(|p| (start..end).contains(&p.start.as_second()));
    if readings.is_empty() {
        return out;
    }
    let weights = weights_of(&readings);

    let total: i64 = weights.iter().sum();
    out.readings = u32::try_from(readings.len()).unwrap_or(u32::MAX);
    out.coverage = (total as f64 / (end - start) as f64).min(1.0);

    let mut bands = [0i64; 5];
    for (reading, weight) in readings.iter().zip(&weights) {
        bands[band_of(reading.value)] += weight;
    }
    if total > 0 {
        let share = |band: usize| bands[band] as f64 / total as f64;
        out.very_low = share(0);
        out.low = share(1);
        out.in_range = share(2);
        out.high = share(3);
        out.very_high = share(4);
    }

    let count = readings.len() as f64;
    let mean = readings.iter().map(|r| r.value).sum::<f64>() / count;
    let variance = readings.iter().map(|r| (r.value - mean).powi(2)).sum::<f64>() / count;
    out.mean = Some(mean);
    out.gmi = Some(gmi(mean));
    out.cv = (mean > 0.0).then(|| variance.sqrt() / mean * 100.0);

    out.agp = agp(&readings, tz);
    out.events = event_runs(&readings, &weights);
    out
}

/// The `p` (0..=1) percentile of `values` (sorted, and not empty),
/// interpolating linearly between the two values either side of it.
fn percentile(values: &[f64], p: f64) -> f64 {
    let rank = p * (values.len() - 1) as f64;
    let (below, above) = (rank.floor() as usize, rank.ceil() as usize);
    values[below] + (values[above] - values[below]) * (rank - below as f64)
}

/// The percentile bands of `readings` by quarter hour of the local day,
/// leaving out the quarters nothing was read in.
fn agp(readings: &[&SamplePoint], tz: &TimeZone) -> Vec<AgpSlot> {
    let mut slots: Vec<Vec<f64>> = vec![Vec::new(); SLOTS];
    for reading in readings {
        let local = tz.to_datetime(reading.start);
        let minute = local.hour() as u32 * 60 + local.minute() as u32;
        slots[(minute / SLOT_MINUTES) as usize % SLOTS].push(reading.value);
    }
    slots
        .into_iter()
        .enumerate()
        .filter(|(_, values)| !values.is_empty())
        .map(|(slot, mut values)| {
            values.sort_by(f64::total_cmp);
            AgpSlot {
                minute: slot as u32 * SLOT_MINUTES,
                p5: percentile(&values, 0.05),
                p25: percentile(&values, 0.25),
                p50: percentile(&values, 0.5),
                p75: percentile(&values, 0.75),
                p95: percentile(&values, 0.95),
            }
        })
        .collect()
}

/// Runs of readings below `LOW` / `VERY_LOW` or above `HIGH` /
/// `VERY_HIGH` lasting at least 15 minutes.
///
/// A run is readings on one side of the target range with no more than 15
/// minutes between one and the next, and lasts what its readings stand for
/// (see [`report`]): three readings five minutes apart are fifteen minutes.
/// A low run is `veryLow` if any reading in it is under 54, a high one
/// `veryHigh` if any is over 250; `extreme` is its lowest or highest reading
/// and `end` where the last reading's time ends.
pub fn events(points: &[SamplePoint]) -> Vec<GlucoseEvent> {
    let readings = sorted(points);
    event_runs(&readings, &weights_of(&readings))
}

fn event_runs(readings: &[&SamplePoint], weights: &[i64]) -> Vec<GlucoseEvent> {
    let mut out = Vec::new();
    let mut first = 0;
    while first < readings.len() {
        let Some(side) = side_of(readings[first].value) else {
            first += 1;
            continue;
        };
        let mut last = first;
        while let Some(next) = readings.get(last + 1)
            && side_of(next.value) == Some(side)
            && next.start.as_second() - readings[last].start.as_second() <= MAX_WEIGHT_SECS
        {
            last += 1;
        }
        if weights[first..=last].iter().sum::<i64>() >= EVENT_SECS {
            let run = &readings[first..=last];
            let (kind, extreme) = match side {
                Side::Low => {
                    let lowest = run.iter().map(|r| r.value).fold(f64::INFINITY, f64::min);
                    (if lowest < VERY_LOW { "veryLow" } else { "low" }, lowest)
                }
                Side::High => {
                    let highest = run.iter().map(|r| r.value).fold(f64::NEG_INFINITY, f64::max);
                    (if highest > VERY_HIGH { "veryHigh" } else { "high" }, highest)
                }
            };
            let ends = readings[last].start.as_second() + weights[last];
            out.push(GlucoseEvent {
                start: readings[first].start,
                end: Timestamp::from_second(ends).unwrap_or(readings[last].start),
                extreme,
                kind: kind.to_string(),
            });
        }
        first = last + 1;
    }
    out
}

/// What glucose did in the three hours after `meal`. `points` should cover
/// from 30 minutes before it.
///
/// The baseline is the latest reading at or up to 30 minutes before the
/// meal, or failing that the first within 10 minutes after it; without one
/// there is no rise, recovery or area, though the peak still stands. The
/// peak is the highest reading in the three hours (the first, if several
/// are as high), `rise` its excess over baseline (negative if glucose only
/// fell), and `recovered_after_min` when a reading after the peak was first
/// within 10 mg/dL above baseline. The area is the incremental one: what lies
/// above baseline over the first two hours, by the trapezoid rule on the
/// readings with those below baseline counted as baseline, starting from the
/// meal at baseline -- an estimate from however many readings there are.
pub fn meal_response(meal: &Meal, points: &[SamplePoint]) -> MealResponse {
    let at = meal.at.as_second();
    let secs_after = |p: &SamplePoint| p.start.as_second() - at;
    let mut readings = sorted(points);
    readings.retain(|p| (-BASELINE_BEFORE_SECS..=RESPONSE_SECS).contains(&secs_after(p)));
    let after: Vec<&SamplePoint> =
        readings.iter().copied().filter(|p| secs_after(p) >= 0).collect();

    let baseline = readings
        .iter()
        .rev()
        .find(|p| secs_after(p) <= 0)
        .or_else(|| after.iter().find(|p| secs_after(p) <= BASELINE_AFTER_SECS))
        .map(|p| p.value);

    let mut peak: Option<&SamplePoint> = None;
    for &reading in &after {
        if peak.is_none_or(|best| reading.value > best.value) {
            peak = Some(reading);
        }
    }
    let whole_minutes = |secs: i64| u32::try_from(secs / 60).ok();

    let recovered_after_min = baseline.zip(peak).and_then(|(baseline, peak)| {
        let recovered = after
            .iter()
            .find(|r| r.start > peak.start && r.value <= baseline + RECOVERED_WITHIN)?;
        whole_minutes(secs_after(recovered))
    });

    let iauc = baseline.and_then(|baseline| {
        let mut area = 0.0;
        let mut seen = false;
        // The meal itself, taken to be at baseline.
        let mut before = (0.0, 0.0);
        for reading in after.iter().filter(|p| secs_after(p) <= AREA_SECS) {
            let minutes = secs_after(reading) as f64 / 60.0;
            let above = (reading.value - baseline).max(0.0);
            area += (before.1 + above) / 2.0 * (minutes - before.0);
            before = (minutes, above);
            seen = true;
        }
        seen.then_some(area)
    });

    MealResponse {
        meal_id: meal.id,
        baseline,
        peak: peak.map(|p| p.value),
        peak_after_min: peak.and_then(|p| whole_minutes(secs_after(p))),
        rise: baseline.zip(peak).map(|(baseline, peak)| peak.value - baseline),
        recovered_after_min,
        iauc,
        points: readings.iter().copied().cloned().collect(),
        carbs_g: meal.items.iter().map(|item| item.nutrients.carbs_g).sum(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{MealItem, MealSlot, Nutrients};
    use super::*;
    use crate::id::MealId;
    use jiff::civil::date;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn nov(day: i8) -> Date {
        date(2026, 11, day)
    }

    fn reading(at: &str, value: f64) -> SamplePoint {
        SamplePoint { start: ts(at), end: ts(at), value, source: "Dexcom".into() }
    }

    /// Readings every `minutes` minutes from `from`, one per value.
    fn every(from: &str, minutes: i64, values: &[f64]) -> Vec<SamplePoint> {
        let first = ts(from).as_second();
        let at = |i: usize| Timestamp::from_second(first + i as i64 * minutes * 60).unwrap();
        let point = |(i, &value): (usize, &f64)| SamplePoint {
            start: at(i),
            end: at(i),
            value,
            source: "Dexcom".into(),
        };
        values.iter().enumerate().map(point).collect()
    }

    /// When an event began and ended.
    fn span(event: &GlucoseEvent) -> (Timestamp, Timestamp) {
        (event.start, event.end)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn utc() -> TimeZone {
        TimeZone::UTC
    }

    #[test]
    fn gmi_is_the_published_formula() {
        assert!(close(gmi(154.0), 3.31 + 0.02392 * 154.0));
        assert!((gmi(154.0) - 6.99).abs() < 0.01);
    }

    #[test]
    fn bands_are_shares_of_time_with_the_consensus_bounds() {
        // Eight readings five minutes apart, each standing for five.
        let points =
            every("2026-11-10T00:00:00Z", 5, &[53.9, 54.0, 69.9, 70.0, 180.0, 180.1, 250.0, 250.1]);
        let r = report(&points, nov(10), nov(10), &utc());
        assert_eq!(r.readings, 8);
        assert_eq!((r.from, r.to), (Some(nov(10)), Some(nov(10))));
        assert!(close(r.very_low, 0.125));
        assert!(close(r.low, 0.25));
        assert!(close(r.in_range, 0.25));
        assert!(close(r.high, 0.25));
        assert!(close(r.very_high, 0.125));
        assert!(close(r.very_low + r.low + r.in_range + r.high + r.very_high, 1.0));
        assert!(close(r.coverage, 40.0 / 1440.0));
    }

    #[test]
    fn a_reading_stands_for_fifteen_minutes_at_most_so_a_gap_costs_coverage() {
        // 00:00 (5 minutes to the next), 00:05 (15, not the 55 until 01:00),
        // 01:00 (5), 01:05 (5, the last): 30 minutes of the day.
        let points = vec![
            reading("2026-11-10T00:00:00Z", 100.0),
            reading("2026-11-10T00:05:00Z", 300.0),
            reading("2026-11-10T01:00:00Z", 100.0),
            reading("2026-11-10T01:05:00Z", 100.0),
        ];
        let r = report(&points, nov(10), nov(10), &utc());
        assert!(close(r.coverage, 30.0 / 1440.0));
        // The 300 stood for 15 of the 30 minutes.
        assert!(close(r.very_high, 0.5));
        assert!(close(r.in_range, 0.5));
    }

    #[test]
    fn readings_may_come_in_any_order_and_those_outside_the_dates_do_not_count() {
        let mut points = every("2026-11-10T12:00:00Z", 5, &[100.0, 110.0, 120.0]);
        points.reverse();
        points.push(reading("2026-11-09T23:55:00Z", 400.0));
        points.push(reading("2026-11-11T00:00:00Z", 400.0));
        let r = report(&points, nov(10), nov(10), &utc());
        assert_eq!(r.readings, 3);
        assert!(close(r.in_range, 1.0));
        assert_eq!(r.mean, Some(110.0));
    }

    #[test]
    fn mean_gmi_and_the_coefficient_of_variation() {
        let points = every("2026-11-10T00:00:00Z", 5, &[100.0, 140.0]);
        let r = report(&points, nov(10), nov(10), &utc());
        assert_eq!(r.mean, Some(120.0));
        assert!(close(r.gmi.unwrap(), 3.31 + 0.02392 * 120.0));
        // A population standard deviation of 20 over a mean of 120.
        assert!(close(r.cv.unwrap(), 20.0 / 120.0 * 100.0));
        let one = report(&points[..1], nov(10), nov(10), &utc());
        assert_eq!(one.cv, Some(0.0));
    }

    #[test]
    fn no_readings_is_an_empty_report_for_the_dates() {
        let r = report(&[], nov(10), nov(12), &utc());
        assert_eq!((r.from, r.to, r.readings), (Some(nov(10)), Some(nov(12)), 0));
        assert_eq!((r.coverage, r.mean, r.gmi, r.cv), (0.0, None, None, None));
        assert_eq!((r.in_range, r.very_low, r.very_high), (0.0, 0.0, 0.0));
        assert!(r.agp.is_empty() && r.events.is_empty());
        // A range that runs backwards holds nothing either.
        let points = every("2026-11-10T00:00:00Z", 5, &[100.0]);
        assert_eq!(report(&points, nov(12), nov(10), &utc()).readings, 0);
    }

    #[test]
    fn coverage_is_of_the_local_days_and_a_daylight_saving_day_is_twenty_five_hours() {
        let la = TimeZone::get("America/Los_Angeles").unwrap();
        // Clocks go back on 1 November 2026: the local day is 07:00Z to
        // 08:00Z on the 2nd. A reading every five minutes through all of it.
        let points = every("2026-11-01T07:00:00Z", 5, &[100.0; 300]);
        let r = report(&points, nov(1), nov(1), &la);
        assert_eq!(r.readings, 300);
        assert!(close(r.coverage, 1.0));
        // Half of that is half covered, however many hours the day has.
        let half = report(&points[..150], nov(1), nov(1), &la);
        assert!(close(half.coverage, 0.5));
    }

    #[test]
    fn the_agp_is_percentiles_by_quarter_hour_of_local_time() {
        // Five days of readings at 08:00 (UTC) and a lone one at 17:20.
        let mut points: Vec<SamplePoint> = [100.0, 120.0, 140.0, 160.0, 180.0]
            .iter()
            .enumerate()
            .map(|(i, &v)| reading(&format!("2026-11-{:02}T08:07:00Z", 10 + i), v))
            .collect();
        points.push(reading("2026-11-12T17:20:00Z", 90.0));
        let r = report(&points, nov(10), nov(14), &utc());
        assert_eq!(r.agp.len(), 2);
        let morning = &r.agp[0];
        assert_eq!(morning.minute, 8 * 60);
        // Linear interpolation: the 5th of five values is a fifth of the way
        // from the lowest to the next.
        assert!(close(morning.p5, 104.0));
        assert!(close(morning.p25, 120.0));
        assert!(close(morning.p50, 140.0));
        assert!(close(morning.p75, 160.0));
        assert!(close(morning.p95, 176.0));
        // A single reading is every percentile of its quarter hour.
        let evening = &r.agp[1];
        assert_eq!(evening.minute, 17 * 60 + 15);
        assert_eq!((evening.p5, evening.p50, evening.p95), (90.0, 90.0, 90.0));
    }

    #[test]
    fn the_agp_reads_the_clock_where_the_person_was() {
        // 16:07Z is 08:07 in Los Angeles in November.
        let la = TimeZone::get("America/Los_Angeles").unwrap();
        let points = every("2026-11-10T16:07:00Z", 5, &[100.0]);
        let r = report(&points, nov(10), nov(10), &la);
        assert_eq!(r.agp.len(), 1);
        assert_eq!(r.agp[0].minute, 8 * 60);
    }

    /// Readings every five minutes, from midnight on the 10th, with the
    /// stretches that make the events below.
    fn eventful() -> Vec<SamplePoint> {
        let mut values = vec![100.0; 16];
        // 00:05-00:15: three readings under 70.
        values[1..4].copy_from_slice(&[65.0, 60.0, 66.0]);
        // 00:25-00:35: three under 70, one under 54.
        values[5..8].copy_from_slice(&[52.0, 55.0, 58.0]);
        // 00:45-00:50: two over 180, but only ten minutes.
        values[9..11].copy_from_slice(&[200.0, 210.0]);
        // 01:00-01:10: three over 180, one over 250.
        values[12..15].copy_from_slice(&[190.0, 260.0, 255.0]);
        every("2026-11-10T00:00:00Z", 5, &values)
    }

    #[test]
    fn events_are_runs_of_at_least_fifteen_minutes_outside_the_range() {
        let got = events(&eventful());
        let summary: Vec<(&str, f64)> = got.iter().map(|e| (e.kind.as_str(), e.extreme)).collect();
        assert_eq!(summary, vec![("low", 60.0), ("veryLow", 52.0), ("veryHigh", 260.0)]);
        // From the first reading to where the last one's five minutes end.
        let between = |from, to| (ts(from), ts(to));
        assert_eq!(span(&got[0]), between("2026-11-10T00:05:00Z", "2026-11-10T00:20:00Z"));
        assert_eq!(span(&got[1]), between("2026-11-10T00:25:00Z", "2026-11-10T00:40:00Z"));
        assert_eq!(span(&got[2]), between("2026-11-10T01:00:00Z", "2026-11-10T01:15:00Z"));
    }

    #[test]
    fn a_high_that_never_passes_250_is_just_high_and_reports_its_highest_reading() {
        let points = every("2026-11-10T00:00:00Z", 5, &[150.0, 190.0, 240.0, 200.0, 150.0]);
        let got = events(&points);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].kind.as_str(), got[0].extreme), ("high", 240.0));
    }

    #[test]
    fn a_low_beside_a_high_is_two_short_runs_and_not_one_event() {
        let points = every("2026-11-10T00:00:00Z", 5, &[65.0, 65.0, 190.0, 190.0, 100.0]);
        assert!(events(&points).is_empty());
    }

    #[test]
    fn a_gap_of_more_than_fifteen_minutes_ends_a_run() {
        // Three lows, a half hour of nothing, two more. Were the runs joined
        // it would be one event to 00:55; it is the first run, to the
        // 15 minutes the reading before the gap stands for.
        let mut points = every("2026-11-10T00:00:00Z", 5, &[60.0, 60.0, 60.0]);
        points.extend(every("2026-11-10T00:45:00Z", 5, &[60.0, 60.0]));
        let got = events(&points);
        assert_eq!(got.len(), 1);
        assert_eq!(span(&got[0]), (ts("2026-11-10T00:00:00Z"), ts("2026-11-10T00:25:00Z")));
    }

    #[test]
    fn the_report_carries_the_events() {
        let r = report(&eventful(), nov(10), nov(10), &utc());
        assert_eq!(r.events.len(), 3);
        assert_eq!(r.events, events(&eventful()));
    }

    /// A meal of `carbs` grams at `at`.
    fn meal(at: &str, carbs: &[f64]) -> Meal {
        let now = ts(at);
        Meal {
            id: MealId::new(),
            at: now,
            offset_min: 0,
            slot: MealSlot::default(),
            name: String::new(),
            items: carbs
                .iter()
                .map(|&carbs_g| MealItem {
                    food_id: None,
                    name: String::new(),
                    amount: 1.0,
                    unit: "g".into(),
                    grams: None,
                    nutrients: Nutrients { carbs_g, ..Nutrients::default() },
                    estimated: false,
                })
                .collect(),
            photo: None,
            note: String::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Readings at these minutes from noon, with these values.
    fn around_noon(readings: &[(i64, f64)]) -> Vec<SamplePoint> {
        let noon = ts("2026-11-10T12:00:00Z").as_second();
        readings
            .iter()
            .map(|&(minute, value)| {
                let at = Timestamp::from_second(noon + minute * 60).unwrap();
                SamplePoint { start: at, end: at, value, source: "Dexcom".into() }
            })
            .collect()
    }

    fn rising_meal() -> Vec<SamplePoint> {
        around_noon(&[
            (-45, 90.0),
            (-30, 100.0),
            (-10, 104.0),
            (10, 130.0),
            (30, 190.0),
            (60, 210.0),
            (90, 150.0),
            (100, 112.0),
            (120, 100.0),
            (150, 98.0),
            (180, 99.0),
            (200, 97.0),
        ])
    }

    #[test]
    fn a_meal_response_has_baseline_peak_rise_recovery_and_area() {
        let m = meal("2026-11-10T12:00:00Z", &[30.0, 15.5]);
        let r = meal_response(&m, &rising_meal());
        assert_eq!(r.meal_id, m.id);
        // The latest reading before the meal, not the one 15 minutes earlier.
        assert_eq!(r.baseline, Some(104.0));
        assert_eq!((r.peak, r.peak_after_min), (Some(210.0), Some(60)));
        assert_eq!(r.rise, Some(106.0));
        // 112 at 100 minutes is the first after the peak within 104 + 10.
        assert_eq!(r.recovered_after_min, Some(100));
        // Above 104, trapezoids from the meal (at baseline) to two hours:
        // 130 + 1120 + 2880 + 2280 + 270 + 80.
        assert!(close(r.iauc.unwrap(), 6760.0));
        assert_eq!(r.carbs_g, 45.5);
        // From 30 minutes before to 3 hours after, and no further.
        assert_eq!(r.points.len(), 10);
        assert_eq!(r.points[0].value, 100.0);
        assert_eq!(r.points[9].value, 99.0);
    }

    #[test]
    fn with_no_reading_before_the_meal_the_first_within_ten_minutes_after_is_the_baseline() {
        let m = meal("2026-11-10T12:00:00Z", &[]);
        let points = around_noon(&[(8, 120.0), (30, 160.0), (60, 130.0), (90, 125.0)]);
        let r = meal_response(&m, &points);
        assert_eq!(r.baseline, Some(120.0));
        assert_eq!((r.peak, r.rise), (Some(160.0), Some(40.0)));
        assert_eq!(r.recovered_after_min, Some(60));
        assert_eq!(r.carbs_g, 0.0);

        // Twelve minutes is too late: no baseline, so no rise, recovery or
        // area, but the peak is still the peak.
        let r = meal_response(&m, &around_noon(&[(12, 120.0), (30, 160.0)]));
        assert_eq!(r.baseline, None);
        assert_eq!((r.peak, r.peak_after_min), (Some(160.0), Some(30)));
        assert_eq!((r.rise, r.recovered_after_min, r.iauc), (None, None, None));
    }

    #[test]
    fn a_reading_at_the_meal_is_the_baseline_and_the_start_of_the_area() {
        let m = meal("2026-11-10T12:00:00Z", &[]);
        let r = meal_response(&m, &around_noon(&[(0, 100.0), (30, 130.0), (60, 100.0)]));
        assert_eq!(r.baseline, Some(100.0));
        // A triangle of height 30 and base 60 minutes.
        assert!(close(r.iauc.unwrap(), 900.0));
        assert_eq!((r.peak, r.peak_after_min), (Some(130.0), Some(30)));
        assert_eq!(r.recovered_after_min, Some(60));
    }

    #[test]
    fn readings_below_baseline_add_no_area_and_a_fall_is_a_negative_rise() {
        let m = meal("2026-11-10T12:00:00Z", &[]);
        let r = meal_response(&m, &around_noon(&[(-5, 120.0), (30, 90.0), (60, 80.0)]));
        assert_eq!(r.baseline, Some(120.0));
        assert_eq!(r.peak, Some(90.0));
        assert_eq!(r.rise, Some(-30.0));
        assert_eq!(r.iauc, Some(0.0));
    }

    #[test]
    fn the_area_stops_at_two_hours_and_the_peak_does_not() {
        let m = meal("2026-11-10T12:00:00Z", &[]);
        let points = around_noon(&[(-5, 100.0), (60, 150.0), (120, 100.0), (150, 300.0)]);
        let r = meal_response(&m, &points);
        // 50 high at the hour, back to baseline at two hours: 3000 over the
        // two hours; the 300 at 150 minutes is after the area's window.
        assert!(close(r.iauc.unwrap(), 3000.0));
        assert_eq!((r.peak, r.peak_after_min), (Some(300.0), Some(150)));
    }

    #[test]
    fn a_meal_with_no_readings_has_nothing_to_say() {
        let m = meal("2026-11-10T12:00:00Z", &[20.0]);
        let r = meal_response(&m, &[]);
        assert_eq!((r.baseline, r.peak, r.peak_after_min), (None, None, None));
        assert_eq!((r.rise, r.recovered_after_min, r.iauc), (None, None, None));
        assert!(r.points.is_empty());
        assert_eq!(r.carbs_g, 20.0);
        // Readings only from before it: a baseline and nothing else.
        let r = meal_response(&m, &around_noon(&[(-20, 100.0)]));
        assert_eq!(r.baseline, Some(100.0));
        assert_eq!((r.peak, r.rise, r.iauc), (None, None, None));
    }
}
