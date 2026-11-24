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
//!
//! Under the speech, when there is none, the sound of the assistant working
//! ([`Bed`]) can play: built on the engine thread, only filled here.
//!
//! Over everything, once, the listening sound
//! ([`listening_chime`](karmayoga_service::voice::cue::listening_chime)) can
//! be played: it is not speech, so it is neither reported nor held, and it is
//! mixed in rather than queued, so it never cuts speech off or waits for it.

use std::collections::VecDeque;

use karmayoga_service::voice::cue::Bed;

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
    /// The sound of working, once it has been asked for.
    bed: Option<Bed>,
    /// The listening sound, at the speaker's rate; empty for a queue with
    /// nothing to play it on.
    chime: Vec<f32>,
    /// How much of it has gone to the device: `chime.len()` or more when it
    /// is not sounding.
    chime_at: usize,
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
            bed: None,
            chime: Vec::new(),
            chime_at: 0,
        }
    }

    /// A queue for a speaker that can play the sound of working, built
    /// where building may allocate: when the speaker opens.
    pub fn with_working_sound(bed: Bed) -> Self {
        Self { bed: Some(bed), ..Self::new() }
    }

    /// The same, able to play the listening sound too: `chime` is made for
    /// the speaker's rate where making it may allocate, and only read once
    /// the callback has it.
    pub fn with_chime(mut self, chime: Vec<f32>) -> Self {
        self.chime_at = chime.len();
        self.chime = chime;
        self
    }

    /// Play the sound of working while nothing else plays, or stop it.
    pub fn set_working(&mut self, on: bool) {
        if let Some(bed) = self.bed.as_mut() {
            bed.set(on);
        }
    }

    /// Play the listening sound, once, from its start, over whatever else is
    /// playing. Nothing if the queue has no such sound, and nothing again
    /// while it is sounding already: restarting it mid-note would click.
    pub fn play_chime(&mut self) {
        if self.chime_at >= self.chime.len() {
            self.chime_at = 0;
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
    /// played. The listening sound, if it is sounding, is thrown away too:
    /// whatever asked to clear is about the person talking, and it is no
    /// longer wanted.
    pub fn clear(&mut self) {
        self.paused = false;
        self.chime_at = self.chime.len();
        while let Some(segment) = self.segments.pop_front() {
            self.spent.push(segment.samples);
        }
    }

    /// Write the next `out.len()` mono samples, silence for whatever there is
    /// not (or all of it, when paused) -- or the sound of working, if it is
    /// on and nothing is queued or held -- and the listening sound over any
    /// of it, if it is sounding: not speech, and not held by a pause, which is
    /// about speech. Returns how many were speech.
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
        let rest = &mut out[written..];
        match self.bed.as_mut() {
            // Speech comes first, and a hold is a hold on everything.
            Some(bed) => bed.fill_idle(rest, !self.paused && self.segments.is_empty()),
            None => rest.fill(0.0),
        }
        self.mix_chime(out);
        written
    }

    /// Add the next of the listening sound, if it is sounding, to `out`. A sum
    /// that would go past full scale is held there: it is soft, and so is
    /// nearly everything it is played over, but the device's range is not
    /// something to find out by overshooting.
    fn mix_chime(&mut self, out: &mut [f32]) {
        let Some(rest) = self.chime.get(self.chime_at..).filter(|rest| !rest.is_empty()) else {
            return;
        };
        let count = rest.len().min(out.len());
        for (sample, tone) in out.iter_mut().zip(rest) {
            *sample = (*sample + tone).clamp(-1.0, 1.0);
        }
        self.chime_at += count;
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
    fn the_working_sound_plays_only_when_nothing_else_is_queued() {
        let mut queue = PlaybackQueue::with_working_sound(Bed::new(1_000));
        queue.set_working(true);
        // A second of it: its pause, then its pulse.
        let mut out = [0.0; 1_000];
        assert_eq!(queue.fill(&mut out), 0, "none of it is speech");
        assert!(out.iter().any(|&s| s != 0.0), "the pulse is heard");

        queue.push(1, vec![0.5; 4]);
        let mut out = [0.0; 4];
        queue.fill(&mut out);
        assert_eq!(out, [0.5; 4], "speech is never mixed with it");

        queue.push(2, vec![0.25; 4]);
        queue.pause();
        let mut out = [9.0; 4];
        queue.fill(&mut out);
        assert_eq!(out, [0.0; 4], "a hold is silence");
    }

    #[test]
    fn the_working_sound_stops_when_told() {
        let mut queue = PlaybackQueue::with_working_sound(Bed::new(1_000));
        queue.set_working(true);
        queue.fill(&mut [0.0; 700]);
        queue.set_working(false);
        let mut out = [1.0; 200];
        queue.fill(&mut out);
        assert!(out[100..].iter().all(|&s| s == 0.0), "faded out and gone");
    }

    #[test]
    fn the_listening_sound_plays_once_and_is_not_reported() {
        let mut queue = PlaybackQueue::new().with_chime(vec![0.25, 0.5, 0.25]);
        let mut out = [9.0; 4];
        queue.fill(&mut out);
        assert_eq!(out, [0.0; 4], "nothing until it is asked for");

        queue.play_chime();
        let mut out = [0.0; 2];
        assert_eq!(queue.fill(&mut out), 0, "none of it is speech");
        assert_eq!(out, [0.25, 0.5]);
        let mut out = [0.0; 4];
        queue.fill(&mut out);
        assert_eq!(out, [0.25, 0.0, 0.0, 0.0], "the rest of it, then silence");
        queue.fill(&mut out);
        assert_eq!(out, [0.0; 4], "once, not round again");
        assert!(!queue.has_finished(), "it is not an utterance anybody waits on");
        assert!(finished(&mut queue).is_empty());
    }

    #[test]
    fn the_listening_sound_is_mixed_over_speech_and_never_cuts_it_off() {
        let mut queue = PlaybackQueue::new().with_chime(vec![0.25; 2]);
        queue.push(1, vec![0.5; 4]);
        queue.fill(&mut [0.0; 1]);
        queue.play_chime();

        let mut out = [0.0; 4];
        assert_eq!(queue.fill(&mut out), 3, "all that was left of the speech");
        assert_eq!(out, [0.75, 0.75, 0.5, 0.0], "both, and the speech goes on to its end");
        assert_eq!(finished(&mut queue), vec![1], "reported when its last sample has gone");
    }

    #[test]
    fn the_listening_sound_is_not_held_by_a_pause_and_is_never_past_full_scale() {
        let mut queue = PlaybackQueue::new().with_chime(vec![0.5; 2]);
        queue.push(1, vec![0.9; 4]);
        queue.pause();
        queue.play_chime();
        let mut out = [9.0; 3];
        assert_eq!(queue.fill(&mut out), 0);
        assert_eq!(out, [0.5, 0.5, 0.0], "speech is held; the sound is not speech");

        queue.resume();
        queue.play_chime();
        let mut out = [0.0; 2];
        queue.fill(&mut out);
        assert_eq!(out, [1.0, 1.0], "0.9 and 0.5 are held at full scale");
    }

    #[test]
    fn the_listening_sound_over_the_working_sound_is_both() {
        let chime = vec![0.25, -0.25, 0.125];
        let mut with = PlaybackQueue::with_working_sound(Bed::new(1_000)).with_chime(chime.clone());
        let mut without = PlaybackQueue::with_working_sound(Bed::new(1_000));
        for queue in [&mut with, &mut without] {
            queue.set_working(true);
            // Past its pause and into the pulse.
            queue.fill(&mut [0.0; 600]);
        }
        with.play_chime();
        let (mut sounded, mut alone) = ([0.0; 5], [0.0; 5]);
        with.fill(&mut sounded);
        without.fill(&mut alone);
        assert!(alone.iter().any(|&s| s != 0.0), "the bed is in the pulse");
        for (i, tone) in chime.iter().enumerate() {
            assert!((sounded[i] - (alone[i] + tone)).abs() < 1e-6, "sample {i}");
        }
        assert_eq!(sounded[3..], alone[3..], "and then it is the bed alone");
    }

    #[test]
    fn asking_again_while_it_sounds_does_not_restart_it() {
        let mut queue = PlaybackQueue::new().with_chime(vec![0.1, 0.2, 0.3, 0.4]);
        queue.play_chime();
        let mut out = [0.0; 2];
        queue.fill(&mut out);
        queue.play_chime();
        queue.fill(&mut out);
        assert_eq!(out, [0.3, 0.4], "it carried on from where it was");
        // Once it has gone, it can be asked for again.
        queue.play_chime();
        queue.fill(&mut out);
        assert_eq!(out, [0.1, 0.2]);
    }

    #[test]
    fn clearing_stops_the_listening_sound() {
        let mut queue = PlaybackQueue::new().with_chime(vec![0.25; 4]);
        queue.play_chime();
        queue.fill(&mut [0.0; 2]);
        queue.clear();
        let mut out = [9.0; 4];
        queue.fill(&mut out);
        assert_eq!(out, [0.0; 4]);
        // And it is there to be asked for again after.
        queue.play_chime();
        let mut out = [0.0; 4];
        queue.fill(&mut out);
        assert_eq!(out, [0.25; 4]);
    }

    #[test]
    fn a_queue_with_no_listening_sound_plays_nothing_for_it() {
        let mut queue = PlaybackQueue::new();
        queue.play_chime();
        let mut out = [9.0; 4];
        queue.fill(&mut out);
        assert_eq!(out, [0.0; 4]);
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
