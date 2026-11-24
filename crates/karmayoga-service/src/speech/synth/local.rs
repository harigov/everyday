//! The voices on this computer: Phonon 31M, through `ptts`, here, and
//! Kokoro 82M, through sherpa-onnx, in [`super::kokoro`].
//!
//! Everything that needs a model is behind the `speech` feature; a build
//! without it can still be configured with a local voice and is told, when it
//! tries to load one, that it cannot.
//!
//! # How freely Phonon says a sentence
//!
//! Phonon draws each moment of a sentence from noise, and two settings decide
//! how that noise is used. The temperature is how far from the most likely
//! next moment the model may go ([`TEMPERATURE`]). The seed is which noise it
//! gets: `ptts` gives every sentence the same one unless told otherwise, so
//! every sentence began from the same noise; each is now seeded from its own
//! words ([`seed_for`]). Neither moves the tune of a sentence much: how a
//! sentence rises and falls is mostly the model's reading of its words, and
//! what most made a reply sound read out was each sentence being made on its
//! own ([`crate::speech::narrate`] says them together).

use super::Synthesiser;
use crate::error::{CommandError, CommandResult, codes};
use karmayoga_core::speech::PHONON_SPEAKERS;
use std::sync::Arc;

/// How far from its most likely next moment of sound Phonon may go. `ptts`
/// uses 0.3 unless told; Phonon's own config asks for 0.8. Measured on twelve
/// replies in two speakers, 0.3 to 0.8 changed neither how well Parakeet
/// heard them (0.4 to 1.2% of words, the same name missed at each) nor how
/// far the pitch moved (within a tenth of a semitone) nor the length of a
/// clip (by a sixth of a second at most): the difference is in the detail of
/// the sound, near enough what the model was made with.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
const TEMPERATURE: f32 = 0.7;

/// The seed `text`'s noise is drawn from: its own words. Each sentence is
/// said its own way, and the same sentence the same way every time, so one
/// that comes out wrong can be made again to hear why. FNV-1a, whose every
/// value is fixed by its definition: the standard library's hasher may change
/// between Rust releases, and the promise with it.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
fn seed_for(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The name Phonon knows speaker number `speaker` by. Settings refuse a
/// number past the last speaker, so one here is a config that skipped them,
/// and is said rather than quietly spoken in somebody else's voice.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
fn speaker_name(speaker: u32) -> CommandResult<&'static str> {
    usize::try_from(speaker).ok().and_then(|index| PHONON_SPEAKERS.get(index).copied()).ok_or_else(
        || {
            CommandError::new(
                codes::INVALID,
                format!(
                    "the voice on this computer has {} speakers, and no speaker {speaker}",
                    PHONON_SPEAKERS.len()
                ),
            )
        },
    )
}

/// The voice, loaded, as a [`Synthesiser`]. It is loaded here so that a voice
/// that is missing is found out when speech is asked for, not on its first
/// sentence.
#[cfg(feature = "speech")]
pub(super) async fn build(
    voice: karmayoga_core::speech::LocalVoice,
    speaker: u32,
    speed: f32,
) -> CommandResult<Arc<dyn Synthesiser>> {
    use karmayoga_core::speech::LocalVoice;
    match voice {
        LocalVoice::Phonon => {
            let synthesiser = engine::LocalSynthesiser::new(voice, speaker_name(speaker)?, speed);
            synthesiser.warm().await?;
            Ok(Arc::new(synthesiser))
        }
        LocalVoice::Kokoro => {
            if speaker >= voice.speakers() {
                return Err(CommandError::new(
                    codes::INVALID,
                    format!("Kokoro has {} speakers, and no speaker {speaker}", voice.speakers()),
                ));
            }
            let synthesiser = super::kokoro::KokoroSynthesiser::new(speaker, speed);
            synthesiser.warm().await?;
            Ok(Arc::new(synthesiser))
        }
    }
}

