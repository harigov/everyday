//! The whole path, over a real homeserver: an owner on one side, the
//! assistant's bot on the other, and a scripted model behind it.
//!
//! Everything else about chat is tested with something standing in for the
//! other end -- a fake `Connector` for the relay, a fake runner for the
//! service. This is the one test where nothing is faked but the language
//! model: a real Matrix server, three real accounts, end-to-end encryption
//! between them, the real `everyday_chat::Runner` under the real supervisor,
//! and the real agent harness talking to a scripted Chat Completions model on
//! loopback (the pattern from `everyday-service/tests/routines.rs`). It is
//! what shows that a message typed in Element comes out as an answer in
//! Element, that a delete waits for a tapped reaction, and that a stranger
//! gets nothing at all.
//!
//! # Running it
//!
//! It skips itself, printing why, unless `EVERYDAY_TEST_MATRIX_URL` names a
//! homeserver -- a local Conduit with open registration, started for the
//! purpose:
//!
//! ```text
//! docker run --rm -d --name everyday-conduit-test -p 127.0.0.1:16167:6167 \
//!   -e CONDUIT_SERVER_NAME=localhost -e CONDUIT_ADDRESS=0.0.0.0 -e CONDUIT_PORT=6167 \
//!   -e CONDUIT_DATABASE_BACKEND=rocksdb -e CONDUIT_DATABASE_PATH=/var/lib/matrix-conduit \
//!   -e CONDUIT_ALLOW_REGISTRATION=true -e CONDUIT_ALLOW_FEDERATION=false \
//!   -e CONDUIT_ALLOW_CHECK_FOR_UPDATES=false -e CONDUIT_TRUSTED_SERVERS='[]' \
//!   -e CONDUIT_MAX_REQUEST_SIZE=20000000 \
//!   matrixconduit/matrix-conduit:v0.10.12
//!
//! EVERYDAY_TEST_MATRIX_URL=http://127.0.0.1:16167 \
//!   cargo test -p everyday-chat --test matrix_live -- --nocapture
//! ```
//!
//! A server that wants a registration token takes it from
//! `EVERYDAY_TEST_MATRIX_TOKEN`.
//!
//! **Never point it at a real server.** It registers three accounts with
//! random names on every run and leaves them there. To make that mistake
//! hard, an address that is not this machine or a private network is refused
//! unless `EVERYDAY_TEST_MATRIX_ALLOW_REMOTE` is also set.
//!
//! # How it listens
//!
//! The owner and the stranger are ordinary `matrix-sdk` clients, each with an
//! encrypted-capable SQLite store of its own, signed in with a password and
//! syncing in a background task -- Element, in miniature. What they *hear* is
//! not taken from event handlers but by asking each joined room for its recent
//! messages every few hundred milliseconds. `/messages` decrypts as it goes,
//! and it does not matter which sync response a message happened to arrive in,
//! or whether the room key got there before the message did: an event that
//! cannot be read yet is simply read on a later look. Handlers would hear a
//! message exactly once, at the moment it could not yet be decrypted.

use everyday_chat::Runner;
use everyday_core::chat::{ChatLink, ChatService, MatrixLink};
use everyday_core::{AgentSettings, ChatLinkId, Task, Vault, VaultConfig};
use everyday_service::Service;
use everyday_service::chat::{ChatState, FromRun, task_key};
use everyday_service::ctx::Ctx;
use everyday_service::supervisor::Supervisor;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::room::MessagesOptions;
use matrix_sdk::ruma::api::client::account::register::v3::Request as RegistrationRequest;
use matrix_sdk::ruma::api::client::uiaa::{AuthData, AuthType, Dummy, RegistrationToken, UiaaInfo};
use matrix_sdk::ruma::events::reaction::ReactionEventContent;
use matrix_sdk::ruma::events::relation::Annotation;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::ruma::{OwnedEventId, OwnedRoomId, OwnedUserId, RoomId, UserId};
use matrix_sdk::{Client, RoomMemberships};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// How long to wait for any one thing that has to happen: a sign-in, a
/// greeting, an answer. Generous on purpose -- a cold Conduit, an encrypted
/// room's first key exchange and a model turn are each a few seconds on a
/// slow machine, and a test that fails because the machine was busy is
/// worse than one that takes a minute to fail.
const WAIT: Duration = Duration::from_secs(60);

/// How often the owner looks at its rooms.
const POLL: Duration = Duration::from_millis(300);

/// How long to keep watching, after the message that should be the only one,
/// for a second that should not exist.
const QUIET: Duration = Duration::from_secs(4);

/// How long a stranger's message is given to produce a reply that must never
/// come. The bot's sync loop and a model turn both fit well inside it, so
/// silence at the end means refusal rather than slowness.
const STRANGER_QUIET: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// What the fake model says to the next request it gets.
#[derive(Clone)]
enum Reply {
    /// Some words, no tools.
    Says(String),
    /// A request for one tool, by name, with these JSON arguments. The harness
    /// runs it and asks again, so a turn that uses a tool is two replies in
    /// the script: this, then what to say once it is done.
    Calls { tool: String, arguments: String },
}

