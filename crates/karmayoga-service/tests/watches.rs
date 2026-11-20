//! The watch loop, end to end: a watch that is due is looked at, and what the
//! look came to is in the record afterwards.
//!
//! Two kinds of test. The first need no model and no service: a watch whose
//! time is up, whose probe cannot be taken, that is cancelled or not yet due,
//! or whose assistant is switched off. The second give the assistant's own
//! look to a model on loopback that says what it is told to, and the judge the
//! same way, so that a whole look -- the look, the fingerprint, the judgement,
//! the update, the sleep -- runs against the real record.
//!
//! The fake answers by looking at what it was asked. A structured ask (the
//! judge, or the model that writes an update) has `submit` as its one tool and
//! expects one JSON document back; an assistant's turn expects a stream. A
//! single script serves both.

#[allow(dead_code)]
mod support;

use jiff::{SignedDuration, Timestamp};
use karmayoga_core::llm::ModelSettings;
use karmayoga_core::{AgentSettings, Job, JobRules, JobStatus, Probe};
use karmayoga_service::Service;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

// ---- fixtures ---------------------------------------------------------------

/// A vault with the assistant switched on and its model at `endpoint`.
fn service(endpoint: &str) -> (Arc<Service>, tempfile::TempDir) {
    let (svc, dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let settings =
        AgentSettings { enabled: true, timezone: Some("UTC".into()), ..Default::default() };
    vault.save_agent_settings(&settings).unwrap();
    let mut models = ModelSettings::default();
    models.medium.connection.base_url = Some(endpoint.to_string());
    models.medium.model.model = "scripted".into();
    vault.save_model_settings(&models).unwrap();
    (svc, dir)
}

/// A watch on a connection that is not there: every look of it fails.
fn nowhere() -> Probe {
    Probe::Connection {
        connection: "Nowhere".into(),
        tool: "get_check_runs".into(),
        arguments: json!({}),
    }
}

/// The assistant looks for itself.
fn by_looking() -> Probe {
    Probe::Assistant { how: "Check the CI page for pull request 412".into() }
}

/// Make a watch, due at `at`, and put it in the vault.
fn add(svc: &Arc<Service>, probe: Probe, rules: JobRules, at: Timestamp) -> Job {
    let job = Job::watch("CI on pull request 412", probe, rules, None, at);
    svc.get().unwrap().save_job(&job).unwrap();
    job
}

fn finishes() -> JobRules {
    JobRules { notify_when: "when it finishes or fails".into(), ..Default::default() }
}

/// Make the watch due now, as `look_now` does.
fn due_now(svc: &Arc<Service>, job: &Job) {
    let vault = svc.get().unwrap();
    let mut job = vault.job(job.id).unwrap();
    job.wake_at = Some(Timestamp::now());
    vault.save_job(&job).unwrap();
}

async fn pass(svc: &Arc<Service>) {
    karmayoga_service::jobs::watch::pass(svc).await;
}

// ---- no model --------------------------------------------------------------

#[tokio::test]
async fn a_watch_whose_time_is_up_says_so_and_ends() {
    let (svc, _dir) = service("http://127.0.0.1:1/v1");
    let created = Timestamp::now() - SignedDuration::from_hours(9);
    let rules = JobRules { until: Some(created + SignedDuration::from_hours(8)), ..finishes() };
    let watch = add(&svc, nowhere(), rules, created);

    pass(&svc).await;

    let job = svc.get().unwrap().job(watch.id).unwrap();
    assert_eq!(job.status, JobStatus::Done, "{job:?}");
    assert!(
        job.summary.starts_with("I've stopped watching CI on pull request 412"),
        "{}",
        job.summary
    );
    assert!(job.summary.contains("the 8 hours are up"), "{}", job.summary);
    assert_eq!(job.updates.len(), 1, "what it said is on its record");
    assert!(job.updates[0].delivered);
    assert!(job.answered, "it said what it had to; nobody is owed a follow-up");
    assert!(!job.owes_answer());
    assert!(!job.seen, "and it is news");
    assert!(job.wake_at.is_none());
    assert_eq!(svc.running_job(), None, "nothing is left claimed");
    // It never tried to look: that is what out of time means.
    assert_eq!(job.observed.as_ref().map(|o| o.checks), Some(0));
}

#[tokio::test]
async fn a_watch_that_cannot_look_backs_off_and_then_gives_up() {
    let (svc, _dir) = service("http://127.0.0.1:1/v1");
    let vault = svc.get().unwrap();
    let watch = add(&svc, nowhere(), finishes(), Timestamp::now());

    pass(&svc).await;

    let job = vault.job(watch.id).unwrap();
    assert_eq!(job.status, JobStatus::Waiting, "one failure does not end it");
    let observed = job.observed.clone().unwrap();
    assert_eq!((observed.failures, observed.checks), (1, 1));
    assert!(observed.trouble.contains("Nowhere"), "{}", observed.trouble);
    assert!(job.updates.is_empty(), "it does not tell them about every failed look");
    // Twice its five minutes.
    let wake = job.wake_at.expect("asleep until");
    let now = Timestamp::now();
    assert!(wake > now + SignedDuration::from_mins(9), "{wake}");
    assert!(wake < now + SignedDuration::from_mins(11), "{wake}");

    // Not looked at again before then.
    pass(&svc).await;
    assert_eq!(vault.job(watch.id).unwrap().observed.unwrap().checks, 1);

    // The third failure in a row is the last.
    let mut third = vault.job(watch.id).unwrap();
    third.observed.as_mut().unwrap().failures = 2;
    third.wake_at = Some(Timestamp::now());
    vault.save_job(&third).unwrap();
    pass(&svc).await;

    let job = vault.job(watch.id).unwrap();
    assert_eq!(job.status, JobStatus::Failed, "{job:?}");
    assert!(job.reason.contains("3 times in a row"), "{}", job.reason);
    assert!(job.answered, "it told them itself; the worker must not tell them again");
    assert!(job.wake_at.is_none());
    let said = job.last_update().expect("it said it had stopped");
    assert!(said.text.starts_with("I've stopped watching CI on pull request 412"), "{}", said.text);
    assert!(said.text.contains("Nowhere"), "{}", said.text);
    assert_eq!(svc.running_job(), None);
}

#[tokio::test]
async fn a_cancelled_watch_and_one_not_yet_due_are_left_alone() {
    let (svc, _dir) = service("http://127.0.0.1:1/v1");
    let vault = svc.get().unwrap();

    let mut cancelled = add(&svc, nowhere(), finishes(), Timestamp::now());
    cancelled.cancel("they changed their mind").unwrap();
    vault.save_job(&cancelled).unwrap();
    let later = Timestamp::now() + SignedDuration::from_hours(1);
    let not_yet = add(&svc, nowhere(), finishes(), later);

    pass(&svc).await;

    let cancelled = vault.job(cancelled.id).unwrap();
    assert_eq!(cancelled.status, JobStatus::Cancelled);
    assert_eq!(cancelled.reason, "they changed their mind");
    assert!(cancelled.updates.is_empty());
    assert_eq!(cancelled.observed.unwrap().checks, 0);

    let not_yet = vault.job(not_yet.id).unwrap();
    assert_eq!(not_yet.status, JobStatus::Waiting);
    assert_eq!(not_yet.wake_at, Some(later), "not touched");
    assert_eq!(not_yet.observed.unwrap().checks, 0);
}

#[tokio::test]
async fn with_the_assistant_off_the_watches_sleep() {
    let (svc, _dir) = service("http://127.0.0.1:1/v1");
    let vault = svc.get().unwrap();
    let watch = add(&svc, nowhere(), finishes(), Timestamp::now());
    let off = AgentSettings { enabled: false, ..vault.agent_settings().unwrap() };
    vault.save_agent_settings(&off).unwrap();

    pass(&svc).await;
    let job = vault.job(watch.id).unwrap();
    assert_eq!(job.observed.unwrap().checks, 0, "not looked at while it is off");
    assert_eq!(job.status, JobStatus::Waiting);

    // And at once when it is switched on.
    vault.save_agent_settings(&AgentSettings { enabled: true, ..off }).unwrap();
    pass(&svc).await;
    assert_eq!(vault.job(watch.id).unwrap().observed.unwrap().checks, 1);
}

// ---- a model that says what it is told ---------------------------------------

/// What the fake says.
#[derive(Clone)]
struct Script {
    /// What the assistant's look reports.
    look: String,
    /// What the judge decides: the arguments of its `submit` call.
    judgement: serde_json::Value,
}

struct FakeModel {
    endpoint: String,
    /// Assistant turns it has been asked for.
    looks: Arc<AtomicUsize>,
    /// Structured asks it has been asked for: judgements and updates.
    judgements: Arc<AtomicUsize>,
    _shutdown: tokio::sync::watch::Sender<bool>,
}

async fn fake_model(script: Script) -> FakeModel {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let looks = Arc::new(AtomicUsize::new(0));
    let judgements = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let (looks, judgements) = (looks.clone(), judgements.clone());
        async move {
            loop {
                let accepted = tokio::select! {
                    r = listener.accept() => r,
                    _ = rx.changed() => break,
                };
                let Ok((mut socket, _)) = accepted else { break };
                let (script, looks, judgements) =
                    (script.clone(), looks.clone(), judgements.clone());
                tokio::spawn(async move {
                    use tokio::io::AsyncWriteExt;
                    let request = read_request(&mut socket).await;
                    let response = if request.contains("\"name\":\"submit\"") {
                        judgements.fetch_add(1, Ordering::SeqCst);
                        // The judge's schema has a verdict; the one that only
                        // writes an update has an update and nothing else.
                        let arguments = if request.contains("\"verdict\"") {
                            script.judgement.to_string()
                        } else {
                            json!({ "update": script.look }).to_string()
                        };
                        let body = tool_call("submit", &arguments);
                        http("application/json", &body)
                    } else {
                        looks.fetch_add(1, Ordering::SeqCst);
                        http("text/event-stream", &stream(&script.look))
                    };
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                });
            }
        }
    });

    FakeModel { endpoint: format!("http://127.0.0.1:{port}/v1"), looks, judgements, _shutdown: tx }
}

