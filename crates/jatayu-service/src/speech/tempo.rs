//! Speech made faster or slower without changing its pitch.
//!
//! The built-in voice has one pace of its own, and playing its samples back
//! faster would raise the pitch with it, so the speed setting has to change the
//! length of the audio and leave the voice alone. [`stretch`] does it with
//! WSOLA (waveform-similarity overlap-add): the audio is cut into short
//! overlapping frames, the frames are laid down again closer together or
//! further apart, and each is nudged a little so that its waveform carries on
//! from the one before it. That keeps the pitch, needs no FFT and no
//! dependency, and runs in a few milliseconds for a spoken answer.

use std::f64::consts::PI;

/// The slowest and fastest speeds that are honoured. Beyond them WSOLA starts
/// to stutter or smear speech, so the samples are handed back as they are.
const MIN_SPEED: f32 = 0.5;
const MAX_SPEED: f32 = 2.0;

/// How long a frame is, in milliseconds: a few pitch periods of a low voice,
/// and short enough that a stretched syllable does not sound repeated.
const FRAME_MS: u64 = 20;

/// The smallest frame ever used, in samples, so that a silly sample rate (zero
/// included) still gives a frame that can be windowed and overlapped.
const MIN_FRAME: usize = 32;

/// How far apart the offsets tried in the first, coarse pass of the search
/// are, in samples. A pass over every offset costs four times as much for the
/// same answer on speech, which is smooth at this scale.
const COARSE_STEP: usize = 4;

/// `samples`, mono at `sample_rate`, played `speed` times as fast: about
/// `samples.len() / speed` samples long, at the same pitch. A `speed` of 1
/// (or one outside 0.5..=2, or not finite) hands the samples back unchanged.
///
/// Input shorter than two frames (about 40 ms) is handed back unchanged too:
/// it is too short to be heard as faster or slower, and has too little in it
/// for the overlap to have anything to join.
pub fn stretch(samples: &[f32], sample_rate: u32, speed: f32) -> Vec<f32> {
    if !speed.is_finite() || !(MIN_SPEED..=MAX_SPEED).contains(&speed) || speed == 1.0 {
        return samples.to_vec();
    }
    let frame = frame_len(sample_rate);
    if samples.len() < 2 * frame {
        return samples.to_vec();
    }

    // Frames overlap by half, so the Hann window below sums to exactly one
    // wherever two frames meet. The search reaches as far as one hop, which is
    // 10 ms.
    let hop = frame / 2;
    let tolerance = hop;
    let window = hann(frame);

    let out_len = ((samples.len() as f64 / f64::from(speed)).round() as usize).max(1);
    // A frame that starts before `out_len` may end after it, so there is room
    // for a whole one past the end; it is cut off at the finish.
    let mut out = vec![0.0f32; out_len + frame];
    let mut weight = vec![0.0f32; out_len + frame];

    let mut previous: Option<usize> = None;
    let mut at = 0usize;
    while at < out_len {
        // Where this frame would be taken from if the audio were simply
        // sped up, and then where it is actually taken from, once moved to
        // where it best continues the frame before.
        let nominal = (at as f64 * f64::from(speed)).round() as usize;
        let start = match previous {
            None => 0,
            Some(prev) => best_start(samples, prev, nominal, frame, tolerance),
        };
        previous = Some(start);

        let segment = &samples[start..start + frame];
        for (i, (&sample, &w)) in segment.iter().zip(&window).enumerate() {
            out[at + i] += sample * w;
            weight[at + i] += w;
        }
        at += hop;
    }

    out.truncate(out_len);
    // Dividing by what the windows added up to keeps the level the same at the
    // two ends, where only one frame covers a sample. The very first sample
    // has a window of nothing, so there is nothing to divide.
    for (sample, &w) in out.iter_mut().zip(&weight) {
        if w > 1e-6 {
            *sample /= w;
        } else {
            *sample = 0.0;
        }
    }
    out
}

/// A frame's length in samples: [`FRAME_MS`] at `sample_rate`, never below
/// [`MIN_FRAME`], and even so that half of it is a whole number of samples.
fn frame_len(sample_rate: u32) -> usize {
    let n = (u64::from(sample_rate) * FRAME_MS / 1000) as usize;
    let n = n.max(MIN_FRAME);
    n - n % 2
}

/// A periodic Hann window `n` long (`n` even). Two of them half a window apart
/// add up to one everywhere, which is what makes the overlap-add keep the level.
fn hann(n: usize) -> Vec<f32> {
    (0..n).map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos()) as f32).collect()
}

/// Where in `samples` to take the next frame from: within `tolerance` of
/// `nominal`, the start whose first half looks most like the second half of the
/// frame taken at `previous`, which is what the audio did next.
///
/// Every start it returns leaves room for a whole frame, so the caller can
/// slice from it without checking. It looks at every [`COARSE_STEP`]th start
/// first and then at each start around the best of those, so a search costs a
/// few dozen comparisons rather than hundreds. Ties go to the start nearest the
/// nominal one, so silence is taken at the pace asked for.
fn best_start(
    samples: &[f32],
    previous: usize,
    nominal: usize,
    frame: usize,
    tolerance: usize,
) -> usize {
    let hop = frame / 2;
    // The caller has checked that a frame fits, and every start handed back
    // here is at most `last`, so `previous + frame` is within the samples.
    let last = samples.len() - frame;
    let nominal = nominal.min(last);
    let lo = nominal.saturating_sub(tolerance);
    let hi = nominal.saturating_add(tolerance).min(last);

    let target = &samples[previous + hop..previous + frame];
    let score = |start: usize| similarity(target, &samples[start..start + hop]);

    let mut best = nominal;
    let mut best_score = score(nominal);

    let mut start = lo;
    while start <= hi {
        let s = score(start);
        if s > best_score {
            best = start;
            best_score = s;
        }
        start += COARSE_STEP;
    }

    let from = best.saturating_sub(COARSE_STEP - 1).max(lo);
    let to = best.saturating_add(COARSE_STEP - 1).min(hi);
    for start in from..=to {
        let s = score(start);
        if s > best_score {
            best = start;
            best_score = s;
        }
    }
    best
}

