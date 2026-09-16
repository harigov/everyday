//! Two tracks into one ordered transcript, without the echo.

use super::TrackSegment;

/// Drop mic segments that are the call's audio heard through the speakers:
/// overlapping in time a system segment with near-identical text
/// (normalised token overlap at or above `threshold`, e.g. 0.6). Returns
/// the kept segments of both tracks, ordered by start, and whether any
/// echo was found.
pub fn remove_echo(segments: Vec<TrackSegment>, threshold: f32) -> (Vec<TrackSegment>, bool) {
    let _ = (segments, threshold);
    todo!("core-logic agent")
}

/// Normalised token overlap of two strings, 0.0..=1.0.
pub fn similarity(a: &str, b: &str) -> f32 {
    let _ = (a, b);
    todo!("core-logic agent")
}
