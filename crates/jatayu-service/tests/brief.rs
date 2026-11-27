//! A brief, end to end, against a fake model: the digest becomes the first
//! message, the turn is offered only tools that read, and what the model
//! wrote is taken apart and kept on the run. Uses `tests/dream.rs`'s harness.

use jatayu_core::brief::{BriefKind, DIGEST_MARKER};
use jatayu_core::routine::{Outcome, RoutineRun};
use jatayu_service::Service;
use std::sync::{Arc, Mutex};

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
    let mut task = jatayu_core::Task::new("Send the grant report");
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

    jatayu_service::scheduler::tick(&svc).await;

    let run = svc.get().unwrap().run(queued.id).unwrap();
    assert_eq!(run.outcome, Outcome::Done, "{}", run.reason);
    let brief = run.brief.as_deref().expect("the brief is kept on the run");
    assert_eq!(brief.headline, "A light day: **the grant report** is due.");
    assert_eq!(brief.parts.len(), 1);
    assert_eq!(brief.parts[0].section, Some(jatayu_core::brief::BriefSection::Tasks));
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

/// The first user message of the first request, whichever shape the content
/// came in.
fn first_user_message(model: &FakeModel) -> String {
    let requests = model.requests.lock().unwrap();
    requests[0]["messages"]
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
        .unwrap()
}

#[tokio::test]
async fn the_brief_is_asked_to_lead_with_a_bottom_line_and_not_to_repeat_itself_outside_the_fence()
{
    let _serial = SERIAL.lock().await;
    let model = fake_model(says(ANSWER)).await;
    let (svc, _dir) = service(&model.endpoint);
    let queued = queue_morning(&svc);

    jatayu_service::scheduler::tick(&svc).await;

    let run = svc.get().unwrap().run(queued.id).unwrap();
    assert_eq!(run.outcome, Outcome::Done, "{}", run.reason);
    let user = first_user_message(&model);
    let marker = user.find(DIGEST_MARKER).expect("the marker");
    let (own, digest) = user.split_at(marker);
    assert!(own.contains("<the bottom line: one to three short lines"), "{own}");
    assert!(own.contains("Judgement, not inventory."), "{own}");
    assert!(
        own.contains("Do not reuse the tidbit; repeat a point only if it still matters"),
        "{own}"
    );
    assert!(digest.contains("BEGIN TEXT WRITTEN BY SOMEBODY ELSE"), "{digest}");
    assert!(
        !digest.to_lowercase().contains("do not repeat"),
        "no instruction in the fence: {digest}"
    );
}

#[tokio::test]
async fn the_weekly_brief_asks_for_the_large_model_and_runs_on_the_medium_one_when_there_is_none() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(says(ANSWER)).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let routine = vault.set_brief(BriefKind::Weekly, true).unwrap();
    assert_eq!(routine.model, Some(jatayu_core::llm::ModelTier::Large));
    assert!(vault.model_settings().unwrap().large.is_none(), "no large model is set up");
    let run = RoutineRun::new(&routine, None);
    vault.save_run(&run).unwrap();

    jatayu_service::scheduler::tick(&svc).await;

    let run = vault.run(run.id).unwrap();
    assert_eq!(run.outcome, Outcome::Done, "it fell back to the medium model: {}", run.reason);
    let user = first_user_message(&model);
    assert!(user.contains("weekly brief"), "{user}");
    assert!(user.contains("the shape of the week and the one thing that matters most in it"));
}

#[tokio::test]
async fn a_brief_that_writes_nothing_fails_rather_than_keeping_an_empty_one() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(says("")).await;
    let (svc, _dir) = service(&model.endpoint);
    let queued = queue_morning(&svc);

    jatayu_service::scheduler::tick(&svc).await;

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

    jatayu_service::scheduler::tick(&svc).await;

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
    let today = jatayu_core::routine::Weekday::parse(&now.strftime("%a").to_string()).unwrap();

    // The weekly brief runs today; the morning brief is due right now.
    let mut weekly = vault.set_brief(BriefKind::Weekly, true).unwrap();
    weekly.trigger = jatayu_core::routine::Trigger::Schedule {
        at: jiff::civil::time(23, 59, 0, 0),
        days: vec![today],
        day_of_month: None,
    };
    vault.save_routine(&weekly).unwrap();
    let mut morning = vault.set_brief(BriefKind::Morning, true).unwrap();
    let then = now.timestamp() - jiff::SignedDuration::from_mins(5);
    let local = then.to_zoned(now.time_zone().clone());
    morning.trigger = jatayu_core::routine::Trigger::Schedule {
        at: jiff::civil::time(local.hour(), local.minute(), 0, 0),
        days: vec![],
        day_of_month: None,
    };
    morning.created_at = then - jiff::SignedDuration::from_hours(1);
    vault.save_routine(&morning).unwrap();

    jatayu_service::scheduler::tick(&svc).await;

    let runs =
        vault.runs(&jatayu_core::store::routines::RunQuery::for_routine(morning.id)).unwrap();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0].outcome, Outcome::Skipped);
    assert!(runs[0].reason.contains("weekly brief"), "{}", runs[0].reason);
    assert!(model.requests.lock().unwrap().is_empty(), "no model was asked");
}

