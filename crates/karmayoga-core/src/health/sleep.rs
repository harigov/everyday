//! Sleep: stage samples into nights, and a night's score.
//!
//! A watch writes sleep as stretches of stages (core, deep, REM, awake) and
//! the phone may write "in bed" around them; another app may write plain
//! "asleep" over the same hours. [`segments`] keeps one story per stretch of
//! time -- staged sleep over unstaged, the best-ranked source over others --
//! and [`nights`] cuts it into nights filed under the local date each ended
//! on.

use super::chunk;
use super::{SamplePoint, SeriesDay, SleepNight, SleepSegment, SleepStage};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use std::collections::{BTreeMap, HashMap};

/// A gap longer than this between segments starts a new night.
pub const NIGHT_GAP_MINUTES: i64 = 180;

/// Less asleep than this, ending after noon, is a nap and not a night.
const NAP_MINUTES: f64 = 180.0;
/// Noon, in seconds into the local day: where "a nap" starts to be a nap.
const NOON_SECS: i32 = 12 * 3600;

// How much each part counts towards `score`.
const DURATION_WEIGHT: f64 = 0.5;
const EFFICIENCY_WEIGHT: f64 = 0.2;
const RESTORATIVE_WEIGHT: f64 = 0.15;
const REGULARITY_WEIGHT: f64 = 0.15;

/// One stretch of one stage as a source wrote it, in epoch seconds, with
/// where that source ranks (lower is more trusted).
struct Piece {
    stage: SleepStage,
    start: i64,
    end: i64,
    rank: usize,
}

impl Piece {
    /// 0 for a stage somebody measured, 1 for plain "asleep", which only
    /// says the person was out and is overruled by any stage over the same
    /// minutes. (Awake is a measured stage: a watch seeing you awake beats an
    /// app that guessed you slept through.)
    fn class(&self) -> u8 {
        u8::from(self.stage == SleepStage::Asleep)
    }
}

/// The sleep-stage days' samples as non-overlapping segments, in order.
/// `rank` is the source priority ([`super::chunk::ranker`]).
///
/// Where two stretches overlap, staged sleep beats plain asleep and, between
/// equals, the better-ranked source wins; the loser is clipped to what the
/// winner leaves rather than dropped, so an app's "asleep 22:30 to 06:30"
/// still fills the hours a watch had nothing to say about. In-bed stretches
/// are their own layer, resolved among themselves the same way but never
/// against sleep, since in bed *contains* sleep.
pub fn segments(days: &[SeriesDay], rank: &dyn Fn(&str) -> usize) -> Vec<SleepSegment> {
    let mut sleep = Vec::new();
    let mut in_bed = Vec::new();
    for day in days {
        for sample in &day.samples {
            let Some(stage) = SleepStage::from_code(sample.value) else { continue };
            let end = chunk::end_of(sample);
            if end <= sample.start {
                continue;
            }
            let source = day.sources.get(usize::from(sample.source)).map_or("", String::as_str);
            let piece = Piece { stage, start: sample.start, end, rank: rank(source) };
            if stage == SleepStage::InBed {
                in_bed.push(piece);
            } else {
                sleep.push(piece);
            }
        }
    }
    let mut out = resolve(&sleep);
    out.extend(resolve(&in_bed));
    out.sort_by_key(|s| (s.start, s.end));
    out
}

/// Cut `pieces` into non-overlapping segments, each stretch of time going to
/// the piece that wins it: lowest class, then best rank, then first given.
/// A sweep over the pieces' boundaries, so a night with a few hundred
/// stretches costs a few hundred steps.
fn resolve(pieces: &[Piece]) -> Vec<SleepSegment> {
    let mut bounds: Vec<i64> = pieces.iter().flat_map(|p| [p.start, p.end]).collect();
    bounds.sort_unstable();
    bounds.dedup();
    let mut by_start: Vec<usize> = (0..pieces.len()).collect();
    by_start.sort_by_key(|&i| pieces[i].start);

    let mut next = 0;
    let mut active: Vec<usize> = Vec::new();
    // (winning piece, start, end), adjacent stretches of one piece joined.
    let mut won: Vec<(usize, i64, i64)> = Vec::new();
    for pair in bounds.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        while next < by_start.len() && pieces[by_start[next]].start <= a {
            active.push(by_start[next]);
            next += 1;
        }
        active.retain(|&i| pieces[i].end > a);
        let best = |&&i: &&usize| (pieces[i].class(), pieces[i].rank, i);
        let Some(&winner) = active.iter().min_by_key(best) else { continue };
        if let Some(last) = won.last_mut()
            && last.0 == winner
            && last.2 == a
        {
            last.2 = b;
        } else {
            won.push((winner, a, b));
        }
    }
    won.into_iter()
        .filter_map(|(i, a, b)| {
            Some(SleepSegment {
                stage: pieces[i].stage,
                start: Timestamp::from_second(a).ok()?,
                end: Timestamp::from_second(b).ok()?,
            })
        })
        .collect()
}

