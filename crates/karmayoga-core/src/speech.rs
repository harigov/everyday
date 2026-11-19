//! How speech becomes text: the speech model, the fifth card in Settings →
//! Models.
//!
//! Application configuration rather than a feature's, for the reason
//! [`crate::llm`] gives for the language models. Meeting notes are the first
//! thing in this app that listens, and they will not be the last. The choice
//! lived in their own settings while they were the only listener, with a key
//! of its own beside it and a switch for borrowing the models' OpenAI key
//! instead of pasting a second copy. Left there, every later listener --
//! dictating into the journal, a voice memo -- would grow another copy of the
//! same choice and the same key, and somebody moving from OpenAI to a
//! recogniser on this computer would have to do it once per feature. Here it
//! is chosen once, and anything that turns speech into text asks
//! [`Vault::speech_access`](crate::Vault::speech_access) for it.
//!
//! # Its key belongs to an endpoint too
//!
//! [`TranscriberConfig::endpoint`] names where the audio goes, spelt exactly
//! as [`LLMProviderConfig::endpoint`] spells a language model's address --
//! OpenAI's is `https://api.openai.com/v1` in both -- and its key is filed in
//! the same sealed map under that string ([`crate::llm::MODEL_KEYS_OWNER_KIND`]).
//! So OpenAI speech and an OpenAI language model share one key without a
//! switch having to say so, which is what "use the OpenAI key from Models"
//! did by hand; and a speech model moved to somebody else's server finds no
//! key waiting there, exactly as a tier does. Google's Gemini API is an
//! address of its own ([`GOOGLE_ENDPOINT`]), a compatible server is its base
//! URL, and a recogniser on this computer has none.
//!
//! # Not in a mail acknowledgement
//!
//! An account's acknowledgement names every place its mail may go
//! ([`crate::llm::ModelSettings::acknowledgement_name`]). Speech carries a
//! call's audio and never mail, so its endpoint is left out of that list --
//! adding it would have cleared every account's tick the day somebody chose
//! a transcriber, for a destination no mail goes to -- and it is counted only
//! where a key may be kept for it: [`crate::llm::ModelSettings::all_endpoints`].
//!
//! # Carried over once
//!
//! A vault from before this kept the choice in
//! [`crate::meeting::MeetingSettings`] and its key under a secret of its own.
//! Both are moved at unlock, by `Vault::carry_over_speech`, and
//! [`crate::meeting::MeetingSettings::speech_moved`] records that they were.

use crate::error::{Error, Result};
use crate::llm::{LLMProviderConfig, Provider, is_loopback};
use serde::{Deserialize, Serialize};

/// Where a Google speech model's audio is sent: the Gemini API, which
/// `karmayoga_service::speech::transcribe::gemini` posts to -- and so what
/// its key is filed under.
pub const GOOGLE_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta";

/// The local recognisers on offer. See `karmayoga_service::local::files`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum LocalModel {
    /// Parakeet TDT 0.6B v3, int8: English and European languages, fast.
    ParakeetV3,
    /// Whisper large-v3-turbo: any language, slower.
    WhisperTurbo,
}

impl LocalModel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LocalModel::ParakeetV3 => "parakeet-tdt-0.6b-v3",
            LocalModel::WhisperTurbo => "whisper-large-v3-turbo",
        }
    }
}

/// Who turns speech into text. There is deliberately no default: nothing
/// that listens can be switched on until somebody has chosen one of these
/// and made it usable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TranscriberConfig {
    Local {
        model: LocalModel,
    },
    OpenAi {
        model: String,
    },
    Google {
        model: String,
    },
    /// Anything that speaks OpenAI's `/v1/audio/transcriptions`.
    Compatible {
        base_url: String,
        model: String,
    },
}

impl TranscriberConfig {
    /// Where the audio goes, trailing slashes off -- and what its key is
    /// filed under. `None` for a recogniser on this computer, which is sent
    /// nothing.
    ///
    /// OpenAI's is [`Provider::default_base_url`] itself, not a copy of the
    /// string, so OpenAI speech and an OpenAI language model can never drift
    /// apart into two keys; and the request is sent to the same address --
    /// `karmayoga_service::speech::transcribe::build_transcriber` has a test
    /// holding the two to one spelling, as Google's -- so a key cannot be
    /// filed under one host and sent to another. A compatible server's is
    /// trimmed the way [`LLMProviderConfig::endpoint`] trims a base URL, so
    /// one typed both ways finds one key -- and a compatible server that is
    /// also somebody's language model gateway shares that model's.
    pub fn endpoint(&self) -> Option<&str> {
        match self {
            TranscriberConfig::Local { .. } => None,
            TranscriberConfig::OpenAi { .. } => Some(Provider::OpenAi.default_base_url()),
            TranscriberConfig::Google { .. } => Some(GOOGLE_ENDPOINT),
            TranscriberConfig::Compatible { base_url, .. } => {
                Some(base_url.trim_end_matches('/')).filter(|u| !u.is_empty())
            }
        }
    }

