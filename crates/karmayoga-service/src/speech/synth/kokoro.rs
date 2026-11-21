//! Kokoro 82M on this computer, through sherpa-onnx.
//!
//! sherpa-onnx runs the model, but since 2.0 it has no way of its own to turn
//! words it does not know into sounds: it links no espeak-ng (GPL-3), and with
//! only a lexicon it skips every word not in it, without a sound -- a name, a
//! place, most of what is in somebody's mail. So the words are turned into
//! Kokoro's phonemes here, by `loqui-g2p`: misaki's English pipeline, which is
//! what Kokoro was trained on, with its lexicons, and a small neural model for
//! the words they do not have. sherpa-onnx is handed the phonemes.
//!
//! Everything that needs the model is behind the `speech` feature, as for
//! [`super::local`].

use karmayoga_core::speech::KOKORO_SPEAKERS;

/// The most phonemes handed to the model in one go. Kokoro reads at most 510
/// tokens, two of them the padding around a sentence, and sherpa-onnx does not
/// split a longer run of phonemes itself; this leaves room under that.
const MAX_PHONEMES: usize = 400;

/// Whether speaker number `speaker` is one of the British voices, read with
/// British pronunciations.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
fn is_british(speaker: u32) -> bool {
    usize::try_from(speaker)
        .ok()
        .and_then(|index| KOKORO_SPEAKERS.get(index))
        .is_some_and(|name| name.starts_with('b'))
}

/// `phonemes` in pieces of at most [`MAX_PHONEMES`] characters, each piece one
/// run the model is asked for. A piece ends after a sentence's last mark when
/// it can, then after a comma, then at a space, and only in the middle of a
/// word when the word alone is longer than a piece. The pieces are spoken one
/// after the other, so where they are cut is heard as a pause.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
fn pieces(phonemes: &str) -> Vec<Vec<i32>> {
    let mut out = Vec::new();
    let mut rest: Vec<char> = phonemes.trim().chars().collect();
    while rest.len() > MAX_PHONEMES {
        let window = &rest[..=MAX_PHONEMES];
        // A cut after the character at `i`, which the next character, a
        // space, confirms is the end of something.
        let after = |marks: &[char]| {
            (1..window.len()).rev().find(|&i| window[i] == ' ' && marks.contains(&window[i - 1]))
        };
        let cut = after(&['.', '!', '?', ';', '…'])
            .or_else(|| after(&[',', ':', '—']))
            .or_else(|| window.iter().rposition(|&c| c == ' ').filter(|&i| i > 0))
            .unwrap_or(MAX_PHONEMES);
        let piece: String = rest[..cut].iter().collect();
        if !piece.trim().is_empty() {
            out.push(piece.trim().chars().map(|c| c as i32).collect());
        }
        rest.drain(..cut);
    }
    let last: String = rest.into_iter().collect();
    if !last.trim().is_empty() {
        out.push(last.trim().chars().map(|c| c as i32).collect());
    }
    out
}

#[cfg(feature = "speech")]
pub(super) use engine::KokoroSynthesiser;

#[cfg(feature = "speech")]
mod engine {
    use super::{is_british, pieces};
    use crate::error::{CommandError, CommandResult, codes};
    use crate::local::files as models;
    use crate::local::resident::Resident;
    use crate::speech::pcm::sample_to_i16;
    use crate::speech::synth::{Speech, Synthesiser, nothing_to_say};
    use crate::speech::transcribe::BoxFuture;
    use karmayoga_core::speech::LocalVoice;
    use loqui_g2p::{Dialect, G2p, OovFallback};
    use std::sync::{Mutex, OnceLock};

    /// The model, loaded, and what turns words into its phonemes.
    ///
    /// The `Mutex` has one sentence made at a time, as the Phonon engine's
    /// does in [`super::super::local`]: a conversation can ask for the next
    /// sentence while one is being made, and the model's worker threads would
    /// only be fought over. The two pronunciations are each built the first
    /// time a speaker needs them (about a tenth of a second) and kept for as
    /// long as the model is.
    struct Engine {
        tts: Mutex<sherpa_onnx::OfflineTts>,
        american: OnceLock<G2p>,
        british: OnceLock<G2p>,
    }

    impl Engine {
        fn g2p(&self, british: bool) -> CommandResult<&G2p> {
            let (cell, dialect) = if british {
                (&self.british, Dialect::British)
            } else {
                (&self.american, Dialect::American)
            };
            if let Some(g2p) = cell.get() {
                return Ok(g2p);
            }
            let g2p = G2p::new(dialect, OovFallback::Neural).map_err(|e| {
                CommandError::new(
                    codes::INTERNAL,
                    format!("could not get the voice's pronunciations ready: {e}"),
                )
            })?;
            Ok(cell.get_or_init(|| g2p))
        }
    }

    static VOICES: Resident<LocalVoice, Engine> = Resident::new("voice");

