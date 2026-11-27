//! Whether a stretch of audio is somebody's voice: the pitch every vowel
//! has, which a keyboard, a fan or a door does not.
//!
//! Silero says whether a window sounds like speech, and a burst of typing,
//! a knock or a chair is enough to make it say yes now and then. What those
//! sounds lack is a pitch. A vowel is the vocal cords opening and closing
//! somewhere between 80 and 500 times a second, so its waveform repeats
//! itself at that period, and a window of it correlates strongly with
//! itself a period later. A key's click is one sharp transient and a short
//! ringing, a hiss has no period at all, and a hum or a rumble repeats too
//! slowly to be a voice. [`is_voiced`] asks that question of one window;
//! [`voiced_ms`] of a whole recording.
//!
//! # One window is not enough
//!
//! A key that bottoms out is two transients a few milliseconds apart, which
//! now and then lines up into something periodic for one window. A vowel
//! lasts a good deal longer than that. So a window counts as voiced sound only
//! when the window beside it is voiced too ([`Sustained`], [`MIN_RUN`]): a lone
//! voiced window is what a key does and not what a person does.
//!
//! # What it keeps out, and what it does not
//!
//! Noise, clicks, rumble, a hum below 80 Hz and a tone above about 1 kHz have
//! no pitch a voice could have, or are mostly sound a voice does not make, and
//! are not voiced. A steady tone inside the voice's own range does have one:
//! the application's own listening chime is two such notes, and it is voiced
//! as far as this goes. Telling those from a person is left to Silero and to
//! the echo canceller, which know what the application has just played.
//!
//! # What it expects of the audio
//!
//! The microphone is cleaned of rumble before it gets here (every
//! application microphone has a high-pass filter and noise suppression), and
//! this does not repeat that. What a rumble would leave, a correlation that is
//! only the slow fall from zero lag, is not a peak and is not taken for a
//! pitch (see [`is_voiced`]). Silero's own window is the unit throughout, so
//! what is found here lines up with what it called speech.

use crate::speech::SAMPLE_RATE;

/// The samples [`is_voiced`] looks at at a time: Silero's own window, 32 ms
/// at [`SAMPLE_RATE`].
pub const WINDOW: usize = 512;

/// How long a window lasts, in milliseconds.
const WINDOW_MS: u64 = (WINDOW as u64 * 1000) / SAMPLE_RATE as u64;

/// Where the window's sound is cut off from above, in hertz. A voice keeps
/// its fundamental and first harmonics under 1 kHz, and a key click's
/// ringing, a hiss and the whistle of a fricative are mostly above it, so
/// what is left is the part of the sound that has a pitch to find. Set a
/// little under 1 kHz because the filter is a gentle one: at this setting a
/// 3 kHz tone is down by about 20 dB, not nothing.
const CUTOFF_HZ: f64 = 900.0;

/// The least sound, as RMS of the filtered window on the -1.0..=1.0 scale
/// (about -56 dBFS), that can be somebody at the microphone. Under it a
/// window can be as periodic as it likes -- a faint hum, the last of a tone
/// dying away -- and still be nobody. The same floor under which the meeting
/// notes treat any sound as no sound at all
/// (`jatayu_core::meeting::hallucination`).
const RMS_FLOOR: f64 = 0.0015;

/// How much of the window's sound, by energy, must be under [`CUTOFF_HZ`]: a
/// fifth. Vowels keep most of their energy there, and a vowel running into an
/// "s" ("yes") still keeps a third, while a tone or a hiss up in the treble
/// keeps almost none. Without this a loud enough 3 kHz tone would get through
/// the filter's gentle slope and be as periodic as anything. Set low, because
/// it is only there to tell a tone up in the treble from a person.
const LOW_SHARE: f64 = 0.2;

/// The highest pitch, in hertz, that is looked for: a child's, or a woman
/// singing. Fixes the shortest lag that is tried ([`MIN_LAG`]).
const HIGHEST_PITCH_HZ: u32 = 500;

/// The lowest pitch, in hertz, that is looked for: a deep bass voice. Fixes
/// the longest lag that is tried ([`MAX_LAG`]). Under it a sound repeats too
/// slowly to be a voice and is most likely a hum.
const LOWEST_PITCH_HZ: u32 = 80;

/// The shortest period tried, in samples: 32 at 16 kHz.
const MIN_LAG: usize = (SAMPLE_RATE / HIGHEST_PITCH_HZ) as usize;

