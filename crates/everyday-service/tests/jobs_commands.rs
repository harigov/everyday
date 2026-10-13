//! Background jobs' commands, and the way back to the conversation that asked,
//! end to end against a real vault.
//!
//! What is here is what only the command layer and `jobs::answer` decide: what
//! starting research does with a conversation, which jobs may be cancelled,
//! deleted or answered again, which changes each write announces, and what is
//! put in a thread -- and sent to a chat -- when a job comes back. The record's
//! own rules have their tests in the core, and the worker that runs a job has
//! its own.
//!
//! The model is a `TcpListener` on loopback speaking the Chat Completions shape,
//! as in `tests/routines.rs`: the base URL is a setting, so a scripted one is a
//! local model that always says the same thing. No key, no network, no bill.

use everyday_core::chat::Via;
use everyday_core::{
    AgentSettings, ChatLinkId, Conversation, ConversationId, Job, JobId, Message, MessageRole,
    Note, Vault,
};
use everyday_service::agent::ConfirmAnswer;
use everyday_service::chat::{ChatOutlet, FromRun};
use everyday_service::ctx::Ctx;
use everyday_service::events::{Change, EventSink, Kind, Op};
use everyday_service::jobs::answer;
use everyday_service::{Service, error::CommandError};
use futures::future::BoxFuture;
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[allow(dead_code)]
mod support;

#[derive(Default)]
struct Collector {
    changes: Mutex<Vec<Change>>,
}

impl EventSink for Collector {
    fn changed(&self, change: Change) {
        self.changes.lock().unwrap().push(change);
    }
}

impl Collector {
    fn take(&self) -> Vec<Change> {
        std::mem::take(&mut *self.changes.lock().unwrap())
    }
}

async fn call(svc: &Arc<Service>, name: &str, args: Value) -> Value {
    svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"))
}

async fn fails(svc: &Arc<Service>, name: &str, args: Value) -> CommandError {
    svc.call(Ctx::local(), name, args).await.expect_err("expected a failure")
}

/// A vault with a collector on it, and no assistant.
fn service() -> (Arc<Service>, Arc<Vault>, Arc<Collector>, tempfile::TempDir) {
    let (svc, dir) = support::vault::service(None);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let vault = svc.get().unwrap();
    (svc, vault, events, dir)
}

/// [`service`], with an assistant pointed at `endpoint`.
fn service_with_model(
    endpoint: &str,
) -> (Arc<Service>, Arc<Vault>, Arc<Collector>, tempfile::TempDir) {
    let (svc, vault, events, dir) = service();
    let mut settings = AgentSettings {
        enabled: true,
        // Everything is reckoned in UTC so the tests are about jobs rather
        // than about where the machine running them is.
        timezone: Some("UTC".into()),
        ..Default::default()
    };
    settings.provider_config.base_url = Some(endpoint.to_string());
    settings.assistant_model.model = "scripted".into();
    vault.save_agent_settings(&settings).unwrap();
    (svc, vault, events, dir)
}

const QUESTION: &str = "Which heat pumps suit a 1930s semi?";

/// A thread with the question in it, saved straight into the vault.
fn conversation(vault: &Vault) -> Conversation {
    let conversation = Conversation::new();
    vault.save_conversation(&conversation).unwrap();
    vault.save_message(&Message::user(conversation.id, QUESTION)).unwrap();
    conversation
}

/// A thread that arrived through a chat.
fn chat_conversation(vault: &Vault, link: ChatLinkId, room: &str) -> Conversation {
    let conversation = Conversation {
        via: Some(Via { link, service: "Matrix".into(), chat: room.into() }),
        ..Conversation::new()
    };
    vault.save_conversation(&conversation).unwrap();
    vault.save_message(&Message::user(conversation.id, QUESTION)).unwrap();
    conversation
}

fn queued(vault: &Vault, asked_in: Option<ConversationId>) -> Job {
    let job = Job::research(QUESTION, "Solid walls.", asked_in);
    vault.save_job(&job).unwrap();
    job
}

fn running(vault: &Vault, asked_in: Option<ConversationId>) -> Job {
    let mut job = Job::research(QUESTION, "", asked_in);
    job.begin();
    vault.save_job(&job).unwrap();
    job
}

