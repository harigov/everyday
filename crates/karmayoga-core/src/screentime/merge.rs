//! Joining heartbeats into spans.
//!
//! Pure: handed the stream's last span and a batch, it answers the spans to
//! write. The vault holds the write lock around it, so two batches from one
//! stream cannot interleave.
//!
//! # The rule
//!
//! Beats are taken in start order. A beat of the same item as the open span
//! (same kind, app and host -- a new title does not start a new span) that
//! begins no later than [`PULSE_SECS`](super::PULSE_SECS) after the open
//! span's end extends it; anything else closes it and opens a new one. A
//! span is never grown past [`MAX_SPAN_SECS`](super::MAX_SPAN_SECS). A beat
//! that lies wholly inside the open span of the same item changes nothing.
//!
//! # The stream's last span is a second chance
//!
//! The open span starts out as the stream's last stored span. A collector
//! that was offline sends its backlog, and a backlog is older than that
//! span: its first beat cannot extend it and so opens a span of its own.
//! Taken literally, the rule would then leave the stored span closed for
//! good, and the backlog's tail -- which may run right up to it -- would
//! start yet another span beside it. So a beat that cannot join the open
//! span is offered to the stored span too, until that span has been joined
//! or passed. Spans are still never joined across a gap or past the cap;
//! this only keeps one batch's backlog from splitting a run that the next
//! heartbeat continues.
//!
//! Overlaps are left alone: a beat that overlaps a span of another item
//! simply becomes a span of its own. Collectors report one focused thing at
//! a time, so an overlap is a clock or a backlog being untidy, and
//! [`report::resolve`](super::report::resolve) is what decides who had the
//! overlapping seconds -- here nothing is thrown away.

use super::{
    Heartbeat, HeartbeatBatch, MAX_APP, MAX_BACKLOG_DAYS, MAX_BEATS, MAX_COLLECTOR, MAX_DEVICE,
    MAX_FUTURE_SECS, MAX_HOST, MAX_NAME, MAX_SPAN_SECS, MAX_TITLE, PULSE_SECS, ScreenSpan,
    SpanKind, domain,
};
use crate::error::{Error, Result};
use crate::id::ScreenSpanId;
use jiff::Timestamp;

const MICROS: i64 = 1_000_000;

/// A batch after checking.
#[derive(Debug, Clone, PartialEq)]
pub struct Cleaned {
    pub device: String,
    pub collector: String,
    /// In start order; trimmed and capped (see the `MAX_*` constants);
    /// titles emptied unless kept; hosts normalised; `end` filled in.
    pub beats: Vec<Heartbeat>,
    pub rejected: u32,
}

/// Check a batch. Refuses the whole batch (`Error::Invalid`) for a blank or
/// over-long device, a collector that is not lower-case letters, digits and
/// `-`, or more than [`MAX_BEATS`](super::MAX_BEATS) beats; drops, counting
/// them as rejected, beats older than
/// [`MAX_BACKLOG_DAYS`](super::MAX_BACKLOG_DAYS), more than
/// [`MAX_FUTURE_SECS`](super::MAX_FUTURE_SECS) ahead of `now`, ending before
/// they start, lasting longer than a span may, with no app, or a site with
/// no host.
///
/// The device and collector are refused rather than cut because they name
/// the stream: two different over-long names cut to the same hundred
/// characters would join two machines' spans. A beat's own text is cut,
/// never refused (see the `MAX_*` constants). An app beat's host is
/// dropped -- only a site has one -- and a beat's age is judged by its
/// start, its future by its end.
pub fn clean(batch: &HeartbeatBatch, now: Timestamp, keep_titles: bool) -> Result<Cleaned> {
    let device = batch.device.trim();
    if device.is_empty() {
        return Err(Error::Invalid("a heartbeat batch must name its device".into()));
    }
    if device.chars().count() > MAX_DEVICE {
        return Err(Error::Invalid(format!(
            "a device's name must be at most {MAX_DEVICE} characters"
        )));
    }
    let collector = batch.collector.trim();
    if collector.is_empty()
        || collector.len() > MAX_COLLECTOR
        || !collector.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(Error::Invalid(format!(
            "a collector must be named by at most {MAX_COLLECTOR} lower-case letters, digits \
             and dashes, like \"gnome\" or \"firefox\""
        )));
    }
    if batch.beats.len() > MAX_BEATS {
        return Err(Error::Invalid(format!(
            "a heartbeat batch may carry at most {MAX_BEATS} beats; send them in smaller batches"
        )));
    }

    let now_us = now.as_microsecond();
    let oldest = now_us.saturating_sub(MAX_BACKLOG_DAYS * 86_400 * MICROS);
    let newest = now_us.saturating_add(MAX_FUTURE_SECS * MICROS);

    let mut beats = Vec::with_capacity(batch.beats.len());
    let mut rejected = 0u32;
    for beat in &batch.beats {
        let app = cut(&beat.app, MAX_APP);
        let host = match beat.kind {
            SpanKind::App => String::new(),
            SpanKind::Site => cut(&domain::normalize_host(&beat.host), MAX_HOST),
        };
        let end = beat.end.unwrap_or(beat.start);
        let (start_us, end_us) = (beat.start.as_microsecond(), end.as_microsecond());
        let refused = app.is_empty()
            || (beat.kind == SpanKind::Site && host.is_empty())
            || end_us < start_us
            || end_us - start_us > MAX_SPAN_SECS * MICROS
            || start_us < oldest
            || end_us > newest;
        if refused {
            rejected += 1;
            continue;
        }
        beats.push(Heartbeat {
            kind: beat.kind,
            app,
            name: cut(&beat.name, MAX_NAME),
            host,
            title: if keep_titles { cut(&beat.title, MAX_TITLE) } else { String::new() },
            start: beat.start,
            end: Some(end),
        });
    }
    // Stable, so two beats with one start keep the order they were sent in.
    beats.sort_by_key(|b| (b.start, b.end));

    Ok(Cleaned { device: device.to_string(), collector: collector.to_string(), beats, rejected })
}

