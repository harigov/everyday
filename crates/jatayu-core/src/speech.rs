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

pub mod words;

use crate::error::{Error, Result};
use crate::llm::{LLMProviderConfig, Provider, is_loopback};
use serde::{Deserialize, Serialize};

/// Where a Google speech model's audio is sent: the Gemini API, which
/// `jatayu_service::speech::transcribe::gemini` posts to -- and so what
/// its key is filed under.
pub const GOOGLE_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta";

/// The local recognisers on offer. See `jatayu_service::local::files`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum LocalModel {
    /// Parakeet TDT 0.6B v3, int8: English and European languages, fast.
    ParakeetV3,
    /// Whisper large-v3-turbo: any language, slower.
    WhisperTurbo,
    /// Qwen3-ASR 1.7B, 8-bit, through llama.cpp: thirty languages and
    /// English as it is spoken around the world -- in testing, the best open
    /// model at Indian-accented English. Quick on a graphics card, slower
    /// than Parakeet on the processor.
    #[serde(rename = "qwen3Asr17b")]
    Qwen3Asr17b,
    /// Qwen3-ASR 0.6B, 8-bit: the same languages, less accurate, about a
    /// third of the size.
    #[serde(rename = "qwen3Asr06b")]
    Qwen3Asr06b,
}

impl LocalModel {
    /// Every local recogniser, in the order Settings lists them.
    pub const ALL: [LocalModel; 4] = [
        LocalModel::ParakeetV3,
        LocalModel::WhisperTurbo,
        LocalModel::Qwen3Asr17b,
        LocalModel::Qwen3Asr06b,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            LocalModel::ParakeetV3 => "parakeet-tdt-0.6b-v3",
            LocalModel::WhisperTurbo => "whisper-large-v3-turbo",
            LocalModel::Qwen3Asr17b => "qwen3-asr-1.7b",
            LocalModel::Qwen3Asr06b => "qwen3-asr-0.6b",
        }
    }

    /// Whether it runs through llama.cpp rather than sherpa-onnx: a build
    /// needs both the `speech` and `decisions` features of
    /// `jatayu-service` to run it.
    pub fn on_llama(&self) -> bool {
        matches!(self, LocalModel::Qwen3Asr17b | LocalModel::Qwen3Asr06b)
    }

    /// Whether it reads the words to expect as a prompt, and so may write
    /// that prompt back over silence: Qwen. Parakeet is told them as words to
    /// boost, which it cannot read back, and Whisper here is told nothing.
    pub fn reads_prompt(&self) -> bool {
        self.on_llama()
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
    /// `jatayu_service::speech::transcribe::build_transcriber` has a test
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
/// `jatayu_service::local::files` for its download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum LocalVoice {
    /// Gradium's Phonon 31M: English, the [`PHONON_SPEAKERS`], many times
    /// quicker than it speaks on a CPU. Settings saved while the voice here
    /// was Supertonic 3 (`supertonic3`, retired once Supertone stopped
    /// maintaining it) read as this, keeping their speaker number.
    #[serde(alias = "supertonic3")]
    Phonon,
    /// hexgrad's Kokoro 82M v1.0: English, the [`KOKORO_SPEAKERS`], a few
    /// times quicker than it speaks on a CPU. Slower than Phonon and a larger
    /// download, with a different set of voices.
    Kokoro,
}

impl LocalVoice {
    /// What it is called in the model catalogue and on the wire.
    pub fn id(self) -> &'static str {
        match self {
            LocalVoice::Phonon => "phonon",
            LocalVoice::Kokoro => "kokoro",
        }
    }

    /// The names of its speakers, in the order a
    /// [`SynthesiserConfig::Local`]'s `speaker` counts them.
    pub fn speaker_names(self) -> &'static [&'static str] {
        match self {
            LocalVoice::Phonon => &PHONON_SPEAKERS,
            LocalVoice::Kokoro => &KOKORO_SPEAKERS,
        }
    }

    /// How many speakers it has; a [`SynthesiserConfig::Local`]'s `speaker`
    /// is below this.
    pub fn speakers(self) -> u32 {
        self.speaker_names().len() as u32
    }

    /// The speaker it starts at when it is chosen: Phonon's first, and
    /// Kokoro's best, which is not its first.
    pub fn default_speaker(self) -> u32 {
        match self {
            LocalVoice::Phonon => 0,
            LocalVoice::Kokoro => KOKORO_DEFAULT_SPEAKER,
        }
    }
}