/// A finished job, with the note it wrote if `note` is given.
fn finished(vault: &Vault, asked_in: Option<ConversationId>, note: Option<&Note>) -> Job {
    let mut job = Job::research(QUESTION, "", asked_in);
    job.begin();
    if let Some(note) = note {
        job.add_note(note.id);
    }
    job.finish("Air-to-water wins.");
    vault.save_job(&job).unwrap();
    job
}

fn failed(vault: &Vault, asked_in: Option<ConversationId>, note: Option<&Note>) -> Job {
    let mut job = Job::research(QUESTION, "", asked_in);
    job.begin();
    if let Some(note) = note {
        job.add_note(note.id);
    }
    job.fail("the endpoint stopped answering");
    vault.save_job(&job).unwrap();
    job
}

fn write_up(vault: &Vault) -> Note {
    let note = Note::written("Heat pumps for old houses", "Air-to-water suits solid walls.");
    vault.save_note(&note, None).unwrap();
    note
}

/// How a job reads after the command that changed it, so a test can ask the
/// vault rather than trust a result.
fn status_of(vault: &Vault, id: JobId) -> String {
    vault.job(id).unwrap().status.as_str().to_string()
}

fn system_lines(vault: &Vault, conversation: ConversationId) -> Vec<String> {
    vault
        .messages(conversation)
        .unwrap()
        .into_iter()
        .filter(|m| m.role == MessageRole::System)
        .map(|m| m.content)
        .collect()
}

// ---- starting research -------------------------------------------------------

#[tokio::test]
async fn starting_research_queues_a_job_that_comes_back_to_the_conversation() {
    let (svc, vault, events, _dir) = service();
    let asking = conversation(&vault);

    let job = call(
        &svc,
        "start_research",
        json!({
            "question": format!("  {QUESTION}  "),
            "brief": "  Solid walls, no loft insulation yet.  ",
            "conversationId": asking.id.to_string(),
        }),
    )
    .await;

    assert_eq!(job["status"], "queued", "queued, not run: the worker does that");
    assert_eq!(
        job["kind"],
        json!({
            "type": "research",
            "question": QUESTION,
            "brief": "Solid walls, no loft insulation yet.",
        }),
        "trimmed, and the brief kept"
    );
    assert_eq!(job["askedIn"], json!(asking.id.to_string()));

    let id: JobId = serde_json::from_value(job["id"].clone()).unwrap();
    let saved = vault.job(id).unwrap();
    assert_eq!(saved.asked_in, Some(asking.id));
    assert!(!saved.answered && !saved.seen);

    let changes = events.take();
    assert_eq!(changes.len(), 1);
    assert_eq!((changes[0].kind, changes[0].op), (Kind::Job, Op::Created));
}

#[tokio::test]
async fn a_job_started_by_hand_has_no_thread_to_come_back_to() {
    let (svc, vault, _events, _dir) = service();

    let job = call(&svc, "start_research", json!({ "question": QUESTION })).await;

    assert!(job.get("askedIn").is_none(), "{job}");
    assert_eq!(job["kind"]["brief"], "", "the brief is optional");
    assert_eq!(vault.jobs(&Default::default()).unwrap().len(), 1);
}

#[tokio::test]
async fn research_needs_a_question_and_a_conversation_that_exists() {
    let (svc, vault, events, _dir) = service();

    let blank = fails(&svc, "start_research", json!({ "question": "   " })).await;
    assert_eq!(blank.code, "invalid");

    let nowhere = fails(
        &svc,
        "start_research",
        json!({ "question": QUESTION, "conversationId": ConversationId::new().to_string() }),
    )
    .await;
    assert_eq!(nowhere.code, "not_found", "refused now, not when the answer has nowhere to go");

    assert!(vault.jobs(&Default::default()).unwrap().is_empty(), "nothing was queued");
    assert!(events.take().is_empty(), "and nothing was announced");
}

// ---- reading -------------------------------------------------------------------