/// Seconds a segment lasts.
fn secs(s: &SleepSegment) -> i64 {
    s.end.as_second() - s.start.as_second()
}

/// Nights ending on local dates `from..=to` in `tz`. Naps (under three
/// hours, ending after noon) are not nights.
///
/// The segments are clustered, a gap of more than [`NIGHT_GAP_MINUTES`]
/// starting a new cluster, and a cluster is filed under the local date of its
/// last segment's end -- so sleep from 23:00 to 07:00 is the night of the
/// morning. Somebody awake for more than three hours in the middle of the
/// night has two. A cluster with no sleep in it (only awake time) is not a
/// night. In-bed time is the union of the in-bed segments that overlap the
/// cluster, or, where there are none, its whole span.
pub fn nights(segments: &[SleepSegment], from: Date, to: Date, tz: &TimeZone) -> Vec<SleepNight> {
    let mut sleep: Vec<&SleepSegment> =
        segments.iter().filter(|s| s.stage != SleepStage::InBed && secs(s) > 0).collect();
    sleep.sort_by_key(|s| (s.start, s.end));
    let in_bed: Vec<&SleepSegment> =
        segments.iter().filter(|s| s.stage == SleepStage::InBed && secs(s) > 0).collect();

    let mut clusters: Vec<Vec<&SleepSegment>> = Vec::new();
    let mut current: Vec<&SleepSegment> = Vec::new();
    // The latest end in `current`.
    let mut reach = 0;
    for s in sleep {
        if !current.is_empty() && s.start.as_second() - reach > NIGHT_GAP_MINUTES * 60 {
            clusters.push(std::mem::take(&mut current));
        }
        reach = if current.is_empty() { s.end.as_second() } else { reach.max(s.end.as_second()) };
        current.push(s);
    }
    if !current.is_empty() {
        clusters.push(current);
    }

    clusters
        .iter()
        .filter_map(|cluster| night_of(cluster, &in_bed, tz))
        .filter(|night| night.date >= from && night.date <= to)
        .collect()
}

/// One cluster of sleep segments as a night, or none when it is a nap or
/// holds no sleep.
fn night_of(
    cluster: &[&SleepSegment],
    in_bed: &[&SleepSegment],
    tz: &TimeZone,
) -> Option<SleepNight> {
    let first = cluster.iter().map(|s| s.start).min()?;
    let last = cluster.iter().map(|s| s.end).max()?;

    let mut minutes: HashMap<SleepStage, f64> = HashMap::new();
    for s in cluster {
        *minutes.entry(s.stage).or_insert(0.0) += secs(s) as f64 / 60.0;
    }
    let of = |stage: SleepStage| minutes.get(&stage).copied().unwrap_or(0.0);
    let (core, deep, rem) = (of(SleepStage::Core), of(SleepStage::Deep), of(SleepStage::Rem));
    let asleep = core + deep + rem + of(SleepStage::Asleep);
    if asleep <= 0.0 {
        return None;
    }

    let local_end = tz.to_datetime(last);
    let into_day = i32::from(local_end.hour()) * 3600
        + i32::from(local_end.minute()) * 60
        + i32::from(local_end.second());
    if asleep < NAP_MINUTES && into_day > NOON_SECS {
        return None;
    }

    // Asleep from the first stage that is not awake to the last.
    let slept: Vec<&SleepSegment> =
        cluster.iter().copied().filter(|s| s.stage != SleepStage::Awake).collect();
    let sleep_start = slept.iter().map(|s| s.start).min();
    let sleep_end = slept.iter().map(|s| s.end).max();

    let mut beds: Vec<&SleepSegment> =
        in_bed.iter().copied().filter(|s| s.start < last && s.end > first).collect();
    beds.sort_by_key(|s| (s.start, s.end));
    let in_bed_min = if beds.is_empty() {
        (last.as_second() - first.as_second()) as f64 / 60.0
    } else {
        union_secs(&beds) as f64 / 60.0
    };

    let mut all: Vec<SleepSegment> =
        cluster.iter().chain(beds.iter()).map(|s| (**s).clone()).collect();
    all.sort_by_key(|s| (s.start, s.end));

    Some(SleepNight {
        date: local_end.date(),
        in_bed_start: beds.first().map(|s| s.start),
        sleep_start,
        sleep_end,
        asleep_min: asleep,
        in_bed_min,
        awake_min: of(SleepStage::Awake),
        core_min: core,
        deep_min: deep,
        rem_min: rem,
        efficiency: (in_bed_min > 0.0).then_some((asleep / in_bed_min).min(1.0)),
        hr: None,
        hrv_ms: None,
        respiratory_rate: None,
        score: None,
        score_parts: BTreeMap::new(),
        segments: all,
    })
}