/// A scripted Chat Completions model on loopback.
///
/// One queue of replies, taken in order, one per request, rather than one
/// fixed behaviour: the scenarios run one after another against the same
/// assistant, and each needs the model to say something different. A test
/// pushes what the next turn will take before it sends the message that
/// starts it. A request that arrives with nothing queued is answered with a
/// placeholder and counted, so a turn that asked for more than was scripted
/// is reported at the end instead of hanging or passing by accident.
struct FakeModel {
    endpoint: String,
    script: Arc<Mutex<VecDeque<Reply>>>,
    /// Every request body received, in order. Lets a test ask whether the
    /// model was ever shown some words -- or, for a stranger's, never.
    requests: Arc<Mutex<Vec<String>>>,
    unscripted: Arc<AtomicUsize>,
    /// Dropping this stops the listener.
    _shutdown: tokio::sync::watch::Sender<bool>,
}

impl FakeModel {
    fn push(&self, reply: Reply) {
        self.script.lock().unwrap().push_back(reply);
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn unscripted(&self) -> usize {
        self.unscripted.load(Ordering::SeqCst)
    }
}

fn says(text: &str) -> Reply {
    Reply::Says(text.to_string())
}

fn calls(tool: &str, arguments: Value) -> Reply {
    Reply::Calls { tool: tool.to_string(), arguments: arguments.to_string() }
}

async fn fake_model() -> FakeModel {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let script = Arc::new(Mutex::new(VecDeque::new()));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let unscripted = Arc::new(AtomicUsize::new(0));
    let (tx, mut rx) = tokio::sync::watch::channel(false);

    let (s, r, u) = (script.clone(), requests.clone(), unscripted.clone());
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                accepted = listener.accept() => accepted,
                _ = rx.changed() => break,
            };
            let Ok((socket, _)) = accepted else { break };
            tokio::spawn(answer(socket, s.clone(), r.clone(), u.clone()));
        }
    });

    FakeModel {
        endpoint: format!("http://127.0.0.1:{port}/v1"),
        script,
        requests,
        unscripted,
        _shutdown: tx,
    }
}

/// Read one request in full, record it, and answer with the next scripted
/// reply.
///
/// The whole body is read, unlike the routines test's single `read`: that
/// test never looks at what it was asked, but this one does, and an assistant
/// with all its tool definitions in the request is well over one read's worth.
async fn answer(
    mut socket: TcpStream,
    script: Arc<Mutex<VecDeque<Reply>>>,
    requests: Arc<Mutex<Vec<String>>>,
    unscripted: Arc<AtomicUsize>,
) {
    let Some(body) = read_request(&mut socket).await else { return };
    requests.lock().unwrap().push(body);

    let next = script.lock().unwrap().pop_front();
    let reply = next.unwrap_or_else(|| {
        unscripted.fetch_add(1, Ordering::SeqCst);
        Reply::Says("(the scripted model was asked for more than it was given)".into())
    });
    let body = match &reply {
        Reply::Says(text) => stream(text, None),
        Reply::Calls { tool, arguments } => stream("", Some((tool.as_str(), arguments.as_str()))),
    };

    // Server-sent events, not a JSON document: the harness streams every
    // turn, so a plain body is refused on its content type before it is read.
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.flush().await;
    let _ = socket.shutdown().await;
}

/// The body of a request, once all of it has arrived.
async fn read_request(socket: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 16 * 1024];

    let (body_at, length, chunked) = loop {
        let n = tokio::time::timeout(Duration::from_secs(10), socket.read(&mut chunk))
            .await
            .ok()?
            .ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            break (end + 4, length, head.contains("transfer-encoding: chunked"));
        }
    };

    // Complete when the declared length has arrived -- or, for a chunked
    // body, when the terminating empty chunk has. The framing stays in what
    // is recorded; it is only ever searched for words.
    loop {
        let done =
            if chunked { buf.ends_with(b"0\r\n\r\n") } else { buf.len() >= body_at + length };
        if done {
            return Some(String::from_utf8_lossy(&buf[body_at..]).into_owned());
        }
        let n = tokio::time::timeout(Duration::from_secs(10), socket.read(&mut chunk))
            .await
            .ok()?
            .ok()?;
        if n == 0 {
            return Some(String::from_utf8_lossy(&buf[body_at..]).into_owned());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// One answer, as a stream of Chat Completions chunks. See `routines.rs`:
/// the harness streams every turn, so this has to be `text/event-stream`
/// with `data:` frames.
fn stream(text: &str, call: Option<(&str, &str)>) -> String {
    let mut out = String::new();
    let mut frame = |delta: Value, finish: Option<&str>| {
        let chunk = json!({
            "id": "chatcmpl-test",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "scripted",
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
        });
        out.push_str(&format!("data: {chunk}\n\n"));
    };

    match call {
        Some((tool, arguments)) => {
            frame(
                json!({
                    "role": "assistant",
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": tool, "arguments": arguments },
                    }],
                }),
                None,
            );
            frame(json!({}), Some("tool_calls"));
        }
        None => {
            frame(json!({ "role": "assistant", "content": text }), None);
            frame(json!({}), Some("stop"));
        }
    }
    out.push_str("data: [DONE]\n\n");
    out
}

