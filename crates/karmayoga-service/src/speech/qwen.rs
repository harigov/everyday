//! The Qwen3-ASR recognisers: an audio encoder feeding a Qwen3 language
//! model that writes down what it heard, run by llama.cpp (`local::llama`)
//! rather than sherpa-onnx. [`crate::speech::local::Recogniser`] holds one
//! as it holds Parakeet or Whisper; this module is what is particular to
//! it.
//!
//! The prompt, the language names, reading its answer and cutting a long
//! clip are plain Rust and checked by plain `cargo test`. Running it needs
//! llama.cpp, so [`Qwen`] is there only with both the `speech` and
//! `decisions` features -- the desktop app, not the server.
//!
//! # The prompt
//!
//! Qwen's own toolkit (`qwen_asr`, `Qwen3ASRModel._build_text_prompt`)
//! asks with an empty system turn, a user turn holding only the audio, and
//! the assistant turn opened. Left at that, the model answers
//! `language English<asr_text>what was said`; told the language, the
//! assistant turn already ends `language English<asr_text>` and the model
//! writes the words alone. llama.cpp puts `<|audio_start|>` and
//! `<|audio_end|>` around the audio itself, where the prompt has its
//! marker.
//!
//! # Graphics cards
//!
//! A recogniser goes on the graphics cards llama.cpp can see when both its
//! files and room to hear a clip fit there, and on the processor otherwise
//! -- the rule `local::decide` uses for a decision model; room is checked by
//! hearing a full window of silence there. A clip that fails on the card
//! after all is heard on a copy loaded on the processor, which takes the
//! card copy's place, freeing it, for the clips after it; the card is tried
//! again once the recogniser is next loaded.

// Without llama.cpp nothing runs Qwen, but its plain parts are still built
// and tested.
#![cfg_attr(not(all(feature = "speech", feature = "decisions")), allow(dead_code))]

/// The longest clip heard in one go. Qwen reads twenty minutes at once, but
/// llama.cpp's working memory grows with the clip; half a minute keeps it
/// small and is longer than nearly every turn the voice detector cuts.
pub(crate) const MAX_WINDOW_SECONDS: usize = 30;
/// How far back from a window's end a quiet place to cut it is looked for.
const CUT_SEARCH_SECONDS: usize = 5;
/// How much audio is measured at a time when looking for the quiet place.
const CUT_MEASURE_MS: usize = 100;
/// The shortest clip Qwen hears; a shorter one is padded with silence, as
/// Qwen's toolkit does.
const MIN_CLIP_MS: usize = 500;

/// The languages Qwen3-ASR names, by the BCP-47 code Settings keeps, as its
/// toolkit spells them (`SUPPORTED_LANGUAGES`). Cantonese is `yue`.
const LANGUAGES: &[(&str, &str)] = &[
    ("zh", "Chinese"),
    ("en", "English"),
    ("yue", "Cantonese"),
    ("ar", "Arabic"),
    ("de", "German"),
    ("fr", "French"),
    ("es", "Spanish"),
    ("pt", "Portuguese"),
    ("id", "Indonesian"),
    ("it", "Italian"),
    ("ko", "Korean"),
    ("ru", "Russian"),
    ("th", "Thai"),
    ("vi", "Vietnamese"),
    ("ja", "Japanese"),
    ("tr", "Turkish"),
    ("hi", "Hindi"),
    ("ms", "Malay"),
    ("nl", "Dutch"),
    ("sv", "Swedish"),
    ("da", "Danish"),
    ("fi", "Finnish"),
    ("pl", "Polish"),
    ("cs", "Czech"),
    ("fil", "Filipino"),
    ("fa", "Persian"),
    ("el", "Greek"),
    ("ro", "Romanian"),
    ("hu", "Hungarian"),
    ("mk", "Macedonian"),
];