/// Seconds covered by `segments` (sorted by start), overlaps counted once.
fn union_secs(segments: &[&SleepSegment]) -> i64 {
    let mut total = 0;
    let mut reach = i64::MIN;
    for s in segments {
        let (start, end) = (s.start.as_second().max(reach), s.end.as_second());
        if end > start {
            total += end - start;
        }
        reach = reach.max(end);
    }
    total
}

/// Fill in a night's heart rate, HRV and breathing from samples taken
/// while it was asleep: the mean of those that start between
/// `sleep_start` and `sleep_end`. A measure with nothing in that window is
/// left as it was.
pub fn enrich(
    night: &mut SleepNight,
    heart_rate: &[SamplePoint],
    hrv: &[SamplePoint],
    respiratory: &[SamplePoint],
) {
    let (Some(start), Some(end)) = (night.sleep_start, night.sleep_end) else { return };
    let mean = |points: &[SamplePoint]| {
        let inside: Vec<f64> =
            points.iter().filter(|p| p.start >= start && p.start < end).map(|p| p.value).collect();
        (!inside.is_empty()).then(|| inside.iter().sum::<f64>() / inside.len() as f64)
    };
    night.hr = mean(heart_rate).or(night.hr);
    night.hrv_ms = mean(hrv).or(night.hrv_ms);
    night.respiratory_rate = mean(respiratory).or(night.respiratory_rate);
}

/// `value` as a share of the way from `zero` (0) to `full` (100), clamped.
fn ramp(value: f64, zero: f64, full: f64) -> f64 {
    (100.0 * (value - zero) / (full - zero)).clamp(0.0, 100.0)
}

/// How far the middles of the nights in `recent` wander, as the population
/// standard deviation in minutes. Each middle is measured from the start of
/// its own date, so midnight is no cliff and a night that ends after
/// midnight sits beside one that began before it. `None` for fewer than
/// three nights with times.
///
/// This has no zone to read the clock in, so it reads the instants' own
/// (UTC): a constant offset changes nothing, a change of clocks inside the
/// window shows as the hour it moved.
fn midpoint_spread(recent: &[SleepNight]) -> Option<f64> {
    let spots: Vec<f64> = recent
        .iter()
        .filter_map(|n| {
            let (start, end) = (n.sleep_start?.as_second(), n.sleep_end?.as_second());
            let midnight = TimeZone::UTC.to_timestamp(n.date.at(0, 0, 0, 0)).ok()?.as_second();
            Some(((start + end) / 2 - midnight) as f64 / 60.0)
        })
        .collect();
    if spots.len() < 3 {
        return None;
    }
    let mean = spots.iter().sum::<f64>() / spots.len() as f64;
    let variance = spots.iter().map(|m| (m - mean).powi(2)).sum::<f64>() / spots.len() as f64;
    Some(variance.sqrt())
}

/// This app's own 0..=100 score and its parts: duration against the goal,
/// efficiency, deep and REM share, and how regular bed and wake times were
/// over `recent` nights. A heuristic, said so on screen, with every part
/// shown.
///
/// Each part is 0..=100 and the score is their weighted mean:
///
/// - `duration` (0.5): full from the goal to an hour and a half past it,
///   falling in a line to 0 at none below it, and by ten points an hour
///   beyond that, to no less than 50 -- long sleep is not penalised hard.
/// - `efficiency` (0.2): full at 0.9 asleep over in bed, 0 at 0.6. Left out,
///   and the other weights stretched to fill, when the night has none.
/// - `restorative` (0.15): deep plus REM as a share of sleep, full at 0.4.
/// - `regularity` (0.15): the spread of sleep midpoints over `recent`, full
///   at 30 minutes or less, 0 at 120 or more; full too when there are fewer
///   than three nights to judge by.
pub fn score(
    night: &SleepNight,
    goal_hours: f64,
    recent: &[SleepNight],
) -> (f64, BTreeMap<String, f64>) {
    let asleep_hours = night.asleep_min / 60.0;
    let duration = if goal_hours <= 0.0 {
        100.0
    } else if asleep_hours <= goal_hours + 1.5 {
        ramp(asleep_hours, 0.0, goal_hours)
    } else {
        (100.0 - 10.0 * (asleep_hours - goal_hours - 1.5)).max(50.0)
    };
    let restorative = if night.asleep_min > 0.0 {
        ramp((night.deep_min + night.rem_min) / night.asleep_min, 0.0, 0.4)
    } else {
        0.0
    };
    let regularity = midpoint_spread(recent).map_or(100.0, |sd| ramp(sd, 120.0, 30.0));

    let mut parts = vec![
        ("duration", duration, DURATION_WEIGHT),
        ("restorative", restorative, RESTORATIVE_WEIGHT),
        ("regularity", regularity, REGULARITY_WEIGHT),
    ];
    if let Some(efficiency) = night.efficiency {
        parts.push(("efficiency", ramp(efficiency, 0.6, 0.9), EFFICIENCY_WEIGHT));
    }
    let weight: f64 = parts.iter().map(|(_, _, w)| w).sum();
    let total: f64 = parts.iter().map(|(_, part, w)| part * w).sum();
    let named = parts.into_iter().map(|(name, part, _)| (name.to_string(), part)).collect();
    ((total / weight).clamp(0.0, 100.0), named)
}

