//! The Qwen3-ASR recognisers against the real models, on speech made by
//! the real local voice: what each hears, and how long it takes, beside
//! Parakeet's.
//!
//! `#[ignore]`, and gated a second time by `KARMAYOGA_TEST_SPEECH=1`, as
//! `speech_real` is. Needs the `speech` and `decisions` features, and the
//! models in `models_dir()`: `qwen3Asr17b`, `qwen3Asr06b`, `parakeetV3` and
//! `phonon` (the voice is downloaded if it is missing; the recognisers are
//! not, being gigabytes). Point `XDG_DATA_HOME` at a scratch directory whose
//! `karmayoga/models` holds them.
//!
//! Where it runs: on the graphics card when there is room, as the app does.
//! `VK_ICD_FILENAMES=/dev/null` hides every Vulkan driver, which keeps it on
//! the processor.

#![cfg(all(feature = "speech", feature = "decisions"))]

use karmayoga_core::llm::ModelSettings;
use karmayoga_core::meeting::{LocalModel, SAMPLE_RATE};
use karmayoga_core::speech::{LocalVoice, SynthesiserConfig, VoiceConfig};
use karmayoga_service::local::files as models;
use karmayoga_service::speech::local as speech;
use karmayoga_service::speech::synth::{Speech, build_synthesiser};
use std::time::{Duration, Instant};

#[allow(dead_code)]
mod support;

const SAID: &[&str] = &[
    "Please send the quarterly report to Maria before Friday.",
    "The meeting with the Bangalore team moved to half past three.",
    "Remind me to buy coriander, tomatoes and a dozen eggs on the way home.",
];

fn enabled() -> bool {
    std::env::var("KARMAYOGA_TEST_SPEECH").as_deref() == Ok("1")
}

async fn install_voice() {
    if !models::voice_installed(LocalVoice::Phonon) {
        models::start_download(LocalVoice::Phonon.id()).expect("the download starts");
    }
    let began = Instant::now();
    while !models::voice_installed(LocalVoice::Phonon) {
        if let Some(error) = models::last_error(LocalVoice::Phonon.id()) {
            panic!("the voice did not download: {error}");
        }
        assert!(began.elapsed() < Duration::from_secs(300), "the voice took too long");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// To 16 kHz, averaging what each output sample covers.
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

/// Lower case, letters and digits only, one space between words.
fn plain(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// How many of `expected`'s words `heard` has, as a share.
fn words_heard(expected: &str, heard: &str) -> f32 {
    let heard: Vec<String> = plain(heard).split(' ').map(str::to_string).collect();
    let expected = plain(expected);
    let words: Vec<&str> = expected.split(' ').collect();
    words.iter().filter(|w| heard.iter().any(|h| h == *w)).count() as f32 / words.len() as f32
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "real models; set KARMAYOGA_TEST_SPEECH=1"]
async fn qwen_hears_what_the_voice_says() {
    if !enabled() {
        eprintln!("KARMAYOGA_TEST_SPEECH is not 1; skipping");
        return;
    }
    for model in [LocalModel::Qwen3Asr17b, LocalModel::Qwen3Asr06b, LocalModel::ParakeetV3] {
        assert!(
            models::installed(model),
            "{} is not in {:?}",
            model.as_str(),
            models::models_dir()
        );
    }
    install_voice().await;

    let (svc, _dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let settings = ModelSettings {
        voice: Some(VoiceConfig::new(SynthesiserConfig::Local {
            voice: LocalVoice::Phonon,
            speaker: 5,
        })),
        ..Default::default()
    };
    vault.save_model_settings(&settings).unwrap();
    let voice = build_synthesiser(vault.voice_access().unwrap()).await.unwrap();
    let mut clips = Vec::new();
    for line in SAID {
        clips.push(to_16k(&voice.synthesise(line).await.unwrap()));
    }
    // Over a minute of all three, a second's pause between them, so the
    // recogniser has to cut it into windows.
    let pause = vec![0i16; SAMPLE_RATE as usize];
    let long: Vec<i16> =
        (0..4).flat_map(|_| clips.iter().flat_map(|c| c.iter().chain(&pause)).copied()).collect();
    let long_said = SAID.join(" ").repeat(4);

    for model in [LocalModel::Qwen3Asr17b, LocalModel::Qwen3Asr06b, LocalModel::ParakeetV3] {
        let started = Instant::now();
        let recogniser = speech::recogniser_async(model).await.unwrap();
        let loaded = started.elapsed();
        let gpu = recogniser.gpu().unwrap_or("the processor").to_string();
        eprintln!("{}: loaded on {gpu} in {loaded:.1?}", model.as_str());

        for (line, clip) in SAID.iter().zip(&clips) {
            let r = recogniser.clone();
            let samples = clip.clone();
            let started = Instant::now();
            let heard = tokio::task::spawn_blocking(move || r.transcribe(&samples, Some("en")))
                .await
                .unwrap()
                .unwrap()
                .text;
            let took = started.elapsed();
            let seconds = clip.len() as f64 / f64::from(SAMPLE_RATE);
            let share = words_heard(line, &heard);
            eprintln!(
                "  {:.1}s clip in {took:.2?} ({:.1}x real time), {:.0}% of words: {heard:?}",
                seconds,
                seconds / took.as_secs_f64(),
                share * 100.0
            );
            assert!(share >= 0.8, "{} heard {heard:?} for {line:?}", model.as_str());
        }

        // Undetected language, a long clip.
        let r = recogniser.clone();
        let samples = long.clone();
        let started = Instant::now();
        let heard = tokio::task::spawn_blocking(move || r.transcribe(&samples, None))
            .await
            .unwrap()
            .unwrap();
        let took = started.elapsed();
        let seconds = long.len() as f64 / f64::from(SAMPLE_RATE);
        let share = words_heard(&long_said, &heard.text);
        eprintln!(
            "  {seconds:.0}s clip, language not given, in {took:.2?} ({:.1}x real time), \
             {:.0}% of words",
            seconds / took.as_secs_f64(),
            share * 100.0
        );
        assert!(share >= 0.8, "{} heard {:?}", model.as_str(), heard.text);
        assert!(
            plain(&heard.text).matches("coriander").count() >= 3,
            "every window was heard: {:?}",
            heard.text
        );

        // What "Check speed" measures.
        let r = recogniser.clone();
        let noise: Vec<i16> = (0..10 * SAMPLE_RATE as usize)
            .map(|i| ((i.wrapping_mul(2_654_435_761) % 2000) as i16) - 1000)
            .collect();
        let started = Instant::now();
        tokio::task::spawn_blocking(move || r.transcribe_timed(&noise)).await.unwrap().unwrap();
        eprintln!("  check speed: {:.1}x real time", 10.0 / started.elapsed().as_secs_f64());
    }
}
