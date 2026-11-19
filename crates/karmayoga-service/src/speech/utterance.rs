//! From one stretch of speech to its words.
//!
//! The configured speech model, the one meeting notes use, handed one finished
//! utterance at a time: whatever a detector has cut from a stream, a spoken
//! command, a dictated sentence, a turn in a conversation. Two differences from
//! transcribing a recording:
//!
//! - **The detector has already cut it.** A recogniser on this computer is
//!   called directly rather than through `LocalTranscriber`, which runs voice
//!   detection over its chunk again and, over an utterance as short as "yes",
//!   can decide there was nothing in it.
//! - **There is no prompt.** A recording can tell the recogniser the names to
//!   spell; one utterance on its own has none, so nothing here is read back at
//!   the person -- but what a recogniser writes over near-silence ("Thank you.",
//!   "[Music]") still goes through [`hallucination::clean_line`], since a breath
//!   or a chair creaking is what ends up here as often as speech.
//!
//! Utterances heard in a stream are handed here by [`super::live`], one at a
//! time and in order; this module does not care where they came from.

use super::pcm;
use super::transcribe::BoxFuture;
use crate::error::CommandResult;
#[cfg(not(feature = "speech"))]
use crate::error::{CommandError, codes};
use crate::speech::transcribe::{Hints, RawSegment, SpeechChunk, Transcriber, is_faint};
use karmayoga_core::meeting::hallucination;
use karmayoga_core::speech::{SpeechAccess, TranscriberConfig};
use std::sync::Arc;

/// Shorter than this, in milliseconds, is a click, not words: nobody is asked.
const MIN_UTTERANCE_MS: u64 = 200;

/// Turns one finished utterance into text. Empty when nothing was said.
pub trait UtteranceTranscriber: Send + Sync {
    fn hear<'a>(&'a self, samples: &'a [i16]) -> BoxFuture<'a, CommandResult<String>>;
}

/// The configured speech model, as a [`UtteranceTranscriber`].
///
/// Fails when the choice cannot be built into one: a remote model without its
/// key, or a recogniser on this computer in a build without local speech.
pub fn build(access: SpeechAccess) -> CommandResult<Arc<dyn UtteranceTranscriber>> {
    let language = access.config.language.clone();
    match &access.config.transcriber {
        TranscriberConfig::Local { model } => local_utterances(*model, language),
        config => {
            let transcriber = crate::speech::transcribe::build_transcriber(config, access.key)?;
            Ok(Arc::new(RemoteUtterances { transcriber: Arc::from(transcriber), language }))
        }
    }
}

#[cfg(feature = "speech")]
fn local_utterances(
    model: karmayoga_core::speech::LocalModel,
    language: Option<String>,
) -> CommandResult<Arc<dyn UtteranceTranscriber>> {
    Ok(Arc::new(LocalUtterances { model, language }))
}

#[cfg(not(feature = "speech"))]
fn local_utterances(
    _model: karmayoga_core::speech::LocalModel,
    _language: Option<String>,
) -> CommandResult<Arc<dyn UtteranceTranscriber>> {
    Err(CommandError::new(codes::UNSUPPORTED, "this build does not include local speech"))
}

/// Whether `samples` is too short to ask anybody about.
fn too_short(samples: &[i16]) -> bool {
    pcm::duration_ms(samples.len()) < MIN_UTTERANCE_MS
}

/// Whether the turn held almost no clearly loud sound. The noise floor is the
/// turn's own: its quietest tenth, which holds the pause the detector waited
/// out at the end.
fn faint(samples: &[i16]) -> bool {
    is_faint(samples, hallucination::noise_floor(samples))
}

