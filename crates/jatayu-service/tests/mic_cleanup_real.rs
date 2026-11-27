//! The microphone's filters and the owner's voice check, on the real models:
//! VoxLock over the recorded call its author publishes, and two of Phonon's
//! voices, one standing in for the person and one for somebody else in the
//! room, for the noise filter and the check.
//!
//! Downloads both filters through the catalogue, which checks every pinned
//! size and hash, and Phonon if it is not there. Needs the speech kit and
//! Parakeet already in the data directory:
//!
//! ```sh
//! JATAYU_TEST_SPEECH=1 XDG_DATA_HOME=<scratch> cargo test -p jatayu-service \
//!   --features speech --release --test mic_cleanup_real -- --ignored --nocapture
//! ```
#![cfg(feature = "speech")]

use jatayu_core::meeting::{LocalModel, SAMPLE_RATE};
use jatayu_core::speech::{LocalVoice, SynthesiserConfig, VoiceAccess, VoiceConfig};
use jatayu_service::error::CommandResult;
use jatayu_service::local::files as models;
use jatayu_service::speech::cleanup::{Cleanup, MicCleanup};
use jatayu_service::speech::local as speech;
use jatayu_service::speech::owner::{self, OwnerOnly, OwnerVoice, Strictness};
use jatayu_service::speech::synth::{Speech, Synthesiser, build_synthesiser};
use jatayu_service::speech::transcribe::BoxFuture;
use jatayu_service::speech::utterance::UtteranceTranscriber;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn enabled() -> bool {
    std::env::var("JATAYU_TEST_SPEECH").as_deref() == Ok("1")
}

