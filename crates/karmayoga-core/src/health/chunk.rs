//! Series days: how samples are filed, deduplicated and summed into
//! quarter hours.
//!
//! One [`SeriesDay`] holds every sample of one metric that *started* on one
//! UTC day. [`merge`] adds samples to it without ever holding two equal ones
//! (same start, duration and value, whatever the source), which is what
//! makes a re-import, or the same reading arriving from the export and from
//! the phone, harmless. [`summarize`] then sums the day into
//! [`SeriesSummary`] quarter hours with the source priority applied.

use super::metric::{Metric, MetricKind};
use super::{Bucket, HealthSettings, Sample, SamplePoint, SeriesDay, SeriesSummary, Tombstone};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use std::cmp::{Ordering, Reverse};
use std::collections::BTreeMap;

const DAY_SECS: i64 = 86_400;
const QUARTER_SECS: i64 = 900;
const QUARTERS: usize = 96;

/// A canonical sample on its way into a day, its source still a name.
#[derive(Debug, Clone, PartialEq)]
pub struct Incoming {
    /// Seconds since the epoch.
    pub start: i64,
    pub secs: u32,
    pub value: f64,
    pub source: String,
    pub offset_min: i16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MergeOutcome {
    pub added: u64,
    pub duplicate: u64,
}

/// The UTC day a sample starting at `start` (epoch seconds) is filed under.
pub fn utc_day(start: i64) -> Date {
    Timestamp::from_second(start).map(|t| t.to_zoned(TimeZone::UTC).date()).unwrap_or_default()
}

/// Group samples by the UTC day each starts on.
pub fn by_day(samples: Vec<Incoming>) -> BTreeMap<Date, Vec<Incoming>> {
    let mut out: BTreeMap<Date, Vec<Incoming>> = BTreeMap::new();
    for s in samples {
        out.entry(utc_day(s.start)).or_default().push(s);
    }
    out
}

/// Add `incoming` (all starting on `day.day`) to `day`, keeping the samples
/// sorted and never holding two that are the same reading.
///
/// Two samples are the same reading when they start at the same second with
/// the same value and either last the same or one of them is an instant.
/// The instant rule is for Health Auto Export, which sends most quantities
/// with only a `date` (their start) while Apple's export carries the same
/// sample's interval: without it a day imported both ways would count every
/// step twice. When an interval arrives for a reading held as an instant,
/// the held one takes the interval's length, so the better description wins
/// whichever came first.
///
/// "The same value" allows for the rounding a unit conversion leaves: the two
/// differ by at most a billionth of the first's size (or of 1, for a value
/// smaller than that). Which source brought a sample does not matter, so the
/// first to arrive keeps it, and a source whose every sample is a repeat
/// leaves no name in the day's table. A sample matching one of the day's
/// [`tombstones`](SeriesDay::tombstones) -- one a person deleted -- is
/// refused as a repeat.
pub fn merge(day: &mut SeriesDay, incoming: &[Incoming]) -> MergeOutcome {
    // In the order a day keeps, so `fresh` comes out sorted and can be searched. The
    // sort is stable: of two equal samples the batch's first is the one kept.
    let mut batch: Vec<&Incoming> = incoming.iter().collect();
    batch.sort_by(|a, b| {
        a.start.cmp(&b.start).then(a.secs.cmp(&b.secs)).then(a.value.total_cmp(&b.value))
    });

    let mut fresh: Vec<Sample> = Vec::new();
    let mut lengthened = false;
    for s in batch {
        let probe = Sample {
            start: s.start,
            secs: s.secs,
            value: s.value,
            source: 0,
            offset_min: s.offset_min,
        };
        if buried(&day.tombstones, &probe) {
            continue;
        }
        if let Some(at) = held(&day.samples, &probe) {
            if day.samples[at].secs == 0 && probe.secs > 0 {
                day.samples[at].secs = probe.secs;
                lengthened = true;
            }
            continue;
        }
        if let Some(at) = held(&fresh, &probe) {
            if fresh[at].secs == 0 && probe.secs > 0 {
                fresh[at].secs = probe.secs;
            }
            continue;
        }
        fresh.push(Sample { source: source_index(day, &s.source), ..probe });
    }

    let added = fresh.len() as u64;
    if !fresh.is_empty() || lengthened {
        day.samples.extend(fresh);
        // Two sorted runs back to back: the sort merges them in one pass.
        day.samples.sort_by(order);
    }
    MergeOutcome { added, duplicate: incoming.len() as u64 - added }
}

/// Remove samples starting in `from..to` (epoch seconds), only `source`'s
/// when given. Answers how many went.
///
/// Each removed sample that a source brought leaves a tombstone, so the
/// phone sending the same week again does not bring it back. One typed in
/// here (the source "Karma Yoga") leaves none: nothing will send it again.
/// The source's name stays in the day's table, since the samples that remain
/// index into it.
pub fn remove(day: &mut SeriesDay, from: i64, to: i64, source: Option<&str>) -> u64 {
    let only = match source {
        Some(name) => match day.sources.iter().position(|s| s == name) {
            Some(at) => Some(u16::try_from(at).unwrap_or(u16::MAX)),
            None => return 0,
        },
        None => None,
    };
    let before = day.samples.len();
    let mut gone = Vec::new();
    day.samples.retain(|s| {
        let in_range = (from..to).contains(&s.start);
        let removed = in_range && only.is_none_or(|at| s.source == at);
        if removed {
            gone.push(*s);
        }
        !removed
    });
    for s in gone {
        let typed_here = day
            .sources
            .get(usize::from(s.source))
            .is_some_and(|name| name.to_lowercase().contains("karma yoga"));
        let probe = Sample { source: 0, ..s };
        if !typed_here && !buried(&day.tombstones, &probe) {
            day.tombstones.push(Tombstone { start: s.start, secs: s.secs, value: s.value });
        }
    }
    (before - day.samples.len()) as u64
}

/// How trusted a source is when nobody has said: an Apple Watch first,
/// then an iPhone, then other devices and apps, then readings typed in
/// here. Lower is more trusted.
pub fn default_rank(source: &str) -> usize {
    let name = source.to_lowercase();
    if name.contains("watch") {
        0
    } else if name.contains("phone") {
        // "iPhone" and "Hari's phone" alike.
        1
    } else if name.contains("karma yoga") {
        3
    } else {
        2
    }
}

/// The rank function for `metric` under `settings`: the per-metric list,
/// then the global one, then [`default_rank`] (offset past both lists).
///
/// Names are matched without regard to case or surrounding space. A source
/// the person ordered ranks by its place in the list; the lists are offset
/// (0.., 100.., 1000..) so one listed source always outranks an unlisted one.
pub fn ranker(settings: &HealthSettings, metric: Metric) -> Box<dyn Fn(&str) -> usize + '_> {
    let for_metric: &[String] =
        settings.priority_by_metric.get(&metric).map(Vec::as_slice).unwrap_or_default();
    Box::new(move |source: &str| {
        if let Some(at) = listed(for_metric, source) {
            return at;
        }
        if let Some(at) = listed(&settings.priority, source) {
            return 100 + at;
        }
        1000 + default_rank(source)
    })
}

