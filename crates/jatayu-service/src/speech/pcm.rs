//! Samples as 16-bit integers and as floats, and how long a run of them is.
//!
//! Devices, files and servers hand audio over as `i16`; the models want `f32`
//! between -1 and 1. Everything that crosses between the two does it here, so
//! the same sample means the same thing wherever it is met.
//!
//! There are two ways from a float back to an integer, on purpose, and each
//! says why it exists:
//!
//! - [`to_i16`] is for audio a detector hands back after cutting it from a
//!   stream, which was made by [`to_f32`]. It scales by 32768, so that going
//!   there and back is exact for every sample.
//! - [`sample_to_i16`] is for audio a voice has made. It scales by 32767, so
//!   that a full-scale sample is `i16::MAX` and its negation is `-i16::MAX`: a
//!   voice that peaks at exactly 1.0 is symmetrical rather than clipped on one
//!   side.

use super::SAMPLE_RATE;

/// 16-bit samples as floats from -1 to just under 1.
pub fn to_f32(samples: &[i16]) -> Vec<f32> {
    samples.iter().map(|&s| f32::from(s) / 32768.0).collect()
}

/// Floats as 16-bit samples, the inverse of [`to_f32`]: scaled by 32768 and
/// clamped, so that a float that came from a sample goes back to it.
pub fn to_i16(samples: &[f32]) -> Vec<i16> {
    samples.iter().map(|&s| (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16).collect()
}

/// One float sample from a voice as 16 bits, scaled by 32767 and clamped: a
/// voice that overshoots clips rather than wrapping round into a crackle.
/// Not-a-number is silence.
pub fn sample_to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// How long `sample_count` samples last at [`SAMPLE_RATE`], in milliseconds.
pub fn duration_ms(sample_count: usize) -> u64 {
    sample_count as u64 * 1000 / u64::from(SAMPLE_RATE)
}

/// How many samples `ms` milliseconds is at [`SAMPLE_RATE`].
pub fn sample_count(ms: u64) -> usize {
    (ms * u64::from(SAMPLE_RATE) / 1000) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_become_floats_between_minus_one_and_one() {
        assert_eq!(to_f32(&[0, 16384, -16384, i16::MIN]), vec![0.0, 0.5, -0.5, -1.0]);
        assert!(to_f32(&[i16::MAX])[0] < 1.0);
        assert!(to_f32(&[]).is_empty());
    }

    #[test]
    fn a_detectors_conversion_round_trips_every_sample() {
        let all: Vec<i16> = (i16::MIN..=i16::MAX).collect();
        assert_eq!(to_i16(&to_f32(&all)), all);
    }

    #[test]
    fn floats_beyond_full_scale_are_clamped_and_nan_is_silence() {
        assert_eq!(
            to_i16(&[0.0, 0.5, -0.5, 1.0, -1.0, 2.0, -2.0, f32::NAN]),
            vec![0, 16384, -16384, i16::MAX, i16::MIN, i16::MAX, i16::MIN, 0]
        );
    }

    #[test]
    fn a_voices_conversion_is_symmetrical_about_silence() {
        assert_eq!(sample_to_i16(0.0), 0);
        assert_eq!(sample_to_i16(0.5), 16_384);
        assert_eq!(sample_to_i16(1.0), i16::MAX);
        assert_eq!(sample_to_i16(-1.0), -i16::MAX);
        assert_eq!(sample_to_i16(2.0), i16::MAX);
        assert_eq!(sample_to_i16(-3.0), -i16::MAX);
        assert_eq!(sample_to_i16(f32::NAN), 0);
    }

    #[test]
    fn the_two_integer_conversions_disagree_about_negative_full_scale() {
        // Documented, not accidental: see the module docs.
        assert_eq!(to_i16(&[-1.0]), vec![i16::MIN]);
        assert_eq!(sample_to_i16(-1.0), -i16::MAX);
    }

    #[test]
    fn a_run_of_samples_is_as_long_as_its_rate_makes_it() {
        assert_eq!(duration_ms(0), 0);
        assert_eq!(duration_ms(SAMPLE_RATE as usize), 1000);
        assert_eq!(duration_ms(SAMPLE_RATE as usize / 2), 500);
        assert_eq!(duration_ms(SAMPLE_RATE as usize * 3600), 3_600_000);
        // Rounds down: a sample is a fraction of a millisecond.
        assert_eq!(duration_ms(15), 0);
        assert_eq!(duration_ms(16), 1);
    }
}
