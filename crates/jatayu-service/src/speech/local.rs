//! The sherpa-onnx models on this computer: voice activity detection,
//! diarisation, speaker embeddings and local recognition. Feature `speech`
//! only -- see `local::files` for the files this loads and
//! `docs/plans/meeting-notes.md`'s "The libraries".
//!
//! A platform service, not a part of meeting notes: the meeting pipeline,
//! [`crate::speech::transcribe::local`], the assistant's voice mode
//! ([`crate::speech::detect`], [`crate::speech::utterance`]) and whatever
//! listens next all load their models from here, so that one process holds
//! one copy of each. What is specific to a meeting -- who is who, which
//! chunk is which -- is not here; see `crate::meeting`.
//!
//! # Blocking
//!
//! Every public method here loads a native model or runs inference on the
//! calling thread. None of it is `async`, and none of it should be called
//! from one: a caller on the async runtime must run it inside
//! [`tokio::task::spawn_blocking`], the same as any other CPU-heavy work in
//! this crate -- see [`crate::service::blocking`].
//!
//! # Sharing
//!
//! [`speech_kit`] and [`recogniser`] each load their models the first time
//! either is asked for, and keep them behind an [`Arc`] -- cloning an
//! `Arc<SpeechKit>` or `Arc<Recogniser>` is cheap, and every caller in one
//! process is meant to share the same one rather than loading a second
//! copy. Both are [`Resident`]s: once nobody has used one for
//! [`crate::local::resident::IDLE_AFTER`], it is let go of, and the next
//! call loads it again. Loading is expensive (hundreds of megabytes of ONNX
//! weights, a few seconds); running inference on an already-loaded model is
//! not, and the
//! `Mutex` each wrapper keeps around its native objects only ever serialises
//! that part -- see [`SpeechKit`] and [`Recogniser`]'s own docs for why the
//! `Mutex` is there at all given sherpa-onnx's own `Send`/`Sync` impls.

use crate::error::{CommandError, CommandResult, codes};
use crate::local::files::{self as models, RecogniserPaths};
use crate::local::resident::Resident;
use crate::speech::hotwords;
use crate::speech::pcm::{duration_ms, to_f32};
use jatayu_core::meeting::{LocalModel, SAMPLE_RATE};
use jatayu_core::speech::words::Expected;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// How long a merged gap of silence may be before it still counts as one
/// speech turn. Below `docs/plans/meeting-notes.md`'s own numbers for "Who
/// said what": a natural pause inside one sentence is shorter than this; the
/// gap between two different people talking usually is not.
const MERGE_GAP_MS: u64 = 300;
/// A speech turn shorter than this, after merging, is noise or a stray
/// click -- not a turn transcription or diarisation should be asked about.
const MIN_SPEECH_MS: u64 = 250;
/// Each surviving turn is padded by this much on both ends. Recognisers
/// clip word onsets and codas when a segment boundary lands exactly on them;
/// the pad gives both sides a little room without reintroducing the noise
/// [`MIN_SPEECH_MS`] just dropped.
const PAD_MS: u64 = 150;

/// The stable name [`SpeechKit::embed`]'s embeddings are computed with,
/// stored beside a voiceprint so a later model change cannot silently
/// compare embeddings that were never comparable. See
/// `jatayu_core::meeting::Voiceprint::model`.
pub const EMBEDDING_MODEL_ID: &str = "sherpa-onnx:eres2net-en-voxceleb-16k";

/// Cap on worker threads handed to a native model. Read from
/// [`std::thread::available_parallelism`] rather than fixed, and capped
/// rather than left open: one recording, or one conversation, is not a
/// server serving many at once, and a laptop with sixteen cores should not
/// hand every one of them to a single VAD pass.
const MAX_THREADS: usize = 4;

/// How many threads a native model may use: the machine's cores, between one
/// and [`MAX_THREADS`]. For anything that loads a sherpa-onnx model of its
/// own and so cannot take the number the shared ones here were given.
pub(crate) fn worker_threads() -> i32 {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, MAX_THREADS) as i32
}