#[cfg(not(feature = "speech"))]
pub(super) async fn build(
    _voice: karmayoga_core::speech::LocalVoice,
    _speaker: u32,
    _speed: f32,
) -> CommandResult<Arc<dyn Synthesiser>> {
    Err(CommandError::new(
        codes::UNSUPPORTED,
        "this copy of Karma Yoga was built without voices that run on this computer",
    ))
}

#[cfg(feature = "speech")]
mod engine {
    use super::{TEMPERATURE, seed_for};
    use crate::error::{CommandError, CommandResult, codes};
    use crate::local::files as models;
    use crate::local::resident::Resident;
    use crate::speech::pcm::sample_to_i16;
    use crate::speech::script;
    use crate::speech::synth::{Speech, Synthesiser, nothing_to_say};
    use crate::speech::tempo::stretch;
    use crate::speech::transcribe::BoxFuture;
    use karmayoga_core::speech::LocalVoice;
    use std::sync::{Mutex, Once};

    /// Cap on worker threads handed to the model, for the reasons
    /// `speech::local` caps its own: one voice is one person's
    /// conversation, and should not take every core there is.
    const MAX_THREADS: usize = 4;

    /// A voice that is loaded.
    ///
    /// `ptts` can be used from several threads, but the `Mutex` has one
    /// sentence made at a time all the same, as `speech::local::Recogniser`'s
    /// does for sherpa-onnx: a conversation can have a sentence being made
    /// while the next is asked for, and the model's worker threads have one
    /// job at a time to give, so two sentences would only fight over them.
    struct Engine {
        tts: Mutex<ptts::synth::Synth>,
        /// The speakers that have said something, or been got ready to,
        /// since this load: see [`LocalSynthesiser::warm`].
        primed: Mutex<Vec<&'static str>>,
    }

    impl Engine {
        fn is_primed(&self, speaker: &str) -> bool {
            self.primed.lock().unwrap_or_else(|e| e.into_inner()).contains(&speaker)
        }

        fn primed(&self, speaker: &'static str) {
            let mut primed = self.primed.lock().unwrap_or_else(|e| e.into_inner());
            if !primed.contains(&speaker) {
                primed.push(speaker);
            }
        }
    }

    static VOICES: Resident<LocalVoice, Engine> = Resident::new("voice");

    /// Give the model its worker threads, the first time one is wanted and
    /// never again.
    ///
    /// `xn`, which runs the model, would otherwise build its pool by itself,
    /// a thread to a core as `RAYON_NUM_THREADS` or the machine says, and the
    /// way to size it, `xn::set_num_threads`, sets an environment variable,
    /// which is not safe in a process that has other threads running. So the
    /// pool is started here, at the width wanted, on threads of this app's own
    /// making. Its workers are permanent: they spin for a moment after work
    /// and then park, costing nothing while no voice is speaking. The thread
    /// that asks for a sentence is one of the pool's, so there is one worker
    /// fewer than `threads`.
    ///
    /// A worker that cannot be started is a panic, not an error: the pool waits
    /// on every worker it was promised, so one short would leave the first
    /// sentence waiting for ever. A machine that cannot start four threads has
    /// bigger troubles than a voice.
    fn start_workers() {
        static STARTED: Once = Once::new();
        STARTED.call_once(|| {
            let threads = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
                .clamp(1, MAX_THREADS);
            xn::threadpool::start_pool_with(
                xn::threadpool::PoolConfig { workers: threads - 1, spin_budget: None },
                |job| {
                    std::thread::Builder::new()
                        .name("voice-worker".into())
                        .spawn(job)
                        .expect("starting a thread for the voice on this computer");
                },
            );
        });
    }

    /// `what` went wrong, in `error`'s words.
    fn failed(what: &str, error: ptts::Error) -> CommandError {
        CommandError::new(codes::INTERNAL, format!("{what}: {error}"))
    }