/// Sum a day into quarter hours. For a
/// [`Cumulative`](super::metric::MetricKind::Cumulative) metric only the
/// best-ranked source with anything in a quarter counts there; a sample
/// spanning quarters is spread across them in proportion to its overlap.
/// For any other kind every source counts and a sample belongs to the
/// quarter it starts in.
///
/// Two sources of equal rank are not both counted, which would double a
/// walk: the one whose name sorts first stands. A cumulative sample reaching
/// past the day's end gives the rest of its value to the last quarter, and
/// one starting before it the first, rather than losing it: the day holds
/// every sample that starts on it, and the next day does not hold this one.
pub fn summarize(day: &SeriesDay, metric: Metric, rank: &dyn Fn(&str) -> usize) -> SeriesSummary {
    let day_start = start_of_day(day);
    let quarters = match metric.info().kind {
        MetricKind::Cumulative => cumulative(day, day_start, rank),
        MetricKind::Discrete | MetricKind::Category => pooled(day, day_start),
    };
    SeriesSummary {
        metric: Some(metric),
        day: day.day,
        buckets: buckets(&quarters),
        offset_min: common_offset(&day.samples),
    }
}

/// The day's samples as points, in order, sources by name.
pub fn points(day: &SeriesDay) -> Vec<SamplePoint> {
    day.samples
        .iter()
        .filter_map(|s| {
            Some(SamplePoint {
                start: Timestamp::from_second(s.start).ok()?,
                end: Timestamp::from_second(end_of(s)).ok()?,
                value: s.value,
                source: day.sources.get(usize::from(s.source)).cloned().unwrap_or_default(),
            })
        })
        .collect()
}

/// One sample's end, in epoch seconds.
pub fn end_of(sample: &Sample) -> i64 {
    sample.start + i64::from(sample.secs)
}

// ---- merging ----------------------------------------------------------------

/// Start, then length, then value: the order a day keeps its samples in.
fn order(a: &Sample, b: &Sample) -> Ordering {
    a.start.cmp(&b.start).then(a.secs.cmp(&b.secs)).then(a.value.total_cmp(&b.value))
}

fn same_value(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(1.0)
}

/// Whether two samples are the same reading: see [`merge`].
fn same_reading(a_secs: u32, a_value: f64, b: &Sample) -> bool {
    (a_secs == b.secs || a_secs == 0 || b.secs == 0) && same_value(a_value, b.value)
}

/// Where `sorted` already has the reading `probe` is, if it does. Only the
/// samples that share the probe's start are looked at.
fn held(sorted: &[Sample], probe: &Sample) -> Option<usize> {
    let first = sorted.partition_point(|s| s.start < probe.start);
    sorted[first..]
        .iter()
        .take_while(|s| s.start == probe.start)
        .position(|s| same_reading(s.secs, s.value, probe))
        .map(|at| first + at)
}

/// Whether a person deleted the reading `probe` is.
fn buried(tombstones: &[Tombstone], probe: &Sample) -> bool {
    tombstones.iter().any(|t| t.start == probe.start && same_reading(t.secs, t.value, probe))
}

