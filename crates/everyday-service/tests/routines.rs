//! The scheduler, end to end, against a fake model.
//!
//! The thing that has been untestable until now: whether a routine actually
//! runs, what happens when the model asks to delete something with nobody
//! there, and what a wedged provider does to the loop.
//!
//! The model is a `TcpListener` on loopback speaking the Chat Completions
//! shape, which is all the agent harness needs -- the base URL is a setting,
//! precisely so that a local model works, and a scripted one is a local model
//! that always says the same thing. No key, no network, no bill.

use everyday_core::routine::{Outcome, Routine, Trigger, Weekday};
use everyday_core::store::routines::RunQuery;
use everyday_service::Service;
use everyday_service::events::{Change, EventSink, Notification};
use std::sync::{Arc, Mutex};

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

    let mut settings = everyday_core::AgentSettings {
        enabled: true,
        // Everything is reckoned in UTC so the tests are about the scheduler
        // rather than about where the machine running them is.
        timezone: Some("UTC".into()),
        ..Default::default()
    };
    settings.provider_config.base_url = Some(endpoint.to_string());
    settings.assistant_model.model = "scripted".into();
    vault.save_agent_settings(&settings).unwrap();

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
    /// Accept the connection and never answer. What a wedged provider is.
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
    _shutdown: tokio::sync::watch::Sender<bool>,
}