    fn load(_voice: LocalVoice) -> CommandResult<Engine> {
        let paths = models::phonon_paths().ok_or_else(|| {
            CommandError::new(
                codes::NOT_FOUND,
                "Phonon is not downloaded yet; download it in Settings → Models",
            )
        })?;
        start_workers();

        let config = ptts::checkpoint::read_config(&paths.config)
            .map_err(|e| failed("could not read the voice on this computer", e))?;
        let mut builder =
            ptts::synth::Synth::builder(config, &paths.weights, ptts::preprocess::Lang::En)
                .tokenizer_file(&paths.tokenizer)
                .temperature(TEMPERATURE)
                .quant(ptts::synth::Quant::Q80)
                .device(ptts::synth::DeviceKind::Cpu);
        for (name, file) in &paths.speakers {
            builder = builder.add_voice(*name, file);
        }
        let tts =
            builder.build().map_err(|e| failed("could not load the voice on this computer", e))?;
        Ok(Engine { tts: Mutex::new(tts), primed: Mutex::new(Vec::new()) })
    }

    #[derive(Clone, Copy)]
    pub struct LocalSynthesiser {
        voice: LocalVoice,
        /// One of [`PHONON_SPEAKERS`](karmayoga_core::speech::PHONON_SPEAKERS).
        speaker: &'static str,
        speed: f32,
    }

    impl LocalSynthesiser {
        pub fn new(voice: LocalVoice, speaker: &'static str, speed: f32) -> Self {
            Self { voice, speaker, speed }
        }

        /// Load the voice now, if it is not loaded already, and start getting
        /// this speaker ready behind it. It is not held on to: that is the
        /// [`Resident`]'s to do, and to let go of.
        ///
        /// A speaker's first sentence after a load costs about 300 ms more
        /// than any after it, measured on a 12-core desktop: `ptts` primes
        /// each speaker the first time it is asked for one. Priming it here,
        /// in the background, spends that while voice mode waits for the
        /// person to speak rather than in its first reply; a caller that asks
        /// for a sentence at once just waits for it under the lock, as it
        /// would have anyway. Priming is a session `ptts` builds and this
        /// drops at once, which allocates its whole cache; so a speaker that
        /// has been primed, or has spoken, since the load is not primed again
        /// -- Listen builds a synthesiser on every change of speed.
        pub async fn warm(&self) -> CommandResult<()> {
            let voice = self.voice;
            let engine =
                crate::service::blocking(move || VOICES.get(voice, || load(voice))).await?;
            let speaker = self.speaker;
            if engine.is_primed(speaker) {
                return Ok(());
            }
            tokio::spawn(crate::service::blocking(move || {
                let tts = engine.tts.lock().unwrap_or_else(|e| e.into_inner());
                if engine.is_primed(speaker) {
                    return Ok(());
                }
                let options = ptts::synth::SpeechOptions::default().voice(speaker);
                match tts.session_default(&options) {
                    Ok(_) => engine.primed(speaker),
                    Err(error) => {
                        tracing::warn!(error = %error, speaker, "could not get the voice's speaker ready")
                    }
                }
                Ok(())
            }));
            Ok(())
        }

