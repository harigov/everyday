//! A spoken conversation against the real models: Silero, Parakeet and
//! Phonon, with only the language model and the sound devices replaced. The
//! same models are first used without a session at all, through
//! `Service::live_transcription`, which is the seam dictation and live meeting
//! transcripts are built on.
//!
//! `#[ignore]`, and gated a second time by `KARMAYOGA_TEST_SPEECH=1`, as
//! `speech_real.rs` is: it downloads a voice and runs real inference. The audio
//! that goes in is Phonon's, so nothing outside this test is needed but the
//! speech kit and Parakeet in the models folder. The voice itself is
//! `models/phonon`, downloaded here if it is not already there.

#![cfg(feature = "speech")]

use karmayoga_core::ConversationId;
use karmayoga_core::llm::ModelSettings;
use karmayoga_core::meeting::{LocalModel, SAMPLE_RATE};
use karmayoga_core::speech::{
    LocalVoice, SpeechConfig, SynthesiserConfig, TranscriberConfig, VoiceConfig,
};
use karmayoga_service::Service;
use karmayoga_service::agent::AgentEvent;
use karmayoga_service::local::files as models;
use karmayoga_service::speech::detect::Tuning;
use karmayoga_service::speech::live::{LiveEvent, LiveTranscription};
use karmayoga_service::speech::local as speech;
use karmayoga_service::speech::synth::{Speech, build_synthesiser};
use karmayoga_service::voice::{VoiceEvent, VoiceInput, VoiceLink, VoiceOutput, VoiceState};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[allow(dead_code)]
mod support;

const ASKED: &str = "What is on my calendar tomorrow morning?";
const FIRST: &str = "You have the dentist at nine in the morning.";
const SECOND: &str = "After that you are meeting Priya for coffee at eleven.";
const THIRD: &str = "The afternoon is completely free, so you could go for a long walk.";

fn enabled() -> bool {
    std::env::var("KARMAYOGA_TEST_SPEECH").as_deref() == Ok("1")
}

/// A model on loopback that streams `reply` to everything, and keeps what it
/// was asked.
async fn fake_model(reply: String) -> (String, Arc<Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = requests.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let reply = reply.clone();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut data = Vec::new();
                let mut chunk = vec![0u8; 16 * 1024];
                loop {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    data.extend_from_slice(&chunk[..n]);
                    if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&data[..end]).to_lowercase();
                        let length = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if data.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                seen.lock().unwrap().push(String::from_utf8_lossy(&data).into_owned());
                let body = stream_of(&reply);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: \
                     no-cache\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(body.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });
    (format!("http://127.0.0.1:{port}/v1"), requests)
}

fn stream_of(text: &str) -> String {
    let frame = |delta: Value, finish: Option<&str>| {
        let chunk = json!({
            "id": "chatcmpl-test",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "scripted",
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
        });
        format!("data: {chunk}\n\n")
    };
    let mut out = frame(json!({ "role": "assistant", "content": "" }), None);
    for word in text.split_inclusive(' ') {
        out.push_str(&frame(json!({ "content": word }), None));
    }
    out.push_str(&frame(json!({}), Some("stop")));
    out.push_str("data: [DONE]\n\n");
    out
}

