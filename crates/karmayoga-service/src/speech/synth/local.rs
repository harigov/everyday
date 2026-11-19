//! Supertonic 3 on this computer, through sherpa-onnx.
//!
//! Everything that needs the models is behind the `speech` feature; a build
//! without it can still be configured with a local voice and is told, when it
//! tries to load one, that it cannot.

use super::Synthesiser;
use crate::error::CommandResult;
#[cfg(not(feature = "speech"))]
use crate::error::{CommandError, codes};
use std::sync::Arc;

/// The languages Supertonic 3 speaks, by their two-letter codes.
const SUPERTONIC_LANGUAGES: [&str; 31] = [
    "en", "ko", "ja", "ar", "bg", "cs", "da", "de", "el", "es", "et", "fi", "fr", "hi", "hr", "hu",
    "id", "it", "lt", "lv", "nl", "pl", "pt", "ro", "ru", "sk", "sl", "sv", "tr", "uk", "vi",
];

/// The code to tell Supertonic, from the speech model's language, which is
/// "BCP-47-ish" (`en`, `de-DE`, `pt_BR`): its first two letters, when
/// Supertonic speaks that, and English when it does not or there is none.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
fn supertonic_language(language: Option<&str>) -> &'static str {
    let code: String =
        language.unwrap_or_default().trim().chars().take(2).flat_map(char::to_lowercase).collect();
    SUPERTONIC_LANGUAGES.iter().copied().find(|l| *l == code).unwrap_or("en")
}

/// The voice, loaded, as a [`Synthesiser`]. It is loaded here so that a voice
/// that is missing is found out when speech is asked for, not on its first
/// sentence.
#[cfg(feature = "speech")]
pub(super) async fn build(
    voice: karmayoga_core::speech::LocalVoice,
    speaker: u32,
    speed: f32,
    language: Option<&str>,
) -> CommandResult<Arc<dyn Synthesiser>> {
    let synthesiser = engine::LocalSynthesiser::new(voice, speaker, speed, language);
    synthesiser.warm().await?;
    Ok(Arc::new(synthesiser))
}

#[cfg(not(feature = "speech"))]
pub(super) async fn build(
    _voice: karmayoga_core::speech::LocalVoice,
    _speaker: u32,
    _speed: f32,
    _language: Option<&str>,
) -> CommandResult<Arc<dyn Synthesiser>> {
    Err(CommandError::new(
        codes::UNSUPPORTED,
        "this copy of Karma Yoga was built without voices that run on this computer",
    ))
}

#[cfg(feature = "speech")]
mod engine {
    use super::supertonic_language;
    use crate::error::{CommandError, CommandResult, codes};
    use crate::local::files as models;
    use crate::local::resident::Resident;
    use crate::speech::pcm::sample_to_i16;
    use crate::speech::synth::{Speech, Synthesiser, nothing_to_say};
    use crate::speech::transcribe::BoxFuture;
    use karmayoga_core::speech::LocalVoice;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Cap on worker threads handed to the model, for the reasons
    /// `speech::local` caps its own: one voice is one person's
    /// conversation, and should not take every core there is.
    const MAX_THREADS: usize = 4;

    /// A voice that is loaded.
    ///
    /// The `Mutex` is for what `speech::local::Recogniser`'s is: sherpa-onnx
    /// promises an object may be used from one thread at a time, and a
    /// conversation can have a sentence being made while the next is asked
    /// for.
    struct Engine {
        tts: Mutex<sherpa_onnx::OfflineTts>,
    }

    static VOICES: Resident<LocalVoice, Engine> = Resident::new("voice");

