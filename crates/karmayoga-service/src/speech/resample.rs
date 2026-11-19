//! Sample-rate conversion for everything that handles audio at more than one
//! rate: the desktop's devices, and a call's 48 kHz codec.
//!
//! Two shapes of it. [`StreamResampler`] is for audio that never ends -- a
//! microphone, a copy of what was sent to the speakers, a meeting's tracks --
//! and keeps its state between pushes. [`clip`] is for one finished stretch of
//! speech, which is converted whole with the converter's start-up delay
//! trimmed off, so a sentence does not begin with a fraction of a second of
//! nothing.
//!
//! None of it belongs in a device callback: building a resampler allocates,
//! and so does running one.

use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler as _};

/// Why a conversion could not be made: a rate the converter refuses, or a
/// buffer of the wrong size.
#[derive(Debug, Clone)]
pub struct ResampleError(pub String);

impl std::fmt::Display for ResampleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ResampleError {}

/// Input frames handed to the converter at a time. At 48 kHz it is a little
/// over 21 ms. Meeting capture's output depends on it, so it is not a tuning
/// knob.
const CHUNK_FRAMES: usize = 1024;

/// Converts a continuous mono stream from one rate to another.
pub struct StreamResampler {
    /// `None` when the two rates are the same and a copy is enough.
    inner: Option<Fft<f32>>,
    /// Input not yet enough for a whole chunk.
    pending: Vec<f32>,
    /// Where one chunk's output is written.
    scratch: Vec<f32>,
    target_rate: u32,
}

impl StreamResampler {
    /// The same rate on both sides is a plain copy, with no delay.
    pub fn new(from: u32, to: u32) -> Result<Self, ResampleError> {
        if from == to {
            return Ok(Self {
                inner: None,
                pending: Vec::new(),
                scratch: Vec::new(),
                target_rate: to,
            });
        }
        Self::always_converting(from, to)
    }

    /// As [`new`](Self::new), but the audio goes through the converter even at
    /// the same rate on both sides, with its filtering and delay. Meeting
    /// capture asks for this so that a 16 kHz microphone is treated exactly
    /// as it was before the two shared this.
    pub fn always_converting(from: u32, to: u32) -> Result<Self, ResampleError> {
        let inner = build(from, to)?;
        let scratch = vec![0.0; inner.output_frames_max()];
        Ok(Self { inner: Some(inner), pending: Vec::new(), scratch, target_rate: to })
    }

    /// Feed more input; whatever output is ready is appended to `out`. Often
    /// nothing, while a chunk fills.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<f32>) -> Result<(), ResampleError> {
        if self.inner.is_none() {
            out.extend_from_slice(samples);
            return Ok(());
        }
        self.pending.extend_from_slice(samples);
        while self.pending.len() >= CHUNK_FRAMES {
            let (taken, made) = self.process(None)?;
            out.extend_from_slice(&self.scratch[..made]);
            // The converter says how much it took, rather than this assuming
            // a whole chunk: it nudges the chunk to keep the ratio exact.
            self.pending.drain(..taken.max(1));
        }
        Ok(())
    }

    /// Drain whatever input is still waiting for a whole chunk, padded with
    /// silence, and append its output to `out`. For a genuine stop, or before
    /// a stream's resampler is rebuilt for a reopened device: afterwards
    /// nothing is held and a fresh `push` starts clean.
    pub fn flush(&mut self, out: &mut Vec<f32>) -> Result<(), ResampleError> {
        if self.inner.is_none() || self.pending.is_empty() {
            return Ok(());
        }
        let remaining = self.pending.len();
        self.pending.resize(CHUNK_FRAMES, 0.0);
        let (_taken, made) = self.process(Some(remaining))?;
        self.pending.clear();
        out.extend_from_slice(&self.scratch[..made]);
        Ok(())
    }

    /// One call to the converter over the first [`CHUNK_FRAMES`] of `pending`,
    /// of which only `partial_len` are real (the last, padded call of a
    /// flush). Returns how many input frames it took and how many output
    /// frames it made, written at the start of `scratch`.
    fn process(&mut self, partial_len: Option<usize>) -> Result<(usize, usize), ResampleError> {
        let Some(inner) = self.inner.as_mut() else {
            return Ok((0, 0));
        };
        let input = InterleavedSlice::new(&self.pending[..CHUNK_FRAMES], 1, CHUNK_FRAMES)
            .map_err(|e| ResampleError(e.to_string()))?;
        let frames = self.scratch.len();
        let mut output = InterleavedSlice::new_mut(&mut self.scratch, 1, frames)
            .map_err(|e| ResampleError(e.to_string()))?;
        let indexing = partial_len.map(|n| Indexing::new().partial_len(n));
        inner
            .process_into_buffer(&input, &mut output, indexing.as_ref())
            .map_err(|e| ResampleError(format!("resample: {e}")))
    }

    /// How long a sound takes to come out after it goes in, because of the
    /// converter itself.
    pub fn delay_ms(&self) -> u32 {
        match &self.inner {
            Some(inner) => {
                (inner.output_delay() as u64 * 1000 / self.target_rate.max(1) as u64) as u32
            }
            None => 0,
        }
    }
}