// ---------------------------------------------------------------------------
// The vault and the service
// ---------------------------------------------------------------------------

/// A vault with the assistant switched on and pointed at the fake model, one
/// task in it, and a service around it with a real supervisor and the real
/// chat runner installed.
struct Rig {
    svc: Arc<Service>,
    vault: Arc<Vault>,
    /// The task the delete scenario asks the assistant to remove.
    task: Task,
    _dir: tempfile::TempDir,
}

fn rig(endpoint: &str) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let config = VaultConfig {
        name: "Chat".into(),
        backend: "sqlite".into(),
        settings: Default::default(),
        password: None,
        kdf: Default::default(),
        auto_lock_seconds: 900,
        forget_key_seconds: 0,
    };
    let vault = everyday_vault::create(dir.path(), config).unwrap();
    let task = Task::new("Renew the passport");
    vault.save_task(&task).unwrap();

    let settings = AgentSettings {
        enabled: true,
        timezone: Some("UTC".into()),
        // Off on the desktop, and it must make no difference here: a chat
        // turn confirms a delete whatever this says, because a phone is a
        // likelier thing to lose than a desk. The scenario below is the
        // proof, so the setting has to be the one that would skip the question.
        confirm_destructive: false,
        ..Default::default()
    };
    vault.save_agent_settings(&settings).unwrap();
    let mut models = everyday_core::llm::ModelSettings::default();
    models.medium.connection.base_url = Some(endpoint.to_string());
    models.medium.model.model = "scripted".into();
    vault.save_model_settings(&models).unwrap();

    let svc = Arc::new(Service::new());
    let vault = svc.set(vault);
    // The shells install a supervisor that announces through the final event
    // sink; the one `Service::new` starts with is a placeholder that does too,
    // but a test says so rather than leaning on that.
    svc.set_supervisor(Arc::new(Supervisor::new(svc.events())));
    svc.set_chat_runner(Arc::new(Runner));

    Rig { svc, vault, task, _dir: dir }
}

// ---------------------------------------------------------------------------
// The homeserver
// ---------------------------------------------------------------------------

/// The server to test against, or `None` -- after saying why not -- when none
/// was named.
fn homeserver() -> Option<String> {
    let Some(url) = std::env::var("EVERYDAY_TEST_MATRIX_URL").ok().filter(|u| !u.trim().is_empty())
    else {
        eprintln!(
            "skipping: set EVERYDAY_TEST_MATRIX_URL (e.g. http://127.0.0.1:16167, a throwaway \
             Conduit with open registration) to run the Matrix end-to-end test"
        );
        return None;
    };
    let url = url.trim().trim_end_matches('/').to_string();
    assert!(
        is_local(&url) || std::env::var_os("EVERYDAY_TEST_MATRIX_ALLOW_REMOTE").is_some(),
        "refusing to register test accounts on {url}: it is not this machine or a private \
         network. This test creates three users on every run and must only ever meet a \
         throwaway server. If this one really is, set EVERYDAY_TEST_MATRIX_ALLOW_REMOTE=1."
    );
    Some(url)
}

/// Whether `url` names this machine or a private network -- somewhere a
/// throwaway server can be, and a real one with strangers on it rarely is.
fn is_local(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split('/').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    if host == "localhost" || host == "::1" {
        return true;
    }
    host.parse::<std::net::Ipv4Addr>().is_ok_and(|ip| ip.is_loopback() || ip.is_private())
}

/// A fresh random string, for names and passwords that must not collide with
/// the last run's.
fn random() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Make a new account and return the Matrix ID the server gave it.
///
/// Registration is user-interactive authentication: the first request is
/// refused with the flows the server will accept, and is repeated with each
/// stage's answer until it goes through. Only the two stages a test server
/// has -- `m.login.dummy` for open registration, `m.login.registration_token`
/// when a token is required -- are understood; anything else is reported with
/// what the server asked for. The account is created without logging in
/// (`inhibit_login`): each of them signs in on its own client, with a store of
/// its own, as a person's devices do.
async fn register(
    homeserver: &str,
    token: Option<&str>,
    localpart: &str,
    password: &str,
) -> OwnedUserId {
    let client = Client::builder()
        .homeserver_url(homeserver)
        .build()
        .await
        .unwrap_or_else(|e| panic!("cannot reach the homeserver at {homeserver}: {e}"));

    let mut answer: Option<AuthData> = None;
    for _ in 0..5 {
        let mut request = RegistrationRequest::new();
        request.username = Some(localpart.to_owned());
        request.password = Some(password.to_owned());
        request.inhibit_login = true;
        request.auth = answer.take();

        let refused = match client.matrix_auth().register(request).await {
            Ok(done) => return done.user_id,
            Err(e) => e,
        };
        let Some(info) = refused.as_uiaa_response() else {
            panic!(
                "the homeserver refused to register {localpart}: {refused}\n\
                 (does it allow registration? Conduit needs CONDUIT_ALLOW_REGISTRATION=true)"
            );
        };
        answer = Some(next_stage(info, token, localpart));
    }
    panic!("registering {localpart} still was not finished after five rounds of authentication");
}