#[cfg(test)]
mod tests {
    use super::super::{Metric, Sample};
    use super::*;
    use jiff::civil::date;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn utc() -> TimeZone {
        TimeZone::UTC
    }

    fn nov(day: i8) -> Date {
        date(2026, 11, day)
    }

    /// The watch is trusted over everything else.
    fn rank(source: &str) -> usize {
        if source == "Watch" { 0 } else { 1 }
    }

    /// A day of stage samples from `sources`: (source index, stage, start,
    /// end).
    fn series(sources: &[&str], samples: &[(u16, SleepStage, &str, &str)]) -> SeriesDay {
        let sample = |(source, stage, start, end): &(u16, SleepStage, &str, &str)| {
            let (start, end) = (ts(start).as_second(), ts(end).as_second());
            Sample {
                start,
                secs: (end - start) as u32,
                value: stage.code(),
                source: *source,
                offset_min: 0,
            }
        };
        SeriesDay {
            metric: Some(Metric::SleepStage),
            day: None,
            sources: sources.iter().map(|s| s.to_string()).collect(),
            samples: samples.iter().map(sample).collect(),
            tombstones: Vec::new(),
        }
    }

    fn seg(stage: SleepStage, start: &str, end: &str) -> SleepSegment {
        SleepSegment { stage, start: ts(start), end: ts(end) }
    }

    /// What a segment is, as something `assert_eq!` can print.
    type Shape = (SleepStage, Timestamp, Timestamp);

    fn shape(segments: &[SleepSegment]) -> Vec<Shape> {
        segments.iter().map(|s| (s.stage, s.start, s.end)).collect()
    }

    fn entry(stage: SleepStage, start: &str, end: &str) -> Shape {
        (stage, ts(start), ts(end))
    }

    #[test]
    fn staged_sleep_overrules_plain_asleep_and_clips_it_rather_than_dropping_it() {
        let day = series(
            &["Watch", "Sleep app"],
            &[
                (1, SleepStage::Asleep, "2026-11-09T22:30:00Z", "2026-11-10T06:30:00Z"),
                (0, SleepStage::Core, "2026-11-09T23:00:00Z", "2026-11-10T01:00:00Z"),
                (0, SleepStage::Awake, "2026-11-10T03:00:00Z", "2026-11-10T03:20:00Z"),
            ],
        );
        assert_eq!(
            shape(&segments(&[day], &rank)),
            vec![
                entry(SleepStage::Asleep, "2026-11-09T22:30:00Z", "2026-11-09T23:00:00Z"),
                entry(SleepStage::Core, "2026-11-09T23:00:00Z", "2026-11-10T01:00:00Z"),
                entry(SleepStage::Asleep, "2026-11-10T01:00:00Z", "2026-11-10T03:00:00Z"),
                entry(SleepStage::Awake, "2026-11-10T03:00:00Z", "2026-11-10T03:20:00Z"),
                entry(SleepStage::Asleep, "2026-11-10T03:20:00Z", "2026-11-10T06:30:00Z"),
            ]
        );
    }