async fn install(id: &str, installed: fn() -> bool) {
    if !installed() {
        models::start_download(id).expect("the download starts");
    }
    let began = Instant::now();
    while !installed() {
        if let Some(error) = models::last_error(id) {
            panic!("{id} did not download: {error}");
        }
        assert!(began.elapsed() < Duration::from_secs(600), "{id} took too long");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn to_16k(speech: &Speech) -> Vec<i16> {
    let ratio = speech.sample_rate as f64 / f64::from(SAMPLE_RATE);
    let count = (speech.samples.len() as f64 / ratio) as usize;
    (0..count)
        .map(|i| {
            let from = (i as f64 * ratio) as usize;
            let to = (((i + 1) as f64 * ratio) as usize).clamp(from + 1, speech.samples.len());
            let part = &speech.samples[from.min(speech.samples.len() - 1)..to];
            (part.iter().map(|s| i64::from(*s)).sum::<i64>() / part.len() as i64) as i16
        })
        .collect()
}

async fn voice(speaker: u32) -> Arc<dyn Synthesiser> {
    build_synthesiser(VoiceAccess {
        config: VoiceConfig::new(SynthesiserConfig::Local { voice: LocalVoice::Phonon, speaker }),
        key: None,
        pronunciations: Vec::new(),
    })
    .await
    .unwrap()
}

async fn say(voice: &Arc<dyn Synthesiser>, lines: &[&str]) -> Vec<i16> {
    let mut out = Vec::new();
    for line in lines {
        out.extend(to_16k(&voice.synthesise(line).await.unwrap()));
        out.extend(std::iter::repeat_n(0i16, (SAMPLE_RATE / 4) as usize));
    }
    out
}

fn rms(x: &[i16]) -> f64 {
    (x.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

#[allow(dead_code)]
fn mix(a: &[i16], b: &[i16], db: f64) -> Vec<i16> {
    let gain = rms(a) / rms(b).max(1.0) * 10f64.powf(db / 20.0);
    a.iter()
        .zip(b.iter().cycle())
        .map(|(&x, &y)| (f64::from(x) + f64::from(y) * gain).clamp(-32768.0, 32767.0) as i16)
        .collect()
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// How many of `said`'s words the transcript of `audio` has.
fn caught(recogniser: &speech::Recogniser, audio: &[i16], said: &str) -> usize {
    let heard = words(&recogniser.transcribe(audio, Some("en")).unwrap().text);
    words(said).iter().filter(|w| heard.contains(w)).count()
}

fn clean(cleanup: &Cleanup, audio: &[i16]) -> Vec<i16> {
    let mut models = MicCleanup::build(cleanup).unwrap().expect("something was asked for");
    let mut out = Vec::new();
    for block in audio.chunks(480) {
        models.process(block, &mut out);
    }
    out
}

const OWNER: &[&str] = &[
    "I'd like to hear what's on my calendar for the rest of the week, please.",
    "Remind me to call my sister about the trip on Saturday afternoon.",
    "Can you move the design review to Thursday morning and tell everybody?",
    "Write down that the plumber is coming between nine and eleven tomorrow.",
];
const OWNER_LATER: &str = "Please send a short note to Maria saying I will be late today.";
const STRANGER: &[&str] = &[
    "The match went into extra time and the crowd would not stop singing.",
    "After the break we will look at the weather for the coming weekend.",
    "Prices at the market rose again this month, mostly for fresh fruit.",
];

/// A WAV published with VoxLock: where it is, its SHA-256 and size.
struct Published {
    url: &'static str,
    sha256: &'static str,
    bytes: usize,
}

const TEST_CALL: Published = Published {
    url: "https://github.com/arbazsiddiqui/voxlock/releases/download/sherpa-onnx-models/voxlock_test_call.wav",
    sha256: "16342ff0b5cefb64cc012a3185c641f8762a78803fa0d9f4966b288669b132b4",
    bytes: 448_364,
};

const TEST_CALLER: Published = Published {
    url: "https://github.com/arbazsiddiqui/voxlock/releases/download/sherpa-onnx-models/voxlock_test_voice_sample.wav",
    sha256: "a3cd4eba63f33ae1001a73144286e877802c141f8189c84ea1461b332817255f",
    bytes: 96_044,
};

/// Download `file`, check it, and read it as 16 kHz mono.
async fn fetch(file: Published) -> Vec<i16> {
    use sha2::Digest;
    let bytes = reqwest::get(file.url).await.unwrap().bytes().await.unwrap();
    assert_eq!(bytes.len(), file.bytes, "{}", file.url);
    let hash: String = sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hash, file.sha256, "{}", file.url);
    let speech = jatayu_service::speech::wav::decode(&bytes).unwrap();
    assert_eq!(speech.sample_rate, SAMPLE_RATE);
    speech.samples
}

/// What a transcriber behind the check was handed, as words, so a test can
/// see whether anything got through.
struct Counts;

impl UtteranceTranscriber for Counts {
    fn hear<'a>(&'a self, samples: &'a [i16]) -> BoxFuture<'a, CommandResult<String>> {
        Box::pin(async move { Ok(format!("{} samples", samples.len())) })
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads models and runs them; needs JATAYU_TEST_SPEECH=1"]
async fn the_microphone_keeps_its_owner_and_drops_the_room() {
    if !enabled() {
        return;
    }
    assert!(models::speech_kit_installed(), "the speech kit is needed");
    assert!(models::installed(LocalModel::ParakeetV3), "Parakeet is needed");
    install(models::VOICE_ISOLATION_ID, models::voice_isolation_installed).await;
    install(models::NOISE_FILTER_ID, models::noise_filter_installed).await;
    install(LocalVoice::Phonon.id(), || models::voice_installed(LocalVoice::Phonon)).await;
    let recogniser = speech::recogniser(LocalModel::ParakeetV3).unwrap();
    let kit = speech::speech_kit().unwrap();

    // Speaker 0 is a woman's voice, 6 a man's.
    let (me, them) = (voice(0).await, voice(6).await);
    let recording = say(&me, OWNER).await;
    let sample = owner::best_stretch(&recording).to_vec();
    let later = say(&me, &[OWNER_LATER]).await;
    let room = say(&them, STRANGER).await;

    // ---- taking other voices out --------------------------------------------
    // VoxLock held on to neither of two Phonon voices, while it held on to a
    // person recorded at a desk; so this half is a real recording, the one its
    // author publishes: a caller with somebody talking behind them, and three
    // seconds of the caller alone.
    let call = fetch(TEST_CALL).await;
    let caller = fetch(TEST_CALLER).await;
    let isolate = Cleanup { strong_noise: false, voice_sample: Some(caller) };
    let started = Instant::now();
    let isolated = clean(&isolate, &call);
    let spent = started.elapsed().as_secs_f64();
    // The caller talks for the first ten seconds; the last four are the
    // other voice alone, straight after them.
    let (theirs_from, ours_to) = (11 * SAMPLE_RATE as usize, 10 * SAMPLE_RATE as usize);
    println!(
        "caller {:.0} -> {:.0} rms; the other voice after them {:.0} -> {:.0}; {:.2}x real time, loading included",
        rms(&call[..ours_to]),
        rms(&isolated[..ours_to]),
        rms(&call[theirs_from..]),
        rms(&isolated[theirs_from..]),
        spent / (call.len() as f64 / 16_000.0)
    );
    assert!(rms(&isolated[..ours_to]) > rms(&call[..ours_to]) * 0.5, "the caller is kept");
    assert!(
        rms(&isolated[theirs_from..]) < rms(&call[theirs_from..]) / 10.0,
        "the other voice after them loses at least 20 dB"
    );
    let said = recogniser.transcribe(&isolated[..ours_to], Some("en")).unwrap().text;
    println!("the caller, isolated: {said}");
    let caller_words = ["poyser", "poiser", "dairy", "milk", "captain", "blushing", "interested"];
    let found = words(&said).iter().filter(|w| caller_words.contains(&w.as_str())).count();
    assert!(found >= 3, "the caller is still understood: {said}");

    // ---- the stronger noise filter ------------------------------------------
    let mut state = 7u32;
    let hiss: Vec<i16> = (0..later.len())
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (((state >> 8) as f32 / (1 << 23) as f32 - 1.0) * 3000.0) as i16
        })
        .collect();
    let noisy: Vec<i16> = later.iter().zip(&hiss).map(|(&a, &b)| a.saturating_add(b)).collect();
    let denoise = Cleanup { strong_noise: true, voice_sample: None };
    let quiet = clean(&denoise, &hiss);
    let cleaned = clean(&denoise, &noisy);
    println!(
        "noise alone {:.0} -> {:.0} rms; words caught {} raw, {} filtered",
        rms(&hiss),
        rms(&quiet),
        caught(&recogniser, &noisy, OWNER_LATER),
        caught(&recogniser, &cleaned, OWNER_LATER)
    );
    assert!(rms(&quiet) < rms(&hiss) / 4.0, "steady noise loses at least 12 dB");
    assert!(
        caught(&recogniser, &cleaned, OWNER_LATER) * 10 >= words(OWNER_LATER).len() * 7,
        "the owner is still understood through it"
    );

    // ---- the voice check ----------------------------------------------------
    let enrolled = kit.embed(&sample).expect("an embedding of the sample");
    let voice = Arc::new(OwnerVoice { centroids: vec![enrolled], sample: None });
    let checked = OwnerOnly {
        inner: Arc::new(Counts),
        voice: voice.clone(),
        check: Arc::new(std::sync::Mutex::new(Some(Strictness::Balanced))),
    };
    let mine = checked.hear(&later).await.unwrap();
    let theirs = checked.hear(&room).await.unwrap();
    println!(
        "similarity: owner {:.2}, stranger {:.2}; heard {mine:?} and {theirs:?}",
        voice.similarity(&kit.embed(&later).unwrap()),
        voice.similarity(&kit.embed(&room).unwrap()),
    );
    let sims: Vec<String> = owner::slices(room.len())
        .into_iter()
        .map(|r| {
            let level = rms(&room[r.clone()]);
            let sim = kit.embed(&room[r.clone()]).map(|e| voice.similarity(&e));
            format!(
                "{:.1}s:{}@{level:.0}",
                r.start as f64 / 16000.0,
                sim.map_or("-".into(), |s| format!("{s:.2}"))
            )
        })
        .collect();
    println!("stranger slices: {}", sims.join(" "));
    assert!(!mine.is_empty(), "the owner is heard");
    assert!(theirs.is_empty(), "the stranger is not");
    // Both in one utterance, the stranger first: only the owner's part.
    let mut both = room.clone();
    both.extend_from_slice(&later);
    let heard = checked.hear(&both).await.unwrap();
    let kept: usize = heard.split(' ').next().unwrap().parse().unwrap();
    println!("stranger then owner: {kept} of {} samples heard", both.len());
    assert!(kept < both.len() * 3 / 4, "most of the stranger is cut");
    assert!(kept > later.len() / 2, "and the owner kept");
}
