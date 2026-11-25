//! Chunks of a recording into timed segments: one trait, several backends.
//!
//! This is for anything that needs to know when each stretch of speech was
//! said, not only for meeting notes -- the meeting pipeline is its first
//! user, and a live transcript or a voice message would be handed the same
//! [`SpeechChunk`]s. Nothing here knows about speakers, meetings or where a
//! chunk was cut from; that is the caller's.
//!
//! Every backend is handed speech already cut by local VAD, in chunks under
//! its own limits, as 16 kHz mono WAV. Silence never crosses the wire.

mod build;
pub mod gemini;
#[cfg(feature = "speech")]
pub mod local;
pub mod openai;

pub use build::{OPENAI_BASE_URL, build_transcriber};

use crate::error::CommandResult;
use karmayoga_core::speech::words::Expected;
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
        crate::speech::pcm::duration_ms(self.samples.len())
    }

    /// A WAV file of the chunk, as uploaded.
    pub fn wav(&self) -> Vec<u8> {
        crate::speech::wav::encode(&self.samples, crate::speech::SAMPLE_RATE)
    }
}

/// What a request is told besides the audio.
#[derive(Debug, Clone, Default)]
pub struct Hints {
    /// BCP-47-ish, or `None` to detect.
    pub language: Option<String>,
    /// Names, companies, the title and the person's own words, so they are
    /// spelled right. See `karmayoga_core::meeting::vocab::transcriber_prompt`.
    /// What a server and Qwen read.
    pub prompt: String,
    /// The same words for a recogniser that boosts them as it decodes --
    /// Parakeet -- names and unusual words only. See
    /// `karmayoga_core::speech::words::Expected::boosted`.
    pub boosted: Vec<String>,
}

impl Hints {
    /// Told `language` and to expect `expected`.
    pub fn new(language: Option<String>, expected: Expected) -> Self {
        Self { language, prompt: expected.prompt, boosted: expected.boosted }
    }

    /// The words to expect, as a recogniser on this computer takes them.
    pub fn expected(&self) -> Expected {
        Expected { prompt: self.prompt.clone(), boosted: self.boosted.clone() }
    }
}

/// One stretch of speech as a backend heard it. Times relative to the chunk.
#[derive(Debug, Clone, PartialEq)]
pub struct RawSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// The backend's own speaker label, if it diarises. Only ever a hint.
    pub speaker_hint: Option<String>,
    /// How sure a Whisper-style backend was that nothing was said, and its
    /// mean log-probability for the words -- the two figures
    /// `verbose_json` carries that tell a line made up over silence from
    /// real speech. `None` from every backend that does not give them. See
    /// `karmayoga_core::meeting::hallucination::clean_line`.
    pub no_speech_prob: Option<f32>,
    pub avg_logprob: Option<f32>,
}

impl RawSegment {
    /// What was said, if it survives
    /// `karmayoga_core::meeting::hallucination::clean_line`: lines over a
    /// faint sound, a prompt read back, noise tags and loops come back as
    /// `None`. `prompt` is what the request was given as [`Hints::prompt`],
    /// and `faint` is [`is_faint`] for the audio it was asked about. Every
    /// caller that puts a backend's words in front of a person goes through
    /// this, so that what is dropped as made up is the same everywhere.
    pub fn cleaned(&self, prompt: &str, faint: bool) -> Option<String> {
        use karmayoga_core::meeting::hallucination::{self, Heard};
        let heard = Heard {
            text: &self.text,
            no_speech_prob: self.no_speech_prob,
            avg_logprob: self.avg_logprob,
        };
        hallucination::clean_line(heard, prompt, faint)
    }
}

/// Whether `samples` held almost no clearly loud sound, which is when a
/// recogniser is most likely to have made something up -- the `faint` that
/// [`RawSegment::cleaned`] asks for. `floor` is the noise floor to judge it
/// against, `karmayoga_core::meeting::hallucination::noise_floor` of the
/// whole stretch the audio came from, or of the audio itself when that is
/// all there is.
pub fn is_faint(samples: &[i16], floor: f32) -> bool {
    use karmayoga_core::meeting::hallucination;
    hallucination::clear_speech_ms(samples, floor) < hallucination::MIN_CLEAR_MS
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
