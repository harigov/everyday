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
//! - **A voice on this computer** ([`local`], feature `speech`): Gradium's
//!   Phonon 31M, run by `ptts`, or hexgrad's Kokoro 82M, run by sherpa-onnx
//!   ([`kokoro`]), both English. Held in a
//!   [`Resident`](crate::local::resident::Resident), so it is loaded for the
//!   first sentence and let go of once nobody has needed a voice for a while.
//!
//! What is said, and who hears it, is the caller's: this turns one piece of
//! text into one stretch of audio.
//!
//! Every voice is built inside a [`Delivery`], which is what makes them say
//! things the same way: the words the person has said how to say marked for
//! the voice, and the pause after each sentence cut to the one its ending
//! asks for ([`pace`]). The text a voice is handed is a sentence in
//! [`script`](crate::speech::script)'s terms -- prose, and perhaps a
//! `{word|respelling}` -- which each voice says in its own way.

mod kokoro;
mod local;
mod pace;
mod remote;

pub use remote::OpenAiSynthesiser;

use super::script::{self, Book, LINE_END};
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
    let book = Book::new(&access.pronunciations);
    build_synthesiser_with(access, book).await
}

/// [`build_synthesiser`], reading the person's pronunciations from `book`
/// as each sentence is said rather than from `access`, so that a change to
/// them reaches a voice already in use. See
/// [`Service::pronunciations`](crate::service::Service::pronunciations).
pub async fn build_synthesiser_with(
    access: VoiceAccess,
    book: Book,
) -> CommandResult<Arc<dyn Synthesiser>> {
    let VoiceAccess { config, key, pronunciations: _ } = access;
    let speed = config.speed;
    let voice: Arc<dyn Synthesiser> = match config.synthesiser {
        SynthesiserConfig::Local { voice, speaker } => local::build(voice, speaker, speed).await?,
        SynthesiserConfig::OpenAi { model, voice } => Arc::new(
            OpenAiSynthesiser::new(
                crate::speech::transcribe::OPENAI_BASE_URL,
                key,
                model.clone(),
                voice,
                speed,
            )
            .with_instructions(remote::instructions_for(&model)),
        ),
        SynthesiserConfig::Compatible { base_url, model, voice } => {
            Arc::new(OpenAiSynthesiser::new(base_url, key, model, voice, speed))
        }
    };
    Ok(Arc::new(Delivery { voice, book, speed }))
}

/// A voice as everything that talks uses it.
///
/// Hands the voice each sentence with the person's pronunciations marked in
/// it and the line-end mark taken off, and hands back the audio with the
/// voice's own silence round it replaced by the pause the sentence's ending
/// asks for -- shorter at a faster speed, as everything else is.
struct Delivery {
    voice: Arc<dyn Synthesiser>,
    book: Book,
    speed: f32,
}

impl Synthesiser for Delivery {
    fn synthesise<'a>(&'a self, text: &'a str) -> BoxFuture<'a, CommandResult<Speech>> {
        Box::pin(async move {
            let ended_line = text.trim_end().ends_with(LINE_END);
            let words: String = text.chars().filter(|&c| c != LINE_END).collect();
            let shown = script::shown(&words);
            if shown.trim().is_empty() {
                return Err(nothing_to_say());
            }
            let pause =
                script::pause_after_shown(&shown, ended_line).div_f32(self.speed.clamp(0.5, 2.0));
            let speech = self.voice.synthesise(&self.book.mark(&words)).await?;
            Ok(pace::paced(speech, pause))
        })
    }
}

/// What every voice answers to words with nothing in them.
fn nothing_to_say() -> CommandError {
    CommandError::new(codes::INVALID, "there is nothing to say")
}

#[cfg(test)]
mod tests {
    use super::*;
    use karmayoga_core::speech::Pronunciation;
    use std::sync::Mutex;

    /// A voice that keeps what it was handed and says a short tone with a
    /// long silence after it, as Phonon would.
    #[derive(Default)]
    struct Heard(Mutex<Vec<String>>);

    impl Synthesiser for Heard {
        fn synthesise<'a>(&'a self, text: &'a str) -> BoxFuture<'a, CommandResult<Speech>> {
            self.0.lock().unwrap().push(text.to_string());
            Box::pin(async move {
                let mut samples: Vec<i16> =
                    (0..2400).map(|i| if i % 20 < 10 { 6000 } else { -6000 }).collect();
                samples.extend(vec![0; 12_000]);
                Ok(Speech { samples, sample_rate: 24_000 })
            })
        }
    }

    fn delivery(heard: Arc<Heard>, speed: f32) -> Delivery {
        let pronunciations = [Pronunciation::new("Nguyen", "win").unwrap()];
        Delivery { voice: heard, book: Book::new(&pronunciations), speed }
    }

    #[tokio::test]
    async fn a_voice_is_handed_the_sentence_with_the_person_s_words_marked() {
        let heard = Arc::new(Heard::default());
        let voice = delivery(heard.clone(), 1.0);
        voice.synthesise("Ask Nguyen and {Siobhan|shiv-AWN}.¶").await.unwrap();
        assert_eq!(*heard.0.lock().unwrap(), ["Ask {Nguyen|win} and {Siobhan|shiv-AWN}."]);
        // Nothing but the line end is nothing to say, and is not handed over.
        let err = voice.synthesise(" ¶").await.unwrap_err();
        assert_eq!(err.code, codes::INVALID);
        assert_eq!(heard.0.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_pause_after_is_the_sentence_s_and_shorter_when_faster() {
        let length = |speech: &Speech| speech.samples.len() as f32 / speech.sample_rate as f32;
        let heard = Arc::new(Heard::default());
        let normal = delivery(heard.clone(), 1.0);
        let sentence = length(&normal.synthesise("Done.").await.unwrap());
        let line = length(&normal.synthesise("Done.¶").await.unwrap());
        // 0.1 s of tone and the trimmed tail, then the pause: never the
        // voice's own half second.
        assert!((0.45..0.55).contains(&sentence), "{sentence}");
        assert!(line > sentence + 0.2, "{line} after a line, {sentence} after a sentence");
        let quick = length(&delivery(heard, 2.0).synthesise("Done.").await.unwrap());
        assert!(quick < sentence - 0.1, "{quick} at twice the speed, {sentence} at its own");
    }
}
