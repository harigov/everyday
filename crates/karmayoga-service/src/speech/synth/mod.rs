//! From words to audio.
//!
//! Two kinds of voice, one [`Synthesiser`]:
//!
//! - **OpenAI, or anything that speaks its dialect** ([`remote`]).
//!   `POST {base}/audio/speech` with the words, a model and a voice, answered
//!   with a WAV. OpenAI's own voices, and open-source ones run as a server of
//!   their own (Kokoro-FastAPI, Speaches, LocalAI), are the same request at
//!   different addresses, as [`crate::speech::transcribe::openai`] is for
//!   transcription. `wav` is asked for rather than `mp3` or `opus` because it
//!   is the one format that can be read here without a codec; reading it is
//!   [`crate::speech::wav`]'s.
//! - **Phonon 31M on this computer** ([`local`], feature `speech`), Gradium's
//!   English voice, run by `ptts`. Held in a
//!   [`Resident`](crate::local::resident::Resident), so it is loaded for the
//!   first sentence and let go of once nobody has needed a voice for a while.
//!
//! What is said, and who hears it, is the caller's: this turns one piece of
//! text into one stretch of audio.

mod local;
mod remote;

pub use remote::OpenAiSynthesiser;

use super::transcribe::BoxFuture;
use crate::error::{CommandError, CommandResult, codes};
use karmayoga_core::speech::{SynthesiserConfig, VoiceAccess};
use std::sync::Arc;

/// A stretch of synthesised speech, mono.
#[derive(Debug, Clone, PartialEq)]
pub struct Speech {
    pub samples: Vec<i16>,
    pub sample_rate: u32,
}

/// Turns words into speech.
pub trait Synthesiser: Send + Sync {
    fn synthesise<'a>(&'a self, text: &'a str) -> BoxFuture<'a, CommandResult<Speech>>;
}

/// The configured voice, as a [`Synthesiser`].
///
/// A local voice is loaded here, so that the person is told it is missing
/// when they first ask for speech rather than after, and so that the seconds
/// it takes belong to whatever is starting up.
pub async fn build_synthesiser(access: VoiceAccess) -> CommandResult<Arc<dyn Synthesiser>> {
    let VoiceAccess { config, key } = access;
    let speed = config.speed;
    match config.synthesiser {
        SynthesiserConfig::Local { voice, speaker } => local::build(voice, speaker, speed).await,
        SynthesiserConfig::OpenAi { model, voice } => Ok(Arc::new(OpenAiSynthesiser::new(
            crate::speech::transcribe::OPENAI_BASE_URL,
            key,
            model,
            voice,
            speed,
        ))),
        SynthesiserConfig::Compatible { base_url, model, voice } => {
            Ok(Arc::new(OpenAiSynthesiser::new(base_url, key, model, voice, speed)))
        }
    }
}

/// What every voice answers to words with nothing in them.
fn nothing_to_say() -> CommandError {
    CommandError::new(codes::INVALID, "there is nothing to say")
}
