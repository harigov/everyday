//! A call to the assistant over a real homeserver, answered by the real
//! runner with the real speech models, and a phone made of str0m on the
//! other end.
//!
//! The owner's side is what FluffyChat does on a call: an `m.call.invite`
//! carrying a WebRTC offer (Opus, one audio section, the host candidate of
//! this machine's own address, as a phone on the tailnet offers its tailnet
//! address), then audio. What it says is Supertonic's voice asking a question;
//! what comes back is decoded, and Parakeet has to hear the scripted model's
//! answer in it. The thread has to hold both, marked as from the chat.
//!
//! # Running it
//!
//! `#[ignore]`, and needs three things on top of `matrix_live.rs`'s throwaway
//! Conduit (see that file): `KARMAYOGA_TEST_MATRIX_URL`,
//! `KARMAYOGA_TEST_SPEECH=1`, and the speech kit and Parakeet in the models
//! folder (Supertonic is downloaded if missing). Built only with the `speech`
//! feature:
//!
//! ```text
//! KARMAYOGA_TEST_MATRIX_URL=http://127.0.0.1:16167 KARMAYOGA_TEST_SPEECH=1 \
//!   cargo test -p karmayoga-chat --features speech --test call_live -- --ignored --nocapture
//! ```

#![cfg(feature = "speech")]

use karmayoga_chat::Runner;
use karmayoga_core::agent::Role;
use karmayoga_core::chat::{ChatLink, ChatService, MatrixLink};
use karmayoga_core::llm::ModelSettings;
use karmayoga_core::meeting::{LocalModel, SAMPLE_RATE};
use karmayoga_core::speech::{
    LocalVoice, SpeechConfig, SynthesiserConfig, TranscriberConfig, VoiceConfig,
};
use karmayoga_core::{AgentSettings, ConversationQuery, VaultConfig};
use karmayoga_service::Service;
use karmayoga_service::chat::ChatState;
use karmayoga_service::ctx::Ctx;
use karmayoga_service::local::files as models;
use karmayoga_service::speech::local as speech;
use karmayoga_service::speech::resample;
use karmayoga_service::speech::synth::build_synthesiser;
use karmayoga_service::supervisor::Supervisor;
use matrix_sdk::Client;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::ruma::api::client::account::register::v3::Request as RegistrationRequest;
use matrix_sdk::ruma::api::client::uiaa::{AuthData, Dummy};
use matrix_sdk::ruma::events::AnySyncTimelineEvent;
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId, UserId};
use serde_json::{Value, json};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use str0m::change::{SdpAnswer, SdpPendingOffer};
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tokio::net::UdpSocket;
use tokio::sync::watch;

/// What the owner asks, aloud.
const ASKED: &str = "What is on my calendar tomorrow morning?";
/// What the scripted model answers, and so what has to be heard back.
const ANSWER: &str = "You have the dentist at nine in the morning.";

/// How long any one thing is given to happen.
const WAIT: Duration = Duration::from_secs(90);

/// The call's clock: Opus frames of 20 ms at 48 kHz.
const FRAME: usize = 960;

fn enabled() -> Option<String> {
    if std::env::var("KARMAYOGA_TEST_SPEECH").as_deref() != Ok("1") {
        eprintln!("skipping: set KARMAYOGA_TEST_SPEECH=1");
        return None;
    }
    let url = std::env::var("KARMAYOGA_TEST_MATRIX_URL").ok().filter(|u| !u.trim().is_empty());
    if url.is_none() {
        eprintln!("skipping: set KARMAYOGA_TEST_MATRIX_URL to a throwaway Conduit");
    }
    url.map(|u| u.trim().trim_end_matches('/').to_string())
}

fn step(what: &str) {
    eprintln!("call_live: {what}");
}

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// A Chat Completions model on loopback that streams `reply` to everything.
async fn fake_model(reply: &'static str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
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
                let body = stream_of(reply);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: \
                     no-cache\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(body.as_bytes()).await;
            });
        }
    });
    format!("http://127.0.0.1:{port}/v1")
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

// ---------------------------------------------------------------------------
// The assistant
// ---------------------------------------------------------------------------

