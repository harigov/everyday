//! The job worker, end to end, against a fake model.
//!
//! What the unit tests beside the worker cannot say: that a queued research
//! job is really picked up and carried to an end, that the note it writes in
//! is there before the model is asked anything, that a job a dead process
//! left is taken back rather than left spinning, and that a job somebody
//! cancels is never run, or stops, and stays cancelled.
//!
//! The model is the one `tests/routines.rs` uses: a `TcpListener` on loopback
//! speaking the Chat Completions shape, so no key, no network and no bill.
//! A job turn is the same turn a routine run takes, which is why the same fake
//! serves.

use everyday_core::job::MAX_ATTEMPTS;
use everyday_core::{ConversationQuery, Job, JobKind, JobOutput, JobStatus, MessageRole, Note};
use everyday_service::Service;
use everyday_service::events::{Change, EventSink, Kind, Notification, Op, Reach};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A vault with a journal, an assistant pointed at `endpoint`, and nothing else.
fn service(endpoint: &str) -> (Arc<Service>, tempfile::TempDir) {
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
        timezone: Some("UTC".into()),
        ..Default::default()
    };
    vault.save_agent_settings(&settings).unwrap();
    let mut models = everyday_core::llm::ModelSettings::default();
    models.medium.connection.base_url = Some(endpoint.to_string());
    models.medium.model.model = "scripted".into();
    vault.save_model_settings(&models).unwrap();

    let svc = Arc::new(Service::new());
    svc.set(vault);
    (svc, dir)
}

#[derive(Default)]
struct Collector {
    changes: Mutex<Vec<Change>>,
    notices: Mutex<Vec<Notification>>,
}

impl EventSink for Collector {
    fn changed(&self, change: Change) {
        self.changes.lock().unwrap().push(change);
    }
    fn notify(&self, notification: Notification) {
        self.notices.lock().unwrap().push(notification);
    }
}

/// What the fake model does when it is asked.
#[derive(Clone)]
enum Script {
    /// One reply, no tools.
    Says(String),
    /// Ask for a tool by name with these arguments, then say something.
    Calls { tool: String, arguments: String, then: String },
    /// Accept the connection and never answer. What a model that is slow to
    /// think looks like from outside, and what a job is cancelled during.
    Hangs,
}

fn says(text: &str) -> Script {
    Script::Says(text.to_string())
}

fn calls(tool: &str, arguments: String, then: &str) -> Script {
    Script::Calls { tool: tool.to_string(), arguments, then: then.to_string() }
}

/// A model on loopback. Dropping the handle stops it.
struct FakeModel {
    endpoint: String,
    /// How many times it has been asked anything. What "never run" is
    /// proved by.
    asked: Arc<AtomicUsize>,
    _shutdown: tokio::sync::watch::Sender<bool>,
}

