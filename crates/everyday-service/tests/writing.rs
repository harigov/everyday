//! Writing help's commands, end to end against a fake model on loopback:
//! what the model is told, what comes back, and that every use is kept --
//! the ones that fail included.

#[allow(dead_code)]
mod support;

use std::sync::{Arc, Mutex};

use everyday_core::WritingQuery;
use everyday_core::llm::ModelSettings;
use everyday_core::writing::{WritingAction, WritingAssist, WritingOutcome};
use everyday_service::Service;
use everyday_service::ctx::Ctx;
use serde_json::{Value, json};

// ---- a fake model on loopback ---------------------------------------------

/// A model that answers every request with a `submit` tool call carrying
/// `{"text": answer}`, and keeps the body of each request it was sent.
struct FakeModel {
    endpoint: String,
    requests: Arc<Mutex<Vec<String>>>,
    _shutdown: tokio::sync::watch::Sender<bool>,
}

async fn fake_model(answer: &str) -> FakeModel {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let arguments = Arc::new(json!({ "text": answer }).to_string());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();

    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                r = listener.accept() => r,
                _ = rx.changed() => break,
            };
            let Ok((mut socket, _)) = accepted else { break };
            let (arguments, seen) = (arguments.clone(), seen.clone());
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let request = read_request(&mut socket).await;
                seen.lock().unwrap().push(request);
                let body = json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "created": 0,
                    "model": "scripted",
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "call_1",
                                "type": "function",
                                "function": { "name": "submit", "arguments": *arguments },
                            }],
                        },
                        "finish_reason": "tool_calls",
                    }],
                    "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 },
                })
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });

    FakeModel { endpoint: format!("http://127.0.0.1:{port}/v1"), requests, _shutdown: tx }
}

/// The whole of one HTTP request: headers, then as many body bytes as its
/// `Content-Length` says.
async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let n = socket.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf);
        if let Some(end) = text.find("\r\n\r\n") {
            let length = text[..end]
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length").then(|| v.trim().parse().ok())?
                })
                .unwrap_or(0usize);
            if buf.len() >= end + 4 + length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

// ---- fixtures ---------------------------------------------------------

fn env(endpoint: &str) -> (Arc<Service>, tempfile::TempDir) {
    let (svc, dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let mut models = ModelSettings::default();
    models.medium.connection.base_url = Some(endpoint.to_string());
    models.medium.model.model = "scripted-medium".into();
    vault.save_model_settings(&models).unwrap();
    (svc, dir)
}

async fn call(svc: &Arc<Service>, name: &str, args: Value) -> Value {
    svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"))
}

async fn history(svc: &Arc<Service>) -> Vec<WritingAssist> {
    serde_json::from_value(call(svc, "list_writing_assists", json!({})).await).unwrap()
}

// ---- tests ------------------------------------------------------------

#[tokio::test]
async fn a_selection_is_rewritten_on_the_medium_model_and_kept() {
    let model = fake_model("They're going home tomorrow.").await;
    let (svc, _dir) = env(&model.endpoint);

    let assist: WritingAssist = serde_json::from_value(
        call(
            &svc,
            "rewrite_selection",
            json!({ "text": "their going home tomorow\n", "action": "grammar", "app": "gedit" }),
        )
        .await,
    )
    .unwrap();

    // The selection's own trailing line break is put back, so pasting the
    // answer over it does not join two paragraphs.
    assert_eq!(assist.result, "They're going home tomorrow.\n");
    assert_eq!(assist.outcome, WritingOutcome::Pending);
    assert_eq!(assist.model, "scripted-medium");
    assert_eq!(assist.app.as_deref(), Some("gedit"));

    // What the model was told: the grammar instruction, and the words --
    // without the whitespace around them.
    let requests = model.requests.lock().unwrap().clone();
    // The first request is the ask; `quick::run_prompt` may hand the tool's
    // result back afterwards, which is a second request of its own.
    assert!(!requests.is_empty(), "the model was asked");
    let sent = &requests[0];
    assert!(sent.contains("scripted-medium"), "aimed at the medium model: {sent}");
    assert!(sent.contains("fix spelling, grammar"), "the grammar instruction: {sent}");
    assert!(sent.contains("their going home tomorow"), "the selection: {sent}");

    let kept = history(&svc).await;
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0], assist, "the record handed back is the one kept");
}

#[tokio::test]
async fn an_answer_is_settled_once_and_the_history_says_how() {
    let model = fake_model("Shorter.").await;
    let (svc, _dir) = env(&model.endpoint);

    let assist: WritingAssist = serde_json::from_value(
        call(
            &svc,
            "rewrite_selection",
            json!({ "text": "A much longer way of saying it.", "action": "concise" }),
        )
        .await,
    )
    .unwrap();
    let settled: WritingAssist = serde_json::from_value(
        call(&svc, "settle_writing_assist", json!({ "id": assist.id, "outcome": "replaced" }))
            .await,
    )
    .unwrap();
    assert_eq!(settled.outcome, WritingOutcome::Replaced);

    // Settling it differently afterwards is refused, and changes nothing.
    let refused = svc
        .call(
            Ctx::local(),
            "settle_writing_assist",
            json!({ "id": assist.id, "outcome": "cancelled" }),
        )
        .await;
    assert!(refused.is_err());

    let kept: Vec<WritingAssist> = serde_json::from_value(
        call(&svc, "list_writing_assists", json!({ "query": { "outcomes": ["replaced"] } })).await,
    )
    .unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].action, WritingAction::Concise);
    assert_eq!(kept[0].result, "Shorter.");
}

#[tokio::test]
async fn a_rewrite_that_fails_is_reported_and_still_kept() {
    // Nothing listens here: bind a port, then let it go.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let (svc, _dir) = env(&format!("http://127.0.0.1:{port}/v1"));

    let failed = svc
        .call(
            Ctx::local(),
            "rewrite_selection",
            json!({ "text": "Improve me.", "action": "improve" }),
        )
        .await;
    assert!(failed.is_err(), "a model that cannot be reached is an error the popup shows");

    let kept = history(&svc).await;
    assert_eq!(kept.len(), 1, "the failure is kept too");
    assert_eq!(kept[0].outcome, WritingOutcome::Failed);
    assert_eq!(kept[0].original, "Improve me.");
    assert_eq!(kept[0].model, "scripted-medium");
    assert!(kept[0].error.as_deref().is_some_and(|e| !e.is_empty()));
}

#[tokio::test]
async fn a_blank_selection_asks_nobody_and_keeps_nothing() {
    let model = fake_model("unused").await;
    let (svc, _dir) = env(&model.endpoint);

    let refused = svc
        .call(Ctx::local(), "rewrite_selection", json!({ "text": "  \n ", "action": "improve" }))
        .await
        .expect_err("nothing selected");
    assert!(refused.message.contains("select some text"), "{}", refused.message);
    assert!(model.requests.lock().unwrap().is_empty());
    assert!(history(&svc).await.is_empty());
    assert!(
        svc.get().unwrap().writing_assists(&WritingQuery::default()).unwrap().is_empty(),
        "and nothing was written behind the command's back"
    );
}
