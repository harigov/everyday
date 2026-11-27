//! Two tracks into one ordered transcript, without the echo.

use super::echo::{EchoCheck, is_echo};
use super::{Track, TrackSegment};

/// Drop mic segments that are the call's audio heard through the speakers --
/// see [`super::echo::is_echo`], which decides, and never does without the
/// audio's own evidence that echo is present (`check`). Returns the kept
/// segments of both tracks, ordered by start, and whether any echo was
/// dropped.
pub fn remove_echo(segments: Vec<TrackSegment>, check: &EchoCheck) -> (Vec<TrackSegment>, bool) {
    let system: Vec<&TrackSegment> = segments.iter().filter(|s| s.track == Track::System).collect();
    let dropped: Vec<bool> = segments
        .iter()
        .map(|seg| seg.track == Track::Mic && is_echo(seg, &system, check))
        .collect();
    let echo_found = dropped.iter().any(|d| *d);
    let mut kept: Vec<TrackSegment> =
        segments.iter().zip(&dropped).filter(|(_, d)| !**d).map(|(s, _)| s.clone()).collect();
    kept.sort_by(|a, b| a.start_ms.cmp(&b.start_ms).then_with(|| a.track.cmp(&b.track)));
    (kept, echo_found)
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
    fn an_exact_echo_is_dropped_when_echo_was_heard() {
        let segments = vec![
            seg(Track::Mic, 4_000, 5_000, "sounds good to me"),
            seg(Track::Mic, 1_200, 3_100, "let's ship on Friday"),
            seg(Track::System, 1_000, 3_000, "let's ship on Friday"),
        ];
        let (kept, echo) = remove_echo(segments, &EchoCheck::assume_present());
        assert!(echo);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].track, Track::System);
        assert_eq!(kept[1].text, "sounds good to me");
    }

    #[test]
    fn without_evidence_of_echo_nothing_is_dropped() {
        // Headphones: a line that repeats theirs is the owner's own.
        let segments = vec![
            seg(Track::System, 1_000, 3_000, "let's ship on Friday"),
            seg(Track::Mic, 1_200, 3_100, "let's ship on Friday"),
            seg(Track::Mic, 3_200, 3_800, "yeah, Friday"),
        ];
        let (kept, echo) = remove_echo(segments, &EchoCheck::none());
        assert!(!echo);
        assert_eq!(kept.len(), 3);
    }

    #[test]
    fn a_genuine_short_reply_is_kept_even_when_echo_was_heard() {
        let segments = vec![
            seg(Track::System, 1_000, 3_500, "do you think we can ship the design on Friday"),
            seg(Track::Mic, 3_600, 4_400, "Yeah, I think so."),
            seg(Track::Mic, 4_500, 4_900, "right okay"),
            seg(Track::Mic, 5_000, 5_800, "let's ship Friday"),
        ];
        let (kept, echo) = remove_echo(segments, &EchoCheck::assume_present());
        assert!(!echo, "a real reply must not be treated as echo: {kept:?}");
        assert_eq!(kept.len(), 4);
    }

    #[test]
    fn echo_needs_both_the_same_moment_and_the_same_words() {
        let present = EchoCheck::assume_present();
        // The same moment, different words: not echo.
        let different_text = vec![
            seg(Track::System, 1_000, 3_000, "let's ship on Friday"),
            seg(Track::Mic, 1_200, 3_100, "sounds good to me"),
        ];
        let (kept, echo) = remove_echo(different_text, &present);
        assert!(!echo);
        assert_eq!(kept.len(), 2);

        // The same words, nowhere near each other in time: not echo.
        let no_overlap = vec![
            seg(Track::System, 1_000, 2_000, "let's ship on Friday"),
            seg(Track::Mic, 60_000, 61_000, "let's ship on Friday"),
        ];
        let (kept, echo) = remove_echo(no_overlap, &present);
        assert!(!echo);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn a_copy_begun_after_the_speakers_tail_is_a_read_back() {
        let present = EchoCheck::assume_present();
        // Starts 400 ms after their line ended: still coming out of the speakers.
        let tail = vec![
            seg(Track::System, 1_000, 2_000, "let's ship on Friday"),
            seg(Track::Mic, 2_400, 3_400, "let's ship on Friday"),
        ];
        let (kept, echo) = remove_echo(tail, &present);
        assert!(echo);
        assert_eq!(kept.len(), 1);

        // Starts 600 ms after: the owner saying it back.
        let read_back = vec![
            seg(Track::System, 1_000, 2_000, "let's ship on Friday"),
            seg(Track::Mic, 2_600, 3_600, "let's ship on Friday"),
        ];
        let (kept, echo) = remove_echo(read_back, &present);
        assert!(!echo);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn kept_segments_are_sorted_by_start_then_track() {
        let segments = vec![
            seg(Track::Mic, 5_000, 6_000, "later mic"),
            seg(Track::System, 1_000, 2_000, "earlier system"),
            seg(Track::Mic, 1_000, 2_000, "earlier mic"),
        ];
        let (kept, _) = remove_echo(segments, &EchoCheck::none());
        assert_eq!(kept[0].text, "earlier mic");
        assert_eq!(kept[0].track, Track::Mic);
        assert_eq!(kept[1].text, "earlier system");
        assert_eq!(kept[2].text, "later mic");
    }
}
