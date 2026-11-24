//! Typing from a spoken conversation, end to end against a fake model: a
//! turn at this computer's desktop is offered `type_text` and
//! `start_dictation` and told what they are for, a call to the second says so
//! to the session, and no other kind of turn hears of either.
//!
//! The harness is `tests/voice_end.rs`'s: a `TcpListener` on loopback speaking
//! the Chat Completions shape, one scripted answer per request.

use futures::future::BoxFuture;
use karmayoga_core::ConversationId;
use karmayoga_service::Service;
use karmayoga_service::agent::{Reach, Sink, Turn, Typist};
use karmayoga_service::voice::assistant::ServiceAssistant;
use karmayoga_service::voice::{DICTATE_TOOL, END_TOOL, VoiceAssistant};
use std::sync::{Arc, Mutex};

/// A vault with an assistant pointed at `endpoint`.
fn service(endpoint: &str) -> (Arc<Service>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = karmayoga_core::VaultConfig {
        name: "Test".into(),
        backend: "sqlite".into(),
        settings: Default::default(),
        password: None,
        kdf: Default::default(),
        auto_lock_seconds: 900,
        forget_key_seconds: 0,
    };
    let vault = karmayoga_vault::create(dir.path(), config).unwrap();
    let settings = karmayoga_core::AgentSettings {
        enabled: true,
        timezone: Some("UTC".into()),
        ..Default::default()
    };
    vault.save_agent_settings(&settings).unwrap();
    let mut models = karmayoga_core::llm::ModelSettings::default();
    models.medium.connection.base_url = Some(endpoint.to_string());
    models.medium.model.model = "scripted".into();
    vault.save_model_settings(&models).unwrap();

    let svc = Arc::new(Service::new());
    svc.set(vault);
    (svc, dir)
}

/// Tool calls answered in turn -- the tool's name and its arguments as JSON
/// -- then a final message with no more calls.
#[derive(Clone)]
struct Script {
    calls: Vec<(String, String)>,
    then: String,
}

/// A model on loopback. Dropping the handle stops it. `requests` collects each
/// call's JSON body, in order.
struct FakeModel {
    endpoint: String,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    _shutdown: tokio::sync::watch::Sender<bool>,
}

async fn fake_model(script: Script) -> FakeModel {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let requests: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();

    tokio::spawn({
        let requests = requests.clone();
        async move {
            let mut asked = 0usize;
            loop {
                let accepted = tokio::select! {
                    r = listener.accept() => r,
                    _ = rx.changed() => break,
                };
                let Ok((mut socket, _)) = accepted else { break };
                let script = script.clone();
                let requests = requests.clone();
                asked += 1;
                let turn = asked;
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf: Vec<u8> = Vec::new();
                    let mut chunk = vec![0u8; 64 * 1024];
                    loop {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(body_at) = find_body(&buf)
                            && buf.len() - body_at >= content_length(&buf[..body_at])
                        {
                            break;
                        }
                    }
                    if let Some(body_at) = find_body(&buf)
                        && let Ok(json) =
                            serde_json::from_slice::<serde_json::Value>(&buf[body_at..])
                    {
                        requests.lock().unwrap().push(json);
                    }
                    let body = match script.calls.get(turn - 1) {
                        Some((tool, arguments)) => reply("", Some((tool, arguments))),
                        None => reply(&script.then, None),
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        }
    });

    FakeModel { endpoint: format!("http://127.0.0.1:{port}/v1"), requests, _shutdown: tx }
}

fn content_length(head: &[u8]) -> usize {
    String::from_utf8_lossy(head)
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim().eq_ignore_ascii_case("content-length").then(|| value.trim().parse().ok())?
        })
        .unwrap_or(0)
}