/// The answer to the first stage of a registration flow that has not been
/// completed, choosing a flow made only of stages this test can pass.
fn next_stage(info: &UiaaInfo, token: Option<&str>, localpart: &str) -> AuthData {
    let passable = |stage: &AuthType| match stage {
        AuthType::Dummy => true,
        AuthType::RegistrationToken => token.is_some(),
        _ => false,
    };
    let Some(flow) =
        info.flows.iter().find(|f| !f.stages.is_empty() && f.stages.iter().all(&passable))
    else {
        let wanted: Vec<String> = info
            .flows
            .iter()
            .map(|f| f.stages.iter().map(|s| format!("{s}")).collect::<Vec<_>>().join(" + "))
            .collect();
        panic!(
            "registering {localpart}: the homeserver wants [{}], and this test can do \
             m.login.dummy, or m.login.registration_token with EVERYDAY_TEST_MATRIX_TOKEN set",
            wanted.join(" | ")
        );
    };
    let stage = flow.stages.iter().find(|s| !info.completed.contains(s)).unwrap_or_else(|| {
        panic!("registering {localpart}: every stage is done yet it was refused")
    });

    let session = info.session.clone();
    match stage {
        AuthType::RegistrationToken => {
            let mut answer = RegistrationToken::new(token.unwrap_or_default().to_owned());
            answer.session = session;
            AuthData::RegistrationToken(answer)
        }
        _ => {
            let mut answer = Dummy::new();
            answer.session = session;
            AuthData::Dummy(answer)
        }
    }
}

// ---------------------------------------------------------------------------
// A person at a chat client
// ---------------------------------------------------------------------------

/// What somebody in a room did, as far as this test cares.
#[derive(Debug, Clone)]
struct Heard {
    event: OwnedEventId,
    room: OwnedRoomId,
    sender: OwnedUserId,
    what: What,
}

#[derive(Debug, Clone)]
enum What {
    /// Words: a `m.text` or `m.notice` message's body.
    Text(String),
    /// A reaction to `to`, with its emoji.
    Reaction { to: OwnedEventId, key: String },
}

/// The words of a message, or nothing for a reaction.
fn body(heard: &Heard) -> &str {
    match &heard.what {
        What::Text(text) => text,
        What::Reaction { .. } => "",
    }
}

/// Everything one client has heard, in the order it came to light.
#[derive(Default)]
struct Shared {
    heard: Vec<Heard>,
    /// Events in the last look that could not be decrypted (yet). Kept only to
    /// explain a timeout: an answer that never comes and an answer that came
    /// in a language the client could not read look the same otherwise.
    undecryptable: usize,
    /// The last thing that went wrong while syncing or looking.
    trouble: Option<String>,
}

fn note(shared: &Mutex<Shared>, trouble: String) {
    shared.lock().unwrap().trouble = Some(trouble);
}

/// One account on its own client -- its own device, its own SQLite store --
/// syncing and watching in the background, the way an Element on a phone does.
struct Person {
    id: OwnedUserId,
    client: Client,
    shared: Arc<Mutex<Shared>>,
    tasks: Vec<JoinHandle<()>>,
    _store: tempfile::TempDir,
}