/// The longest period tried, in samples: 200 at 16 kHz.
const MAX_LAG: usize = (SAMPLE_RATE / LOWEST_PITCH_HZ) as usize;

/// How alike the window and itself one period later must be for the period
/// to be a pitch: 0.6, on a scale where 1.0 is an exact repeat and 0.0 no
/// relation. White noise tops out around 0.4 and a clean vowel scores 0.9 or
/// more. Asking for more costs real words first: run behind Silero over
/// recorded one-word answers, 0.75 dropped "yes" and "enemy" outright, while
/// at 0.6 every word of them got through and seven recorded keyboards, plain
/// and 24 dB louder, still gave nothing. This is a floor under Silero, not a
/// second judge of speech: what it is for is a sound with no pitch in it at
/// all, and a word nobody hears is worse than a click that is.
const MIN_PERIODICITY: f64 = 0.6;

/// How many windows in a row must be voiced for any of them to count, see
/// the module doc. Two is 64 ms, well under the shortest vowel and over a
/// key's transient.
pub const MIN_RUN: usize = 2;

/// Whether one [`WINDOW`] of audio, scaled to -1.0..=1.0, is voiced: loud
/// enough to be somebody near the microphone, mostly sound under about
/// 900 Hz, and periodic at a pitch a human voice has. A shorter window is
/// never voiced; only the first [`WINDOW`] samples of a longer one are
/// looked at.
///
/// In order: the average is taken out of the window and what is left filtered
/// ([`CUTOFF_HZ`]); if it is under [`RMS_FLOOR`], or holds under
/// [`LOW_SHARE`] of what the window did, it is not voiced. Then the
/// window is compared with itself shifted by every lag from [`MIN_LAG`] to
/// [`MAX_LAG`], the comparison being the correlation of the overlap
/// normalised by how loud each side is, so that 1.0 is an exact repeat
/// whatever the level. Voiced if some lag is a peak -- better than the lag on
/// either side, which a slow fall from zero lag never is -- of at least
/// [`MIN_PERIODICITY`].
pub fn is_voiced(window: &[f32]) -> bool {
    let Some(window) = window.get(..WINDOW) else {
        return false;
    };
    let mean = window.iter().map(|&s| f64::from(s)).sum::<f64>() / WINDOW as f64;
    let centred: [f64; WINDOW] = std::array::from_fn(|i| f64::from(window[i]) - mean);
    let whole: f64 = centred.iter().map(|s| s * s).sum();
    if whole <= 0.0 {
        return false;
    }
    let low = low_pass(&centred);
    // Running totals of the filtered energy, so the energy of any stretch of
    // it is a subtraction.
    let mut energy = [0.0_f64; WINDOW + 1];
    for (i, s) in low.iter().enumerate() {
        energy[i + 1] = energy[i] + s * s;
    }
    let low_energy = energy[WINDOW];
    if (low_energy / WINDOW as f64).sqrt() < RMS_FLOOR || low_energy < whole * LOW_SHARE {
        return false;
    }

    // One lag either side of the range, to tell a peak from a slope.
    let correlation = |lag: usize| -> f64 {
        let overlap = WINDOW - lag;
        let product: f64 = low[..overlap].iter().zip(&low[lag..]).map(|(a, b)| a * b).sum();
        let norm = (energy[overlap] * (low_energy - energy[lag])).sqrt();
        if norm > 0.0 { product / norm } else { 0.0 }
    };
    let correlations: Vec<f64> = (MIN_LAG - 1..=MAX_LAG + 1).map(correlation).collect();
    correlations.windows(3).any(|w| w[1] >= MIN_PERIODICITY && w[1] > w[0] && w[1] >= w[2])
}

/// The window through a second-order Butterworth low-pass at [`CUTOFF_HZ`].
///
/// A fresh filter for every window, so one window's verdict never depends on
/// another's, and started as though its first sample had always been the
/// input, so a window that opens in the middle of a wave is not given a step.
fn low_pass(window: &[f64; WINDOW]) -> [f64; WINDOW] {
    // The standard biquad low-pass (Audio EQ Cookbook), Q of 1/sqrt(2) for
    // the flattest passband.
    let w = 2.0 * std::f64::consts::PI * CUTOFF_HZ / f64::from(SAMPLE_RATE);
    let alpha = w.sin() / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
    let a0 = 1.0 + alpha;
    let b0 = (1.0 - w.cos()) / 2.0 / a0;
    let b1 = (1.0 - w.cos()) / a0;
    let a1 = -2.0 * w.cos() / a0;
    let a2 = (1.0 - alpha) / a0;

    let first = window[0];
    let (mut x1, mut x2, mut y1, mut y2) = (first, first, first, first);
    std::array::from_fn(|i| {
        let x = window[i];
        let y = b0 * x + b1 * x1 + b0 * x2 - a1 * y1 - a2 * y2;
        (x2, x1, y2, y1) = (x1, x, y1, y);
        y
    })
}

