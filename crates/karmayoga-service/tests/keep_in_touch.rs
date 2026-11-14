//! Keeping in touch end to end: the command, the journal read by a fake model on
//! loopback, and what lands on the shelf. The arithmetic and the asking are
//! under test in `karmayoga-core` and `karmayoga-vault`; this is the wiring.

#[allow(dead_code)]
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use karmayoga_core::library::{CONTACT_SLUG, Item, Via};
use karmayoga_core::llm::{LLMConfig, LLMModelConfig, ModelSettings};
use karmayoga_core::store::library::LogQuery;
use karmayoga_service::{Caller, Ctx, Scope, Service};
use serde_json::{Value, json};

struct FakeModel {
    endpoint: String,
    _shutdown: tokio::sync::watch::Sender<bool>,
    calls: Arc<AtomicU32>,
}

/// A model that answers every request with one `submit` call carrying
/// `arguments`, or with a server error if `arguments` is null.
async fn fake_model(arguments: Value) -> FakeModel {
    let failing = arguments.is_null();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let body = Arc::new(
        json!({
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
                        "function": { "name": "submit", "arguments": arguments.to_string() },
                    }],
                },
                "finish_reason": "tool_calls",
            }],
            "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 },
        })
        .to_string(),
    );
    let calls = Arc::new(AtomicU32::new(0));
    let counted = calls.clone();
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                r = listener.accept() => r,
                _ = rx.changed() => break,
            };
            let Ok((mut socket, _)) = accepted else { break };
            counted.fetch_add(1, Ordering::SeqCst);
            let body = body.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 64 * 1024];
                let _ = socket.read(&mut buf).await;
                if failing {
                    let refusal = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = socket.write_all(refusal.as_bytes()).await;
                    return;
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });
    FakeModel { endpoint: format!("http://127.0.0.1:{port}/v1"), _shutdown: tx, calls }
}

