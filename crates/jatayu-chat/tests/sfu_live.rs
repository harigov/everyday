//! A call's media against a real LiveKit server: two participants, both this
//! crate's own client (`jatayu_chat::call`), in one room, each sending a
//! tone and hearing the other's -- through the server, encrypted frame by
//! frame with keys the server never sees.
//!
//! This is the part of a call no fake can vouch for: LiveKit's signalling,
//! its two connections per participant, ICE and DTLS with its WebRTC stack,
//! and frames forwarded untouched. Whether the frames and keys are the ones
//! FluffyChat writes is not proved here (both ends are ours); that rests on
//! `call::frames`' test vector and on a call from a real phone.
//!
//! Ignored by default: it needs a LiveKit server. `make test-livekit` starts a
//! throwaway one in Docker on loopback and runs this.
//!
//! - `JATAYU_TEST_LIVEKIT_URL`: the server, e.g. `ws://127.0.0.1:17880`.
//! - `JATAYU_TEST_LIVEKIT_KEY`, `JATAYU_TEST_LIVEKIT_SECRET`: an API key
//!   it knows, to sign the participants' tokens with.
//! - Or, against a deployed server whose key is not to hand,
//!   `JATAYU_TEST_LIVEKIT_ALICE` and `JATAYU_TEST_LIVEKIT_BOB`: each
//!   `identity token`, tokens a token service gave out for one room; with
//!   `JATAYU_TEST_LIVEKIT_INTERFACES=1` to offer this machine's own
//!   addresses as a bot does, rather than loopback.
//! - `RUST_LOG` for the clients' own log lines.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jatayu_chat::call::{self, Addresses, CallEnd, ToCall};
use jatayu_chat::connector::{CallJoin, CallKey};
use jatayu_service::voice::{VoiceEvent, VoiceInput, VoiceLink, VoiceOutput};
use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// How long the two may take to connect and hear each other.
const PATIENCE: Duration = Duration::from_secs(30);

const ALICE: &str = "@alice:rtc-test.localhost:PHONE";
const BOB: &str = "@bob:rtc-test.localhost:BOT";

/// A participant handed in by the environment as `identity token`, if any.
fn given(who: &str) -> Option<(String, String)> {
    let value = std::env::var(format!("JATAYU_TEST_LIVEKIT_{who}")).ok()?;
    let (identity, token) = value.trim().split_once(' ')?;
    Some((identity.to_string(), token.to_string()))
}

/// Alice and Bob: given, or the test's own with tokens it signs.
fn people() -> (String, String) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init();
    match (given("ALICE"), given("BOB")) {
        (Some((alice, _)), Some((bob, _))) => (alice, bob),
        _ => (ALICE.to_string(), BOB.to_string()),
    }
}

fn addresses() -> Addresses {
    if std::env::var("JATAYU_TEST_LIVEKIT_INTERFACES").as_deref() == Ok("1") {
        Addresses::Interfaces
    } else {
        Addresses::Only(vec![LOOPBACK])
    }
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set; run `make test-livekit`"))
}

/// A LiveKit access token for `identity` in `room`: a JWT signed with the
/// server's API secret (HS256), allowed to join, publish and subscribe.
fn token(identity: &str, room: &str) -> String {
    for who in ["ALICE", "BOB"] {
        if let Some((given, token)) = given(who)
            && given == identity
        {
            return token;
        }
    }
    let now = jiff::Timestamp::now().as_second();
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let claims = serde_json::json!({
        "iss": env("JATAYU_TEST_LIVEKIT_KEY"),
        "sub": identity,
        "nbf": now - 10,
        "exp": now + 600,
        "video": { "roomJoin": true, "room": room, "canPublish": true, "canSubscribe": true },
    });
    let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
    let signing = format!("{header}.{payload}");
    let key =
        ring::hmac::Key::new(ring::hmac::HMAC_SHA256, env("JATAYU_TEST_LIVEKIT_SECRET").as_bytes());
    let signature = URL_SAFE_NO_PAD.encode(ring::hmac::sign(&key, signing.as_bytes()));
    format!("{signing}.{signature}")
}

fn key_of(participant: &str, byte: u8) -> CallKey {
    CallKey { participant: participant.into(), index: 0, key: vec![byte; 16] }
}

/// One participant, running, with the test's ends of its voice session.
struct Side {
    task: JoinHandle<CallEnd>,
    to_call: UnboundedSender<ToCall>,
    /// What it heard, at 16 kHz, as the call handed it to the session.
    heard: UnboundedReceiver<VoiceInput>,
    /// What the session asks it to say.
    say: UnboundedSender<VoiceOutput>,
    _events: UnboundedReceiver<VoiceEvent>,
}

/// Join `room` as `me`, listening to `caller`, with `keys` of theirs --
/// waiting for the caller to be settled in the room first, as the bot does,
/// or not, as a phone does not.
async fn join(room: &str, me: &CallKey, caller: &str, keys: Vec<CallKey>, wait: bool) -> Side {
    let join = CallJoin {
        url: env("JATAYU_TEST_LIVEKIT_URL"),
        token: token(&me.participant, room),
        caller: caller.into(),
        own_key: me.clone(),
        wait_for_caller: wait,
    };
    let call = call::connect(&join, keys, addresses())
        .await
        .unwrap_or_else(|e| panic!("{} could not join: {e}", me.participant));
    let (input, heard) = unbounded_channel();
    let (say, output) = unbounded_channel();
    let (to_call, to_call_rx) = unbounded_channel();
    let (events_tx, events) = unbounded_channel();
    let task = tokio::spawn(call.run(VoiceLink { input, output }, to_call_rx, events_tx));
    Side { task, to_call, heard, say, _events: events }
}

