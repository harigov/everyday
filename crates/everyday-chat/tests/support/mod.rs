//! What the relay's tests stand on: a connector that writes down what it is
//! asked to do, a model that says whatever it is told to, and a vault with a
//! task in it to be deleted.
//!
//! Rust compiles every file in `tests/` as its own binary, so this is shared
//! by `mod support;` rather than imported, and the declaration carries
//! `#[allow(dead_code)]`: no one binary uses all of it.

use async_trait::async_trait;
use everyday_chat::connector::{
    ChatError, ChatId, Choice, Connector, Incoming, MessageRef, Question,
};
use everyday_chat::relay::{self, Timing};
use everyday_core::chat::{ChatLink, ChatService, MatrixLink};
use everyday_core::{Conversation, ConversationQuery, Task, Vault};
use everyday_service::Service;
use jiff::Timestamp;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

// ---- a connector that writes things down ------------------------------------

/// One thing the relay asked the connector to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Sent { chat: ChatId, text: String },
    Asked { chat: ChatId, question: Question, reference: MessageRef },
    Typing { chat: ChatId, on: bool },
}

/// A [`Connector`] that records everything it is asked to do, in order, and
/// delivers whatever a test hands it.
///
/// `listen` forwards what [`FakeConnector::deliver`] is given until it is
/// stopped, which is all a real connector's `listen` is; tests that drive the
/// relay directly skip it and push into the inbox themselves.
pub struct FakeConnector {
    events: Mutex<Vec<Event>>,
    serial: AtomicUsize,
    send_attempts: AtomicUsize,
    sends_to_fail: Mutex<usize>,
    deliveries: Mutex<Option<mpsc::UnboundedReceiver<Incoming>>>,
    delivering: mpsc::UnboundedSender<Incoming>,
    listen_error: Mutex<Option<ChatError>>,
    home: Mutex<Option<ChatId>>,
}

impl FakeConnector {
    pub fn new() -> Arc<Self> {
        let (delivering, deliveries) = mpsc::unbounded_channel();
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
            serial: AtomicUsize::new(0),
            send_attempts: AtomicUsize::new(0),
            sends_to_fail: Mutex::new(0),
            deliveries: Mutex::new(Some(deliveries)),
            delivering,
            listen_error: Mutex::new(None),
            home: Mutex::new(None),
        })
    }

    /// Say which chat is the owner's private one, for a routine to reach.
    /// Until this is called there is none, as for an owner who has not
    /// joined yet.
    pub fn set_home(&self, chat: ChatId) {
        *self.home.lock().unwrap() = Some(chat);
    }

    /// Hand the connector something the owner "did", for `listen` to pass on.
    pub fn deliver(&self, incoming: Incoming) {
        self.delivering.send(incoming).expect("the fake connector is still listening");
    }

    /// Make the next `n` sends fail, as a server that is briefly unreachable
    /// would.
    pub fn fail_next_sends(&self, n: usize) {
        *self.sends_to_fail.lock().unwrap() = n;
    }

    /// Make `listen` fail at once, as a connection that dropped would.
    pub fn fail_listening_with(&self, error: ChatError) {
        *self.listen_error.lock().unwrap() = Some(error);
    }

    pub fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    /// Every message that went out, in order. A question is not one.
    pub fn sent(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                Event::Sent { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    /// Every question that was put, with the message it went out as.
    pub fn questions(&self) -> Vec<(Question, MessageRef)> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                Event::Asked { question, reference, .. } => Some((question, reference)),
                _ => None,
            })
            .collect()
    }

    /// The typing notice, on and off, in order.
    pub fn typing(&self) -> Vec<bool> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                Event::Typing { on, .. } => Some(on),
                _ => None,
            })
            .collect()
    }

    /// How many sends were tried, whether or not they got through.
    pub fn send_attempts(&self) -> usize {
        self.send_attempts.load(Ordering::SeqCst)
    }

    /// Wait until `done` is true of everything recorded so far. Fails the test
    /// rather than hanging it, with what was seen.
    pub async fn until(&self, what: &str, done: impl Fn(&[Event]) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if done(&self.events()) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}; saw {:#?}",
                self.events()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Wait for the `n`th message to go out.
    pub async fn wait_for_sent(&self, n: usize) {
        self.until(&format!("{n} message(s) to be sent"), |events| {
            events.iter().filter(|e| matches!(e, Event::Sent { .. })).count() >= n
        })
        .await;
    }

    /// Wait for a question to be put, and return the first one.
    pub async fn wait_for_question(&self) -> (Question, MessageRef) {
        self.until("a question to be asked", |events| {
            events.iter().any(|e| matches!(e, Event::Asked { .. }))
        })
        .await;
        self.questions().remove(0)
    }

    fn next_reference(&self, prefix: &str) -> MessageRef {
        MessageRef(format!("{prefix}{}", self.serial.fetch_add(1, Ordering::SeqCst)))
    }

    fn record(&self, event: Event) {
        self.events.lock().unwrap().push(event);
    }
}

