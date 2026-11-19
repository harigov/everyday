//! What is waiting to come out of the speakers.
//!
//! A queue of utterances, each a stretch of mono audio already at the output
//! device's rate, with a marker at its end so the caller can say exactly when
//! the last sample of one has been handed to the device. Pure data: the device
//! callback calls [`PlaybackQueue::fill`] under a `try_lock`, and everything
//! else -- pausing, clearing, collecting what finished -- is done by the audio
//! engine thread between callbacks.
//!
//! The callback must not allocate or free, so a segment that has been played
//! out is not dropped there. It moves to a graveyard the engine thread empties
//! ([`PlaybackQueue::take_finished`]), and the lists it moves into are kept at
//! a capacity that makes a push in the callback a copy, not a growth.

use std::collections::VecDeque;

/// How many finished utterances, and how many spent buffers, the lists hold
/// without growing. Several sentences can end between two visits of the engine
/// thread; this is generous for that.
const SPARE: usize = 32;

struct Segment {
    utterance: u64,
    samples: Vec<f32>,
    /// How much of `samples` has gone to the device.
    position: usize,
}

pub struct PlaybackQueue {
    segments: VecDeque<Segment>,
    paused: bool,
    /// Utterances whose last sample has gone to the device, oldest first.
    finished: Vec<u64>,
    /// Their buffers, waiting for a thread that may free them.
    spent: Vec<Vec<f32>>,
}

impl Default for PlaybackQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackQueue {
    pub fn new() -> Self {
        Self {
            segments: VecDeque::with_capacity(SPARE),
            paused: false,
            finished: Vec::with_capacity(SPARE),
            spent: Vec::with_capacity(SPARE),
        }
    }

    /// Queue an utterance after whatever is queued.
    ///
    /// An utterance with nothing in it has nothing to wait for: it is finished
    /// at once if nothing is ahead of it, and when the audio ahead of it has
    /// gone if something is, so reports still come in the order the utterances
    /// were sent.
    pub fn push(&mut self, utterance: u64, samples: Vec<f32>) {
        if samples.is_empty() && self.segments.is_empty() {
            self.finished.push(utterance);
            return;
        }
        self.segments.push_back(Segment { utterance, samples, position: 0 });
    }

    /// Hold where it is. [`fill`](Self::fill) gives silence until resumed.
    pub fn pause(&mut self) {
        self.paused = true;
    }

    pub fn resume(&mut self) {
        self.paused = false;
    }

    /// Throw away everything queued, and the hold too: what is said next is
    /// not meant to wait on a pause that was about what was said before.
    /// Utterances thrown away are not reported as finished -- they were not
    /// played.
    pub fn clear(&mut self) {
        self.paused = false;
        while let Some(segment) = self.segments.pop_front() {
            self.spent.push(segment.samples);
        }
    }

    /// Write the next `out.len()` mono samples, silence for whatever there is
    /// not (or all of it, when paused). Returns how many were audio.
    ///
    /// Every utterance whose last sample lands in `out`, even exactly at its
    /// end, is finished by the time this returns.
    pub fn fill(&mut self, out: &mut [f32]) -> usize {
        let mut written = 0;
        while !self.paused {
            let Some(segment) = self.segments.front_mut() else { break };
            let left = segment.samples.len() - segment.position;
            if left > 0 {
                if written == out.len() {
                    break;
                }
                let count = left.min(out.len() - written);
                let from = segment.position;
                out[written..written + count].copy_from_slice(&segment.samples[from..from + count]);
                segment.position += count;
                written += count;
                if segment.position < segment.samples.len() {
                    continue;
                }
            }
            // Either the last of it just went, or it was empty to begin with.
            if let Some(done) = self.segments.pop_front() {
                self.finished.push(done.utterance);
                self.spent.push(done.samples);
            }
        }
        out[written..].fill(0.0);
        written
    }

    /// Whether [`take_finished`](Self::take_finished) has anything. Cheap, so
    /// the callback can raise a flag the engine thread checks without taking
    /// the lock every few milliseconds.
    pub fn has_finished(&self) -> bool {
        !self.finished.is_empty() || !self.spent.is_empty()
    }

    /// Move what has finished into `finished` (oldest first) and the buffers
    /// that can now be freed into `spent`, which the caller drops somewhere it
    /// is allowed to free.
    pub fn take_finished(&mut self, finished: &mut Vec<u64>, spent: &mut Vec<Vec<f32>>) {
        finished.append(&mut self.finished);
        spent.append(&mut self.spent);
    }