/// Ten seconds of a tone at `hz`, 16 kHz, for a side to say.
fn tone(hz: f32) -> VoiceOutput {
    let samples = (0..16_000 * 10)
        .map(|i| {
            let t = i as f32 / 16_000.0;
            (0.4 * (std::f32::consts::TAU * hz * t).sin() * 32767.0) as i16
        })
        .collect();
    VoiceOutput::Play { utterance: 1, samples, sample_rate: 16_000 }
}

/// How much of `samples` (16 kHz) is at `hz`, against everything: Goertzel's
/// power at that frequency over the total.
fn share_at(samples: &[i16], hz: f32) -> f32 {
    let x: Vec<f32> = samples.iter().map(|s| *s as f32 / 32768.0).collect();
    let w = std::f32::consts::TAU * hz / 16_000.0;
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for v in &x {
        let s0 = v + 2.0 * w.cos() * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2;
    let total: f32 = x.iter().map(|v| v * v).sum::<f32>() * x.len() as f32 / 2.0;
    if total == 0.0 { 0.0 } else { power / total }
}

/// Gather what `side` hears until a second of it is mostly `hz`, or give up.
async fn until_heard(side: &mut Side, hz: f32, who: &str) {
    let deadline = Instant::now() + PATIENCE;
    let mut window: Vec<i16> = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match timeout(left, side.heard.recv()).await {
            Ok(Some(VoiceInput::Audio(samples))) => {
                window.extend(samples);
                if window.len() >= 16_000 {
                    let tail = &window[window.len() - 16_000..];
                    if share_at(tail, hz) > 0.5 {
                        return;
                    }
                    window.drain(..window.len() - 16_000);
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("{who}'s call ended before it heard the tone"),
            Err(_) => panic!("{who} never heard a {hz} Hz tone"),
        }
    }
}

fn room_name(test: &str) -> String {
    format!("{test}-{}", uuid::Uuid::new_v4().simple())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a LiveKit server: make test-livekit"]
async fn two_participants_hear_each_other_through_the_server_encrypted() {
    let room = room_name("hear");
    let (alice_id, bob_id) = people();
    let (alice_key, bob_key) = (key_of(&alice_id, 0xa1), key_of(&bob_id, 0xb0));

    // Alice is the phone, and joins at once; Bob is the bot, and waits for
    // her to be settled in the room, as it would for FluffyChat.
    let mut alice = join(&room, &alice_key, &bob_id, vec![bob_key.clone()], false).await;
    let mut bob = join(&room, &bob_key, &alice_id, Vec::new(), true).await;
    // Bob's key for Alice's frames arrives late, as a key over a chat
    // service can, while the call runs.
    bob.to_call.send(ToCall::Keys(vec![alice_key.clone()])).expect("running");

    alice.say.send(tone(440.0)).expect("running");
    bob.say.send(tone(1000.0)).expect("running");

    until_heard(&mut bob, 440.0, "Bob").await;
    until_heard(&mut alice, 1000.0, "Alice").await;

    // Alice hangs up; Bob's call ends as the caller leaving the room.
    alice.to_call.send(ToCall::End).expect("running");
    assert_eq!(
        timeout(PATIENCE, alice.task).await.expect("ends").expect("no panic"),
        CallEnd::Ended
    );
    assert_eq!(
        timeout(PATIENCE, bob.task).await.expect("ends").expect("no panic"),
        CallEnd::Ended,
        "the caller leaving ends the call"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a LiveKit server: make test-livekit"]
async fn without_the_callers_key_nothing_is_heard() {
    let room = room_name("nokey");
    let (alice_id, bob_id) = people();
    let (alice_key, bob_key) = (key_of(&alice_id, 0xa1), key_of(&bob_id, 0xb0));

    let alice = join(&room, &alice_key, &bob_id, vec![bob_key.clone()], false).await;
    // Bob has a key for Alice, but the wrong one.
    let mut bob = join(&room, &bob_key, &alice_id, vec![key_of(&alice_id, 0x00)], true).await;
    alice.say.send(tone(440.0)).expect("running");

    let deadline = Instant::now() + Duration::from_secs(8);
    let mut heard = 0usize;
    while let Ok(Some(input)) =
        timeout(deadline.saturating_duration_since(Instant::now()), bob.heard.recv()).await
    {
        if let VoiceInput::Audio(samples) = input {
            heard += samples.len();
        }
    }
    assert_eq!(heard, 0, "frames that do not open are not handed on");

    alice.to_call.send(ToCall::End).expect("running");
    bob.to_call.send(ToCall::End).expect("running");
    let _ = timeout(PATIENCE, alice.task).await;
    let _ = timeout(PATIENCE, bob.task).await;
}