/// A service with the assistant on, both models pointed at `endpoint`, keeping
/// in touch switched on, Ravi Kumar on the shelf, and today's entry saying they
/// talked.
fn env(endpoint: &str, assistant_on: bool) -> (Arc<Service>, tempfile::TempDir, Item) {
    let (svc, dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let settings = karmayoga_core::AgentSettings { enabled: assistant_on, ..Default::default() };
    vault.save_agent_settings(&settings).unwrap();
    let mut models = ModelSettings::default();
    models.medium.connection.base_url = Some(endpoint.to_string());
    models.medium.model.model = "scripted".into();
    models.small = Some(LLMConfig {
        connection: models.medium.connection.clone(),
        model: LLMModelConfig { model: "scripted".into(), ..LLMModelConfig::quick() },
    });
    vault.save_model_settings(&models).unwrap();

    vault.seed_library().unwrap();
    vault.update_keep_in_touch(|k| k.settings.enabled = true).unwrap();
    let shelf = vault.kinds().unwrap().into_iter().find(|k| k.slug == CONTACT_SLUG).unwrap();
    let mut ravi = Item::new(shelf.id, "Ravi Kumar");
    ravi.facts.insert("circle".into(), "Close".into());
    vault.save_item(&ravi).unwrap();

    let tz = vault.person_tz();
    let today = svc.now().to_zoned(jiff::tz::TimeZone::get(&tz).unwrap()).date();
    let journal = vault.journals().unwrap().remove(0);
    let mut entry = karmayoga_core::Entry::new(journal.id, &tz);
    entry.local_date = today;
    entry.body = karmayoga_core::richtext::RichDoc::from_plain_text("Called Ravi about the move.");
    vault.save_entry(&entry, None).unwrap();
    (svc, dir, ravi)
}

async fn call(svc: &Arc<Service>, name: &str) -> Value {
    svc.call(Ctx::local(), name, json!({})).await.unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[tokio::test]
async fn a_check_reads_the_journal_and_logs_who_it_names_once() {
    // "Ghost" is not in the entry, so whatever the model says, it is not believed.
    let fake = fake_model(json!({ "people": [
        { "name": "Ravi", "via": "call", "note": "the move" },
        { "name": "Ghost", "via": "call", "note": "" },
    ]}))
    .await;
    let (svc, _dir, ravi) = env(&fake.endpoint, true);

    let report = call(&svc, "run_keep_in_touch").await;
    assert_eq!(report["entriesRead"], 1);
    assert_eq!(report["logged"], 1);
    assert_eq!(report["asked"], 0, "{report}");
    let vault = svc.get().unwrap();
    let logs = vault.logs(&LogQuery::for_item(ravi.id)).unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!((logs[0].via, logs[0].note.as_str()), (Some(Via::Call), "the move"));

    let first = fake.calls.load(Ordering::SeqCst);
    assert!(first > 0);
    let again = call(&svc, "run_keep_in_touch").await;
    assert_eq!(again["logged"], 0, "the same entry, read again, writes nothing");
    assert_eq!(vault.logs(&LogQuery::for_item(ravi.id)).unwrap().len(), 1);
    assert_eq!(fake.calls.load(Ordering::SeqCst), first * 2, "asked about the one entry again");

    let status = call(&svc, "keep_in_touch_status").await;
    assert_eq!(status["lastReport"]["entriesRead"], 1);
    assert_eq!(status["settings"]["enabled"], true);
}

#[tokio::test]
async fn what_the_journal_said_is_kept_from_a_client_without_the_journal() {
    let fake =
        fake_model(json!({ "people": [{ "name": "Ravi", "via": "call", "note": "the move" }]}))
            .await;
    let (svc, _dir, _ravi) = env(&fake.endpoint, true);
    call(&svc, "run_keep_in_touch").await;

    let mine = call(&svc, "list_people_due").await;
    assert_eq!(mine[0]["lastNote"], "the move");
    let shelf_only = Ctx {
        caller: Caller::Device("phone".into()),
        scopes: vec![Scope::Library],
        proved_at: None,
        request_id: None,
    };
    let theirs = svc.call(shelf_only.clone(), "list_people_due", json!({})).await.unwrap();
    assert_eq!(theirs[0]["lastNote"], "", "{theirs}");
    assert_eq!(theirs[0]["lastVia"], "call", "the rest of it is the library's");
    let id = theirs[0]["itemId"].clone();
    let logs =
        svc.call(shelf_only, "list_logs", json!({ "query": { "itemId": id } })).await.unwrap();
    assert_eq!(logs[0]["note"], "");
}

#[tokio::test]
async fn an_entry_the_model_fails_on_is_said_and_the_rest_still_runs() {
    let fake = fake_model(Value::Null).await;
    let (svc, _dir, _ravi) = env(&fake.endpoint, true);
    let report = call(&svc, "run_keep_in_touch").await;
    assert_eq!(report["entriesRead"], 0);
    let problem = report["journalProblem"].as_str().unwrap_or("");
    assert!(problem.contains("1 of 1 entries could not be read"), "{report}");
}

#[tokio::test]
async fn with_the_assistant_off_the_journal_is_left_unread_and_says_why() {
    let fake = fake_model(json!({ "people": [] })).await;
    let (svc, _dir, _ravi) = env(&fake.endpoint, false);
    let report = call(&svc, "run_keep_in_touch").await;
    assert_eq!(report["entriesRead"], 0);
    assert!(report["journalProblem"].as_str().unwrap_or("").contains("switched off"), "{report}");
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0, "nothing was sent anywhere");
}

#[tokio::test]
async fn nothing_runs_while_it_is_switched_off() {
    let fake = fake_model(json!({ "people": [] })).await;
    let (svc, _dir, _ravi) = env(&fake.endpoint, true);
    svc.get().unwrap().update_keep_in_touch(|k| k.settings.enabled = false).unwrap();
    assert!(svc.call(Ctx::local(), "run_keep_in_touch", json!({})).await.is_err());
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
}
