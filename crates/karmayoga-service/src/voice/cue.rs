//! The sound of the assistant working.
//!
//! Between somebody finishing what they said and the first words of the
//! answer there can be seconds -- the words being understood, the model
//! thinking, a tool running -- and silence on a call or at the desk reads as
//! nothing happening. So while a session is working and has nothing to say it
//! asks its transport for this ([`VoiceOutput::Working`](super::VoiceOutput)):
//! a soft, low pulse, repeated, that stops the moment there is speech or the
//! person talks.
//!
//! [`Bed`] is that sound as a transport plays it: looped under nothing, never
//! over speech, faded out rather than cut. Pure data, filled into whatever
//! buffer the transport is writing, so the desktop's speaker callback and a
//! call's 20 ms frames use the same one. It never allocates once built.

/// How long one round of the sound is: a pause, one pulse, and the rest.
const PERIOD_SECONDS: f32 = 1.6;

/// Silence before the pulse. Most of a short wait -- a quick answer, a
/// sentence understood at once -- passes inside it, so it is heard only when
/// there is really something to wait for.
const LEAD_SECONDS: f32 = 0.45;

/// How long the pulse swells and fades.
const PULSE_SECONDS: f32 = 0.7;

/// The pulse's two tones: a low note and its fifth, quietly. Soft enough to
/// sit under a room, low enough not to sound like a notification.
const TONES: [(f32, f32); 2] = [(392.0, 1.0), (587.33, 0.35)];

/// The loudest the pulse gets, as a fraction of full scale (about -20 dBFS):
/// well under the voice that follows it.
const PEAK: f32 = 0.1;

/// How long the sound takes to fade out when it is no longer wanted, so it
/// never stops with a click.
const FADE_SECONDS: f32 = 0.03;

/// One round of the sound at `sample_rate`, mono, in `-1.0..=1.0`.
pub fn working_loop(sample_rate: u32) -> Vec<f32> {
    let rate = sample_rate.max(1) as f32;
    let len = (PERIOD_SECONDS * rate) as usize;
    let lead = (LEAD_SECONDS * rate) as usize;
    let pulse = ((PULSE_SECONDS * rate) as usize).max(1);
    let total_weight: f32 = TONES.iter().map(|(_, weight)| weight).sum();
    (0..len)
        .map(|i| {
            let Some(at) = i.checked_sub(lead).filter(|at| *at < pulse) else { return 0.0 };
            // A raised cosine: no edge to click on at either end, and a
            // swell rather than a beep.
            let phase = at as f32 / pulse as f32;
            let envelope = 0.5 - 0.5 * (std::f32::consts::TAU * phase).cos();
            let t = i as f32 / rate;
            let tone: f32 = TONES
                .iter()
                .map(|(hz, weight)| weight * (std::f32::consts::TAU * hz * t).sin())
                .sum::<f32>()
                / total_weight;
            PEAK * envelope * tone
        })
        .collect()
}

/// The working sound as a transport plays it. See the module doc.
pub struct Bed {
    samples: Vec<f32>,
    position: usize,
    on: bool,
    /// Samples left of a fade-out under way.
    fading: usize,
    fade: usize,
}

impl Bed {
    /// The sound, ready to play at `sample_rate`, and off.
    pub fn new(sample_rate: u32) -> Self {
        let fade = ((FADE_SECONDS * sample_rate.max(1) as f32) as usize).max(1);
        Self { samples: working_loop(sample_rate), position: 0, on: false, fading: 0, fade }
    }

    /// Play it, or stop. Starting begins a round from its pause, so a short
    /// wait is quiet; stopping fades out whatever is sounding.
    pub fn set(&mut self, on: bool) {
        if on == self.on {
            return;
        }
        self.on = on;
        if on {
            self.position = 0;
            self.fading = 0;
        } else {
            self.fading = self.fade;
        }
    }

    /// Write into `out`, the part of a buffer with no speech in it: the
    /// sound when the transport is `idle` -- nothing queued, nothing held --
    /// and silence when it is not, which also stops any fade at once (speech
    /// is starting, and louder than a cut) and starts the next round from its
    /// pause. The one rule every transport plays it by.
    pub fn fill_idle(&mut self, out: &mut [f32], idle: bool) {
        if idle {
            self.fill(out);
        } else {
            self.fading = 0;
            self.position = 0;
            out.fill(0.0);
        }
    }