/// The index of `name` in the day's source table, adding it if it is new. A
/// day that somehow has more names than an index can say files the rest
/// under the last one.
fn source_index(day: &mut SeriesDay, name: &str) -> u16 {
    if let Some(at) = day.sources.iter().position(|s| s == name) {
        return u16::try_from(at).unwrap_or(u16::MAX);
    }
    if day.sources.len() <= usize::from(u16::MAX) {
        day.sources.push(name.to_string());
        return u16::try_from(day.sources.len() - 1).unwrap_or(u16::MAX);
    }
    u16::MAX
}

fn listed(list: &[String], source: &str) -> Option<usize> {
    list.iter().position(|name| name.trim().eq_ignore_ascii_case(source.trim()))
}

// ---- summarizing ------------------------------------------------------------

/// What one quarter hour has gathered so far.
#[derive(Debug, Clone, Copy, Default)]
struct Quarter {
    count: u32,
    sum: f64,
    min: f64,
    max: f64,
    last: f64,
    covered: i64,
}

impl Quarter {
    fn add(&mut self, value: f64, covered: i64) {
        if self.count == 0 {
            self.min = value;
            self.max = value;
        } else {
            self.min = self.min.min(value);
            self.max = self.max.max(value);
        }
        self.count += 1;
        self.sum += value;
        self.last = value;
        self.covered += covered;
    }
}

/// Epoch seconds at which the day begins. The day's own date, or failing
/// that the day its first sample is on.
fn start_of_day(day: &SeriesDay) -> i64 {
    day.day
        .and_then(|d| d.to_zoned(TimeZone::UTC).ok())
        .map(|z| z.timestamp().as_second())
        .or_else(|| day.samples.first().map(|s| s.start.div_euclid(DAY_SECS) * DAY_SECS))
        .unwrap_or(0)
}

/// The quarter hour (0..96) of the day containing `at`, the nearest one for a
/// time outside the day.
fn quarter_of(day_start: i64, at: i64) -> usize {
    (at - day_start).div_euclid(QUARTER_SECS).clamp(0, QUARTERS as i64 - 1) as usize
}

/// Discrete and category metrics: every sample counts, in the quarter it
/// starts in.
fn pooled(day: &SeriesDay, day_start: i64) -> [Quarter; QUARTERS] {
    let mut quarters = [Quarter::default(); QUARTERS];
    for s in &day.samples {
        let q = quarter_of(day_start, s.start);
        let quarter_end = day_start + (q as i64 + 1) * QUARTER_SECS;
        let covered = if s.secs == 0 { 0 } else { (end_of(s).min(quarter_end) - s.start).max(0) };
        quarters[q].add(s.value, covered);
    }
    quarters
}

/// Cumulative metrics: a sample's value is spread over the quarters it
/// overlaps, and each quarter keeps only its best-ranked source.
fn cumulative(
    day: &SeriesDay,
    day_start: i64,
    rank: &dyn Fn(&str) -> usize,
) -> [Quarter; QUARTERS] {
    // A source by how it ranks, then its name, so equal ranks still have
    // one winner and it is the same one on every day.
    let keys: Vec<(usize, &str)> =
        day.sources.iter().map(|name| (rank(name.as_str()), name.as_str())).collect();
    let unknown = (rank(""), "");
    let key_of = |source: u16| keys.get(usize::from(source)).copied().unwrap_or(unknown);
    // The first and last quarter a sample overlaps. An instant is in one.
    let span = |s: &Sample| -> (usize, usize) {
        let first = quarter_of(day_start, s.start);
        let last = if s.secs == 0 { first } else { quarter_of(day_start, end_of(s) - 1) };
        (first, last)
    };

    let mut best: [Option<(usize, &str)>; QUARTERS] = [None; QUARTERS];
    for s in &day.samples {
        let key = key_of(s.source);
        let (first, last) = span(s);
        for slot in &mut best[first..=last] {
            if slot.is_none_or(|held| key < held) {
                *slot = Some(key);
            }
        }
    }

    let mut quarters = [Quarter::default(); QUARTERS];
    for s in &day.samples {
        let key = key_of(s.source);
        let (first, last) = span(s);
        for q in first..=last {
            if best[q] != Some(key) {
                continue;
            }
            // Seconds of the sample inside quarter `q`. The first quarter
            // takes whatever came before it and the last whatever comes
            // after, so the parts always add up to the whole.
            let lo = if q == first { s.start } else { day_start + q as i64 * QUARTER_SECS };
            let hi = if q == last { end_of(s) } else { day_start + (q as i64 + 1) * QUARTER_SECS };
            let (share, covered) = if s.secs == 0 {
                (1.0, 0)
            } else {
                ((hi - lo) as f64 / f64::from(s.secs), hi - lo)
            };
            quarters[q].add(s.value * share, covered);
        }
    }
    quarters
}

/// The quarters with something in them, as the buckets a summary keeps.
fn buckets(quarters: &[Quarter; QUARTERS]) -> Vec<Bucket> {
    quarters
        .iter()
        .enumerate()
        .filter(|(_, q)| q.count > 0)
        .map(|(at, q)| Bucket {
            quarter: at as u8,
            count: q.count,
            sum: q.sum,
            min: q.min,
            max: q.max,
            last: q.last,
            // Two sources measuring the same minutes cannot cover more than
            // the quarter.
            covered: u32::try_from(q.covered.clamp(0, QUARTER_SECS)).unwrap_or(0),
        })
        .collect()
}