/// One whole clip, converted in one go. The same rate on both sides returns the
/// clip as it is.
pub fn clip(samples: &[f32], from: u32, to: u32) -> Result<Vec<f32>, ResampleError> {
    if from == to || samples.is_empty() {
        return Ok(samples.to_vec());
    }
    let mut inner = build(from, to)?;
    let input = InterleavedSlice::new(samples, 1, samples.len())
        .map_err(|e| ResampleError(e.to_string()))?;
    let converted = inner
        .process_all(&input, samples.len(), None)
        .map_err(|e| ResampleError(format!("resample: {e}")))?;
    Ok(converted.take_data())
}

fn build(from: u32, to: u32) -> Result<Fft<f32>, ResampleError> {
    Fft::<f32>::new(from as usize, to as usize, CHUNK_FRAMES, 1, FixedSync::Input)
        .map_err(|e| ResampleError(format!("could not build resampler: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hertz: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (2.0 * std::f32::consts::PI * hertz * i as f32 / rate as f32).sin() * 0.5)
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
    }

    #[test]
    fn the_same_rate_is_a_copy() {
        let mut resampler = StreamResampler::new(16_000, 16_000).unwrap();
        let input = sine(16_000, 440.0, 300);
        let mut out = Vec::new();
        resampler.push(&input, &mut out).unwrap();
        assert_eq!(out, input);
        assert_eq!(resampler.delay_ms(), 0);
        assert_eq!(clip(&input, 16_000, 16_000).unwrap(), input);
    }

    #[test]
    fn a_stream_comes_out_at_the_ratio_in_pieces_of_any_size() {
        let mut resampler = StreamResampler::new(48_000, 16_000).unwrap();
        let input = sine(48_000, 440.0, 48_000);
        let mut out = Vec::new();
        // Odd, unequal pieces, as device callbacks come.
        for piece in input.chunks(441) {
            resampler.push(piece, &mut out).unwrap();
        }
        // A second of input is a second of output, less what is still held in
        // an unfinished chunk.
        assert!(out.len() <= 16_000, "{} frames", out.len());
        assert!(out.len() >= 16_000 - CHUNK_FRAMES, "{} frames", out.len());
        // And it is still the tone, not noise or silence.
        assert!(rms(&out[2_000..]) > 0.25, "rms {}", rms(&out[2_000..]));
    }

    #[test]
    fn a_flush_drains_the_last_partial_chunk_and_leaves_nothing_behind() {
        let mut resampler = StreamResampler::new(48_000, 16_000).unwrap();
        let input = sine(48_000, 440.0, 20_000);
        let mut out = Vec::new();
        resampler.push(&input, &mut out).unwrap();
        let before = out.len();
        // 20 000 frames is nineteen whole chunks and a bit.
        resampler.flush(&mut out).unwrap();
        assert!(out.len() > before, "the partial chunk came out");
        let flushed = out.len();
        resampler.flush(&mut out).unwrap();
        assert_eq!(out.len(), flushed, "a second flush has nothing to give");
    }

    #[test]
    fn a_flush_with_nothing_waiting_or_nothing_to_convert_is_empty() {
        let mut out = Vec::new();
        StreamResampler::new(48_000, 16_000).unwrap().flush(&mut out).unwrap();
        let mut copy = StreamResampler::new(16_000, 16_000).unwrap();
        copy.push(&sine(16_000, 440.0, 100), &mut out).unwrap();
        out.clear();
        copy.flush(&mut out).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn asking_for_the_converter_at_the_same_rate_gets_it() {
        let mut resampler = StreamResampler::always_converting(16_000, 16_000).unwrap();
        let input = sine(16_000, 440.0, 4 * CHUNK_FRAMES);
        let mut out = Vec::new();
        resampler.push(&input, &mut out).unwrap();
        // Not the copy `new` would have made: the converter delays and filters.
        assert_ne!(out, input);
    }

    #[test]
    fn a_clip_keeps_its_length_and_starts_at_once() {
        let input = sine(24_000, 300.0, 24_000);
        let out = clip(&input, 24_000, 48_000).unwrap();
        assert_eq!(out.len(), 48_000);
        // The start-up delay is trimmed, so the first tenth of a second is
        // already the tone.
        assert!(rms(&out[..4_800]) > 0.25, "rms {}", rms(&out[..4_800]));
    }

    #[test]
    fn an_empty_clip_is_empty() {
        assert!(clip(&[], 24_000, 48_000).unwrap().is_empty());
    }
}