    /// Whether this backend needs a key. A loopback compatible server does
    /// not, the same rule `Provider::needs_key` applies to the assistant.
    pub fn needs_key(&self) -> bool {
        match self {
            TranscriberConfig::Local { .. } => false,
            TranscriberConfig::OpenAi { .. } | TranscriberConfig::Google { .. } => true,
            TranscriberConfig::Compatible { base_url, .. } => !is_loopback(base_url),
        }
    }

    /// "local:…", "openai:…" -- what a transcript records as its backend.
    pub fn label(&self) -> String {
        match self {
            TranscriberConfig::Local { model } => format!("local:{}", model.as_str()),
            TranscriberConfig::OpenAi { model } => format!("openai:{model}"),
            TranscriberConfig::Google { model } => format!("google:{model}"),
            TranscriberConfig::Compatible { model, .. } => format!("compatible:{model}"),
        }
    }

    /// Whether audio leaves the machine.
    pub fn is_remote(&self) -> bool {
        match self {
            TranscriberConfig::Local { .. } => false,
            TranscriberConfig::Compatible { base_url, .. } => !is_loopback(base_url),
            _ => true,
        }
    }

    /// Which recogniser on this computer, when it is one.
    pub fn local_model(&self) -> Option<LocalModel> {
        match self {
            TranscriberConfig::Local { model } => Some(*model),
            _ => None,
        }
    }

    /// Reject a choice no request could be built from.
    ///
    /// A recogniser on this computer always passes: [`LocalModel`] is an
    /// enum, so one that parsed is one this build knows, and whether it has
    /// been downloaded yet is not a reason to refuse it -- somebody chooses a
    /// recogniser first and downloads it after, on the same card. A provider
    /// needs a model name, as a language model does. A compatible server
    /// needs an address, and the same address rules a language model's base
    /// URL has: http(s), one address, and no plain http anywhere but this
    /// machine, since a key would cross the network in the clear.
    pub fn validate(&self) -> Result<()> {
        let model = match self {
            TranscriberConfig::Local { .. } => return Ok(()),
            TranscriberConfig::OpenAi { model } | TranscriberConfig::Google { model } => model,
            TranscriberConfig::Compatible { base_url, model } => {
                if base_url.trim().is_empty() {
                    return Err(Error::Invalid("a compatible server needs a base URL".into()));
                }
                LLMProviderConfig { provider: Provider::OpenAi, base_url: Some(base_url.clone()) }
                    .validate()?;
                model
            }
        };
        if model.trim().is_empty() {
            return Err(Error::Invalid("a model name is required".into()));
        }
        Ok(())
    }
}

/// The speech model: who turns speech into text, and in which language.
///
/// Its own record rather than a bare [`TranscriberConfig`] because the
/// language is part of the same decision. It is told to the recogniser, and
/// whether Parakeet (European languages) or Whisper (any) is the right
/// recogniser depends on it -- so it travels with the recogniser rather than
/// with whichever feature happens to be listening.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechConfig {
    pub transcriber: TranscriberConfig,
    /// BCP-47-ish language code, or `None` to let the recogniser detect it.
    #[serde(default)]
    pub language: Option<String>,
}

impl SpeechConfig {
    pub fn new(transcriber: TranscriberConfig) -> Self {
        Self { transcriber, language: None }
    }

    /// See [`TranscriberConfig::endpoint`].
    pub fn endpoint(&self) -> Option<&str> {
        self.transcriber.endpoint()
    }

    pub fn needs_key(&self) -> bool {
        self.transcriber.needs_key()
    }

    pub fn is_remote(&self) -> bool {
        self.transcriber.is_remote()
    }

    pub fn validate(&self) -> Result<()> {
        self.transcriber.validate()
    }
}

/// The speech model ready to be used: what
/// [`Vault::speech_access`](crate::Vault::speech_access) hands the one layer
/// that builds a request, [`crate::llm::ModelAccess`]'s counterpart.
///
/// No acknowledgement beside it, as a language model's has: speech carries no
/// mail -- see the module docs. Holds the key, so it does not derive
/// `Debug`; the manual impl says whether there is one and nothing else.
#[derive(Clone)]
pub struct SpeechAccess {
    pub config: SpeechConfig,
    /// `None` only where none is needed -- a recogniser on this computer, or
    /// a compatible server on this machine.
    pub key: Option<String>,
}