/// The UTC offset most of the samples carry; the lowest when two tie, and 0
/// for a day with none.
fn common_offset(samples: &[Sample]) -> i16 {
    let mut counts: BTreeMap<i16, usize> = BTreeMap::new();
    for s in samples {
        *counts.entry(s.offset_min).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|&(offset, n)| (n, Reverse(offset)))
        .map_or(0, |(offset, _)| offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    /// 1970-01-02 00:00:00 UTC. Every test day is that one, so an instant is
    /// a small number of seconds into it and a quarter is a multiple of 900.
    const DAY: i64 = 86_400;

    fn empty(metric: Metric) -> SeriesDay {
        SeriesDay { metric: Some(metric), day: Some(date(1970, 1, 2)), ..Default::default() }
    }

    /// A sample `offset` seconds into the test day.
    fn at(offset: i64, secs: u32, value: f64, source: &str) -> Incoming {
        Incoming { start: DAY + offset, secs, value, source: source.to_string(), offset_min: 0 }
    }

    fn filed(metric: Metric, incoming: &[Incoming]) -> SeriesDay {
        let mut day = empty(metric);
        merge(&mut day, incoming);
        day
    }

    fn bucket_of(summary: &SeriesSummary, quarter: u8) -> Bucket {
        *summary.buckets.iter().find(|b| b.quarter == quarter).expect("a bucket in that quarter")
    }

    fn quarters_with_something(summary: &SeriesSummary) -> Vec<u8> {
        summary.buckets.iter().map(|b| b.quarter).collect()
    }

    const WATCH: &str = "Hari's Apple Watch";
    const PHONE: &str = "Hari's iPhone";

    // ---- days ----------------------------------------------------------------

    #[test]
    fn utc_day_changes_exactly_at_midnight() {
        assert_eq!(utc_day(0), date(1970, 1, 1));
        assert_eq!(utc_day(86_399), date(1970, 1, 1));
        assert_eq!(utc_day(86_400), date(1970, 1, 2));
        assert_eq!(utc_day(-1), date(1969, 12, 31));
        // 2023-11-14 00:00:00 UTC is 1_699_920_000.
        assert_eq!(utc_day(1_699_919_999), date(2023, 11, 13));
        assert_eq!(utc_day(1_699_920_000), date(2023, 11, 14));
        assert_eq!(utc_day(1_700_006_399), date(2023, 11, 14));
        assert_eq!(utc_day(1_700_006_400), date(2023, 11, 15));
    }

    #[test]
    fn by_day_groups_by_the_utc_day_each_sample_starts_on() {
        let groups = by_day(vec![
            at(-1, 0, 1.0, "W"),
            at(0, 0, 2.0, "W"),
            at(86_399, 0, 3.0, "W"),
            at(86_400, 0, 4.0, "W"),
        ]);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[&date(1970, 1, 1)].len(), 1);
        assert_eq!(groups[&date(1970, 1, 2)].len(), 2);
        assert_eq!(groups[&date(1970, 1, 3)].len(), 1);
    }

    // ---- merge ---------------------------------------------------------------

    #[test]
    fn merge_keeps_samples_sorted_by_start_then_length_then_value() {
        let mut day = empty(Metric::HeartRate);
        let out = merge(
            &mut day,
            &[
                at(30, 0, 70.0, WATCH),
                at(10, 60, 80.0, WATCH),
                at(10, 0, 90.0, WATCH),
                at(10, 0, 60.0, WATCH),
                at(20, 0, 65.0, WATCH),
            ],
        );
        assert_eq!(out, MergeOutcome { added: 5, duplicate: 0 });
        let held: Vec<(i64, u32, f64)> =
            day.samples.iter().map(|s| (s.start - DAY, s.secs, s.value)).collect();
        assert_eq!(
            held,
            vec![(10, 0, 60.0), (10, 0, 90.0), (10, 60, 80.0), (20, 0, 65.0), (30, 0, 70.0)]
        );
    }

    #[test]
    fn a_later_merge_slots_in_among_what_is_already_there() {
        let mut day = empty(Metric::HeartRate);
        merge(&mut day, &[at(10, 0, 60.0, WATCH), at(30, 0, 70.0, WATCH)]);
        let out = merge(
            &mut day,
            &[at(40, 0, 75.0, WATCH), at(20, 0, 65.0, WATCH), at(5, 0, 55.0, WATCH)],
        );
        assert_eq!(out, MergeOutcome { added: 3, duplicate: 0 });
        let starts: Vec<i64> = day.samples.iter().map(|s| s.start - DAY).collect();
        assert_eq!(starts, vec![5, 10, 20, 30, 40]);
    }

    #[test]
    fn the_same_reading_from_two_sources_is_stored_once() {
        let mut day = empty(Metric::Steps);
        let first = merge(&mut day, &[at(0, 60, 42.0, WATCH)]);
        // The phone saw the same minute; the second is that value to within
        // a conversion's rounding.
        let again = merge(&mut day, &[at(0, 60, 42.0, PHONE), at(0, 60, 42.0 + 1e-12, PHONE)]);
        assert_eq!(first, MergeOutcome { added: 1, duplicate: 0 });
        assert_eq!(again, MergeOutcome { added: 0, duplicate: 2 });
        assert_eq!(day.samples.len(), 1);
        // The first to arrive keeps the sample, and a source that brought
        // nothing new leaves no name behind.
        assert_eq!(day.sources, vec![WATCH.to_string()]);
        assert_eq!(day.samples[0].source, 0);
    }

    #[test]
    fn a_different_value_or_length_at_the_same_start_is_a_different_sample() {
        let mut day = empty(Metric::Steps);
        merge(&mut day, &[at(0, 60, 42.0, WATCH)]);
        let out = merge(
            &mut day,
            &[at(0, 60, 43.0, WATCH), at(0, 120, 42.0, WATCH), at(1, 60, 42.0, WATCH)],
        );
        assert_eq!(out, MergeOutcome { added: 3, duplicate: 0 });
        assert_eq!(day.samples.len(), 4);
    }

    #[test]
    fn a_repeat_inside_one_batch_counts_as_a_duplicate() {
        let mut day = empty(Metric::Weight);
        let out = merge(&mut day, &[at(5, 0, 80.5, "Scale"), at(5, 0, 80.5, "Health")]);
        assert_eq!(out, MergeOutcome { added: 1, duplicate: 1 });
        assert_eq!(day.sources, vec!["Scale".to_string()]);
    }

    #[test]
    fn merging_a_batch_twice_leaves_the_day_as_it_was() {
        let batch = [at(0, 60, 10.0, WATCH), at(60, 60, 20.0, PHONE), at(120, 0, 30.0, WATCH)];
        let mut day = empty(Metric::Steps);
        merge(&mut day, &batch);
        let once = day.clone();
        let out = merge(&mut day, &batch);
        assert_eq!(out, MergeOutcome { added: 0, duplicate: 3 });
        assert_eq!(day, once);
    }

    #[test]
    fn a_known_source_name_reuses_its_index_and_a_new_one_is_appended() {
        let day = filed(
            Metric::Steps,
            &[
                at(0, 60, 1.0, WATCH),
                at(60, 60, 2.0, PHONE),
                at(120, 60, 3.0, WATCH),
                at(180, 60, 4.0, "Other"),
            ],
        );
        assert_eq!(day.sources, vec![WATCH.to_string(), PHONE.to_string(), "Other".to_string()]);
        let indexes: Vec<u16> = day.samples.iter().map(|s| s.source).collect();
        assert_eq!(indexes, vec![0, 1, 0, 2]);
    }

    #[test]
    fn the_source_table_stops_growing_at_the_index_limit() {
        let mut day = empty(Metric::Steps);
        day.sources = (0..=usize::from(u16::MAX)).map(|n| format!("source {n}")).collect();
        let out = merge(&mut day, &[at(0, 60, 1.0, "one more")]);
        assert_eq!(out.added, 1);
        assert_eq!(day.sources.len(), usize::from(u16::MAX) + 1);
        assert_eq!(day.samples[0].source, u16::MAX);
    }

    // ---- remove --------------------------------------------------------------

    fn removal_day() -> SeriesDay {
        filed(
            Metric::HeartRate,
            &[
                at(0, 0, 60.0, WATCH),
                at(50, 0, 61.0, PHONE),
                at(100, 0, 62.0, WATCH),
                at(200, 0, 63.0, WATCH),
            ],
        )
    }

    #[test]
    fn remove_takes_a_range_and_the_end_is_exclusive() {
        let mut day = removal_day();
        assert_eq!(remove(&mut day, DAY + 50, DAY + 200, None), 2);
        let left: Vec<i64> = day.samples.iter().map(|s| s.start - DAY).collect();
        assert_eq!(left, vec![0, 200]);
    }

    #[test]
    fn remove_by_source_leaves_the_other_sources_alone() {
        let mut day = removal_day();
        assert_eq!(remove(&mut day, DAY, DAY + 1000, Some(WATCH)), 3);
        assert_eq!(day.samples.len(), 1);
        assert_eq!(points(&day)[0].source, PHONE);
    }

    #[test]
    fn remove_of_an_unknown_source_or_an_empty_range_removes_nothing() {
        let mut day = removal_day();
        assert_eq!(remove(&mut day, DAY, DAY + 1000, Some("Nobody")), 0);
        assert_eq!(remove(&mut day, DAY + 1, DAY + 49, None), 0);
        assert_eq!(remove(&mut day, DAY + 100, DAY + 100, None), 0);
        assert_eq!(day.samples.len(), 4);
    }

    // ---- rank ----------------------------------------------------------------

    #[test]
    fn default_rank_puts_the_watch_before_the_phone_before_the_rest() {
        assert_eq!(default_rank("Hari's Apple Watch"), 0);
        assert_eq!(default_rank("APPLE WATCH"), 0);
        assert_eq!(default_rank("iPhone"), 1);
        assert_eq!(default_rank("Hari's phone"), 1);
        assert_eq!(default_rank("Dexcom G7"), 2);
        assert_eq!(default_rank(""), 2);
        assert_eq!(default_rank("Karma Yoga"), 3);
        assert_eq!(default_rank("karma yoga"), 3);
    }

    #[test]
    fn ranker_prefers_the_metric_list_then_the_global_list_then_the_default() {
        let mut settings = HealthSettings {
            priority: vec!["Dexcom G7".into(), "Hari's iPhone".into()],
            ..Default::default()
        };
        settings.priority_by_metric.insert(Metric::Steps, vec!["hari's iphone".into()]);

        let steps = ranker(&settings, Metric::Steps);
        assert_eq!(steps(PHONE), 0);
        assert_eq!(steps("Dexcom G7"), 100);
        assert_eq!(steps(WATCH), 1000);
        assert_eq!(steps("Karma Yoga"), 1003);

        let heart = ranker(&settings, Metric::HeartRate);
        assert_eq!(heart("Dexcom G7"), 100);
        assert_eq!(heart(PHONE), 101);
        assert_eq!(heart(WATCH), 1000);
        assert_eq!(heart("Some app"), 1002);
    }

    #[test]
    fn ranker_with_no_settings_is_the_default_order() {
        let settings = HealthSettings::default();
        let rank = ranker(&settings, Metric::Steps);
        assert!(rank(WATCH) < rank(PHONE));
        assert!(rank(PHONE) < rank("Pedometer"));
        assert!(rank("Pedometer") < rank("Karma Yoga"));
    }

    // ---- summarize: cumulative -----------------------------------------------

    #[test]
    fn a_cumulative_quarter_counts_only_the_best_ranked_source_in_it() {
        let day = filed(
            Metric::Steps,
            &[
                at(0, 60, 100.0, WATCH),
                // The phone counted the same walk in the same quarter.
                at(60, 60, 120.0, PHONE),
                // Nobody but the phone reported in the next quarter.
                at(900, 60, 50.0, PHONE),
            ],
        );
        let summary = summarize(&day, Metric::Steps, &default_rank);
        assert_eq!(quarters_with_something(&summary), vec![0, 1]);
        let first = bucket_of(&summary, 0);
        assert_eq!((first.count, first.sum), (1, 100.0));
        let second = bucket_of(&summary, 1);
        assert_eq!((second.count, second.sum), (1, 50.0));
    }

    #[test]
    fn the_persons_priority_list_decides_which_source_a_quarter_counts() {
        let day = filed(Metric::Steps, &[at(0, 60, 100.0, WATCH), at(60, 60, 120.0, PHONE)]);
        let settings = HealthSettings { priority: vec![PHONE.into()], ..Default::default() };
        let rank = ranker(&settings, Metric::Steps);
        let summary = summarize(&day, Metric::Steps, &*rank);
        assert_eq!(bucket_of(&summary, 0).sum, 120.0);
    }

    #[test]
    fn two_sources_of_equal_rank_are_not_both_counted() {
        // Neither is a watch, a phone or this app, so both rank 2. The one
        // whose name sorts first stands, whichever filed first.
        for names in [["AppA", "AppB"], ["AppB", "AppA"]] {
            let day =
                filed(Metric::Steps, &[at(0, 60, 10.0, names[0]), at(60, 60, 99.0, names[1])]);
            let summary = summarize(&day, Metric::Steps, &default_rank);
            let bucket = bucket_of(&summary, 0);
            assert_eq!(bucket.count, 1);
            let winner = if names[0] == "AppA" { 10.0 } else { 99.0 };
            assert_eq!(bucket.sum, winner, "{names:?}");
        }
    }

    #[test]
    fn a_cumulative_sample_is_spread_across_the_quarters_it_overlaps() {
        // Ten minutes from 10:00 to 20:00 after midnight straddles the
        // 15-minute mark: half of it on each side.
        let day = filed(Metric::Steps, &[at(600, 600, 100.0, WATCH)]);
        let summary = summarize(&day, Metric::Steps, &default_rank);
        assert_eq!(quarters_with_something(&summary), vec![0, 1]);
        let (first, second) = (bucket_of(&summary, 0), bucket_of(&summary, 1));
        assert_eq!((first.sum, first.covered), (50.0, 300));
        assert_eq!((second.sum, second.covered), (50.0, 300));
        assert_eq!(first.sum + second.sum, 100.0);
    }

    #[test]
    fn a_long_cumulative_sample_is_spread_in_proportion_to_the_overlap() {
        // 450 s to 2250 s: a quarter of it in quarter 0, half in 1, a quarter in 2.
        let day = filed(Metric::ActiveEnergy, &[at(450, 1800, 400.0, WATCH)]);
        let summary = summarize(&day, Metric::ActiveEnergy, &default_rank);
        assert_eq!(quarters_with_something(&summary), vec![0, 1, 2]);
        assert_eq!(bucket_of(&summary, 0).sum, 100.0);
        assert_eq!(bucket_of(&summary, 1).sum, 200.0);
        assert_eq!(bucket_of(&summary, 2).sum, 100.0);
        let covered: Vec<u32> = summary.buckets.iter().map(|b| b.covered).collect();
        assert_eq!(covered, vec![450, 900, 450]);
    }

    #[test]
    fn a_sample_that_ends_on_a_quarter_boundary_stays_in_its_quarter() {
        let day = filed(Metric::Steps, &[at(0, 900, 90.0, WATCH)]);
        let summary = summarize(&day, Metric::Steps, &default_rank);
        assert_eq!(quarters_with_something(&summary), vec![0]);
        assert_eq!(bucket_of(&summary, 0).sum, 90.0);
    }

    #[test]
    fn spreading_only_competes_with_sources_in_the_quarters_it_touches() {
        // The watch's sample spans quarters 0 and 1; the phone reported in
        // quarter 1 only. The watch wins quarter 1 as well, and the phone's
        // steps there do not count.
        let day = filed(Metric::Steps, &[at(600, 600, 100.0, WATCH), at(1000, 60, 7.0, PHONE)]);
        let summary = summarize(&day, Metric::Steps, &default_rank);
        assert_eq!(bucket_of(&summary, 0).sum, 50.0);
        let second = bucket_of(&summary, 1);
        assert_eq!((second.count, second.sum), (1, 50.0));
    }

    #[test]
    fn an_instant_in_a_cumulative_metric_goes_whole_to_its_quarter() {
        let day = filed(Metric::InsulinBolus, &[at(1000, 0, 4.0, "Karma Yoga")]);
        let summary = summarize(&day, Metric::InsulinBolus, &default_rank);
        assert_eq!(quarters_with_something(&summary), vec![1]);
        let bucket = bucket_of(&summary, 1);
        assert_eq!((bucket.count, bucket.sum, bucket.covered), (1, 4.0, 0));
    }

    #[test]
    fn a_cumulative_sample_running_past_midnight_keeps_all_of_its_value() {
        // 23:55 for ten minutes. The next day does not hold this sample, so
        // the last quarter takes the part that is past the end.
        let day = filed(Metric::Steps, &[at(DAY - 300, 600, 80.0, WATCH)]);
        let summary = summarize(&day, Metric::Steps, &default_rank);
        assert_eq!(quarters_with_something(&summary), vec![95]);
        assert_eq!(bucket_of(&summary, 95).sum, 80.0);
    }

    #[test]
    fn a_covered_time_never_exceeds_the_quarter() {
        let day = filed(Metric::Steps, &[at(0, 900, 10.0, WATCH), at(1, 900, 10.0, WATCH)]);
        let summary = summarize(&day, Metric::Steps, &default_rank);
        assert_eq!(bucket_of(&summary, 0).covered, 900);
    }

    // ---- summarize: discrete and category ------------------------------------

    #[test]
    fn a_discrete_quarter_pools_every_source_and_counts_a_sample_where_it_starts() {
        let day = filed(
            Metric::HeartRate,
            &[
                at(10, 0, 60.0, WATCH),
                at(20, 0, 70.0, PHONE),
                at(890, 0, 80.0, WATCH),
                at(900, 0, 90.0, WATCH),
            ],
        );
        let summary = summarize(&day, Metric::HeartRate, &default_rank);
        assert_eq!(quarters_with_something(&summary), vec![0, 1]);
        let first = bucket_of(&summary, 0);
        assert_eq!((first.count, first.sum), (3, 210.0));
        assert_eq!((first.min, first.max, first.last, first.covered), (60.0, 80.0, 80.0, 0));
        let second = bucket_of(&summary, 1);
        assert_eq!(
            (second.count, second.sum, second.min, second.max, second.last),
            (1, 90.0, 90.0, 90.0, 90.0)
        );
    }

    #[test]
    fn a_discrete_sample_with_a_duration_covers_only_the_part_in_its_quarter() {
        let day = filed(
            Metric::BloodGlucose,
            &[at(800, 300, 110.0, "Dexcom G7"), at(0, 0, 100.0, "Dexcom G7")],
        );
        let summary = summarize(&day, Metric::BloodGlucose, &default_rank);
        // 800..1100 starts in quarter 0, which holds the first 100 s of it,
        // and is not counted again in quarter 1.
        assert_eq!(quarters_with_something(&summary), vec![0]);
        let bucket = bucket_of(&summary, 0);
        assert_eq!((bucket.count, bucket.covered), (2, 100));
        assert_eq!((bucket.min, bucket.max, bucket.last), (100.0, 110.0, 110.0));
    }

    #[test]
    fn a_category_sample_is_counted_in_the_quarter_it_starts_in() {
        let day =
            filed(Metric::SleepStage, &[at(800, 3600, 3.0, WATCH), at(4400, 1800, 4.0, WATCH)]);
        let summary = summarize(&day, Metric::SleepStage, &default_rank);
        assert_eq!(quarters_with_something(&summary), vec![0, 4]);
        let first = bucket_of(&summary, 0);
        assert_eq!((first.count, first.sum, first.covered), (1, 3.0, 100));
        // 4400 s is 4 * 900 + 800: quarter 4, 100 s of it before the next.
        assert_eq!(bucket_of(&summary, 4).covered, 100);
    }

    #[test]
    fn the_last_quarter_of_the_day_is_ninety_five() {
        let day =
            filed(Metric::HeartRate, &[at(DAY - 1, 0, 70.0, WATCH), at(DAY - 900, 0, 71.0, WATCH)]);
        let summary = summarize(&day, Metric::HeartRate, &default_rank);
        assert_eq!(quarters_with_something(&summary), vec![95]);
        assert_eq!(bucket_of(&summary, 95).count, 2);
    }

    // ---- summarize: the rest --------------------------------------------------

    #[test]
    fn the_summary_carries_the_metric_the_day_and_the_commonest_offset() {
        let mut batch = vec![at(0, 0, 60.0, WATCH), at(10, 0, 61.0, WATCH), at(20, 0, 62.0, WATCH)];
        batch[0].offset_min = -480;
        batch[1].offset_min = -480;
        batch[2].offset_min = 60;
        let day = filed(Metric::HeartRate, &batch);
        let summary = summarize(&day, Metric::HeartRate, &default_rank);
        assert_eq!(summary.metric, Some(Metric::HeartRate));
        assert_eq!(summary.day, Some(date(1970, 1, 2)));
        assert_eq!(summary.offset_min, -480);
    }

    #[test]
    fn an_empty_day_summarizes_to_nothing_and_offset_zero() {
        let summary = summarize(&empty(Metric::Steps), Metric::Steps, &default_rank);
        assert!(summary.buckets.is_empty());
        assert_eq!(summary.offset_min, 0);
        let summary = summarize(&empty(Metric::HeartRate), Metric::HeartRate, &default_rank);
        assert!(summary.buckets.is_empty());
    }

    #[test]
    fn a_day_without_a_date_is_read_from_its_first_sample() {
        let mut day = filed(Metric::HeartRate, &[at(1000, 0, 70.0, WATCH)]);
        day.day = None;
        let summary = summarize(&day, Metric::HeartRate, &default_rank);
        assert_eq!(quarters_with_something(&summary), vec![1]);
        assert_eq!(summary.day, None);
    }

    // ---- points ----------------------------------------------------------------

    #[test]
    fn points_are_the_samples_in_order_with_their_sources_by_name() {
        let day = filed(Metric::HeartRate, &[at(10, 0, 70.0, WATCH), at(5, 30, 65.0, PHONE)]);
        let got = points(&day);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].source, PHONE);
        assert_eq!(got[0].start, Timestamp::from_second(DAY + 5).unwrap());
        assert_eq!(got[0].end, Timestamp::from_second(DAY + 35).unwrap());
        assert_eq!(got[0].value, 65.0);
        assert_eq!(got[1].source, WATCH);
        assert_eq!(got[1].start, got[1].end);
    }

    #[test]
    fn a_sample_whose_source_is_missing_from_the_table_has_an_empty_name() {
        let mut day = filed(Metric::HeartRate, &[at(10, 0, 70.0, WATCH)]);
        day.samples[0].source = 9;
        assert_eq!(points(&day)[0].source, "");
    }

    #[test]
    fn end_of_adds_the_duration_to_the_start() {
        let day = filed(Metric::Steps, &[at(10, 60, 5.0, WATCH)]);
        assert_eq!(end_of(&day.samples[0]), DAY + 70);
    }

    // ---- the same reading told two ways, and deleted readings -----------------

    #[test]
    fn an_instant_and_an_interval_with_one_start_and_value_are_one_reading() {
        // Health Auto Export sends a step count with only its start; the
        // Apple export sends the same sample with its five minutes.
        let mut day = filed(Metric::Steps, &[at(600, 0, 412.0, WATCH)]);
        let out = merge(&mut day, &[at(600, 300, 412.0, WATCH)]);
        assert_eq!(out, MergeOutcome { added: 0, duplicate: 1 });
        assert_eq!(day.samples.len(), 1);
        // The interval is the better description, so the held one takes it.
        assert_eq!(day.samples[0].secs, 300);

        // And the other way round leaves the interval as it was.
        let mut day = filed(Metric::Steps, &[at(600, 300, 412.0, WATCH)]);
        let out = merge(&mut day, &[at(600, 0, 412.0, PHONE)]);
        assert_eq!(out, MergeOutcome { added: 0, duplicate: 1 });
        assert_eq!(day.samples.len(), 1);
        assert_eq!(day.samples[0].secs, 300);
    }

    #[test]
    fn two_intervals_of_different_lengths_are_still_two_readings() {
        let day = filed(Metric::Steps, &[at(600, 300, 412.0, WATCH), at(600, 60, 412.0, WATCH)]);
        assert_eq!(day.samples.len(), 2);
    }

    #[test]
    fn a_removed_reading_from_a_source_is_not_brought_back_by_a_repeat() {
        let mut day = filed(Metric::BloodGlucose, &[at(60, 0, 250.0, "Dexcom G7")]);
        assert_eq!(remove(&mut day, DAY, DAY + 120, None), 1);
        assert_eq!(day.tombstones.len(), 1);
        // The phone sends the week again, with the interval this time.
        let out = merge(&mut day, &[at(60, 0, 250.0, "Dexcom G7"), at(60, 300, 250.0, WATCH)]);
        assert_eq!(out.added, 0);
        assert!(day.samples.is_empty());
        // Something else at that minute is not a deleted reading.
        let out = merge(&mut day, &[at(60, 0, 180.0, "Dexcom G7")]);
        assert_eq!(out.added, 1);
    }

    #[test]
    fn a_reading_typed_in_here_leaves_no_tombstone() {
        let mut day = filed(Metric::Weight, &[at(60, 0, 80.0, "Karma Yoga")]);
        assert_eq!(remove(&mut day, DAY, DAY + 120, None), 1);
        assert!(day.tombstones.is_empty());
        assert_eq!(merge(&mut day, &[at(60, 0, 80.0, "Karma Yoga")]).added, 1);
    }
}
