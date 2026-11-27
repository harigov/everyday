//! Ending a spoken conversation out loud, end to end against a fake model:
//! the tool is offered to a spoken turn and to no other, and a turn that calls
//! it says so to the session.
//!
//! The harness is `tests/dream.rs`'s: a `TcpListener` on loopback speaking the
//! Chat Completions shape, one scripted answer per request.

use jatayu_core::ConversationId;
use jatayu_service::Service;
use jatayu_service::agent::{Reach, Sink, Turn};
use jatayu_service::voice::assistant::ServiceAssistant;
use jatayu_service::voice::{END_TOOL, VoiceAssistant};
use std::sync::{Arc, Mutex};

/// A vault with an assistant pointed at `endpoint`.
fn service(endpoint: &str) -> (Arc<Service>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = jatayu_core::VaultConfig {
        name: "Test".into(),
        backend: "sqlite".into(),
        settings: Default::default(),
        password: None,
        kdf: Default::default(),
        auto_lock_seconds: 900,
        forget_key_seconds: 0,
    };
    let vault = jatayu_vault::create(dir.path(), config).unwrap();
    let settings = jatayu_core::AgentSettings {
        enabled: true,
        timezone: Some("UTC".into()),
        ..Default::default()
    };
    vault.save_agent_settings(&settings).unwrap();
    let mut models = jatayu_core::llm::ModelSettings::default();
    models.medium.connection.base_url = Some(endpoint.to_string());
    models.medium.model.model = "scripted".into();
    vault.save_model_settings(&models).unwrap();

    let svc = Arc::new(Service::new());
    svc.set(vault);
    (svc, dir)
}

/// Tool calls answered in turn, then a final message with no more calls.
#[derive(Clone)]
struct Script {
    calls: Vec<String>,
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
                        Some(tool) => reply("", Some(tool)),
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

/// One answer, as a stream of Chat Completions chunks: a call to `tool` with
/// no arguments, or `text`.
fn reply(text: &str, tool: Option<&str>) -> String {
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
        Some(tool) => {
            frame(
                serde_json::json!({
                    "role": "assistant",
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": tool, "arguments": "{}" },
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

#[tokio::test]
async fn a_spoken_turn_that_says_goodbye_ends_the_conversation_out_loud() {
    let model =
        fake_model(Script { calls: vec![END_TOOL.to_string()], then: "Talk soon.".into() }).await;
    let (svc, _dir) = service(&model.endpoint);
    let conversation = ConversationId::new();
    let assistant = ServiceAssistant {
        service: svc.clone(),
        conversation,
        via: None,
        typist: None,
        woken: false,
        away: false,
    };

    let outcome = assistant.turn("That's all I have.".into(), true, None, quiet()).await.unwrap();

    assert!(outcome.ended_voice, "the session is told to end once the goodbye is heard");
    assert!(!outcome.stopped);
    let requests = model.requests.lock().unwrap();
    assert!(offered(&requests[0]).contains(&END_TOOL.to_string()), "a spoken turn is offered it");
    // The thread stays, with the goodbye in it.
    let vault = svc.get().unwrap();
    let messages = vault.messages(conversation).unwrap();
    assert!(messages.iter().any(|m| m.content.contains("Talk soon.")), "{messages:?}");
}

#[tokio::test]
async fn a_spoken_turn_that_carries_on_does_not() {
    let model = fake_model(Script { calls: Vec::new(), then: "It is sunny.".into() }).await;
    let (svc, _dir) = service(&model.endpoint);
    let assistant = ServiceAssistant {
        service: svc.clone(),
        conversation: ConversationId::new(),
        via: None,
        typist: None,
        woken: false,
        away: false,
    };

    let outcome = assistant.turn("What is the weather?".into(), true, None, quiet()).await.unwrap();

    assert!(!outcome.ended_voice);
}

#[tokio::test]
async fn a_typed_turn_is_not_offered_it() {
    let model = fake_model(Script { calls: Vec::new(), then: "Hello.".into() }).await;
    let (svc, _dir) = service(&model.endpoint);
    let turned = jatayu_service::agent::run_turn(Turn {
        service: svc.clone(),
        vault: svc.get().unwrap(),
        pending: svc.pending(),
        conversation: ConversationId::new(),
        prompt: "That's all I have.".into(),
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
        reach: Reach::default(),
        job: None,
        follow_up: None,
        about_job: None,
        max_steps: None,
        tier: None,
        read_only: false,
    })
    .await
    .unwrap();

    assert!(!turned.ended_voice);
    let requests = model.requests.lock().unwrap();
    assert!(!offered(&requests[0]).contains(&END_TOOL.to_string()), "typing has no voice to end");
}
