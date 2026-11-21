//! A brief, end to end, against a fake model: the digest becomes the first
//! message, the turn is offered only tools that read, and what the model
//! wrote is taken apart and kept on the run. Uses `tests/dream.rs`'s harness.

use karmayoga_core::brief::{BriefKind, DIGEST_MARKER};
use karmayoga_core::routine::{Outcome, RoutineRun};
use karmayoga_service::Service;
use std::sync::{Arc, Mutex};

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
    let mut task = karmayoga_core::Task::new("Send the grant report");
    task.due_date = Some(settings.now().date());
    vault.save_task(&task).unwrap();

    let svc = Arc::new(Service::new());
    svc.set(vault);
    (svc, dir)
}

/// Holds the tests in this file to one at a time, as `tests/dream.rs` does.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What the fake model does when it is asked. A sequence of tool calls,
/// each answered in turn, ending in a final message with no more calls.
/// An empty `calls` is a model that only ever says `then`.
#[derive(Clone)]
enum Script {
    Sequence { calls: Vec<(String, String)>, then: String },
}

fn says(text: &str) -> Script {
    Script::Sequence { calls: Vec::new(), then: text.to_string() }
}

fn sequence(calls: &[(&str, &str)], then: &str) -> Script {
    Script::Sequence {
        calls: calls.iter().map(|(t, a)| (t.to_string(), a.to_string())).collect(),
        then: then.to_string(),
    }
}

/// A model on loopback. Dropping the handle stops it. `requests` collects
/// each call's JSON body, in order, so a test can inspect what the harness
/// actually sent -- the tools it offered, and the first user message.
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
                    // Read the whole request: a single `read` returns what one
                    // segment held, and the full tool catalogue is larger than
                    // that.
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
                    let raw = &buf[..];
                    if let Some(body_at) = find_body(raw)
                        && let Ok(json) =
                            serde_json::from_slice::<serde_json::Value>(&raw[body_at..])
                    {
                        requests.lock().unwrap().push(json);
                    }

                    let Script::Sequence { calls, then } = &script;
                    let body = match calls.get(turn - 1) {
                        Some((tool, arguments)) => reply("", Some((tool, arguments))),
                        None => reply(then, None),
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

/// The request's `Content-Length`, 0 when it names none.
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

/// One answer, as a stream of Chat Completions chunks. See `tests/routines.rs`.
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

/// Switch the morning brief on and ask for one now, as "Make one now" does.
fn queue_morning(svc: &Arc<Service>) -> RoutineRun {
    let vault = svc.get().unwrap();
    let routine = vault.set_brief(BriefKind::Morning, true).unwrap();
    let run = RoutineRun::new(&routine, None);
    vault.save_run(&run).unwrap();
    run
}

const ANSWER: &str = "# A light day: **the grant report** is due.\n\n\
    ## Due today {tasks}\n- **Send the grant report** -- nothing else is due.\n\n\
    --- spoken ---\nGood morning. One thing today: the grant report.";

#[tokio::test]
async fn a_brief_is_written_from_its_digest_and_kept_on_its_run() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(says(ANSWER)).await;
    let (svc, _dir) = service(&model.endpoint);
    let queued = queue_morning(&svc);

    karmayoga_service::scheduler::tick(&svc).await;

    let run = svc.get().unwrap().run(queued.id).unwrap();
    assert_eq!(run.outcome, Outcome::Done, "{}", run.reason);
    let brief = run.brief.as_deref().expect("the brief is kept on the run");
    assert_eq!(brief.headline, "A light day: **the grant report** is due.");
    assert_eq!(brief.parts.len(), 1);
    assert_eq!(brief.parts[0].section, Some(karmayoga_core::brief::BriefSection::Tasks));
    assert_eq!(brief.spoken, "Good morning. One thing today: the grant report.");
    assert_eq!(brief.digest.due.len(), 1, "the digest it was written from is kept too");
    // What the chat and the email get: the written brief, no section marks.
    assert!(run.summary.starts_with("# A light day"));
    assert!(!run.summary.contains("{tasks}"));
    assert!(!run.summary.contains("spoken"));

    let requests = model.requests.lock().unwrap();
    let first = &requests[0];
    let user = first["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .and_then(|m| {
            m["content"].as_str().map(str::to_string).or_else(|| {
                m["content"]
                    .as_array()
                    .and_then(|parts| parts[0]["text"].as_str().map(str::to_string))
            })
        })
        .unwrap();
    assert!(user.contains(DIGEST_MARKER), "{user}");
    assert!(user.contains("BEGIN TEXT WRITTEN BY SOMEBODY ELSE"), "the digest is fenced");
    assert!(user.contains("Send the grant report"));
    let offered: Vec<String> = first["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert!(offered.iter().any(|t| t == "list_tasks"), "it may read: {offered:?}");
    for write in
        ["create_task", "delete_task", "create_note", "send_draft", "web_search", "update_plan"]
    {
        assert!(!offered.iter().any(|t| t == write), "{write} was offered: {offered:?}");
    }
}

