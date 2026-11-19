//! Taking the assistant's own voice out of the microphone.
//!
//! A thin wrapper over `sonora`, a Rust port of WebRTC's audio processing, at
//! 16 kHz mono: echo cancellation against what was actually sent to the
//! speakers, noise suppression, a high-pass filter and adaptive digital gain.
//! The wrapper owns the two things the library leaves to its caller: cutting
//! the microphone into exact 10 ms frames, and pairing every one of them with
//! a frame of the reference.
//!
//! The reference is a bounded queue. Output and input callbacks do not tick in
//! step, so it fills a little ahead of the microphone or a little behind it;
//! what it must never do is stall the microphone (an empty queue feeds
//! silence, so capture goes on) or grow without limit (the oldest of it is
//! dropped past [`MAX_REFERENCE`]).

use std::collections::VecDeque;

use karmayoga_service::speech::SAMPLE_RATE;
use sonora::config::{
    AdaptiveDigital, EchoCanceller, GainController2, HighPassFilter, NoiseSuppression,
};
use sonora::{AudioProcessing, Config, StreamConfig};

use super::input::f32_to_i16;

/// One 10 ms frame, the only size `sonora` works in.
pub const FRAME: usize = (SAMPLE_RATE / 100) as usize;

/// The most reference kept waiting for the microphone to catch up: 200 ms.
/// Anything more is a backlog the canceller could not use, since it looks for
/// an echo within a few hundred milliseconds of what was played.
pub const MAX_REFERENCE: usize = (SAMPLE_RATE / 5) as usize;

pub struct Canceller {
    apm: AudioProcessing,
    /// What was played, at 16 kHz mono, not yet paired with a frame of
    /// microphone.
    reference: VecDeque<f32>,
    /// Microphone samples that do not yet make a frame.
    pending: Vec<f32>,
    render_in: [f32; FRAME],
    render_out: [f32; FRAME],
    capture_out: [f32; FRAME],
}

impl Default for Canceller {
    fn default() -> Self {
        Self::new()
    }
}

