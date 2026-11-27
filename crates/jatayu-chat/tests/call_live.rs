//! A call to the assistant over a real homeserver and a real LiveKit, answered
//! by the real runner with the real speech models, and this crate's own media
//! client playing the owner's phone.
//!
//! The owner's side is what FluffyChat 2.11 does on a call (MatrixRTC): an
//! `org.matrix.msc3401.call.member` state event naming the token service, a
//! LiveKit token from that service for its OpenID token, its media key sent
//! Olm-encrypted to the bot's device once the bot has joined, and audio,
//! encrypted frame by frame. What it says is Phonon's voice asking a
//! question; what comes back is decrypted with the bot's key and decoded, and
//! Parakeet has to hear the scripted model's answer in it. The thread has to
//! hold both, marked as from the chat.
//!
//! # Running it
//!
//! `#[ignore]`, built only with the `speech` feature, and run by `make
//! test-call`, which starts the throwaway servers it needs on loopback: a
//! Conduit with federation on (behind a TLS proxy with a self-signed
//! certificate, since the token service checks OpenID tokens over the
//! federation API), a LiveKit SFU, and `lk-jwt-service`. It needs:
//!
//! - `JATAYU_TEST_MATRIX_URL`: the Conduit, plain HTTP.
//! - `JATAYU_TEST_LIVEKIT_JWT_URL`: the token service.
//! - `JATAYU_TEST_SPEECH=1`, and the speech kit and Parakeet in the models
//!   folder (Phonon is downloaded if missing).

#![cfg(feature = "speech")]

use jatayu_chat::Runner;
use jatayu_chat::call::{self, Addresses, ToCall};
use jatayu_chat::connector::{CallJoin, CallKey};
use jatayu_core::agent::Role;
use jatayu_core::chat::{ChatLink, ChatService, MatrixLink};
use jatayu_core::llm::ModelSettings;
use jatayu_core::meeting::{LocalModel, SAMPLE_RATE};
use jatayu_core::speech::{
    LocalVoice, SpeechConfig, SynthesiserConfig, TranscriberConfig, VoiceConfig,
};
use jatayu_core::{AgentSettings, ConversationQuery, VaultConfig};
use jatayu_service::Service;
use jatayu_service::chat::ChatState;
use jatayu_service::ctx::Ctx;
use jatayu_service::local::files as models;
use jatayu_service::speech::local as speech;
use jatayu_service::speech::synth::build_synthesiser;
use jatayu_service::supervisor::Supervisor;
use jatayu_service::voice::{VoiceInput, VoiceLink, VoiceOutput};
use matrix_sdk::Client;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::deserialized_responses::EncryptionInfo;
use matrix_sdk::ruma::api::client::account::register::v3::Request as RegistrationRequest;
use matrix_sdk::ruma::api::client::uiaa::{AuthData, Dummy};
use matrix_sdk::ruma::events::{AnySyncTimelineEvent, AnyToDeviceEvent, AnyToDeviceEventContent};
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId, UserId};
use serde_json::{Value, json};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// What the owner asks, aloud.
const ASKED: &str = "What is on my calendar tomorrow morning?";
/// What the scripted model answers, and so what has to be heard back.
const ANSWER: &str = "You have the dentist at nine in the morning.";

/// How long any one thing is given to happen.
const WAIT: Duration = Duration::from_secs(90);

/// The media keys' to-device event type, as FluffyChat and Element Call name it.
const KEYS: &str = "io.element.call.encryption_keys";

/// A call membership's state event type.
const MEMBER: &str = "org.matrix.msc3401.call.member";

fn enabled() -> Option<String> {
    if std::env::var("JATAYU_TEST_SPEECH").as_deref() != Ok("1") {
        eprintln!("skipping: set JATAYU_TEST_SPEECH=1");
        return None;
    }
    let url = std::env::var("JATAYU_TEST_MATRIX_URL").ok().filter(|u| !u.trim().is_empty());
    if url.is_none() {
        eprintln!("skipping: set JATAYU_TEST_MATRIX_URL to a throwaway Conduit");
    }
    url.map(|u| u.trim().trim_end_matches('/').to_string())
}