impl Person {
    /// Sign in with a password, sync once so the device's keys are on the
    /// server before anybody needs to encrypt to it, then keep syncing and
    /// keep looking.
    ///
    /// The first sync matters for ordering: the bot's greeting is encrypted to
    /// the devices the owner has *when it is sent*, so an owner that signed in
    /// after the bot spoke could never read it.
    async fn sign_in(homeserver: &str, id: &UserId, password: &str, device: &str) -> Person {
        let store = tempfile::tempdir().unwrap();
        let client = Client::builder()
            .homeserver_url(homeserver)
            .sqlite_store(store.path(), None)
            .build()
            .await
            .unwrap_or_else(|e| panic!("building a client for {id}: {e}"));
        client
            .matrix_auth()
            .login_username(id.as_str(), password)
            .initial_device_display_name(device)
            .send()
            .await
            .unwrap_or_else(|e| panic!("{id} could not sign in: {e}"));
        client.encryption().wait_for_e2ee_initialization_tasks().await;
        client
            .sync_once(
                SyncSettings::default()
                    .timeout(Duration::from_secs(1))
                    .ignore_timeout_on_first_sync(true),
            )
            .await
            .unwrap_or_else(|e| panic!("{id}'s first sync failed: {e}"));

        let shared = Arc::new(Mutex::new(Shared::default()));

        let syncing: JoinHandle<()> = tokio::spawn({
            let client = client.clone();
            let shared = shared.clone();
            async move {
                loop {
                    let settings = SyncSettings::default().timeout(Duration::from_secs(2));
                    if let Err(e) = client.sync_once(settings).await {
                        note(&shared, format!("sync: {e}"));
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
            }
        });
        let watching: JoinHandle<()> = tokio::spawn({
            let client = client.clone();
            let shared = shared.clone();
            async move {
                loop {
                    look(&client, &shared).await;
                    tokio::time::sleep(POLL).await;
                }
            }
        });

        Person { id: id.to_owned(), client, shared, tasks: vec![syncing, watching], _store: store }
    }

    /// The number of things heard so far: a place to stand when a scenario
    /// starts, so that only what comes after is looked at.
    fn mark(&self) -> usize {
        self.shared.lock().unwrap().heard.len()
    }

    /// Wait for the first thing after `*seen` that `wanted` accepts, and move
    /// `*seen` past it. Anything skipped on the way -- one's own messages, the
    /// bot's reactions -- is passed over too. `None` if nothing came in time.
    async fn try_wait_for(
        &self,
        seen: &mut usize,
        within: Duration,
        wanted: impl Fn(&Heard) -> bool,
    ) -> Option<Heard> {
        let deadline = Instant::now() + within;
        loop {
            {
                let shared = self.shared.lock().unwrap();
                if let Some(at) = shared.heard.iter().skip(*seen).position(&wanted) {
                    let found = shared.heard[*seen + at].clone();
                    *seen += at + 1;
                    return Some(found);
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// As [`Person::try_wait_for`], but a timeout is a failure, and says what
    /// this client could see when it gave up.
    async fn wait_for(
        &self,
        seen: &mut usize,
        what: &str,
        wanted: impl Fn(&Heard) -> bool,
    ) -> Heard {
        match self.try_wait_for(seen, WAIT, wanted).await {
            Some(found) => found,
            None => panic!("timed out after {WAIT:?} waiting for {what}\n{}", self.describe()),
        }
    }

    /// Whether `ready` becomes true of everything heard within `within`.
    async fn wait_until(&self, within: Duration, ready: impl Fn(&Shared) -> bool) -> bool {
        let deadline = Instant::now() + within;
        loop {
            let is_ready = {
                let shared = self.shared.lock().unwrap();
                ready(&shared)
            };
            if is_ready {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Watch for `within`, then say whether anything after `seen` that
    /// `unwanted` accepts turned up. The check for "exactly one message" and
    /// for "no reply at all".
    async fn anything_more(
        &self,
        seen: usize,
        within: Duration,
        unwanted: impl Fn(&Heard) -> bool,
    ) -> Option<Heard> {
        tokio::time::sleep(within).await;
        let shared = self.shared.lock().unwrap();
        shared.heard.iter().skip(seen).find(|&h| unwanted(h)).cloned()
    }

    /// Send `text` to `room`, encrypted if the room is.
    async fn say(&self, room: &RoomId, text: &str) {
        let joined = self
            .client
            .get_room(room)
            .unwrap_or_else(|| panic!("{} is not in {room}\n{}", self.id, self.describe()));
        joined
            .send(RoomMessageEventContent::text_plain(text))
            .await
            .unwrap_or_else(|e| panic!("{} could not send {text:?}: {e}", self.id));
    }

    /// React to `event` in `room`, as tapping an emoji under a message does.
    async fn react(&self, room: &RoomId, event: &OwnedEventId, key: &str) {
        let joined = self
            .client
            .get_room(room)
            .unwrap_or_else(|| panic!("{} is not in {room}\n{}", self.id, self.describe()));
        joined
            .send(ReactionEventContent::new(Annotation::new(event.clone(), key.to_owned())))
            .await
            .unwrap_or_else(|e| panic!("{} could not react {key} to {event}: {e}", self.id));
    }

    /// What this client has seen, for a failure message.
    fn describe(&self) -> String {
        let shared = self.shared.lock().unwrap();
        let mut out = format!(
            "{} is in {} room(s) and invited to {}; heard {} event(s); {} undecryptable on the \
             last look",
            self.id,
            self.client.joined_rooms().len(),
            self.client.invited_rooms().len(),
            shared.heard.len(),
            shared.undecryptable,
        );
        for heard in &shared.heard {
            match &heard.what {
                What::Text(text) => out.push_str(&format!("\n  {} said {text:?}", heard.sender)),
                What::Reaction { to, key } => {
                    out.push_str(&format!("\n  {} reacted {key} to {to}", heard.sender));
                }
            }
        }
        if let Some(trouble) = &shared.trouble {
            out.push_str(&format!("\n  last trouble: {trouble}"));
        }
        out
    }
}

impl Drop for Person {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// One look at every room: accept any invitation (the only account that ever
/// invites the owner is the bot), then read each joined room's recent
/// messages, newest last, and add what is new.
async fn look(client: &Client, shared: &Mutex<Shared>) {
    for room in client.invited_rooms() {
        if let Err(e) = room.join().await {
            note(shared, format!("joining {}: {e}", room.room_id()));
        }
    }

    let mut undecryptable = 0;
    for room in client.joined_rooms() {
        let mut options = MessagesOptions::backward();
        options.limit = 100u32.into();
        let page = match room.messages(options).await {
            Ok(page) => page,
            Err(e) => {
                note(shared, format!("reading {}: {e}", room.room_id()));
                continue;
            }
        };

        let mut fresh = Vec::new();
        for event in page.chunk.iter().rev() {
            match read(room.room_id(), event.raw().json().get()) {
                Read::Heard(heard) => fresh.push(heard),
                Read::Undecryptable => undecryptable += 1,
                Read::Other => {}
            }
        }
        let mut log = shared.lock().unwrap();
        for heard in fresh {
            if !log.heard.iter().any(|h| h.event == heard.event) {
                log.heard.push(heard);
            }
        }
    }
    shared.lock().unwrap().undecryptable = undecryptable;
}

enum Read {
    Heard(Heard),
    /// An encrypted event whose key has not arrived. Looked at again next time.
    Undecryptable,
    /// Anything else a room holds: membership, settings, edits.
    Other,
}

/// One event, as the JSON the SDK hands back -- already decrypted if it could
/// be. Read as plain JSON rather than through `ruma`'s event enums, because
/// four fields are all this wants and they are not going to change shape.
fn read(room: &RoomId, raw: &str) -> Read {
    let Ok(json) = serde_json::from_str::<Value>(raw) else { return Read::Other };
    if json["type"] == "m.room.encrypted" {
        return Read::Undecryptable;
    }
    match heard(room, &json) {
        Some(heard) => Read::Heard(heard),
        None => Read::Other,
    }
}

fn heard(room: &RoomId, json: &Value) -> Option<Heard> {
    let event: OwnedEventId = serde_json::from_value(json["event_id"].clone()).ok()?;
    let sender: OwnedUserId = serde_json::from_value(json["sender"].clone()).ok()?;
    let content = &json["content"];
    let relation = &content["m.relates_to"];
    let what = match json["type"].as_str()? {
        "m.room.message" => {
            // An edit is the same message again, not a new one.
            if relation["rel_type"] == "m.replace" {
                return None;
            }
            What::Text(content["body"].as_str()?.to_owned())
        }
        "m.reaction" => What::Reaction {
            to: serde_json::from_value(relation["event_id"].clone()).ok()?,
            key: relation["key"].as_str()?.to_owned(),
        },
        _ => return None,
    };
    Some(Heard { event, room: room.to_owned(), sender, what })
}

/// A message of words from the bot.
fn text_from(bot: &UserId) -> impl Fn(&Heard) -> bool + '_ {
    move |h| h.sender.as_str() == bot.as_str() && matches!(h.what, What::Text(_))
}

/// Whether a reaction's emoji is the "yes" tick, with or without the variation
/// selector some clients add.
fn is_yes(key: &str) -> bool {
    key.trim_end_matches('\u{fe0f}') == "✅"
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

fn step(what: &str) {
    eprintln!("matrix_live: {what}");
}

/// Wait for the link to report `Online`, or say why it did not.
async fn wait_online(svc: &Arc<Service>, link: ChatLinkId) {
    let deadline = Instant::now() + WAIT;
    loop {
        let status = svc.chat_status(link);
        match status.state {
            ChatState::Online => return,
            // Nothing will change until somebody types a password; waiting out
            // the minute would only hide the reason.
            ChatState::NeedsSignIn => {
                panic!("the bot cannot sign in: {}", status.detail);
            }
            _ if Instant::now() >= deadline => panic!(
                "the bot was not online after {WAIT:?}: {:?} - {}",
                status.state, status.detail
            ),
            _ => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_talks_to_the_assistant_over_encrypted_matrix_and_a_stranger_does_not() {
    let Some(homeserver) = homeserver() else { return };
    let token = std::env::var("EVERYDAY_TEST_MATRIX_TOKEN").ok().filter(|t| !t.is_empty());
    let started = Instant::now();

    // Things that went wrong but should not stop the other scenarios running:
    // checked once, at the end, so one broken behaviour does not hide the rest.
    let mut problems: Vec<String> = Vec::new();

    // ---- three accounts -------------------------------------------------
    step("registering the bot, the owner and a stranger");
    let suffix: String = random().chars().take(8).collect();
    let (bot_password, owner_password, stranger_password) = (random(), random(), random());
    let bot =
        register(&homeserver, token.as_deref(), &format!("everyday-bot-{suffix}"), &bot_password)
            .await;
    let owner_id = register(
        &homeserver,
        token.as_deref(),
        &format!("everyday-owner-{suffix}"),
        &owner_password,
    )
    .await;
    let stranger_id = register(
        &homeserver,
        token.as_deref(),
        &format!("everyday-stranger-{suffix}"),
        &stranger_password,
    )
    .await;

    // ---- the assistant --------------------------------------------------
    let model = fake_model().await;
    let rig = rig(&model.endpoint);

    // The owner signs in first and has synced before the bot exists, so its
    // device keys are on the server by the time the bot greets it.
    step("the owner signs in on a client of its own");
    let owner = Person::sign_in(&homeserver, &owner_id, &owner_password, "Test owner").await;

    step("saving the chat link");
    let mut link = ChatLink::new_matrix();
    link.service = ChatService::Matrix(MatrixLink {
        homeserver: homeserver.clone(),
        user_id: bot.to_string(),
        owner: owner_id.to_string(),
    });
    rig.svc
        .call(Ctx::local(), "save_chat_link", json!({ "link": link, "password": bot_password }))
        .await
        .unwrap_or_else(|e| panic!("save_chat_link: {e}"));
    wait_online(&rig.svc, link.id).await;
    eprintln!("matrix_live: bot online: {}", rig.svc.chat_status(link.id).detail);

    // ---- the greeting ---------------------------------------------------
    // The bot makes an encrypted room with the owner, invites it and says
    // hello. The owner's client accepts the invitation by itself.
    step("waiting for the greeting in an encrypted room");
    let mut seen = 0;
    let greeting = owner.try_wait_for(&mut seen, WAIT, text_from(&bot)).await;
    let room = match &greeting {
        Some(greeting) => {
            assert!(
                body(greeting).contains("!help"),
                "the greeting should point at !help, got: {:?}",
                body(greeting)
            );
            greeting.room.clone()
        }
        None => {
            // The rest can still be tried if the owner is in the room; what is
            // wrong then is the greeting, not everything after it.
            let undecryptable = owner.shared.lock().unwrap().undecryptable;
            problems.push(format!(
                "no readable greeting from the bot within {WAIT:?} ({undecryptable} event(s) \
                 the owner could not decrypt; if there are any, the bot probably sent it before \
                 its first sync had told it who was in the new room, so it was encrypted to \
                 nobody)\n{}",
                owner.describe()
            ));
            match owner.client.joined_rooms().as_slice() {
                [only] => only.room_id().to_owned(),
                _ => panic!("the owner never joined the bot's room\n{}", owner.describe()),
            }
        }
    };

    // ---- scenario 1: a plain answer -------------------------------------
    step("scenario 1: hello");
    model.push(says("Hi there."));
    owner.say(&room, "hello").await;
    let reply = owner.wait_for(&mut seen, "the assistant's answer to hello", text_from(&bot)).await;
    assert_eq!(
        body(&reply),
        "Hi there.",
        "the owner should get the model's words and nothing else"
    );
    if let Some(extra) = owner.anything_more(seen, QUIET, text_from(&bot)).await {
        panic!("one turn should be one message, but a second followed: {:?}", body(&extra));
    }
    assert!(
        model.requests().iter().any(|r| r.contains("hello")),
        "the model should have been shown what the owner wrote"
    );

    // ---- commands -------------------------------------------------------
    step("!help");
    let asked = model.requests().len();
    owner.say(&room, "!help").await;
    let help = owner.wait_for(&mut seen, "the reply to !help", text_from(&bot)).await;
    assert!(body(&help).contains("!new"), "!help should list the commands, got: {:?}", body(&help));
    assert_eq!(
        model.requests().len(),
        asked,
        "a command is the bot's own business and must not reach the model"
    );

    // ---- scenario 2: a delete waits for a tapped reaction ---------------
    step("scenario 2: delete a task, confirmed by reacting");
    model.push(calls("delete_task", json!({ "task_id": rig.task.id.to_string() })));
    model.push(says("Deleted it."));
    owner.say(&room, "Please delete the passport task.").await;

    let question =
        owner.wait_for(&mut seen, "the question before the delete", text_from(&bot)).await;
    assert_ne!(
        body(&question),
        "Deleted it.",
        "the bot answered instead of asking: a chat turn must confirm a delete even with \
         confirm_destructive off"
    );
    assert!(
        body(&question).contains("Renew the passport"),
        "the question should say what would be deleted, got: {:?}",
        body(&question)
    );
    assert!(
        rig.vault.task(rig.task.id).is_ok(),
        "the task must still be there while the question is unanswered"
    );

    // The choices are drawn as reactions on the question, so that tapping one
    // is all it takes.
    let offered = owner
        .wait_until(Duration::from_secs(30), |s| {
            s.heard.iter().any(|h| {
                h.sender == bot
                    && matches!(&h.what, What::Reaction { to, key } if *to == question.event && is_yes(key))
            })
        })
        .await;
    if !offered {
        problems.push("the bot never offered ✅ as a reaction on its question".into());
    }

    owner.react(&room, &question.event, "✅").await;
    let done = owner.wait_for(&mut seen, "the answer after confirming", text_from(&bot)).await;
    assert_eq!(body(&done), "Deleted it.", "the turn should finish with the model's closing words");
    assert!(
        rig.vault.task(rig.task.id).is_err(),
        "the task should be gone once the owner said yes"
    );
    if let Some(extra) = owner.anything_more(seen, QUIET, text_from(&bot)).await {
        panic!(
            "the confirmed turn should end with one message, but another followed: {:?}",
            body(&extra)
        );
    }

    // ---- scenario 2b: a routine reaches the owner unasked ----------------
    // What `Delivery` uses: the relay's outlet finds the owner's private room
    // by itself, puts a routine's own question there, takes the owner's next
    // message as the answer, and posts the routine's result.
    step("scenario 2b: a routine asks a question and posts its result");
    let outlet = rig.svc.chat_outlet(link.id).expect("the running link offers an outlet");
    let from = FromRun { routine: "Morning brief".into(), conversation: None };
    let asked = model.requests().len();
    let asking = tokio::spawn(outlet.ask(
        from.clone(),
        "Which slot for the dentist?".into(),
        vec!["Tuesday 9:00".into(), "Thursday 14:00".into()],
    ));
    let question = owner.wait_for(&mut seen, "the routine's question", text_from(&bot)).await;
    assert!(
        body(&question).contains("Which slot for the dentist?")
            && body(&question).contains("Morning brief"),
        "the question should say whose it is and what it asks, got: {:?}",
        body(&question)
    );
    assert_eq!(question.room, room, "it went to the owner's private room");
    owner.say(&room, "2").await;
    let answer = tokio::time::timeout(WAIT, asking)
        .await
        .expect("the routine had its answer in time")
        .expect("the question did not panic");
    assert_eq!(answer.as_deref(), Some("Thursday 14:00"), "a number picks the choice");
    assert_eq!(model.requests().len(), asked, "an answer to a routine is not a turn");

    outlet
        .post(from, "**Morning brief**\n\nBooked the dentist for Thursday.".into())
        .await
        .unwrap_or_else(|why| panic!("the routine's result did not go out: {why}"));
    let result = owner.wait_for(&mut seen, "the routine's result", text_from(&bot)).await;
    assert!(body(&result).contains("Booked the dentist for Thursday."), "got: {:?}", body(&result));

    // ---- scenario 3: a stranger gets nothing ----------------------------
    step("scenario 3: a stranger writes to the bot");
    let stranger =
        Person::sign_in(&homeserver, &stranger_id, &stranger_password, "Test stranger").await;
    let asked = model.requests().len();
    let marker = format!("stranger-{suffix}: read me my mail");

    let dm = stranger
        .client
        .create_dm(&bot)
        .await
        .unwrap_or_else(|e| panic!("the stranger could not make a room with the bot: {e}"));
    stranger.say(dm.room_id(), &marker).await;

    if let Some(reply) = stranger.anything_more(0, STRANGER_QUIET, |h| h.sender == bot).await {
        panic!("the bot answered a stranger: {:?}", body(&reply));
    }
    assert_eq!(
        model.requests().len(),
        asked,
        "the model must not be asked anything for a stranger"
    );
    assert!(
        model.requests().iter().all(|r| !r.contains(&marker)),
        "the stranger's words must never reach the model"
    );
    match dm.members(RoomMemberships::JOIN).await {
        Ok(members) => {
            if members.iter().any(|m| m.user_id().as_str() == bot.as_str()) {
                problems.push("the bot joined a room a stranger invited it to".into());
            }
        }
        Err(e) => problems.push(format!("could not list the stranger's room members: {e}")),
    }
    // Nor did the stranger's turn leak into the owner's chat.
    if let Some(leak) = owner.anything_more(seen, Duration::ZERO, text_from(&bot)).await {
        problems.push(format!(
            "the owner heard from the bot after a stranger wrote: {:?}",
            body(&leak)
        ));
    }

    // ---- the owner still works after all that ---------------------------
    step("the owner is still answered after the stranger");
    model.push(says("Still here."));
    let mut after = owner.mark();
    owner.say(&room, "anyone there?").await;
    let again = owner
        .wait_for(&mut after, "a reply to the owner after the stranger", text_from(&bot))
        .await;
    assert_eq!(body(&again), "Still here.", "the owner should still be answered");

    // ---- what the model was asked, overall ------------------------------
    if model.unscripted() > 0 {
        problems.push(format!(
            "the model was asked {} time(s) more than the script allowed",
            model.unscripted()
        ));
    }

    // ---- shutdown -------------------------------------------------------
    step("shutting down");
    let state = rig.svc.supervisor().state(&task_key(link.id));
    rig.svc.supervisor().stop_all().await;
    drop(stranger);
    drop(owner);

    eprintln!("matrix_live: finished in {:?} (supervisor had {state:?})", started.elapsed());
    assert!(problems.is_empty(), "the scenarios ran, but:\n- {}", problems.join("\n- "));
}