/// Trimmed and at most `max` characters -- characters, never bytes, since a
/// title cut through the middle of a multi-byte character is not a shorter
/// title but a broken one.
fn cut(text: &str, max: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(max) {
        Some((end, _)) => text[..end].trim_end().to_string(),
        None => text.to_string(),
    }
}

/// The spans to write for `beats` (already [`clean`]ed), given the stream's
/// last span. A span that was extended keeps its id; new spans get new ones.
/// In order; each id at most once.
pub fn fold(
    last: Option<&ScreenSpan>,
    device: &str,
    collector: &str,
    beats: &[Heartbeat],
) -> Vec<ScreenSpan> {
    let mut out: Vec<ScreenSpan> = Vec::new();
    // The stored span, while it is still worth offering beats to: until one
    // extends it (it moves into `out`) or a beat lands past its reach.
    let mut stored: Option<ScreenSpan> = last.cloned();
    // Which of `out` is the open span. `None` before the first new or
    // extended span, when the stored one is the only thing open.
    let mut open: Option<usize> = None;

    for beat in beats {
        if let Some(i) = open {
            match join(&mut out[i], beat) {
                Join::Inside | Join::Extended => continue,
                Join::Capped(from) => {
                    out.push(new_span(device, collector, beat, from));
                    open = Some(out.len() - 1);
                    continue;
                }
                Join::Apart => {}
            }
        }
        if let Some(span) = stored.as_mut() {
            match join(span, beat) {
                Join::Inside => continue,
                Join::Extended => {
                    out.push(stored.take().expect("just matched"));
                    open = Some(out.len() - 1);
                    continue;
                }
                Join::Capped(from) => {
                    stored = None;
                    out.push(new_span(device, collector, beat, from));
                    open = Some(out.len() - 1);
                    continue;
                }
                Join::Apart => {
                    // A beat beyond the stored span's reach means every
                    // later beat is too: they come in start order.
                    if beat.start.as_microsecond() > span.end.as_microsecond() + PULSE_SECS * MICROS
                    {
                        stored = None;
                    }
                }
            }
        }
        // A new span of another item: the one before it on this stream had
        // the person's attention until this one took it, not only until its
        // last sample -- see `bridge`.
        if let Some(i) = open {
            bridge(&mut out[i], beat.start);
        } else if let Some(mut span) = stored.take() {
            if bridge(&mut span, beat.start) {
                out.push(span);
            } else {
                stored = Some(span);
            }
        }
        out.push(new_span(device, collector, beat, beat.start));
        open = Some(out.len() - 1);
    }
    out
}