/// What Qwen calls the language `code` names -- `en-IN` is English --
/// or `None` for one it does not know, which it is then left to detect.
pub(crate) fn language_name(code: &str) -> Option<&'static str> {
    let primary = code.trim().split(['-', '_']).next()?.to_ascii_lowercase();
    let primary = match primary.as_str() {
        "tl" => "fil",
        "cmn" => "zh",
        other => other,
    };
    LANGUAGES.iter().find(|(c, _)| *c == primary).map(|(_, name)| *name)
}

/// The prompt for one clip, `marker` where the audio goes, told the
/// language when there is one Qwen knows. See this module's doc.
pub(crate) fn prompt(marker: &str, language: Option<&str>) -> String {
    let mut prompt = format!(
        "<|im_start|>system\n<|im_end|>\n<|im_start|>user\n{marker}<|im_end|>\n\
         <|im_start|>assistant\n"
    );
    if let Some(name) = language.and_then(language_name) {
        prompt.push_str(&format!("language {name}<asr_text>"));
    }
    prompt
}

/// The words in what Qwen wrote: everything after `<asr_text>`, or all of
/// it when it was `told` the language and so wrote no heading. Nothing for a
/// heading with no words after it -- `language None<asr_text>` is its answer
/// to a clip with no speech in it, and it can stop after `language English`
/// before the tag. Only a heading it could have written counts as one: told
/// the language it writes none, and otherwise the second word must be a
/// language it names, so a turn that is just "language model" is kept.
pub(crate) fn words(answer: &str, told: bool) -> String {
    let answer = answer.trim();
    if let Some((_, text)) = answer.split_once("<asr_text>") {
        return text.trim().to_string();
    }
    let mut parts = answer.split_whitespace();
    let bare_heading = !told
        && parts.next() == Some("language")
        && parts.next().is_some_and(|name| {
            name == "None" || LANGUAGES.iter().any(|(_, known)| *known == name)
        })
        && parts.next().is_none();
    if bare_heading { String::new() } else { answer.to_string() }
}

/// Whether text in `language` takes Qwen many tokens a second to write:
/// scripts its vocabulary splits finely (Devanagari, Thai, Arabic and the
/// like), Vietnamese with its marks, and a language it was not told, which
/// could be any of them.
fn writes_densely(language: Option<&str>) -> bool {
    const DENSE: &[&str] =
        &["zh", "yue", "ar", "ko", "ru", "th", "vi", "ja", "hi", "fa", "el", "mk"];
    match language.and_then(language_name) {
        Some(name) => LANGUAGES.iter().any(|(code, known)| *known == name && DENSE.contains(code)),
        None => true,
    }
}

/// How many tokens Qwen may write for a clip of `samples` at `rate` in
/// `language`: more than anybody says in that time, and few enough that a
/// model stuck repeating itself stops soon. English takes it about three
/// tokens a second; a script it splits finely can take several times that.
pub(crate) fn token_budget(samples: usize, rate: usize, language: Option<&str>) -> usize {
    let seconds = samples.div_ceil(rate.max(1));
    let a_second = if writes_densely(language) { 24 } else { 10 };
    48 + a_second * seconds
}

/// `samples` cut into windows of at most [`MAX_WINDOW_SECONDS`] at `rate`,
/// each cut at the quietest [`CUT_MEASURE_MS`] in the
/// [`CUT_SEARCH_SECONDS`] before the limit, so a word is rarely cut in two.
/// Together they are `samples`, in order, with nothing left out or
/// repeated.
pub(crate) fn windows(samples: &[f32], rate: usize) -> Vec<&[f32]> {
    let max = MAX_WINDOW_SECONDS * rate;
    let search = CUT_SEARCH_SECONDS * rate;
    let measure = (CUT_MEASURE_MS * rate / 1000).max(1);
    let mut out = Vec::new();
    let mut rest = samples;
    while rest.len() > max {
        let from = max.saturating_sub(search);
        let cut = quietest(&rest[from..max], measure).map_or(max, |at| from + at).max(1);
        let (window, after) = rest.split_at(cut);
        out.push(window);
        rest = after;
    }
    if !rest.is_empty() {
        out.push(rest);
    }
    out
}

