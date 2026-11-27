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
//! # Pauses
//!
//! sherpa-onnx shortens every silence of a fifth of a second or more to a
//! fifth of its length unless told otherwise (`silence_scale`, 0.2), and
//! does it by cutting from the first fifth straight to the next sound, with
//! no fade. Every comma's pause came out at a third of what Kokoro made of
//! it, and every pause between sentences said together as a blink: heard as
//! the voice stopping short and jumping on, which is what it was doing. So
//! the scale is 1: the pauses are Kokoro's, and the silence before and after
//! a clip is [`super::pace`]'s to cut.
//!
//! A sentence is also mended a little for misaki on the way
//! ([`for_misaki`]): it drops `…` and a hyphen standing alone, and the pause
//! with them, where it keeps `...` and a dash. A word marked with how it is
//! said is handed over as misaki's own mark for a pronunciation,
//! `[word](/phonemes/)`, the phonemes made from its respelling a syllable at
//! a time ([`sounded`]): the whole respelling run through the lexicons is
//! read as a word, and the neural fallback guesses at it as it guessed at
//! the name -- "shih-VAWN" came out with the capitals read as letters.
//!
//! Everything that needs the model is behind the `speech` feature, as for
//! [`super::local`].

use crate::speech::script::{self, Piece};
use jatayu_core::speech::KOKORO_SPEAKERS;

/// What sherpa-onnx is asked to keep of a pause: all of it. See the module
/// docs.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
const SILENCE_SCALE: f32 = 1.0;

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

/// `text`, a sentence in [`script`]'s terms, as misaki is to be given it:
/// see the module docs. `sound` turns a respelling into phonemes.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
fn for_misaki(text: &str, sound: impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    for piece in script::pieces(text) {
        match piece {
            Piece::Plain(plain) => out.push_str(&mended(plain)),
            Piece::Say { word, sound: respelling } => {
                let phonemes = sound(respelling);
                // misaki's mark ends its word at `]` and its phonemes at
                // `)`; neither may be in them.
                let word: String =
                    word.chars().filter(|c| !matches!(c, '[' | ']' | '(' | ')')).collect();
                let phonemes: String =
                    phonemes.chars().filter(|c| !matches!(c, '(' | ')' | '/')).collect();
                if phonemes.trim().is_empty() || word.trim().is_empty() {
                    out.push_str(&mended(&script::respelled(respelling)));
                } else {
                    out.push_str(&format!("[{}](/{}/)", word.trim(), phonemes.trim()));
                }
            }
        }
    }
    out
}

/// Prose with the marks misaki drops made ones it keeps: `…` as `...`, and a
/// hyphen standing alone between spaces as a dash -- after a dash between
/// two numbers is made "to" ([`script::ranges_said`]), which Kokoro would
/// otherwise say as a pause between two numbers.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
fn mended(plain: &str) -> String {
    script::ranges_said(plain).replace('…', "...").replace(" - ", " — ")
}

/// The vowels of misaki's phonemes, before the first of which a syllable's
/// stress goes, as misaki puts it: `wˈɪn`, not `ˈwɪn`. The capitals are its
/// diphthongs.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
const VOWELS: &str = "aeiouɑɐɒæɔəɚɛɜɪɨʊʌᵻᵊAIOQWY";