    #[test]
    fn between_equal_stages_the_better_ranked_source_wins() {
        let day = series(
            &["Watch", "Ring"],
            &[
                (0, SleepStage::Deep, "2026-11-10T00:00:00Z", "2026-11-10T01:00:00Z"),
                (1, SleepStage::Rem, "2026-11-10T00:30:00Z", "2026-11-10T02:00:00Z"),
            ],
        );
        assert_eq!(
            shape(&segments(std::slice::from_ref(&day), &rank)),
            vec![
                entry(SleepStage::Deep, "2026-11-10T00:00:00Z", "2026-11-10T01:00:00Z"),
                entry(SleepStage::Rem, "2026-11-10T01:00:00Z", "2026-11-10T02:00:00Z"),
            ]
        );
        // The same samples with the ring trusted instead.
        let ring_first = |source: &str| if source == "Ring" { 0 } else { 1 };
        assert_eq!(
            shape(&segments(&[day], &ring_first)),
            vec![
                entry(SleepStage::Deep, "2026-11-10T00:00:00Z", "2026-11-10T00:30:00Z"),
                entry(SleepStage::Rem, "2026-11-10T00:30:00Z", "2026-11-10T02:00:00Z"),
            ]
        );
    }

    #[test]
    fn in_bed_is_its_own_layer_and_everything_comes_back_in_start_order() {
        let day = series(
            &["Watch", "iPhone"],
            &[
                (0, SleepStage::Core, "2026-11-09T23:00:00Z", "2026-11-10T02:00:00Z"),
                (1, SleepStage::InBed, "2026-11-09T22:30:00Z", "2026-11-10T07:00:00Z"),
                (0, SleepStage::InBed, "2026-11-09T22:45:00Z", "2026-11-10T07:30:00Z"),
            ],
        );
        // The watch's in-bed (better ranked) wins where the two overlap; the
        // phone's is clipped to the quarter hour before it. Sleep is untouched.
        assert_eq!(
            shape(&segments(&[day], &rank)),
            vec![
                entry(SleepStage::InBed, "2026-11-09T22:30:00Z", "2026-11-09T22:45:00Z"),
                entry(SleepStage::InBed, "2026-11-09T22:45:00Z", "2026-11-10T07:30:00Z"),
                entry(SleepStage::Core, "2026-11-09T23:00:00Z", "2026-11-10T02:00:00Z"),
            ]
        );
    }

    #[test]
    fn samples_of_two_utc_days_make_one_story_and_junk_is_skipped() {
        let late = series(
            &["Watch"],
            &[(0, SleepStage::Core, "2026-11-09T23:00:00Z", "2026-11-10T00:00:00Z")],
        );
        let mut early = series(
            &["Watch"],
            &[(0, SleepStage::Deep, "2026-11-10T00:00:00Z", "2026-11-10T01:00:00Z")],
        );
        // A stage code nobody defined, a sample with no duration, and one
        // naming a source the day does not list.
        let extra = |at: &str, secs: u32, value: f64, source: u16| Sample {
            start: ts(at).as_second(),
            secs,
            value,
            source,
            offset_min: 0,
        };
        early.samples.push(extra("2026-11-10T01:00:00Z", 600, 9.0, 0));
        early.samples.push(extra("2026-11-10T01:00:00Z", 0, 3.0, 0));
        early.samples.push(extra("2026-11-10T02:00:00Z", 600, 5.0, 7));
        assert_eq!(
            shape(&segments(&[early, late], &rank)),
            vec![
                entry(SleepStage::Core, "2026-11-09T23:00:00Z", "2026-11-10T00:00:00Z"),
                entry(SleepStage::Deep, "2026-11-10T00:00:00Z", "2026-11-10T01:00:00Z"),
                entry(SleepStage::Rem, "2026-11-10T02:00:00Z", "2026-11-10T02:10:00Z"),
            ]
        );
    }

    /// 23:00 to 07:00 across midnight: 2 h core, 2 h deep, 2 h REM, 1.5 h
    /// core, half an hour awake at the end.
    fn night_segments() -> Vec<SleepSegment> {
        vec![
            seg(SleepStage::Core, "2026-11-09T23:00:00Z", "2026-11-10T01:00:00Z"),
            seg(SleepStage::Deep, "2026-11-10T01:00:00Z", "2026-11-10T03:00:00Z"),
            seg(SleepStage::Rem, "2026-11-10T03:00:00Z", "2026-11-10T05:00:00Z"),
            seg(SleepStage::Core, "2026-11-10T05:00:00Z", "2026-11-10T06:30:00Z"),
            seg(SleepStage::Awake, "2026-11-10T06:30:00Z", "2026-11-10T07:00:00Z"),
        ]
    }