    /// How many samples are still to be played.
    #[cfg(test)]
    pub fn queued(&self) -> usize {
        self.segments.iter().map(|s| s.samples.len() - s.position).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finished(queue: &mut PlaybackQueue) -> Vec<u64> {
        let (mut done, mut spent) = (Vec::new(), Vec::new());
        queue.take_finished(&mut done, &mut spent);
        done
    }

    #[test]
    fn utterances_play_in_the_order_they_were_queued() {
        let mut queue = PlaybackQueue::new();
        queue.push(1, vec![1.0, 2.0, 3.0]);
        queue.push(2, vec![4.0, 5.0]);

        let mut out = [0.0; 4];
        assert_eq!(queue.fill(&mut out), 4);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(finished(&mut queue), vec![1]);

        let mut out = [9.0; 4];
        assert_eq!(queue.fill(&mut out), 1);
        assert_eq!(out, [5.0, 0.0, 0.0, 0.0], "the rest of the buffer is silence");
        assert_eq!(finished(&mut queue), vec![2]);
        assert_eq!(queue.queued(), 0);
    }

    #[test]
    fn an_utterance_is_reported_only_when_its_last_sample_has_gone() {
        let mut queue = PlaybackQueue::new();
        queue.push(7, vec![0.5; 6]);

        let mut out = [0.0; 4];
        queue.fill(&mut out);
        assert!(finished(&mut queue).is_empty(), "two samples are still to come");

        queue.fill(&mut out);
        assert_eq!(finished(&mut queue), vec![7]);
    }

    #[test]
    fn an_utterance_ending_exactly_at_the_end_of_a_buffer_is_reported_there() {
        let mut queue = PlaybackQueue::new();
        queue.push(1, vec![0.5; 4]);
        queue.push(2, vec![0.25; 4]);

        let mut out = [0.0; 4];
        queue.fill(&mut out);
        assert_eq!(finished(&mut queue), vec![1]);
        assert_eq!(out, [0.5; 4]);

        queue.fill(&mut out);
        assert_eq!(finished(&mut queue), vec![2]);
    }

    #[test]
    fn many_utterances_can_end_in_one_buffer() {
        let mut queue = PlaybackQueue::new();
        queue.push(1, vec![1.0]);
        queue.push(2, vec![2.0]);
        queue.push(3, vec![3.0]);

        let mut out = [0.0; 8];
        assert_eq!(queue.fill(&mut out), 3);
        assert_eq!(finished(&mut queue), vec![1, 2, 3]);
    }

    #[test]
    fn pausing_gives_silence_and_resuming_carries_on_where_it_left_off() {
        let mut queue = PlaybackQueue::new();
        queue.push(1, vec![1.0, 2.0, 3.0, 4.0]);

        let mut out = [0.0; 2];
        queue.fill(&mut out);
        assert_eq!(out, [1.0, 2.0]);

        queue.pause();
        let mut out = [9.0; 2];
        assert_eq!(queue.fill(&mut out), 0);
        assert_eq!(out, [0.0, 0.0]);
        assert_eq!(queue.queued(), 2, "nothing was lost while it was held");
        assert!(finished(&mut queue).is_empty());

        queue.resume();
        let mut out = [0.0; 2];
        queue.fill(&mut out);
        assert_eq!(out, [3.0, 4.0]);
        assert_eq!(finished(&mut queue), vec![1]);
    }

    #[test]
    fn nothing_finishes_while_paused() {
        let mut queue = PlaybackQueue::new();
        queue.push(1, vec![1.0]);
        queue.pause();
        queue.fill(&mut [0.0; 4]);
        assert!(finished(&mut queue).is_empty());
        queue.resume();
        queue.fill(&mut [0.0; 4]);
        assert_eq!(finished(&mut queue), vec![1]);
    }

    #[test]
    fn clearing_throws_the_rest_away_without_reporting_it() {
        let mut queue = PlaybackQueue::new();
        queue.push(1, vec![1.0; 4]);
        queue.push(2, vec![2.0; 4]);

        let mut out = [0.0; 2];
        queue.fill(&mut out);
        queue.clear();

        let mut out = [9.0; 4];
        assert_eq!(queue.fill(&mut out), 0);
        assert_eq!(out, [0.0; 4]);
        assert!(finished(&mut queue).is_empty(), "neither was played to the end");
        assert_eq!(queue.queued(), 0);
    }

    #[test]
    fn clearing_also_ends_a_pause_so_what_comes_next_is_heard() {
        let mut queue = PlaybackQueue::new();
        queue.push(1, vec![1.0; 4]);
        queue.pause();
        queue.clear();
        queue.push(2, vec![2.0; 2]);

        let mut out = [0.0; 4];
        assert_eq!(queue.fill(&mut out), 2);
        assert_eq!(out, [2.0, 2.0, 0.0, 0.0]);
        assert_eq!(finished(&mut queue), vec![2]);
    }

    #[test]
    fn what_finished_before_a_clear_is_still_reported() {
        let mut queue = PlaybackQueue::new();
        queue.push(1, vec![1.0; 2]);
        queue.push(2, vec![2.0; 2]);
        queue.fill(&mut [0.0; 2]);
        queue.clear();
        assert_eq!(finished(&mut queue), vec![1]);
    }

    #[test]
    fn an_empty_utterance_is_reported_at_once_when_nothing_is_ahead_of_it() {
        let mut queue = PlaybackQueue::new();
        queue.push(1, Vec::new());
        assert!(queue.has_finished());
        assert_eq!(finished(&mut queue), vec![1]);
        assert!(!queue.has_finished());
    }

    #[test]
    fn an_empty_utterance_waits_its_turn_behind_audio() {
        let mut queue = PlaybackQueue::new();
        queue.push(1, vec![1.0; 4]);
        queue.push(2, Vec::new());
        queue.push(3, vec![3.0; 2]);

        let mut out = [0.0; 4];
        queue.fill(&mut out);
        assert_eq!(finished(&mut queue), vec![1, 2], "the empty one follows the one before it");

        queue.fill(&mut out);
        assert_eq!(finished(&mut queue), vec![3]);
    }

    #[test]
    fn spent_buffers_are_handed_over_for_freeing_elsewhere() {
        let mut queue = PlaybackQueue::new();
        queue.push(1, vec![1.0; 2]);
        queue.fill(&mut [0.0; 2]);
        let (mut done, mut spent) = (Vec::new(), Vec::new());
        queue.take_finished(&mut done, &mut spent);
        assert_eq!(done, vec![1]);
        assert_eq!(spent.len(), 1);
        assert!(!queue.has_finished());
    }
}