fn assistant(endpoint: &str) -> (Arc<Service>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = VaultConfig {
        name: "Call".into(),
        backend: "sqlite".into(),
        settings: Default::default(),
        password: None,
        kdf: Default::default(),
        auto_lock_seconds: 900,
        forget_key_seconds: 0,
    };
    let vault = karmayoga_vault::create(dir.path(), config).unwrap();
    vault
        .save_agent_settings(&AgentSettings {
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
    settings.voice = Some(VoiceConfig::new(SynthesiserConfig::Local {
        voice: LocalVoice::Supertonic3,
        speaker: 0,
    }));
    vault.save_model_settings(&settings).unwrap();

    let svc = Arc::new(Service::new());
    svc.set(vault);
    svc.set_supervisor(Arc::new(Supervisor::new(svc.events())));
    svc.set_chat_runner(Arc::new(Runner));
    (svc, dir)
}

async fn install_voice() {
    if !models::voice_installed(LocalVoice::Supertonic3) {
        models::start_download(LocalVoice::Supertonic3.id()).expect("the download starts");
    }
    let began = Instant::now();
    while !models::voice_installed(LocalVoice::Supertonic3) {
        if let Some(error) = models::last_error(LocalVoice::Supertonic3.id()) {
            panic!("the voice did not download: {error}");
        }
        assert!(began.elapsed() < Duration::from_secs(300), "the voice took too long");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_online(svc: &Arc<Service>, link: karmayoga_core::ChatLinkId) {
    let deadline = Instant::now() + WAIT;
    loop {
        let status = svc.chat_status(link);
        match status.state {
            ChatState::Online => return,
            ChatState::NeedsSignIn => panic!("the bot cannot sign in: {}", status.detail),
            _ if Instant::now() >= deadline => {
                panic!("the bot was not online: {:?} - {}", status.state, status.detail)
            }
            _ => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
}

// ---------------------------------------------------------------------------
// The homeserver
// ---------------------------------------------------------------------------

/// Make an account on a server with open registration.
async fn register(homeserver: &str, localpart: &str, password: &str) -> OwnedUserId {
    let client = Client::builder().homeserver_url(homeserver).build().await.unwrap();
    let mut auth: Option<AuthData> = None;
    for _ in 0..3 {
        let mut request = RegistrationRequest::new();
        request.username = Some(localpart.to_owned());
        request.password = Some(password.to_owned());
        request.inhibit_login = true;
        request.auth = auth.take();
        match client.matrix_auth().register(request).await {
            Ok(done) => return done.user_id,
            Err(e) => {
                let info = e.as_uiaa_response().unwrap_or_else(|| panic!("register: {e}"));
                let mut dummy = Dummy::new();
                dummy.session = info.session.clone();
                auth = Some(AuthData::Dummy(dummy));
            }
        }
    }
    panic!("could not register {localpart}");
}

/// The owner's client: syncing in the background, and keeping every timeline
/// event it can read, decrypted, as JSON.
struct Owner {
    client: Client,
    heard: Arc<Mutex<Vec<(OwnedRoomId, Value)>>>,
    _syncing: tokio::task::JoinHandle<()>,
    _store: tempfile::TempDir,
}

impl Owner {
    async fn sign_in(homeserver: &str, id: &UserId, password: &str) -> Owner {
        let store = tempfile::tempdir().unwrap();
        let client = Client::builder()
            .homeserver_url(homeserver)
            .sqlite_store(store.path(), None)
            .build()
            .await
            .unwrap();
        client.matrix_auth().login_username(id.as_str(), password).send().await.unwrap();
        client.encryption().wait_for_e2ee_initialization_tasks().await;
        client.sync_once(SyncSettings::default().timeout(Duration::from_secs(1))).await.unwrap();

        let heard: Arc<Mutex<Vec<(OwnedRoomId, Value)>>> = Arc::default();
        let keep = heard.clone();
        client.add_event_handler(
            move |event: Raw<AnySyncTimelineEvent>, room: matrix_sdk::Room| {
                let keep = keep.clone();
                async move {
                    if let Ok(value) = event.deserialize_as::<Value>() {
                        keep.lock().unwrap().push((room.room_id().to_owned(), value));
                    }
                }
            },
        );
        let syncing = tokio::spawn({
            let client = client.clone();
            async move {
                loop {
                    for room in client.invited_rooms() {
                        let _ = room.join().await;
                    }
                    let settings = SyncSettings::default().timeout(Duration::from_secs(1));
                    if client.sync_once(settings).await.is_err() {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
            }
        });
        Owner { client, heard, _syncing: syncing, _store: store }
    }

    /// Wait for an event `wanted` accepts, sent by `from`.
    async fn wait_for(&self, what: &str, from: &UserId, wanted: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            {
                let heard = self.heard.lock().unwrap();
                if let Some((_, found)) = heard
                    .iter()
                    .find(|(_, e)| e["sender"].as_str() == Some(from.as_str()) && wanted(e))
                {
                    return found.clone();
                }
            }
            if Instant::now() >= deadline {
                let heard = self.heard.lock().unwrap();
                let types: Vec<_> = heard
                    .iter()
                    .map(|(_, e)| e["type"].as_str().unwrap_or("?").to_string())
                    .collect();
                panic!("timed out waiting for {what}; heard {types:?}");
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

// ---------------------------------------------------------------------------
// The phone
// ---------------------------------------------------------------------------

/// This machine's address as a phone on the same network would reach it: the
/// tailnet's if there is one, else the first other address that is up.
fn this_machine() -> IpAddr {
    let addresses: Vec<IpAddr> = if_addrs::get_if_addrs()
        .unwrap()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .map(|i| i.ip())
        .filter(IpAddr::is_ipv4)
        .collect();
    addresses
        .iter()
        .find(|ip| matches!(ip, IpAddr::V4(v4) if v4.octets()[0] == 100))
        .or(addresses.first())
        .copied()
        .expect("an address that is not loopback")
}

struct Phone {
    rtc: Rtc,
    socket: UdpSocket,
    local: SocketAddr,
    mid: Mid,
    pending: Option<SdpPendingOffer>,
}

impl Phone {
    /// A phone with one Opus audio section, and its offer.
    async fn dial() -> (Phone, String) {
        let ip = this_machine();
        let socket = UdpSocket::bind(SocketAddr::new(IpAddr::from([0, 0, 0, 0]), 0)).await.unwrap();
        let local = SocketAddr::new(ip, socket.local_addr().unwrap().port());
        let mut rtc = Rtc::builder().clear_codecs().enable_opus(true, false).build(Instant::now());
        rtc.add_local_candidate(Candidate::host(local, "udp").unwrap());
        let mut change = rtc.sdp_api();
        let mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        let (offer, pending) = change.apply().expect("an offer");
        let sdp = offer.to_sdp_string();
        (Phone { rtc, socket, local, mid, pending: Some(pending) }, sdp)
    }

    fn accept(&mut self, answer: &str) {
        let answer = SdpAnswer::from_sdp_string(answer).expect("a readable answer");
        let pending = self.pending.take().expect("an offer waiting");
        self.rtc.sdp_api().accept_answer(pending, answer).expect("the answer is accepted");
    }

    /// Talk and listen until `stop`: `say` (48 kHz) after a second of
    /// silence, then silence; everything heard, decoded to 16 kHz, into
    /// `heard`.
    async fn run(
        mut self,
        say: Vec<i16>,
        heard: Arc<Mutex<Vec<i16>>>,
        connected: Arc<AtomicBool>,
        mut stop: watch::Receiver<bool>,
    ) {
        let mut encoder =
            opus::Encoder::new(48_000, opus::Channels::Mono, opus::Application::Voip).unwrap();
        let mut decoder = opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono).unwrap();
        let mut queue: std::collections::VecDeque<i16> =
            std::iter::repeat_n(0, 48_000).chain(say).collect();
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        let mut rtp: u64 = 0;
        let mut buf = vec![0u8; 2000];
        let mut packet = vec![0u8; 1500];
        let mut pcm = vec![0i16; 5760];
        loop {
            // Drain, as str0m requires after every mutation.
            let timeout = loop {
                match self.rtc.poll_output().expect("the phone's peer") {
                    Output::Timeout(t) => break t,
                    Output::Transmit(t) => {
                        let _ = self.socket.send_to(&t.contents, t.destination).await;
                    }
                    Output::Event(Event::IceConnectionStateChange(state)) => {
                        if matches!(
                            state,
                            IceConnectionState::Connected | IceConnectionState::Completed
                        ) {
                            connected.store(true, Ordering::SeqCst);
                        }
                    }
                    Output::Event(Event::MediaData(data)) => {
                        if let Ok(n) = decoder.decode(&data.data, &mut pcm, false) {
                            heard.lock().unwrap().extend_from_slice(&pcm[..n]);
                        }
                    }
                    Output::Event(_) => {}
                }
            };
            let wait = timeout.saturating_duration_since(Instant::now());
            tokio::select! {
                _ = stop.changed() => return,
                received = self.socket.recv_from(&mut buf) => {
                    let Ok((n, source)) = received else { continue };
                    let Ok(contents) = buf[..n].try_into() else { continue };
                    let input = Input::Receive(Instant::now(), Receive {
                        proto: Protocol::Udp,
                        source,
                        destination: self.local,
                        contents,
                    });
                    let _ = self.rtc.handle_input(input);
                }
                _ = tick.tick() => {
                    if !connected.load(Ordering::SeqCst) {
                        let _ = self.rtc.handle_input(Input::Timeout(Instant::now()));
                        continue;
                    }
                    let frame: Vec<i16> =
                        (0..FRAME).map(|_| queue.pop_front().unwrap_or(0)).collect();
                    let n = encoder.encode(&frame, &mut packet).unwrap();
                    let Some(writer) = self.rtc.writer(self.mid) else { continue };
                    let Some(pt) = writer.payload_params().next().map(|p| p.pt()) else { continue };
                    let time = MediaTime::new(rtp, Frequency::FORTY_EIGHT_KHZ);
                    rtp += FRAME as u64;
                    let _ = writer.write(pt, Instant::now(), time, &packet[..n]);
                }
                () = tokio::time::sleep(wait) => {
                    let _ = self.rtc.handle_input(Input::Timeout(Instant::now()));
                }
            }
        }
    }
}

async fn transcribe(samples: Vec<i16>) -> String {
    let recogniser = speech::recogniser_async(LocalModel::ParakeetV3).await.unwrap();
    tokio::task::spawn_blocking(move || recogniser.transcribe(&samples, Some("en")).text)
        .await
        .unwrap()
        .to_lowercase()
}

fn random() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a throwaway Conduit and the real speech models; see the module doc"]
async fn the_owner_calls_the_assistant_and_hears_its_answer() {
    let Some(homeserver) = enabled() else { return };
    assert!(models::speech_kit_installed(), "the speech kit is needed");
    assert!(models::installed(LocalModel::ParakeetV3), "Parakeet is needed");
    install_voice().await;

    step("registering the bot and the owner");
    let suffix: String = random().chars().take(8).collect();
    let (bot_password, owner_password) = (random(), random());
    let bot = register(&homeserver, &format!("karmayoga-callbot-{suffix}"), &bot_password).await;
    let owner_id =
        register(&homeserver, &format!("karmayoga-caller-{suffix}"), &owner_password).await;

    let endpoint = fake_model(ANSWER).await;
    let (svc, _dir) = assistant(&endpoint);
    let owner = Owner::sign_in(&homeserver, &owner_id, &owner_password).await;

    step("saving the chat link");
    let mut link = ChatLink::new_matrix();
    link.service = ChatService::Matrix(MatrixLink {
        homeserver: homeserver.clone(),
        user_id: bot.to_string(),
        owner: owner_id.to_string(),
    });
    svc.call(Ctx::local(), "save_chat_link", json!({ "link": link, "password": bot_password }))
        .await
        .unwrap_or_else(|e| panic!("save_chat_link: {e}"));
    wait_online(&svc, link.id).await;

    step("waiting for the greeting");
    let greeting = owner.wait_for("the greeting", &bot, |e| e["type"] == "m.room.message").await;
    let room: OwnedRoomId = serde_json::from_value(
        owner
            .heard
            .lock()
            .unwrap()
            .iter()
            .find(|(_, e)| e["event_id"] == greeting["event_id"])
            .map(|(room, _)| json!(room))
            .unwrap(),
    )
    .unwrap();

    step("the question, in Supertonic's voice, at the call's rate");
    let vault = svc.get().unwrap();
    let voice = build_synthesiser(vault.voice_access().unwrap(), Some("en".into())).await.unwrap();
    let asked = voice.synthesise(ASKED).await.unwrap();
    let floats = karmayoga_service::speech::pcm::to_f32(&asked.samples);
    let say = karmayoga_service::speech::pcm::to_i16(
        &resample::clip(&floats, asked.sample_rate, 48_000).unwrap(),
    );

    step("calling");
    let (mut phone, offer) = Phone::dial().await;
    let call_id = random();
    let party_id = random();
    let joined = owner.client.get_room(&room).expect("the owner is in the room");
    joined
        .send_raw(
            "m.call.invite",
            json!({
                "call_id": call_id,
                "party_id": party_id,
                "version": "1",
                "lifetime": 60_000,
                "offer": { "type": "offer", "sdp": offer },
            }),
        )
        .await
        .expect("the invite is sent");

    let answer = owner
        .wait_for("the bot's answer", &bot, |e| {
            e["type"] == "m.call.answer" && e["content"]["call_id"] == call_id
        })
        .await;
    let sdp = answer["content"]["answer"]["sdp"].as_str().expect("an answer SDP").to_string();
    eprintln!("call_live: answered with:\n{sdp}");
    phone.accept(&sdp);
    joined
        .send_raw(
            "m.call.select_answer",
            json!({
                "call_id": call_id,
                "party_id": party_id,
                "version": "1",
                "selected_party_id": answer["content"]["party_id"],
            }),
        )
        .await
        .unwrap();

    let heard: Arc<Mutex<Vec<i16>>> = Arc::default();
    let connected = Arc::new(AtomicBool::new(false));
    let (stop, stopped) = watch::channel(false);
    let talking = tokio::spawn(phone.run(say, heard.clone(), connected.clone(), stopped));

    step("waiting to connect");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !connected.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "the call never connected");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    step("waiting for the answer to be said");
    // Speech is anything louder than the silence the bot sends between
    // sentences; the answer is over once a second and a half of quiet
    // follows some speech.
    let deadline = Instant::now() + WAIT;
    let mut spoke_at = None;
    loop {
        assert!(Instant::now() < deadline, "the assistant never finished speaking");
        tokio::time::sleep(Duration::from_millis(250)).await;
        let samples = heard.lock().unwrap().clone();
        let loud = |s: &[i16]| s.iter().any(|v| v.unsigned_abs() > 1_000);
        if spoke_at.is_none() && loud(&samples) {
            spoke_at = Some(Instant::now());
            step("the assistant is speaking");
        }
        let quiet = (SAMPLE_RATE as usize * 3) / 2;
        if spoke_at.is_some() && samples.len() > quiet && !loud(&samples[samples.len() - quiet..]) {
            break;
        }
    }
    let samples = heard.lock().unwrap().clone();
    let said = transcribe(samples).await;
    eprintln!("call_live: heard back {said:?}");
    assert!(said.contains("dentist"), "the answer should have been heard: {said:?}");

    step("hanging up");
    joined
        .send_raw(
            "m.call.hangup",
            json!({ "call_id": call_id, "party_id": party_id, "version": "1", "reason": "user_hangup" }),
        )
        .await
        .unwrap();
    let _ = stop.send(true);
    let _ = talking.await;

    step("the thread holds the call");
    let deadline = Instant::now() + WAIT;
    loop {
        let conversations = vault.conversations(&ConversationQuery::default()).unwrap();
        let from_chat: Vec<_> = conversations.iter().filter(|c| c.via.is_some()).collect();
        if let Some(thread) = from_chat.first() {
            let messages = vault.messages(thread.id).unwrap();
            let asked = messages
                .iter()
                .any(|m| m.role == Role::User && m.content.to_lowercase().contains("calendar"));
            let answered =
                messages.iter().any(|m| m.role == Role::Assistant && m.content.contains("dentist"));
            if asked && answered {
                break;
            }
        }
        assert!(Instant::now() < deadline, "the thread never held the question and the answer");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
