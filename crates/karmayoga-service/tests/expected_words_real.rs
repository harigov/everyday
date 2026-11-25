//! The words to expect, against the real recognisers. Kokoro says sentences
//! full of names an everyday recogniser has never heard, in four voices, and
//! each recogniser hears every one twice: told nothing, and told the words a
//! conversation with the assistant would be told (`words::expected`). It
//! prints both, and checks that being told helps and that sentences with none
//! of the names in them come out the same.
//!
//! `#[ignore]`, and gated a second time by `KARMAYOGA_TEST_SPEECH=1`, as
//! `kokoro_real.rs` is: it runs real inference, needs Parakeet and Kokoro in
//! the models folder, and the Qwen test downloads Qwen3-ASR 0.6B if it is not
//! there. Point `XDG_DATA_HOME` at a scratch folder whose
//! `karmayoga/models` links to the real models.

#![cfg(feature = "speech")]

use karmayoga_core::meeting::{LocalModel, SAMPLE_RATE};
use karmayoga_core::speech::words::{self, Expected, Occasion, WordSources};
use karmayoga_core::speech::{
    KOKORO_SPEAKERS, LocalVoice, SynthesiserConfig, VoiceAccess, VoiceConfig,
};
use karmayoga_service::local::files as models;
use karmayoga_service::speech::local as speech;
use karmayoga_service::speech::synth::{Speech, Synthesiser, build_synthesiser};
use std::sync::Arc;
use std::time::Instant;

fn enabled() -> bool {
    std::env::var("KARMAYOGA_TEST_SPEECH").as_deref() == Ok("1")
}

/// What is said, and the names in it a listener should hear.
const NAMED: &[(&str, &[&str])] = &[
    ("Karma, add a task to call Priya Shah tomorrow.", &["karma", "priya shah"]),
    ("Remind me to message Srinivasan about the Konark project.", &["srinivasan", "konark"]),
    (
        "Schedule lunch with Lakshmi Venkatesh in Koramangala on Friday.",
        &["lakshmi venkatesh", "koramangala"],
    ),
    ("Ask Siddharth to look at the Tailscale settings.", &["siddharth", "tailscale"]),
    ("Jarvis, open Obsidian and find my notes.", &["jarvis", "obsidian"]),
    ("Send the invoice to Hari Govardhanam today.", &["govardhanam"]),
];

/// Sentences with none of the names in them.
const PLAIN: &[&str] = &[
    "What is the weather like this afternoon?",
    "Add milk and eggs to my shopping list.",
    "Turn the volume down a little.",
    "Call my mother when I get home.",
];

const VOICES: &[&str] = &["af_heart", "am_michael", "bf_emma", "bm_george"];

/// What a conversation with the assistant would be told, from a vault that
/// knows these people and words.
fn told() -> Expected {
    let sources = WordSources {
        assistant: vec!["Karma".into(), "Jarvis".into()],
        owner: "Hari Govardhanam".into(),
        own: vec!["Konark".into(), "Tailscale".into()],
        people: vec![
            "Priya Shah".into(),
            "Srinivasan".into(),
            "Lakshmi Venkatesh".into(),
            "Siddharth".into(),
        ],
        places: vec!["Koramangala".into()],
        apps: vec!["Obsidian".into()],
        ..WordSources::default()
    };
    words::expected(Occasion::Conversation, &sources)
}