/// How many windows count as voiced sound, given whether each was voiced:
/// those that belong to a run of at least [`MIN_RUN`], see the module doc.
///
/// Fed one window at a time, so that a detector holding a stream can keep
/// the count as it goes and [`voiced_ms`] can keep it over a recording with
/// the same rule.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sustained {
    /// Voiced windows in a row, ending at the last one given.
    streak: usize,
    /// Windows counted so far.
    counted: usize,
}

impl Sustained {
    /// Take the next window, and say how many have counted so far. The window
    /// that makes a run [`MIN_RUN`] long brings the whole run with it.
    pub fn push(&mut self, voiced: bool) -> usize {
        if voiced {
            self.streak += 1;
            if self.streak == MIN_RUN {
                self.counted += MIN_RUN;
            } else if self.streak > MIN_RUN {
                self.counted += 1;
            }
        } else {
            self.streak = 0;
        }
        self.counted
    }

    /// Windows counted so far.
    pub fn windows(&self) -> usize {
        self.counted
    }
}

/// How many windows of `windows` count as voiced sound, as [`Sustained`]
/// counts them, given what `voiced` says of each; it stops looking as soon as
/// `enough` have, since a caller that only wants to know whether there is
/// enough has no use for the rest.
fn sustained_windows<W>(windows: &[W], voiced: impl Fn(&W) -> bool, enough: usize) -> usize {
    let mut sustained = Sustained::default();
    for window in windows {
        if sustained.push(voiced(window)) >= enough {
            break;
        }
    }
    sustained.windows()
}

/// How much of `samples` (at [`SAMPLE_RATE`]) is voiced, in milliseconds:
/// whole [`WINDOW`]s that [`is_voiced`] says are, and that are part of a run
/// of at least [`MIN_RUN`], end to end. A part-window at the end is not
/// looked at.
pub fn voiced_ms(samples: &[i16]) -> u64 {
    let windows = samples.as_chunks::<WINDOW>().0;
    let voiced = |w: &[i16; WINDOW]| is_voiced(&crate::speech::pcm::to_f32(w));
    sustained_windows(windows, voiced, usize::MAX) as u64 * WINDOW_MS
}

/// Whether `samples` (at [`SAMPLE_RATE`]) hold at least `needed` samples of
/// voiced sound, counted whole windows at a time as [`voiced_ms`] counts, but
/// with `voiced` in place of [`is_voiced`]. Nothing needed is nothing to look
/// for: `voiced` is not called at all. Otherwise it stops at the first window
/// that makes it so, which for speech is a few windows in, not the whole of a
/// long utterance.
pub fn voiced_at_least(samples: &[f32], needed: usize, voiced: fn(&[f32]) -> bool) -> bool {
    if needed == 0 {
        return true;
    }
    let enough = needed.div_ceil(WINDOW);
    let windows = samples.as_chunks::<WINDOW>().0;
    sustained_windows(windows, |w| voiced(w), enough) >= enough
}

/// Synthetic sound for tests here and in the detector, each deterministic:
/// the same arguments always give the same samples.
#[cfg(test)]
pub(crate) mod signals {
    use super::SAMPLE_RATE;
    use std::f64::consts::PI;

    const RATE: f64 = SAMPLE_RATE as f64;

    /// A small, fast, repeatable source of numbers (xorshift64*), so no test
    /// depends on a random seed it cannot see.
    pub struct Rng(u64);

    impl Rng {
        pub fn new(seed: u64) -> Self {
            Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
        }

        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        /// From -1.0 up to 1.0.
        pub fn signed(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1u64 << 52) as f64 - 1.0
        }