        /// Blocking, as is everything that runs the model.
        ///
        /// Phonon reads letters and nothing else: a marked word is handed
        /// over as its respelling, and a dash as a comma, since Phonon reads
        /// a dash as a space and a comma is how it is asked to pause.
        fn speak(&self, text: &str) -> CommandResult<Speech> {
            let text = script::dashes_as_commas(&script::as_read(text));
            if text.trim().is_empty() {
                return Err(nothing_to_say());
            }
            let engine = VOICES.get(self.voice, || load(self.voice))?;
            let options =
                ptts::synth::SpeechOptions::default().voice(self.speaker).seed(seed_for(&text));
            let (pcm, sample_rate) = {
                let tts = engine.tts.lock().unwrap_or_else(|e| e.into_inner());
                let pcm = tts
                    .say_with(&text, &options)
                    .map_err(|e| failed("the voice on this computer could not say that", e))?;
                engine.primed(self.speaker);
                (pcm, tts.sample_rate())
            };
            // The model has no pace of its own to set, so the speed is the
            // audio's: sped up or slowed down without the voice changing pitch.
            // At the voice's own pace this hands the audio back as it was.
            let pcm = stretch(&pcm, sample_rate, self.speed);
            let samples: Vec<i16> = pcm.iter().map(|&s| sample_to_i16(s)).collect();
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
    fn a_speaker_number_is_the_name_at_that_place() {
        assert_eq!(speaker_name(0).unwrap(), "Harper");
        assert_eq!(speaker_name(1).unwrap(), PHONON_SPEAKERS[1]);
        assert_eq!(speaker_name(24).unwrap(), PHONON_SPEAKERS[24]);
    }

    #[test]
    fn each_sentence_has_a_seed_of_its_own_and_keeps_it() {
        let first = seed_for("Okay, I've moved it to Friday.");
        assert_eq!(first, seed_for("Okay, I've moved it to Friday."));
        assert_ne!(first, seed_for("Okay, I've moved it to Thursday."));
        assert_ne!(first, seed_for("Let me see."));
        // FNV-1a's own published value: no toolchain can move it.
        assert_eq!(seed_for("a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn a_speaker_number_that_is_not_there_is_refused() {
        for speaker in [25, u32::MAX] {
            let err = speaker_name(speaker).unwrap_err();
            assert_eq!(err.code, codes::INVALID);
            assert!(err.message.contains("25 speakers"), "{}", err.message);
        }
    }

    /// The settings pane lists each voice's speakers by hand, under their
    /// accent; its lists must name them in this order, or choosing one would
    /// speak as another.
    #[test]
    fn the_settings_pane_lists_the_speakers_in_this_order() {
        use karmayoga_core::speech::{KOKORO_SPEAKERS, LocalVoice};
        let pane = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../ui/src/components/ModelsPanel.svelte");
        let text = std::fs::read_to_string(&pane).expect("the settings pane is in the tree");
        // The `['name', 'accent', ...]` lines of the list that starts at
        // `const {list}: Speaker[]`, up to the end of that list.
        let listed = |list: &str| -> Vec<(String, String)> {
            let start = text
                .find(&format!("const {list}: Speaker[]"))
                .unwrap_or_else(|| panic!("ModelsPanel.svelte has no {list}"));
            text[start..]
                .lines()
                .skip(1)
                .take_while(|line| !line.contains("] as ["))
                .filter_map(|line| {
                    let rest = line.trim().strip_prefix("['")?;
                    let (name, rest) = rest.split_once("', '")?;
                    let (accent, _) = rest.split_once("',")?;
                    matches!(accent, "American" | "British")
                        .then(|| (name.to_string(), accent.to_string()))
                })
                .collect()
        };

        let phonon = listed("PHONON_SPEAKERS");
        let names: Vec<&str> = phonon.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, PHONON_SPEAKERS, "ModelsPanel.svelte's PHONON_SPEAKERS");
        let american = phonon.iter().take_while(|(_, accent)| accent == "American").count();
        assert_eq!(american, 12, "the American voices come first, then the British");
        assert!(phonon[american..].iter().all(|(_, accent)| accent == "British"));

        let kokoro = listed("KOKORO_SPEAKERS");
        let names: Vec<&str> = kokoro.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, KOKORO_SPEAKERS, "ModelsPanel.svelte's KOKORO_SPEAKERS");
        for (name, accent) in &kokoro {
            let british = name.starts_with('b');
            assert_eq!(accent == "British", british, "{name} is listed as {accent}");
        }

        // Each voice starts at the speaker `LocalVoice::default_speaker` says.
        for voice in [LocalVoice::Phonon, LocalVoice::Kokoro] {
            let line = text
                .lines()
                .find(|line| line.trim().starts_with(&format!("{}: {{", voice.id())))
                .unwrap_or_else(|| panic!("LOCAL_VOICES has no {}", voice.id()));
            let default = line
                .split("defaultSpeaker: ")
                .nth(1)
                .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|n| n.parse::<u32>().ok())
                .unwrap_or_else(|| panic!("{line} names no defaultSpeaker"));
            assert_eq!(default, voice.default_speaker(), "{}'s first speaker", voice.id());
        }
    }
}