    #[test]
    fn a_night_across_midnight_is_filed_under_the_day_it_ended() {
        let mut segs = night_segments();
        segs.push(seg(SleepStage::InBed, "2026-11-09T22:30:00Z", "2026-11-10T07:15:00Z"));
        let got = nights(&segs, nov(1), nov(30), &utc());
        assert_eq!(got.len(), 1);
        let n = &got[0];
        assert_eq!(n.date, nov(10));
        assert_eq!((n.core_min, n.deep_min, n.rem_min, n.awake_min), (210.0, 120.0, 120.0, 30.0));
        assert_eq!((n.asleep_min, n.in_bed_min), (450.0, 525.0));
        assert_eq!(n.efficiency, Some(450.0 / 525.0));
        assert_eq!(n.in_bed_start, Some(ts("2026-11-09T22:30:00Z")));
        assert_eq!(n.sleep_start, Some(ts("2026-11-09T23:00:00Z")));
        // Asleep until the last stage that was not awake.
        assert_eq!(n.sleep_end, Some(ts("2026-11-10T06:30:00Z")));
        // The five stages and the in-bed stretch, in start order.
        assert_eq!(n.segments.len(), 6);
        assert_eq!(n.segments[0].stage, SleepStage::InBed);
        assert_eq!(n.segments[1].stage, SleepStage::Core);

        assert!(nights(&segs, nov(11), nov(30), &utc()).is_empty());
        assert!(nights(&segs, nov(1), nov(9), &utc()).is_empty());
    }

    #[test]
    fn without_in_bed_samples_the_span_stands_in() {
        let got = nights(&night_segments(), nov(10), nov(10), &utc());
        let n = &got[0];
        assert_eq!(n.in_bed_min, 480.0);
        assert_eq!(n.in_bed_start, None);
        assert_eq!(n.efficiency, Some(450.0 / 480.0));
        assert_eq!(n.segments.len(), 5);
    }

    #[test]
    fn the_date_is_the_local_one() {
        // 23:00 to 07:00 in Tokyo (UTC+9) is 14:00Z to 22:00Z, all on the
        // UTC 10th, but the night of the 11th.
        let segs = [seg(SleepStage::Core, "2026-11-10T14:00:00Z", "2026-11-10T22:00:00Z")];
        let tokyo = TimeZone::get("Asia/Tokyo").unwrap();
        assert_eq!(nights(&segs, nov(1), nov(30), &tokyo)[0].date, nov(11));
        assert_eq!(nights(&segs, nov(1), nov(30), &utc())[0].date, nov(10));
    }

    #[test]
    fn a_short_afternoon_sleep_is_a_nap_and_a_short_morning_one_is_not() {
        let mut segs = night_segments();
        // An hour at 14:00, four and a half hours from 09:00 to 13:30 the
        // day after (a shift worker's night, which is a night), and two
        // hours that end before noon, which still count.
        segs.push(seg(SleepStage::Core, "2026-11-10T14:00:00Z", "2026-11-10T15:00:00Z"));
        segs.push(seg(SleepStage::Core, "2026-11-11T09:00:00Z", "2026-11-11T13:30:00Z"));
        segs.push(seg(SleepStage::Core, "2026-11-12T08:00:00Z", "2026-11-12T10:00:00Z"));
        let got = nights(&segs, nov(1), nov(30), &utc());
        assert_eq!(got.iter().map(|n| n.date).collect::<Vec<_>>(), vec![nov(10), nov(11), nov(12)]);
        assert_eq!((got[1].asleep_min, got[2].asleep_min), (270.0, 120.0));
    }

    #[test]
    fn a_nap_is_judged_on_the_local_clock_and_noon_itself_is_still_morning() {
        // 02:00Z to 04:00Z is 18:00 to 20:00 the evening before in Los Angeles.
        let la = TimeZone::get("America/Los_Angeles").unwrap();
        let segs = [seg(SleepStage::Core, "2026-11-10T02:00:00Z", "2026-11-10T04:00:00Z")];
        assert!(nights(&segs, nov(1), nov(30), &la).is_empty());
        assert_eq!(nights(&segs, nov(1), nov(30), &utc()).len(), 1);
        // Ending at exactly 12:00 keeps; one minute later does not.
        let at_noon = [seg(SleepStage::Core, "2026-11-10T10:00:00Z", "2026-11-10T12:00:00Z")];
        let after = [seg(SleepStage::Core, "2026-11-10T10:01:00Z", "2026-11-10T12:01:00Z")];
        assert_eq!(nights(&at_noon, nov(10), nov(10), &utc()).len(), 1);
        assert!(nights(&after, nov(10), nov(10), &utc()).is_empty());
    }