        /// From `low` up to `high`.
        pub fn between(&mut self, low: f64, high: f64) -> f64 {
            low + (self.signed() + 1.0) / 2.0 * (high - low)
        }
    }

    fn samples(seconds: f64) -> usize {
        (seconds * RATE) as usize
    }

    /// `signal` scaled so that its loudest sample is `peak`.
    fn scaled(signal: Vec<f64>, peak: f64) -> Vec<f32> {
        let loudest = signal.iter().fold(0.0_f64, |m, s| m.max(s.abs()));
        let by = if loudest > 0.0 { peak / loudest } else { 0.0 };
        signal.into_iter().map(|s| (s * by) as f32).collect()
    }

    /// A voice: ten harmonics of `pitch_hz`, each 1/k as strong as the first,
    /// the pitch drifting by up to 3% three times a second as a real voice's
    /// does, a little noise on top, and, with `resonance_hz`, the whole
    /// through a resonance about 100 Hz wide there, as a vowel's first
    /// formant shapes it. `peak` is its loudest sample.
    pub fn voice(
        pitch_hz: f64,
        resonance_hz: Option<f64>,
        seconds: f64,
        peak: f64,
        seed: u64,
    ) -> Vec<f32> {
        let mut rng = Rng::new(seed);
        let mut phase = 0.0_f64;
        let pulses: Vec<f64> = (0..samples(seconds))
            .map(|i| {
                let t = i as f64 / RATE;
                phase += 2.0 * PI * pitch_hz * (1.0 + 0.03 * (2.0 * PI * 3.0 * t).sin()) / RATE;
                let harmonics: f64 =
                    (1..=10).map(|k| (f64::from(k) * phase).cos() / f64::from(k)).sum();
                harmonics + 0.05 * rng.signed()
            })
            .collect();
        let shaped = match resonance_hz {
            None => pulses,
            Some(hz) => {
                let r = (-PI * 100.0 / RATE).exp();
                let (c1, c2) = (2.0 * r * (2.0 * PI * hz / RATE).cos(), -r * r);
                let (mut y1, mut y2) = (0.0, 0.0);
                pulses
                    .into_iter()
                    .map(|x| {
                        let y = x + c1 * y1 + c2 * y2;
                        (y2, y1) = (y1, y);
                        y
                    })
                    .collect()
            }
        };
        scaled(shaped, peak)
    }

    /// Typing: a key every 80 to 250 ms or so, each a sharp broadband attack
    /// and a ringing between `ring_hz.0` and `ring_hz.1` that dies away in a
    /// few milliseconds, then a second, weaker transient 3 to 10 ms later as
    /// the key bottoms out. Each click peaks at `peak`, give or take.
    pub fn typing(seconds: f64, ring_hz: (f64, f64), peak: f64, seed: u64) -> Vec<f32> {
        let mut rng = Rng::new(seed);
        let mut out = vec![0.0_f64; samples(seconds)];
        let mut at = samples(0.05);
        while at + samples(0.03) < out.len() {
            let mut click = vec![0.0_f64; samples(0.02)];
            let mut ring = rng.between(ring_hz.0, ring_hz.1);
            let mut offset = 0;
            for strength in [1.0, 0.7] {
                for (k, slot) in click.iter_mut().enumerate().skip(offset) {
                    let t = (k - offset) as f64 / RATE;
                    let attack = rng.signed() * (-t / 0.0007).exp();
                    let ringing = (2.0 * PI * ring * t).sin() * (-t / 0.002).exp();
                    *slot += strength * (0.7 * attack + ringing);
                }
                offset += samples(rng.between(0.003, 0.010));
                ring *= rng.between(0.8, 1.25);
            }
            let loudest = click.iter().fold(0.0_f64, |m, s| m.max(s.abs()));
            let by = peak * rng.between(0.6, 1.0) / loudest;
            for (slot, s) in out[at..].iter_mut().zip(&click) {
                *slot += s * by;
            }
            at += click.len() + samples(rng.between(0.08, 0.25));
        }
        out.into_iter().map(|s| s as f32).collect()
    }

    /// White noise with RMS about `rms`.
    pub fn noise(seconds: f64, rms: f64, seed: u64) -> Vec<f32> {
        let mut rng = Rng::new(seed);
        // Uniform noise has an RMS of 1/sqrt(3) of its peak.
        (0..samples(seconds)).map(|_| (rng.signed() * rms * 3.0_f64.sqrt()) as f32).collect()
    }

    /// A pure tone of `hz` with the given peak.
    pub fn tone(hz: f64, seconds: f64, peak: f64) -> Vec<f32> {
        (0..samples(seconds))
            .map(|i| (peak * (2.0 * PI * hz * i as f64 / RATE).sin()) as f32)
            .collect()
    }

    /// `seconds` of nothing.
    pub fn silence(seconds: f64) -> Vec<f32> {
        vec![0.0; samples(seconds)]
    }
}