impl std::fmt::Debug for SpeechAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpeechAccess")
            .field("config", &self.config)
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// A voice that runs on this computer. See
/// `karmayoga_service::local::files` for its download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum LocalVoice {
    /// Supertonic 3, int8: 31 languages, ten speakers, quick on a CPU.
    Supertonic3,
}

impl LocalVoice {
    /// What it is called in the model catalogue and on the wire.
    pub fn id(self) -> &'static str {
        match self {
            LocalVoice::Supertonic3 => "supertonic3",
        }
    }

    /// How many speakers it has; a [`SynthesiserConfig::Local`]'s `speaker`
    /// is below this.
    pub fn speakers(self) -> u32 {
        match self {
            LocalVoice::Supertonic3 => 10,
        }
    }
}

/// Who turns the assistant's words into speech: this computer, OpenAI, or
/// anything that speaks OpenAI's `/v1/audio/speech` -- which is how an
/// open-source voice run as a server of its own (Kokoro-FastAPI, Speaches,
/// LocalAI and the like) is reached.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SynthesiserConfig {
    Local {
        voice: LocalVoice,
        #[serde(default)]
        speaker: u32,
    },
    OpenAi {
        model: String,
        voice: String,
    },
    Compatible {
        base_url: String,
        model: String,
        voice: String,
    },
}

impl SynthesiserConfig {
    /// Where the assistant's words go, and what the key is filed under: the
    /// same spelling rules as [`TranscriberConfig::endpoint`], so OpenAI's
    /// voice, speech model and language models share one key.
    pub fn endpoint(&self) -> Option<&str> {
        match self {
            SynthesiserConfig::Local { .. } => None,
            SynthesiserConfig::OpenAi { .. } => Some(Provider::OpenAi.default_base_url()),
            SynthesiserConfig::Compatible { base_url, .. } => {
                Some(base_url.trim_end_matches('/')).filter(|u| !u.is_empty())
            }
        }
    }

    /// Whether this backend needs a key: not on this computer, and not from
    /// a compatible server on this machine.
    pub fn needs_key(&self) -> bool {
        match self {
            SynthesiserConfig::Local { .. } => false,
            SynthesiserConfig::OpenAi { .. } => true,
            SynthesiserConfig::Compatible { base_url, .. } => !is_loopback(base_url),
        }
    }

    /// Whether the assistant's words leave the machine.
    pub fn is_remote(&self) -> bool {
        match self {
            SynthesiserConfig::Local { .. } => false,
            SynthesiserConfig::OpenAi { .. } => true,
            SynthesiserConfig::Compatible { base_url, .. } => !is_loopback(base_url),
        }
    }

    /// Which voice on this computer, when it is one.
    pub fn local_voice(&self) -> Option<LocalVoice> {
        match self {
            SynthesiserConfig::Local { voice, .. } => Some(*voice),
            _ => None,
        }
    }

    /// Reject a choice no request could be built from. As for
    /// [`TranscriberConfig::validate`], a local voice that has not been
    /// downloaded yet is not a reason to refuse it.
    pub fn validate(&self) -> Result<()> {
        let (model, voice) = match self {
            SynthesiserConfig::Local { voice, speaker } => {
                if *speaker >= voice.speakers() {
                    return Err(Error::Invalid(format!(
                        "this voice has {} speakers",
                        voice.speakers()
                    )));
                }
                return Ok(());
            }
            SynthesiserConfig::OpenAi { model, voice } => (model, voice),
            SynthesiserConfig::Compatible { base_url, model, voice } => {
                if base_url.trim().is_empty() {
                    return Err(Error::Invalid("a compatible server needs a base URL".into()));
                }
                LLMProviderConfig { provider: Provider::OpenAi, base_url: Some(base_url.clone()) }
                    .validate()?;
                (model, voice)
            }
        };
        if model.trim().is_empty() {
            return Err(Error::Invalid("a model name is required".into()));
        }
        if voice.trim().is_empty() {
            return Err(Error::Invalid("a voice name is required".into()));
        }
        Ok(())
    }
}

fn normal_speed() -> f32 {
    1.0
}

/// The assistant's voice: the sixth card in Settings → Models.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceConfig {
    pub synthesiser: SynthesiserConfig,
    /// How fast it talks, where 1 is the voice's own pace.
    #[serde(default = "normal_speed")]
    pub speed: f32,
}

impl VoiceConfig {
    pub fn new(synthesiser: SynthesiserConfig) -> Self {
        Self { synthesiser, speed: normal_speed() }
    }