#[tokio::test]
async fn jobs_are_listed_newest_first_and_can_be_filtered() {
    let (svc, vault, _events, _dir) = service();
    let asking = conversation(&vault);
    let now = Timestamp::now();

    let mut oldest = Job::research("oldest", "", None);
    oldest.created_at = now - SignedDuration::from_mins(30);
    oldest.begin();
    oldest.finish("done");
    let mut middle = Job::research("middle", "", Some(asking.id));
    middle.created_at = now - SignedDuration::from_mins(20);
    middle.seen = true;
    let mut newest = Job::research("newest", "", None);
    newest.created_at = now - SignedDuration::from_mins(10);
    for job in [&oldest, &middle, &newest] {
        vault.save_job(job).unwrap();
    }

    let all = call(&svc, "list_jobs", json!({})).await;
    let order: Vec<_> = all.as_array().unwrap().iter().map(|j| j["id"].clone()).collect();
    assert_eq!(
        order,
        [newest.id, middle.id, oldest.id].map(|id| json!(id.to_string())),
        "newest first"
    );

    let by = |query: Value| {
        let svc = svc.clone();
        async move {
            let listed = call(&svc, "list_jobs", json!({ "query": query })).await;
            listed.as_array().unwrap().iter().map(|j| j["id"].clone()).collect::<Vec<_>>()
        }
    };
    assert_eq!(by(json!({ "statuses": ["done"] })).await, [json!(oldest.id.to_string())]);
    assert_eq!(
        by(json!({ "askedIn": asking.id.to_string() })).await,
        [json!(middle.id.to_string())]
    );
    assert_eq!(
        by(json!({ "unseen": true })).await,
        [json!(newest.id.to_string()), json!(oldest.id.to_string())]
    );
    assert_eq!(by(json!({ "limit": 1 })).await, [json!(newest.id.to_string())]);
}

#[tokio::test]
async fn one_job_is_read_by_id_and_an_unknown_one_is_not_found() {
    let (svc, vault, _events, _dir) = service();
    let job = queued(&vault, None);

    let got = call(&svc, "get_job", json!({ "id": job.id.to_string() })).await;
    assert_eq!(got["id"], json!(job.id.to_string()));
    assert_eq!(got["kind"]["question"], QUESTION);

    let missing = fails(&svc, "get_job", json!({ "id": JobId::new().to_string() })).await;
    assert_eq!(missing.code, "not_found");
}

#[tokio::test]
async fn jobs_nobody_has_looked_at_are_counted_until_they_are_marked_seen() {
    let (svc, vault, events, _dir) = service();
    let (a, b, c) = (queued(&vault, None), queued(&vault, None), queued(&vault, None));
    assert_eq!(call(&svc, "unseen_jobs", json!({})).await, json!(3));

    call(&svc, "mark_jobs_seen", json!({ "ids": [a.id.to_string()] })).await;
    assert_eq!(call(&svc, "unseen_jobs", json!({})).await, json!(2));
    let changes = events.take();
    assert_eq!(changes.len(), 1);
    assert_eq!((changes[0].kind, changes[0].op), (Kind::Job, Op::Updated));
    assert_eq!(changes[0].ids, [a.id.to_string()], "a batch names its rows in `ids`");

    // None named means all of them, as for a routine's runs.
    call(&svc, "mark_jobs_seen", json!({})).await;
    assert_eq!(call(&svc, "unseen_jobs", json!({})).await, json!(0));
    assert!(vault.job(b.id).unwrap().seen && vault.job(c.id).unwrap().seen);
}

// ---- cancelling and deleting -----------------------------------------------------

#[tokio::test]
async fn cancelling_a_queued_job_cancels_it_and_says_who() {
    let (svc, vault, events, _dir) = service();
    let job = queued(&vault, None);

    let cancelled = call(&svc, "cancel_job", json!({ "id": job.id.to_string() })).await;

    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled["reason"], "cancelled by you");
    assert!(cancelled.get("finishedAt").is_some());
    assert_eq!(status_of(&vault, job.id), "cancelled");

    let changes = events.take();
    assert_eq!(changes.len(), 1);
    assert_eq!((changes[0].kind, changes[0].op), (Kind::Job, Op::Updated));
    assert_eq!(changes[0].id.as_deref(), Some(job.id.to_string().as_str()));
}