/// Stretch `span` to `to` -- the moment the next item on the same stream
/// began -- when the gap between them is short enough to be one sitting.
///
/// A collector samples every few seconds, so a span ends at the last sample
/// that saw it and the next begins at the first that saw something else:
/// the seconds between belong to one of them, and without this to neither.
/// At a five-second sample and a switch every twenty seconds that was a
/// quarter of the day uncounted. Credited to the earlier item, which is
/// what was in front of the person until the sample that noticed the
/// change. Answers whether it moved; never past [`MAX_SPAN_SECS`], and
/// never backwards over an overlap.
fn bridge(span: &mut ScreenSpan, to: Timestamp) -> bool {
    let gap = to.as_microsecond() - span.end.as_microsecond();
    let length = to.as_microsecond() - span.start.as_microsecond();
    if gap <= 0 || gap > PULSE_SECS * MICROS || length > MAX_SPAN_SECS * MICROS {
        return false;
    }
    span.end = to;
    true
}

/// What offering a beat to a span came to.
enum Join {
    /// Another item, before the span began, or too long after it ended.
    Apart,
    /// Wholly inside the span: nothing changes.
    Inside,
    /// The span now reaches the beat's end.
    Extended,
    /// Joining would grow the span past the cap: the beat starts a new
    /// span at this instant (its own start, or the old span's end where the
    /// two overlap) and the old one is left as it was.
    Capped(Timestamp),
}

fn same_item(span: &ScreenSpan, beat: &Heartbeat) -> bool {
    span.kind == beat.kind
        && span.host == beat.host
        && span.app.to_lowercase() == beat.app.to_lowercase()
}

fn join(span: &mut ScreenSpan, beat: &Heartbeat) -> Join {
    if !same_item(span, beat) {
        return Join::Apart;
    }
    let end = beat.end.unwrap_or(beat.start);
    let (start_us, end_us) = (beat.start.as_microsecond(), end.as_microsecond());
    let (span_start, span_end) = (span.start.as_microsecond(), span.end.as_microsecond());
    if start_us < span_start {
        return Join::Apart;
    }
    if end_us <= span_end {
        return Join::Inside;
    }
    if start_us > span_end + PULSE_SECS * MICROS {
        return Join::Apart;
    }
    if end_us - span_start > MAX_SPAN_SECS * MICROS {
        return Join::Capped(beat.start.max(span.end));
    }
    span.end = end;
    if !beat.title.is_empty() {
        span.title = beat.title.clone();
    }
    if !beat.name.is_empty() {
        span.name = beat.name.clone();
    }
    Join::Extended
}

