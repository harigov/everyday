//! Two tracks into one ordered transcript, without the echo.

use super::{Track, TrackSegment};
use std::collections::HashMap;

/// How much two segments' times are allowed to slip and still count as the
/// same moment: a chunk boundary and two different pipelines' VAD rarely
/// agree to the millisecond.
const OVERLAP_SLACK_MS: i64 = 1_500;

/// Drop mic segments that are the call's audio heard through the speakers:
/// overlapping in time a system segment with near-identical text
/// (normalised token overlap at or above `threshold`, e.g. 0.6). Returns
/// the kept segments of both tracks, ordered by start, and whether any
/// echo was found.
pub fn remove_echo(segments: Vec<TrackSegment>, threshold: f32) -> (Vec<TrackSegment>, bool) {
    let system: Vec<&TrackSegment> = segments.iter().filter(|s| s.track == Track::System).collect();

    let mut echo_found = false;
    let mut kept: Vec<TrackSegment> = Vec::with_capacity(segments.len());

    for seg in &segments {
        if seg.track == Track::Mic {
            let is_echo = system.iter().any(|sys| {
                overlaps_with_slack(seg, sys) && similarity(&seg.text, &sys.text) >= threshold
            });
            if is_echo {
                echo_found = true;
                continue;
            }
        }
        kept.push(seg.clone());
    }

    kept.sort_by(|a, b| a.start_ms.cmp(&b.start_ms).then_with(|| a.track.cmp(&b.track)));
    (kept, echo_found)
}

fn overlaps_with_slack(a: &TrackSegment, b: &TrackSegment) -> bool {
    let a_start = a.start_ms as i64 - OVERLAP_SLACK_MS;
    let a_end = a.end_ms as i64 + OVERLAP_SLACK_MS;
    let b_start = b.start_ms as i64;
    let b_end = b.end_ms as i64;
    a_start <= b_end && b_start <= a_end
}

/// Normalised token overlap of two strings, 0.0..=1.0.
///
/// Lower-cased, punctuation stripped, split on whitespace; the overlap is
/// the size of the multiset intersection over the shorter of the two token
/// counts, so "yes" against "yes yes yes" scores 1.0 rather than 0.33 --
/// echo is about whether the mic heard *the same words*, not the same
/// number of them.
pub fn similarity(a: &str, b: &str) -> f32 {
    let ta = tokenize(a);
    let tb = tokenize(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }

    let mut counts: HashMap<&str, i32> = HashMap::new();
    for t in &ta {
        *counts.entry(t.as_str()).or_insert(0) += 1;
    }
    let mut overlap = 0;
    for t in &tb {
        if let Some(c) = counts.get_mut(t.as_str()) {
            if *c > 0 {
                *c -= 1;
                overlap += 1;
            }
        }
    }
    overlap as f32 / ta.len().min(tb.len()) as f32
}

fn tokenize(s: &str) -> Vec<String> {
    s.to_ascii_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(track: Track, start_ms: u64, end_ms: u64, text: &str) -> TrackSegment {
        TrackSegment {
            track,
            start_ms,
            end_ms,
            text: text.to_string(),
            speaker_hint: None,
            hint_scope: 0,
        }
    }

    #[test]
    fn similarity_ignores_case_and_punctuation() {
        assert!((similarity("Hello, world!", "hello world") - 1.0).abs() < 1e-6);
    }

    #[test]
    fn similarity_of_empty_text_is_zero() {
        assert_eq!(similarity("", "hello"), 0.0);
        assert_eq!(similarity("hello", ""), 0.0);
        assert_eq!(similarity("", ""), 0.0);
    }

    #[test]
    fn similarity_is_a_fraction_of_the_shorter_side() {
        // "yes" fully inside "yes yes yes" -- overlap 1, shorter side is 1 token.
        assert!((similarity("yes", "yes yes yes") - 1.0).abs() < 1e-6);
        // Two of three words shared.
        let s = similarity("we should ship it", "we should not ship");
        assert!(s > 0.5 && s < 1.0, "{s}");
    }

    #[test]
    fn an_exact_echo_is_dropped() {
        let segments = vec![
            seg(Track::System, 1_000, 3_000, "let's ship on Friday"),
            seg(Track::Mic, 1_200, 3_100, "let's ship on Friday"),
        ];
        let (kept, echo) = remove_echo(segments, 0.6);
        assert!(echo);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].track, Track::System);
    }

    #[test]
    fn a_genuine_short_reply_sharing_a_few_words_is_kept() {
        let segments = vec![
            seg(Track::System, 1_000, 4_000, "should we ship the design on Friday or Monday"),
            seg(Track::Mic, 4_200, 5_000, "let's ship Friday"),
        ];
        let (kept, echo) = remove_echo(segments, 0.6);
        assert!(!echo, "a real reply must not be treated as echo");
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn echo_needs_both_overlap_and_similar_text() {
        // Overlaps in time, but says something different: not echo.
        let different_text = vec![
            seg(Track::System, 1_000, 3_000, "let's ship on Friday"),
            seg(Track::Mic, 1_200, 3_100, "sounds good to me"),
        ];
        let (kept, echo) = remove_echo(different_text, 0.6);
        assert!(!echo);
        assert_eq!(kept.len(), 2);

        // Same text, but nowhere near each other in time: not echo.
        let no_overlap = vec![
            seg(Track::System, 1_000, 2_000, "let's ship on Friday"),
            seg(Track::Mic, 60_000, 61_000, "let's ship on Friday"),
        ];
        let (kept, echo) = remove_echo(no_overlap, 0.6);
        assert!(!echo);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn slack_covers_a_chunk_boundarys_worth_of_drift() {
        let segments = vec![
            seg(Track::System, 1_000, 2_000, "let's ship on Friday"),
            // Starts 1.4s after the system segment ends -- within the 1.5s slack.
            seg(Track::Mic, 3_400, 4_400, "let's ship on Friday"),
        ];
        let (_, echo) = remove_echo(segments, 0.6);
        assert!(echo);
    }

    #[test]
    fn kept_segments_are_sorted_by_start_then_track() {
        let segments = vec![
            seg(Track::Mic, 5_000, 6_000, "later mic"),
            seg(Track::System, 1_000, 2_000, "earlier system"),
            seg(Track::Mic, 1_000, 2_000, "earlier mic"),
        ];
        let (kept, _) = remove_echo(segments, 0.6);
        assert_eq!(kept[0].text, "earlier mic");
        assert_eq!(kept[0].track, Track::Mic);
        assert_eq!(kept[1].text, "earlier system");
        assert_eq!(kept[2].text, "later mic");
    }
}