/// Silero's configuration, with the numbers every use of it agrees on --
/// the 0.5 threshold, the 512-sample window that is the only size it accepts,
/// 16 kHz, the CPU -- and the ones that differ by use handed in:
/// [`SpeechKit::vad`] cuts a finished recording and leaves merging to itself,
/// a conversation's detector waits a natural pause before calling a turn
/// over.
pub(crate) fn vad_config(
    model: &str,
    threads: i32,
    min_silence_seconds: f32,
    min_speech_seconds: f32,
    max_speech_seconds: f32,
) -> sherpa_onnx::VadModelConfig {
    sherpa_onnx::VadModelConfig {
        silero_vad: sherpa_onnx::SileroVadModelConfig {
            model: Some(model.to_string()),
            threshold: 0.5,
            min_silence_duration: min_silence_seconds,
            min_speech_duration: min_speech_seconds,
            window_size: 512,
            max_speech_duration: max_speech_seconds,
        },
        ten_vad: sherpa_onnx::TenVadModelConfig::default(),
        sample_rate: SAMPLE_RATE as i32,
        num_threads: threads,
        provider: Some("cpu".to_string()),
        debug: false,
    }
}

fn l2_normalise(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
    v
}

/// The speech kit, loaded from `local::files::speech_kit_paths`: voice
/// activity detection, offline speaker diarisation, and speaker embeddings.
///
/// # Why a `Mutex` around objects sherpa-onnx already marks `Send + Sync`
///
/// sherpa-onnx's own doc for every wrapper type here is "thread-safe for
/// single-object usage" -- which this reads as a promise that an object may
/// be *moved* to another thread and used there, not that two threads may
/// call into the same object at once. This crate's own callers already
/// serialise access by running each call inside its own
/// [`tokio::task::spawn_blocking`] (see this module's doc), but nothing
/// stops two different spawned tasks from holding the same `Arc<SpeechKit>`
/// and calling into it concurrently -- the whole point of sharing one. The
/// `Mutex` is what turns "probably fine in practice" into a guarantee.
///
/// The Silero VAD model is the exception: [`SpeechKit::vad`] builds a fresh
/// [`sherpa_onnx::VoiceActivityDetector`] on every call rather than sharing
/// one, because that object is a *streaming* buffer -- interleaving two
/// unrelated chunks through the same detector would corrupt both. The model
/// file is small and loading it is not the expensive part of a VAD pass.
pub struct SpeechKit {
    vad_model_path: String,
    embedding: Mutex<sherpa_onnx::SpeakerEmbeddingExtractor>,
    diariser: Mutex<sherpa_onnx::OfflineSpeakerDiarization>,
    threads: i32,
}

impl SpeechKit {
    /// Load every speech-kit model from disk. Blocking -- see this module's
    /// doc.
    pub fn load() -> CommandResult<Self> {
        let paths = models::speech_kit_paths().ok_or_else(|| {
            CommandError::new(codes::NOT_FOUND, "the speech kit is not installed")
        })?;
        let threads = worker_threads();

        let embedding_model = path_string(&paths.speaker_embedding);
        let embedding = sherpa_onnx::SpeakerEmbeddingExtractor::create(
            &sherpa_onnx::SpeakerEmbeddingExtractorConfig {
                model: Some(embedding_model.clone()),
                num_threads: threads,
                debug: false,
                provider: Some("cpu".to_string()),
            },
        )
        .ok_or_else(|| {
            CommandError::new(codes::INTERNAL, "could not load the speaker-embedding model")
        })?;

        // The diariser owns its own embedding extractor internally -- there
        // is no way to hand it the one just built above -- so the (small)
        // embedding model ends up loaded twice. Simpler than threading a
        // shared extractor through sherpa-onnx's own diarisation config,
        // which has no seam for one.
        let diariser = sherpa_onnx::OfflineSpeakerDiarization::create(
            &sherpa_onnx::OfflineSpeakerDiarizationConfig {
                segmentation: sherpa_onnx::OfflineSpeakerSegmentationModelConfig {
                    pyannote: sherpa_onnx::OfflineSpeakerSegmentationPyannoteModelConfig {
                        model: Some(path_string(&paths.segmentation)),
                        window_shift_ratio: 0.1,
                    },
                    num_threads: threads,
                    debug: false,
                    provider: Some("cpu".to_string()),
                },
                embedding: sherpa_onnx::SpeakerEmbeddingExtractorConfig {
                    model: Some(embedding_model),
                    num_threads: threads,
                    debug: false,
                    provider: Some("cpu".to_string()),
                },
                // Unknown number of speakers: threshold-based clustering rather
                // than a fixed count. See docs/plans/meeting-notes.md's "Who
                // said what": every cluster is matched against attendees
                // afterwards, in `jatayu_core::meeting::identify`.
                clustering: sherpa_onnx::FastClusteringConfig {
                    num_clusters: -1,
                    threshold: 0.5,
                    compute_confidence: false,
                },
                min_duration_on: 0.3,
                min_duration_off: 0.5,
            },
        )
        .ok_or_else(|| {
            CommandError::new(codes::INTERNAL, "could not load the diarisation models")
        })?;

        Ok(Self {
            vad_model_path: path_string(&paths.vad),
            embedding: Mutex::new(embedding),
            diariser: Mutex::new(diariser),
            threads,
        })
    }