/// The whole of one request, headers and body.
async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    use tokio::io::AsyncReadExt;
    let mut data: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        let n = socket.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&data).into_owned();
        if let Some(split) = text.find("\r\n\r\n") {
            let length = text[..split]
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    if name.trim().eq_ignore_ascii_case("content-length") {
                        value.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            if data.len() >= split + 4 + length {
                return text;
            }
        }
    }
    String::from_utf8_lossy(&data).into_owned()
}

fn http(content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nCache-Control: no-cache\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// A single Chat Completions document answering with a call to `tool`: what a
/// structured ask (`karmayoga_service::quick`) expects back.
fn tool_call(tool: &str, arguments: &str) -> String {
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
                    "function": { "name": tool, "arguments": arguments },
                }],
            },
            "finish_reason": "tool_calls",
        }],
        "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 },
    })
    .to_string()
}

/// One answer of prose, as a stream of Chat Completions chunks: what an
/// assistant's turn expects back.
fn stream(text: &str) -> String {
    let mut out = String::new();
    let mut frame = |delta: serde_json::Value, finish: Option<&str>| {
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

#[tokio::test]
async fn a_look_that_is_nothing_to_tell_is_kept_and_not_looked_at_again_when_the_same() {
    let model = fake_model(Script {
        look: "Build 412 is in progress.".into(),
        judgement: json!({ "verdict": "quiet", "update": "" }),
    })
    .await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let watch = add(&svc, by_looking(), finishes(), Timestamp::now());

    pass(&svc).await;

    let job = vault.job(watch.id).unwrap();
    assert_eq!(job.status, JobStatus::Waiting, "{job:?}");
    let observed = job.observed.clone().unwrap();
    assert_eq!(observed.last, "Build 412 is in progress.");
    assert!(!observed.fingerprint.is_empty());
    assert_eq!((observed.checks, observed.failures), (1, 0));
    assert!(observed.trouble.is_empty(), "{}", observed.trouble);
    assert!(job.updates.is_empty(), "nothing they asked to hear about");
    // A look by the assistant is at most every fifteen minutes.
    let wake = job.wake_at.expect("asleep until");
    assert!(wake > Timestamp::now() + SignedDuration::from_mins(14), "{wake}");
    assert_eq!(svc.running_job(), None);

    // It looked in a transcript of its own, kept out of the rail.
    let transcript = vault.conversation(job.conversation_id.expect("a transcript")).unwrap();
    assert_eq!(transcript.job_id, Some(job.id));
    assert!(!transcript.is_chat());
    // Requests, not asks: a structured ask is answered and then told its
    // answer was taken, which is a second request for the same judgement.
    let judged = model.judgements.load(Ordering::SeqCst);
    assert_eq!(model.looks.load(Ordering::SeqCst), 1);
    assert!(judged >= 1, "the first look is judged");
    let first_look: Vec<_> = vault.messages(transcript.id).unwrap().iter().map(|m| m.id).collect();
    assert!(!first_look.is_empty());

    // The same words again: the fingerprint says so, and no model is asked to
    // judge. The transcript keeps the look before.
    due_now(&svc, &job);
    pass(&svc).await;
    let job = vault.job(watch.id).unwrap();
    assert_eq!(job.observed.as_ref().unwrap().checks, 2);
    assert_eq!(model.looks.load(Ordering::SeqCst), 2);
    assert_eq!(model.judgements.load(Ordering::SeqCst), judged, "no model judged it again");
    let second: Vec<_> = vault.messages(transcript.id).unwrap().iter().map(|m| m.id).collect();
    assert!(first_look.iter().all(|id| second.contains(id)), "the last look is still there");

    // A third look takes the first out: only the last turn is kept.
    due_now(&svc, &job);
    pass(&svc).await;
    let third: Vec<_> = vault.messages(transcript.id).unwrap().iter().map(|m| m.id).collect();
    assert!(first_look.iter().all(|id| !third.contains(id)), "the oldest look is gone");
    assert!(second.iter().any(|id| third.contains(id)), "and the one before is kept");
    assert_eq!(vault.job(watch.id).unwrap().observed.unwrap().checks, 3);
}

#[tokio::test]
async fn a_look_that_is_the_end_of_it_tells_them_and_finishes_the_watch() {
    let model = fake_model(Script {
        look: "Build 412 finished: it passed.".into(),
        judgement: json!({ "verdict": "tell_and_stop", "update": "Build 412 finished: it passed." }),
    })
    .await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let watch = add(&svc, by_looking(), finishes(), Timestamp::now());

    pass(&svc).await;

    let job = vault.job(watch.id).unwrap();
    assert_eq!(job.status, JobStatus::Done, "{job:?}");
    assert_eq!(job.summary, "Build 412 finished: it passed.");
    assert_eq!(job.updates.len(), 1);
    assert_eq!(job.updates[0].text, "Build 412 finished: it passed.");
    assert!(job.updates[0].delivered);
    assert!(job.answered && !job.owes_answer(), "it said what it had to as it went");
    assert!(job.wake_at.is_none());
    assert_eq!(job.observed.unwrap().last, "Build 412 finished: it passed.");
    assert_eq!(svc.running_job(), None);
}

#[tokio::test]
async fn a_look_that_is_worth_telling_goes_on_watching() {
    let model = fake_model(Script {
        look: "Build 412 failed on lint.".into(),
        judgement: json!({ "verdict": "tell", "update": "Build 412 failed on lint." }),
    })
    .await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let watch = add(&svc, by_looking(), finishes(), Timestamp::now());

    pass(&svc).await;

    let job = vault.job(watch.id).unwrap();
    assert_eq!(job.status, JobStatus::Waiting, "an update is not the end");
    assert_eq!(job.updates.len(), 1);
    assert_eq!(job.last_update().unwrap().text, "Build 412 failed on lint.");
    assert!(job.wake_at.is_some());
}