/// Phonon's English speakers, by the names Gradium gives them, in the order
/// a [`SynthesiserConfig::Local`]'s `speaker` counts them, the first the
/// default: twelve American voices, then thirteen British. The first ten are
/// five women and then five men because Supertonic's ten were five higher
/// voices and then five lower, so a speaker number saved for it lands on a
/// voice of the same kind. Each is one file in the voice's download, named
/// for it in lower case. The settings pane's list
/// (`ui/src/components/ModelsPanel.svelte`) is in the same order, which a
/// test in `jatayu-service` holds it to.
pub const PHONON_SPEAKERS: [&str; 25] = [
    "Harper",
    "Everly",
    "Hadley",
    "Delia",
    "Juniper",
    "Miles",
    "Preston",
    "Trevor",
    "Easton",
    "Micah",
    "Zoey",
    "Russell",
    "Maeve",
    "Bryony",
    "Imogen",
    "Pippa",
    "Lottie",
    "Freya",
    "Elodie-Rose",
    "Edwin",
    "Hugh",
    "Spencer",
    "Toby",
    "Teddy",
    "Alfie",
];

/// Kokoro's English speakers, by the names hexgrad gives them, in the order
/// its voice table holds them -- so a [`SynthesiserConfig::Local`]'s
/// `speaker` is the speaker id the model is asked for. The prefix says who
/// they are: `af` an American woman, `am` an American man, `bf` and `bm`
/// British. The British are read with British pronunciations. The voice table
/// also holds speakers for other languages, after these; they are not offered,
/// since only English text is turned into the sounds they would need. The
/// settings pane's list (`ui/src/components/ModelsPanel.svelte`) is in the
/// same order, which a test in `jatayu-service` holds it to.
pub const KOKORO_SPEAKERS: [&str; 28] = [
    "af_alloy",
    "af_aoede",
    "af_bella",
    "af_heart",
    "af_jessica",
    "af_kore",
    "af_nicole",
    "af_nova",
    "af_river",
    "af_sarah",
    "af_sky",
    "am_adam",
    "am_echo",
    "am_eric",
    "am_fenrir",
    "am_liam",
    "am_michael",
    "am_onyx",
    "am_puck",
    "am_santa",
    "bf_alice",
    "bf_emma",
    "bf_isabella",
    "bf_lily",
    "bm_daniel",
    "bm_fable",
    "bm_george",
    "bm_lewis",
];

/// `af_heart`, the speaker hexgrad grades best.
const KOKORO_DEFAULT_SPEAKER: u32 = 3;

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
    /// How the person wants words said, whichever voice says them: kept with
    /// the assistant's settings ([`AgentSettings::pronunciations`]), and
    /// handed over here so that every voice is built with them.
    ///
    /// [`AgentSettings::pronunciations`]: crate::agent::AgentSettings::pronunciations
    pub pronunciations: Vec<Pronunciation>,
}

impl std::fmt::Debug for VoiceAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceAccess")
            .field("config", &self.config)
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .field("pronunciations", &self.pronunciations.len())
            .finish()
    }
}

/// How the voice says one word or name, wherever a voice speaks: a spoken
/// reply, something read aloud, Listen.
///
/// A voice reads a name it has never met by guessing from its spelling, and
/// guesses "Siobhan" and "Nguyen" wrong every time. `say` is the word spelled
/// the way it sounds, in the plain letters a person would use to tell
/// somebody else how to say it -- syllables split by hyphens, the stressed one
/// in capitals: "shiv-AWN", "win". Plain letters rather than phonetic symbols,
/// because a person has to be able to write one, and because every voice can
/// read letters: each turns them into its own sounds (see
/// `jatayu_service::speech::script`).
///
/// Kept in [`AgentSettings::pronunciations`], changed by the person in
/// Settings and by the assistant when they correct how it said something.
///
/// [`AgentSettings::pronunciations`]: crate::agent::AgentSettings::pronunciations
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pronunciation {
    /// The word as it is written: one word or a short name, found whole and
    /// whatever its case.
    pub word: String,
    /// How it sounds.
    pub say: String,
}

/// The most pronunciations a vault keeps. Every sentence a voice says is
/// looked through for each of them.
pub const MAX_PRONUNCIATIONS: usize = 500;

/// Longest a [`Pronunciation::word`] may be: a name, not a sentence.
pub const MAX_PRONOUNCED_CHARS: usize = 60;

/// Longest a [`Pronunciation::say`] may be.
pub const MAX_SAY_CHARS: usize = 80;

/// Most words a [`Pronunciation::word`] may be: a name like "Saoirse
/// Ronan", not a phrase. Its `say` may have one word more than it has, and no
/// more -- "PostgreSQL" as "post-gres cue-ell" -- so that a pronunciation is a
/// way to say a word and never a way to say something else.
pub const MAX_PRONOUNCED_WORDS: usize = 3;