    fn path_string(path: &std::path::Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn load(voice: LocalVoice) -> CommandResult<Engine> {
        let paths = models::voice_paths(voice).ok_or_else(|| {
            CommandError::new(
                codes::NOT_FOUND,
                "the voice on this computer is not downloaded yet; download it in Settings → Models",
            )
        })?;
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .clamp(1, MAX_THREADS);

        let mut config = sherpa_onnx::OfflineTtsConfig::default();
        config.model.supertonic = sherpa_onnx::OfflineTtsSupertonicModelConfig {
            duration_predictor: Some(path_string(&paths.duration_predictor)),
            text_encoder: Some(path_string(&paths.text_encoder)),
            vector_estimator: Some(path_string(&paths.vector_estimator)),
            vocoder: Some(path_string(&paths.vocoder)),
            tts_json: Some(path_string(&paths.tts_json)),
            unicode_indexer: Some(path_string(&paths.unicode_indexer)),
            voice_style: Some(path_string(&paths.voice_style)),
        };
        config.model.num_threads = threads as i32;
        config.model.provider = Some("cpu".to_string());
        config.max_num_sentences = 1;
        config.silence_scale = 0.2;

        let tts = sherpa_onnx::OfflineTts::create(&config).ok_or_else(|| {
            CommandError::new(codes::INTERNAL, "could not load the voice on this computer")
        })?;
        Ok(Engine { tts: Mutex::new(tts) })
    }

    #[derive(Clone, Copy)]
    pub struct LocalSynthesiser {
        voice: LocalVoice,
        speaker: u32,
        speed: f32,
        language: &'static str,
    }

    impl LocalSynthesiser {
        pub fn new(voice: LocalVoice, speaker: u32, speed: f32, language: Option<&str>) -> Self {
            Self { voice, speaker, speed, language: supertonic_language(language) }
        }

        /// Load the voice now, if it is not loaded already. It is not held
        /// on to: that is the [`Resident`]'s to do, and to let go of.
        pub async fn warm(&self) -> CommandResult<()> {
            let voice = self.voice;
            crate::service::blocking(move || VOICES.get(voice, || load(voice)).map(|_| ())).await
        }

        /// Blocking, as is everything that runs the model.
        fn speak(&self, text: &str) -> CommandResult<Speech> {
            // sherpa-onnx hands the words to C as a string, and panics on one
            // with a NUL inside it.
            let text = text.replace('\0', " ");
            if text.trim().is_empty() {
                return Err(nothing_to_say());
            }
            let engine = VOICES.get(self.voice, || load(self.voice))?;
            let config = sherpa_onnx::GenerationConfig {
                sid: self.speaker as i32,
                speed: self.speed,
                extra: Some(HashMap::from([(
                    "lang".to_string(),
                    serde_json::Value::String(self.language.to_string()),
                )])),
                ..Default::default()
            };
            let tts = engine.tts.lock().unwrap_or_else(|e| e.into_inner());
            let audio = tts
                .generate_with_config(&text, &config, None::<fn(&[f32], f32) -> bool>)
                .ok_or_else(|| {
                CommandError::new(codes::INTERNAL, "the voice on this computer could not say that")
            })?;
            let sample_rate = u32::try_from(audio.sample_rate()).unwrap_or(0);
            let samples: Vec<i16> = audio.samples().iter().map(|&s| sample_to_i16(s)).collect();
            if samples.is_empty() || sample_rate == 0 {
                return Err(CommandError::new(
                    codes::INTERNAL,
                    "the voice on this computer made no sound",
                ));
            }
            Ok(Speech { samples, sample_rate })
        }
    }

    impl Synthesiser for LocalSynthesiser {
        fn synthesise<'a>(&'a self, text: &'a str) -> BoxFuture<'a, CommandResult<Speech>> {
            let this = *self;
            let text = text.to_string();
            Box::pin(async move { crate::service::blocking(move || this.speak(&text)).await })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_voice_is_told_the_first_two_letters_when_it_speaks_them() {
        assert_eq!(supertonic_language(Some("de")), "de");
        assert_eq!(supertonic_language(Some("de-DE")), "de");
        assert_eq!(supertonic_language(Some("PT_br")), "pt");
        assert_eq!(supertonic_language(Some(" ja ")), "ja");
        assert_eq!(supertonic_language(Some("vi")), "vi");
    }

    #[test]
    fn anything_else_is_english() {
        assert_eq!(supertonic_language(None), "en");
        assert_eq!(supertonic_language(Some("")), "en");
        assert_eq!(supertonic_language(Some("zh")), "en");
        assert_eq!(supertonic_language(Some("x")), "en");
        assert_eq!(supertonic_language(Some("é")), "en");
    }
}