/// The weekly brief and the morning brief, both on today, the morning one due
/// now. Hands back the morning brief's routine.
fn both_due_today(svc: &Arc<Service>) -> jatayu_core::Routine {
    let vault = svc.get().unwrap();
    let now = vault.agent_settings().unwrap().now();
    let today = jatayu_core::routine::Weekday::parse(&now.strftime("%a").to_string()).unwrap();
    let mut weekly = vault.set_brief(BriefKind::Weekly, true).unwrap();
    weekly.trigger = jatayu_core::routine::Trigger::Schedule {
        at: jiff::civil::time(0, 0, 0, 0),
        days: vec![today],
        day_of_month: None,
    };
    vault.save_routine(&weekly).unwrap();
    let mut morning = vault.set_brief(BriefKind::Morning, true).unwrap();
    let then = now.timestamp() - jiff::SignedDuration::from_mins(5);
    let local = then.to_zoned(now.time_zone().clone());
    morning.trigger = jatayu_core::routine::Trigger::Schedule {
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

    jatayu_service::scheduler::tick(&svc).await;

    let runs =
        vault.runs(&jatayu_core::store::routines::RunQuery::for_routine(morning.id)).unwrap();
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

    jatayu_service::scheduler::tick(&svc).await;

    let run = vault.run(queued.id).unwrap();
    assert_eq!(run.outcome, Outcome::Skipped);
    assert!(!run.reason.is_empty());
    assert!(model.requests.lock().unwrap().is_empty());
}

// ---- the daily podcast ------------------------------------------------------

/// A source with one story that arrived an hour ago, in the service's vault.
fn feed_with_a_story(svc: &Arc<Service>) {
    use jatayu_core::news::{BodyOrigin, NewsSource, SourceKind, Story, StoryBody};
    let vault = svc.get().unwrap();
    let source =
        NewsSource::new(SourceKind::Web { url: "https://blog.example/feed".into() }, "Blog");
    vault.save_news_source(&source).unwrap();
    let hour_ago =
        vault.agent_settings().unwrap().now().timestamp() - jiff::SignedDuration::from_hours(1);
    let mut story =
        Story::new(source.id, vault.news_key("blog/chips").unwrap(), "Chips are late", hour_ago);
    story.summary = "The fab is a year behind.".into();
    let body = StoryBody {
        story_id: story.id,
        html: "<p>x</p>".into(),
        text: "The fab says its new line will start a year late, after the tools arrived.".into(),
        origin: BodyOrigin::Extracted,
        images: Vec::new(),
    };
    vault.insert_stories(&[(story, Some(body))]).unwrap();
}

/// Switch the daily podcast on and ask for one now, as "Make one now" does.
fn queue_podcast(svc: &Arc<Service>) -> RoutineRun {
    let vault = svc.get().unwrap();
    let routine = vault.set_brief(BriefKind::Podcast, true).unwrap();
    let run = RoutineRun::new(&routine, None);
    vault.save_run(&run).unwrap();
    run
}

const PODCAST: &str = "# Chips are late, and not much else.\n\n\
    ## Chips\n- **Chips are late** (Blog): the fab is a year behind.\n\n\
    --- spoken ---\nGood morning, Hari. Chips lead today.\n\n\
    Blog says the new fab will start a year late.\n\nThat is all for today.";

/// The text of the first user message a request carried.
fn user_message(request: &serde_json::Value) -> String {
    request["messages"]
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
        .unwrap()
}

#[tokio::test]
async fn a_daily_podcast_is_written_from_the_feed_and_kept_as_a_script() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(says(PODCAST)).await;
    let (svc, _dir) = service(&model.endpoint);
    feed_with_a_story(&svc);
    let queued = queue_podcast(&svc);

    jatayu_service::scheduler::tick(&svc).await;
    jatayu_service::scheduler::settle(&svc).await;

    let run = svc.get().unwrap().run(queued.id).unwrap();
    assert_eq!(run.outcome, Outcome::Done, "{}", run.reason);
    let brief = run.brief.as_deref().expect("the podcast is kept on the run");
    assert_eq!(brief.kind, BriefKind::Podcast);
    assert_eq!(brief.headline, "Chips are late, and not much else.");
    assert_eq!(brief.parts.len(), 1);
    assert_eq!(brief.parts[0].section, None, "a topic, not one of the day's sections");
    // The spoken half is a script of one narrator, a line a paragraph, and a
    // call or Listen says its words.
    let script = brief.script.as_ref().expect("a script");
    assert_eq!(script.title, format!("{} Daily podcast", brief.day));
    assert_eq!(script.lines.len(), 3);
    assert!(script.lines.iter().all(|l| l.speaker == jatayu_core::news::script::NARRATOR));
    assert_eq!(brief.spoken, script.text());
    assert!(brief.spoken.starts_with("Good morning, Hari."));
    // What it was written from is kept, for the next podcast to leave out.
    let digest = brief.digest.podcast.as_ref().expect("its digest");
    assert_eq!(digest.stories.len(), 1);
    assert_eq!(digest.stories[0].title, "Chips are late");
    assert!(digest.stories[0].excerpt.starts_with("The fab says"));
    // The chat and the email get the written podcast, without a section mark.
    assert!(run.summary.starts_with("# Chips are late"));
    assert!(!run.summary.contains("spoken"));

    // No voice is set up in a test: the written podcast stands, and the run
    // says why there is no audio.
    assert!(brief.audio.is_none());
    assert!(run.reason.starts_with("the audio could not be made: "), "{}", run.reason);

    let requests = model.requests.lock().unwrap();
    let user = user_message(&requests[0]);
    assert!(user.contains(DIGEST_MARKER), "{user}");
    assert!(user.contains("BEGIN TEXT WRITTEN BY SOMEBODY ELSE"), "the stories are fenced");
    assert!(user.contains("daily podcast"), "{user}");
    assert!(user.contains("2250 words"), "{user}");
    assert!(user.contains("Stories to cover (1)"), "{user}");
    assert!(user.contains("The fab says its new line will start a year late"), "{user}");
    // Read-only, like every brief.
    let offered: Vec<String> = requests[0]["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for write in ["create_task", "send_draft", "web_search"] {
        assert!(!offered.iter().any(|t| t == write), "{write} was offered: {offered:?}");
    }
}

