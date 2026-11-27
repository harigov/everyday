//! The dream routine, end to end, against a fake model.
//!
//! Mirrors `tests/routines.rs`'s own harness -- a `TcpListener` on loopback
//! speaking the Chat Completions shape -- because a dream is a routine and
//! runs down exactly the same path once `scheduler::execute` has decided it
//! may start. What is new here is everything about that decision, and about
//! the digest that becomes the run's first message.

use jatayu_core::routine::{DreamScope, Outcome, RoutineKind, Trigger};
use jatayu_core::store::routines::RunQuery;
use jatayu_service::Service;
use std::sync::{Arc, Mutex};

/// A vault with a journal, an assistant pointed at `endpoint`, and dreaming
/// off -- the state every new vault is in.
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
    vault.save_journal(&jatayu_core::Journal::new("Journal")).unwrap();

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

/// Holds every test in this file to one at a time.
///
/// [`jatayu_service::agent::turn_in_flight`] reads a process-wide counter
/// -- deliberately, see its own doc -- and this binary runs its tests in
/// parallel threads by default, which would otherwise let one test's turn
/// bleed into another's idle-guard check. Acquired for the whole of every
/// test below.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What the fake model does when it is asked. A sequence of tool calls,
/// each answered in turn, ending in a final message with no more calls.
/// An empty `calls` is a model that only ever says `then`.
#[derive(Clone)]
enum Script {
    Sequence {
        calls: Vec<(String, String)>,
        then: String,
    },
    /// Accept the connection and never answer. What a wedged provider is.
    Hangs,
}

fn says(text: &str) -> Script {
    Script::Sequence { calls: Vec::new(), then: text.to_string() }
}

fn calls(tool: &str, arguments: String, then: &str) -> Script {
    Script::Sequence { calls: vec![(tool.to_string(), arguments)], then: then.to_string() }
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

                    if matches!(script, Script::Hangs) {
                        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                        return;
                    }
                    let Script::Sequence { calls, then } = &script else { unreachable!() };
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

/// Turn dreaming fully on: the three routines, enabled, and the switch in
/// `AgentSettings` that the scheduler itself reads.
fn dreaming_on(svc: &Arc<Service>) {
    let vault = svc.get().unwrap();
    vault.set_dreaming(true).unwrap();
    let mut settings = vault.agent_settings().unwrap();
    settings.dreaming = true;
    vault.save_agent_settings(&settings).unwrap();
}

fn nightly(svc: &Arc<Service>) -> jatayu_core::Routine {
    let vault = svc.get().unwrap();
    vault
        .routines()
        .unwrap()
        .into_iter()
        .find(|r| r.kind == RoutineKind::Dream { scope: DreamScope::Day })
        .expect("set_dreaming already ran")
}

/// Move the nightly dream's slot `hours_late` (fractional) behind the real
/// clock, keeping its 20-hour grace, so its lateness relative to now is
/// exactly what the test wants to assert about -- the same relative-clock
/// idiom `tests/routines.rs::due_now` uses, since nothing in this harness can
/// fake the wall clock the scheduler actually reads.
fn set_nightly_slot(svc: &Arc<Service>, hours_late: f64) {
    let vault = svc.get().unwrap();
    let mut routine = nightly(svc);
    let now = vault.agent_settings().unwrap().now();
    let then = now.timestamp() - jiff::SignedDuration::from_secs((hours_late * 3600.0) as i64);
    let local = then.to_zoned(now.time_zone().clone());
    routine.trigger = Trigger::Schedule {
        at: jiff::civil::time(local.hour(), local.minute(), 0, 0),
        days: vec![],
        day_of_month: None,
    };
    routine.created_at = then - jiff::SignedDuration::from_hours(1);
    vault.save_routine(&routine).unwrap();
}

// ---- whether a dream may start at all --------------------------------------

#[tokio::test]
async fn a_nightly_dream_within_its_grace_runs() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(says("A quiet night.")).await;
    let (svc, _dir) = service(&model.endpoint);
    dreaming_on(&svc);
    seed_yesterday(&svc);
    // 03:00 plus six hours twelve is 09:12 -- inside a twenty-hour grace.
    set_nightly_slot(&svc, 6.2);

    jatayu_service::scheduler::tick(&svc).await;

    let vault = svc.get().unwrap();
    let runs = vault.runs(&RunQuery::default()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, Outcome::Done, "{:?}", runs[0]);
}

