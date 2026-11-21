//! The pause after a sentence, made the same whichever voice said it.
//!
//! Each model leaves its own silence round a clip, and they are nothing
//! alike: measured on the same sentences, Phonon starts at once and leaves
//! half a second to three quarters after every one, so a reply of short
//! sentences dragged; Kokoro, through sherpa-onnx, leaves a third of a second
//! before and nearly half after. Played back to back, neither is a pause a
//! person would leave. So the silence a voice made is cut back to the speech
//! in it, with a moment either side for the breath and the decay, and the
//! pause [`script::pause_after`](crate::speech::script::pause_after) asks
//! for is added after. The edges are faded, so a clip that starts or stops
//! on a sample that is not silence -- Phonon opens every clip with a faint
//! thump -- does not click.
//!
//! Only the edges: a pause inside a clip, at a comma or between two short
//! sentences said together, is the voice's own.

use super::Speech;
use std::time::Duration;

/// How finely the level is measured.
const FRAME: Duration = Duration::from_millis(10);
/// Quieter than this far under the loudest frame is silence: well under
/// the softest consonant, well over a model's hiss.
const QUIET_DB: f32 = -50.0;
/// Speech starts with this many frames in a row over the line, so that a
/// click on its own before it is not taken for its start.
const ONSET_FRAMES: usize = 3;
/// Kept before the speech starts: a soft onset ("h") that measures under the
/// line, and room for the fade.
const LEAD: Duration = Duration::from_millis(30);
/// Kept after the speech ends: the decay of the last sound.
const TAIL: Duration = Duration::from_millis(80);
const FADE_IN: Duration = Duration::from_millis(8);
const FADE_OUT: Duration = Duration::from_millis(30);

fn samples_in(duration: Duration, rate: u32) -> usize {
    (duration.as_secs_f64() * f64::from(rate)).round() as usize
}

/// `speech` with its own silence cut back to the speech in it, faded in and
/// out, and `pause` of silence after. A clip with no sound in it is handed
/// back as it is, the pause added.
pub fn paced(speech: Speech, pause: Duration) -> Speech {
    let Speech { mut samples, sample_rate } = speech;
    if sample_rate == 0 {
        return Speech { samples, sample_rate };
    }
    if let Some((start, end)) = speech_span(&samples, sample_rate) {
        samples.truncate(end);
        samples.drain(..start);
        fade(&mut samples, sample_rate);
    }
    samples.resize(samples.len() + samples_in(pause, sample_rate), 0);
    Speech { samples, sample_rate }
}

/// Where the speech in `samples` starts and ends, with [`LEAD`] and [`TAIL`]
/// round it, or `None` when there is none.
fn speech_span(samples: &[i16], rate: u32) -> Option<(usize, usize)> {
    let frame = samples_in(FRAME, rate).max(1);
    let levels: Vec<f32> = samples
        .chunks(frame)
        .map(|c| (c.iter().map(|&s| f32::from(s).powi(2)).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let loudest = levels.iter().copied().fold(0.0f32, f32::max);
    if loudest < 1.0 {
        return None;
    }
    let line = loudest * 10f32.powf(QUIET_DB / 20.0);
    let loud: Vec<bool> = levels.iter().map(|&l| l > line).collect();
    let first = (0..loud.len())
        .find(|&i| loud[i..(i + ONSET_FRAMES).min(loud.len())].iter().all(|&l| l))
        .or_else(|| loud.iter().position(|&l| l))?;
    let last = loud.iter().rposition(|&l| l)?;
    let start = (first * frame).saturating_sub(samples_in(LEAD, rate));
    let end = ((last + 1) * frame + samples_in(TAIL, rate)).min(samples.len());
    (start < end).then_some((start, end))
}

/// A straight ramp up at the start and down at the end.
fn fade(samples: &mut [i16], rate: u32) {
    let len = samples.len();
    let fade_in = samples_in(FADE_IN, rate).min(len / 2);
    let fade_out = samples_in(FADE_OUT, rate).min(len / 2);
    for (i, s) in samples.iter_mut().take(fade_in).enumerate() {
        *s = (f32::from(*s) * i as f32 / fade_in as f32) as i16;
    }
    for (i, s) in samples.iter_mut().rev().take(fade_out).enumerate() {
        *s = (f32::from(*s) * i as f32 / fade_out as f32) as i16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 24_000;

    fn ms(n: u64) -> usize {
        samples_in(Duration::from_millis(n), RATE)
    }

    /// A tone `loud_ms` long with `before` and `after` of silence round it.
    fn clip(before: u64, loud_ms: u64, after: u64) -> Speech {
        let mut samples = vec![0i16; ms(before)];
        samples.extend((0..ms(loud_ms)).map(|i| {
            let t = i as f32 / RATE as f32;
            (8000.0 * (2.0 * std::f32::consts::PI * 220.0 * t).sin()) as i16
        }));
        samples.extend(vec![0i16; ms(after)]);
        Speech { samples, sample_rate: RATE }
    }

    #[test]
    fn a_voice_s_own_silence_is_cut_back_and_the_pause_asked_for_added() {
        // Kokoro's shape: a third of a second before, nearly half after.
        let out = paced(clip(320, 1000, 460), Duration::from_millis(300));
        let expected = ms(30) + ms(1000) + ms(80) + ms(300);
        let slack = ms(20);
        assert!(
            out.samples.len().abs_diff(expected) <= slack,
            "{} vs {expected}",
            out.samples.len()
        );
        // Phonon's: nothing before, three quarters of a second after.
        let out = paced(clip(0, 1000, 730), Duration::from_millis(300));
        let expected = ms(1000) + ms(80) + ms(300);
        assert!(
            out.samples.len().abs_diff(expected) <= slack,
            "{} vs {expected}",
            out.samples.len()
        );
        // Ends in the silence asked for.
        assert!(out.samples[out.samples.len() - ms(300)..].iter().all(|&s| s == 0));
    }

    #[test]
    fn the_edges_are_faded_so_nothing_clicks() {
        let out = paced(clip(0, 500, 0), Duration::ZERO);
        assert_eq!(out.samples[0], 0);
        assert_eq!(*out.samples.last().unwrap(), 0);
        let loudest = out.samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(out.samples[1..10].iter().all(|s| s.unsigned_abs() < loudest / 4));
    }

    #[test]
    fn a_click_before_the_speech_is_not_its_start() {
        let mut speech = clip(150, 600, 100);
        for s in &mut speech.samples[..ms(10)] {
            *s = -300;
        }
        let out = paced(speech, Duration::ZERO);
        // Cut to just before the tone: the click is gone with the silence.
        assert!(out.samples.len() <= ms(30 + 600 + 80 + 20), "{}", out.samples.len());
    }

    #[test]
    fn a_clip_with_no_sound_keeps_what_it_had_and_gains_the_pause() {
        let silent = Speech { samples: vec![0; ms(200)], sample_rate: RATE };
        assert_eq!(paced(silent, Duration::from_millis(100)).samples.len(), ms(300));
        let none = Speech { samples: Vec::new(), sample_rate: 0 };
        assert!(paced(none, Duration::from_millis(100)).samples.is_empty());
    }
}