#[tokio::test]
async fn a_daily_podcast_with_nothing_new_is_skipped_and_says_why() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(says(PODCAST)).await;
    let (svc, _dir) = service(&model.endpoint);
    let queued = queue_podcast(&svc);

    jatayu_service::scheduler::tick(&svc).await;

    let run = svc.get().unwrap().run(queued.id).unwrap();
    assert_eq!(run.outcome, Outcome::Skipped);
    assert!(run.reason.contains("nothing new"), "{}", run.reason);
    assert!(run.brief.is_none());
    assert!(model.requests.lock().unwrap().is_empty(), "no model was asked to write about nothing");
}

#[tokio::test]
async fn a_podcast_for_two_speakers_is_a_conversation() {
    let _serial = SERIAL.lock().await;
    let model = fake_model(says(
        "# Chips.\n\n## Chips\n- **Chips are late** (Blog): late.\n\n--- spoken ---\n\
         MAYA: Welcome. I'm Maya.\n\nLEO: And I'm Leo. The fab is late.\n\nMAYA: A year.",
    ))
    .await;
    let (svc, _dir) = service(&model.endpoint);
    feed_with_a_story(&svc);
    let vault = svc.get().unwrap();
    let mut routine = vault.set_brief(BriefKind::Podcast, true).unwrap();
    let mut spec = routine.kind.brief().unwrap().clone();
    let mut maya = jatayu_core::news::script::ScriptSpeaker::new("maya", "Maya");
    maya.voice = Some("alba".into());
    spec.podcast.as_mut().unwrap().speakers =
        vec![maya, jatayu_core::news::script::ScriptSpeaker::new("leo", "Leo")];
    routine.kind = jatayu_core::routine::RoutineKind::Brief(spec);
    vault.save_routine(&routine).unwrap();
    let run = RoutineRun::new(&routine, None);
    vault.save_run(&run).unwrap();

    jatayu_service::scheduler::tick(&svc).await;
    jatayu_service::scheduler::settle(&svc).await;

    let run = vault.run(run.id).unwrap();
    assert_eq!(run.outcome, Outcome::Done, "{}", run.reason);
    let script = run.brief.as_deref().unwrap().script.as_ref().unwrap();
    let said: Vec<_> = script.lines.iter().map(|l| l.speaker.as_str()).collect();
    assert_eq!(said, vec!["maya", "leo", "maya"]);
    assert_eq!(script.speaker("maya").unwrap().voice.as_deref(), Some("alba"));
    assert_eq!(script.lines[1].text, "And I'm Leo. The fab is late.");
    let user = user_message(&model.requests.lock().unwrap()[0]);
    assert!(user.contains("a conversation between Maya, Leo"), "{user}");
}
