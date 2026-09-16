//! Real verification against the actual sherpa-onnx models and real,
//! synthesised speech -- not a mock, not a fixture of pre-computed numbers.
//!
//! `#[ignore]`, and gated a second time by `EVERYDAY_TEST_SPEECH=1`: unlike
//! this crate's other environment-gated integration tests (`mailsync_dovecot`,
//! `caldav_docker`), which print why they did nothing and pass when their
//! variable is unset, this one downloads the better part of a gigabyte of
//! models on its first run and then runs real ONNX inference several times
//! over -- minutes of work even on a fast link, and not something a plain
//! `cargo test --features speech` (which every one of this phase's other
//! five engineers, and CI, run routinely) should ever do by surprise. Both
//! `#[ignore]` *and* the environment variable have to be asked for --
//! `cargo test -- --ignored` with `EVERYDAY_TEST_SPEECH` unset still skips.
//!
//! # Fixtures
//!
//! `$HOME/.cache/everyday-test-tmp/speech/piper-voices/` holds a short
//! two-speaker conversation, synthesised with two different Piper voices
//! (`en_US-amy-medium`, `en_US-ryan-high`, from `rhasspy/piper-voices` on
//! Hugging Face) and downsampled to 16 kHz mono with `ffmpeg` -- not part of
//! this repository; see this phase's report for exactly how it was built.
//! Models come from the real network, into the real [`models::models_dir`],
//! the same path the application itself would use.

#![cfg(feature = "speech")]

use everyday_core::meeting::{LocalModel, SAMPLE_RATE};
use everyday_service::meeting::models;
use everyday_service::meeting::speech;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn enabled() -> bool {
    std::env::var("EVERYDAY_TEST_SPEECH").as_deref() == Ok("1")
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME should be set"))
        .join(".cache/everyday-test-tmp/speech/piper-voices")
}

/// A 16 kHz mono WAV, read whole. Panics with the path on any mismatch --
/// there is no caller here that should ever see a WAV in the wrong shape.
fn read_wav(path: &Path) -> Vec<i16> {
    let mut reader = hound::WavReader::open(path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    let spec = reader.spec();
    assert_eq!(spec.sample_rate, SAMPLE_RATE, "{} is not 16 kHz", path.display());
    assert_eq!(spec.channels, 1, "{} is not mono", path.display());
    reader
        .samples::<i16>()
        .map(|s| s.unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .collect()
}

/// Cosine similarity of two already-L2-normalised vectors: just their dot
/// product.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Start `id` downloading if it is not already installed, and wait for it.
async fn ensure_installed(id: &str) {
    if models::status(id).is_some_and(|s| s.installed) {
        eprintln!("{id}: already installed");
        return;
    }
    eprintln!("{id}: downloading...");
    models::start_download(id).unwrap_or_else(|e| panic!("{id}: {e}"));
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let status = models::status(id).unwrap_or_else(|| panic!("{id} is not in the catalogue"));
        if let Some(error) = &status.error {
            panic!("{id} failed to download: {error}");
        }
        if let Some((done, total)) = status.progress {
            eprintln!("{id}: {done}/{total} bytes");
        }
        if status.installed {
            eprintln!("{id}: installed");
            break;
        }
    }
}

#[tokio::test]
#[ignore]
async fn real_models_hear_what_was_actually_said() {
    if !enabled() {
        eprintln!(
            "skipped: set EVERYDAY_TEST_SPEECH=1 to run this against real models and real audio"
        );
        return;
    }
    let fixtures = fixtures_dir();
    assert!(
        fixtures.join("conversation.wav").is_file(),
        "expected piper fixtures at {} -- see this file's own doc on how they are built",
        fixtures.display()
    );

    ensure_installed(models::SPEECH_KIT_ID).await;
    ensure_installed("parakeetV3").await;
    ensure_installed("whisperTurbo").await;

    let kit = speech::speech_kit().expect("the speech kit should load, now that it is installed");

    // --- VAD: four turns, 800ms of silence apart -------------------------
    let conversation = read_wav(&fixtures.join("conversation.wav"));
    let conversation_seconds = conversation.len() as f64 / f64::from(SAMPLE_RATE);
    let vad_started = Instant::now();
    let turns = kit.vad(&conversation);
    let vad_rtf = conversation_seconds / vad_started.elapsed().as_secs_f64();
    eprintln!("VAD: {} turn(s) found, {vad_rtf:.1}x realtime: {turns:?}", turns.len());
    assert!(
        (3..=5).contains(&turns.len()),
        "expected about four turns, got {}: {turns:?}",
        turns.len()
    );

    // --- Diarisation: two distinct speakers -------------------------------
    let diarise_started = Instant::now();
    let diarised = kit.diarise(&conversation);
    let diarise_rtf = conversation_seconds / diarise_started.elapsed().as_secs_f64();
    let speakers: HashSet<usize> = diarised.iter().map(|(_, _, speaker)| *speaker).collect();
    eprintln!(
        "Diarisation: {} speaker(s), {diarise_rtf:.1}x realtime: {diarised:?}",
        speakers.len()
    );
    assert_eq!(speakers.len(), 2, "expected two speakers, got {speakers:?}: {diarised:?}");

    // --- Embeddings: same speaker closer than different speakers ---------
    let amy1 = read_wav(&fixtures.join("amy1_16k.wav"));
    let amy_ref = read_wav(&fixtures.join("amy_ref_16k.wav"));
    let ryan_ref = read_wav(&fixtures.join("ryan_ref_16k.wav"));
    let e_amy1 = kit.embed(&amy1).expect("amy1 should be long enough to embed");
    let e_amy_ref = kit.embed(&amy_ref).expect("amy_ref should be long enough to embed");
    let e_ryan_ref = kit.embed(&ryan_ref).expect("ryan_ref should be long enough to embed");
    let same_speaker = cosine(&e_amy1, &e_amy_ref);
    let cross_speaker = cosine(&e_amy1, &e_ryan_ref);
    eprintln!(
        "Embeddings ({}): same-speaker cosine = {same_speaker:.4}, cross-speaker cosine = {cross_speaker:.4}",
        kit.embedding_model()
    );
    assert!(
        same_speaker > cross_speaker,
        "same-speaker similarity ({same_speaker}) should exceed cross-speaker ({cross_speaker})"
    );

    // --- Recognition: both local models, measuring realtime factor -------
    // amy1.wav is "Good morning everyone. I wanted to start by reviewing the
    // quarterly numbers before we move on to the roadmap discussion." --
    // loose enough that either model's own capitalisation, punctuation and
    // itn choices cannot fail it.
    for model in [LocalModel::ParakeetV3, LocalModel::WhisperTurbo] {
        let recogniser =
            speech::recogniser(model).unwrap_or_else(|e| panic!("{}: {e}", model.as_str()));
        let samples = read_wav(&fixtures.join("amy1_16k.wav"));
        let duration_seconds = samples.len() as f64 / f64::from(SAMPLE_RATE);
        let started = Instant::now();
        let heard = recogniser.transcribe(&samples, Some("en"));
        let rtf = duration_seconds / started.elapsed().as_secs_f64();
        eprintln!("{}: {rtf:.1}x realtime -- {:?}", model.as_str(), heard.text);

        let lower = heard.text.to_lowercase();
        assert!(
            lower.contains("quarter") && (lower.contains("review") || lower.contains("morning")),
            "{}: expected the quarterly-numbers line, got {:?}",
            model.as_str(),
            heard.text
        );
    }
}