/// Where in `samples` the quietest stretch of `measure` samples ends -- its
/// own quietest sample -- or `None` when there is not room for one.
fn quietest(samples: &[f32], measure: usize) -> Option<usize> {
    if samples.len() <= measure {
        return None;
    }
    let mut sum: f32 = samples[..measure].iter().map(|s| s.abs()).sum();
    let (mut best, mut best_sum) = (0, sum);
    for start in 1..=samples.len() - measure {
        sum += samples[start + measure - 1].abs() - samples[start - 1].abs();
        if sum < best_sum {
            (best, best_sum) = (start, sum);
        }
    }
    let stretch = &samples[best..best + measure];
    let inner = stretch
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .map_or(0, |(i, _)| i);
    Some(best + inner)
}

/// `clip`, padded with silence to at least [`MIN_CLIP_MS`] at `rate`.
pub(crate) fn padded(clip: &[f32], rate: usize) -> std::borrow::Cow<'_, [f32]> {
    let min = MIN_CLIP_MS * rate / 1000;
    if clip.len() >= min {
        return std::borrow::Cow::Borrowed(clip);
    }
    let mut owned = clip.to_vec();
    owned.resize(min, 0.0);
    std::borrow::Cow::Owned(owned)
}

#[cfg(all(feature = "speech", feature = "decisions"))]
pub use run::Qwen;

#[cfg(all(feature = "speech", feature = "decisions"))]
mod run {
    use super::*;
    use crate::error::{CommandError, CommandResult, codes};
    use crate::local::llama::{self, Context, Hearing, Model};
    use karmayoga_core::meeting::{LocalModel, SAMPLE_RATE};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// Room, in positions, for one clip and its words: a full window's audio
    /// tokens (Qwen's encoder gives about thirteen a second) and the most
    /// [`token_budget`] allows for it, with some over.
    const ROOM: u32 = 1536;
    /// Tokens read at a time.
    const BATCH: u32 = 512;
    /// Tokens a second of talking takes Qwen to write: about three, as
    /// measured on a minute of the local voice (87 tokens for each 29 s
    /// window, the heading included).
    const TIMED_TOKENS_A_SECOND: usize = 3;
    /// Threads at most. Unlike the sherpa-onnx models, which stop gaining at
    /// about four, llama.cpp on the processor keeps getting quicker; half
    /// the machine, up to this, leaves the rest for the person using it.
    const MAX_THREADS: usize = 8;

    fn threads() -> i32 {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        (cores / 2).clamp(1, MAX_THREADS) as i32
    }

    fn failed(what: &str, e: impl std::fmt::Display) -> CommandError {
        CommandError::new(codes::INTERNAL, format!("{what}: {e}"))
    }

    /// One copy of the two files, loaded, with the context every clip is
    /// heard in: made once, at [`ROOM`], and cleared between clips, so the
    /// room it takes on a card is set aside while the copy is held rather
    /// than found again for each clip. Fields drop in order: the context and
    /// the encoder before the model they were made for.
    struct Loaded {
        context: Context<'static>,
        hearing: Hearing,
        model: Model,
    }

    impl Loaded {
        fn load(model: &Path, mmproj: &Path, on_gpu: bool) -> Result<Self, String> {
            let model = Model::load(model, on_gpu)?;
            let hearing = Hearing::load(mmproj, &model, on_gpu, threads())?;
            if hearing.sample_rate() != SAMPLE_RATE {
                return Err(format!(
                    "the encoder hears at {} Hz, not {SAMPLE_RATE}",
                    hearing.sample_rate()
                ));
            }
            let context = model.context_batched(ROOM, BATCH, threads())?;
            // SAFETY: a context borrows its model only in name -- it holds
            // llama.cpp's own pointer, which moving `model` does not move --
            // and `context` is declared first, so it is dropped before the
            // model it was made for.
            let context = unsafe { std::mem::transmute::<Context<'_>, Context<'static>>(context) };
            Ok(Self { context, hearing, model })
        }