    #[test]
    fn more_than_three_hours_awake_splits_a_night_and_exactly_three_does_not() {
        let kept = [
            seg(SleepStage::Core, "2026-11-09T23:00:00Z", "2026-11-10T01:00:00Z"),
            seg(SleepStage::Core, "2026-11-10T04:00:00Z", "2026-11-10T06:00:00Z"),
        ];
        let one = nights(&kept, nov(1), nov(30), &utc());
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].asleep_min, 240.0);
        let split = [
            seg(SleepStage::Core, "2026-11-09T23:00:00Z", "2026-11-10T01:00:00Z"),
            seg(SleepStage::Core, "2026-11-10T04:01:00Z", "2026-11-10T06:00:00Z"),
        ];
        assert_eq!(nights(&split, nov(1), nov(30), &utc()).len(), 2);
    }

    #[test]
    fn time_awake_alone_is_not_a_night() {
        let segs = [seg(SleepStage::Awake, "2026-11-10T03:00:00Z", "2026-11-10T03:30:00Z")];
        assert!(nights(&segs, nov(1), nov(30), &utc()).is_empty());
    }

    #[test]
    fn plain_asleep_counts_as_sleep_with_no_stages() {
        let segs = [seg(SleepStage::Asleep, "2026-11-09T23:00:00Z", "2026-11-10T06:00:00Z")];
        let got = nights(&segs, nov(10), nov(10), &utc());
        assert_eq!(got[0].asleep_min, 420.0);
        assert_eq!((got[0].core_min, got[0].deep_min, got[0].rem_min), (0.0, 0.0, 0.0));
    }

    /// A night of `start` to `end` with every number but the times neutral.
    fn slept(on: Date, start: &str, end: &str) -> SleepNight {
        let (start, end) = (ts(start), ts(end));
        SleepNight {
            date: on,
            in_bed_start: None,
            sleep_start: Some(start),
            sleep_end: Some(end),
            asleep_min: (end.as_second() - start.as_second()) as f64 / 60.0,
            in_bed_min: 0.0,
            awake_min: 0.0,
            core_min: 0.0,
            deep_min: 0.0,
            rem_min: 0.0,
            efficiency: None,
            hr: None,
            hrv_ms: None,
            respiratory_rate: None,
            score: None,
            score_parts: BTreeMap::new(),
            segments: Vec::new(),
        }
    }

    fn point(start: &str, value: f64) -> SamplePoint {
        SamplePoint { start: ts(start), end: ts(start), value, source: "Watch".into() }
    }

    #[test]
    fn enrich_averages_what_was_measured_while_asleep() {
        let mut n = slept(nov(10), "2026-11-09T23:00:00Z", "2026-11-10T07:00:00Z");
        let heart = [
            point("2026-11-09T22:59:00Z", 90.0),
            point("2026-11-09T23:00:00Z", 60.0),
            point("2026-11-10T03:00:00Z", 50.0),
            point("2026-11-10T06:59:00Z", 55.0),
            point("2026-11-10T07:00:00Z", 100.0),
        ];
        let breathing = [point("2026-11-10T02:00:00Z", 14.0), point("2026-11-10T04:00:00Z", 16.0)];
        enrich(&mut n, &heart, &[], &breathing);
        assert_eq!(n.hr, Some(55.0));
        assert_eq!(n.hrv_ms, None);
        assert_eq!(n.respiratory_rate, Some(15.0));
        // With nothing inside the window a measure already filled stays.
        enrich(&mut n, &[point("2026-11-11T03:00:00Z", 1.0)], &[], &[]);
        assert_eq!(n.hr, Some(55.0));
    }

    #[test]
    fn enrich_leaves_a_night_with_no_sleep_alone() {
        let mut n = slept(nov(10), "2026-11-09T23:00:00Z", "2026-11-10T07:00:00Z");
        n.sleep_start = None;
        enrich(&mut n, &[point("2026-11-10T03:00:00Z", 50.0)], &[], &[]);
        assert_eq!(n.hr, None);
    }

    /// A night of `asleep` minutes, `restorative` of them deep or REM, and
    /// an efficiency.
    fn shaped(asleep: f64, restorative: f64, efficiency: Option<f64>) -> SleepNight {
        let mut n = slept(nov(10), "2026-11-09T23:00:00Z", "2026-11-10T07:00:00Z");
        n.asleep_min = asleep;
        n.deep_min = restorative / 2.0;
        n.rem_min = restorative / 2.0;
        n.efficiency = efficiency;
        n
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn duration_is_full_between_the_goal_and_an_hour_and_a_half_over() {
        let part = |minutes| score(&shaped(minutes, 0.0, Some(0.9)), 8.0, &[]).1["duration"];
        assert!(close(part(480.0), 100.0));
        assert!(close(part(360.0), 75.0));
        assert!(close(part(0.0), 0.0));
        assert!(close(part(570.0), 100.0));
        // Two hours past the 9.5 that is free: twenty points off.
        assert!(close(part(690.0), 80.0));
        assert!(close(part(1400.0), 50.0));
    }

    #[test]
    fn efficiency_is_full_at_nine_tenths_and_nothing_at_six() {
        let part = |e| score(&shaped(480.0, 0.0, Some(e)), 8.0, &[]).1["efficiency"];
        assert!(close(part(0.95), 100.0));
        assert!(close(part(0.9), 100.0));
        assert!(close(part(0.75), 50.0));
        assert!(close(part(0.6), 0.0));
        assert!(close(part(0.5), 0.0));
    }

    #[test]
    fn restorative_is_full_at_four_tenths_deep_and_rem() {
        let part = |minutes| score(&shaped(480.0, minutes, Some(0.9)), 8.0, &[]).1["restorative"];
        assert!(close(part(192.0), 100.0));
        assert!(close(part(96.0), 50.0));
        assert!(close(part(0.0), 0.0));
        assert!(close(part(400.0), 100.0));
        assert_eq!(score(&shaped(0.0, 0.0, None), 8.0, &[]).1["restorative"], 0.0);
    }

    #[test]
    fn regularity_is_full_with_fewer_than_three_nights_and_follows_the_spread() {
        let part = |recent: &[SleepNight]| score(&shaped(480.0, 0.0, Some(0.9)), 8.0, recent).1["regularity"];
        assert_eq!(part(&[]), 100.0);
        let two = [
            slept(nov(5), "2026-11-04T20:00:00Z", "2026-11-05T04:00:00Z"),
            slept(nov(6), "2026-11-05T23:00:00Z", "2026-11-06T09:00:00Z"),
        ];
        assert_eq!(part(&two), 100.0);

        // Middles of 02:00, 04:00 and 03:00 on their own dates: a spread of
        // sqrt(2400) minutes.
        let three = [
            slept(nov(5), "2026-11-04T21:00:00Z", "2026-11-05T07:00:00Z"),
            slept(nov(6), "2026-11-05T23:00:00Z", "2026-11-06T09:00:00Z"),
            slept(nov(7), "2026-11-06T22:00:00Z", "2026-11-07T08:00:00Z"),
        ];
        assert!(close(part(&three), (120.0 - 2400f64.sqrt()) / 90.0 * 100.0));

        // The same hours every night are perfectly regular.
        let steady = [
            slept(nov(5), "2026-11-04T23:00:00Z", "2026-11-05T07:00:00Z"),
            slept(nov(6), "2026-11-05T23:00:00Z", "2026-11-06T07:00:00Z"),
            slept(nov(7), "2026-11-06T23:00:00Z", "2026-11-07T07:00:00Z"),
        ];
        assert_eq!(part(&steady), 100.0);

        // Middles of 23:00 the night before, 06:00 and 01:00: hours apart is
        // no regularity at all.
        let erratic = [
            slept(nov(5), "2026-11-04T19:00:00Z", "2026-11-05T03:00:00Z"),
            slept(nov(6), "2026-11-06T02:00:00Z", "2026-11-06T10:00:00Z"),
            slept(nov(7), "2026-11-06T21:00:00Z", "2026-11-07T05:00:00Z"),
        ];
        assert_eq!(part(&erratic), 0.0);
    }

    #[test]
    fn the_score_is_the_weighted_mean_of_its_parts() {
        // A perfect night: eight hours, 0.9 efficient, 40 % deep and REM.
        let (total, parts) = score(&shaped(480.0, 192.0, Some(0.9)), 8.0, &[]);
        assert!(close(total, 100.0));
        let keys: Vec<&str> = parts.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["duration", "efficiency", "regularity", "restorative"]);

        // Six hours (75), 0.75 efficient (50), 20 % restorative (50), no
        // history (100): 37.5 + 10 + 7.5 + 15.
        let (total, parts) = score(&shaped(360.0, 72.0, Some(0.75)), 8.0, &[]);
        assert!(close(total, 70.0));
        assert!(close(parts["duration"], 75.0) && close(parts["efficiency"], 50.0));
        assert!(close(parts["restorative"], 50.0) && close(parts["regularity"], 100.0));
    }

    #[test]
    fn a_night_with_no_efficiency_is_scored_on_the_rest() {
        let (total, parts) = score(&shaped(360.0, 72.0, None), 8.0, &[]);
        assert!(!parts.contains_key("efficiency"));
        // (37.5 + 7.5 + 15) of the 0.8 that counts.
        assert!(close(total, 75.0));
    }

    #[test]
    fn the_score_stays_between_nothing_and_a_hundred() {
        let (zero, _) = score(&shaped(0.0, 0.0, Some(0.0)), 8.0, &[]);
        assert!(zero >= 0.0);
        let (long, _) = score(&shaped(3000.0, 3000.0, Some(1.0)), 8.0, &[]);
        assert!(long <= 100.0);
    }
}
