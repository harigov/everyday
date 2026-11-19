//! What the session asked to be played, paced out. See `super`.
//!
//! A call has no sound card to pull audio at its own pace: the network is
//! sent 20 ms every 20 ms, and a stream that skips a beat is heard as a
//! stutter or dropped as a dead line. So what the session says is queued
//! here, and every 20 ms the call takes one frame from it -- speech if there
//! is any and it is not held, silence if not.
//!
//! This is the call's twin of the desktop's `audio::playback::PlaybackQueue`,
//! and keeps its rules: pause holds where it is, resume carries on, clear
//! throws away everything queued and also the hold, and an utterance is
//! reported once its last sample has gone out, never if it was cleared first.
//! It is pure data, so those rules are tested without a network.

use super::codec::FRAME;
use std::collections::VecDeque;

struct Segment {
    utterance: u64,
    samples: Vec<f32>,
    /// How much of `samples` has gone out.
    position: usize,
}

/// The speech waiting to be sent, at the call's 48 kHz.
#[derive(Default)]
pub struct Outbound {
    segments: VecDeque<Segment>,
    paused: bool,
    /// Utterances with nothing in them that were queued behind nothing: they
    /// have nothing to wait for, and are reported with the next frame.
    finished: Vec<u64>,
}

impl Outbound {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue an utterance, already at 48 kHz, after whatever is queued.
    ///
    /// One with nothing in it is reported at once if nothing is ahead of it,
    /// and when what is ahead of it has gone if something is, so reports come
    /// in the order the utterances were sent.
    pub fn push(&mut self, utterance: u64, samples: Vec<f32>) {
        if samples.is_empty() && self.segments.is_empty() {
            self.finished.push(utterance);
            return;
        }
        self.segments.push_back(Segment { utterance, samples, position: 0 });
    }

    /// Hold where it is: frames are silence until [`resume`](Self::resume).
    pub fn pause(&mut self) {
        self.paused = true;
    }

    pub fn resume(&mut self) {
        self.paused = false;
    }

    /// Throw away everything queued, and the hold: what is said next is not
    /// meant to wait on a pause that was about what was said before.
    /// Utterances thrown away are not reported: they were not played.
    pub fn clear(&mut self) {
        self.paused = false;
        self.segments.clear();
    }

    /// The next 20 ms: queued speech, or silence while there is none or it is
    /// held, with the utterances whose last sample is in this frame (or that
    /// were empty), oldest first.
    pub fn next_frame(&mut self) -> ([f32; FRAME], Vec<u64>) {
        let mut frame = [0.0; FRAME];
        let mut written = 0;
        while !self.paused {
            let Some(segment) = self.segments.front_mut() else { break };
            let left = segment.samples.len() - segment.position;
            if left > 0 {
                if written == FRAME {
                    break;
                }
                let count = left.min(FRAME - written);
                let from = segment.position;
                frame[written..written + count]
                    .copy_from_slice(&segment.samples[from..from + count]);
                segment.position += count;
                written += count;
                if segment.position < segment.samples.len() {
                    continue;
                }
            }
            // Either the last of it just went, or it was empty to begin with.
            if let Some(done) = self.segments.pop_front() {
                self.finished.push(done.utterance);
            }
        }
        (frame, std::mem::take(&mut self.finished))
    }

