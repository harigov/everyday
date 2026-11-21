//! Getting at speech from anywhere in the application.
//!
//! What is chosen in Settings → Models, built and ready to use, or a sentence
//! saying what is missing and where to put it right. Nothing that listens or
//! talks needs to know which recogniser or voice that is, whether it is on this
//! computer or somebody's server, or where its key is kept.
//!
//! Each method refuses when the vault is locked, when nothing is chosen (or the
//! choice lacks its key), and when a model that has to be on this computer has
//! not been downloaded. Loading what is there -- the detector, a local voice
//! -- can take seconds, so it happens here, on the blocking pool, and what is
//! returned is ready to use. Two things carry on in the background after it
//! is returned, because nobody needs them until the person has said or been
//! told something: a local recogniser loading for [`Service::live_transcription`],
//! and a local voice getting its speaker ready. Whatever needs either before
//! it is done waits for it.

use super::detect::{Tuning, build_detector};
use super::live::LiveTranscription;
use super::synth::{Synthesiser, build_synthesiser_with};
use super::utterance::{self, UtteranceTranscriber};
use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};
use karmayoga_core::speech::{SpeechConfig, TranscriberConfig, VoiceConfig};
use std::sync::Arc;

const FIX: &str = "Download it in Settings → Models.";

/// The sentence for a speech kit that is not on this computer.
///
/// The kit is needed to listen whichever recogniser is chosen -- it holds the
/// voice detector -- so a remote recogniser does not excuse it. The booleans in
/// these three checks are what `local::files` said, passed in so that they can
/// be tested without anybody's disk.
fn missing_kit(kit_installed: bool) -> Option<String> {
    (!kit_installed).then(|| {
        format!("Listening needs the speech kit, which has not been downloaded yet. {FIX}")
    })
}

/// The sentence for a recogniser that has to be on this computer and is not.
fn missing_recogniser(speech: &SpeechConfig, recogniser_installed: bool) -> Option<String> {
    if let TranscriberConfig::Local { model } = &speech.transcriber
        && !recogniser_installed
    {
        return Some(format!(
            "The speech model {} has not been downloaded yet. {FIX}",
            model.as_str()
        ));
    }
    None
}

/// The sentence for a voice that has to be on this computer and is not.
fn missing_voice(voice: &VoiceConfig, voice_installed: bool) -> Option<String> {
    if let Some(local) = voice.synthesiser.local_voice()
        && !voice_installed
    {
        return Some(format!("The voice, {}, has not been downloaded yet. {FIX}", local.id()));
    }
    None
}

impl Service {
    /// The chosen speech model, ready to turn utterances into words.
    pub async fn utterance_transcriber(
        self: &Arc<Self>,
    ) -> CommandResult<Arc<dyn UtteranceTranscriber>> {
        let vault = self.require_unlocked()?;
        let speech = blocking(move || Ok(vault.speech_access()?)).await?;
        let recogniser = speech.config.transcriber.local_model();
        if let Some(refusal) = missing_recogniser(
            &speech.config,
            recogniser.is_none_or(crate::local::files::installed),
        ) {
            return Err(CommandError::new(codes::INVALID, refusal));
        }
        blocking(move || utterance::build(speech)).await
    }

    /// The chosen voice, ready to speak.
    pub async fn speech_synthesiser(self: &Arc<Self>) -> CommandResult<Arc<dyn Synthesiser>> {
        self.speech_synthesiser_at(1.0).await
    }

    /// [`Self::speech_synthesiser`] at `multiple` times the speed chosen in
    /// Settings, for a listener who wants it faster or slower.
    pub async fn speech_synthesiser_at(
        self: &Arc<Self>,
        multiple: f32,
    ) -> CommandResult<Arc<dyn Synthesiser>> {
        let vault = self.require_unlocked()?;
        let mut voice = blocking(move || Ok(vault.voice_access()?)).await?;
        let installed =
            voice.config.synthesiser.local_voice().is_none_or(crate::local::files::voice_installed);
        if let Some(refusal) = missing_voice(&voice.config, installed) {
            return Err(CommandError::new(codes::INVALID, refusal));
        }
        if multiple.is_finite() && multiple > 0.0 {
            voice.config.speed *= multiple;
        }
        let book = self.pronunciations();
        book.set(&voice.pronunciations);
        build_synthesiser_with(voice, book).await
    }

    /// Words from a stream of audio, as they are said: the chosen speech model
    /// behind a detector that is tuned for `tuning`.
    ///
    /// Needs the speech kit as well as the recogniser. Starts on the current
    /// Tokio runtime. A recogniser on this computer may still be loading when
    /// this returns; the first utterance waits for it.
    pub async fn live_transcription(
        self: &Arc<Self>,
        tuning: Tuning,
    ) -> CommandResult<LiveTranscription> {
        self.require_unlocked()?;
        if let Some(refusal) = missing_kit(crate::local::files::speech_kit_installed()) {
            return Err(CommandError::new(codes::INVALID, refusal));
        }
        let transcriber = self.utterance_transcriber().await?;
        let detector = blocking(move || build_detector(tuning)).await?;
        // Loading a recogniser on this computer takes seconds -- 2.4 on a
        // quiet 12-core desktop, 6.7 on a busy one -- and used to begin with
        // the first thing heard, so voice mode's first reply and voice
        // typing's first words waited for all of it. Begun now, once nothing
        // here can refuse any more, it loads while the person is still
        // talking, and the first utterance waits only for what is left.
        transcriber.warm();
        Ok(LiveTranscription::start(detector, transcriber))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use karmayoga_core::speech::{LocalModel, LocalVoice, SynthesiserConfig};

    fn local_speech() -> SpeechConfig {
        SpeechConfig::new(TranscriberConfig::Local { model: LocalModel::ParakeetV3 })
    }

    fn remote_speech() -> SpeechConfig {
        SpeechConfig::new(TranscriberConfig::OpenAi { model: "gpt-4o-transcribe".into() })
    }

    fn local_voice() -> VoiceConfig {
        VoiceConfig::new(SynthesiserConfig::Local { voice: LocalVoice::Phonon, speaker: 0 })
    }

    fn remote_voice() -> VoiceConfig {
        VoiceConfig::new(SynthesiserConfig::OpenAi { model: "tts-1".into(), voice: "alloy".into() })
    }

    #[test]
    fn everything_downloaded_is_nothing_missing() {
        assert_eq!(missing_kit(true), None);
        assert_eq!(missing_recogniser(&local_speech(), true), None);
        assert_eq!(missing_voice(&local_voice(), true), None);
    }

    #[test]
    fn a_missing_speech_kit_is_a_sentence_saying_where_to_get_it() {
        let said = missing_kit(false).unwrap();
        assert!(said.contains("speech kit"), "{said}");
        assert!(said.contains("Settings → Models"), "{said}");
    }

    #[test]
    fn a_remote_recogniser_and_voice_need_nothing_downloaded() {
        assert_eq!(missing_recogniser(&remote_speech(), false), None);
        assert_eq!(missing_voice(&remote_voice(), false), None);
    }

    #[test]
    fn a_local_recogniser_must_be_downloaded() {
        let said = missing_recogniser(&local_speech(), false).unwrap();
        assert!(said.contains("parakeet"), "{said}");
        assert!(said.contains("Settings → Models"), "{said}");
    }

    #[test]
    fn a_local_voice_must_be_downloaded() {
        let said = missing_voice(&local_voice(), false).unwrap();
        assert!(said.contains("voice"), "{said}");
        assert!(said.contains("phonon"), "{said}");
        assert!(said.contains("Settings → Models"), "{said}");
    }
}
