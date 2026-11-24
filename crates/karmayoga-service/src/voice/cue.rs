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

/// The listening sound's two notes, in the order they play: E5 and then A5, a
/// fourth up, which is the interval of a question being opened for an answer
/// rather than of a result arriving.
const CHIME_NOTES: [f32; 2] = [659.25, 880.0];

/// How long each note swells and fades. Short enough that the whole sound is
/// over before the person has finished taking a breath to speak.
const CHIME_NOTE_SECONDS: f32 = 0.09;

/// The silence between the two notes, so they are heard as two.
const CHIME_GAP_SECONDS: f32 = 0.03;

/// The silence after the second note, where its ringing would die away. It
/// gives the sound a quiet end, so that it is over -- for the ear, and for the
/// echo canceller's reference alike -- a beat before the person is likely to
/// speak.
const CHIME_TAIL_SECONDS: f32 = 0.04;

/// How much of each note's octave above is added to it, against 1.0 for the
/// note: enough to make a bell of a plain tone, not enough to be bright.
const CHIME_OVERTONE: f32 = 0.25;

/// The loudest the listening sound gets, as a fraction of full scale (about
/// -15 dBFS): a little over the working sound's, since this is the one sound
/// that is meant to be noticed, and still well under a voice.
const CHIME_PEAK: f32 = 0.18;

/// The sound that says "listening": played once when a conversation the wake
/// word began with the name alone has opened, so the person knows to speak
/// (see [`super::VoiceInput::Chime`]). At `sample_rate`, mono, in
/// `-1.0..=1.0`.
///
/// Two soft rising notes, about a quarter of a second in all, each a swell
/// and fade of a raised cosine so that neither has an edge to click on, with
/// silence at both ends so the whole starts and ends at zero. Like the working
/// sound it is made once, where allocating is allowed, and only read after.
pub fn listening_chime(sample_rate: u32) -> Vec<f32> {
    let rate = sample_rate.max(1) as f32;
    let note = ((CHIME_NOTE_SECONDS * rate) as usize).max(1);
    let gap = (CHIME_GAP_SECONDS * rate) as usize;
    let tail = (CHIME_TAIL_SECONDS * rate) as usize;
    let weight = 1.0 + CHIME_OVERTONE;
    let mut samples = Vec::with_capacity(CHIME_NOTES.len() * (note + gap) + tail);
    for (index, hz) in CHIME_NOTES.iter().enumerate() {
        samples.extend((0..note).map(|i| {
            let phase = i as f32 / note as f32;
            let envelope = 0.5 - 0.5 * (std::f32::consts::TAU * phase).cos();
            let t = i as f32 / rate;
            let tone = ((std::f32::consts::TAU * hz * t).sin()
                + CHIME_OVERTONE * (std::f32::consts::TAU * 2.0 * hz * t).sin())
                / weight;
            CHIME_PEAK * envelope * tone
        }));
        if index + 1 < CHIME_NOTES.len() {
            samples.resize(samples.len() + gap, 0.0);
        }
    }
    samples.resize(samples.len() + tail, 0.0);
    samples
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

    /// How many times `samples` crosses zero: twice a cycle of a note, which
    /// is how a note's pitch is read without a transform.
    fn crossings(samples: &[f32]) -> usize {
        samples.windows(2).filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0)).count()
    }

    #[test]
    fn the_listening_chime_is_about_a_quarter_of_a_second_at_any_rate() {
        for rate in [8_000, 16_000, 22_050, 24_000, 44_100, 48_000] {
            let chime = listening_chime(rate);
            let seconds = chime.len() as f32 / rate as f32;
            assert!((seconds - 0.25).abs() < 0.002, "{rate} Hz: {seconds} s");
        }
        assert!(listening_chime(0).len() <= 4, "no rate is next to no sound, not a crash");
    }

    #[test]
    fn the_listening_chime_is_soft_but_not_silent() {
        for rate in [16_000, 48_000] {
            let loudest = peak(&listening_chime(rate));
            assert!(loudest > 0.1 && loudest <= CHIME_PEAK + 1e-6, "{rate} Hz: {loudest}");
        }
    }

    #[test]
    fn the_listening_chime_starts_and_ends_in_silence() {
        for rate in [16_000, 44_100, 48_000] {
            let chime = listening_chime(rate);
            assert_eq!(chime[0], 0.0, "{rate} Hz");
            assert_eq!(*chime.last().unwrap(), 0.0, "{rate} Hz");
            let tail = (CHIME_TAIL_SECONDS * rate as f32) as usize;
            assert!(chime[chime.len() - tail..].iter().all(|&s| s == 0.0), "{rate} Hz");
        }
    }

    #[test]
    fn the_listening_chime_has_nothing_to_click_on() {
        // No step from one sample to the next bigger than a full-strength tone
        // an octave above the higher note could take.
        let rate = 48_000;
        let steepest = CHIME_PEAK * std::f32::consts::TAU * 2.0 * CHIME_NOTES[1] / rate as f32;
        let chime = listening_chime(rate);
        let widest = chime.windows(2).map(|pair| (pair[1] - pair[0]).abs()).fold(0.0, f32::max);
        assert!(widest <= steepest, "a step of {widest}, a tone's steepest is {steepest}");
    }

    #[test]
    fn the_listening_chime_is_two_notes_rising_with_a_gap_between() {
        let rate = 48_000;
        let chime = listening_chime(rate);
        let note = (CHIME_NOTE_SECONDS * rate as f32) as usize;
        let gap = (CHIME_GAP_SECONDS * rate as f32) as usize;
        let (first, between, second) =
            (&chime[..note], &chime[note..note + gap], &chime[note + gap..note + gap + note]);
        assert!(between.iter().all(|&s| s == 0.0), "two notes, not one");
        assert!(peak(first) > 0.1 && peak(second) > 0.1);
        assert!(crossings(second) > crossings(first), "the second note is the higher");
        // Roughly the pitches they were written at: two crossings a cycle.
        let hz = |samples: &[f32]| crossings(samples) as f32 / 2.0 / CHIME_NOTE_SECONDS;
        assert!((hz(first) - CHIME_NOTES[0]).abs() < 40.0, "{}", hz(first));
        assert!((hz(second) - CHIME_NOTES[1]).abs() < 40.0, "{}", hz(second));
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
