//! Taking the assistant's own voice out of the microphone, and the room's
//! rumble and hiss out of it when nothing is playing.
//!
//! A thin wrapper over `sonora`, a Rust port of WebRTC's audio processing, at
//! 16 kHz mono. With a speaker ([`Cleaning::Echo`]): echo cancellation
//! against what was actually sent to the speakers, noise suppression, a
//! high-pass filter and adaptive digital gain. Without one
//! ([`Cleaning::Noise`]): the high-pass filter and noise suppression alone.
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

/// What a [`Canceller`] takes out of the microphone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cleaning {
    /// The speaker's echo, against the reference, then noise suppression, the
    /// high-pass filter and adaptive gain: voice mode.
    Echo,
    /// Rumble, mains hum and steady hiss, for a microphone with nothing
    /// playing beside it. A webcam on a monitor hears the desk: measured on
    /// one, most of its noise in silence was vibration below 40 Hz and 60 Hz
    /// hum, which the high-pass filter takes out (over 40 dB at 60 Hz, flat
    /// from 100 Hz). No gain: the level is left as the device gives it, since
    /// voice typing's meter and meeting capture's silence threshold read it.
    Noise,
}

pub struct Canceller {
    apm: AudioProcessing,
    cleaning: Cleaning,
    /// What was played, at 16 kHz mono, not yet paired with a frame of
    /// microphone.
    reference: VecDeque<f32>,
    /// Microphone samples that do not yet make a frame.
    pending: Vec<f32>,
    render_in: [f32; FRAME],
    render_out: [f32; FRAME],
    capture_out: [f32; FRAME],
}