        /// Hear one clip of at most [`MAX_WINDOW_SECONDS`]. With `exactly`,
        /// write that many tokens whatever the model would rather do -- see
        /// `llama::Context::write_after` -- for timing it.
        fn hear(
            &mut self,
            clip: &[f32],
            language: Option<&str>,
            exactly: Option<usize>,
        ) -> Result<String, String> {
            let clip = padded(clip, SAMPLE_RATE as usize);
            let heard = self.hearing.tokenize(&prompt(Hearing::marker(), language), &clip)?;
            let budget = token_budget(clip.len(), SAMPLE_RATE as usize, language);
            // Past where it would have stopped, a model repeats itself until
            // it is stopped: write the tokens asked for and no more.
            let (budget, at_least) = match exactly {
                Some(n) => (n.min(budget), n.min(budget)),
                None => (budget, 0),
            };
            let (answer, stopped) =
                self.context.write_after(&self.model, &self.hearing, &heard, budget, at_least)?;
            if stopped && exactly.is_none() {
                tracing::warn!(
                    budget,
                    "Qwen was stopped at its budget; the end of this clip may be missing"
                );
            }
            let told = language.and_then(language_name).is_some();
            Ok(words(&answer, told))
        }
    }

    /// The copy a recogniser is hearing with, and the cards it is on.
    struct Copy {
        loaded: Loaded,
        /// The cards' names, or `None` on the processor.
        gpu: Option<String>,
    }

    /// A Qwen3-ASR recogniser, loaded -- see this module's doc.
    pub struct Qwen {
        which: LocalModel,
        paths: (PathBuf, PathBuf),
        /// Held while a clip is heard: one at a time. A copy on the cards is
        /// swapped for one on the processor the first time a clip fails
        /// there.
        copy: Mutex<Copy>,
    }

    impl Qwen {
        /// Load `which` from its two files, on the cards when it fits there
        /// and the processor otherwise. Blocking.
        pub fn load(which: LocalModel, model: PathBuf, mmproj: PathBuf) -> CommandResult<Self> {
            let (loaded, gpu) = place(which, &model, &mmproj)?;
            Ok(Self { which, paths: (model, mmproj), copy: Mutex::new(Copy { loaded, gpu }) })
        }

        /// The graphics cards it is on, by name, or `None` on the processor.
        pub fn gpu(&self) -> Option<String> {
            self.copy.lock().unwrap_or_else(|e| e.into_inner()).gpu.clone()
        }

        /// What was said in `samples` (16 kHz mono, -1..1), in the language
        /// named when Qwen knows it and whichever it hears otherwise. A long
        /// clip is heard in [`windows`] and their words joined. Blocking.
        pub fn transcribe(&self, samples: &[f32], language: Option<&str>) -> CommandResult<String> {
            self.transcribe_writing(samples, language, None)
        }

        /// [`Self::transcribe`], writing as many tokens for each second as
        /// somebody talking would need -- [`TIMED_TOKENS_A_SECOND`] -- even
        /// for audio with nothing in it: what timing it on made-up audio
        /// needs, since most of its time goes on writing. Blocking.
        pub fn transcribe_timed(&self, samples: &[f32]) -> CommandResult<String> {
            self.transcribe_writing(samples, None, Some(TIMED_TOKENS_A_SECOND))
        }