    /// Write the sound into `out`: on, the loop; fading, the rest of the
    /// fade; otherwise silence.
    fn fill(&mut self, out: &mut [f32]) {
        if self.samples.is_empty() || (!self.on && self.fading == 0) {
            out.fill(0.0);
            return;
        }
        for sample in out.iter_mut() {
            let gain = if self.on {
                1.0
            } else if self.fading > 0 {
                self.fading -= 1;
                self.fading as f32 / self.fade as f32
            } else {
                0.0
            };
            *sample = self.samples[self.position] * gain;
            self.position = (self.position + 1) % self.samples.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |most, s| most.max(s.abs()))
    }

    #[test]
    fn one_round_is_a_quiet_pause_then_a_soft_pulse() {
        let round = working_loop(RATE);
        assert_eq!(round.len(), (PERIOD_SECONDS * RATE as f32) as usize);
        let lead = (LEAD_SECONDS * RATE as f32) as usize;
        assert!(round[..lead].iter().all(|&s| s == 0.0), "a short wait hears nothing");
        let loudest = peak(&round);
        assert!(loudest > 0.05 && loudest <= PEAK + 1e-6, "{loudest}");
        assert_eq!(*round.last().unwrap(), 0.0, "it loops without a step");
    }

    #[test]
    fn off_it_is_silence() {
        let mut bed = Bed::new(RATE);
        let mut out = [1.0f32; 480];
        bed.fill(&mut out);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn on_it_loops_the_round_from_its_pause() {
        let mut bed = Bed::new(RATE);
        bed.set(true);
        let round = working_loop(RATE);
        let mut out = vec![0.0f32; round.len() * 2];
        bed.fill(&mut out);
        assert_eq!(&out[..round.len()], &round[..]);
        assert_eq!(&out[round.len()..], &round[..], "and round again");
    }

    #[test]
    fn stopping_fades_out_and_then_is_silent() {
        let mut bed = Bed::new(RATE);
        bed.set(true);
        // Into the pulse, where there is something to fade.
        let lead = (LEAD_SECONDS * RATE as f32) as usize;
        let pulse = (PULSE_SECONDS * RATE as f32) as usize;
        bed.fill(&mut vec![0.0f32; lead + pulse / 2]);
        bed.set(false);
        let fade = (FADE_SECONDS * RATE as f32) as usize;
        let mut out = vec![0.0f32; fade + 480];
        bed.fill(&mut out);
        assert!(peak(&out[..fade]) > 0.0, "it fades rather than cuts");
        assert!(peak(&out[fade / 2..fade]) < peak(&out[..fade / 2]) + 1e-6);
        assert!(out[fade..].iter().all(|&s| s == 0.0), "and then it is gone");
    }

    #[test]
    fn starting_again_begins_with_the_pause() {
        let mut bed = Bed::new(RATE);
        bed.set(true);
        bed.fill(&mut vec![0.0f32; RATE as usize]);
        bed.set(false);
        bed.set(true);
        let mut out = vec![1.0f32; 480];
        bed.fill(&mut out);
        assert!(out.iter().all(|&s| s == 0.0), "back to the start of the round");
    }

    #[test]
    fn speech_cuts_a_fade_short_and_the_next_round_starts_from_its_pause() {
        let mut bed = Bed::new(RATE);
        bed.set(true);
        bed.fill_idle(&mut vec![0.0f32; RATE as usize], true);
        bed.set(false);
        let mut out = vec![1.0f32; 480];
        bed.fill_idle(&mut out, false);
        assert!(out.iter().all(|&s| s == 0.0), "silence while something else plays");
        bed.fill_idle(&mut out, true);
        assert!(out.iter().all(|&s| s == 0.0), "the fade did not come back");
        bed.set(true);
        bed.fill_idle(&mut vec![0.0f32; RATE as usize / 2], false);
        bed.fill_idle(&mut out, true);
        assert!(out.iter().all(|&s| s == 0.0), "from the round's pause");
    }
}