fn find_body(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// One answer, as a stream of Chat Completions chunks: a call to a tool with
/// its arguments, or `text`.
fn reply(text: &str, tool: Option<(&str, &str)>) -> String {
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
    match tool {
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

fn offered(request: &serde_json::Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn quiet() -> Sink {
    Arc::new(|_| {})
}

/// What types where the person was typing, remembering what it was given.
struct Keyboard {
    typed: Mutex<Vec<String>>,
}

impl Typist for Keyboard {
    fn type_text(&self, text: String) -> BoxFuture<'static, Result<(), String>> {
        self.typed.lock().unwrap().push(text);
        Box::pin(async { Ok(()) })
    }
}

fn keyboard() -> Arc<Keyboard> {
    Arc::new(Keyboard { typed: Mutex::new(Vec::new()) })
}

/// The messages of one request to the model, as one string to search.
fn said_to_the_model(request: &serde_json::Value) -> String {
    request["messages"].to_string()
}

/// Said at the start of the typing note, which only a turn that can type is
/// given.
const TYPING_NOTE_OPENS: &str = "wherever they are on their desktop";

fn assistant(svc: &Arc<Service>, typist: Option<Arc<Keyboard>>) -> ServiceAssistant {
    ServiceAssistant {
        service: svc.clone(),
        conversation: ConversationId::new(),
        via: None,
        typist: typist.map(|k| k as Arc<dyn Typist>),
        woken: false,
    }
}

#[tokio::test]
async fn a_spoken_turn_with_a_typist_is_offered_dictation_and_told_what_typing_is_for() {
    let model = fake_model(Script {
        calls: vec![(DICTATE_TOOL.to_string(), "{}".to_string())],
        then: "Go ahead.".into(),
    })
    .await;
    let (svc, _dir) = service(&model.endpoint);
    let keyboard = keyboard();
    let assistant = assistant(&svc, Some(keyboard.clone()));

    let outcome =
        assistant.turn("Let me dictate a reply.".into(), true, None, quiet()).await.unwrap();

    assert!(outcome.dictate, "the session is told to type what is said next");
    assert!(!outcome.ended_voice, "dictating is not ending");
    assert!(!outcome.stopped);
    assert!(keyboard.typed.lock().unwrap().is_empty(), "handing over types nothing itself");
    let requests = model.requests.lock().unwrap();
    let tools = offered(&requests[0]);
    assert!(tools.contains(&DICTATE_TOOL.to_string()), "{tools:?}");
    assert!(tools.contains(&"type_text".to_string()), "offered under the same conditions");
    assert!(tools.contains(&END_TOOL.to_string()));
    assert!(
        said_to_the_model(&requests[0]).contains(TYPING_NOTE_OPENS),
        "the turn says the person is typing on their desktop"
    );
}

#[tokio::test]
async fn a_spoken_turn_that_asks_to_type_types_where_they_were_typing() {
    // The model's answer to "Jimmy, type that I'll be late": the turn must
    // reach the typist -- it once did not, a reset of the turn's reach having
    // taken it away before the tools were chosen.
    let model = fake_model(Script {
        calls: vec![("type_text".to_string(), r#"{"text":"I'll be late."}"#.to_string())],
        then: "Typed.".into(),
    })
    .await;
    let (svc, _dir) = service(&model.endpoint);
    let keyboard = keyboard();
    let assistant = assistant(&svc, Some(keyboard.clone()));

    let outcome =
        assistant.turn("Type that I'll be late.".into(), true, None, quiet()).await.unwrap();

    assert_eq!(*keyboard.typed.lock().unwrap(), ["I'll be late."]);
    assert!(!outcome.dictate, "one line typed is not a dictation");
}

#[tokio::test]
async fn a_spoken_turn_with_no_typist_is_offered_neither_and_told_nothing_of_typing() {
    let model = fake_model(Script { calls: Vec::new(), then: "It is sunny.".into() }).await;
    let (svc, _dir) = service(&model.endpoint);
    let assistant = assistant(&svc, None);

    let outcome =
        assistant.turn("Type that I'll be late.".into(), true, None, quiet()).await.unwrap();

    assert!(!outcome.dictate);
    let requests = model.requests.lock().unwrap();
    let tools = offered(&requests[0]);
    assert!(!tools.contains(&DICTATE_TOOL.to_string()), "nothing to type with: {tools:?}");
    assert!(!tools.contains(&"type_text".to_string()));
    assert!(!said_to_the_model(&requests[0]).contains(TYPING_NOTE_OPENS));
}

#[tokio::test]
async fn a_typed_turn_is_not_offered_dictation_even_with_a_typist_about() {
    let model = fake_model(Script { calls: Vec::new(), then: "Hello.".into() }).await;
    let (svc, _dir) = service(&model.endpoint);
    let turned = karmayoga_service::agent::run_turn(Turn {
        service: svc.clone(),
        vault: svc.get().unwrap(),
        pending: svc.pending(),
        conversation: ConversationId::new(),
        prompt: "Let me dictate a reply.".into(),
        pictures: Vec::new(),
        files: Vec::new(),
        on_screen: None,
        channel: quiet(),
        unattended: None,
        drafting: None,
        via: None,
        spoken: false,
        voice_approves: false,
        woken: false,
        reach: Reach { typist: Some(keyboard()), ..Reach::default() },
        job: None,
        follow_up: None,
        about_job: None,
        max_steps: None,
        tier: None,
        read_only: false,
    })
    .await
    .unwrap();

    assert!(!turned.dictate);
    let requests = model.requests.lock().unwrap();
    let tools = offered(&requests[0]);
    assert!(!tools.contains(&DICTATE_TOOL.to_string()), "typing has no voice to hand over");
    assert!(!tools.contains(&"type_text".to_string()));
    assert!(!said_to_the_model(&requests[0]).contains(TYPING_NOTE_OPENS));
}

#[tokio::test]
async fn a_call_through_a_chat_is_not_offered_dictation() {
    // On a call there is no desktop: the session is given no typist, and a
    // turn that somehow had one would still not be offered it.
    let model = fake_model(Script { calls: Vec::new(), then: "Hello.".into() }).await;
    let (svc, _dir) = service(&model.endpoint);
    let keyboard = keyboard();
    let via = karmayoga_core::chat::Via {
        link: karmayoga_core::id::ChatLinkId::new(),
        service: "Matrix".into(),
        chat: "!room:example.org".into(),
    };
    let assistant = ServiceAssistant {
        service: svc.clone(),
        conversation: ConversationId::new(),
        via: Some(via),
        typist: Some(keyboard as Arc<dyn Typist>),
        woken: false,
    };

    let outcome =
        assistant.turn("Let me dictate a reply.".into(), true, None, quiet()).await.unwrap();

    assert!(!outcome.dictate);
    let requests = model.requests.lock().unwrap();
    let tools = offered(&requests[0]);
    assert!(!tools.contains(&DICTATE_TOOL.to_string()), "{tools:?}");
    assert!(!tools.contains(&"type_text".to_string()));
}