    /// Whether any speech is queued, held or not.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(len: usize, from: f32) -> Vec<f32> {
        (0..len).map(|i| from + i as f32).collect()
    }

    #[test]
    fn frames_are_960_samples_and_silent_when_nothing_is_queued() {
        let mut out = Outbound::new();
        let (frame, done) = out.next_frame();
        assert_eq!(frame.len(), 960);
        assert!(frame.iter().all(|&s| s == 0.0));
        assert!(done.is_empty());
    }

    #[test]
    fn speech_is_cut_into_frames_and_the_last_one_is_padded_with_silence() {
        let mut out = Outbound::new();
        out.push(1, ramp(1500, 1.0));

        let (first, done) = out.next_frame();
        assert_eq!(first[0], 1.0);
        assert_eq!(first[959], 960.0);
        assert!(done.is_empty(), "540 samples are still to come");

        let (second, done) = out.next_frame();
        assert_eq!(second[0], 961.0);
        assert_eq!(second[539], 1500.0);
        assert!(second[540..].iter().all(|&s| s == 0.0), "the rest of the frame is silence");
        assert_eq!(done, vec![1]);
        assert!(out.is_empty());
    }

    #[test]
    fn an_utterance_ending_exactly_at_the_end_of_a_frame_is_reported_there() {
        let mut out = Outbound::new();
        out.push(1, vec![0.5; 960]);
        out.push(2, vec![0.25; 960]);

        let (frame, done) = out.next_frame();
        assert!(frame.iter().all(|&s| s == 0.5));
        assert_eq!(done, vec![1], "not a frame later");

        let (frame, done) = out.next_frame();
        assert!(frame.iter().all(|&s| s == 0.25));
        assert_eq!(done, vec![2]);
    }

    #[test]
    fn utterances_play_in_order_and_several_can_end_in_one_frame() {
        let mut out = Outbound::new();
        out.push(1, vec![1.0; 100]);
        out.push(2, vec![2.0; 100]);
        out.push(3, vec![3.0; 100]);

        let (frame, done) = out.next_frame();
        assert_eq!(&frame[..3], &[1.0, 1.0, 1.0]);
        assert_eq!(frame[100], 2.0);
        assert_eq!(frame[299], 3.0);
        assert_eq!(frame[300], 0.0);
        assert_eq!(done, vec![1, 2, 3]);
    }

    #[test]
    fn pausing_gives_silence_and_resuming_carries_on_where_it_held() {
        let mut out = Outbound::new();
        out.push(1, ramp(1500, 1.0));
        let (first, _) = out.next_frame();
        assert_eq!(first[959], 960.0);

        out.pause();
        for _ in 0..3 {
            let (frame, done) = out.next_frame();
            assert!(frame.iter().all(|&s| s == 0.0), "held");
            assert!(done.is_empty(), "nothing finishes while held");
        }

        out.resume();
        let (frame, done) = out.next_frame();
        assert_eq!(frame[0], 961.0, "carries on from the very next sample");
        assert_eq!(frame[539], 1500.0);
        assert_eq!(done, vec![1]);
    }

    #[test]
    fn clearing_throws_it_all_away_without_reporting_it() {
        let mut out = Outbound::new();
        out.push(1, ramp(3000, 1.0));
        out.push(2, vec![2.0; 500]);
        out.next_frame();

        out.clear();
        assert!(out.is_empty());
        let (frame, done) = out.next_frame();
        assert!(frame.iter().all(|&s| s == 0.0));
        assert!(done.is_empty(), "neither the one half played nor the one never started");
    }

    #[test]
    fn clearing_also_lets_go_of_a_hold() {
        let mut out = Outbound::new();
        out.push(1, vec![1.0; 100]);
        out.pause();
        out.clear();
        out.push(2, vec![2.0; 100]);
        let (frame, done) = out.next_frame();
        assert_eq!(frame[0], 2.0, "what is said after a clear is not held");
        assert_eq!(done, vec![2]);
    }

    #[test]
    fn an_empty_utterance_is_reported_at_once_when_nothing_is_ahead_of_it() {
        let mut out = Outbound::new();
        out.push(1, Vec::new());
        let (_, done) = out.next_frame();
        assert_eq!(done, vec![1]);
    }

    #[test]
    fn an_empty_utterance_behind_speech_is_reported_after_it() {
        let mut out = Outbound::new();
        out.push(1, vec![1.0; 1000]);
        out.push(2, Vec::new());
        out.push(3, vec![3.0; 10]);

        let (_, done) = out.next_frame();
        assert!(done.is_empty());
        let (_, done) = out.next_frame();
        assert_eq!(done, vec![1, 2, 3], "in the order they were sent");
    }
}
