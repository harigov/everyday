//! Speech to text: one trait, several backends.
//!
//! Every backend is handed speech already cut by local VAD, in chunks under
//! its own limits, as 16 kHz mono WAV. Silence never crosses the wire.

pub mod gemini;
#[cfg(feature = "speech")]
pub mod local;
pub mod openai;

use crate::error::CommandResult;
use std::future::Future;
use std::pin::Pin;

/// What a backend can take in one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_bytes: usize,
    pub max_seconds: u32,
}

/// One request's worth of speech from one track.
#[derive(Debug, Clone)]
pub struct SpeechChunk {
    /// 16 kHz mono i16 samples.
    pub samples: Vec<i16>,
    /// Where `samples[0]` sits, from the recording's start. Returned segment
    /// times are relative to the chunk; the caller adds this.
    pub offset_ms: u64,
    /// Scopes speaker hints: labels from two chunks are not comparable.
    pub scope: u32,
}

impl SpeechChunk {
    pub fn duration_ms(&self) -> u64 {
        self.samples.len() as u64 * 1000 / u64::from(everyday_core::meeting::SAMPLE_RATE)
    }

    /// A WAV file of the chunk, as uploaded.
    pub fn wav(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(44 + self.samples.len() * 2);
        let data_len = (self.samples.len() * 2) as u32;
        let rate = everyday_core::meeting::SAMPLE_RATE;
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // mono
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for s in &self.samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }
}

/// What a request is told besides the audio.
#[derive(Debug, Clone, Default)]
pub struct Hints {
    /// BCP-47-ish, or `None` to detect.
    pub language: Option<String>,
    /// Names and the title, so they are spelled right.
    /// See `everyday_core::meeting::prompt::transcriber_hint`.
    pub prompt: String,
}

/// One stretch of speech as a backend heard it. Times relative to the chunk.
#[derive(Debug, Clone, PartialEq)]
pub struct RawSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// The backend's own speaker label, if it diarises. Only ever a hint.
    pub speaker_hint: Option<String>,
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A speech-to-text backend.
pub trait Transcriber: Send + Sync {
    fn limits(&self) -> Limits;
    /// Whether segments come back with speaker labels.
    fn diarises(&self) -> bool;
    fn transcribe<'a>(
        &'a self,
        chunk: &'a SpeechChunk,
        hints: &'a Hints,
    ) -> BoxFuture<'a, CommandResult<Vec<RawSegment>>>;
}