#[tokio::test]
async fn cancelling_a_running_job_keeps_what_it_had_written() {
    let (svc, vault, _events, _dir) = service();
    let note = write_up(&vault);
    let mut job = Job::research(QUESTION, "", None);
    job.begin();
    job.add_note(note.id);
    vault.save_job(&job).unwrap();

    let cancelled = call(&svc, "cancel_job", json!({ "id": job.id.to_string() })).await;

    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(vault.job(job.id).unwrap().note_id(), Some(note.id), "the note is still its output");
    assert!(vault.note(note.id).is_ok(), "and still a note");
}

#[tokio::test]
async fn a_job_that_is_over_cannot_be_cancelled() {
    let (svc, vault, events, _dir) = service();
    let done = finished(&vault, None, None);
    let already = {
        let mut job = queued(&vault, None);
        job.cancel("cancelled by you").unwrap();
        vault.save_job(&job).unwrap();
        job
    };

    for job in [&done, &already] {
        let e = fails(&svc, "cancel_job", json!({ "id": job.id.to_string() })).await;
        assert_eq!(e.code, "invalid");
        assert!(e.message.contains("already"), "{}", e.message);
    }
    assert_eq!(status_of(&vault, done.id), "done", "what happened to it is not rewritten");
    assert!(events.take().is_empty(), "a refusal announces nothing");

    let missing = fails(&svc, "cancel_job", json!({ "id": JobId::new().to_string() })).await;
    assert_eq!(missing.code, "not_found");
}

#[tokio::test]
async fn deleting_a_job_takes_its_transcript_and_leaves_its_note() {
    let (svc, vault, events, _dir) = service();
    let note = write_up(&vault);
    let transcript = Conversation::for_job(JobId::new(), "Research");
    vault.save_conversation(&transcript).unwrap();
    let mut job = Job::research(QUESTION, "", None);
    job.begin();
    job.add_note(note.id);
    job.conversation_id = Some(transcript.id);
    job.finish("done");
    vault.save_job(&job).unwrap();

    call(&svc, "delete_job", json!({ "id": job.id.to_string() })).await;

    assert_eq!(fails(&svc, "get_job", json!({ "id": job.id.to_string() })).await.code, "not_found");
    assert!(vault.conversation(transcript.id).is_err(), "its transcript went with it");
    assert!(vault.note(note.id).is_ok(), "the note is a note now, and somebody may have edited it");

    let changes = events.take();
    let deleted = changes.iter().find(|c| c.kind == Kind::Job).expect("a job change");
    assert_eq!(deleted.op, Op::Deleted);
    assert_eq!(deleted.id.as_deref(), Some(job.id.to_string().as_str()));
}

#[tokio::test]
async fn a_running_job_is_cancelled_before_it_is_deleted() {
    let (svc, vault, _events, _dir) = service();
    let job = running(&vault, None);

    let e = fails(&svc, "delete_job", json!({ "id": job.id.to_string() })).await;
    assert_eq!(e.code, "invalid");
    assert!(e.message.contains("cancel it first"), "{}", e.message);

    call(&svc, "cancel_job", json!({ "id": job.id.to_string() })).await;
    call(&svc, "delete_job", json!({ "id": job.id.to_string() })).await;
    assert!(vault.job(job.id).is_err());
}

// ---- answering again ---------------------------------------------------------------

#[tokio::test]
async fn only_a_finished_unanswered_job_asked_from_a_conversation_can_be_answered() {
    let (svc, vault, events, _dir) = service();
    let asking = conversation(&vault);

    let mut answered = finished(&vault, Some(asking.id), None);
    answered.answered = true;
    vault.save_job(&answered).unwrap();

    let refused = [
        ("not finished", queued(&vault, Some(asking.id))),
        ("not finished", running(&vault, Some(asking.id))),
        ("failed", failed(&vault, Some(asking.id), None)),
        ("nobody to answer", finished(&vault, None, None)),
        ("already been answered", answered),
    ];
    for (why, job) in refused {
        let e = fails(&svc, "answer_job", json!({ "id": job.id.to_string() })).await;
        assert_eq!(e.code, "invalid", "{why}");
        assert!(e.message.contains(why), "{} should mention {why:?}", e.message);
    }

    let cancelled = {
        let mut job = queued(&vault, Some(asking.id));
        job.cancel("cancelled by you").unwrap();
        vault.save_job(&job).unwrap();
        job
    };
    let e = fails(&svc, "answer_job", json!({ "id": cancelled.id.to_string() })).await;
    assert!(e.message.contains("cancelled"), "{}", e.message);

    assert!(events.take().is_empty(), "a refusal announces nothing");
}

