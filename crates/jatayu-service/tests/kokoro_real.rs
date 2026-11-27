//! Kokoro against the real model: what it says, heard back by Parakeet.
//!
//! `#[ignore]`, and gated a second time by `JATAYU_TEST_SPEECH=1`, as
//! `voice_real.rs` is: it downloads Kokoro (`models/kokoro`, about 350 MB, if
//! it is not already there) and runs real inference. Parakeet must already be
//! in the models folder. A pass means the words came out intelligible: names
//! the lexicons lack, numbers, both accents, a run long enough to be cut into
//! pieces, and the speed.

#![cfg(feature = "speech")]

use jatayu_core::meeting::{LocalModel, SAMPLE_RATE};
use jatayu_core::speech::{
    KOKORO_SPEAKERS, LocalVoice, SynthesiserConfig, VoiceAccess, VoiceConfig,
};
use jatayu_service::local::files as models;
use jatayu_service::speech::local as speech;
use jatayu_service::speech::synth::{Speech, Synthesiser, build_synthesiser};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn enabled() -> bool {
    std::env::var("JATAYU_TEST_SPEECH").as_deref() == Ok("1")
}

async fn install_kokoro() {
    if !models::voice_installed(LocalVoice::Kokoro) {
        models::start_download(LocalVoice::Kokoro.id()).expect("the download starts");
    }
    let began = Instant::now();
    while !models::voice_installed(LocalVoice::Kokoro) {
        if let Some(error) = models::last_error(LocalVoice::Kokoro.id()) {
            panic!("Kokoro did not download: {error}");
        }
        assert!(began.elapsed() < Duration::from_secs(900), "Kokoro took too long");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn kokoro(speaker: u32, speed: f32) -> Arc<dyn Synthesiser> {
    let mut config =
        VoiceConfig::new(SynthesiserConfig::Local { voice: LocalVoice::Kokoro, speaker });
    config.speed = speed;
    build_synthesiser(VoiceAccess { config, key: None, pronunciations: Vec::new() })
        .await
        .expect("Kokoro loads")
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

async fn heard(speech: &Speech) -> String {
    let samples = to_16k(speech);
    let recogniser = speech::recogniser_async(LocalModel::ParakeetV3).await.unwrap();
    tokio::task::spawn_blocking(move || recogniser.transcribe(&samples, Some("en")).unwrap().text)
        .await
        .unwrap()
        .to_lowercase()
}

fn seconds(speech: &Speech) -> f32 {
    speech.samples.len() as f32 / speech.sample_rate as f32
}

/// Writes what was said where `JATAYU_TEST_KOKORO_WAVS` points, for a
/// person to listen to.
fn keep(name: &str, speech: &Speech) {
    let Ok(dir) = std::env::var("JATAYU_TEST_KOKORO_WAVS") else { return };
    let path = std::path::Path::new(&dir).join(format!("{name}.wav"));
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: speech.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav = hound::WavWriter::create(&path, spec).unwrap();
    for sample in &speech.samples {
        wav.write_sample(*sample).unwrap();
    }
    wav.finalize().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads Kokoro and runs real models; needs JATAYU_TEST_SPEECH=1"]
async fn kokoro_says_what_it_is_given_in_every_accent() {
    if !enabled() {
        return;
    }
    assert!(models::installed(LocalModel::ParakeetV3), "Parakeet is needed");
    install_kokoro().await;

    // Words no lexicon has, a number and a sum of money, in an American and
    // a British voice of each kind.
    const SAID: &str = "Good morning. Your meeting with Priya Govardhanam starts at 9:30, \
        and the invoice for $1,250 is still unpaid.";
    for name in ["af_heart", "am_michael", "bf_emma", "bm_george"] {
        let speaker = KOKORO_SPEAKERS.iter().position(|s| *s == name).unwrap() as u32;
        let voice = kokoro(speaker, 1.0).await;
        let began = Instant::now();
        let speech = voice.synthesise(SAID).await.expect("it speaks");
        let took = began.elapsed().as_secs_f32();
        assert_eq!(speech.sample_rate, 24_000);
        keep(name, &speech);
        let text = heard(&speech).await;
        println!("{name}: {:.2}s of speech in {took:.2}s: {text}", seconds(&speech));
        for word in ["morning", "meeting", "priya", "invoice", "unpaid"] {
            assert!(text.contains(word), "{name} did not say {word:?}: {text}");
        }
        assert!(text.contains("nine") || text.contains("9"), "{name}: {text}");
    }

    // Long enough to be cut into pieces: every piece is spoken, the last one
    // too.
    let long = "The weather this week will be mild, with light rain on Tuesday. ".repeat(6)
        + "Finally, remember to call your sister about the birthday dinner.";
    let voice = kokoro(LocalVoice::Kokoro.default_speaker(), 1.0).await;
    let speech = voice.synthesise(&long).await.expect("it speaks at length");
    keep("long", &speech);
    let text = heard(&speech).await;
    println!("long: {:.2}s: {text}", seconds(&speech));
    assert!(text.matches("tuesday").count() >= 5, "{text}");
    assert!(text.contains("sister") && text.contains("birthday"), "{text}");

    // The speed is Kokoro's own: faster is shorter, and still words.
    let normal = kokoro(3, 1.0).await.synthesise("This is how quickly I talk.").await.unwrap();
    let quick = kokoro(3, 1.5).await.synthesise("This is how quickly I talk.").await.unwrap();
    keep("quick", &quick);
    let (normal_s, quick_s) = (seconds(&normal), seconds(&quick));
    println!("speed: {normal_s:.2}s at 1, {quick_s:.2}s at 1.5");
    assert!(quick_s < normal_s * 0.85, "{quick_s} is not much shorter than {normal_s}");
    assert!(heard(&quick).await.contains("quickly"));

    // Nothing to say is said so, before the model is asked.
    assert!(voice.synthesise("   ").await.is_err());
}