async fn fake_model(script: Script) -> FakeModel {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        // One reply per connection, and the tool answer is the *second* one:
        // rig asks again with the tool's result, and a model that repeated its
        // request would loop until `max_steps`.
        let mut asked = 0usize;
        loop {
            let accepted = tokio::select! {
                r = listener.accept() => r,
                _ = rx.changed() => break,
            };
            let Ok((mut socket, _)) = accepted else { break };
            let script = script.clone();
            asked += 1;
            let turn = asked;
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 64 * 1024];
                let _ = socket.read(&mut buf).await;
                if matches!(script, Script::Hangs) {
                    // Held open, saying nothing, until the test gives up on it.
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
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

/// One answer, as a stream of Chat Completions chunks.
///
/// The harness streams every turn, so this has to be `text/event-stream` with
/// `data:` frames rather than one JSON document -- a plain body is refused on
/// its content type before anything reads it.
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

/// A routine whose moment was a minute ago, so one tick runs it.
fn due_now(svc: &Arc<Service>, instructions: &str) -> Routine {
    let vault = svc.get().unwrap();
    let now = vault.agent_settings().unwrap().now();
    let then = now.timestamp() - jiff::SignedDuration::from_mins(1);
    let local = then.to_zoned(now.time_zone().clone());
    let mut routine = Routine::new(
        "Morning brief",
        instructions,
        Trigger::Schedule {
            at: jiff::civil::time(local.hour(), local.minute(), 0, 0),
            days: vec![],
        },
    );
    // Made before its moment, or the slot would predate the routine and be
    // nothing it missed. See `Routine::is_due`.
    routine.created_at = then - jiff::SignedDuration::from_hours(1);
    vault.save_routine(&routine).unwrap();
    routine
}

#[tokio::test]
async fn a_due_routine_runs_and_records_what_the_model_said() {
    let model = fake_model(says("Three things are due and one is overdue.")).await;
    let (svc, _dir) = service(&model.endpoint);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let routine = due_now(&svc, "Say what is due today.");

    everyday_service::scheduler::tick(&svc).await;

    let vault = svc.get().unwrap();
    let runs = vault.runs(&RunQuery::for_routine(routine.id)).unwrap();
    assert_eq!(runs.len(), 1, "one tick, one run");
    let run = &runs[0];
    assert_eq!(run.outcome, Outcome::Done);
    assert_eq!(run.summary, "Three things are due and one is overdue.");
    assert!(run.conversation_id.is_some(), "the transcript is kept");
    assert!(!run.seen, "and it is waiting to be looked at");
    assert_eq!(vault.unseen_runs().unwrap(), 1);

    // Stamped, so the same moment is not claimed again on the next tick.
    assert_eq!(vault.routine(routine.id).unwrap().last_run_at, run.slot);
    everyday_service::scheduler::tick(&svc).await;
    assert_eq!(vault.runs(&RunQuery::default()).unwrap().len(), 1, "one run per slot");

    // The change event says who wrote it, so a window draws it instead of
    // mistaking it for its own.
    let changes = events.changes.lock().unwrap();
    assert!(
        changes.iter().any(|c| c.origin.as_deref() == Some("assistant")),
        "a routine's writes are stamped `assistant`"
    );

    // And one notification, naming the routine and nothing it found: this is
    // the one notice in the application that will be drawn over a lock screen.
    let notices = events.notices.lock().unwrap();
    let notice = notices.iter().find(|n| n.title.contains("Morning brief")).expect("one notice");
    assert_eq!(notice.title, "Morning brief is ready");
    assert!(notice.body.is_none(), "the content stays in the run");
}

#[tokio::test]
async fn a_run_can_write_a_note_which_is_where_its_prose_goes() {
    let model = fake_model(calls(
        "create_note",
        r#"{"title":"Brief for Tuesday","body":"Three things are due."}"#.to_string(),
        "I left it in a note.",
    ))
    .await;
    let (svc, _dir) = service(&model.endpoint);
    due_now(&svc, "Write me a brief.");

    everyday_service::scheduler::tick(&svc).await;

    let vault = svc.get().unwrap();
    let notes = vault.notes(&Default::default()).unwrap();
    assert_eq!(notes.len(), 1, "the note it wrote is an ordinary note");
    assert_eq!(notes[0].title, "Brief for Tuesday");

    let run = &vault.runs(&RunQuery::default()).unwrap()[0];
    assert_eq!(run.outcome, Outcome::Done);
    assert_eq!(run.steps, 1, "one tool call");
}

#[tokio::test]
async fn a_scheduled_run_may_not_delete_anything() {
    // The gate's whole point with nobody watching. Refused on the spot rather
    // than parked on a question nobody will ever see -- and the run still
    // finishes, because the model is told plainly and can say so in its report.
    //
    // The endpoint is pointed at the model *after* the vault exists, because
    // the script has to name an id from it.
    let (svc, _dir) = service("http://127.0.0.1:1/v1");
    let vault = svc.get().unwrap();
    let journal = vault.journals().unwrap()[0].id;
    let mut entry = everyday_core::Entry::new(journal, "UTC");
    entry.title = "Do not delete me".into();
    vault.save_entry(&entry, None).unwrap();

    let model = fake_model(calls(
        "delete_entry",
        serde_json::json!({ "entry_id": entry.id.to_string() }).to_string(),
        "It needs deleting and I could not do it.",
    ))
    .await;
    let mut settings = vault.agent_settings().unwrap();
    settings.provider_config.base_url = Some(model.endpoint.clone());
    vault.save_agent_settings(&settings).unwrap();
    due_now(&svc, "Tidy up.");

    everyday_service::scheduler::tick(&svc).await;

    assert!(vault.entry(entry.id).is_ok(), "nothing may be deleted with nobody watching");
    let run = &vault.runs(&RunQuery::default()).unwrap()[0];
    assert_eq!(run.outcome, Outcome::Done, "the run still finishes and reports");
    assert_eq!(run.summary, "It needs deleting and I could not do it.");
}

#[tokio::test]
async fn a_locked_vault_runs_nothing_at_all() {
    let model = fake_model(says("should never be said")).await;
    let (svc, _dir) = service(&model.endpoint);
    due_now(&svc, "Say what is due.");
    let vault = svc.get().unwrap();
    vault.lock();

    everyday_service::scheduler::tick(&svc).await;

    // Not even a skipped row: a vault locked at seven and unlocked at nine
    // should produce one honest "missed" when somebody is there to be told.
    vault.unlock(None).unwrap();
    assert!(vault.runs(&RunQuery::default()).unwrap().is_empty(), "nothing was written");
}

#[tokio::test]
async fn a_routine_whose_assistant_is_switched_off_is_skipped_with_the_reason() {
    let model = fake_model(says("should never be said")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let mut settings = vault.agent_settings().unwrap();
    settings.enabled = false;
    vault.save_agent_settings(&settings).unwrap();
    due_now(&svc, "Say what is due.");

    everyday_service::scheduler::tick(&svc).await;

    let run = &vault.runs(&RunQuery::default()).unwrap()[0];
    assert_eq!(run.outcome, Outcome::Skipped);
    assert!(!run.reason.is_empty(), "the reason is the useful part: {run:?}");
    assert!(run.seen, "a skip asks for no attention on the app bar");
    assert_eq!(vault.unseen_runs().unwrap(), 0);
}

#[tokio::test]
async fn a_moment_missed_by_more_than_its_grace_is_recorded_rather_than_run_late() {
    let model = fake_model(says("should never be said")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let now = vault.agent_settings().unwrap().now();

    // Seven hours ago, with an hour's grace.
    let then = now.timestamp() - jiff::SignedDuration::from_hours(7);
    let local = then.to_zoned(now.time_zone().clone());
    let mut routine = Routine::new(
        "Morning brief",
        "Say what is due.",
        Trigger::Schedule {
            at: jiff::civil::time(local.hour(), local.minute(), 0, 0),
            days: vec![],
        },
    );
    routine.created_at = then - jiff::SignedDuration::from_hours(1);
    vault.save_routine(&routine).unwrap();

    everyday_service::scheduler::tick(&svc).await;

    let run = &vault.runs(&RunQuery::default()).unwrap()[0];
    assert_eq!(run.outcome, Outcome::Skipped, "a morning brief at two is not a morning brief");
    assert!(run.reason.contains("past the 60 minutes"), "got {:?}", run.reason);

    // And the moment is stamped, or it is reported missed on every tick for
    // the rest of the day.
    assert_eq!(vault.routine(routine.id).unwrap().last_run_at, run.slot);
    everyday_service::scheduler::tick(&svc).await;
    assert_eq!(vault.runs(&RunQuery::default()).unwrap().len(), 1, "said once");
}

#[tokio::test]
async fn a_routine_that_is_switched_off_never_runs() {
    let model = fake_model(says("should never be said")).await;
    let (svc, _dir) = service(&model.endpoint);
    let routine = due_now(&svc, "Say what is due.");
    let vault = svc.get().unwrap();
    let mut off = routine.clone();
    off.enabled = false;
    vault.save_routine(&off).unwrap();

    everyday_service::scheduler::tick(&svc).await;
    assert!(vault.runs(&RunQuery::default()).unwrap().is_empty());
}

#[tokio::test]
async fn run_now_is_queued_and_carried_out_by_the_next_tick() {
    let model = fake_model(says("Done as asked.")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    // A manual routine: never due on the clock, so only "run now" starts it.
    let routine = Routine::new("On demand", "Do the thing.", Trigger::Manual);
    vault.save_routine(&routine).unwrap();

    let queued = svc
        .call(
            everyday_service::ctx::Ctx::local(),
            "run_routine",
            serde_json::json!({ "id": routine.id.to_string() }),
        )
        .await
        .expect("run_routine");
    assert_eq!(queued["outcome"], "queued", "the command queues rather than runs");

    // And pressing it twice does not pay for two model calls.
    svc.call(
        everyday_service::ctx::Ctx::local(),
        "run_routine",
        serde_json::json!({ "id": routine.id.to_string() }),
    )
    .await
    .expect("run_routine again");
    assert_eq!(vault.runs(&RunQuery::default()).unwrap().len(), 1, "one queued run, not two");

    everyday_service::scheduler::tick(&svc).await;
    let run = &vault.runs(&RunQuery::default()).unwrap()[0];
    assert_eq!(run.outcome, Outcome::Done);
    assert_eq!(run.summary, "Done as asked.");
}

#[tokio::test]
async fn a_model_that_never_answers_fails_the_run_rather_than_the_loop() {
    let model = fake_model(Script::Hangs).await;
    let (svc, _dir) = service(&model.endpoint);
    due_now(&svc, "Say what is due.");

    // The real timeout is fifteen minutes, which no test may wait for. Time is
    // paused, so the sleep inside the scheduler's timeout is skipped forward
    // by tokio rather than actually slept.
    tokio::time::pause();
    let ticked = tokio::spawn({
        let svc = svc.clone();
        async move { everyday_service::scheduler::tick(&svc).await }
    });
    // Let the run reach the model, then jump past the timeout.
    tokio::time::advance(std::time::Duration::from_millis(50)).await;
    tokio::time::advance(std::time::Duration::from_secs(16 * 60)).await;
    ticked.await.expect("the tick must finish");
    tokio::time::resume();

    let vault = svc.get().unwrap();
    let run = &vault.runs(&RunQuery::default()).unwrap()[0];
    assert_eq!(run.outcome, Outcome::Failed, "a wedged provider ends the run");
    assert!(run.reason.contains("was stopped"), "got {:?}", run.reason);
}

#[tokio::test]
async fn a_routine_the_assistant_was_asked_to_make_is_a_routine() {
    // "Every Sunday evening, plan my week", typed into the rail.
    let model = fake_model(calls("create_routine", r#"{"name":"Weekly plan","instructions":"Plan the week ahead.","at":"19:00","days":["sun"]}"#.to_string(), "Set up for Sunday evenings."))
    .await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();

    let conversation = everyday_core::Conversation::new();
    vault.save_conversation(&conversation).unwrap();
    svc.send_message(
        everyday_service::ctx::Ctx::local(),
        serde_json::json!({
            "conversationId": conversation.id.to_string(),
            "prompt": "Every Sunday evening, plan my week",
        }),
        Arc::new(|_| {}),
    )
    .await
    .expect("send_message");

    let routines = vault.routines().unwrap();
    assert_eq!(routines.len(), 1, "the assistant set it up itself");
    assert_eq!(routines[0].name, "Weekly plan");
    assert_eq!(
        routines[0].trigger,
        Trigger::Schedule { at: jiff::civil::time(19, 0, 0, 0), days: vec![Weekday::Sun] }
    );
}

#[tokio::test]
async fn a_scheduled_run_may_not_make_more_routines() {
    // The one recursion refused: a routine that makes routines, every morning,
    // is a way to wake up owning forty of them that nobody asked for.
    let model = fake_model(calls(
        "create_routine",
        r#"{"name":"Another","instructions":"And another.","at":"08:00"}"#.to_string(),
        "I could not set that up.",
    ))
    .await;
    let (svc, _dir) = service(&model.endpoint);
    due_now(&svc, "Set up more routines.");

    everyday_service::scheduler::tick(&svc).await;

    let vault = svc.get().unwrap();
    assert_eq!(vault.routines().unwrap().len(), 1, "only the one that was already there");
}

// ── the triggers that are questions rather than moments ─────────────────

/// A calendar with one event on it, starting `minutes` from now.
fn a_meeting(svc: &Arc<Service>, minutes: i64, title: &str) -> everyday_core::Event {
    let vault = svc.get().unwrap();
    let calendar = everyday_core::Calendar::subscribed("Work", "https://example.com/f.ics");
    vault.save_calendar(&calendar).unwrap();

    let start = jiff::Timestamp::now() + jiff::SignedDuration::from_mins(minutes);
    let zone = jiff::tz::TimeZone::UTC;
    let event = everyday_core::Event {
        id: everyday_core::EventId::new(),
        calendar_id: calendar.id,
        uid: format!("{title}@test"),
        title: title.into(),
        description: String::new(),
        location: "The blue room".into(),
        start,
        end: start + jiff::SignedDuration::from_mins(30),
        local_date: start.to_zoned(zone.clone()).date(),
        end_date: start.to_zoned(zone).date(),
        tz: "UTC".into(),
        all_day: false,
        status: everyday_core::EventStatus::Confirmed,
        organizer: "Priya Raman".into(),
        attendees: vec!["Sam Weatherby".into()],
        url: String::new(),
        busy: true,
        updated_at: jiff::Timestamp::now(),
    };
    // Through the store rather than a vault method: writing events wholesale
    // is what a feed sync does, and there is no public path to it that does
    // not also want a `.ics` to parse.
    vault
        .with_store(|store| {
            store.calendars().unwrap().replace_events(calendar.id, std::slice::from_ref(&event))
        })
        .unwrap();
    event
}

#[tokio::test]
async fn a_meeting_inside_the_window_is_prepared_for_once() {
    let model = fake_model(says("Priya called it; you last spoke in March.")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    if !vault.supports_calendars() {
        return;
    }
    let meeting = a_meeting(&svc, 30, "Quarterly review");

    let mut routine = Routine::new(
        "Meeting prep",
        "Say who is coming and what we last said.",
        Trigger::BeforeEvent { lead_minutes: 60, role_id: None },
    );
    routine.created_at = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(1);
    vault.save_routine(&routine).unwrap();

    everyday_service::scheduler::tick(&svc).await;

    let runs = vault.runs(&RunQuery::default()).unwrap();
    assert_eq!(runs.len(), 1, "one meeting, one run");
    assert_eq!(runs[0].outcome, Outcome::Done);
    assert_eq!(
        runs[0].subject.as_deref(),
        Some(meeting.id.to_string().as_str()),
        "the run says which meeting it was about, which is what stops a second one"
    );
    assert_eq!(runs[0].routine_name, "Meeting prep \u{2014} Quarterly review");

    // A minute later the same meeting is still in the window, and must not be
    // prepared for again.
    everyday_service::scheduler::tick(&svc).await;
    assert_eq!(vault.runs(&RunQuery::default()).unwrap().len(), 1, "once per meeting");
}

#[tokio::test]
async fn a_meeting_outside_the_window_is_left_alone() {
    let model = fake_model(says("should never be said")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    if !vault.supports_calendars() {
        return;
    }
    // Six hours out, with an hour's lead. Not yet.
    a_meeting(&svc, 6 * 60, "Later today");

    let mut routine = Routine::new(
        "Meeting prep",
        "Say who is coming.",
        Trigger::BeforeEvent { lead_minutes: 60, role_id: None },
    );
    routine.created_at = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(1);
    vault.save_routine(&routine).unwrap();

    everyday_service::scheduler::tick(&svc).await;
    assert!(vault.runs(&RunQuery::default()).unwrap().is_empty(), "an hour before means an hour");
}

#[tokio::test]
async fn the_web_tool_is_absent_until_somebody_turns_it_on() {
    // Not a scheduler test: a check that the one tool which leaves the machine
    // is not offered by default. Driven through `list_tools`, which is what
    // the palette and the CLI read -- and which deliberately does *not* list
    // it, because it is the service's rather than the core catalogue's.
    let model = fake_model(says("nothing")).await;
    let (svc, _dir) = service(&model.endpoint);
    let listed = svc
        .call(everyday_service::ctx::Ctx::local(), "list_tools", serde_json::json!({}))
        .await
        .expect("list_tools");
    let names: Vec<&str> =
        listed.as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(!names.contains(&"web_search"), "the core catalogue holds no tool that opens a socket");

    let vault = svc.get().unwrap();
    assert!(!vault.agent_settings().unwrap().web, "and the switch is off in a new vault");
}

#[tokio::test]
async fn a_run_left_behind_by_a_dead_process_is_closed_rather_than_left_spinning() {
    // Quitting mid-run, or a machine going to sleep, leaves a row saying
    // `Running` that nothing will ever finish. Until it was swept it sat on
    // the app bar as an unseen run that was perpetually about to arrive.
    let model = fake_model(says("should never be said")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let routine = Routine::new("Brief", "Say what is due.", Trigger::Manual);
    vault.save_routine(&routine).unwrap();

    // What a previous process left: started, never finished, not claimed by
    // anybody now alive.
    let mut orphan = everyday_core::RoutineRun::new(&routine, Some(jiff::Timestamp::now()));
    orphan.begin();
    vault.save_run(&orphan).unwrap();

    everyday_service::scheduler::tick(&svc).await;

    let swept = vault.run(orphan.id).unwrap();
    assert_eq!(swept.outcome, Outcome::Failed, "it cannot be resumed, so it is closed");
    assert!(swept.reason.contains("application stopped"), "got {:?}", swept.reason);

    // And its moment is stamped, or the next tick reports it missed as well.
    assert_eq!(vault.routine(routine.id).unwrap().last_run_at, orphan.slot);
}

#[tokio::test]
async fn a_routine_switched_off_while_it_ran_stays_off() {
    // `stamp` used to write back the copy the run started with, so an edit
    // made during a run -- which may take a quarter of an hour -- was
    // silently reverted the moment the run finished.
    let model = fake_model(says("Done.")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let routine = due_now(&svc, "Say what is due.");

    // Edited between the snapshot and the stamp. Doing it for real would need
    // a hook inside the turn; writing it here is the same race with the same
    // result, because `stamp` runs after `run_turn` returns either way.
    let mut off = vault.routine(routine.id).unwrap();
    off.enabled = false;
    off.instructions = "Changed my mind.".into();
    vault.save_routine(&off).unwrap();

    everyday_service::scheduler::tick(&svc).await;

    let after = vault.routine(routine.id).unwrap();
    assert!(!after.enabled, "an edit made while it ran must not be undone by the stamp");
    assert_eq!(after.instructions, "Changed my mind.");
}

#[tokio::test]
async fn a_routine_deleted_while_it_ran_stays_deleted() {
    let model = fake_model(says("Done.")).await;
    let (svc, _dir) = service(&model.endpoint);
    let vault = svc.get().unwrap();
    let routine = Routine::new("On demand", "Do the thing.", Trigger::Manual);
    vault.save_routine(&routine).unwrap();

    // A run claimed by this process, whose routine goes away underneath it.
    let mut run = everyday_core::RoutineRun::new(&routine, Some(jiff::Timestamp::now()));
    run.begin();
    vault.save_run(&run).unwrap();
    let claim = svc.claim_run(run.id.to_string());
    vault.delete_routine(routine.id).unwrap();

    everyday_service::scheduler::tick(&svc).await;
    drop(claim);

    assert!(
        vault.routine(routine.id).is_err(),
        "the stamp must not resurrect a routine that was deleted while it ran"
    );
}