        fn transcribe_writing(
            &self,
            samples: &[f32],
            language: Option<&str>,
            tokens_a_second: Option<usize>,
        ) -> CommandResult<String> {
            let mut copy = self.copy.lock().unwrap_or_else(|e| e.into_inner());
            let mut said = Vec::new();
            for window in windows(samples, SAMPLE_RATE as usize) {
                let exactly = tokens_a_second.map(|n| n * window.len() / SAMPLE_RATE as usize);
                let text = match copy.loaded.hear(window, language, exactly) {
                    Err(e) if copy.gpu.is_some() => {
                        tracing::info!(
                            model = self.which.as_str(),
                            "a clip failed on the graphics card ({e}); hearing on the \
                             processor from now on"
                        );
                        let processor = Loaded::load(&self.paths.0, &self.paths.1, false)
                            .map_err(|e| failed(self.which.as_str(), e))?;
                        // The copy on the cards is let go of here, and its
                        // memory with it. The processor copy is kept whatever
                        // this clip does on it, so a clip that fails for its
                        // own reasons does not load it again for every clip
                        // after. The cards are tried again on the next load.
                        *copy = Copy { loaded: processor, gpu: None };
                        copy.loaded.hear(window, language, exactly)
                    }
                    other => other,
                }
                .map_err(|e| failed(&format!("{} could not hear it", self.which.as_str()), e))?;
                if !text.is_empty() {
                    said.push(text);
                }
            }
            Ok(said.join(" "))
        }
    }