/// The throwaway token service, which `make test-call` starts.
fn token_service() -> String {
    std::env::var("JATAYU_TEST_LIVEKIT_JWT_URL")
        .expect("JATAYU_TEST_LIVEKIT_JWT_URL is not set; run `make test-call`")
        .trim_end_matches('/')
        .to_string()
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
    let vault = jatayu_vault::create(dir.path(), config).unwrap();
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
    settings.voice =
        Some(VoiceConfig::new(SynthesiserConfig::Local { voice: LocalVoice::Phonon, speaker: 0 }));
    vault.save_model_settings(&settings).unwrap();

    let svc = Arc::new(Service::new());
    svc.set(vault);
    svc.set_supervisor(Arc::new(Supervisor::new(svc.events())));
    svc.set_chat_runner(Arc::new(Runner));
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

async fn wait_online(svc: &Arc<Service>, link: jatayu_core::ChatLinkId) {
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
// The owner's call, as FluffyChat makes it
// ---------------------------------------------------------------------------

/// The owner's membership of the room's call, as FluffyChat 2.11 writes it.
/// `{}` is leaving.
fn membership(owner: &UserId, device: &str, room: &OwnedRoomId, service: &str) -> Value {
    json!({
        "application": "m.call",
        "call_id": "",
        "device_id": device,
        "expires": 14_400_000,
        "foci_preferred": [{
            "type": "livekit",
            "livekit_service_url": service,
            "livekit_alias": room,
        }],
        "focus_active": { "type": "livekit", "focus_selection": "oldest_membership" },
        "m.call.intent": "voice",
        "membershipID": format!("{owner}:{device}"),
        "scope": "m.room",
        "created_at": jiff::Timestamp::now().as_millisecond(),
    })
}

/// A LiveKit token for the owner's device in `room`, from the token service,
/// for an OpenID token: what FluffyChat asks `/sfu/get` for.
async fn livekit_token(owner: &Client, room: &OwnedRoomId, service: &str) -> (String, String) {
    let openid = owner.account().request_openid_token().await.expect("an OpenID token");
    let body = json!({
        "room": room,
        "openid_token": {
            "access_token": openid.access_token,
            "token_type": "Bearer",
            "matrix_server_name": openid.matrix_server_name,
            "expires_in": openid.expires_in.as_secs(),
        },
        "device_id": owner.device_id().expect("signed in").as_str(),
    });
    // matrix-sdk's own HTTP client: its reqwest has neither a TLS provider
    // of its own nor the `json` feature.
    let answer = owner
        .http_client()
        .post(format!("{service}/sfu/get"))
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(&body).unwrap())
        .send()
        .await
        .expect("the token service answers")
        .error_for_status()
        .expect("the token service gives a token")
        .bytes()
        .await
        .expect("an answer");
    let response: Value = serde_json::from_slice(&answer).expect("JSON");
    let url = response["url"].as_str().expect("a url").to_string();
    let jwt = response["jwt"].as_str().expect("a jwt").to_string();
    (url, jwt)
}

/// Send the owner's media key to the bot's device, Olm-encrypted, as
/// FluffyChat sends it.
async fn send_key(owner: &Client, room: &OwnedRoomId, bot: &UserId, bot_device: &str, key: &[u8]) {
    use base64::Engine;
    let device_id = owner.device_id().expect("signed in").to_owned();
    let content = json!({
        "keys": { "index": 0, "key": base64::engine::general_purpose::STANDARD.encode(key) },
        "member": { "id": format!("{}:{device_id}", owner.user_id().unwrap()), "claimed_device_id": device_id },
        "room_id": room,
        "session": { "application": "m.call", "call_id": "", "scope": "m.room" },
        "sent_ts": jiff::Timestamp::now().as_millisecond(),
    });
    let encryption = owner.encryption();
    let deadline = Instant::now() + WAIT;
    let device = loop {
        if let Ok(Some(device)) = encryption.get_device(bot, bot_device.into()).await {
            break device;
        }
        assert!(Instant::now() < deadline, "the bot's device never became known");
        let _ = encryption.request_user_identity(bot).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let raw: Raw<AnyToDeviceEventContent> = Raw::new(&content).unwrap().cast_unchecked();
    let failed = encryption
        .encrypt_and_send_raw_to_device(
            vec![&device],
            KEYS,
            raw,
            // `CollectStrategy::AllDevices`, which matrix-sdk does not
            // re-export: the bot's device is unverified.
            Default::default(),
        )
        .await
        .expect("the key is sent");
    assert!(failed.is_empty(), "the key reached the bot's device: {failed:?}");
}

/// Keys of the bot's audio, as they reach the owner Olm-encrypted.
fn hear_keys(owner: &Client) -> mpsc::UnboundedReceiver<CallKey> {
    let (tx, rx) = mpsc::unbounded_channel();
    owner.add_event_handler(
        move |event: Raw<AnyToDeviceEvent>, encryption: Option<EncryptionInfo>| {
            let tx = tx.clone();
            async move {
                use base64::Engine;
                let Ok(value) = event.deserialize_as::<Value>() else { return };
                if value["type"] != KEYS || encryption.is_none() {
                    return;
                }
                let content = &value["content"];
                let sender = value["sender"].as_str().unwrap_or_default();
                let device = content["member"]["claimed_device_id"].as_str().unwrap_or_default();
                let Some(encoded) = content["keys"]["key"].as_str() else { return };
                let Ok(key) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
                    return;
                };
                let index = content["keys"]["index"].as_u64().unwrap_or(0) as u8;
                let _ = tx.send(CallKey { participant: format!("{sender}:{device}"), index, key });
            }
        },
    );
    rx
}

async fn transcribe(samples: Vec<i16>) -> String {
    let recogniser = speech::recogniser_async(LocalModel::ParakeetV3).await.unwrap();
    tokio::task::spawn_blocking(move || recogniser.transcribe(&samples, Some("en")).unwrap().text)
        .await
        .unwrap()
        .to_lowercase()
}

fn random() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs throwaway Conduit, LiveKit and token service, and the speech models: make test-call"]
async fn the_owner_calls_the_assistant_and_hears_its_answer() {
    let Some(homeserver) = enabled() else { return };
    assert!(models::speech_kit_installed(), "the speech kit is needed");
    assert!(models::installed(LocalModel::ParakeetV3), "Parakeet is needed");
    install_voice().await;

    step("registering the bot and the owner");
    let suffix: String = random().chars().take(8).collect();
    let (bot_password, owner_password) = (random(), random());
    let bot = register(&homeserver, &format!("jatayu-callbot-{suffix}"), &bot_password).await;
    let owner_id = register(&homeserver, &format!("jatayu-caller-{suffix}"), &owner_password).await;

    let endpoint = fake_model(ANSWER).await;
    let (svc, _dir) = assistant(&endpoint);
    let owner = Owner::sign_in(&homeserver, &owner_id, &owner_password).await;

    step("saving the chat link");
    let mut link = ChatLink::new_matrix();
    link.service = ChatService::Matrix(MatrixLink {
        homeserver: homeserver.clone(),
        user_id: bot.to_string(),
        owner: owner_id.to_string(),
        trusted_devices: Vec::new(),
    });
    svc.call(Ctx::local(), "save_chat_link", json!({ "link": link, "password": bot_password }))
        .await
        .unwrap_or_else(|e| panic!("save_chat_link: {e}"));
    // The bot answers calls, and gives its key, to a device the owner picked
    // by hand and to no other: this is the owner picking the one it signed in
    // on, before the bot is online.
    let owner_device = owner.client.device_id().expect("the owner is signed in").to_string();
    svc.call(
        Ctx::local(),
        "set_chat_trusted_devices",
        json!({ "id": link.id, "devices": [owner_device] }),
    )
    .await
    .unwrap_or_else(|e| panic!("set_chat_trusted_devices: {e}"));
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

    step("the question, in Phonon's voice");
    let vault = svc.get().unwrap();
    let voice = build_synthesiser(vault.voice_access().unwrap()).await.unwrap();
    let asked = voice.synthesise(ASKED).await.unwrap();

    step("calling: the owner's membership, then the owner's phone in the LiveKit room");
    let service = token_service();
    let mut bot_keys = hear_keys(&owner.client);
    let joined = owner.client.get_room(&room).expect("the owner is in the room");
    let owner_device = owner.client.device_id().expect("signed in").to_string();
    let state_key = format!("_{owner_id}_{owner_device}_m.call");
    joined
        .send_state_event_raw(
            MEMBER,
            &state_key,
            membership(&owner_id, &owner_device, &room, &service),
        )
        .await
        .expect("the owner joins the call");
    let (url, jwt) = livekit_token(&owner.client, &room, &service).await;
    let owner_key = uuid::Uuid::new_v4().as_bytes().to_vec();

    step("waiting for the bot to join");
    let bots = owner
        .wait_for("the bot's membership", &bot, |e| {
            e["type"] == MEMBER && e["content"]["device_id"].is_string()
        })
        .await;
    let bot_device = bots["content"]["device_id"].as_str().unwrap().to_string();
    let bot_participant = format!("{bot}:{bot_device}");
    assert_eq!(bots["content"]["membershipID"], json!(bot_participant), "as FluffyChat needs it");
    assert_eq!(bots["content"]["m.call.intent"], "voice");
    send_key(&owner.client, &room, &bot, &bot_device, &owner_key).await;

    let phone = call::connect(
        &CallJoin {
            url,
            token: jwt,
            caller: bot_participant.clone(),
            own_key: CallKey {
                participant: format!("{owner_id}:{owner_device}"),
                index: 0,
                key: owner_key,
            },
            // The phone, which joins at once, as FluffyChat does.
            wait_for_caller: false,
        },
        Vec::new(),
        Addresses::Only(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]),
    )
    .await
    .expect("the phone joins the LiveKit room");
    let (input, mut heard_rx) = mpsc::unbounded_channel();
    let (say, output) = mpsc::unbounded_channel();
    let (to_phone, to_phone_rx) = mpsc::unbounded_channel();
    let (events, _events) = mpsc::unbounded_channel();
    let talking = tokio::spawn(phone.run(VoiceLink { input, output }, to_phone_rx, events));

    step("waiting for the bot's key");
    let key = tokio::time::timeout(WAIT, bot_keys.recv())
        .await
        .expect("the bot sent its key")
        .expect("the owner is listening");
    assert_eq!(key.participant, bot_participant);
    to_phone.send(ToCall::Keys(vec![key])).unwrap();

    // The bot is in the room and has the owner's key; give its media a moment
    // to connect, so that the question is not said to nobody.
    tokio::time::sleep(Duration::from_secs(3)).await;
    say.send(VoiceOutput::Play {
        utterance: 1,
        samples: asked.samples,
        sample_rate: asked.sample_rate,
    })
    .unwrap();

    step("waiting for the answer to be said");
    let heard: Arc<Mutex<Vec<i16>>> = Arc::default();
    let gathering = tokio::spawn({
        let heard = heard.clone();
        async move {
            while let Some(input) = heard_rx.recv().await {
                if let VoiceInput::Audio(samples) = input {
                    heard.lock().unwrap().extend(samples);
                }
            }
        }
    });
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
    joined.send_state_event_raw(MEMBER, &state_key, json!({})).await.unwrap();
    let _ = to_phone.send(ToCall::End);
    let _ = talking.await;
    gathering.abort();
    owner
        .wait_for("the bot leaving the call", &bot, |e| {
            e["type"] == MEMBER && e["content"].as_object().is_some_and(|c| c.is_empty())
        })
        .await;

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

/// What the scratch bot says to anything, on a call from a real phone.
const PHONE_REPLY: &str =
    "I can hear you. This is the test assistant on the new calling code, and the call works.";

/// Not a test of anything automatic: a scratch bot, on the real homeserver,
/// for a person to call from FluffyChat on their phone. It signs in as a
/// throwaway account, opens its private room with the owner (who has to
/// accept the invitation), and answers calls with the real speech models and
/// a scripted reply, logging as it goes, until the owner has asked something
/// on a call and been answered, or the time is up.
///
/// - `JATAYU_TEST_PHONE_HOMESERVER`: where the bot signs in, e.g.
///   `http://127.0.0.1:6167`.
/// - `JATAYU_TEST_PHONE_BOT`, `JATAYU_TEST_PHONE_BOT_PASSWORD`: the
///   throwaway account.
/// - `JATAYU_TEST_PHONE_OWNER`: who will call. Every device the owner has
///   when the bot is online is trusted, so sign the phone in first.
/// - `JATAYU_TEST_PHONE_MINUTES`: how long to wait (15).
/// - `JATAYU_TEST_PHONE_RING`: a token service URL, to ring the owner
///   rather than wait to be called. The bot cannot place calls yet, so a
///   second sign-in of the same account does what a caller's FluffyChat
///   does -- its membership naming that service, then a ring -- and when the
///   owner picks up, their joining is a call the bot answers as any other.
/// - `JATAYU_TEST_SPEECH=1`, and the models as for the test above.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "waits for a person to call from a phone; see the doc comment"]
async fn a_real_phone_calls_a_scratch_bot() {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
    assert_eq!(std::env::var("JATAYU_TEST_SPEECH").as_deref(), Ok("1"), "the speech models");
    // What the bot does, as it does it: a person is on the other end, and a
    // call that goes wrong is diagnosed from this.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init();
    let homeserver = var("JATAYU_TEST_PHONE_HOMESERVER");
    let (bot, owner) = (var("JATAYU_TEST_PHONE_BOT"), var("JATAYU_TEST_PHONE_OWNER"));
    let minutes: u64 =
        std::env::var("JATAYU_TEST_PHONE_MINUTES").ok().and_then(|m| m.parse().ok()).unwrap_or(15);
    install_voice().await;

    let endpoint = fake_model(PHONE_REPLY).await;
    let (svc, _dir) = assistant(&endpoint);
    let mut link = ChatLink::new_matrix();
    link.service = ChatService::Matrix(MatrixLink {
        homeserver,
        user_id: bot.clone(),
        owner: owner.clone(),
        trusted_devices: Vec::new(),
    });
    let password = var("JATAYU_TEST_PHONE_BOT_PASSWORD");
    svc.call(Ctx::local(), "save_chat_link", json!({ "link": link, "password": password.clone() }))
        .await
        .unwrap_or_else(|e| panic!("save_chat_link: {e}"));
    wait_online(&svc, link.id).await;
    // The bot trusts only the devices its owner picked. The person holding the
    // phone is not here to pick, so every device the scratch owner has is.
    let devices: Vec<String> = serde_json::from_value::<Vec<Value>>(
        svc.call(Ctx::local(), "chat_devices", json!({ "id": link.id }))
            .await
            .unwrap_or_else(|e| panic!("chat_devices: {e}")),
    )
    .unwrap()
    .iter()
    .map(|device| device["deviceId"].as_str().unwrap().to_owned())
    .collect();
    step(&format!("trusting {devices:?}"));
    svc.call(
        Ctx::local(),
        "set_chat_trusted_devices",
        json!({ "id": link.id, "devices": devices }),
    )
    .await
    .unwrap_or_else(|e| panic!("set_chat_trusted_devices: {e}"));
    step(&format!("{bot} is online; {owner} has to accept its invitation"));
    let ringing = std::env::var("JATAYU_TEST_PHONE_RING").ok().map(|service| {
        let (homeserver, bot, owner) =
            (var("JATAYU_TEST_PHONE_HOMESERVER"), bot.clone(), owner.clone());
        tokio::spawn(async move { ring(&homeserver, &bot, &password, &owner, &service).await })
    });

    let vault = svc.get().unwrap();
    let deadline = Instant::now() + Duration::from_secs(minutes * 60);
    loop {
        let conversations = vault.conversations(&ConversationQuery::default()).unwrap();
        if let Some(thread) = conversations.iter().find(|c| c.via.is_some()) {
            let messages = vault.messages(thread.id).unwrap();
            let asked = messages.iter().find(|m| m.role == Role::User);
            let answered = messages.iter().any(|m| m.role == Role::Assistant);
            if let (Some(asked), true) = (asked, answered) {
                step(&format!("heard on the call: {:?}; answered", asked.content));
                break;
            }
        }
        assert!(Instant::now() < deadline, "nobody called within {minutes} minutes");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    // Long enough for the reply to be said before the bot goes away.
    tokio::time::sleep(Duration::from_secs(20)).await;
    if let Some(ringing) = ringing {
        ringing.abort();
    }
}

/// Ring `owner` from a second sign-in of `bot`, as FluffyChat rings: once
/// the owner is in the bot's private room, a call membership naming
/// `service`, then `org.matrix.msc4075.rtc.notification`. Rings again every
/// 40 seconds until the owner joins the call, then leaves it to the bot.
async fn ring(homeserver: &str, bot: &str, password: &str, owner: &str, service: &str) {
    use matrix_sdk::ruma::events::room::member::MembershipState;
    let owner_id = UserId::parse(owner).expect("the owner's Matrix ID");
    let ringer = Owner::sign_in(homeserver, &UserId::parse(bot).unwrap(), password).await;
    let device = ringer.client.device_id().expect("signed in").to_string();

    let room = loop {
        let mut found = None;
        for room in ringer.client.joined_rooms() {
            if let Ok(Some(member)) = room.get_member_no_sync(&owner_id).await
                && *member.membership() == MembershipState::Join
            {
                found = Some(room);
            }
        }
        if let Some(room) = found {
            break room;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    let room_id = room.room_id().to_owned();
    step(&format!("{owner} is in {room_id}; ringing"));

    let state_key = format!("_{bot}_{device}_m.call");
    let bot_id = UserId::parse(bot).unwrap();
    for attempt in 1..=5 {
        let content = membership(&bot_id, &device, &room_id, service);
        room.send_state_event_raw(MEMBER, &state_key, content).await.expect("the ringer joins");
        room.send_raw(
            "org.matrix.msc4075.rtc.notification",
            json!({
                "sender_ts": jiff::Timestamp::now().as_millisecond(),
                "lifetime": 30_000,
                "notification_type": "ring",
                "m.mentions": { "user_ids": [owner] },
                "m.call.intent": "voice",
            }),
        )
        .await
        .expect("the ring is sent");
        step(&format!("rang (attempt {attempt})"));

        let deadline = Instant::now() + Duration::from_secs(40);
        while Instant::now() < deadline {
            let picked_up = ringer.heard.lock().unwrap().iter().any(|(r, e)| {
                *r == room_id
                    && e["type"] == MEMBER
                    && e["sender"] == owner
                    && e["content"].as_object().is_some_and(|c| !c.is_empty())
            });
            if picked_up {
                step(&format!("{owner} picked up; the bot answers from here"));
                // The ringer was only ever a ring: it leaves the call to the
                // bot once there is somebody in it.
                tokio::time::sleep(Duration::from_secs(5)).await;
                let _ = room.send_state_event_raw(MEMBER, &state_key, json!({})).await;
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    let _ = room.send_state_event_raw(MEMBER, &state_key, json!({})).await;
    step("nobody picked up");
}