    /// Speech turns in `samples`, as `(start_ms, end_ms)`, merged across
    /// gaps under [`MERGE_GAP_MS`], with anything shorter than
    /// [`MIN_SPEECH_MS`] dropped, and [`PAD_MS`] added to both ends of what
    /// survives (clamped to the clip). Blocking.
    pub fn vad(&self, samples: &[i16]) -> Vec<(u64, u64)> {
        let total_ms = duration_ms(samples.len());
        if samples.is_empty() {
            return Vec::new();
        }
        let f32_samples = to_f32(samples);
        // The silence and speech minimums are left to our own
        // `merge_and_pad` below, rather than done twice: sherpa's own
        // thresholds would apply first, and disagreeing with the numbers this
        // module documents would be a second, hidden copy of the same policy.
        let config = vad_config(&self.vad_model_path, self.threads, 0.1, 0.02, 20.0);
        let buffer_seconds = (total_ms as f32 / 1000.0) + 5.0;
        let Some(detector) = sherpa_onnx::VoiceActivityDetector::create(&config, buffer_seconds)
        else {
            return Vec::new();
        };

        let mut raw = Vec::new();
        const WINDOW: usize = 512;
        let drain = |detector: &sherpa_onnx::VoiceActivityDetector, raw: &mut Vec<(u64, u64)>| {
            while let Some(seg) = detector.front() {
                let start_ms = duration_ms(seg.start().max(0) as usize);
                let end_ms = start_ms + duration_ms(seg.n().max(0) as usize);
                raw.push((start_ms, end_ms));
                detector.pop();
            }
        };
        for chunk in f32_samples.chunks(WINDOW) {
            detector.accept_waveform(chunk);
            drain(&detector, &mut raw);
        }
        detector.flush();
        drain(&detector, &mut raw);

        merge_and_pad(raw, MERGE_GAP_MS, MIN_SPEECH_MS, PAD_MS, total_ms)
    }

    /// A speaker embedding for `samples`, L2-normalised, or `None` when
    /// there is not enough audio (roughly a second) to compute one
    /// reliably. Blocking.
    pub fn embed(&self, samples: &[i16]) -> Option<Vec<f32>> {
        let f32_samples = to_f32(samples);
        let extractor = self.embedding.lock().unwrap();
        let stream = extractor.create_stream()?;
        stream.accept_waveform(SAMPLE_RATE as i32, &f32_samples);
        stream.input_finished();
        if !extractor.is_ready(&stream) {
            return None;
        }
        let embedding = extractor.compute(&stream)?;
        Some(l2_normalise(embedding))
    }

    /// The stable name [`Self::embed`]'s embeddings are computed with. See
    /// [`EMBEDDING_MODEL_ID`].
    pub fn embedding_model(&self) -> &str {
        EMBEDDING_MODEL_ID
    }

    /// Offline speaker diarisation over the whole of `samples`:
    /// `(start_ms, end_ms, speaker_index)`, an unknown number of speakers
    /// decided by threshold clustering. `speaker_index` is stable *within
    /// this one call* and comparable across the segments it returns; it is
    /// not comparable across two separate calls, or with anything
    /// [`Self::embed`] returns -- identity is settled afterwards, in
    /// `jatayu_core::meeting::identify`, by comparing embeddings, not
    /// indices. Blocking.
    pub fn diarise(&self, samples: &[i16]) -> Vec<(u64, u64, usize)> {
        let f32_samples = to_f32(samples);
        let diariser = self.diariser.lock().unwrap();
        let Some(result) = diariser.process(&f32_samples) else { return Vec::new() };
        result
            .sort_by_start_time()
            .into_iter()
            .map(|seg| {
                let start_ms = (f64::from(seg.start) * 1000.0).round() as u64;
                let end_ms = (f64::from(seg.end) * 1000.0).round() as u64;
                (start_ms, end_ms, seg.speaker.max(0) as usize)
            })
            .collect()
    }
}