    /// Load `which` onto the graphics cards when its files fit there with
    /// room to hear a full window, and on the processor otherwise. Answers
    /// it and, when it went on cards, their names.
    ///
    /// Room is checked by hearing a full window of silence once loaded: that
    /// sets aside the encoder's working memory for the longest clip as well
    /// as the context, where loading alone sets aside neither (llama.cpp
    /// makes the encoder's on its first clip, and does not say when it could
    /// not).
    fn place(
        which: LocalModel,
        model: &Path,
        mmproj: &Path,
    ) -> CommandResult<(Loaded, Option<String>)> {
        let name = which.as_str();
        let gpus = llama::gpus();
        if !gpus.is_empty() {
            let names = gpus.iter().map(|g| g.name.as_str()).collect::<Vec<_>>().join(" and ");
            let free: u64 = gpus.iter().map(|g| g.free).sum();
            // A file that cannot be measured is taken to be too big.
            let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(u64::MAX);
            let bytes = size(model).saturating_add(size(mmproj));
            if free < bytes {
                tracing::info!(
                    model = name,
                    free_mb = free >> 20,
                    file_mb = bytes >> 20,
                    "no room on {names}; loading on the processor"
                );
            } else {
                let silence = vec![0.0; MAX_WINDOW_SECONDS * SAMPLE_RATE as usize];
                let tried = Loaded::load(model, mmproj, true)
                    .and_then(|mut loaded| loaded.hear(&silence, None, Some(1)).map(|_| loaded));
                match tried {
                    Ok(loaded) => {
                        tracing::info!(model = name, "loaded on {names}");
                        return Ok((loaded, Some(names)));
                    }
                    Err(e) => tracing::info!(
                        model = name,
                        "no room on {names} to hear a clip ({e}); loading on the processor \
                         instead"
                    ),
                }
            }
        }
        let loaded = Loaded::load(model, mmproj, false)
            .map_err(|e| failed(&format!("{name} would not load"), e))?;
        tracing::info!(model = name, "loaded on the processor");
        Ok((loaded, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_language_code_becomes_the_name_qwen_knows_it_by() {
        assert_eq!(language_name("en"), Some("English"));
        assert_eq!(language_name("en-IN"), Some("English"));
        assert_eq!(language_name(" HI "), Some("Hindi"));
        assert_eq!(language_name("yue"), Some("Cantonese"));
        assert_eq!(language_name("tl"), Some("Filipino"));
        assert_eq!(language_name("te"), None, "Telugu is not one of its thirty");
        assert_eq!(language_name(""), None);
        assert_eq!(LANGUAGES.len(), 30);
    }

    #[test]
    fn the_prompt_names_the_language_only_when_qwen_knows_it() {
        let open = prompt("<m>", None);
        assert_eq!(
            open,
            "<|im_start|>system\n<|im_end|>\n<|im_start|>user\n<m><|im_end|>\n\
             <|im_start|>assistant\n"
        );
        assert_eq!(prompt("<m>", Some("en-GB")), format!("{open}language English<asr_text>"));
        assert_eq!(prompt("<m>", Some("te")), open);
    }

    #[test]
    fn the_words_are_what_follows_the_heading() {
        assert_eq!(words("language English<asr_text>Hello there.", false), "Hello there.");
        assert_eq!(
            words("  Told the language, it writes words alone. ", true),
            "Told the language, it writes words alone."
        );
        assert_eq!(words("language None<asr_text>", false), "");
        assert_eq!(words("language None<asr_text> um ", false), "um");
        assert_eq!(words("language English", false), "");
        assert_eq!(words("language None", false), "");
        assert_eq!(words("", false), "");
    }

    /// Somebody's own words that look like a heading are kept: told the
    /// language, Qwen writes no heading at all, and otherwise only a
    /// language it names makes one.
    #[test]
    fn words_that_only_look_like_a_heading_are_kept() {
        assert_eq!(words("language model", false), "language model");
        assert_eq!(words("language barrier", true), "language barrier");
        assert_eq!(words("language English", true), "language English");
        assert_eq!(words("language English please", false), "language English please");
    }

    #[test]
    fn a_short_clip_is_padded_and_a_long_one_left_alone() {
        let rate = 16_000;
        assert_eq!(padded(&[0.5; 100], rate).len(), 8_000);
        assert_eq!(padded(&[0.5; 100], rate)[..100], [0.5; 100]);
        assert_eq!(padded(&[0.5; 9_000], rate).len(), 9_000);
    }

    #[test]
    fn a_long_clip_is_cut_at_its_quietest_place_and_nothing_is_lost() {
        let rate = 100;
        // 70 "seconds" of sound with a silence at 27.5 s and one at 55 s.
        let mut samples = vec![0.8f32; 70 * rate];
        samples[2_740..2_760].fill(0.0);
        samples[5_490..5_510].fill(0.0);
        let cut = windows(&samples, rate);
        assert_eq!(cut.len(), 3);
        assert!(cut.iter().all(|w| w.len() <= MAX_WINDOW_SECONDS * rate));
        assert_eq!(cut.iter().map(|w| w.len()).sum::<usize>(), samples.len());
        assert!((2_740..2_760).contains(&cut[0].len()), "cut at {}", cut[0].len());
        assert_eq!(cut.concat(), samples);
    }

    #[test]
    fn a_clip_with_no_quiet_place_is_cut_at_the_limit() {
        let rate = 100;
        let samples = vec![0.8f32; 31 * rate];
        let cut = windows(&samples, rate);
        assert_eq!(cut.len(), 2);
        assert!(cut[0].len() <= MAX_WINDOW_SECONDS * rate);
        assert_eq!(cut.concat(), samples);
        assert_eq!(windows(&[], rate).len(), 0);
        assert_eq!(windows(&[0.1; 10], rate).len(), 1);
    }

    #[test]
    fn the_budget_grows_with_the_clip_and_the_script() {
        assert_eq!(token_budget(16_000, 16_000, Some("en")), 58);
        assert_eq!(token_budget(30 * 16_000, 16_000, Some("en-IN")), 348);
        assert_eq!(token_budget(30 * 16_000, 16_000, Some("de")), 348);
        // Devanagari, Thai, Chinese and a language it was not told are
        // written in many more tokens.
        for dense in [Some("hi"), Some("th"), Some("zh"), Some("vi"), None, Some("te")] {
            assert_eq!(token_budget(30 * 16_000, 16_000, dense), 768, "{dense:?}");
        }
        // The densest full window still fits the context it is heard in:
        // about thirteen audio tokens a second, the prompt, and the answer.
        assert!(token_budget(30 * 16_000, 16_000, None) + 30 * 13 + 64 < 1536);
    }
}