    fn load() -> CommandResult<Engine> {
        let paths = models::kokoro_paths().ok_or_else(|| {
            CommandError::new(
                codes::NOT_FOUND,
                "Kokoro is not downloaded yet; download it in Settings → Models",
            )
        })?;
        let path = |p: &std::path::Path| Some(p.to_string_lossy().into_owned());
        let config = sherpa_onnx::OfflineTtsConfig {
            model: sherpa_onnx::OfflineTtsModelConfig {
                kokoro: sherpa_onnx::OfflineTtsKokoroModelConfig {
                    model: path(&paths.model),
                    voices: path(&paths.voices),
                    tokens: path(&paths.tokens),
                    ..Default::default()
                },
                num_threads: crate::speech::local::worker_threads(),
                provider: Some("cpu".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        let tts = sherpa_onnx::OfflineTts::create(&config).ok_or_else(|| {
            CommandError::new(codes::INTERNAL, "could not load Kokoro on this computer")
        })?;
        Ok(Engine { tts: Mutex::new(tts), american: OnceLock::new(), british: OnceLock::new() })
    }

    #[derive(Clone, Copy)]
    pub struct KokoroSynthesiser {
        /// The speaker id the model is asked for: an index into
        /// [`KOKORO_SPEAKERS`](karmayoga_core::speech::KOKORO_SPEAKERS),
        /// which settings have already checked.
        speaker: u32,
        speed: f32,
    }

    impl KokoroSynthesiser {
        pub fn new(speaker: u32, speed: f32) -> Self {
            Self { speaker, speed }
        }

        /// Load the model now, if it is not loaded already, and the
        /// pronunciations this speaker reads with. Kokoro has nothing to get
        /// ready per speaker: its first sentence costs what any other does.
        pub async fn warm(&self) -> CommandResult<()> {
            let british = is_british(self.speaker);
            crate::service::blocking(move || {
                let engine = VOICES.get(LocalVoice::Kokoro, load)?;
                engine.g2p(british).map(|_| ())
            })
            .await
        }

        /// Blocking, as is everything that runs the model.
        fn speak(&self, text: &str) -> CommandResult<Speech> {
            if text.trim().is_empty() {
                return Err(nothing_to_say());
            }
            let engine = VOICES.get(LocalVoice::Kokoro, load)?;
            let phonemes = engine.g2p(is_british(self.speaker))?.phonemize(text);
            let pieces = pieces(&phonemes);
            if pieces.is_empty() {
                return Err(nothing_to_say());
            }
            // Kokoro sets its own pace, so the speed is the model's rather
            // than the audio's, stretched afterwards as Phonon's has to be.
            let config = sherpa_onnx::GenerationConfig {
                sid: self.speaker as i32,
                speed: self.speed,
                phoneme_codepoints: Some(pieces),
                ..Default::default()
            };
            let audio = {
                let tts = engine.tts.lock().unwrap_or_else(|e| e.into_inner());
                // The words are in the phonemes, so the text handed beside
                // them is empty, as sherpa-onnx asks.
                tts.generate_with_config::<fn(&[f32], f32) -> bool>("", &config, None)
            };
            let audio = audio
                .ok_or_else(|| CommandError::new(codes::INTERNAL, "Kokoro could not say that"))?;
            let sample_rate = u32::try_from(audio.sample_rate()).unwrap_or(0);
            let samples: Vec<i16> = audio.samples().iter().map(|&s| sample_to_i16(s)).collect();
            if samples.is_empty() || sample_rate == 0 {
                return Err(CommandError::new(codes::INTERNAL, "Kokoro made no sound"));
            }
            Ok(Speech { samples, sample_rate })
        }
    }

    impl Synthesiser for KokoroSynthesiser {
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

    fn text(piece: &[i32]) -> String {
        piece.iter().map(|&c| char::from_u32(c as u32).unwrap()).collect()
    }

    #[test]
    fn the_british_speakers_are_the_b_ones() {
        assert!(!is_british(3), "af_heart");
        assert!(!is_british(19), "am_santa");
        assert!(is_british(20), "bf_alice");
        assert!(is_british(27), "bm_lewis");
        assert!(!is_british(28));
    }

    #[test]
    fn a_short_sentence_is_one_piece() {
        let got = pieces("  hˈɛlO wˈɜɹld. ");
        assert_eq!(got.len(), 1);
        assert_eq!(text(&got[0]), "hˈɛlO wˈɜɹld.");
        assert!(pieces("   ").is_empty());
    }

    #[test]
    fn a_long_run_is_cut_after_a_sentence_then_a_comma_then_a_space() {
        let sentence = "ɐ ".repeat(150) + "ɛnd. ";
        let long = sentence.repeat(3);
        let got = pieces(&long);
        assert!(got.len() >= 2, "{}", got.len());
        for piece in &got {
            assert!(piece.len() <= MAX_PHONEMES, "{}", piece.len());
            assert!(text(piece).ends_with("ɛnd."), "{}", text(piece));
        }

        let clauses = ("ɐ ".repeat(90) + "kˈɑmə, ").repeat(5);
        for piece in pieces(&clauses).iter().rev().skip(1) {
            assert!(text(piece).ends_with(','), "{}", text(piece));
        }

        let words = "wˈɜɹd ".repeat(200);
        let got = pieces(&words);
        assert!(got.iter().all(|p| p.len() <= MAX_PHONEMES && text(p).ends_with('d')));
        let back: Vec<String> = got.iter().map(|p| text(p)).collect();
        assert_eq!(back.join(" "), words.trim());
    }

    #[test]
    fn a_word_longer_than_a_piece_is_cut_in_the_middle() {
        let word = "ɐ".repeat(MAX_PHONEMES * 2 + 10);
        let got = pieces(&word);
        assert_eq!(got.len(), 3);
        assert_eq!(got.iter().map(Vec::len).sum::<usize>(), word.chars().count());
    }
}