#[tokio::test]
async fn answering_now_returns_at_once_and_the_answer_arrives_in_the_thread() {
    let model = fake_model("Air-to-water units suit it best.").await;
    let (svc, vault, _events, _dir) = service_with_model(&model.endpoint);
    let asking = conversation(&vault);
    let job = finished(&vault, Some(asking.id), Some(&write_up(&vault)));

    call(&svc, "answer_job", json!({ "id": job.id.to_string() })).await;

    let arrived = eventually(|| vault.job(job.id).unwrap().answered).await;
    assert!(arrived, "the follow-up ran in the background and marked the job answered");
    let thread = vault.messages(asking.id).unwrap();
    assert_eq!(thread.last().unwrap().content, "Air-to-water units suit it best.");
}

// ---- coming back -----------------------------------------------------------------

#[tokio::test]
async fn a_finished_job_is_answered_in_the_conversation_that_asked_and_marked_so() {
    let model = fake_model("Air-to-water units suit it best.").await;
    let (svc, vault, events, _dir) = service_with_model(&model.endpoint);
    let asking = conversation(&vault);
    let note = write_up(&vault);
    let job = finished(&vault, Some(asking.id), Some(&note));

    answer::answer(&svc, &vault, job.id).await.unwrap();

    let thread = vault.messages(asking.id).unwrap();
    // The person said one thing, and it was not this.
    let said: Vec<_> = thread.iter().filter(|m| m.role == MessageRole::User).collect();
    assert_eq!(said.len(), 1, "the follow-up is not put in the person's mouth");
    assert_eq!(said[0].content, QUESTION);

    let lines = system_lines(&vault, asking.id);
    assert_eq!(lines.len(), 1, "one line saying the research finished: {lines:?}");
    assert!(lines[0].starts_with(&format!("Research finished: «{QUESTION}»")), "{}", lines[0]);
    assert!(lines[0].contains("Heat pumps for old houses"), "names the note: {}", lines[0]);
    assert!(lines[0].contains(&note.id.to_string()), "and gives its id: {}", lines[0]);

    let last = thread.last().unwrap();
    assert_eq!(last.role, MessageRole::Assistant);
    assert_eq!(last.content, "Air-to-water units suit it best.");

    assert!(vault.job(job.id).unwrap().answered);

    // An open rail refreshes from the conversation's change, and an open card
    // from the job's; both say the assistant made them.
    let changes = events.take();
    let conversation_change = changes
        .iter()
        .find(|c| c.kind == Kind::Conversation)
        .expect("the thread's change is raised");
    assert_eq!(conversation_change.id.as_deref(), Some(asking.id.to_string().as_str()));
    assert_eq!(conversation_change.origin.as_deref(), Some("assistant"));
    let job_change = changes.iter().find(|c| c.kind == Kind::Job).expect("the job's change");
    assert_eq!(job_change.id.as_deref(), Some(job.id.to_string().as_str()));
    assert_eq!(job_change.origin.as_deref(), Some("assistant"));
}

/// A chat link's outlet, as a running relay registers one: writes down what
/// it is asked to send and to confirm, and answers a confirmation as told.
struct Posts {
    sent: Mutex<Vec<(FromRun, String)>>,
    confirms: Mutex<Vec<(FromRun, String, String)>>,
    confirm_with: ConfirmAnswer,
}

impl Default for Posts {
    fn default() -> Self {
        Self::answering(ConfirmAnswer::Decline)
    }
}

impl Posts {
    fn answering(confirm_with: ConfirmAnswer) -> Self {
        Self { sent: Mutex::default(), confirms: Mutex::default(), confirm_with }
    }

    fn sent(&self) -> Vec<(FromRun, String)> {
        self.sent.lock().unwrap().clone()
    }