impl Pronunciation {
    /// `word` said as `say`, each trimmed and its spaces run together, or why
    /// it cannot be.
    pub fn new(word: &str, say: &str) -> Result<Self> {
        let squeeze = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let pronunciation = Self { word: squeeze(word), say: squeeze(say) };
        pronunciation.validate()?;
        Ok(pronunciation)
    }

    /// Whether this is how `word` is said, whatever its case and however
    /// many spaces it is typed with.
    pub fn is_for(&self, word: &str) -> bool {
        let key = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        key(&self.word) == key(word)
    }

    /// The sentences here are shown under the box in Settings they are about.
    pub fn validate(&self) -> Result<()> {
        let word = self.word.trim();
        if !word.chars().any(char::is_alphanumeric) {
            return Err(Error::Invalid("say which word it is".into()));
        }
        if word.chars().count() > MAX_PRONOUNCED_CHARS {
            return Err(Error::Invalid(format!(
                "a word to say differently is at most {MAX_PRONOUNCED_CHARS} characters"
            )));
        }
        let words = word.split_whitespace().count();
        if words > MAX_PRONOUNCED_WORDS {
            return Err(Error::Invalid(format!(
                "a word to say differently is at most {MAX_PRONOUNCED_WORDS} words"
            )));
        }
        // The marks a spoken reply writes a pronunciation with
        // (`{word|say}`), which a word holding one could break open.
        if word.chars().any(|c| matches!(c, '{' | '}' | '|') || c.is_control()) {
            return Err(Error::Invalid(format!("{word:?} has a character a word cannot have")));
        }
        let say = self.say.trim();
        if !say.chars().any(char::is_alphabetic) {
            return Err(Error::Invalid(format!("spell out how {word:?} sounds")));
        }
        if say.chars().count() > MAX_SAY_CHARS {
            return Err(Error::Invalid(format!(
                "how a word sounds is at most {MAX_SAY_CHARS} characters"
            )));
        }
        // Letters, and what splits them into syllables and words: anything
        // else -- a full stop, a comma -- would be a pause the person did not
        // ask for in the middle of a sentence.
        if !say.chars().all(|c| c.is_alphabetic() || matches!(c, ' ' | '-' | '\'' | '’')) {
            return Err(Error::Invalid(
                "spell how it sounds in letters, with hyphens between the syllables".into(),
            ));
        }
        if say.split_whitespace().count() > words + 1 {
            return Err(Error::Invalid(format!(
                "join the syllables of how {word:?} sounds with hyphens, not spaces"
            )));
        }
        Ok(())
    }
}