fn new_span(device: &str, collector: &str, beat: &Heartbeat, start: Timestamp) -> ScreenSpan {
    ScreenSpan {
        id: ScreenSpanId::new(),
        device: device.to_string(),
        collector: collector.to_string(),
        kind: beat.kind,
        app: beat.app.clone(),
        name: beat.name.clone(),
        host: beat.host.clone(),
        title: beat.title.clone(),
        start,
        end: beat.end.unwrap_or(beat.start).max(start),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> Timestamp {
        // 2026-11-05T00:00:00Z plus `secs`.
        Timestamp::from_second(1_793_836_800 + secs).unwrap()
    }

    fn now() -> Timestamp {
        at(86_400)
    }

    fn app(name: &str, start: i64, end: Option<i64>) -> Heartbeat {
        Heartbeat {
            kind: SpanKind::App,
            app: name.into(),
            name: String::new(),
            host: String::new(),
            title: String::new(),
            start: at(start),
            end: end.map(at),
        }
    }

    fn site(host: &str, start: i64, end: i64) -> Heartbeat {
        Heartbeat {
            kind: SpanKind::Site,
            app: "Firefox".into(),
            name: String::new(),
            host: host.into(),
            title: String::new(),
            start: at(start),
            end: Some(at(end)),
        }
    }

    fn batch(beats: Vec<Heartbeat>) -> HeartbeatBatch {
        HeartbeatBatch { device: "laptop".into(), collector: "gnome".into(), beats }
    }

    fn cleaned(beats: Vec<Heartbeat>) -> Vec<Heartbeat> {
        clean(&batch(beats), now(), true).unwrap().beats
    }

    fn span_of(beat: &Heartbeat, start: i64, end: i64) -> ScreenSpan {
        let mut span = new_span("laptop", "gnome", beat, at(start));
        span.end = at(end);
        span
    }

    fn secs(span: &ScreenSpan) -> (i64, i64) {
        (span.start.as_second() - at(0).as_second(), span.end.as_second() - at(0).as_second())
    }

    // ---- bridging the switch -------------------------------------------------

    #[test]
    fn the_gap_before_the_next_app_is_credited_to_the_one_before_it() {
        // Sampled every five seconds: code's last sample at 15, slack's
        // first at 20. The five seconds between were code's.
        let beats = cleaned(vec![
            app("code", 0, None),
            app("code", 5, None),
            app("code", 10, None),
            app("code", 15, None),
            app("slack", 20, None),
            app("slack", 25, None),
        ]);
        let spans = fold(None, "laptop", "gnome", &beats);
        assert_eq!(spans.iter().map(secs).collect::<Vec<_>>(), vec![(0, 20), (20, 25)]);
    }

    #[test]
    fn a_long_gap_is_not_bridged_and_neither_is_an_overlap() {
        let beats =
            cleaned(vec![app("code", 0, Some(10)), app("slack", 10 + PULSE_SECS + 1, None)]);
        let spans = fold(None, "laptop", "gnome", &beats);
        assert_eq!(secs(&spans[0]), (0, 10), "somebody looked away");

        let beats = cleaned(vec![app("code", 0, Some(30)), app("slack", 20, Some(40))]);
        let spans = fold(None, "laptop", "gnome", &beats);
        assert_eq!(secs(&spans[0]), (0, 30), "an overlap is the report's to settle");
    }

    #[test]
    fn the_stored_span_is_bridged_and_written_back() {
        let code = app("code", 0, None);
        let last = span_of(&code, 0, 100);
        let beats = cleaned(vec![app("slack", 110, Some(120))]);
        let spans = fold(Some(&last), "laptop", "gnome", &beats);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].id, last.id, "the stored span keeps its id");
        assert_eq!(secs(&spans[0]), (0, 110));
        assert_eq!(secs(&spans[1]), (110, 120));
    }

    // ---- clean --------------------------------------------------------------

    #[test]
    fn a_batch_with_no_device_or_a_bad_collector_is_refused_whole() {
        let mut b = batch(vec![app("code", 0, None)]);
        b.device = "   ".into();
        assert!(matches!(clean(&b, now(), true), Err(Error::Invalid(_))));

        b.device = "x".repeat(MAX_DEVICE + 1);
        assert!(matches!(clean(&b, now(), true), Err(Error::Invalid(_))));
        b.device = "é".repeat(MAX_DEVICE);
        assert!(clean(&b, now(), true).is_ok(), "the limit is characters, not bytes");

        let long = "a".repeat(MAX_COLLECTOR + 1);
        for collector in ["", "GNOME", "my collector", "fire_fox", long.as_str()] {
            let mut b = batch(vec![app("code", 0, None)]);
            b.collector = collector.into();
            assert!(matches!(clean(&b, now(), true), Err(Error::Invalid(_))), "{collector:?}");
        }
        let mut b = batch(vec![]);
        b.collector = "chrome-beta2".into();
        assert!(clean(&b, now(), true).is_ok());
    }

    #[test]
    fn too_many_beats_refuse_the_batch() {
        let beats = (0..=MAX_BEATS as i64).map(|i| app("code", i, None)).collect();
        assert!(matches!(clean(&batch(beats), now(), true), Err(Error::Invalid(_))));
    }

    #[test]
    fn bad_beats_are_dropped_and_counted() {
        let day = 86_400;
        let beats = vec![
            app("code", 100, Some(110)),                         // fine
            app("   ", 100, None),                               // no app
            site("   ", 100, 110),                               // a site with no host
            app("code", 200, Some(150)),                         // ends before it starts
            app("code", 0, Some(MAX_SPAN_SECS + 1)),             // longer than a span
            app("code", day - MAX_BACKLOG_DAYS * day - 1, None), // too old
            app("code", day + MAX_FUTURE_SECS + 60, None),       // too far ahead
            app("code", day + 60, None),                         // a little ahead: clocks drift
        ];
        let out = clean(&batch(beats), now(), true).unwrap();
        assert_eq!(out.rejected, 6);
        assert_eq!(out.beats.len(), 2);
    }

    #[test]
    fn fields_are_trimmed_cut_by_characters_and_normalised() {
        let mut beat = site(" Docs.Google.COM.:443 ", 10, 20);
        beat.title = format!("  {}  ", "ü".repeat(MAX_TITLE + 10));
        beat.name = "  Firefox  ".into();
        beat.app = format!(" {} ", "a".repeat(MAX_APP + 5));
        let out = cleaned(vec![beat]);
        let b = &out[0];
        assert_eq!(b.host, "docs.google.com");
        assert_eq!(b.title.chars().count(), MAX_TITLE);
        assert_eq!(b.name, "Firefox");
        assert_eq!(b.app.chars().count(), MAX_APP);
    }

    #[test]
    fn titles_go_unless_kept_and_a_missing_end_is_the_start() {
        let mut beat = app("code", 10, None);
        beat.title = "secret.rs".into();
        let kept = clean(&batch(vec![beat.clone()]), now(), true).unwrap();
        assert_eq!(kept.beats[0].title, "secret.rs");
        assert_eq!(kept.beats[0].end, Some(at(10)));
        let dropped = clean(&batch(vec![beat]), now(), false).unwrap();
        assert_eq!(dropped.beats[0].title, "");
    }

    #[test]
    fn an_app_beat_carries_no_host() {
        let mut beat = app("code", 10, None);
        beat.host = "github.com".into();
        assert_eq!(cleaned(vec![beat])[0].host, "");
    }

    #[test]
    fn beats_come_out_in_start_order() {
        let out = cleaned(vec![app("c", 30, None), app("a", 10, None), app("b", 20, None)]);
        let apps: Vec<&str> = out.iter().map(|b| b.app.as_str()).collect();
        assert_eq!(apps, ["a", "b", "c"]);
    }

    // ---- fold ---------------------------------------------------------------

    #[test]
    fn heartbeats_close_together_make_one_span() {
        let beats = cleaned((0..10).map(|i| app("code", i * 5, None)).collect());
        let spans = fold(None, "laptop", "gnome", &beats);
        assert_eq!(spans.len(), 1);
        assert_eq!(secs(&spans[0]), (0, 45));
        assert_eq!(spans[0].device, "laptop");
        assert_eq!(spans[0].collector, "gnome");
    }

    #[test]
    fn a_gap_longer_than_the_pulse_starts_a_new_span() {
        let beats = cleaned(vec![
            app("code", 0, Some(10)),
            app("code", 10 + PULSE_SECS, Some(60)),
            app("code", 60 + PULSE_SECS + 1, Some(200)),
        ]);
        let spans = fold(None, "laptop", "gnome", &beats);
        assert_eq!(spans.len(), 2);
        assert_eq!(secs(&spans[0]), (0, 60));
        assert_eq!(secs(&spans[1]), (60 + PULSE_SECS + 1, 200));
    }

    #[test]
    fn another_app_closes_the_open_span() {
        let beats = cleaned(vec![
            app("code", 0, Some(10)),
            app("slack", 12, Some(20)),
            app("code", 22, Some(30)),
        ]);
        let spans = fold(None, "laptop", "gnome", &beats);
        assert_eq!(spans.len(), 3, "focus moved away and back: three runs");
        assert_eq!(spans[0].app, "code");
        assert_eq!(spans[1].app, "slack");
        assert_eq!(spans[2].app, "code");
    }

    #[test]
    fn the_same_app_in_another_case_is_the_same_item_but_another_host_is_not() {
        let beats = cleaned(vec![app("Code", 0, Some(10)), app("code", 12, Some(20))]);
        assert_eq!(fold(None, "laptop", "gnome", &beats).len(), 1);

        let beats = cleaned(vec![site("github.com", 0, 10), site("gitlab.com", 12, 20)]);
        assert_eq!(fold(None, "laptop", "gnome", &beats).len(), 2);
    }

    #[test]
    fn the_stored_span_is_extended_and_keeps_its_id() {
        let beat = app("code", 0, None);
        let last = span_of(&beat, 0, 100);
        let beats = cleaned(vec![app("code", 120, Some(130))]);
        let spans = fold(Some(&last), "laptop", "gnome", &beats);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].id, last.id);
        assert_eq!(secs(&spans[0]), (0, 130));
    }

    #[test]
    fn a_beat_inside_the_open_span_changes_nothing() {
        let beat = app("code", 0, None);
        let last = span_of(&beat, 0, 100);
        let mut inside = app("code", 20, Some(40));
        inside.title = "new title".into();
        let spans = fold(Some(&last), "laptop", "gnome", &cleaned(vec![inside]));
        assert!(spans.is_empty(), "nothing to write");
    }

    #[test]
    fn the_latest_title_and_name_win_and_an_empty_one_does_not() {
        let mut a = app("code", 0, Some(10));
        a.title = "one.rs".into();
        a.name = "Code".into();
        let mut b = app("code", 12, Some(20));
        b.title = "two.rs".into();
        let c = app("code", 22, Some(30));
        let spans = fold(None, "laptop", "gnome", &cleaned(vec![a, b, c]));
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].title, "two.rs");
        assert_eq!(spans[0].name, "Code");
    }

    #[test]
    fn out_of_order_beats_are_folded_as_if_they_had_arrived_in_order() {
        let beats = cleaned(vec![
            app("code", 20, None),
            app("code", 0, None),
            app("code", 10, None),
            app("code", 30, None),
        ]);
        let spans = fold(None, "laptop", "gnome", &beats);
        assert_eq!(spans.len(), 1);
        assert_eq!(secs(&spans[0]), (0, 30));
    }

    #[test]
    fn overlapping_beats_of_different_items_are_each_kept() {
        let beats = cleaned(vec![app("code", 0, Some(100)), app("slack", 50, Some(80))]);
        let spans = fold(None, "laptop", "gnome", &beats);
        assert_eq!(spans.len(), 2, "the report, not the merge, decides who had the overlap");
        assert_eq!(secs(&spans[0]), (0, 100));
        assert_eq!(secs(&spans[1]), (50, 80));
    }

    #[test]
    fn a_backlog_older_than_the_stored_span_does_not_split_it() {
        let beat = app("code", 0, None);
        let last = span_of(&beat, 1_000, 1_100);
        let beats = cleaned(vec![
            app("code", 100, Some(200)),
            app("code", 210, Some(300)),
            // And then the live beat that continues the stored span.
            app("code", 1_120, Some(1_130)),
        ]);
        let spans = fold(Some(&last), "laptop", "gnome", &beats);
        assert_eq!(spans.len(), 2);
        assert_eq!(secs(&spans[0]), (100, 300), "the backlog is a run of its own");
        assert_ne!(spans[0].id, last.id);
        assert_eq!(spans[1].id, last.id, "and the stored span is still extended");
        assert_eq!(secs(&spans[1]), (1_000, 1_130));
    }

    #[test]
    fn a_backlog_inside_the_stored_span_is_a_no_op() {
        let beat = app("code", 0, None);
        let last = span_of(&beat, 1_000, 1_100);
        let spans = fold(Some(&last), "laptop", "gnome", &cleaned(vec![app("code", 1_010, None)]));
        assert!(spans.is_empty());
    }

    #[test]
    fn a_span_never_grows_past_the_cap() {
        // Thirteen hours of heartbeats every thirty seconds.
        let beats: Vec<Heartbeat> = (0..13 * 120).map(|i| app("code", i * 30, None)).collect();
        // `clean` would refuse beats that old relative to `now`, so fold
        // the beats as they are: each is already a cleaned beat's shape.
        let beats: Vec<Heartbeat> =
            beats.into_iter().map(|b| Heartbeat { end: Some(b.start), ..b }).collect();
        let spans = fold(None, "laptop", "gnome", &beats);
        assert_eq!(spans.len(), 2);
        for span in &spans {
            assert!(span.secs() <= MAX_SPAN_SECS, "{} > {MAX_SPAN_SECS}", span.secs());
        }
        assert_eq!(spans[0].start, at(0));
        assert_eq!(spans[0].end, at(MAX_SPAN_SECS));
        assert_eq!(spans[1].start, at(MAX_SPAN_SECS + 30));
        assert_eq!(spans[1].end, at((13 * 120 - 1) * 30));
    }

    #[test]
    fn a_capped_overlapping_beat_starts_where_the_old_span_ends() {
        let beat = app("code", 0, None);
        let last = span_of(&beat, 0, MAX_SPAN_SECS - 10);
        let beats = vec![Heartbeat {
            end: Some(at(MAX_SPAN_SECS + 100)),
            ..app("code", MAX_SPAN_SECS - 50, None)
        }];
        let spans = fold(Some(&last), "laptop", "gnome", &beats);
        assert_eq!(spans.len(), 1, "the stored span is left as it was");
        assert_ne!(spans[0].id, last.id);
        assert_eq!(secs(&spans[0]), (MAX_SPAN_SECS - 10, MAX_SPAN_SECS + 100));
    }

    #[test]
    fn every_id_is_written_at_most_once() {
        let beat = app("code", 0, None);
        let last = span_of(&beat, 0, 10);
        let beats = cleaned(vec![
            app("code", 12, None),
            app("code", 20, None),
            app("slack", 30, None),
            app("slack", 40, None),
        ]);
        let spans = fold(Some(&last), "laptop", "gnome", &beats);
        let mut ids: Vec<_> = spans.iter().map(|s| s.id).collect();
        ids.dedup();
        assert_eq!(ids.len(), spans.len());
        assert_eq!(spans.len(), 2);
    }
}