    fn confirms(&self) -> Vec<(FromRun, String, String)> {
        self.confirms.lock().unwrap().clone()
    }
}

impl ChatOutlet for Posts {
    fn post(&self, from: FromRun, markdown: String) -> BoxFuture<'static, Result<(), String>> {
        self.sent.lock().unwrap().push((from, markdown));
        Box::pin(async { Ok(()) })
    }

    fn confirm(
        &self,
        from: FromRun,
        kind: String,
        subject: String,
        _can_park: bool,
    ) -> BoxFuture<'static, ConfirmAnswer> {
        self.confirms.lock().unwrap().push((from, kind, subject));
        let answer = self.confirm_with;
        Box::pin(async move { answer })
    }

    fn ask(&self, _: FromRun, _: String, _: Vec<String>) -> BoxFuture<'static, Option<String>> {
        panic!("a job's follow-up has no ask_owner to call")
    }
}

#[tokio::test]
async fn a_follow_up_in_a_chat_puts_a_delete_to_the_owner_there_and_does_it_on_yes() {
    let task = everyday_core::Task::new("Book the dentist");
    let model = fake_model_calling(
        "delete_task",
        json!({ "task_id": task.id.to_string() }).to_string(),
        "Deleted the dentist task, as you said.",
    )
    .await;
    let (svc, vault, _events, _dir) = service_with_model(&model.endpoint);
    vault.save_task(&task).unwrap();
    let link = ChatLinkId::new();
    let outlet = Arc::new(Posts::answering(ConfirmAnswer::Confirm));
    svc.open_chat_outlet(link, outlet.clone());
    let asking = chat_conversation(&vault, link, "!room:test");
    let job = finished(&vault, Some(asking.id), None);

    answer::answer(&svc, &vault, job.id).await.unwrap();

    // Asked on the phone, as a turn the owner started there would have been,
    // rather than declined for want of a card nobody would see.
    let confirms = outlet.confirms();
    assert_eq!(confirms.len(), 1, "{confirms:?}");
    assert_eq!(confirms[0].1, "destructive");
    assert_eq!(confirms[0].2, "Book the dentist");
    assert_eq!(
        confirms[0].0,
        FromRun { routine: "Research".into(), conversation: Some(asking.id) }
    );
    assert!(vault.task(task.id).is_err(), "asked, answered yes, and done");
    assert_eq!(outlet.sent().len(), 1, "and the answer still goes as one message");
}

#[tokio::test]
async fn a_follow_up_with_nobody_to_ask_declines_the_delete() {
    let task = everyday_core::Task::new("Book the dentist");
    let model = fake_model_calling(
        "delete_task",
        json!({ "task_id": task.id.to_string() }).to_string(),
        "I could not delete it.",
    )
    .await;
    let (svc, vault, _events, _dir) = service_with_model(&model.endpoint);
    vault.save_task(&task).unwrap();
    // Asked in the rail: no chat, so no outlet to put it to.
    let asking = conversation(&vault);
    let job = finished(&vault, Some(asking.id), None);

    answer::answer(&svc, &vault, job.id).await.unwrap();

    assert!(vault.task(task.id).is_ok(), "nobody said yes, so nothing was deleted");
    assert!(vault.job(job.id).unwrap().answered, "the answer was still had");
}

#[tokio::test]
async fn an_answer_for_a_chat_is_sent_to_that_chat_once() {
    let model = fake_model("Air-to-water units suit it best.").await;
    let (svc, vault, _events, _dir) = service_with_model(&model.endpoint);
    let link = ChatLinkId::new();
    let outlet = Arc::new(Posts::default());
    svc.open_chat_outlet(link, outlet.clone());
    let asking = chat_conversation(&vault, link, "!room:test");
    let job = finished(&vault, Some(asking.id), None);

    answer::answer(&svc, &vault, job.id).await.unwrap();

    let sent = outlet.sent();
    assert_eq!(sent.len(), 1, "one complete message, not a stream of them");
    assert_eq!(sent[0].1, "Air-to-water units suit it best.");
    // Naming the asking thread is what has the relay carry on in it when the
    // owner replies, so a follow-up question has the research behind it.
    assert_eq!(sent[0].0, FromRun { routine: "Research".into(), conversation: Some(asking.id) });
    assert!(vault.job(job.id).unwrap().answered);
}