fn service_with_voices(endpoint: &str) -> (Arc<Service>, tempfile::TempDir) {
    let (svc, dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    vault
        .save_agent_settings(&karmayoga_core::AgentSettings {
            enabled: true,
            timezone: Some("UTC".into()),
            ..Default::default()
        })
        .unwrap();
    let mut settings = ModelSettings::default();
    settings.medium.connection.base_url = Some(endpoint.to_string());
    settings.medium.model.model = "scripted".into();
    settings.speech =
        Some(SpeechConfig::new(TranscriberConfig::Local { model: LocalModel::ParakeetV3 }));
    settings.voice =
        Some(VoiceConfig::new(SynthesiserConfig::Local { voice: LocalVoice::Phonon, speaker: 0 }));
    vault.save_model_settings(&settings).unwrap();
    (svc, dir)
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

async fn transcribe(samples: Vec<i16>) -> String {
    let recogniser = speech::recogniser_async(LocalModel::ParakeetV3).await.unwrap();
    tokio::task::spawn_blocking(move || recogniser.transcribe(&samples, Some("en")).text)
        .await
        .unwrap()
        .to_lowercase()
}

/// The fundamental of voiced audio, by autocorrelation, in Hz.
fn pitch(samples: &[i16]) -> f32 {
    let rate = SAMPLE_RATE as usize;
    let (low, high) = (rate / 320, rate / 70);
    let mut found = Vec::new();
    for frame in samples.chunks_exact(rate / 25) {
        let x: Vec<f32> = frame.iter().map(|s| f32::from(*s)).collect();
        let energy: f32 = x.iter().map(|v| v * v).sum();
        if energy / (x.len() as f32) < 250_000.0 {
            continue;
        }
        let (mut best, mut at) = (0.0, 0);
        for lag in low..high.min(x.len() / 2) {
            let score: f32 = x.iter().zip(&x[lag..]).map(|(a, b)| a * b).sum::<f32>() / energy;
            if score > best {
                (best, at) = (score, lag);
            }
        }
        if best > 0.5 && at > 0 {
            found.push(rate as f32 / at as f32);
        }
    }
    found.sort_by(f32::total_cmp);
    found.get(found.len() / 2).copied().unwrap_or(0.0)
}

fn feed(link: &VoiceLink, samples: &[i16]) {
    for chunk in samples.chunks(480) {
        let _ = link.input.send(VoiceInput::Audio(chunk.to_vec()));
    }
}

fn silence(seconds: f32) -> Vec<i16> {
    vec![0; (seconds * SAMPLE_RATE as f32) as usize]
}

async fn next(link: &mut VoiceLink) -> VoiceOutput {
    tokio::time::timeout(Duration::from_secs(90), link.output.recv())
        .await
        .expect("the session went quiet")
        .expect("the session ended")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads a voice and runs real models; needs KARMAYOGA_TEST_SPEECH=1"]
async fn a_spoken_conversation_with_the_real_models() {
    if !enabled() {
        return;
    }
    assert!(models::speech_kit_installed(), "the speech kit is needed");
    assert!(models::installed(LocalModel::ParakeetV3), "Parakeet is needed");
    install_voice().await;

    let reply = format!("{FIRST} {SECOND} {THIRD}");
    let (endpoint, requests) = fake_model(reply).await;
    let (svc, _dir) = service_with_voices(&endpoint);
    let vault = svc.get().unwrap();

    // The voice, measured, and the question the "person" will ask.
    let synthesiser = build_synthesiser(vault.voice_access().unwrap()).await.unwrap();
    let began = Instant::now();
    let asked = synthesiser.synthesise(ASKED).await.unwrap();
    let took = began.elapsed().as_secs_f32();
    let length = asked.samples.len() as f32 / asked.sample_rate as f32;
    println!("voice: {length:.2}s of speech at {} Hz in {took:.2}s", asked.sample_rate);
    let question = to_16k(&asked);

    // Each speaker's pitch, which is how the settings can tell them apart.
    for speaker in 0..LocalVoice::Phonon.speakers() {
        let mut access = vault.voice_access().unwrap();
        access.config.synthesiser = SynthesiserConfig::Local { voice: LocalVoice::Phonon, speaker };
        let voice = build_synthesiser(access).await.unwrap();
        let said = voice.synthesise("Hello there, this is how I sound today.").await.unwrap();
        println!("speaker {speaker}: {:.0} Hz", pitch(&to_16k(&said)));
    }

    // ---- live transcription, with no session around it -------------------------
    // What dictation and live meeting transcripts will use: audio in, timed
    // words out, nothing about a conversation.
    let LiveTranscription { control, mut events } =
        svc.live_transcription(Tuning::CONVERSATION).await.expect("listening opens");
    let audio: Vec<i16> =
        [silence(0.5), question.clone(), silence(1.2)].into_iter().flatten().collect();
    for chunk in audio.chunks(480) {
        control.feed(chunk.to_vec());
    }
    let (mut started, mut ended) = (false, false);
    let said = loop {
        let event = tokio::time::timeout(Duration::from_secs(90), events.recv())
            .await
            .expect("live transcription went quiet")
            .expect("live transcription ended");
        match event {
            LiveEvent::Started { .. } => started = true,
            LiveEvent::Ended { start_ms, end_ms, .. } => {
                assert!(started, "an utterance ended before it began");
                assert!(start_ms < end_ms, "{start_ms} to {end_ms}");
                ended = true;
            }
            LiveEvent::Transcribed { start_ms, end_ms, result, .. } => {
                assert!(ended, "words came before the utterance had ended");
                let text = result.expect("the words were understood").to_lowercase();
                println!("live: {text} ({start_ms} to {end_ms} ms)");
                assert!(start_ms < end_ms, "{start_ms} to {end_ms}");
                break text;
            }
        }
    };
    assert!(said.contains("calendar"), "{said}");
    drop((control, events));

    // ---- a plain exchange ----------------------------------------------------
    let conversation = ConversationId::new();
    let mut link = svc.voice_open(conversation).await.expect("the session opens");
    feed(&link, &silence(0.5));
    feed(&link, &question);
    let spoke_at = Instant::now();
    feed(&link, &silence(1.2));

    let mut heard = None;
    let mut first_audio = None;
    let mut message_id = None;
    let mut played = Vec::new();
    let mut finished = false;
    // Back to listening once something has been said and the turn is over:
    // that is when the whole reply has been played.
    let mut listening = false;
    while !(finished && listening) {
        match next(&mut link).await {
            VoiceOutput::Event(VoiceEvent::Heard { text }) => {
                println!("heard after {:?}: {text}", spoke_at.elapsed());
                heard = Some(text);
            }
            VoiceOutput::Event(VoiceEvent::Agent { event }) => match event {
                AgentEvent::Started { message_id: id } => message_id = Some(id),
                AgentEvent::Finished { .. } => finished = true,
                AgentEvent::Failed { message } => panic!("the turn failed: {message}"),
                _ => {}
            },
            VoiceOutput::Event(VoiceEvent::Trouble { message }) => panic!("trouble: {message}"),
            VoiceOutput::Play { utterance, samples, sample_rate } => {
                first_audio.get_or_insert_with(|| {
                    println!("first audio after {:?}", spoke_at.elapsed());
                });
                played.push(Speech { samples, sample_rate });
                let _ = link.input.send(VoiceInput::Played { utterance });
                listening = false;
            }
            VoiceOutput::Event(VoiceEvent::State { state: VoiceState::Listening })
                if !played.is_empty() =>
            {
                listening = true;
            }
            _ => {}
        }
    }
    let heard = heard.expect("the question was heard").to_lowercase();
    assert!(heard.contains("calendar"), "{heard}");
    assert!(message_id.is_some());
    let request = requests.lock().unwrap().last().cloned().unwrap();
    assert!(request.contains("read aloud"), "the model is told it is being heard");

    // What came out is what the model said, and a recogniser can tell.
    let out: Vec<i16> = played.iter().flat_map(to_16k).collect();
    let understood = transcribe(out).await;
    println!("the reply, heard back: {understood}");
    assert!(understood.contains("dentist"), "{understood}");
    assert!(understood.contains("coffee"), "{understood}");

    // ---- talking over the next reply -----------------------------------------
    tokio::time::sleep(Duration::from_millis(300)).await;
    feed(&link, &question);
    feed(&link, &silence(1.2));
    let mut new_reply = None;
    let mut first_sentence = None;
    let mut second_sent = false;
    while !second_sent {
        match next(&mut link).await {
            VoiceOutput::Event(VoiceEvent::Agent {
                event: AgentEvent::Started { message_id: id },
            }) => new_reply = Some(id),
            VoiceOutput::Play { utterance, .. } if first_sentence.is_none() => {
                first_sentence = Some(utterance);
                let _ = link.input.send(VoiceInput::Played { utterance });
            }
            VoiceOutput::Play { .. } => second_sent = true,
            _ => {}
        }
    }
    let new_reply = new_reply.expect("a second turn");
    // The first sentence was heard and the second is playing when they cut in.
    feed(&link, &question);
    feed(&link, &silence(1.2));
    let (mut paused, mut cleared, mut cut, mut started_again) = (false, false, None, false);
    while !(cut.is_some() && started_again) {
        match next(&mut link).await {
            VoiceOutput::Pause => paused = true,
            VoiceOutput::Clear => cleared = true,
            VoiceOutput::Event(VoiceEvent::ReplyCut { message_id, text }) => {
                assert_eq!(message_id, new_reply);
                cut = Some(text);
            }
            VoiceOutput::Event(VoiceEvent::Heard { .. }) if cut.is_some() => started_again = true,
            VoiceOutput::Play { utterance, .. } => {
                let _ = link.input.send(VoiceInput::Played { utterance });
            }
            _ => {}
        }
    }
    assert!(paused && cleared);
    assert_eq!(cut.as_deref(), Some(FIRST));
    let stored = vault.messages(conversation).unwrap();
    assert!(
        stored.iter().any(|m| m.id.to_string() == new_reply && m.content == FIRST),
        "the stored reply reads what was heard"
    );

    let _ = link.input.send(VoiceInput::Stop);
    loop {
        if let VoiceOutput::Event(VoiceEvent::Ended { error }) = next(&mut link).await {
            assert_eq!(error, None);
            break;
        }
    }
}

/// Dictation's way of listening, as `karmayoga-app`'s `dictation` module does
/// it: the dictation tuning, two sentences with a pause between them, the
/// audio cut off the moment the last word ends -- a key let go -- then the
/// second of silence that makes the detector end the last piece, and every
/// piece collected, in order, until the stream closes.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "downloads a voice and runs real models; needs KARMAYOGA_TEST_SPEECH=1"]
async fn dictation_hears_every_word_up_to_the_key_being_let_go() {
    if !enabled() {
        return;
    }
    assert!(models::speech_kit_installed(), "the speech kit is needed");
    assert!(models::installed(LocalModel::ParakeetV3), "Parakeet is needed");
    install_voice().await;
    let (endpoint, _requests) = fake_model(String::new()).await;
    let (svc, _dir) = service_with_voices(&endpoint);
    let vault = svc.get().unwrap();
    let synthesiser =
        build_synthesiser(vault.voice_access().unwrap(), Some("en".into())).await.unwrap();
    let first = to_16k(&synthesiser.synthesise("Please send the report to Maria.").await.unwrap());
    let second =
        to_16k(&synthesiser.synthesise("I will review it on Friday afternoon.").await.unwrap());

    let LiveTranscription { control, mut events } =
        svc.live_transcription(Tuning::DICTATION).await.expect("listening opens");
    // No silence after the last word: the key went up as it ended.
    let audio: Vec<i16> =
        [silence(0.3), first, silence(1.0), second].into_iter().flatten().collect();
    for chunk in audio.chunks(480) {
        control.feed(chunk.to_vec());
    }
    control.feed(silence(1.2));
    drop(control);

    let mut pieces = Vec::new();
    let collecting = async {
        while let Some(event) = events.recv().await {
            if let LiveEvent::Transcribed { result, .. } = event {
                pieces.push(result.expect("the words were understood"));
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(120), collecting)
        .await
        .expect("the stream closed once everything was transcribed");
    let heard = pieces.join(" ").to_lowercase();
    println!("dictation: {pieces:?}");
    assert!(heard.contains("maria"), "{heard}");
    // The last word, cut off by the key, is what the trailing silence is for.
    // ("Friday" itself comes back as "Frida" from this voice now and then.)
    assert!(heard.contains("afternoon"), "the last words, cut off by the key: {heard}");
    assert!(heard.find("maria") < heard.find("review"), "in the order said: {heard}");
}