async fn fake_model(script: Script) -> FakeModel {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let asked = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let asked = asked.clone();
        async move {
            // One reply per connection, and the tool answer is the *second*
            // one: rig asks again with the tool's result, and a model that
            // repeated its request would loop until the step ceiling.
            loop {
                let accepted = tokio::select! {
                    r = listener.accept() => r,
                    _ = rx.changed() => break,
                };
                let Ok((mut socket, _)) = accepted else { break };
                let script = script.clone();
                let turn = asked.fetch_add(1, Ordering::SeqCst) + 1;
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = vec![0u8; 64 * 1024];
                    let _ = socket.read(&mut buf).await;
                    if matches!(script, Script::Hangs) {
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
                    // streams every turn, so a plain body is refused before
                    // it is read.
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

    FakeModel { endpoint: format!("http://127.0.0.1:{port}/v1"), asked, _shutdown: tx }
}

/// One answer, as a stream of Chat Completions chunks.
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

/// A research job, queued the way the tool queues one.
fn queue(svc: &Arc<Service>, question: &str) -> Job {
    svc.get()
        .unwrap()
        .queue_job(
            JobKind::Research {
                question: question.to_string(),
                brief: "For hills; twelve kilometres each way.".to_string(),
            },
            None,
        )
        .unwrap()
}

/// Wait, in real time, for `done` to be true.
async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "gave up waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn has_change(events: &Collector, kind: Kind, op: Op, id: Option<&str>) -> bool {
    let changes = events.changes.lock().unwrap();
    changes.iter().any(|c| c.kind == kind && c.op == op && (id.is_none() || c.id.as_deref() == id))
}

#[tokio::test]
async fn a_queued_research_job_runs_to_done_with_a_note_and_a_transcript() {
    let model = fake_model(says("Three bikes are worth a look.")).await;
    let (svc, _dir) = service(&model.endpoint);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let queued = queue(&svc, "Which e-bike for hills?");

    everyday_service::jobs::pass(&svc).await;

    let vault = svc.get().unwrap();
    let job = vault.job(queued.id).unwrap();
    assert_eq!(job.status, JobStatus::Done, "{job:?}");
    assert_eq!(job.summary, "Three bikes are worth a look.");
    assert_eq!(job.attempts, 1);
    assert!(job.started_at.is_some() && job.finished_at.is_some());
    assert!(job.progress.is_empty(), "the line is for while it runs");
    assert!(!job.seen, "it is waiting to be looked at");
    assert_eq!(vault.unseen_jobs().unwrap(), 1);
    assert_eq!(svc.running_job(), None, "and nothing is running any more");

    // The note was made up front, and the job says so.
    let note_id = job.note_id().expect("the note is in the outputs");
    assert_eq!(job.outputs, vec![JobOutput::Note { note_id }]);
    let note = vault.note(note_id).unwrap();
    assert_eq!(note.title, "Which e-bike for hills?");
    assert_eq!(note.tags, vec!["research".to_string()]);
    // The model never wrote in it, so its closing reply was put there rather
    // than leaving the job "done" over a placeholder.
    let body = note.body.plain_text();
    assert!(body.contains("Three bikes are worth a look."), "{body}");
    assert!(!body.contains("Researching"), "the placeholder is gone: {body}");

    // The transcript is the job's own: marked with it, and out of the rail.
    let transcript = vault.conversation(job.conversation_id.expect("a transcript")).unwrap();
    assert_eq!(transcript.job_id, Some(job.id));
    assert!(!transcript.is_chat());
    assert_eq!(transcript.title, "Which e-bike for hills?");
    let chats = vault.conversations(&ConversationQuery::chats(20)).unwrap();
    assert!(chats.iter().all(|c| c.id != transcript.id), "a job's transcript is not a chat");
    let messages = vault.messages(transcript.id).unwrap();
    let first = &messages[0];
    assert_eq!(first.role, MessageRole::User);
    assert!(first.content.contains("Which e-bike for hills?"), "{}", first.content);
    assert!(first.content.contains("twelve kilometres"), "the brief is passed on");
    assert!(
        first.content.contains(&note_id.to_string()),
        "it is told where to write before it is asked anything"
    );

    // Open windows hear about the note and the job.
    let (note_key, job_key) = (note_id.to_string(), job.id.to_string());
    assert!(has_change(&events, Kind::Note, Op::Created, Some(&note_key)));
    assert!(has_change(&events, Kind::Job, Op::Updated, Some(&job_key)));

    // And so does the person, once, keyed on the job so a repeat replaces it.
    let notices = events.notices.lock().unwrap();
    let ready: Vec<_> = notices.iter().filter(|n| n.title == "Research is ready").collect();
    assert_eq!(ready.len(), 1, "once: {notices:?}");
    // Never the question: this is drawn over a lock screen.
    assert_eq!(ready[0].body, None);
    assert_eq!(ready[0].key.as_deref(), Some(format!("job:{}", job.id).as_str()));
    assert!(matches!(ready[0].reach, Reach::User));
}

#[tokio::test]
async fn a_job_started_again_goes_on_in_the_note_and_the_transcript_it_left() {
    // What a restart looks like from the vault: a row saying `Running` that
    // nothing alive is carrying out, with a note and a transcript already made.
    let (svc, _dir) = service("http://127.0.0.1:1/v1");
    let vault = svc.get().unwrap();
    let mut note = Note::written("Which e-bike for hills?", "So far: the Bosch motor.");
    note.tags = vec!["research".to_string()];
    vault.save_note(&note, None).unwrap();

    let mut job = queue(&svc, "Which e-bike for hills?");
    let transcript = everyday_core::Conversation::for_job(job.id, "Which e-bike for hills?");
    vault.save_conversation(&transcript).unwrap();
    job.add_note(note.id);
    job.conversation_id = Some(transcript.id);
    job.begin();
    vault.save_job(&job).unwrap();

    let model = fake_model(calls(
        "update_note",
        serde_json::json!({
            "note_id": note.id.to_string(),
            "body": "# Answer\n\nThe Bosch.\n\n## Sources\n\n- https://example.com/bikes",
        })
        .to_string(),
        "Written up in the note.",
    ))
    .await;
    let mut models = vault.model_settings().unwrap();
    models.medium.connection.base_url = Some(model.endpoint.clone());
    vault.save_model_settings(&models).unwrap();

    everyday_service::jobs::pass(&svc).await;

    let done = vault.job(job.id).unwrap();
    assert_eq!(done.status, JobStatus::Done, "{done:?}");
    assert_eq!(done.attempts, 2, "taken back, then started again");
    assert_eq!(done.steps, 1, "one tool call");
    assert_eq!(done.summary, "Written up in the note.");
    assert_eq!(done.outputs, vec![JobOutput::Note { note_id: note.id }]);
    assert_eq!(done.conversation_id, Some(transcript.id), "the same transcript");

    assert_eq!(vault.notes(&Default::default()).unwrap().len(), 1, "no second note");
    let written = vault.note(note.id).unwrap().body.plain_text();
    assert!(written.contains("The Bosch."), "{written}");
    assert!(!written.contains("So far"), "it replaced the body, as update_note does");
    assert!(!written.contains("did not write its findings"), "it had written in it: {written}");

    let messages = vault.messages(transcript.id).unwrap();
    let prompt = messages.iter().find(|m| m.role == MessageRole::User).unwrap();
    assert!(prompt.content.contains("interrupted"), "told it is a restart: {}", prompt.content);
}

#[tokio::test]
async fn a_job_a_dead_process_left_running_is_started_again() {
    let model = fake_model(says("Done after a restart.")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let mut orphan = queue(&svc, "Which e-bike for hills?");
    orphan.begin();
    vault.save_job(&orphan).unwrap();
    assert_eq!(orphan.attempts, 1);

    everyday_service::jobs::pass(&svc).await;

    let job = vault.job(orphan.id).unwrap();
    assert_eq!(job.status, JobStatus::Done, "{job:?}");
    assert_eq!(job.attempts, 2, "its first attempt counted");
    assert_eq!(model.asked.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_job_interrupted_too_often_is_failed_and_says_why() {
    let model = fake_model(says("should never be said")).await;
    let (svc, _dir) = service(&model.endpoint);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let vault = svc.get().unwrap();
    let mut worn = queue(&svc, "Which e-bike for hills?");
    for _ in 0..MAX_ATTEMPTS {
        worn.begin();
    }
    vault.save_job(&worn).unwrap();

    everyday_service::jobs::pass(&svc).await;

    let job = vault.job(worn.id).unwrap();
    assert_eq!(job.status, JobStatus::Failed, "{job:?}");
    assert!(job.reason.contains("interrupted"), "got {:?}", job.reason);
    assert!(job.reason.contains("the app stopped"), "and why: {:?}", job.reason);
    assert_eq!(job.attempts, MAX_ATTEMPTS, "and it was not started a third time");
    assert_eq!(model.asked.load(Ordering::SeqCst), 0);
    assert!(vault.notes(&Default::default()).unwrap().is_empty(), "nothing was set up");
    assert!(vault.conversations(&ConversationQuery::default()).unwrap().is_empty());

    let notices = events.notices.lock().unwrap();
    let notice = notices.iter().find(|n| n.title == "Research could not finish").expect("a notice");
    assert_eq!(notice.body.as_deref(), Some(job.reason.as_str()));
}

#[tokio::test]
async fn a_job_cancelled_while_queued_is_never_run() {
    let model = fake_model(says("should never be said")).await;
    let (svc, _dir) = service(&model.endpoint);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let queued = queue(&svc, "Which e-bike for hills?");
    let vault = svc.get().unwrap();
    vault.cancel_job(queued.id, "they changed their mind").unwrap();

    everyday_service::jobs::pass(&svc).await;

    let job = vault.job(queued.id).unwrap();
    assert_eq!(job.status, JobStatus::Cancelled);
    assert_eq!(job.attempts, 0, "never started");
    assert_eq!(job.reason, "they changed their mind");
    assert_eq!(model.asked.load(Ordering::SeqCst), 0);
    assert!(vault.notes(&Default::default()).unwrap().is_empty());
    assert!(events.notices.lock().unwrap().is_empty(), "a cancel says nothing");
}

#[tokio::test]
async fn a_job_cancelled_while_it_runs_is_stopped_and_stays_cancelled() {
    // The model never answers, so the job is running until it is cancelled.
    // The worker finds the cancellation on its next look at the record, which
    // is a couple of seconds, in real time.
    let model = fake_model(Script::Hangs).await;
    let (svc, _dir) = service(&model.endpoint);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let queued = queue(&svc, "Which e-bike for hills?");
    let vault = svc.get().unwrap();

    let worker = tokio::spawn({
        let svc = svc.clone();
        async move { everyday_service::jobs::pass(&svc).await }
    });
    eventually("the job to be running with its transcript", || {
        let job = vault.job(queued.id).unwrap();
        job.status == JobStatus::Running && job.conversation_id.is_some()
    })
    .await;
    assert_eq!(svc.running_job(), Some(queued.id));

    vault.cancel_job(queued.id, "stopped by the assistant").unwrap();
    tokio::time::timeout(Duration::from_secs(30), worker)
        .await
        .expect("the worker notices the cancellation and stops the turn")
        .unwrap();

    let job = vault.job(queued.id).unwrap();
    assert_eq!(job.status, JobStatus::Cancelled, "{job:?}");
    assert_eq!(job.reason, "stopped by the assistant");
    assert!(job.progress.is_empty());
    assert!(job.note_id().is_some_and(|id| vault.note(id).is_ok()), "what it wrote is kept");
    assert_eq!(svc.running_job(), None);
    let notices = events.notices.lock().unwrap();
    assert!(notices.is_empty(), "whoever cancelled it knows: {notices:?}");
}

#[tokio::test]
async fn a_job_whose_assistant_is_switched_off_fails_with_the_reason() {
    let model = fake_model(says("should never be said")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let mut settings = vault.agent_settings().unwrap();
    settings.enabled = false;
    vault.save_agent_settings(&settings).unwrap();
    let queued = queue(&svc, "Which e-bike for hills?");

    everyday_service::jobs::pass(&svc).await;

    let job = vault.job(queued.id).unwrap();
    assert_eq!(job.status, JobStatus::Failed, "{job:?}");
    assert!(job.reason.contains("switched off"), "the reason is the useful part: {:?}", job.reason);
    assert_eq!(model.asked.load(Ordering::SeqCst), 0);
    let notes = vault.notes(&Default::default()).unwrap();
    assert!(notes.is_empty(), "no note for a job that cannot run");
}

#[tokio::test]
async fn a_job_that_fails_tells_the_conversation_that_asked() {
    let model = fake_model(says("should never be said")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let mut settings = vault.agent_settings().unwrap();
    settings.enabled = false;
    vault.save_agent_settings(&settings).unwrap();
    let thread = everyday_core::Conversation::new();
    vault.save_conversation(&thread).unwrap();
    let (question, brief) = ("Which e-bike for hills?".to_string(), String::new());
    let kind = JobKind::Research { question, brief };
    let queued = vault.queue_job(kind, Some(thread.id)).unwrap();

    everyday_service::jobs::pass(&svc).await;

    // Said by a task the pass spawns, not by the pass, so it is waited for.
    let said =
        || vault.messages(thread.id).unwrap().into_iter().find(|m| m.role == MessageRole::System);
    eventually("the thread to be told", || said().is_some()).await;
    let line = said().unwrap().content;
    assert!(line.contains("could not finish"), "{line}");
    assert!(line.contains("Which e-bike for hills?"), "it says which: {line}");
    eventually("the job to be marked told", || vault.job(queued.id).unwrap().answered).await;

    // Once, however many passes there are.
    everyday_service::jobs::pass(&svc).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let told = vault.messages(thread.id).unwrap();
    assert_eq!(told.iter().filter(|m| m.role == MessageRole::System).count(), 1);
    assert_eq!(model.asked.load(Ordering::SeqCst), 0, "no model is asked to say it");
}

#[tokio::test]
async fn a_model_that_cannot_be_reached_fails_the_job_rather_than_the_worker() {
    // Nothing listens on port 1.
    let (svc, _dir) = service("http://127.0.0.1:1/v1");
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let queued = queue(&svc, "Which e-bike for hills?");

    everyday_service::jobs::pass(&svc).await;

    let vault = svc.get().unwrap();
    let job = vault.job(queued.id).unwrap();
    assert_eq!(job.status, JobStatus::Failed, "{job:?}");
    assert!(!job.reason.is_empty());
    assert!(job.note_id().is_some(), "the note exists, so a failure still leaves a place to look");
    assert_eq!(svc.running_job(), None);
    let notices = events.notices.lock().unwrap();
    assert!(notices.iter().any(|n| n.title == "Research could not finish"), "{notices:?}");
}

#[tokio::test]
async fn nothing_runs_while_the_vault_is_locked() {
    let model = fake_model(says("should never be said")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let queued = queue(&svc, "Which e-bike for hills?");
    // What a job a restart left looks like: it would be swept, if anything
    // were allowed to write.
    let mut running = queue(&svc, "And the other one?");
    running.begin();
    vault.save_job(&running).unwrap();
    vault.lock();

    everyday_service::jobs::pass(&svc).await;

    vault.unlock(None).unwrap();
    assert_eq!(vault.job(queued.id).unwrap().status, JobStatus::Queued);
    let untouched = vault.job(running.id).unwrap();
    assert_eq!(untouched.status, JobStatus::Running, "not even the sweep ran");
    assert_eq!(untouched.attempts, 1);
    assert_eq!(model.asked.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_oldest_queued_job_runs_first_and_only_one_runs_at_a_time() {
    let model = fake_model(says("Looked into it.")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let mut older = Job::research("The older question", "", None);
    older.created_at = jiff::Timestamp::now() - jiff::SignedDuration::from_mins(10);
    let newer = Job::research("The newer question", "", None);
    // Saved the wrong way round on purpose: the store lists newest first.
    vault.save_job(&newer).unwrap();
    vault.save_job(&older).unwrap();

    everyday_service::jobs::pass(&svc).await;
    assert_eq!(vault.job(older.id).unwrap().status, JobStatus::Done);
    assert_eq!(vault.job(newer.id).unwrap().status, JobStatus::Queued, "it waits its turn");
    assert_eq!(model.asked.load(Ordering::SeqCst), 1);

    everyday_service::jobs::pass(&svc).await;
    assert_eq!(vault.job(newer.id).unwrap().status, JobStatus::Done);
}

#[tokio::test]
async fn the_loop_runs_a_job_when_woken_and_stops_when_asked() {
    let model = fake_model(says("Looked into it.")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let (stop, listen) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(everyday_service::jobs::run(svc.clone(), listen));

    // It has looked once, found nothing, and is waiting to be woken.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let queued = queue(&svc, "Which e-bike for hills?");
    svc.wake_jobs();

    eventually("the woken worker to finish the job", || {
        vault.job(queued.id).unwrap().status == JobStatus::Done
    })
    .await;

    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(10), worker)
        .await
        .expect("the worker stops when asked")
        .unwrap();
}
