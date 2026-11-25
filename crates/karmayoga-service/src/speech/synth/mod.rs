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
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tokio::time::Instant;

/// A stretch of synthesised speech, mono.
#[derive(Debug, Clone, PartialEq)]
pub struct Speech {
    pub samples: Vec<i16>,
    pub sample_rate: u32,
}

/// Turns words into speech.
///
/// A voice says what it is handed in one go, up to
/// [`MAX_CLIP`](crate::speech::text::MAX_CLIP) characters: where speech is
/// cut is the [narrator](crate::speech::narrate)'s to decide, since a voice
/// starts afresh wherever it is cut -- the same pitch and the same fall, like
/// a list being read out -- and the narrator hands over as much at once as
/// there is time to make. A voice whose engine cuts text into pieces of its
/// own is told not to below that: Phonon's `ptts` makes pieces of about one
/// long sentence unless told otherwise, and Kokoro's run of phonemes is cut
/// only past what a clip comes to.
pub trait Synthesiser: Send + Sync {
    fn synthesise<'a>(&'a self, text: &'a str) -> BoxFuture<'a, CommandResult<Speech>>;

    /// How many characters a second this voice has been making into speech
    /// on this computer, once it has been timed: what the narrator sizes a
    /// clip by. A voice from [`build_synthesiser`] times itself; any other
    /// answers `None`, and one that wraps another answers for it.
    fn rate(&self) -> Option<f64> {
        None
    }
}

/// The fewest characters a clip must have to be timed: a phrase of a few
/// words is mostly what it costs to ask a voice for anything at all, and
/// would make the voice look slower than it is at a sentence.
const MIN_TIMED: usize = 30;

/// The fastest a voice is believed to be, in characters a second, however
/// fast a clip came back: a clock too coarse to time it would have it making
/// speech in no time.
const MAX_RATE: f64 = 1000.0;

/// How many of the clips timed last a voice's rate is read from.
const TIMED: usize = 5;

/// How fast a voice has been making speech: characters made a second.
///
/// The middle of the last [`TIMED`] clips timed, the slower of the two middles
/// when there are an even number. A clip that took longer than it should --
/// the model loaded again after a rest, a wait behind other speech, a retry
/// over the network -- moves nothing, and neither does one that came back
/// quicker; a computer that has become busy shows within three clips, which
/// the [narrator](crate::speech::narrate)'s margin is there to cover.
#[derive(Debug, Clone, Default, PartialEq)]
struct Rate(VecDeque<f64>);

impl Rate {
    fn timed(&mut self, chars: usize, took: Duration) {
        let rate = (chars as f64 / took.as_secs_f64().max(f64::MIN_POSITIVE)).min(MAX_RATE);
        if self.0.len() == TIMED {
            self.0.pop_front();
        }
        self.0.push_back(rate);
    }

    fn rate(&self) -> Option<f64> {
        let mut rates: Vec<f64> = self.0.iter().copied().collect();
        rates.sort_by(f64::total_cmp);
        rates.get(rates.len().saturating_sub(1) / 2).copied()
    }
}

/// How fast each voice has been. Every use of a voice in this process -- a
/// voice session, Listen at its own speed, a podcast's hosts -- is built
/// afresh, and shares this, so each starts knowing. Kept by engine rather
/// than by speaker or speed: what a clip costs is the model's.
static RATES: LazyLock<Mutex<HashMap<String, Arc<Mutex<Rate>>>>> = LazyLock::new(Mutex::default);

fn rate_of(engine: String) -> Arc<Mutex<Rate>> {
    RATES.lock().unwrap_or_else(|e| e.into_inner()).entry(engine).or_default().clone()
}

/// Which engine `config` speaks with, as far as how fast it is goes. No key
/// is in it.
fn engine_of(config: &SynthesiserConfig) -> String {
    match config {
        SynthesiserConfig::Local { voice, .. } => format!("{voice:?}"),
        SynthesiserConfig::OpenAi { model, .. } => format!("OpenAI {model}"),
        SynthesiserConfig::Compatible { base_url, model, .. } => format!("{base_url} {model}"),
    }
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
    let rate = rate_of(engine_of(&config.synthesiser));
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
    Ok(Arc::new(Delivery { voice, book, speed, rate }))
}