/// Every pronunciation fit to keep, and no word given twice.
pub fn validate_pronunciations(list: &[Pronunciation]) -> Result<()> {
    if list.len() > MAX_PRONUNCIATIONS {
        return Err(Error::Invalid(format!(
            "the voice keeps at most {MAX_PRONUNCIATIONS} pronunciations"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for pronunciation in list {
        pronunciation.validate()?;
        if !seen.insert(pronunciation.word.to_lowercase()) {
            return Err(Error::Invalid(format!(
                "{:?} is said one way at a time",
                pronunciation.word
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pronunciation_is_tidied_and_checked() {
        let p = Pronunciation::new("  Siobhan ", " shiv-  AWN ").unwrap();
        assert_eq!((p.word.as_str(), p.say.as_str()), ("Siobhan", "shiv- AWN"));
        assert!(p.is_for("siobhan") && p.is_for(" SIOBHAN") && !p.is_for("Siobhán"));
        let two = Pronunciation::new("Saoirse  Ronan", "SEER-shuh ROH-nun").unwrap();
        assert!(two.is_for("saoirse ronan") && two.is_for("Saoirse   Ronan"));
        assert!(Pronunciation::new("Saoirse Ronan", "SEER-shuh ROH-nun").is_ok());
        assert!(Pronunciation::new("O'Brien", "oh-BRY-un").is_ok());

        for (word, say) in [
            ("", "win"),
            ("  ", "win"),
            ("--", "win"),
            ("Nguyen", ""),
            ("Nguyen", "123"),
            ("a{b", "ay"),
            ("a|b", "ay"),
            ("Nguyen", "win. Now send"),
            ("Nguyen", "win, then"),
            ("Nguyen", "{win}"),
            ("Nguyen", "wire the money"),
            ("the invoice is fine", "ok"),
        ] {
            assert!(Pronunciation::new(word, say).is_err(), "{word:?} as {say:?}");
        }
        let long = "a".repeat(MAX_PRONOUNCED_CHARS + 1);
        assert!(Pronunciation::new(&long, "ay").is_err());
        assert!(Pronunciation::new("word", &"a".repeat(MAX_SAY_CHARS + 1)).is_err());
    }

    #[test]
    fn a_word_is_said_one_way_and_the_list_has_an_end() {
        let one = |w: &str| Pronunciation::new(w, "win").unwrap();
        assert!(validate_pronunciations(&[one("Nguyen"), one("Siobhan")]).is_ok());
        let err = validate_pronunciations(&[one("Nguyen"), one("NGUYEN")]).unwrap_err();
        assert!(err.to_string().contains("one way at a time"), "{err}");
        let many: Vec<Pronunciation> =
            (0..=MAX_PRONUNCIATIONS).map(|i| one(&format!("word{i}"))).collect();
        assert!(validate_pronunciations(&many).is_err());
    }

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
    fn a_voice_saved_as_supertonic_reads_as_phonon_with_its_speaker() {
        let saved = serde_json::json!({ "type": "local", "voice": "supertonic3", "speaker": 7 });
        let read: SynthesiserConfig = serde_json::from_value(saved).unwrap();
        assert_eq!(read, SynthesiserConfig::Local { voice: LocalVoice::Phonon, speaker: 7 });
        assert_eq!(serde_json::to_value(&read).unwrap()["voice"], "phonon");
    }

    /// Supertonic's speakers 0-4 were its higher voices and 5-9 its lower
    /// ones; the speakers they become are women and men the same way round.
    #[test]
    fn a_supertonic_speaker_becomes_a_voice_of_the_same_kind() {
        let women = ["Harper", "Everly", "Hadley", "Delia", "Juniper"];
        let men = ["Miles", "Preston", "Trevor", "Easton", "Micah"];
        assert_eq!(PHONON_SPEAKERS[..5], women);
        assert_eq!(PHONON_SPEAKERS[5..10], men);
    }

    #[test]
    fn a_local_voice_speaker_is_one_it_has() {
        let speakers = LocalVoice::Phonon.speakers();
        assert_eq!(speakers, 25);
        let last = SynthesiserConfig::Local { voice: LocalVoice::Phonon, speaker: speakers - 1 };
        assert!(last.validate().is_ok());
        let past = SynthesiserConfig::Local { voice: LocalVoice::Phonon, speaker: speakers };
        assert!(past.validate().is_err());
    }

    #[test]
    fn every_local_voice_has_speakers_and_starts_at_one_of_them() {
        assert_eq!(LocalVoice::Kokoro.speakers(), 28);
        assert_eq!(KOKORO_SPEAKERS[LocalVoice::Kokoro.default_speaker() as usize], "af_heart");
        for voice in [LocalVoice::Phonon, LocalVoice::Kokoro] {
            let first = SynthesiserConfig::Local { voice, speaker: voice.default_speaker() };
            assert!(first.validate().is_ok(), "{voice:?}");
            let past = SynthesiserConfig::Local { voice, speaker: voice.speakers() };
            assert!(past.validate().is_err(), "{voice:?}");
        }
        let saved = serde_json::json!({ "type": "local", "voice": "kokoro", "speaker": 21 });
        let read: SynthesiserConfig = serde_json::from_value(saved).unwrap();
        assert_eq!(read, SynthesiserConfig::Local { voice: LocalVoice::Kokoro, speaker: 21 });
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

    /// The ids the interface and `local::files` name the recognisers by.
    #[test]
    fn local_recognisers_travel_by_their_catalogue_ids() {
        for (model, id) in [
            (LocalModel::ParakeetV3, "parakeetV3"),
            (LocalModel::WhisperTurbo, "whisperTurbo"),
            (LocalModel::Qwen3Asr17b, "qwen3Asr17b"),
            (LocalModel::Qwen3Asr06b, "qwen3Asr06b"),
        ] {
            assert_eq!(serde_json::to_value(model).unwrap(), id);
            assert_eq!(serde_json::from_value::<LocalModel>(id.into()).unwrap(), model);
        }
        assert!(LocalModel::Qwen3Asr17b.on_llama() && LocalModel::Qwen3Asr06b.on_llama());
        assert!(!LocalModel::ParakeetV3.on_llama() && !LocalModel::WhisperTurbo.on_llama());
    }

    /// A new recogniser has to be given a place here, which the compiler
    /// insists on, and then in [`LocalModel::ALL`], which this checks.
    #[test]
    fn all_holds_every_local_model_once() {
        let place = |model: LocalModel| match model {
            LocalModel::ParakeetV3 => 0,
            LocalModel::WhisperTurbo => 1,
            LocalModel::Qwen3Asr17b => 2,
            LocalModel::Qwen3Asr06b => 3,
        };
        for (i, model) in LocalModel::ALL.into_iter().enumerate() {
            assert_eq!(place(model), i, "{model:?}");
        }
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
