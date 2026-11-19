//! Phonon 31M on this computer, through `ptts`.
//!
//! Everything that needs the model is behind the `speech` feature; a build
//! without it can still be configured with a local voice and is told, when it
//! tries to load one, that it cannot.

use super::Synthesiser;
use crate::error::{CommandError, CommandResult, codes};
use karmayoga_core::speech::PHONON_SPEAKERS;
use std::sync::Arc;

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
    let synthesiser = engine::LocalSynthesiser::new(voice, speaker_name(speaker)?, speed);
    synthesiser.warm().await?;
    Ok(Arc::new(synthesiser))
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
    use crate::error::{CommandError, CommandResult, codes};
    use crate::local::files as models;
    use crate::local::resident::Resident;
    use crate::speech::pcm::sample_to_i16;
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

    fn load(voice: LocalVoice) -> CommandResult<Engine> {
        let paths = models::voice_paths(voice).ok_or_else(|| {
            CommandError::new(
                codes::NOT_FOUND,
                "the voice on this computer is not downloaded yet; download it in Settings → Models",
            )
        })?;
        start_workers();

        let config = ptts::checkpoint::read_config(&paths.config)
            .map_err(|e| failed("could not read the voice on this computer", e))?;
        let mut builder =
            ptts::synth::Synth::builder(config, &paths.weights, ptts::preprocess::Lang::En)
                .tokenizer_file(&paths.tokenizer)
                .quant(ptts::synth::Quant::Q80)
                .device(ptts::synth::DeviceKind::Cpu);
        for (name, file) in &paths.speakers {
            builder = builder.add_voice(*name, file);
        }
        let tts =
            builder.build().map_err(|e| failed("could not load the voice on this computer", e))?;
        Ok(Engine { tts: Mutex::new(tts) })
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

        /// Load the voice now, if it is not loaded already. It is not held
        /// on to: that is the [`Resident`]'s to do, and to let go of.
        pub async fn warm(&self) -> CommandResult<()> {
            let voice = self.voice;
            crate::service::blocking(move || VOICES.get(voice, || load(voice)).map(|_| ())).await
        }

        /// Blocking, as is everything that runs the model.
        fn speak(&self, text: &str) -> CommandResult<Speech> {
            if text.trim().is_empty() {
                return Err(nothing_to_say());
            }
            let engine = VOICES.get(self.voice, || load(self.voice))?;
            let options = ptts::synth::SpeechOptions::default().voice(self.speaker);
            let (pcm, sample_rate) = {
                let tts = engine.tts.lock().unwrap_or_else(|e| e.into_inner());
                let pcm = tts
                    .say_with(text, &options)
                    .map_err(|e| failed("the voice on this computer could not say that", e))?;
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
    fn a_speaker_number_that_is_not_there_is_refused() {
        for speaker in [25, u32::MAX] {
            let err = speaker_name(speaker).unwrap_err();
            assert_eq!(err.code, codes::INVALID);
            assert!(err.message.contains("25 speakers"), "{}", err.message);
        }
    }

    /// The settings pane lists the speakers by hand, under their accent; its
    /// list must name them in this order, or choosing one would speak as
    /// another.
    #[test]
    fn the_settings_pane_lists_the_speakers_in_this_order() {
        let pane = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../ui/src/components/ModelsPanel.svelte");
        let text = std::fs::read_to_string(&pane).expect("the settings pane is in the tree");
        let listed: Vec<(&str, &str)> = text
            .lines()
            .filter_map(|line| {
                let rest = line.trim().strip_prefix("['")?;
                let (name, rest) = rest.split_once("', '")?;
                let (accent, _) = rest.split_once("',")?;
                matches!(accent, "American" | "British").then_some((name, accent))
            })
            .collect();
        let names: Vec<&str> = listed.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, PHONON_SPEAKERS, "ModelsPanel.svelte's SPEAKERS");
        let american = listed.iter().take_while(|(_, accent)| *accent == "American").count();
        assert_eq!(american, 12, "the American voices come first, then the British");
        assert!(listed[american..].iter().all(|(_, accent)| *accent == "British"));
    }
}