/// How well `candidate` matches `target`: their correlation, divided by the
/// candidate's size so that a loud stretch does not win just for being loud.
/// The target is the same for every candidate, so its size would change no
/// ranking and is left out. Silence scores nothing.
fn similarity(target: &[f32], candidate: &[f32]) -> f64 {
    let mut dot = 0.0f64;
    let mut energy = 0.0f64;
    for (&t, &c) in target.iter().zip(candidate) {
        let c = f64::from(c);
        dot += f64::from(t) * c;
        energy += c * c;
    }
    dot / (energy + 1e-9).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 24_000;

    fn sine(freq: f64, seconds: f64) -> Vec<f32> {
        let n = (f64::from(RATE) * seconds) as usize;
        let step = 2.0 * PI * freq / f64::from(RATE);
        (0..n).map(|i| (0.8 * (step * i as f64).sin()) as f32).collect()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0, |m, &x| m.max(x.abs()))
    }

    /// Upward zero crossings per second over `samples`, which is the pitch of
    /// a clean tone.
    fn crossings_per_second(samples: &[f32]) -> f64 {
        let crossings = samples.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        crossings as f64 * f64::from(RATE) / samples.len() as f64
    }

    fn middle(samples: &[f32]) -> &[f32] {
        &samples[samples.len() / 4..samples.len() * 3 / 4]
    }

    #[test]
    fn speeds_that_mean_no_change_hand_the_samples_back() {
        let input = sine(220.0, 1.0);
        assert_eq!(stretch(&input, RATE, 1.0), input);
        assert_eq!(stretch(&input, RATE, f32::NAN), input);
        assert_eq!(stretch(&input, RATE, f32::INFINITY), input);
        assert_eq!(stretch(&input, RATE, 0.3), input);
        assert_eq!(stretch(&input, RATE, 3.0), input);
    }

    #[test]
    fn speech_twice_as_fast_is_half_as_long() {
        let input = sine(220.0, 3.0);
        let out = stretch(&input, RATE, 2.0);
        assert!(out.len().abs_diff(input.len() / 2) <= frame_len(RATE));
    }

    #[test]
    fn the_length_follows_the_speed_within_a_frame() {
        let input = sine(220.0, 4.0);
        let frame = frame_len(RATE);
        for speed in [0.5f32, 1.25, 1.5, 2.0] {
            let out = stretch(&input, RATE, speed);
            let expected = (input.len() as f64 / f64::from(speed)) as usize;
            assert!(
                out.len().abs_diff(expected) <= frame,
                "speed {speed}: {} samples, expected about {expected}",
                out.len()
            );
        }
    }

    #[test]
    fn a_tone_keeps_its_pitch_when_sped_up_or_slowed_down() {
        let input = sine(220.0, 4.0);
        let original = crossings_per_second(middle(&input));
        for speed in [1.5f32, 0.75] {
            let out = stretch(&input, RATE, speed);
            let pitch = crossings_per_second(middle(&out));
            assert!(
                (pitch - original).abs() <= original * 0.03,
                "speed {speed}: {pitch} crossings a second, was {original}"
            );
        }
    }

    #[test]
    fn silence_stays_silence() {
        let out = stretch(&[0.0f32; 48_000], RATE, 1.5);
        assert!(!out.is_empty());
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn the_level_is_kept() {
        let input = sine(220.0, 4.0);
        let frame = frame_len(RATE);
        for speed in [1.5f32, 0.75] {
            let out = stretch(&input, RATE, speed);
            let loudest = peak(&out[frame..out.len() - frame]);
            let ratio = loudest / peak(&input);
            assert!((0.9..=1.1).contains(&ratio), "speed {speed}: peak ratio {ratio}");
        }
    }

    #[test]
    fn audio_too_short_to_hear_is_handed_back() {
        assert!(stretch(&[], RATE, 1.5).is_empty());
        let few = [0.1f32, -0.2, 0.3, 0.0, 0.5, -0.5, 0.25, 0.0, 0.1, -0.1];
        assert_eq!(stretch(&few, RATE, 1.5), few.to_vec());
        let almost = sine(220.0, 0.03);
        assert_eq!(stretch(&almost, RATE, 0.75), almost);
    }

    #[test]
    fn a_sample_rate_of_zero_does_not_panic() {
        let input = sine(220.0, 1.0);
        let out = stretch(&input, 0, 1.5);
        assert!(!out.is_empty());
    }

    #[test]
    fn the_same_audio_is_stretched_the_same_way_every_time() {
        let input = sine(220.0, 1.0);
        assert_eq!(stretch(&input, RATE, 1.3), stretch(&input, RATE, 1.3));
    }

    #[test]
    fn the_window_adds_up_to_one_where_frames_overlap() {
        let frame = frame_len(RATE);
        let window = hann(frame);
        let (first, second) = window.split_at(frame / 2);
        for (a, b) in first.iter().zip(second) {
            assert!((a + b - 1.0).abs() < 1e-6);
        }
    }
}