#[tokio::test]
async fn a_nightly_dream_past_its_grace_is_missed() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(says("should never be said")).await;
    let (svc, _dir) = service(&model.endpoint);
    dreaming_on(&svc);
    // 03:00 plus twenty hours thirty is past the twenty-hour grace.
    set_nightly_slot(&svc, 20.5);

    jatayu_service::scheduler::tick(&svc).await;

    let vault = svc.get().unwrap();
    let runs = vault.runs(&RunQuery::default()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, Outcome::Skipped);
    assert!(runs[0].reason.contains("past the"), "got {:?}", runs[0].reason);
}

#[tokio::test]
async fn a_dream_is_skipped_when_dreaming_is_switched_off() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(says("should never be said")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    // The routines exist and are enabled, but the switch itself -- the one
    // `save_agent_settings`'s own hook flips -- was never turned on. A
    // belt-and-braces state a race or a hand-edited settings row could leave.
    vault.set_dreaming(true).unwrap();
    assert!(!vault.agent_settings().unwrap().dreaming);
    set_nightly_slot(&svc, 1.0);

    jatayu_service::scheduler::tick(&svc).await;

    let runs = vault.runs(&RunQuery::default()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, Outcome::Skipped);
    assert!(runs[0].reason.contains("switched off"), "got {:?}", runs[0].reason);
    assert!(runs[0].seen, "a skip asks for no attention on the app bar");
}

#[tokio::test]
async fn an_empty_digest_is_skipped_without_a_model_call() {
    let _serial = SERIAL.lock().await;
    // Nothing at all happened yesterday: no tasks, no entries, nothing.
    // The model must never even be asked.
    let model = fake_model(Script::Hangs).await;
    let (svc, _dir) = service(&model.endpoint);
    dreaming_on(&svc);
    set_nightly_slot(&svc, 1.0);

    jatayu_service::scheduler::tick(&svc).await;

    let vault = svc.get().unwrap();
    let runs = vault.runs(&RunQuery::default()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, Outcome::Skipped);
    assert!(runs[0].reason.contains("nothing happened"), "got {:?}", runs[0].reason);
    assert!(runs[0].seen);
    assert!(
        runs[0].conversation_id.is_none(),
        "no transcript for a run that never reached the model"
    );
}

#[tokio::test]
async fn a_dream_does_not_start_while_a_conversation_is_in_flight() {
    let _serial = SERIAL.lock().await;
    // A rail turn is left hanging on the model -- occupying the one live-turn
    // slot `crate::agent::turn_in_flight` reads -- while a nightly dream's
    // slot is due. The dream must not start, and must not stamp its slot:
    // it has to be tried again on a later tick.
    let model = fake_model(Script::Hangs).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let conversation = jatayu_core::Conversation::new();
    vault.save_conversation(&conversation).unwrap();

    let busy = tokio::spawn({
        let svc = svc.clone();
        let id = conversation.id;
        async move {
            let _ = svc
                .send_message(
                    jatayu_service::ctx::Ctx::local(),
                    serde_json::json!({ "conversationId": id.to_string(), "prompt": "hello" }),
                    Arc::new(|_| {}),
                )
                .await;
        }
    });
    // Give the rail's turn a moment to actually reach the model and register
    // itself as live before the tick runs.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    dreaming_on(&svc);
    set_nightly_slot(&svc, 1.0);
    jatayu_service::scheduler::tick(&svc).await;

    assert!(vault.runs(&RunQuery::default()).unwrap().is_empty(), "the dream must wait");
    let nightly_after = nightly(&svc);
    assert!(nightly_after.last_run_at.is_none(), "the slot must not be stamped either");

    busy.abort();
    // `abort` only requests cancellation; wait for the guard it was holding
    // to actually drop before this test ends. `turn_in_flight` is a
    // process-wide counter (see its own doc), and this binary runs its
    // tests in parallel by default -- leaving it dirty would defer an
    // unrelated test's dream for no reason it could see.
    for _ in 0..100 {
        if !jatayu_service::agent::turn_in_flight() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(!jatayu_service::agent::turn_in_flight(), "the guard must have been dropped by now");
}

// ---- the digest and the drafting turn --------------------------------------

/// A vault with something in yesterday's digest, so the dream is not skipped
/// as empty.
fn seed_yesterday(svc: &Arc<Service>) {
    let vault = svc.get().unwrap();
    let now = vault.agent_settings().unwrap().now();
    let yesterday = now.date().yesterday().unwrap();
    let mut task = jatayu_core::Task::new("Book the dentist");
    task.status = jatayu_core::task::TaskStatus::Done;
    task.completed_at = Some(yesterday.at(12, 0, 0, 0).in_tz("UTC").unwrap().timestamp());
    vault.save_task(&task).unwrap();
}

#[tokio::test]
async fn the_runs_first_message_carries_the_digest_contract() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(says("Nothing needed doing.")).await;
    let (svc, _dir) = service(&model.endpoint);
    dreaming_on(&svc);
    seed_yesterday(&svc);
    set_nightly_slot(&svc, 1.0);

    jatayu_service::scheduler::tick(&svc).await;

    let vault = svc.get().unwrap();
    let run = &vault.runs(&RunQuery::default()).unwrap()[0];
    assert_eq!(run.outcome, Outcome::Done, "{:?}", run);
    let conversation = run.conversation_id.expect("a transcript");
    let messages = vault.messages(conversation).unwrap();
    let first_user =
        messages.iter().find(|m| m.role == jatayu_core::MessageRole::User).expect("a user message");

    let marker = first_user.content.find("--- digest ---").expect("the exact marker line");
    let digest_part = &first_user.content[marker..];
    assert!(digest_part.contains("Book the dentist"), "the digest itself follows the marker");
    // Nothing else in the application may emit this line -- it must appear
    // exactly once.
    assert_eq!(first_user.content.matches("--- digest ---").count(), 1);
}