/// Resolve once `stop` has been flipped, and never otherwise.
async fn stopped(stop: &mut watch::Receiver<bool>) {
    if stop.wait_for(|stop| *stop).await.is_err() {
        std::future::pending::<()>().await;
    }
}

#[async_trait]
impl Connector for FakeConnector {
    async fn listen(
        &self,
        inbox: mpsc::Sender<Incoming>,
        mut stop: watch::Receiver<bool>,
    ) -> Result<(), ChatError> {
        let failure = self.listen_error.lock().unwrap().take();
        if let Some(error) = failure {
            return Err(error);
        }
        let mut deliveries =
            self.deliveries.lock().unwrap().take().expect("a fake connector is listened to once");
        loop {
            tokio::select! {
                () = stopped(&mut stop) => return Ok(()),
                item = deliveries.recv() => match item {
                    Some(item) => {
                        if inbox.send(item).await.is_err() {
                            return Ok(());
                        }
                    }
                    None => return Ok(()),
                },
            }
        }
    }

    async fn send(&self, chat: &ChatId, markdown: &str) -> Result<MessageRef, ChatError> {
        self.send_attempts.fetch_add(1, Ordering::SeqCst);
        let fails = {
            let mut left = self.sends_to_fail.lock().unwrap();
            let fails = *left > 0;
            *left = left.saturating_sub(1);
            fails
        };
        if fails {
            return Err(ChatError::Transient("the server is unreachable".into()));
        }
        self.record(Event::Sent { chat: chat.clone(), text: markdown.to_string() });
        Ok(self.next_reference("$message"))
    }

    async fn ask(&self, chat: &ChatId, question: &Question) -> Result<MessageRef, ChatError> {
        let reference = self.next_reference("$question");
        self.record(Event::Asked {
            chat: chat.clone(),
            question: question.clone(),
            reference: reference.clone(),
        });
        Ok(reference)
    }

    async fn home(&self) -> Option<ChatId> {
        self.home.lock().unwrap().clone()
    }

    async fn typing(&self, chat: &ChatId, on: bool) -> Result<(), ChatError> {
        self.record(Event::Typing { chat: chat.clone(), on });
        Ok(())
    }
}

// ---- a model that says what it is told --------------------------------------

/// What the fake model does when it is asked.
#[derive(Clone)]
pub enum Script {
    /// One reply, no tools.
    Says(String),
    /// Ask for a tool by name with these arguments, then say something.
    Calls { tool: String, arguments: String, then: String },
    /// Accept the connection and never answer. What a wedged provider is, and
    /// what a turn somebody wants to stop looks like from outside.
    Hangs,
}

pub fn says(text: &str) -> Script {
    Script::Says(text.to_string())
}

pub fn calls(tool: &str, arguments: String, then: &str) -> Script {
    Script::Calls { tool: tool.to_string(), arguments, then: then.to_string() }
}

/// Ask to delete `task`, then say `then` -- whatever became of the request.
pub fn deleting(task: &Task, then: &str) -> Script {
    calls("delete_task", serde_json::json!({ "task_id": task.id.to_string() }).to_string(), then)
}