#[tokio::test]
async fn a_brief_that_writes_nothing_fails_rather_than_keeping_an_empty_one() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(says("")).await;
    let (svc, _dir) = service(&model.endpoint);
    let queued = queue_morning(&svc);

    karmayoga_service::scheduler::tick(&svc).await;

    let run = svc.get().unwrap().run(queued.id).unwrap();
    assert_ne!(run.outcome, Outcome::Done);
    assert!(run.brief.is_none());
    let briefs = svc.get().unwrap().briefs(None, 5).unwrap();
    assert!(briefs.is_empty());
}

#[tokio::test]
async fn a_brief_may_read_before_it_writes() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(sequence(&[("list_tasks", "{}")], ANSWER)).await;
    let (svc, _dir) = service(&model.endpoint);
    let queued = queue_morning(&svc);

    karmayoga_service::scheduler::tick(&svc).await;

    let run = svc.get().unwrap().run(queued.id).unwrap();
    assert_eq!(run.outcome, Outcome::Done, "{}", run.reason);
    assert_eq!(model.requests.lock().unwrap().len(), 2, "asked again after the read");
    assert!(run.brief.is_some());
}

#[tokio::test]
async fn on_the_weekly_briefs_day_the_morning_brief_steps_aside() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(says(ANSWER)).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let now = vault.agent_settings().unwrap().now();
    let today = karmayoga_core::routine::Weekday::parse(&now.strftime("%a").to_string()).unwrap();

    // The weekly brief runs today; the morning brief is due right now.
    let mut weekly = vault.set_brief(BriefKind::Weekly, true).unwrap();
    weekly.trigger = karmayoga_core::routine::Trigger::Schedule {
        at: jiff::civil::time(23, 59, 0, 0),
        days: vec![today],
        day_of_month: None,
    };
    vault.save_routine(&weekly).unwrap();
    let mut morning = vault.set_brief(BriefKind::Morning, true).unwrap();
    let then = now.timestamp() - jiff::SignedDuration::from_mins(5);
    let local = then.to_zoned(now.time_zone().clone());
    morning.trigger = karmayoga_core::routine::Trigger::Schedule {
        at: jiff::civil::time(local.hour(), local.minute(), 0, 0),
        days: vec![],
        day_of_month: None,
    };
    morning.created_at = then - jiff::SignedDuration::from_hours(1);
    vault.save_routine(&morning).unwrap();

    karmayoga_service::scheduler::tick(&svc).await;

    let runs =
        vault.runs(&karmayoga_core::store::routines::RunQuery::for_routine(morning.id)).unwrap();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].outcome, Outcome::Skipped);
    assert!(runs[0].reason.contains("weekly brief"), "{}", runs[0].reason);
    assert!(model.requests.lock().unwrap().is_empty(), "no model was asked");
}

/// The weekly brief and the morning brief, both on today, the morning one due
/// now. Hands back the morning brief's routine.
fn both_due_today(svc: &Arc<Service>) -> karmayoga_core::Routine {
    let vault = svc.get().unwrap();
    let now = vault.agent_settings().unwrap().now();
    let today = karmayoga_core::routine::Weekday::parse(&now.strftime("%a").to_string()).unwrap();
    let mut weekly = vault.set_brief(BriefKind::Weekly, true).unwrap();
    weekly.trigger = karmayoga_core::routine::Trigger::Schedule {
        at: jiff::civil::time(0, 0, 0, 0),
        days: vec![today],
        day_of_month: None,
    };
    vault.save_routine(&weekly).unwrap();
    let mut morning = vault.set_brief(BriefKind::Morning, true).unwrap();
    let then = now.timestamp() - jiff::SignedDuration::from_mins(5);
    let local = then.to_zoned(now.time_zone().clone());
    morning.trigger = karmayoga_core::routine::Trigger::Schedule {
        at: jiff::civil::time(local.hour(), local.minute(), 0, 0),
        days: vec![],
        day_of_month: None,
    };
    morning.created_at = then - jiff::SignedDuration::from_hours(1);
    vault.save_routine(&morning).unwrap();
    morning
}

#[tokio::test]
async fn if_the_weekly_brief_failed_today_the_morning_brief_runs_after_all() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(says(ANSWER)).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let morning = both_due_today(&svc);
    let weekly = vault.brief_routine(BriefKind::Weekly).unwrap().unwrap();
    let mut failed = RoutineRun::new(&weekly, None);
    failed.fail("the model was unreachable");
    vault.save_run(&failed).unwrap();

    karmayoga_service::scheduler::tick(&svc).await;

    let runs =
        vault.runs(&karmayoga_core::store::routines::RunQuery::for_routine(morning.id)).unwrap();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].outcome, Outcome::Done, "{}", runs[0].reason);
}

#[tokio::test]
async fn a_brief_with_no_model_is_skipped_before_anything_is_gathered() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(says(ANSWER)).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let mut settings = vault.agent_settings().unwrap();
    settings.enabled = false;
    vault.save_agent_settings(&settings).unwrap();
    let queued = queue_morning(&svc);

    karmayoga_service::scheduler::tick(&svc).await;

    let run = vault.run(queued.id).unwrap();
    assert_eq!(run.outcome, Outcome::Skipped);
    assert!(!run.reason.is_empty());
    assert!(model.requests.lock().unwrap().is_empty());
}