/// A voice as everything that talks uses it.
///
/// Hands the voice each sentence with the person's pronunciations marked in
/// it and the line-end mark taken off, and hands back the audio with the
/// voice's own silence round it replaced by the pause the sentence's ending
/// asks for -- shorter at a faster speed, as everything else is. And times
/// the voice ([`Synthesiser::rate`]).
struct Delivery {
    voice: Arc<dyn Synthesiser>,
    book: Book,
    speed: f32,
    rate: Arc<Mutex<Rate>>,
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
            let started = Instant::now();
            let speech = self.voice.synthesise(&self.book.mark(&words)).await?;
            let chars = words.chars().count();
            if chars >= MIN_TIMED {
                self.rate.lock().unwrap_or_else(|e| e.into_inner()).timed(chars, started.elapsed());
            }
            Ok(pace::paced(speech, pause))
        })
    }

    fn rate(&self) -> Option<f64> {
        self.rate.lock().unwrap_or_else(|e| e.into_inner()).rate()
    }
}

/// What every voice answers to words with nothing in them.
fn nothing_to_say() -> CommandError {
    CommandError::new(codes::INVALID, "there is nothing to say")
}

#[cfg(test)]
mod tests {
    use super::*;
    use karmayoga_core::speech::{LocalVoice, Pronunciation};

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
        Delivery { voice: heard, book: Book::new(&pronunciations), speed, rate: Arc::default() }
    }

    /// A voice that takes a hundredth of a second over each character.
    struct Slow;

    impl Synthesiser for Slow {
        fn synthesise<'a>(&'a self, text: &'a str) -> BoxFuture<'a, CommandResult<Speech>> {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(10) * text.chars().count() as u32).await;
                Ok(Speech { samples: vec![1000; 2400], sample_rate: 24_000 })
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_voice_is_timed_by_the_sentences_it_makes_and_not_by_a_few_words() {
        let voice = Delivery {
            voice: Arc::new(Slow),
            book: Book::new(&[]),
            speed: 1.0,
            rate: Arc::default(),
        };
        assert_eq!(voice.rate(), None);
        voice.synthesise("Okay, one sec.").await.unwrap();
        assert_eq!(voice.rate(), None, "a phrase is mostly the cost of asking");
        voice.synthesise("You have the dentist at nine in the morning.").await.unwrap();
        assert_eq!(voice.rate(), Some(100.0));
    }

    #[test]
    fn a_rate_is_the_middle_of_the_last_clips_so_one_slow_clip_moves_nothing() {
        let second = Duration::from_secs(1);
        let mut rate = Rate::default();
        assert_eq!(rate.rate(), None);
        rate.timed(100, second);
        assert_eq!(rate.rate(), Some(100.0), "the first clip timed is believed");
        rate.timed(140, second);
        assert_eq!(rate.rate(), Some(100.0), "the slower of two");
        rate.timed(130, second);
        rate.timed(135, second);
        rate.timed(10, second);
        assert_eq!(rate.rate(), Some(130.0), "a model loaded again moves nothing");
        rate.timed(40, second);
        rate.timed(40, second);
        assert_eq!(rate.rate(), Some(40.0), "a computer that has become busy shows");

        let mut instant = Rate::default();
        instant.timed(40, Duration::ZERO);
        assert_eq!(instant.rate(), Some(MAX_RATE), "made in no time");
    }

    #[test]
    fn every_use_of_an_engine_shares_how_fast_it_is_whatever_the_speaker() {
        let phonon = |speaker| SynthesiserConfig::Local { voice: LocalVoice::Phonon, speaker };
        let one = rate_of(engine_of(&phonon(0)));
        assert!(Arc::ptr_eq(&one, &rate_of(engine_of(&phonon(5)))));
        let kokoro = SynthesiserConfig::Local { voice: LocalVoice::Kokoro, speaker: 0 };
        assert!(!Arc::ptr_eq(&one, &rate_of(engine_of(&kokoro))));
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