impl Canceller {
    pub fn new(cleaning: Cleaning) -> Self {
        let config = match cleaning {
            Cleaning::Echo => Config {
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
            },
            Cleaning::Noise => Config {
                high_pass_filter: Some(HighPassFilter::default()),
                noise_suppression: Some(NoiseSuppression::default()),
                ..Default::default()
            },
        };
        let apm = AudioProcessing::builder()
            .config(config)
            .capture_config(StreamConfig::new(SAMPLE_RATE, 1))
            .render_config(StreamConfig::new(SAMPLE_RATE, 1))
            .build();
        Self {
            apm,
            cleaning,
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

    /// Add what was just played, at 16 kHz mono. Only [`Cleaning::Echo`] has
    /// a use for it.
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
    /// With [`Cleaning::Echo`], each frame's reference goes in first -- a
    /// frame of the queue if there is one, silence if there is not -- so the
    /// canceller has heard what could have caused an echo before it is asked
    /// to remove it.
    pub fn process(&mut self, microphone: &[f32], out: &mut Vec<i16>) {
        self.pending.extend_from_slice(microphone);
        let mut used = 0;
        while self.pending.len() - used >= FRAME {
            if self.cleaning == Cleaning::Echo {
                self.hear_reference_frame();
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

    /// Hand the canceller the next frame of the reference, or silence if not
    /// enough has been played yet.
    fn hear_reference_frame(&mut self) {
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
    }

    #[cfg(test)]
    pub fn cleaning(&self) -> Cleaning {
        self.cleaning
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
        let mut canceller = Canceller::new(Cleaning::Echo);
        let mut out = Vec::new();
        canceller.process(&[0.0; FRAME * 2 + 80], &mut out);
        assert_eq!(out.len(), FRAME * 2);
        canceller.process(&[0.0; 80], &mut out);
        assert_eq!(out.len(), FRAME * 3, "the 80 left over completed a frame");
    }

    #[test]
    fn a_microphone_pushed_a_sample_at_a_time_still_makes_frames() {
        let mut canceller = Canceller::new(Cleaning::Echo);
        let mut out = Vec::new();
        for _ in 0..FRAME * 3 {
            canceller.process(&[0.0], &mut out);
        }
        assert_eq!(out.len(), FRAME * 3);
    }

    #[test]
    fn an_empty_reference_does_not_stall_the_microphone() {
        let mut canceller = Canceller::new(Cleaning::Echo);
        canceller.set_delay_ms(50);
        let mut out = Vec::new();
        canceller.process(&[0.0; FRAME * 10], &mut out);
        assert_eq!(out.len(), FRAME * 10);
    }

    #[test]
    fn silence_stays_silent() {
        let mut canceller = Canceller::new(Cleaning::Echo);
        canceller.set_delay_ms(50);
        let mut out = Vec::new();
        canceller.push_reference(&[0.0; FRAME * 20]);
        canceller.process(&[0.0; FRAME * 20], &mut out);
        assert!(out.iter().all(|s| s.abs() < 50), "{:?}", &out[..8]);
    }

    #[test]
    fn the_reference_is_used_a_frame_at_a_time_as_the_microphone_arrives() {
        let mut canceller = Canceller::new(Cleaning::Echo);
        canceller.push_reference(&[0.1; FRAME * 3 + 10]);
        let mut out = Vec::new();
        canceller.process(&[0.0; FRAME], &mut out);
        assert_eq!(canceller.reference_len(), FRAME * 2 + 10);
    }

    #[test]
    fn a_reference_that_is_not_a_whole_frame_waits_rather_than_being_padded() {
        let mut canceller = Canceller::new(Cleaning::Echo);
        canceller.push_reference(&[0.1; FRAME - 1]);
        let mut out = Vec::new();
        canceller.process(&[0.0; FRAME], &mut out);
        assert_eq!(canceller.reference_len(), FRAME - 1);
    }

    #[test]
    fn the_reference_cannot_grow_without_limit() {
        let mut canceller = Canceller::new(Cleaning::Echo);
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
        let mut canceller = Canceller::new(Cleaning::Echo);
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

    /// A voiced sound: harmonics of a wavering pitch, in syllables. Not
    /// [`syllables`], whose flat spectrum noise suppression rightly calls
    /// noise.
    fn voiced(seconds: usize, rate: f32) -> Vec<f32> {
        let mut phase = 0.0f32;
        (0..seconds * SAMPLE_RATE as usize)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                let pitch = 140.0 + 15.0 * (t * 1.3 * std::f32::consts::TAU).sin();
                phase += pitch / SAMPLE_RATE as f32 * std::f32::consts::TAU;
                let tone: f32 = (1..=20).map(|k| (phase * k as f32).sin() / k as f32).sum();
                let envelope = (t * rate * std::f32::consts::TAU).sin().max(0.0);
                tone * envelope * 0.15
            })
            .collect()
    }

    fn tone(seconds: usize, hz: f32, level: f32) -> Vec<f32> {
        (0..seconds * SAMPLE_RATE as usize)
            .map(|i| (i as f32 / SAMPLE_RATE as f32 * hz * std::f32::consts::TAU).sin() * level)
            .collect()
    }

    /// Everything after the first second, once the filters have settled.
    fn settled(canceller: &mut Canceller, microphone: &[f32]) -> Vec<f32> {
        let mut out = Vec::new();
        canceller.process(microphone, &mut out);
        out[SAMPLE_RATE as usize..].iter().map(|s| f32::from(*s) / 32768.0).collect()
    }

    #[test]
    fn with_nothing_playing_audio_still_comes_out_in_whole_frames() {
        let mut canceller = Canceller::new(Cleaning::Noise);
        let mut out = Vec::new();
        canceller.process(&[0.0; FRAME * 2 + 80], &mut out);
        assert_eq!(out.len(), FRAME * 2);
        canceller.process(&[0.0; 80], &mut out);
        assert_eq!(out.len(), FRAME * 3);
    }

    /// What a webcam on a monitor hears of the desk: vibration far below
    /// speech, and mains hum.
    #[test]
    fn with_nothing_playing_rumble_and_hum_are_taken_out() {
        let desk: Vec<f32> = tone(4, 12.0, 0.2)
            .iter()
            .zip(tone(4, 30.0, 0.1))
            .zip(tone(4, 60.0, 0.05))
            .map(|((a, b), c)| a + b + c)
            .collect();
        let left = rms(&settled(&mut Canceller::new(Cleaning::Noise), &desk));
        let before = rms(&desk[SAMPLE_RATE as usize..]);
        println!("rumble {before:.4}, left of it {left:.5}");
        assert!(left < before / 100.0, "at least 40 dB of it goes");
    }

    #[test]
    fn with_nothing_playing_a_voice_keeps_its_level() {
        let voice = voiced(5, 2.7);
        let after = rms(&settled(&mut Canceller::new(Cleaning::Noise), &voice));
        let before = rms(&voice[SAMPLE_RATE as usize..]);
        println!("voice {before:.4}, after {after:.4}");
        assert!(after > before * 0.7, "no more than 3 dB lost");
        assert!(after < before * 1.2, "and no gain added");
    }

    #[test]
    fn with_nothing_playing_steady_hiss_is_turned_down() {
        let mut state = 3u32;
        let hiss: Vec<f32> = (0..4 * SAMPLE_RATE as usize)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((state >> 8) as f32 / (1 << 23) as f32 - 1.0) * 0.02
            })
            .collect();
        let out = settled(&mut Canceller::new(Cleaning::Noise), &hiss);
        // And a second more, for the suppressor to learn what the noise is.
        let left = rms(&out[SAMPLE_RATE as usize..]);
        let before = rms(&hiss);
        println!("hiss {before:.4}, left of it {left:.4}");
        assert!(left < before / 2.0, "at least 6 dB of it goes");
    }
}