#[tokio::test]
async fn a_chat_that_is_down_does_not_stop_the_answer_being_had() {
    let model = fake_model("Air-to-water units suit it best.").await;
    let (svc, vault, _events, _dir) = service_with_model(&model.endpoint);
    // No outlet registered: the link's relay is not running here.
    let asking = chat_conversation(&vault, ChatLinkId::new(), "!room:test");
    let job = finished(&vault, Some(asking.id), None);

    answer::answer(&svc, &vault, job.id).await.unwrap();

    assert!(vault.job(job.id).unwrap().answered, "it is in the thread, which is where it counts");
}

#[tokio::test]
async fn a_job_that_owes_nothing_is_left_alone() {
    // An endpoint nothing listens on: any attempt to ask the model would fail,
    // so Ok here means it never tried.
    let (svc, vault, events, _dir) = service_with_model("http://127.0.0.1:1/v1");
    let asking = conversation(&vault);
    let before = vault.messages(asking.id).unwrap().len();

    let mut answered = finished(&vault, Some(asking.id), None);
    answered.answered = true;
    vault.save_job(&answered).unwrap();
    let jobs = [
        queued(&vault, Some(asking.id)),
        failed(&vault, Some(asking.id), None),
        finished(&vault, None, None),
        answered,
    ];
    for job in &jobs {
        answer::answer(&svc, &vault, job.id).await.unwrap();
    }
    // One that was deleted while it waited its turn.
    answer::answer(&svc, &vault, JobId::new()).await.unwrap();

    assert_eq!(vault.messages(asking.id).unwrap().len(), before, "nothing was said");
    assert!(events.take().is_empty());
}

#[tokio::test]
async fn a_job_whose_conversation_is_gone_stops_owing_an_answer() {
    let (svc, vault, _events, _dir) = service_with_model("http://127.0.0.1:1/v1");
    let job = finished(&vault, Some(ConversationId::new()), None);

    answer::answer(&svc, &vault, job.id).await.unwrap();

    assert!(vault.job(job.id).unwrap().answered, "left owing, the worker would try again for ever");
}

#[tokio::test]
async fn a_follow_up_that_fails_leaves_the_job_owing_and_is_not_retried_at_once() {
    let (svc, vault, _events, _dir) = service_with_model("http://127.0.0.1:1/v1");
    let asking = conversation(&vault);
    let job = finished(&vault, Some(asking.id), None);

    assert!(answer::answer(&svc, &vault, job.id).await.is_err(), "the model could not be reached");
    assert!(!vault.job(job.id).unwrap().answered, "still owing: the card offers Answer now");
    assert_eq!(status_of(&vault, job.id), "done", "and still finished");

    // The worker offers the job again on its next pass. Each attempt would put
    // another line in the thread, so it waits.
    assert!(answer::answer(&svc, &vault, job.id).await.is_ok(), "backing off, not failing");
    // A person pressing the button does not wait.
    assert!(answer::answer_now(&svc, &vault, job.id).await.is_err());
}

// ---- a job that could not finish ----------------------------------------------------

#[tokio::test]
async fn a_failed_job_tells_the_conversation_that_asked_once() {
    let (svc, vault, events, _dir) = service();
    let asking = conversation(&vault);
    let note = write_up(&vault);
    let job = failed(&vault, Some(asking.id), Some(&note));

    answer::report_failure(&svc, &vault, job.id).await.unwrap();
    // Called again -- a restart after the sweep that failed it -- it does not
    // say it twice.
    answer::report_failure(&svc, &vault, job.id).await.unwrap();

    let lines = system_lines(&vault, asking.id);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with(&format!("Research on «{QUESTION}» could not finish")),
        "{}",
        lines[0]
    );
    assert!(lines[0].contains("the endpoint stopped answering"), "{}", lines[0]);
    assert!(
        lines[0].contains("What it found so far is in the note «Heat pumps for old houses»"),
        "{}",
        lines[0]
    );

    let changes = events.take();
    let thread = changes.iter().filter(|c| c.kind == Kind::Conversation).count();
    assert_eq!(thread, 1, "the open thread is told to refresh, once");
    assert_eq!(changes.iter().filter(|c| c.kind == Kind::Job).count(), 1);
}