#[cfg(test)]
mod tests {
    use super::signals::*;
    use super::*;
    use crate::speech::pcm;

    /// The windows of `signal` that are voiced, and how many windows there are.
    fn voiced_windows(signal: &[f32]) -> (usize, usize) {
        let windows = signal.as_chunks::<WINDOW>().0;
        (windows.iter().filter(|w| is_voiced(*w)).count(), windows.len())
    }

    #[test]
    fn a_voice_is_voiced_across_the_range_of_pitches() {
        for pitch in [85.0, 110.0, 160.0, 220.0, 300.0, 400.0] {
            let (voiced, all) = voiced_windows(&voice(pitch, None, 1.0, 0.1, 1));
            assert_eq!(voiced, all, "a voice at {pitch} Hz");
        }
    }

    #[test]
    fn a_vowel_through_a_resonance_is_voiced() {
        for pitch in [110.0, 220.0] {
            let (voiced, all) = voiced_windows(&voice(pitch, Some(700.0), 1.0, 0.1, 2));
            assert_eq!(voiced, all, "a vowel at {pitch} Hz");
        }
    }

    #[test]
    fn a_quiet_voice_over_the_floor_is_still_voiced() {
        let (voiced, all) = voiced_windows(&voice(150.0, None, 1.0, 0.03, 3));
        assert_eq!(voiced, all);
    }

    #[test]
    fn a_voice_with_a_hiss_under_it_is_voiced() {
        let hiss = noise(1.0, 0.005, 4);
        let mixed: Vec<f32> =
            voice(130.0, Some(600.0), 1.0, 0.1, 5).iter().zip(&hiss).map(|(a, b)| a + b).collect();
        let (voiced, all) = voiced_windows(&mixed);
        assert_eq!(voiced, all);
    }

    #[test]
    fn white_noise_is_not_voiced() {
        for rms in [0.01, 0.05, 0.2] {
            assert_eq!(voiced_windows(&noise(2.0, rms, 6)).0, 0, "noise at {rms}");
        }
    }

    #[test]
    fn a_bright_key_click_is_not_voiced() {
        // A window here and there may look periodic -- the threshold is low,
        // to keep every real "yes" -- but never enough of them to count.
        for seed in 1..=6 {
            let typed = typing(3.0, (2_000.0, 4_000.0), 0.3, seed);
            assert!(voiced_windows(&typed).0 <= 2, "typing, seed {seed}");
            assert_eq!(voiced_ms(&pcm::to_i16(&typed)), 0, "typing, seed {seed}");
        }
    }

    #[test]
    fn typing_is_hardly_voiced_at_all() {
        // Bright clicks, the duller knock of a desk, and a thud under 300 Hz:
        // a window or two may look periodic, but not two in a row.
        for ring in [(2_000.0, 4_000.0), (300.0, 900.0), (100.0, 300.0)] {
            for seed in 1..=6 {
                let typed = pcm::to_i16(&typing(2.0, ring, 0.3, seed));
                assert_eq!(voiced_ms(&typed), 0, "ringing at {ring:?}, seed {seed}");
            }
        }
    }

    #[test]
    fn silence_is_not_voiced() {
        assert_eq!(voiced_windows(&silence(1.0)).0, 0);
    }

    #[test]
    fn a_voice_too_faint_to_be_anybody_is_not_voiced() {
        assert_eq!(voiced_windows(&voice(150.0, None, 1.0, 0.0005, 12)).0, 0);
    }

    #[test]
    fn a_tone_up_in_the_treble_is_not_voiced() {
        // Perfectly periodic, and far too loud to be written off as faint.
        assert_eq!(voiced_windows(&tone(3_000.0, 1.0, 0.1)).0, 0);
        assert_eq!(voiced_windows(&tone(3_000.0, 1.0, 0.5)).0, 0);
    }

