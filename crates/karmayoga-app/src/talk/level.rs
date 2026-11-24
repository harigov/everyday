//! How loud the microphone is, for the card's meter.
//!
//! The session has no level of its own, but the desktop's side of it sees
//! every block of microphone audio on the way in, echo already taken out, so
//! the meter shows the person and not what the assistant is saying.

use std::time::{Duration, Instant};

/// The quietest sound the meter shows and the loudest it tells apart, in dB
/// below full scale. Speech at a desk is around -30.
const QUIET_DB: f32 = -55.0;
const LOUD_DB: f32 = -10.0;

/// The level goes to the card this often, however often the microphone
/// reports.
const EVERY: Duration = Duration::from_millis(50);

/// How loud a block of microphone audio is, from 0 to 1, on a dB scale
/// because that is how loudness is heard.
pub fn level(block: &[i16]) -> f32 {
    if block.is_empty() {
        return 0.0;
    }
    let mean =
        block.iter().map(|&s| (f64::from(s) / 32768.0).powi(2)).sum::<f64>() / block.len() as f64;
    if mean <= 0.0 {
        return 0.0;
    }
    let db = 10.0 * mean.log10() as f32;
    ((db - QUIET_DB) / (LOUD_DB - QUIET_DB)).clamp(0.0, 1.0)
}

/// Thins what the microphone reports to what the card can use, keeping the
/// loudest of each stretch so a syllable between two updates still shows.
pub struct Meter {
    last: Instant,
    peak: f32,
}

impl Meter {
    pub fn new(now: Instant) -> Self {
        Self { last: now, peak: 0.0 }
    }

    /// The level to send now, if it is time to send one.
    pub fn hear(&mut self, block: &[i16], now: Instant) -> Option<f32> {
        self.peak = self.peak.max(level(block));
        if now.saturating_duration_since(self.last) < EVERY {
            return None;
        }
        self.last = now;
        Some(std::mem::take(&mut self.peak))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_nothing_and_a_loud_voice_is_most_of_the_meter() {
        assert_eq!(level(&[]), 0.0);
        assert_eq!(level(&[0; 480]), 0.0);
        let loud: Vec<i16> = (0..480).map(|i| if i % 2 == 0 { 10_000 } else { -10_000 }).collect();
        let shown = level(&loud);
        assert!((0.7..=1.0).contains(&shown), "{shown}");
        let faint: Vec<i16> = (0..480).map(|i| if i % 2 == 0 { 20 } else { -20 }).collect();
        assert!(level(&faint) < 0.1);
    }

    #[test]
    fn the_meter_reports_about_twenty_times_a_second_and_keeps_the_peak() {
        let start = Instant::now();
        let mut meter = Meter::new(start);
        let loud = vec![12_000i16; 480];
        let quiet = vec![0i16; 480];
        assert_eq!(meter.hear(&loud, start + Duration::from_millis(30)), None);
        let shown = meter.hear(&quiet, start + Duration::from_millis(60)).expect("time to report");
        assert!(shown > 0.5, "the loud block between is not lost: {shown}");
        assert_eq!(meter.hear(&quiet, start + Duration::from_millis(80)), None);
    }
}