fn path_string(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The vocabulary sherpa-onnx cuts Parakeet's expected words with, written
/// beside `tokens` from it -- see `speech::hotwords` -- and its path. Written
/// whole under another name and then renamed, every load: sherpa-onnx exits
/// the process on a line it cannot read, so it must never see half a file,
/// and a vocabulary left from another copy of the model must not outlive it.
fn hotword_vocabulary(tokens: &Path) -> std::io::Result<PathBuf> {
    let text = hotwords::vocabulary(&std::fs::read_to_string(tokens)?);
    if text.is_empty() {
        return Err(std::io::Error::other("tokens.txt has no pieces"));
    }
    let dir = tokens.parent().unwrap_or(Path::new("."));
    let path = dir.join(hotwords::VOCABULARY_FILE);
    // Unique to this process and this call, so two loads never write one file.
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nth = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let partial =
        dir.join(format!("{}.{}-{nth}.part", hotwords::VOCABULARY_FILE, std::process::id()));
    std::fs::write(&partial, text)?;
    std::fs::rename(&partial, &path)?;
    Ok(path)
}

/// Merge speech segments across gaps under `merge_gap_ms`, drop anything
/// shorter than `min_ms`, then pad each survivor by `pad_ms` on both ends
/// (clamped to `[0, total_ms]`) and merge again -- padding two segments that
/// were close but not touching can make them overlap.
///
/// Pure and independent of sherpa-onnx, so it is tested directly rather than
/// through a loaded VAD model. See the `#[cfg(test)]` module below.
fn merge_and_pad(
    segments: Vec<(u64, u64)>,
    merge_gap_ms: u64,
    min_ms: u64,
    pad_ms: u64,
    total_ms: u64,
) -> Vec<(u64, u64)> {
    let merged = merge_close(segments, merge_gap_ms);
    let long_enough: Vec<(u64, u64)> =
        merged.into_iter().filter(|(s, e)| e.saturating_sub(*s) >= min_ms).collect();
    let padded: Vec<(u64, u64)> = long_enough
        .into_iter()
        .map(|(s, e)| (s.saturating_sub(pad_ms), (e + pad_ms).min(total_ms)))
        .collect();
    merge_close(padded, 0)
}

/// Sort by start time and merge any two segments whose gap is `< gap_ms`
/// (touching or overlapping segments always merge, regardless of `gap_ms`).
fn merge_close(mut segments: Vec<(u64, u64)>, gap_ms: u64) -> Vec<(u64, u64)> {
    segments.sort_by_key(|s| s.0);
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(segments.len());
    for (start, end) in segments {
        match merged.last_mut() {
            Some(last) if start.saturating_sub(last.1) < gap_ms || start <= last.1 => {
                last.1 = last.1.max(end);
            }
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// [`SpeechKit`], loaded on first use and shared. See this module's doc on
/// sharing.
pub fn speech_kit() -> CommandResult<Arc<SpeechKit>> {
    static KIT: Resident<(), SpeechKit> = Resident::new("speech kit");
    KIT.get((), SpeechKit::load)
}

/// [`speech_kit`] from async code. Loading it again after it was let go of
/// takes seconds, which belong on a blocking thread, not a runtime worker.
pub async fn speech_kit_async() -> CommandResult<Arc<SpeechKit>> {
    crate::service::blocking(speech_kit).await
}

/// [`recogniser`] from async code, for the same reason as
/// [`speech_kit_async`].
pub async fn recogniser_async(model: LocalModel) -> CommandResult<Arc<Recogniser>> {
    crate::service::blocking(move || recogniser(model)).await
}

/// A local recogniser for one [`LocalModel`]: a transducer (Parakeet) or
/// Whisper, both driven through sherpa-onnx's `OfflineRecognizer`, or
/// Qwen3-ASR, run by llama.cpp ([`crate::speech::qwen`]) in a build with
/// the `decisions` feature too.
///
/// # The `Mutex`
///
/// See [`SpeechKit`]'s doc -- the same reasoning applies here. Unlike the
/// VAD detector, `OfflineRecognizer` is not itself a streaming buffer: every
/// call below creates its own fresh `OfflineStream`, feeds it once, and
/// reads one result back. The `Mutex` exists only so two threads sharing one
/// `Arc<Recogniser>` cannot call into the same native `OfflineRecognizer`
/// object at once, not because a call leaves state behind for the next one.
/// Qwen keeps its own, for the same reason.
pub struct Recogniser {
    model: LocalModel,
    engine: Engine,
}

enum Engine {
    Sherpa {
        recognizer: Mutex<sherpa_onnx::OfflineRecognizer>,
        /// Whether it takes words to expect: Parakeet, when its vocabulary
        /// could be written. See `speech::hotwords`.
        boosts: bool,
    },
    #[cfg(feature = "decisions")]
    Qwen(crate::speech::qwen::Qwen),
}

/// What [`Recogniser::transcribe`] heard.
pub struct Transcription {
    pub text: String,
    /// Per-token timestamps, in milliseconds from the start of `samples`,
    /// when the model gives them. Parakeet does, Qwen does not; this crate
    /// does not rely on Whisper's, which sherpa-onnx's docs note are
    /// approximate, since the caller already knows the turn's own start and
    /// end from [`SpeechKit::vad`].
    pub token_timestamps_ms: Option<Vec<u64>>,
}

impl Recogniser {
    /// Load `model`'s files. Blocking.
    pub fn load(model: LocalModel) -> CommandResult<Self> {
        let paths = models::recogniser_paths(model).ok_or_else(|| {
            CommandError::new(codes::NOT_FOUND, format!("{} is not installed", model.as_str()))
        })?;
        let threads = worker_threads();

        let mut config = sherpa_onnx::OfflineRecognizerConfig::default();
        config.model_config.num_threads = threads;
        config.model_config.provider = Some("cpu".to_string());
        let mut boosts = false;
        match paths {
            RecogniserPaths::Transducer { encoder, decoder, joiner, tokens } => {
                config.model_config.transducer = sherpa_onnx::OfflineTransducerModelConfig {
                    encoder: Some(path_string(&encoder)),
                    decoder: Some(path_string(&decoder)),
                    joiner: Some(path_string(&joiner)),
                };
                config.model_config.tokens = Some(path_string(&tokens));
                // Expected words need the beam search and a vocabulary to cut
                // them with -- see `speech::hotwords`. Without the vocabulary
                // it hears as it always did, greedily, told nothing.
                match hotword_vocabulary(&tokens) {
                    Ok(vocabulary) => {
                        config.decoding_method = Some("modified_beam_search".to_string());
                        config.hotwords_score = hotwords::BACKGROUND;
                        config.model_config.modeling_unit =
                            Some(hotwords::MODELING_UNIT.to_string());
                        config.model_config.bpe_vocab = Some(path_string(&vocabulary));
                        boosts = true;
                    }
                    Err(e) => tracing::warn!(
                        model = model.as_str(),
                        error = %e,
                        "could not write the vocabulary expected words are cut with; hearing \
                         without them"
                    ),
                }
            }
            #[cfg(feature = "decisions")]
            RecogniserPaths::Llama { model: weights, mmproj } => {
                let qwen = crate::speech::qwen::Qwen::load(model, weights, mmproj)?;
                return Ok(Self { model, engine: Engine::Qwen(qwen) });
            }
            #[cfg(not(feature = "decisions"))]
            RecogniserPaths::Llama { .. } => {
                return Err(CommandError::new(
                    codes::UNSUPPORTED,
                    format!(
                        "this copy of Jatayu cannot run {}, which needs llama.cpp; choose \
                         Parakeet or Whisper",
                        model.as_str()
                    ),
                ));
            }
            RecogniserPaths::Whisper { encoder, decoder, tokens } => {
                config.model_config.whisper = sherpa_onnx::OfflineWhisperModelConfig {
                    encoder: Some(path_string(&encoder)),
                    decoder: Some(path_string(&decoder)),
                    // Empty string, sherpa-onnx's own spelling of "detect
                    // the language" -- see the Whisper example this crate's
                    // report cites. `Recogniser::transcribe`'s own
                    // `language` argument overrides this per call, when the
                    // stream accepts it; see that method's doc.
                    language: Some(String::new()),
                    task: Some("transcribe".to_string()),
                    tail_paddings: 0,
                    // The turbo export sherpa-onnx publishes has no
                    // cross-attention output, so asking for token timestamps
                    // here only buys a C++-side warning on every call and an
                    // always-`None` `Transcription::token_timestamps_ms` --
                    // see this struct's own doc on why nothing here leans on
                    // Whisper's timestamps anyway.
                    enable_token_timestamps: false,
                    enable_segment_timestamps: false,
                };
                config.model_config.tokens = Some(path_string(&tokens));
            }
        }

        let mut recognizer = sherpa_onnx::OfflineRecognizer::create(&config);
        if recognizer.is_none() && boosts {
            // Refused with the beam search and the vocabulary: hear as it
            // always did, greedily, told nothing, rather than not at all.
            tracing::warn!(
                model = model.as_str(),
                "could not load it to take expected words; hearing without them"
            );
            config.decoding_method = None;
            config.hotwords_score = 0.0;
            config.model_config.modeling_unit = None;
            config.model_config.bpe_vocab = None;
            boosts = false;
            recognizer = sherpa_onnx::OfflineRecognizer::create(&config);
        }
        let recognizer = recognizer.ok_or_else(|| {
            CommandError::new(codes::INTERNAL, format!("could not load {}", model.as_str()))
        })?;
        Ok(Self { model, engine: Engine::Sherpa { recognizer: Mutex::new(recognizer), boosts } })
    }

    pub fn model(&self) -> LocalModel {
        self.model
    }

    /// The graphics cards it runs on, by name, or `None` on the processor --
    /// where Parakeet and Whisper always are.
    pub fn gpu(&self) -> Option<String> {
        match &self.engine {
            Engine::Sherpa { .. } => None,
            #[cfg(feature = "decisions")]
            Engine::Qwen(qwen) => qwen.gpu(),
        }
    }

    /// Run over `samples` as [`Self::transcribe`] does, for timing it:
    /// Qwen, which writes its words one at a time, writes as many as a
    /// person talking for that long would need, even when `samples` say
    /// nothing. Blocking.
    pub fn transcribe_timed(&self, samples: &[i16]) -> CommandResult<()> {
        match &self.engine {
            Engine::Sherpa { .. } => self.transcribe(samples, None).map(drop),
            #[cfg(feature = "decisions")]
            Engine::Qwen(qwen) => qwen.transcribe_timed(&to_f32(samples)).map(drop),
        }
    }

    /// Transcribe one speech turn, told nothing to expect. See
    /// [`Self::transcribe_expecting`].
    pub fn transcribe(
        &self,
        samples: &[i16],
        language: Option<&str>,
    ) -> CommandResult<Transcription> {
        self.transcribe_expecting(samples, language, &Expected::default())
    }

    /// Transcribe one speech turn. `language` is a BCP-47-ish hint; Whisper
    /// accepts it per stream (`OfflineStream::set_option("language", _)`)
    /// and this passes it on when the loaded model exposes that option,
    /// Parakeet's multilingual model does not take one and the hint is
    /// silently unused, and Qwen is told it when it is one of its languages.
    /// `None` means "detect". `expected` are the words it is told to expect:
    /// Qwen reads its prompt, Parakeet boosts its words, and Whisper, which
    /// sherpa-onnx gives no prompt, takes neither. Fails only for Qwen, whose
    /// llama.cpp can run out of room. Blocking.
    // A build without `decisions` has only the one engine to match.
    #[allow(clippy::infallible_destructuring_match)]
    pub fn transcribe_expecting(
        &self,
        samples: &[i16],
        language: Option<&str>,
        expected: &Expected,
    ) -> CommandResult<Transcription> {
        let f32_samples = to_f32(samples);
        let (recognizer, boosts) = match &self.engine {
            Engine::Sherpa { recognizer, boosts } => (recognizer, *boosts),
            #[cfg(feature = "decisions")]
            Engine::Qwen(qwen) => {
                let text = qwen.transcribe(&f32_samples, language, &expected.prompt)?;
                return Ok(Transcription { text, token_timestamps_ms: None });
            }
        };
        let words = if boosts { hotwords::text(&expected.boosted) } else { String::new() };
        let recognizer = recognizer.lock().unwrap();
        let stream = if words.is_empty() {
            recognizer.create_stream()
        } else {
            recognizer.create_stream_with_hotwords(&words)
        };
        if let Some(lang) = language
            && stream.has_option("language")
        {
            stream.set_option("language", lang);
        }
        stream.accept_waveform(SAMPLE_RATE as i32, &f32_samples);
        recognizer.decode(&stream);
        let Some(result) = stream.get_result() else {
            return Ok(Transcription { text: String::new(), token_timestamps_ms: None });
        };
        let token_timestamps_ms = result
            .timestamps
            .map(|ts| ts.iter().map(|t| (f64::from(*t) * 1000.0).round() as u64).collect());
        Ok(Transcription { text: result.text, token_timestamps_ms })
    }
}

static RECOGNISERS: Resident<LocalModel, Recogniser> = Resident::new("speech recogniser");

/// Whether `model` is in memory right now, without loading it.
pub fn recogniser_loaded(model: LocalModel) -> bool {
    RECOGNISERS.is_loaded(&model)
}

/// Whether any recogniser is in memory right now, without loading one.
pub fn any_recogniser_loaded() -> bool {
    RECOGNISERS.any_loaded()
}

/// A [`Recogniser`] for `model`, loaded on first use and shared. See this
/// module's doc on sharing.
pub fn recogniser(model: LocalModel) -> CommandResult<Arc<Recogniser>> {
    RECOGNISERS.get(model, || Recogniser::load(model))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_gaps_merge_and_far_ones_do_not() {
        // Two turns 200ms apart merge (< MERGE_GAP_MS); the third, 400ms
        // after the second, does not.
        let raw = vec![(0, 1000), (1200, 2000), (2400, 3000)];
        let merged = merge_and_pad(raw, MERGE_GAP_MS, 0, 0, 10_000);
        assert_eq!(merged, vec![(0, 2000), (2400, 3000)]);
    }

    #[test]
    fn short_segments_are_dropped_after_merging() {
        // 100ms long: shorter than MIN_SPEECH_MS, and far from its
        // neighbours, so merging does not save it.
        let raw = vec![(0, 1000), (5000, 5100), (9000, 10_000)];
        let merged = merge_and_pad(raw, MERGE_GAP_MS, MIN_SPEECH_MS, 0, 10_000);
        assert_eq!(merged, vec![(0, 1000), (9000, 10_000)]);
    }

    #[test]
    fn survivors_are_padded_and_clamped_to_the_clip() {
        let raw = vec![(0, 100), (9950, 10_000)];
        // Both are shorter than MIN_SPEECH_MS on their own, but padding by
        // PAD_MS on both ends still must not run outside [0, total_ms].
        let merged = merge_and_pad(raw, 0, 0, PAD_MS, 10_000);
        assert_eq!(merged, vec![(0, 250), (9800, 10_000)]);
    }

    #[test]
    fn padding_that_makes_neighbours_touch_merges_them_again() {
        // 200ms apart -- too far to merge before padding (gap 0 here) -- but
        // PAD_MS (150) on *each* side closes a 200ms gap by 300ms, so the
        // padded segments end up overlapping and must merge.
        let raw = vec![(0, 1000), (1200, 2000)];
        let merged = merge_and_pad(raw, 0, 0, PAD_MS, 10_000);
        assert_eq!(merged, vec![(0, 2150)]);
    }

    #[test]
    fn an_empty_clip_has_no_turns() {
        assert_eq!(merge_and_pad(Vec::new(), MERGE_GAP_MS, MIN_SPEECH_MS, PAD_MS, 0), Vec::new());
    }

    #[test]
    fn l2_normalise_yields_unit_length_unless_the_vector_is_zero() {
        let v = l2_normalise(vec![3.0, 4.0]);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "expected unit length, got {norm}");

        let zero = l2_normalise(vec![0.0, 0.0]);
        assert_eq!(zero, vec![0.0, 0.0]);
    }

    #[test]
    fn worker_threads_is_at_least_one_and_never_more_than_the_cap() {
        let n = worker_threads();
        assert!((1..=MAX_THREADS as i32).contains(&n), "got {n}");
    }
}