    #[test]
    fn a_steady_tone_in_the_voices_own_range_is_voiced() {
        // Not something this can tell from a vowel, and not meant to: the
        // listening chime is two such notes (E5 and A5), and Silero and the
        // echo canceller are what know it is the application's own.
        for hz in [659.3, 880.0] {
            let (voiced, all) = voiced_windows(&tone(hz, 1.0, 0.1));
            assert_eq!(voiced, all, "a tone at {hz} Hz");
        }
    }

    #[test]
    fn a_hum_under_the_lowest_voice_is_not_voiced() {
        assert_eq!(voiced_windows(&tone(50.0, 1.0, 0.1)).0, 0);
    }

    #[test]
    fn a_short_window_is_never_voiced() {
        let voice = voice(150.0, None, 1.0, 0.1, 13);
        assert!(is_voiced(&voice[..WINDOW]));
        assert!(!is_voiced(&voice[..WINDOW - 1]));
        assert!(!is_voiced(&[]));
    }

    #[test]
    fn the_verdict_does_not_depend_on_what_came_before() {
        let voice = voice(150.0, None, 1.0, 0.1, 14);
        let alone = is_voiced(&voice[WINDOW * 3..WINDOW * 4]);
        let again = is_voiced(&voice[WINDOW * 3..WINDOW * 4]);
        assert!(alone && again);
    }

    #[test]
    fn a_lone_voiced_window_is_not_counted() {
        let mut signal = voice(150.0, None, 0.5, 0.1, 15)[..WINDOW].to_vec();
        signal.extend(silence(0.5));
        assert_eq!(voiced_ms(&pcm::to_i16(&signal)), 0);
    }

    #[test]
    fn a_run_of_voiced_windows_is_counted_whole() {
        let mut signal = voice(150.0, None, 1.0, 0.1, 16)[..WINDOW * 3].to_vec();
        signal.extend(silence(0.25));
        signal.extend(&voice(150.0, None, 1.0, 0.1, 17)[..WINDOW]);
        signal.extend(silence(0.25));
        // Three windows in a row; the lone one after the gap is not counted.
        assert_eq!(voiced_ms(&pcm::to_i16(&signal)), 96);
    }

    #[test]
    fn a_part_window_at_the_end_is_not_looked_at() {
        let signal = voice(150.0, None, 1.0, 0.1, 18);
        let whole = pcm::to_i16(&signal[..WINDOW * 4]);
        let with_part = pcm::to_i16(&signal[..WINDOW * 4 + 300]);
        assert_eq!(voiced_ms(&whole), 128);
        assert_eq!(voiced_ms(&with_part), 128);
    }

    /// How many times [`counting`] was asked.
    static ASKED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn counting(_: &[f32]) -> bool {
        ASKED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        true
    }

    fn never_asked(_: &[f32]) -> bool {
        panic!("nothing was needed, so nothing should have been looked at");
    }

    #[test]
    fn nothing_needed_is_found_without_looking() {
        assert!(voiced_at_least(&silence(1.0), 0, never_asked));
        assert!(voiced_at_least(&[], 0, never_asked));
    }

    #[test]
    fn looking_stops_at_the_window_that_makes_enough() {
        // Two windows make 1024 samples, however long the rest is.
        assert!(voiced_at_least(&silence(3.0), 2 * WINDOW, counting));
        assert_eq!(ASKED.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn enough_is_counted_as_voiced_ms_counts() {
        let mut signal = voice(150.0, None, 1.0, 0.1, 19)[..WINDOW * 3].to_vec();
        signal.extend(silence(0.25));
        signal.extend(&voice(150.0, None, 1.0, 0.1, 20)[..WINDOW]);
        signal.extend(silence(0.25));
        // Three windows in a row, and one alone that does not count.
        assert!(voiced_at_least(&signal, 3 * WINDOW, is_voiced));
        assert!(!voiced_at_least(&signal, 3 * WINDOW + 1, is_voiced));
        assert!(!voiced_at_least(&silence(1.0), WINDOW, is_voiced));
    }

    #[test]
    fn a_run_is_two_windows_or_more() {
        let mut sustained = Sustained::default();
        let counted: Vec<usize> = [true, false, true, true, true, false, true]
            .into_iter()
            .map(|voiced| sustained.push(voiced))
            .collect();
        // The first is alone; the two that follow bring the pair, then the
        // third joins it; the last is alone again.
        assert_eq!(counted, vec![0, 0, 0, 2, 3, 3, 3]);
        assert_eq!(sustained.windows(), 3);
    }
}