impl Canceller {
    pub fn new() -> Self {
        let config = Config {
            high_pass_filter: Some(HighPassFilter::default()),
            echo_canceller: Some(EchoCanceller::default()),
            noise_suppression: Some(NoiseSuppression::default()),
            gain_controller2: Some(GainController2 {
                // Off by default in the library, which is the part of this
                // controller that does anything.
                adaptive_digital: Some(AdaptiveDigital::default()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let apm = AudioProcessing::builder()
            .config(config)
            .capture_config(StreamConfig::new(SAMPLE_RATE, 1))
            .render_config(StreamConfig::new(SAMPLE_RATE, 1))
            .build();
        Self {
            apm,
            reference: VecDeque::with_capacity(MAX_REFERENCE + FRAME),
            pending: Vec::with_capacity(FRAME * 4),
            render_in: [0.0; FRAME],
            render_out: [0.0; FRAME],
            capture_out: [0.0; FRAME],
        }
    }

    /// Tell the canceller roughly how long a sound takes to get from being
    /// handed over as reference to arriving in the microphone: the output
    /// device's latency, the input's, and whatever resampling added. It uses
    /// this as a starting point and finds the remainder for itself.
    pub fn set_delay_ms(&mut self, delay_ms: i32) {
        // Clamped to the range it accepts, which it reports as an error that
        // has already been dealt with.
        let _ = self.apm.set_stream_delay_ms(delay_ms);
    }

    /// Add what was just played, at 16 kHz mono.
    pub fn push_reference(&mut self, samples: &[f32]) {
        self.reference.extend(samples.iter().copied());
        let excess = self.reference.len().saturating_sub(MAX_REFERENCE);
        if excess > 0 {
            self.reference.drain(..excess);
        }
    }

    /// Add microphone audio, 16 kHz mono; every whole frame of it is cleaned
    /// and appended to `out`.
    ///
    /// Each frame's reference goes in first -- a frame of the queue if there is
    /// one, silence if there is not -- so the canceller has heard what could
    /// have caused an echo before it is asked to remove it.
    pub fn process(&mut self, microphone: &[f32], out: &mut Vec<i16>) {
        self.pending.extend_from_slice(microphone);
        let mut used = 0;
        while self.pending.len() - used >= FRAME {
            if self.reference.len() >= FRAME {
                for (slot, sample) in self.render_in.iter_mut().zip(self.reference.drain(..FRAME)) {
                    *slot = sample;
                }
            } else {
                self.render_in.fill(0.0);
            }
            if let Err(e) =
                self.apm.process_render_f32(&[&self.render_in[..]], &mut [&mut self.render_out[..]])
            {
                tracing::debug!(error = %e, "audio: echo canceller refused a reference frame");
            }

            let frame = &self.pending[used..used + FRAME];
            match self.apm.process_capture_f32(&[frame], &mut [&mut self.capture_out[..]]) {
                Ok(()) => out.extend(self.capture_out.iter().map(|&s| f32_to_i16(s))),
                Err(e) => {
                    // Not expected with a format fixed at build time. The
                    // person should still be heard.
                    tracing::debug!(error = %e, "audio: echo canceller refused a microphone frame");
                    out.extend(frame.iter().map(|&s| f32_to_i16(s)));
                }
            }
            used += FRAME;
        }
        self.pending.drain(..used);
    }

    #[cfg(test)]
    fn reference_len(&self) -> usize {
        self.reference.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microphone_audio_comes_out_in_whole_frames_with_the_rest_kept() {
        let mut canceller = Canceller::new();
        let mut out = Vec::new();
        canceller.process(&[0.0; FRAME * 2 + 80], &mut out);
        assert_eq!(out.len(), FRAME * 2);
        canceller.process(&[0.0; 80], &mut out);
        assert_eq!(out.len(), FRAME * 3, "the 80 left over completed a frame");
    }

    #[test]
    fn a_microphone_pushed_a_sample_at_a_time_still_makes_frames() {
        let mut canceller = Canceller::new();
        let mut out = Vec::new();
        for _ in 0..FRAME * 3 {
            canceller.process(&[0.0], &mut out);
        }
        assert_eq!(out.len(), FRAME * 3);
    }

    #[test]
    fn an_empty_reference_does_not_stall_the_microphone() {
        let mut canceller = Canceller::new();
        canceller.set_delay_ms(50);
        let mut out = Vec::new();
        canceller.process(&[0.0; FRAME * 10], &mut out);
        assert_eq!(out.len(), FRAME * 10);
    }

    #[test]
    fn silence_stays_silent() {
        let mut canceller = Canceller::new();
        canceller.set_delay_ms(50);
        let mut out = Vec::new();
        canceller.push_reference(&[0.0; FRAME * 20]);
        canceller.process(&[0.0; FRAME * 20], &mut out);
        assert!(out.iter().all(|s| s.abs() < 50), "{:?}", &out[..8]);
    }

    #[test]
    fn the_reference_is_used_a_frame_at_a_time_as_the_microphone_arrives() {
        let mut canceller = Canceller::new();
        canceller.push_reference(&[0.1; FRAME * 3 + 10]);
        let mut out = Vec::new();
        canceller.process(&[0.0; FRAME], &mut out);
        assert_eq!(canceller.reference_len(), FRAME * 2 + 10);
    }

    #[test]
    fn a_reference_that_is_not_a_whole_frame_waits_rather_than_being_padded() {
        let mut canceller = Canceller::new();
        canceller.push_reference(&[0.1; FRAME - 1]);
        let mut out = Vec::new();
        canceller.process(&[0.0; FRAME], &mut out);
        assert_eq!(canceller.reference_len(), FRAME - 1);
    }

    #[test]
    fn the_reference_cannot_grow_without_limit() {
        let mut canceller = Canceller::new();
        for _ in 0..100 {
            canceller.push_reference(&[0.1; SAMPLE_RATE as usize]);
        }
        assert_eq!(canceller.reference_len(), MAX_REFERENCE);
    }

    /// Noise shaped like syllables: what a canceller has to follow in speech.
    fn syllables(seconds: usize, seed: u32, rate: f32) -> Vec<f32> {
        let mut state = seed;
        (0..seconds * SAMPLE_RATE as usize)
            .map(|i| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (state >> 8) as f32 / (1 << 23) as f32 - 1.0;
                let t = i as f32 / SAMPLE_RATE as f32;
                let envelope = (t * rate * std::f32::consts::TAU).sin().max(0.0);
                noise * envelope * 0.3
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
    }

    /// The point of it all: the assistant's voice goes out of the speakers,
    /// comes back quieter and late, and is taken out again, while somebody
    /// talking over it still gets through.
    #[test]
    fn what_was_played_is_taken_out_and_a_voice_over_it_is_not() {
        const DELAY: usize = 640; // 40 ms
        let played = syllables(12, 7, 3.0);
        let person = syllables(12, 99, 2.3);
        let mut canceller = Canceller::new();
        canceller.set_delay_ms(40);
        let (mut alone, mut together) = (Vec::new(), Vec::new());
        for (n, frame) in played.as_chunks::<FRAME>().0.iter().enumerate() {
            canceller.push_reference(frame);
            let start = n * FRAME;
            // Eight seconds of echo alone, then somebody talks too.
            let talking = start >= 8 * SAMPLE_RATE as usize;
            let microphone: Vec<f32> = (start..start + FRAME)
                .map(|i| {
                    let echo = i.checked_sub(DELAY).map_or(0.0, |at| played[at] * 0.4);
                    echo + if talking { person[i] * 0.5 } else { 0.0 }
                })
                .collect();
            let mut out = Vec::new();
            canceller.process(&microphone, &mut out);
            let out = out.iter().map(|s| f32::from(*s) / 32768.0);
            if (5 * SAMPLE_RATE as usize..8 * SAMPLE_RATE as usize).contains(&start) {
                alone.extend(out);
            } else if start >= 9 * SAMPLE_RATE as usize {
                together.extend(out);
            }
        }
        let echo_level = rms(&played) * 0.4;
        let left = rms(&alone);
        let voice = rms(&together);
        println!("echo {echo_level:.4}, left of it {left:.4}, with a voice {voice:.4}");
        assert!(left < echo_level / 4.0, "at least 12 dB of the echo goes");
        assert!(voice > left * 3.0, "somebody talking is still heard");
    }
}
