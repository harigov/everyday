//! The local backend: sherpa-onnx, entirely on this machine. Feature
//! `speech` only.
//!
//! Unlike the remote backends, nothing here is a network request: VAD cuts
//! the chunk into turns first (silence never even reaches a recogniser, the
//! same rule the module doc on `meeting::transcribe` states for every
//! backend, just enforced locally rather than by not sending bytes), and
//! each turn is recognised on its own, off the async runtime.
//!
//! # Why [`Self::diarises`] is `false`
//!
//! `meeting::speech::SpeechKit::diarise` exists and works over a chunk's
//! worth of audio, but this backend never calls it. Per
//! `docs/plans/meeting-notes.md`'s "Who said what", turns are clustered
//! across the *whole* meeting, not per chunk, so that "speaker A" in one
//! chunk and "speaker B" in the next can be recognised as the same person.
//! A per-chunk call here could only ever produce indices scoped to that one
//! chunk -- exactly the labelling `SpeechChunk::scope` exists to warn a
//! caller not to compare across chunks -- so it would be a second,
//! chunk-scoped diarisation pass that the pipeline's own whole-meeting pass
//! immediately supersedes. Returning `false` here means the pipeline
//! diarises the system track itself, once, the same way it would for a
//! remote backend that gave no speaker hints at all.

use super::{BoxFuture, Hints, Limits, RawSegment, SpeechChunk, Transcriber};
use crate::error::CommandResult;
use crate::meeting::speech;
use crate::service::blocking;
use everyday_core::meeting::{LocalModel, SAMPLE_RATE};

/// Generous, per the plan: local recognition pays no per-request bill and
/// crosses no wire, so the only real limit is memory for one chunk's
/// samples. An hour of 16 kHz mono `i16` is well under 200 MB.
const MAX_SECONDS: u32 = 60 * 60;
const MAX_BYTES: usize = 2_000_000_000;

pub struct LocalTranscriber {
    model: LocalModel,
}

impl LocalTranscriber {
    pub fn new(model: LocalModel) -> Self {
        Self { model }
    }

    async fn transcribe_chunk(
        &self,
        chunk: &SpeechChunk,
        hints: &Hints,
    ) -> CommandResult<Vec<RawSegment>> {
        let kit = speech::speech_kit()?;
        let samples = chunk.samples.clone();
        let turns = blocking({
            let kit = kit.clone();
            move || Ok(kit.vad(&samples))
        })
        .await?;

        let recogniser = speech::recogniser(self.model)?;
        let language = hints.language.clone();
        let mut segments = Vec::with_capacity(turns.len());
        for (start_ms, end_ms) in turns {
            let start = index_of(start_ms);
            let end = index_of(end_ms).min(chunk.samples.len());
            if start >= end {
                continue;
            }
            let turn_samples = chunk.samples[start..end].to_vec();
            let recogniser = recogniser.clone();
            let language = language.clone();
            let text = blocking(move || {
                Ok(recogniser.transcribe(&turn_samples, language.as_deref()).text)
            })
            .await?;
            let text = text.trim().to_string();
            if text.is_empty() {
                continue;
            }
            segments.push(RawSegment { start_ms, end_ms, text, speaker_hint: None });
        }
        Ok(segments)
    }
}

fn index_of(ms: u64) -> usize {
    (ms * u64::from(SAMPLE_RATE) / 1000) as usize
}

impl Transcriber for LocalTranscriber {
    fn limits(&self) -> Limits {
        Limits { max_bytes: MAX_BYTES, max_seconds: MAX_SECONDS }
    }

    /// See this module's doc.
    fn diarises(&self) -> bool {
        false
    }

    fn transcribe<'a>(
        &'a self,
        chunk: &'a SpeechChunk,
        hints: &'a Hints,
    ) -> BoxFuture<'a, CommandResult<Vec<RawSegment>>> {
        Box::pin(async move { self.transcribe_chunk(chunk, hints).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_are_generous() {
        let t = LocalTranscriber::new(LocalModel::ParakeetV3);
        let limits = t.limits();
        assert!(limits.max_seconds >= 3600);
        assert!(limits.max_bytes >= 1_000_000_000);
    }

    #[test]
    fn a_local_transcriber_never_diarises_itself() {
        assert!(!LocalTranscriber::new(LocalModel::ParakeetV3).diarises());
        assert!(!LocalTranscriber::new(LocalModel::WhisperTurbo).diarises());
    }

    #[test]
    fn index_of_converts_ms_to_sample_index() {
        assert_eq!(index_of(0), 0);
        assert_eq!(index_of(1000), SAMPLE_RATE as usize);
        assert_eq!(index_of(500), SAMPLE_RATE as usize / 2);
    }
}
