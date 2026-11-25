//! A recogniser on this computer starts loading when listening opens, not
//! when the first words arrive -- and not at all when opening is refused.
//! Against the real models, as `voice_real` is, and gated the same way.
//!
//! A file of its own because which models are in memory belongs to the whole
//! process: a test beside it that had loaded Parakeet already would make this
//! one pass without proving anything.

#![cfg(feature = "speech")]

use karmayoga_core::ConversationId;
use karmayoga_core::llm::ModelSettings;
use karmayoga_core::meeting::{LocalModel, SAMPLE_RATE};
use karmayoga_core::speech::{
    LocalVoice, SpeechConfig, SynthesiserConfig, TranscriberConfig, VoiceConfig,
};
use karmayoga_service::local::files as models;
use karmayoga_service::speech::detect::Tuning;
use karmayoga_service::speech::live::{LiveEvent, LiveTranscription};
use karmayoga_service::speech::local as speech;
use karmayoga_service::speech::synth::{Speech, build_synthesiser};
use std::time::{Duration, Instant};

#[allow(dead_code)]
mod support;

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

fn silence(seconds: f32) -> Vec<i16> {
    vec![0; (seconds * SAMPLE_RATE as f32) as usize]
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads a voice and runs real models; needs KARMAYOGA_TEST_SPEECH=1"]
async fn listening_loads_the_recogniser_before_anything_is_heard() {
    if !enabled() {
        return;
    }
    assert!(models::speech_kit_installed(), "the speech kit is needed");
    assert!(models::installed(LocalModel::ParakeetV3), "Parakeet is needed");
    install_voice().await;
    assert!(!speech::recogniser_loaded(LocalModel::ParakeetV3), "nothing has listened yet");

    let (svc, _dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let settings = ModelSettings {
        speech: Some(SpeechConfig::new(TranscriberConfig::Local { model: LocalModel::ParakeetV3 })),
        // A voice at a provider, with no key: voice mode is refused.
        voice: Some(VoiceConfig::new(SynthesiserConfig::OpenAi {
            model: "tts-1".into(),
            voice: "alloy".into(),
        })),
        ..Default::default()
    };
    vault.save_model_settings(&settings).unwrap();

    // ---- refused: nothing is loaded for a session that never opened ----------
    assert!(svc.voice_open(ConversationId::new()).await.is_err(), "a voice with no key");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        !speech::recogniser_loaded(LocalModel::ParakeetV3),
        "voice mode was refused, and still loaded the recogniser"
    );

    // ---- opened: loaded while the person is still talking --------------------
    let local =
        VoiceConfig::new(SynthesiserConfig::Local { voice: LocalVoice::Phonon, speaker: 0 });
    let synthesiser = build_synthesiser(karmayoga_core::speech::VoiceAccess {
        config: local,
        key: None,
        pronunciations: Vec::new(),
    })
    .await
    .unwrap();
    let question =
        to_16k(&synthesiser.synthesise("What is on my calendar tomorrow morning?").await.unwrap());
    let LiveTranscription { control, mut events } = svc
        .live_transcription(
            Tuning::CONVERSATION,
            karmayoga_service::speech::expect::Occasion::Conversation,
            karmayoga_service::speech::access::HeardFrom::ThisComputer,
        )
        .await
        .expect("listening opens");
    let opened = Instant::now();
    while !speech::recogniser_loaded(LocalModel::ParakeetV3) {
        assert!(opened.elapsed() < Duration::from_secs(60), "the recogniser never loaded");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    println!("loaded {:?} after listening opened, with nothing heard", opened.elapsed());

    let audio: Vec<i16> = [silence(0.5), question, silence(2.0)].into_iter().flatten().collect();
    for chunk in audio.chunks(480) {
        control.feed(chunk.to_vec());
    }
    let fed = Instant::now();
    let said = loop {
        let event = tokio::time::timeout(Duration::from_secs(90), events.recv())
            .await
            .expect("live transcription went quiet")
            .expect("live transcription ended");
        if let LiveEvent::Transcribed { result, .. } = event {
            break result.expect("the words were understood").to_lowercase();
        }
    };
    println!("heard {said:?} {:?} after the end of the turn", fed.elapsed());
    assert!(said.contains("calendar"), "{said}");
}