#[tokio::test]
async fn the_digest_is_fenced_as_other_peoples_words_and_the_instructions_ask_for_the_final_shape()
{
    let _serial = SERIAL.lock().await;
    let model =
        fake_model(says("Noticed:\n- Booked the dentist (2026-09-15)\nProposed:\n- Nothing")).await;
    let (svc, _dir) = service(&model.endpoint);
    dreaming_on(&svc);
    seed_yesterday(&svc);
    set_nightly_slot(&svc, 1.0);

    jatayu_service::scheduler::tick(&svc).await;

    let vault = svc.get().unwrap();
    let run = &vault.runs(&RunQuery::default()).unwrap()[0];
    assert_eq!(run.outcome, Outcome::Done, "{run:?}");
    let messages = vault.messages(run.conversation_id.expect("a transcript")).unwrap();
    let first_user =
        messages.iter().find(|m| m.role == jatayu_core::MessageRole::User).expect("a user message");
    let content = &first_user.content;

    // What it is asked to end with is what the weekly dream reads.
    let marker = content.find("--- digest ---").expect("the marker");
    let (before, digest_part) = content.split_at(marker);
    assert!(before.contains("Noticed:") && before.contains("Proposed:"), "{before}");
    assert!(!before.contains("three separate days"));
    // The digest after the marker is inside the fence, with the record in it.
    let open = digest_part.find("BEGIN TEXT WRITTEN BY SOMEBODY ELSE").expect("fenced");
    let close = digest_part.find("END TEXT WRITTEN BY SOMEBODY ELSE").expect("closed");
    assert!(open < digest_part.find("Book the dentist").unwrap(), "{digest_part}");
    assert!(digest_part.find("Book the dentist").unwrap() < close, "{digest_part}");
    assert_eq!(content.matches("--- digest ---").count(), 1);

    // And the account it wrote is kept after the counts, on lines of its own.
    assert!(
        run.summary.starts_with("0 proposals, 0 memories, 0 notes.\n\nNoticed:"),
        "{:?}",
        run.summary
    );
    assert!(run.summary.contains("- Booked the dentist (2026-09-15)"));
}