/// A respelling as Kokoro's phonemes. Each syllable is phonemized on its
/// own, its stress taken off, and the one marked stressed given it; a word
/// of one syllable that marks none is said as the lexicon says it. A syllable
/// with no vowel ("sh") is spelled out by the lexicon as letters, so it is
/// turned into sounds here instead. `phonemize` is the lexicon.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
fn sounded(respelling: &str, phonemize: impl Fn(&str) -> String) -> String {
    let sounds = |syllable: &str| -> String {
        let raw = if syllable.chars().any(|c| "aeiouy".contains(c)) {
            phonemize(syllable)
        } else {
            consonants(syllable)
        };
        raw.chars().filter(|c| c.is_alphabetic() || "ɚɜᵻᵊ".contains(*c)).collect()
    };
    script::syllables(respelling)
        .iter()
        .map(|word| match word.as_slice() {
            [(syllable, false)] if syllable.chars().any(|c| "aeiouy".contains(c)) => {
                phonemize(syllable)
                    .chars()
                    .filter(|c| !c.is_whitespace() && !c.is_ascii_punctuation())
                    .collect::<String>()
            }
            _ => word
                .iter()
                .map(|(syllable, stressed)| {
                    let plain: String =
                        sounds(syllable).chars().filter(|c| !matches!(c, 'ˈ' | 'ˌ')).collect();
                    match plain.find(|c| VOWELS.contains(c)).filter(|_| *stressed) {
                        Some(at) => format!("{}ˈ{}", &plain[..at], &plain[at..]),
                        None => plain,
                    }
                })
                .collect::<String>(),
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A syllable of consonants alone as their sounds, the pairs first.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
fn consonants(syllable: &str) -> String {
    let mut out = String::new();
    let mut rest = syllable;
    while let Some(c) = rest.chars().next() {
        let pair = [("sh", "ʃ"), ("zh", "ʒ"), ("ch", "ʧ"), ("th", "θ"), ("dh", "ð"), ("ng", "ŋ")]
            .iter()
            .find(|(spelled, _)| rest.starts_with(spelled));
        if let Some((spelled, sound)) = pair {
            out.push_str(sound);
            rest = &rest[spelled.len()..];
            continue;
        }
        out.push_str(match c {
            'j' => "ʤ",
            'y' => "j",
            'c' | 'q' => "k",
            'x' => "ks",
            'r' => "ɹ",
            'g' => "ɡ",
            c if c.is_ascii_alphabetic() => &rest[..1],
            _ => "",
        });
        rest = &rest[c.len_utf8()..];
    }
    out
}

#[cfg(feature = "speech")]
pub(super) use engine::KokoroSynthesiser;

#[cfg(feature = "speech")]
mod engine {
    use super::{SILENCE_SCALE, for_misaki, is_british, pieces, sounded};
    use crate::error::{CommandError, CommandResult, codes};
    use crate::local::files as models;
    use crate::local::resident::Resident;
    use crate::speech::pcm::sample_to_i16;
    use crate::speech::synth::{Speech, Synthesiser, nothing_to_say};
    use crate::speech::transcribe::BoxFuture;
    use jatayu_core::speech::LocalVoice;
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
            silence_scale: SILENCE_SCALE,
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
        /// [`KOKORO_SPEAKERS`](jatayu_core::speech::KOKORO_SPEAKERS),
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
            let g2p = engine.g2p(is_british(self.speaker))?;
            let text = for_misaki(text, |respelling| {
                sounded(respelling, |syllable| g2p.phonemize(syllable))
            });
            let phonemes = g2p.phonemize(&text);
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
                // Its own default is the "use the model's" value, which is
                // 0.2 again: said here too.
                silence_scale: SILENCE_SCALE,
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

    /// What misaki's lexicon says of the syllables the tests use, as
    /// loqui-g2p 0.4 says them.
    fn lexicon(syllable: &str) -> String {
        match syllable {
            "shiv" => "ʃˈɪv",
            "awn" => "ˈɔn",
            "win" => "wˈɪn",
            "keen" => "kˈin",
            "wah" => "wˈɑ",
            "seer" => "sˈɪɹ",
            "shuh" => "ʃˈʌ",
            other => panic!("the test did not expect {other:?}"),
        }
        .to_string()
    }

    #[test]
    fn a_respelling_becomes_phonemes_a_syllable_at_a_time() {
        assert_eq!(sounded("shiv-AWN", lexicon), "ʃɪvˈɔn");
        assert_eq!(sounded("keen-wah", lexicon), "kˈinwɑ", "the first, when none is marked");
        assert_eq!(sounded("win", lexicon), "wˈɪn", "one syllable, as the lexicon says it");
        assert_eq!(sounded("SEER-shuh win", lexicon), "sˈɪɹʃʌ wˈɪn");
        // A syllable with no vowel is never handed to the lexicon, which
        // would spell it out.
        assert_eq!(sounded("SHIV-zh", lexicon), "ʃˈɪvʒ");
        assert_eq!(consonants("shng"), "ʃŋ");
        assert_eq!(consonants("chj"), "ʧʤ");
    }

    #[test]
    fn a_sentence_is_mended_for_misaki_and_a_marked_word_is_its_mark() {
        let sound = |respelling: &str| sounded(respelling, lexicon);
        assert_eq!(
            for_misaki("Ask {Siobhan|shiv-AWN} now… or - later.", sound),
            "Ask [Siobhan](/ʃɪvˈɔn/) now... or — later."
        );
        // Brackets that would end misaki's mark early are left out of it.
        assert_eq!(for_misaki("{Win (Sr)|win}", sound), "[Win Sr](/wˈɪn/)");
        // Nothing came of the respelling: its letters are read instead.
        assert_eq!(for_misaki("{Odd|keen}", |_| String::new()), "keen");
        assert_eq!(for_misaki("plain words.", sound), "plain words.");
        assert_eq!(for_misaki("It takes 10 - 15 minutes.", sound), "It takes 10 to 15 minutes.");
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