    /// See [`SynthesiserConfig::endpoint`].
    pub fn endpoint(&self) -> Option<&str> {
        self.synthesiser.endpoint()
    }

    pub fn needs_key(&self) -> bool {
        self.synthesiser.needs_key()
    }

    pub fn validate(&self) -> Result<()> {
        if !(0.5..=2.0).contains(&self.speed) {
            return Err(Error::Invalid("the speed must be between 0.5 and 2".into()));
        }
        self.synthesiser.validate()
    }
}

/// The voice ready to be used, [`SpeechAccess`]'s counterpart.
#[derive(Clone)]
pub struct VoiceAccess {
    pub config: VoiceConfig,
    /// `None` only where none is needed.
    pub key: Option<String>,
}

impl std::fmt::Debug for VoiceAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceAccess")
            .field("config", &self.config)
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compatible(base_url: &str, model: &str) -> TranscriberConfig {
        TranscriberConfig::Compatible { base_url: base_url.into(), model: model.into() }
    }

    /// The point of the endpoint: OpenAI speech files its key where an
    /// OpenAI language model does, so the two are one key.
    #[test]
    fn openai_speech_is_at_the_endpoint_an_openai_model_is() {
        let speech = TranscriberConfig::OpenAi { model: "gpt-4o-transcribe".into() };
        assert_eq!(speech.endpoint(), Some(LLMProviderConfig::default().endpoint()));
        assert_eq!(speech.endpoint(), Some("https://api.openai.com/v1"));
    }

    #[test]
    fn every_other_backend_has_an_endpoint_of_its_own_or_none() {
        let google = TranscriberConfig::Google { model: "gemini-2.5-flash".into() };
        assert_eq!(google.endpoint(), Some(GOOGLE_ENDPOINT));
        let server = compatible("http://localhost:8000/v1/", "whisper-1");
        assert_eq!(server.endpoint(), Some("http://localhost:8000/v1"));
        assert!(!server.needs_key(), "a server on this machine is sent no key");
        assert_eq!(compatible("", "whisper-1").endpoint(), None);
        let local = TranscriberConfig::Local { model: LocalModel::ParakeetV3 };
        assert_eq!(local.endpoint(), None);
        assert!(!local.needs_key());
    }

    #[test]
    fn a_choice_with_nothing_to_send_to_is_refused() {
        assert!(TranscriberConfig::OpenAi { model: " ".into() }.validate().is_err());
        assert!(TranscriberConfig::Google { model: String::new() }.validate().is_err());
        assert!(compatible("", "whisper-1").validate().is_err());
        assert!(compatible("http://localhost:8000/v1", "").validate().is_err());
        assert!(compatible("transcribe.example.com", "whisper-1").validate().is_err());
        let err = compatible("http://transcribe.example.com", "whisper-1").validate().unwrap_err();
        assert!(err.to_string().contains("unencrypted"), "{err}");

        assert!(compatible("http://127.0.0.1:8000/v1", "whisper-1").validate().is_ok());
        assert!(compatible("https://transcribe.example.com", "whisper-1").validate().is_ok());
        assert!(TranscriberConfig::OpenAi { model: "gpt-4o-transcribe".into() }.validate().is_ok());
        // Not downloaded yet is not a reason to refuse: it is chosen first.
        assert!(TranscriberConfig::Local { model: LocalModel::WhisperTurbo }.validate().is_ok());
    }

    #[test]
    fn it_travels_as_the_interface_spells_it() {
        let speech = SpeechConfig {
            transcriber: compatible("http://localhost:8000/v1", "whisper-1"),
            language: Some("en".into()),
        };
        let json = serde_json::to_value(&speech).unwrap();
        assert_eq!(json["transcriber"]["type"], "compatible");
        assert_eq!(json["transcriber"]["baseUrl"], "http://localhost:8000/v1");
        assert_eq!(json["language"], "en");
        let back: SpeechConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back, speech);

        let sparse: SpeechConfig = serde_json::from_value(serde_json::json!({
            "transcriber": { "type": "local", "model": "parakeetV3" }
        }))
        .unwrap();
        assert_eq!(sparse.language, None);
        assert_eq!(sparse.transcriber.local_model(), Some(LocalModel::ParakeetV3));
    }

    #[test]
    fn the_key_is_not_printed() {
        let access = SpeechAccess {
            config: SpeechConfig::new(TranscriberConfig::OpenAi { model: "m".into() }),
            key: Some("sk-speech-secret".into()),
        };
        assert!(!format!("{access:?}").contains("sk-speech-secret"));
    }
}