/// An account with one unread Priority thread in its Inbox, `subject` the
/// stranger's.
fn account_with_unread_priority(
    vault: &jatayu_core::Vault,
    address: &str,
    subject: &str,
    acknowledged_for: Option<&str>,
) -> jatayu_core::account::Account {
    use jatayu_core::account::{Account, Provider};
    use jatayu_core::id::{MailMessageId, PackId, ThreadId};
    use jatayu_core::mail::{
        Address, Category, CategorySource, Mailbox, MailboxRole, Message, MessageFlags,
    };
    use jatayu_core::packstore::PackRef;
    use jatayu_core::store::mail::IngestMessage;

    let mut account = Account::new(Provider::Custom, address);
    account.services.mail = true;
    account.assistant_provider_acknowledged = acknowledged_for.map(str::to_string);
    vault.save_account(&account).unwrap();
    let mailbox = Mailbox::new(account.id, MailboxRole::Inbox.as_str(), MailboxRole::Inbox);
    vault.save_mailbox(&mailbox).unwrap();
    let id = MailMessageId::new();
    let message = Message {
        id,
        account_id: account.id,
        thread_id: ThreadId::new(),
        message_id_header: format!("<{id}@example.com>"),
        date: jiff::Timestamp::now(),
        from: Address::new("Pat Stranger", "pat@elsewhere.example"),
        to: vec![Address::bare(address)],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: subject.into(),
        snippet: "Please look at this".into(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 128,
        category: Some(Category::Priority),
        category_source: CategorySource::Rules,
        pack: PackRef { account: account.id.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
        invite: None,
    };
    vault
        .ingest_mail(account.id, vec![IngestMessage { message, mailbox: mailbox.id, uid: 1 }])
        .unwrap();
    account
}

#[tokio::test]
async fn a_dream_is_handed_the_subjects_of_only_the_mail_the_assistant_may_read() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(says("Nothing needed doing.")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    // The provider this run's model is on, as the person acknowledges it.
    let (_, access) = vault.agent_credentials(jatayu_core::llm::ModelTier::Medium).unwrap();
    // Shared: mail on, this provider acknowledged, reading allowed. The others
    // each lack one of the three.
    account_with_unread_priority(
        &vault,
        "shared@example.com",
        "Quarterly numbers",
        Some(&access.acknowledgement),
    );
    account_with_unread_priority(
        &vault,
        "unacknowledged@example.com",
        "Unacknowledged subject",
        None,
    );
    account_with_unread_priority(
        &vault,
        "elsewhere@example.com",
        "Other provider subject",
        Some("another provider"),
    );
    let mut no_read = account_with_unread_priority(
        &vault,
        "noread@example.com",
        "No read subject",
        Some(&access.acknowledgement),
    );
    no_read.assistant_access = jatayu_core::account::AgentMailAccess::none();
    vault.save_account(&no_read).unwrap();

    dreaming_on(&svc);
    seed_yesterday(&svc);
    set_nightly_slot(&svc, 1.0);
    jatayu_service::scheduler::tick(&svc).await;

    let run = &vault.runs(&RunQuery::default()).unwrap()[0];
    assert_eq!(run.outcome, Outcome::Done, "{run:?}");
    let messages = vault.messages(run.conversation_id.expect("a transcript")).unwrap();
    let first_user =
        messages.iter().find(|m| m.role == jatayu_core::MessageRole::User).expect("a user message");
    let content = &first_user.content;
    assert!(content.contains("Awaiting reply: \"Quarterly numbers\""), "{content}");
    assert!(content.contains("shared@example.com: 1 unread"), "{content}");
    for held_back in [
        "Unacknowledged subject",
        "unacknowledged@example.com",
        "Other provider subject",
        "elsewhere@example.com",
        "No read subject",
        "noread@example.com",
    ] {
        assert!(
            !content.contains(held_back),
            "{held_back} is from an account the assistant may not read: {content}"
        );
    }
}

#[tokio::test]
async fn the_offered_tools_carry_why_and_exclude_web_search() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(says("Nothing needed doing.")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let mut settings = vault.agent_settings().unwrap();
    settings.web = true; // on, so its absence from a dream is meaningful
    vault.save_agent_settings(&settings).unwrap();
    let mut models = vault.model_settings().unwrap();
    models.medium.connection.base_url = Some(model.endpoint.clone());
    vault.save_model_settings(&models).unwrap();
    dreaming_on(&svc);
    seed_yesterday(&svc);
    set_nightly_slot(&svc, 1.0);

    jatayu_service::scheduler::tick(&svc).await;

    let requests = model.requests.lock().unwrap();
    let first = requests.first().expect("at least one request reached the model");
    let tools = first["tools"].as_array().expect("a tool list");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["function"]["name"].as_str()).collect();
    assert!(!names.contains(&"web_search"), "a dream reads the vault, not the web");

    let create_task =
        tools.iter().find(|t| t["function"]["name"] == "create_task").expect("create_task offered");
    let props = &create_task["function"]["parameters"]["properties"];
    assert!(props.get("why").is_some(), "a writing tool gains `why` while drafting");
}

#[tokio::test]
async fn a_dream_may_write_at_most_one_note() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(sequence(
        &[
            ("create_note", r#"{"title":"First","body":"One."}"#),
            ("create_note", r#"{"title":"Second","body":"Two."}"#),
        ],
        "I wrote what there was to say.",
    ))
    .await;
    let (svc, _dir) = service(&model.endpoint);
    dreaming_on(&svc);
    seed_yesterday(&svc);
    set_nightly_slot(&svc, 1.0);

    jatayu_service::scheduler::tick(&svc).await;

    let vault = svc.get().unwrap();
    let notes = vault.notes(&Default::default()).unwrap();
    assert_eq!(notes.len(), 1, "only the first note is written for real");
    assert_eq!(notes[0].title, "First");

    let run = &vault.runs(&RunQuery::default()).unwrap()[0];
    assert_eq!(run.outcome, Outcome::Done);
    // Names the counts, ahead of whatever the model itself said.
    assert!(run.summary.contains("1 note"), "got {:?}", run.summary);
}

#[tokio::test]
async fn a_dream_proposes_a_task_rather_than_creating_it() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(calls(
        "create_task",
        r#"{"title":"Call the dentist","why":"overdue and mentioned twice this week"}"#.to_string(),
        "Proposed a task about the dentist.",
    ))
    .await;
    let (svc, _dir) = service(&model.endpoint);
    dreaming_on(&svc);
    seed_yesterday(&svc);
    set_nightly_slot(&svc, 1.0);

    jatayu_service::scheduler::tick(&svc).await;

    let vault = svc.get().unwrap();
    assert!(
        vault.tasks(&Default::default()).unwrap().iter().all(|t| t.title != "Call the dentist"),
        "a dream must never write a task directly"
    );
    let proposals = vault.proposals(&Default::default()).unwrap();
    assert_eq!(proposals.len(), 1, "the write became a proposal instead");
}

// ---- a dream asked for by hand ----------------------------------------------

#[tokio::test]
async fn a_dream_run_by_hand_is_still_a_dream() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(calls(
        "create_task",
        r#"{"title":"Call the dentist","why":"mentioned twice"}"#.to_string(),
        "Proposed a task.",
    ))
    .await;
    let (svc, _dir) = service(&model.endpoint);
    dreaming_on(&svc);
    seed_yesterday(&svc);
    let routine = nightly(&svc);

    svc.call(
        jatayu_service::ctx::Ctx::local(),
        "run_routine",
        serde_json::json!({ "id": routine.id }),
    )
    .await
    .expect("run_routine queues it");
    jatayu_service::scheduler::tick(&svc).await;

    let vault = svc.get().unwrap();
    let runs = vault.runs(&RunQuery::default()).unwrap();
    assert_eq!(runs.len(), 1, "the queued row is the run, not a second one");
    let run = &runs[0];
    assert_eq!(run.outcome, Outcome::Done, "{run:?}");

    let messages = vault.messages(run.conversation_id.expect("a transcript")).unwrap();
    let first_user =
        messages.iter().find(|m| m.role == jatayu_core::MessageRole::User).expect("a user message");
    assert!(first_user.content.contains("--- digest ---"), "the digest is its first message");

    assert!(
        vault.tasks(&Default::default()).unwrap().iter().all(|t| t.title != "Call the dentist"),
        "run by hand, it still only proposes"
    );
    assert_eq!(vault.proposals(&Default::default()).unwrap().len(), 1);
}

#[tokio::test]
async fn a_dream_run_by_hand_with_dreaming_off_closes_its_own_row() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(says("should not be asked")).await;
    let (svc, _dir) = service(&model.endpoint);
    dreaming_on(&svc);
    let routine = nightly(&svc);
    let vault = svc.get().unwrap();
    let mut settings = vault.agent_settings().unwrap();
    settings.dreaming = false;
    vault.save_agent_settings(&settings).unwrap();

    svc.call(
        jatayu_service::ctx::Ctx::local(),
        "run_routine",
        serde_json::json!({ "id": routine.id }),
    )
    .await
    .expect("run_routine queues it");
    jatayu_service::scheduler::tick(&svc).await;

    let runs = vault.runs(&RunQuery::default()).unwrap();
    assert_eq!(runs.len(), 1, "skipped in place, not skipped beside");
    assert_eq!(runs[0].outcome, Outcome::Skipped);
    assert!(runs[0].reason.contains("switched off"), "{:?}", runs[0].reason);
    assert!(model.requests.lock().unwrap().is_empty(), "no model call");
}