#[tokio::test]
async fn a_failure_is_sent_to_the_chat_that_asked() {
    let (svc, vault, _events, _dir) = service();
    let link = ChatLinkId::new();
    let outlet = Arc::new(Posts::default());
    svc.open_chat_outlet(link, outlet.clone());
    let asking = chat_conversation(&vault, link, "!room:test");
    let job = failed(&vault, Some(asking.id), None);

    answer::report_failure(&svc, &vault, job.id).await.unwrap();

    let sent = outlet.sent();
    assert_eq!(sent.len(), 1);
    let (from, markdown) = &sent[0];
    assert_eq!(from.conversation, Some(asking.id));
    assert!(markdown.contains("could not finish"), "{markdown}");
    assert_eq!(
        system_lines(&vault, asking.id),
        std::slice::from_ref(markdown),
        "the same words in both places"
    );
}

#[tokio::test]
async fn only_a_failed_job_somebody_asked_for_is_reported() {
    let (svc, vault, events, _dir) = service();
    let asking = conversation(&vault);

    let nobody = failed(&vault, None, None);
    let done = finished(&vault, Some(asking.id), None);
    let cancelled = {
        let mut job = running(&vault, Some(asking.id));
        job.cancel("cancelled by you").unwrap();
        vault.save_job(&job).unwrap();
        job
    };
    for job in [&nobody, &done, &cancelled] {
        answer::report_failure(&svc, &vault, job.id).await.unwrap();
    }
    answer::report_failure(&svc, &vault, JobId::new()).await.unwrap();

    assert!(system_lines(&vault, asking.id).is_empty(), "a cancelled job does not come back");
    assert!(events.take().is_empty());
}

// ---- a scripted model --------------------------------------------------------------

/// A model on loopback that says the same thing to everything. Dropping the
/// handle stops it.
struct FakeModel {
    endpoint: String,
    _shutdown: tokio::sync::watch::Sender<bool>,
}

async fn fake_model(text: &'static str) -> FakeModel {
    serve_model(None, text.to_string()).await
}

/// A model that asks for `tool` with `arguments` the first time it is asked,
/// and says `then` every time after -- rig asks again with the tool's result.
async fn fake_model_calling(tool: &str, arguments: String, then: &str) -> FakeModel {
    serve_model(Some((tool.to_string(), arguments)), then.to_string()).await
}

async fn serve_model(call: Option<(String, String)>, text: String) -> FakeModel {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        let mut asked = 0usize;
        loop {
            let accepted = tokio::select! {
                r = listener.accept() => r,
                _ = rx.changed() => break,
            };
            let Ok((mut socket, _)) = accepted else { break };
            asked += 1;
            let body = match (&call, asked) {
                (Some((tool, arguments)), 1) => reply_calling(tool, arguments),
                _ => reply(&text),
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 64 * 1024];
                let _ = socket.read(&mut buf).await;
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

/// A request for one tool, as a stream of Chat Completions chunks.
fn reply_calling(tool: &str, arguments: &str) -> String {
    let chunk = |delta: Value, finish: Option<&str>| {
        let chunk = json!({
            "id": "chatcmpl-test",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "scripted",
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
        });
        format!("data: {chunk}\n\n")
    };
    let call = json!({
        "role": "assistant",
        "tool_calls": [{
            "index": 0,
            "id": "call_1",
            "type": "function",
            "function": { "name": tool, "arguments": arguments },
        }],
    });
    format!("{}{}data: [DONE]\n\n", chunk(call, None), chunk(json!({}), Some("tool_calls")))
}

/// One answer, as a stream of Chat Completions chunks.
fn reply(text: &str) -> String {
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
    frame(json!({ "role": "assistant", "content": text }), None);
    frame(json!({}), Some("stop"));
    out.push_str("data: [DONE]\n\n");
    out
}

/// Poll `done` for up to ten seconds. For the one thing here that happens on a
/// task the test did not start.
async fn eventually(mut done: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if done() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}