/// A model on loopback. Dropping the handle stops it.
pub struct FakeModel {
    pub endpoint: String,
    _shutdown: watch::Sender<bool>,
}

/// The model every test in `everyday-service/tests/routines.rs` talks to: a
/// `TcpListener` speaking the Chat Completions shape, which is all the agent
/// harness needs. No key, no network, no bill.
pub async fn fake_model(script: Script) -> FakeModel {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = watch::channel(false);

    tokio::spawn(async move {
        // One reply per connection, and the tool answer is the *second* one:
        // rig asks again with the tool's result, and a model that repeated its
        // request would loop until `max_steps`.
        let mut asked = 0usize;
        loop {
            let accepted = tokio::select! {
                r = listener.accept() => r,
                _ = rx.changed() => break,
            };
            let Ok((mut socket, _)) = accepted else { break };
            let script = script.clone();
            asked += 1;
            let turn = asked;
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 64 * 1024];
                let _ = socket.read(&mut buf).await;
                if matches!(script, Script::Hangs) {
                    // Held open, saying nothing, until the test gives up on it.
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    return;
                }
                let body = match &script {
                    Script::Says(text) => reply(text, None),
                    Script::Calls { tool, arguments, then } => {
                        if turn == 1 {
                            reply("", Some((tool, arguments)))
                        } else {
                            reply(then, None)
                        }
                    }
                    Script::Hangs => unreachable!("handled above"),
                };
                // Server-sent events, not a JSON document: the harness
                // streams every turn, so a plain body is refused before it is
                // read. See `reply`.
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });

    FakeModel { endpoint: format!("http://127.0.0.1:{port}/v1"), _shutdown: tx }
}