/// What was said, from what a recogniser returned for it: every line that
/// survives [`RawSegment::cleaned`], in order, as one line of words. Empty when
/// nothing did.
fn words(segments: &[RawSegment], quiet: bool) -> String {
    let lines: Vec<String> =
        segments.iter().filter_map(|segment| segment.cleaned("", quiet)).collect();
    lines.join(" ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A recogniser that is a service somewhere: OpenAI's, Google's or a
/// compatible server's.
struct RemoteUtterances {
    transcriber: Arc<dyn Transcriber>,
    language: Option<String>,
}

impl RemoteUtterances {
    async fn hear_remote(&self, samples: &[i16]) -> CommandResult<String> {
        if too_short(samples) {
            return Ok(String::new());
        }
        let chunk = SpeechChunk { samples: samples.to_vec(), offset_ms: 0, scope: 0 };
        let hints = Hints { language: self.language.clone(), prompt: String::new() };
        let mut segments = self.transcriber.transcribe(&chunk, &hints).await?;
        segments.sort_by_key(|segment| segment.start_ms);
        Ok(words(&segments, faint(samples)))
    }
}

impl UtteranceTranscriber for RemoteUtterances {
    fn hear<'a>(&'a self, samples: &'a [i16]) -> BoxFuture<'a, CommandResult<String>> {
        Box::pin(async move { self.hear_remote(samples).await })
    }
}

/// A recogniser on this computer: Parakeet or Whisper through sherpa-onnx.
#[cfg(feature = "speech")]
struct LocalUtterances {
    model: karmayoga_core::speech::LocalModel,
    language: Option<String>,
}

#[cfg(feature = "speech")]
impl LocalUtterances {
    async fn hear_local(&self, samples: &[i16]) -> CommandResult<String> {
        if too_short(samples) {
            return Ok(String::new());
        }
        // Loading takes seconds the first time, and recognising is real work:
        // neither belongs on the runtime.
        let recogniser = crate::speech::local::recogniser_async(self.model).await?;
        let language = self.language.clone();
        let heard = samples.to_vec();
        let text = crate::service::blocking(move || {
            Ok(recogniser.transcribe(&heard, language.as_deref()).text)
        })
        .await?;
        let segment = RawSegment {
            start_ms: 0,
            end_ms: 0,
            text,
            speaker_hint: None,
            no_speech_prob: None,
            avg_logprob: None,
        };
        Ok(words(&[segment], faint(samples)))
    }
}

#[cfg(feature = "speech")]
impl UtteranceTranscriber for LocalUtterances {
    fn hear<'a>(&'a self, samples: &'a [i16]) -> BoxFuture<'a, CommandResult<String>> {
        Box::pin(async move { self.hear_local(samples).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speech::transcribe::Limits;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn segment(text: &str, start_ms: u64) -> RawSegment {
        RawSegment {
            start_ms,
            end_ms: start_ms + 500,
            text: text.into(),
            speaker_hint: None,
            no_speech_prob: None,
            avg_logprob: None,
        }
    }

    /// A tone loud enough to be clear speech, `ms` long.
    fn loud(ms: usize) -> Vec<i16> {
        (0..ms * 16).map(|i| if (i / 20) % 2 == 0 { 8_000 } else { -8_000 }).collect()
    }

    fn silent(ms: usize) -> Vec<i16> {
        vec![0; ms * 16]
    }

    /// Says what it was told to, and what it was asked.
    struct Scripted {
        segments: Vec<RawSegment>,
        calls: AtomicUsize,
        asked: Mutex<Vec<(usize, Hints)>>,
    }

    impl Scripted {
        fn new(segments: Vec<RawSegment>) -> Arc<Self> {
            Arc::new(Self { segments, calls: AtomicUsize::new(0), asked: Mutex::new(Vec::new()) })
        }
    }

    impl Transcriber for Scripted {
        fn limits(&self) -> Limits {
            Limits { max_bytes: usize::MAX, max_seconds: u32::MAX }
        }

        fn diarises(&self) -> bool {
            false
        }

        fn transcribe<'a>(
            &'a self,
            chunk: &'a SpeechChunk,
            hints: &'a Hints,
        ) -> BoxFuture<'a, CommandResult<Vec<RawSegment>>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.asked.lock().unwrap().push((chunk.samples.len(), hints.clone()));
            let segments = self.segments.clone();
            Box::pin(async move { Ok(segments) })
        }
    }

    fn hearing(script: &Arc<Scripted>, language: Option<&str>) -> RemoteUtterances {
        RemoteUtterances { transcriber: script.clone(), language: language.map(str::to_string) }
    }

    #[tokio::test]
    async fn an_utterance_under_200_ms_is_empty_and_asks_nobody() {
        let script = Scripted::new(vec![segment("Yes", 0)]);
        let heard = hearing(&script, None).hear(&loud(150)).await.unwrap();
        assert_eq!(heard, "");
        assert_eq!(script.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn words_come_back_trimmed_on_one_line() {
        let script = Scripted::new(vec![segment("  what is\n the weather ", 0)]);
        let heard = hearing(&script, None).hear(&loud(1_000)).await.unwrap();
        assert_eq!(heard, "what is the weather");
    }

    #[tokio::test]
    async fn segments_are_joined_in_order_of_time() {
        let script = Scripted::new(vec![segment("tomorrow", 900), segment("add milk", 100)]);
        let heard = hearing(&script, None).hear(&loud(1_000)).await.unwrap();
        assert_eq!(heard, "add milk tomorrow");
    }

    #[tokio::test]
    async fn the_language_is_passed_on_and_there_is_no_prompt() {
        let script = Scripted::new(vec![segment("hola", 0)]);
        hearing(&script, Some("es")).hear(&loud(500)).await.unwrap();
        let asked = script.asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].0, 500 * 16);
        assert_eq!(asked[0].1.language.as_deref(), Some("es"));
        assert_eq!(asked[0].1.prompt, "");
    }

    #[tokio::test]
    async fn a_stock_line_over_silence_is_not_words() {
        let script = Scripted::new(vec![segment("Thank you.", 0)]);
        let heard = hearing(&script, None).hear(&silent(1_000)).await.unwrap();
        assert_eq!(heard, "");
    }

    #[tokio::test]
    async fn the_same_line_over_real_speech_is_kept() {
        let script = Scripted::new(vec![segment("Thank you.", 0)]);
        let heard = hearing(&script, None).hear(&loud(1_000)).await.unwrap();
        assert_eq!(heard, "Thank you.");
    }

    #[tokio::test]
    async fn a_noise_tag_is_dropped_whatever_the_audio() {
        let script = Scripted::new(vec![segment("[Music]", 0), segment("play it", 500)]);
        let heard = hearing(&script, None).hear(&loud(1_000)).await.unwrap();
        assert_eq!(heard, "play it");
    }

    #[tokio::test]
    async fn nothing_heard_is_empty() {
        let script = Scripted::new(Vec::new());
        assert_eq!(hearing(&script, None).hear(&loud(1_000)).await.unwrap(), "");
    }

    #[test]
    fn silence_is_faint_and_a_loud_tone_is_not() {
        assert!(faint(&silent(1_000)));
        assert!(!faint(&loud(1_000)));
    }

    #[test]
    fn the_length_cut_is_200_ms() {
        assert!(too_short(&loud(199)));
        assert!(!too_short(&loud(200)));
        assert!(too_short(&[]));
    }

    #[test]
    fn words_over_nothing_are_empty() {
        assert_eq!(words(&[], false), "");
        assert_eq!(words(&[segment("   ", 0)], false), "");
    }

    #[cfg(not(feature = "speech"))]
    #[test]
    fn a_local_recogniser_needs_the_speech_feature() {
        use karmayoga_core::speech::{LocalModel, SpeechConfig};
        let access = SpeechAccess {
            config: SpeechConfig::new(TranscriberConfig::Local { model: LocalModel::ParakeetV3 }),
            key: None,
        };
        let err = build(access).err().expect("an error");
        assert_eq!(err.code, codes::UNSUPPORTED);
    }
}