async fn kokoro(name: &str) -> Arc<dyn Synthesiser> {
    let speaker = KOKORO_SPEAKERS.iter().position(|s| *s == name).unwrap() as u32;
    let config = VoiceConfig::new(SynthesiserConfig::Local { voice: LocalVoice::Kokoro, speaker });
    build_synthesiser(VoiceAccess { config, key: None, pronunciations: Vec::new() })
        .await
        .expect("Kokoro loads")
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

/// Every sentence in every voice, at 16 kHz.
async fn clips() -> Vec<(String, String, Vec<i16>)> {
    let mut out = Vec::new();
    for voice in VOICES {
        let synth = kokoro(voice).await;
        for said in NAMED.iter().map(|(s, _)| *s).chain(PLAIN.iter().copied()) {
            let speech = synth.synthesise(said).await.expect("it speaks");
            out.push((voice.to_string(), said.to_string(), to_16k(&speech)));
        }
    }
    out
}

/// Heard told nothing and told [`told`], with the time each took, for every
/// clip; then the totals checked.
async fn compare(model: LocalModel) {
    let recogniser = speech::recogniser_async(model).await.expect("the recogniser loads");
    if let Some(gpu) = recogniser.gpu() {
        println!("{} on {gpu}", model.as_str());
    }
    let told = Arc::new(told());
    println!("prompt: {}\nboosted: {:?}", told.prompt, told.boosted);

    let (mut names, mut heard_bare, mut heard_told) = (0, 0, 0);
    let (mut bare_secs, mut told_secs) = (0.0, 0.0);
    let mut plain_changed = Vec::new();
    for (voice, said, samples) in clips().await {
        let r = recogniser.clone();
        let t = told.clone();
        let (bare, told_text, b, w) = tokio::task::spawn_blocking(move || {
            let began = Instant::now();
            let bare = r.transcribe(&samples, Some("en")).unwrap().text;
            let b = began.elapsed().as_secs_f64();
            let began = Instant::now();
            let told_text = r.transcribe_expecting(&samples, Some("en"), &t).unwrap().text;
            (bare, told_text, b, began.elapsed().as_secs_f64())
        })
        .await
        .unwrap();
        bare_secs += b;
        told_secs += w;
        let expected: &[&str] =
            NAMED.iter().find(|(s, _)| *s == said).map(|(_, n)| *n).unwrap_or(&[]);
        for name in expected {
            names += 1;
            heard_bare += usize::from(plain(&bare).contains(name));
            heard_told += usize::from(plain(&told_text).contains(name));
        }
        let mark = if plain(&bare) == plain(&told_text) { " " } else { "*" };
        println!("{mark} {voice:10} {said}\n    bare: {bare}\n    told: {told_text}");
        if expected.is_empty() && plain(&bare) != plain(&told_text) {
            plain_changed.push((said, bare, told_text));
        }
    }
    println!(
        "{}: names heard {heard_bare}/{names} told nothing, {heard_told}/{names} told; \
         {bare_secs:.2}s and {told_secs:.2}s; plain sentences changed: {}",
        model.as_str(),
        plain_changed.len()
    );
    assert!(heard_told >= heard_bare, "being told made it worse: {heard_told} < {heard_bare}");
    assert!(heard_told > heard_bare, "being told did not help: {heard_told} = {heard_bare}");
    // A sentence with none of the names may come out a little differently,
    // never with one of them in it.
    for (said, _, told_text) in &plain_changed {
        for word in told.boosted.iter().map(|w| plain(w)) {
            assert!(
                !plain(told_text).contains(&word),
                "{word:?} was heard in {said:?}: {told_text}"
            );
        }
    }
    assert!(plain_changed.len() <= 2, "too many plain sentences changed: {plain_changed:?}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs real models; needs KARMAYOGA_TEST_SPEECH=1"]
async fn parakeet_hears_the_names_it_is_told_to_expect() {
    if !enabled() {
        return;
    }
    assert!(models::installed(LocalModel::ParakeetV3), "Parakeet is needed");
    assert!(models::voice_installed(LocalVoice::Kokoro), "Kokoro is needed");
    compare(LocalModel::ParakeetV3).await;
}

#[cfg(feature = "decisions")]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads Qwen3-ASR 0.6B and runs real models; needs KARMAYOGA_TEST_SPEECH=1"]
async fn qwen_hears_the_names_it_is_told_to_expect() {
    if !enabled() {
        return;
    }
    assert!(models::voice_installed(LocalVoice::Kokoro), "Kokoro is needed");
    let model = LocalModel::Qwen3Asr06b;
    let id = "qwen3Asr06b";
    if !models::installed(model) {
        models::start_download(id).expect("the download starts");
        let began = Instant::now();
        while !models::installed(model) {
            if let Some(error) = models::last_error(id) {
                panic!("Qwen did not download: {error}");
            }
            assert!(began.elapsed().as_secs() < 1800, "Qwen took too long");
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
    compare(model).await;
}