/// One answer, as a stream of Chat Completions chunks.
///
/// The harness streams every turn, so this has to be `text/event-stream` with
/// `data:` frames rather than one JSON document -- a plain body is refused on
/// its content type before anything reads it.
fn reply(text: &str, call: Option<(&str, &str)>) -> String {
    let mut out = String::new();
    let mut frame = |delta: serde_json::Value, finish: Option<&str>| {
        let chunk = serde_json::json!({
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
                serde_json::json!({
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
            frame(serde_json::json!({}), Some("tool_calls"));
        }
        None => {
            frame(serde_json::json!({ "role": "assistant", "content": text }), None);
            frame(serde_json::json!({}), Some("stop"));
        }
    }
    out.push_str("data: [DONE]\n\n");
    out
}

// ---- a vault and a service to run a turn against ----------------------------

/// A vault in a temporary directory with a journal and an assistant pointed at
/// `endpoint` -- named, so that `!help` has a name to use -- inside a
/// [`Service`].
pub struct World {
    pub service: Arc<Service>,
    pub vault: Arc<Vault>,
    pub dir: tempfile::TempDir,
}

pub fn world(endpoint: &str) -> World {
    let dir = tempfile::tempdir().unwrap();
    let config = everyday_core::VaultConfig {
        name: "Test".into(),
        backend: "sqlite".into(),
        settings: Default::default(),
        password: None,
        kdf: Default::default(),
        auto_lock_seconds: 900,
        forget_key_seconds: 0,
    };
    let vault = everyday_vault::create(dir.path(), config).unwrap();
    vault.save_journal(&everyday_core::Journal::new("Journal")).unwrap();

    let settings = everyday_core::AgentSettings {
        enabled: true,
        name: "Juniper".into(),
        // Everything is reckoned in UTC so the tests are about the relay
        // rather than about where the machine running them is.
        timezone: Some("UTC".into()),
        ..Default::default()
    };
    vault.save_agent_settings(&settings).unwrap();
    let mut models = everyday_core::llm::ModelSettings::default();
    models.medium.connection.base_url = Some(endpoint.to_string());
    models.medium.model.model = "scripted".into();
    vault.save_model_settings(&models).unwrap();

    let service = Arc::new(Service::new());
    let vault = service.set(vault);
    World { service, vault, dir }
}

/// The chat every test talks in.
pub fn room() -> ChatId {
    ChatId("!room:test".into())
}

/// A switched-on Matrix link with valid ids.
pub fn matrix_link() -> ChatLink {
    let mut link = ChatLink::new_matrix();
    link.service = ChatService::Matrix(MatrixLink {
        homeserver: "http://127.0.0.1:6167".into(),
        user_id: "@bot:test".into(),
        owner: "@me:test".into(),
    });
    link
}

// ---- the relay, running ------------------------------------------------------

/// A relay running against a fake connector, a fake model and a vault with
/// one task in it, with an inbox to push into.
pub struct Harness {
    pub service: Arc<Service>,
    pub vault: Arc<Vault>,
    pub link: ChatLink,
    pub connector: Arc<FakeConnector>,
    /// The task a script from [`deleting`] asks to delete.
    pub task: Task,
    inbox: Option<mpsc::Sender<Incoming>>,
    stop: watch::Sender<bool>,
    relay: Option<JoinHandle<()>>,
    _dir: tempfile::TempDir,
    _model: FakeModel,
}

impl Harness {
    /// Start a relay whose model follows the script `script` returns for the
    /// vault's one task.
    pub async fn start(script: impl FnOnce(&Task) -> Script, timing: Timing) -> Self {
        let World { service, vault, dir } = world("http://127.0.0.1:1/v1");

        // The task has to exist before the script can name it, and the script
        // before the model can be pointed at.
        let task = Task::new("Book the dentist");
        vault.save_task(&task).unwrap();
        let model = fake_model(script(&task)).await;
        let mut models = vault.model_settings().unwrap();
        models.medium.connection.base_url = Some(model.endpoint.clone());
        vault.save_model_settings(&models).unwrap();

        let link = matrix_link();
        let connector = FakeConnector::new();
        let (inbox, inbox_rx) = mpsc::channel(32);
        let (stop, stop_rx) = watch::channel(false);
        let relay = tokio::spawn(relay::run_with(
            service.clone(),
            vault.clone(),
            link.clone(),
            connector.clone(),
            inbox_rx,
            stop_rx,
            timing,
        ));

        Self {
            service,
            vault,
            link,
            connector,
            task,
            inbox: Some(inbox),
            stop,
            relay: Some(relay),
            _dir: dir,
            _model: model,
        }
    }

    /// The owner types `text`, now.
    pub async fn say(&self, text: &str) {
        self.say_at(text, Timestamp::now()).await;
    }

    /// The owner typed `text` at `at`, and the bot only now hears of it.
    pub async fn say_at(&self, text: &str, at: Timestamp) {
        self.deliver(Incoming::Said { chat: room(), text: text.into(), at }).await;
    }

    /// The owner taps `choice` on the message `question`.
    pub async fn choose(&self, question: &MessageRef, choice: Choice) {
        self.deliver(Incoming::Chose {
            chat: room(),
            question: question.clone(),
            choice,
            at: Timestamp::now(),
        })
        .await;
    }

    pub async fn deliver(&self, incoming: Incoming) {
        self.inbox
            .as_ref()
            .expect("the inbox is still open")
            .send(incoming)
            .await
            .expect("the relay is still reading its inbox");
    }

    /// Close the inbox and wait for the relay to finish what it has in hand --
    /// the point after which nothing more will happen, so a test can say what
    /// did not.
    pub async fn finish(&mut self) {
        drop(self.inbox.take());
        self.join().await;
    }

    /// Stop the relay, the way the supervisor does, and wait for it to end.
    pub async fn halt(&mut self) {
        self.stop.send_replace(true);
        self.join().await;
    }

    async fn join(&mut self) {
        if let Some(relay) = self.relay.take() {
            tokio::time::timeout(Duration::from_secs(30), relay)
                .await
                .expect("the relay ends")
                .expect("the relay does not panic");
        }
    }

    /// Every conversation in the vault, newest first.
    pub fn conversations(&self) -> Vec<Conversation> {
        self.vault.conversations(&ConversationQuery::default()).unwrap()
    }
}
