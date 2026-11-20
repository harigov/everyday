//! The relay, end to end, against a fake chat service and a fake model.
//!
//! What the pure functions in `relay.rs` cannot say: that the pieces are wired
//! together the way the design describes. A message becomes one reply; a
//! destructive call stops to ask, and the answer -- a tap, or typed words --
//! decides what happens to the thing it was about; `!new`, `!stop` and a
//! message too old to act on do what they say. The model is a `TcpListener` on
//! loopback speaking the Chat Completions shape, the way
//! `karmayoga-service/tests/routines.rs` does it; the chat service is a
//! [`support::FakeConnector`] that writes down what it is asked to do.
//!
//! The second half is `run_link_with`: one link's life around the relay --
//! what the connector is given, what each way of ending means to the
//! supervisor, and what the settings pane is told.

#[allow(dead_code)]
mod support;

use jiff::{SignedDuration, Timestamp};
use karmayoga_chat::connector::{ChatError, Choice, Connector, Incoming, Picture, Setup};
use karmayoga_chat::relay::Timing;
use karmayoga_chat::run_link_with;
use karmayoga_core::agent::Role;
use karmayoga_core::chat::{ChatSecret, ChatSettings, Via};
use karmayoga_service::chat::{ChatState, FromRun};
use karmayoga_service::supervisor::Outcome;
use std::sync::Arc;
use std::time::Duration;
use support::{
    Event, FakeConnector, Harness, deleting, fake_model, matrix_link, room, says, world,
};
use tokio::sync::{oneshot, watch};

/// The via every conversation these tests start should carry.
fn via_of(harness: &Harness) -> Via {
    Via { link: harness.link.id, service: "Matrix".into(), chat: room().0 }
}

fn typing_on(events: &[Event]) -> bool {
    events.iter().any(|e| matches!(e, Event::Typing { on: true, .. }))
}

fn typing_off(events: &[Event]) -> bool {
    events.iter().any(|e| matches!(e, Event::Typing { on: false, .. }))
}

// ---- a plain reply -----------------------------------------------------------

#[tokio::test]
async fn a_message_gets_one_reply_while_the_chat_shows_typing() {
    let mut h = Harness::start(|_| says("Three things are due."), Timing::default()).await;

    h.say("what's due?").await;
    h.connector.wait_for_sent(1).await;
    h.finish().await;

    assert_eq!(
        h.connector.events(),
        vec![
            Event::Typing { chat: room(), on: true },
            Event::Typing { chat: room(), on: false },
            Event::Sent { chat: room(), text: "Three things are due.".into() },
        ],
        "typing on, typing off, then exactly one message"
    );

    let conversations = h.conversations();
    assert_eq!(conversations.len(), 1);
    assert_eq!(
        conversations[0].via,
        Some(via_of(&h)),
        "the conversation says which chat it arrived through"
    );
}

/// A drawing the assistant makes while answering is sent as a picture the
/// moment it is made, ahead of the reply that says what it is.
#[tokio::test]
async fn a_drawing_goes_to_the_chat_as_soon_as_it_is_made() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="60">
        <rect width="120" height="60" fill="#22c55e"/></svg>"##;
    let arguments = serde_json::json!({
        "format": "svg",
        "source": svg,
        "title": "Green box",
        "description": "A green box.",
    })
    .to_string();
    let mut h = Harness::start(
        |_| support::calls("draw_illustration", arguments, "Here is a green box."),
        Timing::default(),
    )
    .await;

    h.say("draw me a green box").await;
    h.connector
        .until("the picture", |events| events.iter().any(|e| matches!(e, Event::Showed { .. })))
        .await;
    h.finish().await;

    let after_typing: Vec<Event> =
        h.connector.events().into_iter().filter(|e| !matches!(e, Event::Typing { .. })).collect();
    let [Event::Showed { chat, shown }, Event::Sent { text, .. }] = after_typing.as_slice() else {
        panic!("the picture, then the reply: {after_typing:?}");
    };
    assert_eq!(chat, &room());
    // Named for its title -- with a " (2)" when an earlier run left one of
    // that name in the test folder, which every test shares.
    assert!(
        shown.starts_with("Green box") && shown.ends_with(".png image/png 120\u{d7}60"),
        "{shown}"
    );
    assert_eq!(text, "Here is a green box.");
}

#[tokio::test]
async fn a_second_message_continues_the_same_conversation() {
    let mut h = Harness::start(|_| says("Noted."), Timing::default()).await;

    h.say("one").await;
    h.connector.wait_for_sent(1).await;
    h.say("two").await;
    h.connector.wait_for_sent(2).await;
    h.finish().await;

    let conversations = h.conversations();
    assert_eq!(conversations.len(), 1, "two messages in a morning are one conversation");
    let said: Vec<String> = h
        .vault
        .messages(conversations[0].id)
        .unwrap()
        .into_iter()
        .filter(|m| m.role == Role::User)
        .map(|m| m.content)
        .collect();
    assert_eq!(said, vec!["one", "two"]);
}

#[tokio::test]
async fn a_failed_send_is_logged_and_the_relay_carries_on() {
    let mut h = Harness::start(|_| says("Here you are."), Timing::default()).await;

    h.connector.fail_next_sends(1);
    h.say("first").await;
    h.connector.until("the first reply to be tried", |_| h.connector.send_attempts() >= 1).await;

    h.say("second").await;
    h.connector.wait_for_sent(1).await;
    h.finish().await;

    assert_eq!(h.connector.sent(), vec!["Here you are."], "only the second reply got through");
    assert_eq!(h.connector.send_attempts(), 2);
}

// ---- confirmations -----------------------------------------------------------

#[tokio::test]
async fn a_delete_asks_first_and_a_tap_on_yes_does_it() {
    let mut h = Harness::start(|task| deleting(task, "Deleted it."), Timing::default()).await;

    h.say("delete the dentist task").await;
    let (question, reference) = h.connector.wait_for_question().await;

    assert!(
        question.markdown.contains("Book the dentist"),
        "the question names the task: {}",
        question.markdown
    );
    assert!(question.choices.starts_with(&[Choice::Yes, Choice::No]), "{:?}", question.choices);
    assert!(h.vault.task(h.task.id).is_ok(), "nothing is deleted before the answer");

    h.choose(&reference, Choice::Yes).await;
    h.connector.wait_for_sent(1).await;
    h.finish().await;

    assert!(h.vault.task(h.task.id).is_err(), "the task was deleted");
    assert_eq!(h.connector.sent(), vec!["Deleted it."]);
    assert_eq!(h.connector.questions().len(), 1);
    assert_eq!(
        h.connector.typing(),
        vec![true, false, true, false],
        "nobody is shown typing while the bot waits to be told"
    );
}

#[tokio::test]
async fn a_typed_yes_does_it_too() {
    let mut h = Harness::start(|task| deleting(task, "Deleted it."), Timing::default()).await;

    h.say("delete the dentist task").await;
    h.connector.wait_for_question().await;
    h.say("Yes.").await;
    h.connector.wait_for_sent(1).await;
    h.finish().await;

    assert!(h.vault.task(h.task.id).is_err(), "the task was deleted");
    assert_eq!(h.connector.sent(), vec!["Deleted it."]);
}

#[tokio::test]
async fn a_typed_no_leaves_the_task_alone_and_is_not_run_as_a_message() {
    let mut h =
        Harness::start(|task| deleting(task, "Understood, I left it."), Timing::default()).await;

    h.say("delete the dentist task").await;
    h.connector.wait_for_question().await;
    h.say("no").await;
    h.connector.wait_for_sent(1).await;
    h.finish().await;

    assert!(h.vault.task(h.task.id).is_ok(), "the task is still there");
    assert_eq!(h.connector.sent(), vec!["Understood, I left it."], "one reply, and no more");

    let conversations = h.conversations();
    assert_eq!(conversations.len(), 1);
    let said: Vec<String> = h
        .vault
        .messages(conversations[0].id)
        .unwrap()
        .into_iter()
        .filter(|m| m.role == Role::User)
        .map(|m| m.content)
        .collect();
    assert_eq!(said, vec!["delete the dentist task"], "the \"no\" was an answer, not a message");
}

#[tokio::test]
async fn a_tap_on_no_declines_too() {
    let mut h = Harness::start(|task| deleting(task, "Left it."), Timing::default()).await;

    h.say("delete the dentist task").await;
    let (_, reference) = h.connector.wait_for_question().await;
    h.choose(&reference, Choice::No).await;
    h.connector.wait_for_sent(1).await;
    h.finish().await;

    assert!(h.vault.task(h.task.id).is_ok());
}

#[tokio::test]
async fn something_else_declines_the_question_and_is_then_answered_in_its_own_right() {
    let mut h = Harness::start(|task| deleting(task, "Noted."), Timing::default()).await;

    h.say("delete the dentist task").await;
    h.connector.wait_for_question().await;
    h.say("actually, what's on my list?").await;
    h.connector.wait_for_sent(2).await;
    h.finish().await;

    assert!(h.vault.task(h.task.id).is_ok(), "moving on is not a yes");
    assert_eq!(h.connector.sent().len(), 2, "the declined turn replied, then the new one did");
    assert_eq!(h.connector.questions().len(), 1);
}

#[tokio::test]
async fn an_unanswered_question_is_saved_for_later_and_the_chat_is_told() {
    let timing = Timing { confirm_timeout: Duration::from_millis(200), ..Timing::default() };
    let mut h = Harness::start(|task| deleting(task, "Left it for you."), timing).await;

    h.say("delete the dentist task").await;
    h.connector.wait_for_question().await;
    // Nobody answers.
    h.connector.wait_for_sent(2).await;
    h.finish().await;

    let sent = h.connector.sent();
    assert!(sent[0].contains("No answer"), "{}", sent[0]);
    assert!(sent[0].contains("saved that for later"), "{}", sent[0]);
    assert_eq!(sent[1], "Left it for you.");
    assert!(h.vault.task(h.task.id).is_ok(), "silence is never a yes");
    assert_eq!(
        h.vault.proposals(&Default::default()).unwrap().len(),
        1,
        "the delete was parked as a proposal"
    );
}

// ---- commands ----------------------------------------------------------------

#[tokio::test]
async fn new_starts_a_different_conversation() {
    let mut h = Harness::start(|_| says("Hello."), Timing::default()).await;

    h.say("hi").await;
    h.connector.wait_for_sent(1).await;
    let first = {
        let conversations = h.conversations();
        assert_eq!(conversations.len(), 1);
        conversations[0].id
    };

    h.say("!new").await;
    h.connector.wait_for_sent(2).await;
    assert_eq!(h.connector.sent()[1], "Starting a new conversation.");

    h.say("hi again").await;
    h.connector.wait_for_sent(3).await;
    h.finish().await;

    let conversations = h.conversations();
    assert_eq!(conversations.len(), 2, "the second message went to a conversation of its own");
    assert!(conversations.iter().any(|c| c.id == first));
    for conversation in &conversations {
        assert_eq!(conversation.via, Some(via_of(&h)), "both belong to this chat");
    }
}

#[tokio::test]
async fn stop_with_nothing_running_says_so() {
    let mut h = Harness::start(|_| says("Hello."), Timing::default()).await;

    h.say("!stop").await;
    h.connector.wait_for_sent(1).await;
    h.finish().await;

    assert_eq!(h.connector.sent(), vec!["Nothing is running."]);
    assert!(h.conversations().is_empty(), "and it was not sent to the assistant");
}

#[tokio::test]
async fn stop_ends_a_running_turn_and_says_so_once() {
    let mut h = Harness::start(|_| support::Script::Hangs, Timing::default()).await;

    h.say("write me a very long essay").await;
    h.connector.until("the turn to begin", typing_on).await;
    h.say("!stop").await;
    h.connector.wait_for_sent(1).await;
    // The model never answers; the turn ends only because it was cancelled.
    h.connector.until("the turn to end", typing_off).await;
    h.finish().await;

    assert_eq!(h.connector.sent(), vec!["Stopped."], "told once, not again when the turn unwinds");
}

#[tokio::test]
async fn stop_while_a_question_waits_declines_it_and_ends_the_turn() {
    let mut h = Harness::start(|task| deleting(task, "Deleted it."), Timing::default()).await;

    h.say("delete the dentist task").await;
    h.connector.wait_for_question().await;
    h.say("!stop").await;
    h.connector.wait_for_sent(1).await;
    h.finish().await;

    assert!(h.vault.task(h.task.id).is_ok(), "stopping is not a yes");
    assert_eq!(h.connector.sent(), vec!["Stopped."]);
}

#[tokio::test]
async fn help_says_what_the_bot_is_and_lists_the_commands() {
    let mut h = Harness::start(|_| says("Hello."), Timing::default()).await;

    h.say("!help").await;
    h.connector.wait_for_sent(1).await;
    h.finish().await;

    let sent = h.connector.sent();
    assert!(sent[0].contains("Juniper"), "it uses the assistant's name: {}", sent[0]);
    for command in ["!new", "!stop", "!help"] {
        assert!(sent[0].contains(command), "{command} missing from {}", sent[0]);
    }
}

// ---- what is not run ---------------------------------------------------------

#[tokio::test]
async fn an_old_message_gets_the_catch_up_reply_and_runs_no_turn() {
    let mut h = Harness::start(|_| says("This must not be said."), Timing::default()).await;

    let sent_at = Timestamp::now() - SignedDuration::from_hours(13);
    h.say_at("reply to Sam and say yes", sent_at).await;
    h.connector.wait_for_sent(1).await;
    h.finish().await;

    let sent = h.connector.sent();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains("offline"), "{}", sent[0]);
    assert!(sent[0].contains("13 hours"), "{}", sent[0]);
    assert!(sent[0].contains("send it again"), "{}", sent[0]);
    assert!(h.connector.typing().is_empty(), "no turn began");
    assert!(h.conversations().is_empty(), "and nothing was written to the vault");
}

#[tokio::test]
async fn a_message_that_could_not_be_read_is_answered_with_why() {
    let mut h = Harness::start(|_| says("Hello."), Timing::default()).await;

    h.deliver(Incoming::Unreadable { chat: room(), at: Timestamp::now() }).await;
    h.connector.wait_for_sent(1).await;
    h.finish().await;

    let sent = h.connector.sent();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains("couldn't read that message"), "{}", sent[0]);
    assert!(h.conversations().is_empty());
}

#[tokio::test]
async fn stopping_the_relay_ends_it_without_a_word() {
    let mut h = Harness::start(|_| support::Script::Hangs, Timing::default()).await;

    h.say("write me a very long essay").await;
    h.connector.until("the turn to begin", typing_on).await;
    h.halt().await;

    assert!(h.connector.sent().is_empty(), "a relay being stopped sends nothing more");
}

// ---- one link's life ---------------------------------------------------------

/// A world with `link` saved in it, switched on, and the service's view of it.
fn saved_link(endpoint: &str) -> (support::World, karmayoga_core::chat::ChatLink) {
    let world = world(endpoint);
    let link = matrix_link();
    world.vault.save_chat_settings(&ChatSettings { links: vec![link.clone()] }).unwrap();
    (world, link)
}

async fn within<T>(what: &str, future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(30), future)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

#[tokio::test]
async fn a_link_that_is_gone_is_done_and_reports_off() {
    let world = world("http://127.0.0.1:1/v1");
    let link = matrix_link(); // never saved
    let (_stop, stop_rx) = watch::channel(false);

    let outcome =
        run_link_with(world.service.clone(), world.vault.clone(), link.id, stop_rx, |_| async {
            Err(ChatError::Transient("must not be asked to sign in".into()))
        })
        .await;

    assert!(matches!(outcome, Ok(Outcome::Done)), "{outcome:?}");
    assert_eq!(world.service.chat_status(link.id).state, ChatState::Off);
}

#[tokio::test]
async fn a_switched_off_link_is_done_and_reports_off() {
    let (world, mut link) = saved_link("http://127.0.0.1:1/v1");
    link.enabled = false;
    world.vault.save_chat_settings(&ChatSettings { links: vec![link.clone()] }).unwrap();
    let (_stop, stop_rx) = watch::channel(false);

    let outcome =
        run_link_with(world.service.clone(), world.vault.clone(), link.id, stop_rx, |_| async {
            Err(ChatError::Transient("must not be asked to sign in".into()))
        })
        .await;

    assert!(matches!(outcome, Ok(Outcome::Done)), "{outcome:?}");
    assert_eq!(world.service.chat_status(link.id).state, ChatState::Off);
}

#[tokio::test]
async fn a_refused_sign_in_is_done_until_somebody_signs_in_again() {
    let (world, link) = saved_link("http://127.0.0.1:1/v1");
    let (_stop, stop_rx) = watch::channel(false);

    let outcome =
        run_link_with(world.service.clone(), world.vault.clone(), link.id, stop_rx, |_| async {
            Err(ChatError::NeedsSignIn("The password was refused.".into()))
        })
        .await;

    assert!(matches!(outcome, Ok(Outcome::Done)), "no backoff will fix a wrong password");
    let status = world.service.chat_status(link.id);
    assert_eq!(status.state, ChatState::NeedsSignIn);
    assert_eq!(status.detail, "The password was refused.");
}

#[tokio::test]
async fn a_server_that_is_down_is_an_error_for_the_supervisor_to_retry() {
    let (world, link) = saved_link("http://127.0.0.1:1/v1");
    let (_stop, stop_rx) = watch::channel(false);

    let outcome =
        run_link_with(world.service.clone(), world.vault.clone(), link.id, stop_rx, |_| async {
            Err(ChatError::Transient("the server is down".into()))
        })
        .await;

    let error = outcome.expect_err("the supervisor must be told to try again");
    assert_eq!(error.to_string(), "the server is down");
    let status = world.service.chat_status(link.id);
    assert_eq!(status.state, ChatState::Failing);
    assert_eq!(status.detail, "the server is down");
}

#[tokio::test]
async fn a_connection_that_fails_while_listening_is_an_error_to_retry() {
    let (world, link) = saved_link("http://127.0.0.1:1/v1");
    let (_stop, stop_rx) = watch::channel(false);
    let connector = FakeConnector::new();
    connector.fail_listening_with(ChatError::Transient("the sync was refused".into()));

    let outcome = within(
        "the link to end",
        run_link_with(world.service.clone(), world.vault.clone(), link.id, stop_rx, {
            let connector = connector.clone();
            move |_| async move { Ok(connector as Arc<dyn Connector>) }
        }),
    )
    .await;

    let error = outcome.expect_err("the supervisor must be told to try again");
    assert_eq!(error.to_string(), "the sync was refused");
    assert_eq!(world.service.chat_status(link.id).state, ChatState::Failing);
}

#[tokio::test]
async fn a_stop_during_sign_in_does_not_wait_for_it() {
    let (world, link) = saved_link("http://127.0.0.1:1/v1");
    let (stop_tx, stop_rx) = watch::channel(false);

    let run = tokio::spawn(run_link_with(
        world.service.clone(),
        world.vault.clone(),
        link.id,
        stop_rx,
        |_| std::future::pending::<Result<Arc<dyn Connector>, ChatError>>(),
    ));
    // Stopped once it is actually waiting on the sign-in, not before it began.
    within("the link to begin signing in", async {
        while world.service.chat_status(link.id).state != ChatState::Connecting {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    stop_tx.send_replace(true);

    let outcome = within("the link to end", run).await.expect("the task does not panic");
    assert!(matches!(outcome, Ok(Outcome::Done)), "{outcome:?}");
}

#[tokio::test]
async fn the_connector_is_given_what_it_needs_and_a_stop_ends_the_link_cleanly() {
    let (world, link) = saved_link("http://127.0.0.1:1/v1");
    world
        .vault
        .save_chat_secret(
            link.id,
            &ChatSecret { password: Some("hunter2".into()), ..Default::default() },
        )
        .unwrap();
    let (stop_tx, stop_rx) = watch::channel(false);
    let connector = FakeConnector::new();
    let (seen_tx, seen_rx) = oneshot::channel::<Setup>();

    let run = tokio::spawn(run_link_with(
        world.service.clone(),
        world.vault.clone(),
        link.id,
        stop_rx,
        {
            let connector = connector.clone();
            move |setup: Setup| async move {
                let _ = seen_tx.send(setup);
                Ok(connector as Arc<dyn Connector>)
            }
        },
    ));

    let setup = within("the connector to be set up", seen_rx).await.expect("it was set up");
    assert_eq!(setup.link.id, link.id);
    assert_eq!(setup.secret.password.as_deref(), Some("hunter2"));
    assert_eq!(setup.assistant_name, "Juniper");
    assert!(setup.state_dir.is_dir(), "its directory exists: {}", setup.state_dir.display());
    assert!(setup.state_dir.ends_with(link.id.to_string()));
    assert!(!setup.store_passphrase.is_empty());
    assert_eq!(world.service.chat_status(link.id).state, ChatState::Connecting);

    // What it reports reaches the service...
    (setup.report)(ChatState::Online, "Signed in as @bot:test.".into());
    let status = world.service.chat_status(link.id);
    assert_eq!(status.state, ChatState::Online);
    assert_eq!(status.detail, "Signed in as @bot:test.");

    // ...and what it saves reaches the vault.
    let session = ChatSecret {
        password: None,
        session: Some(serde_json::json!({ "device_id": "ABCDEF" })),
        recovery_key: Some("EsTc ...".into()),
    };
    (setup.save_secret)(&session).unwrap();
    assert_eq!(world.vault.chat_secret(link.id).unwrap(), session);

    stop_tx.send_replace(true);
    let outcome = within("the link to end", run).await.expect("the task does not panic");
    assert!(
        matches!(outcome, Ok(Outcome::Done)),
        "asked to stop, the link is done and not a failure to back off from: {outcome:?}"
    );
}

#[tokio::test]
async fn a_message_the_connector_delivers_is_answered_through_the_whole_link() {
    let model = fake_model(says("Hello from the assistant.")).await;
    let (world, link) = saved_link(&model.endpoint);
    let (stop_tx, stop_rx) = watch::channel(false);
    let connector = FakeConnector::new();

    let run = tokio::spawn(run_link_with(
        world.service.clone(),
        world.vault.clone(),
        link.id,
        stop_rx,
        {
            let connector = connector.clone();
            move |_| async move { Ok(connector as Arc<dyn Connector>) }
        },
    ));

    connector.deliver(Incoming::Said { chat: room(), text: "hello".into(), at: Timestamp::now() });
    connector.wait_for_sent(1).await;
    assert_eq!(connector.sent(), vec!["Hello from the assistant."]);

    stop_tx.send_replace(true);
    let outcome = within("the link to end", run).await.expect("the task does not panic");
    assert!(matches!(outcome, Ok(Outcome::Done)), "{outcome:?}");
}

// ---- routines reaching the owner ---------------------------------------------
//
// A routine reaches the owner through the outlet the relay registers for its
// link: a message, a confirmation, a question of its own. Each goes to the
// owner's private room, in turn with whatever that room is doing.

/// A routine run's transcript, as the scheduler makes one, and who it is from.
fn routine_run(h: &Harness, name: &str) -> (karmayoga_core::Conversation, FromRun) {
    let thread = karmayoga_core::Conversation::for_run(karmayoga_core::RoutineRunId::new(), name);
    h.vault.save_conversation(&thread).unwrap();
    let from = FromRun { routine: name.into(), conversation: Some(thread.id) };
    (thread, from)
}

/// The outlet the relay registers once it is running -- a moment after it is
/// spawned, so waited for.
async fn outlet(h: &Harness) -> Arc<dyn karmayoga_service::chat::ChatOutlet> {
    within("the relay to offer an outlet", async {
        loop {
            if let Some(outlet) = h.service.chat_outlet(h.link.id) {
                return outlet;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
}

/// A run's pictures go to the owner's room as pictures; one trashed since the
/// run is skipped rather than failing the rest.
#[tokio::test]
async fn a_routines_pictures_go_to_the_owners_room() {
    let mut h = Harness::start(|_| says("Unused."), Timing::default()).await;
    h.connector.set_home(room());
    let (_, from) = routine_run(&h, "Morning brief");
    // A blue picture three pixels by two.
    let png: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 3, 0, 0, 0, 2, 8, 2,
        0, 0, 0, 18, 22, 241, 77, 0, 0, 0, 15, 73, 68, 65, 84, 120, 156, 99, 96, 96, 248, 15, 67,
        48, 22, 0, 53, 222, 5, 251, 226, 150, 96, 158, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96,
        130,
    ];
    let options = karmayoga_core::ImportOptions::new(karmayoga_core::FileOrigin::Jobs);
    let kept = h.vault.import_bytes(png, "Week.png", &options).unwrap();
    let gone = h.vault.import_bytes(png, "Gone.png", &options).unwrap();
    h.vault.trash_files(&[gone.id]).unwrap();

    let sent = outlet(&h).await.show(from, vec![gone.id, kept.id]).await;
    assert_eq!(sent, Ok(1));
    let shown: Vec<String> = h
        .connector
        .events()
        .into_iter()
        .filter_map(|e| match e {
            Event::Showed { chat, shown } if chat == room() => Some(shown),
            _ => None,
        })
        .collect();
    let [shown] = shown.as_slice() else { panic!("one picture: {shown:?}") };
    assert!(
        shown.starts_with(&kept.name.replace(".png", "")) && shown.contains("image/png"),
        "{shown}"
    );
    h.finish().await;
}

#[tokio::test]
async fn a_routine_posts_in_the_owners_room_and_the_reply_carries_on_its_conversation() {
    let mut h = Harness::start(|_| says("Moved it to Thursday."), Timing::default()).await;
    h.connector.set_home(room());
    let (thread, from) = routine_run(&h, "Morning brief");

    outlet(&h)
        .await
        .post(from, "**Morning brief**\n\nThe dentist is at nine.".into())
        .await
        .expect("it went out");
    assert_eq!(h.connector.sent(), vec!["**Morning brief**\n\nThe dentist is at nine."]);
    assert_eq!(
        h.vault.conversation(thread.id).unwrap().via,
        Some(via_of(&h)),
        "a relay started afresh can find it again"
    );

    h.say("move it to thursday").await;
    h.connector.wait_for_sent(2).await;
    h.finish().await;

    let said: Vec<String> = h
        .vault
        .messages(thread.id)
        .unwrap()
        .into_iter()
        .filter(|m| m.role == Role::User)
        .map(|m| m.content)
        .collect();
    assert_eq!(said, vec!["move it to thursday"], "the reply went on in the run's own thread");
    assert_eq!(h.conversations().len(), 1, "and no second conversation was started");
}

#[tokio::test]
async fn a_routine_with_no_room_to_post_in_is_told_why() {
    let mut h = Harness::start(|_| says("unused"), Timing::default()).await;
    let (_, from) = routine_run(&h, "Morning brief");

    let why = outlet(&h).await.post(from, "hello".into()).await.expect_err("there is no room yet");
    assert!(why.contains("has not joined"), "{why}");
    h.finish().await;
    assert!(h.connector.sent().is_empty());
}

#[tokio::test]
async fn the_outlet_goes_when_the_relay_does() {
    let mut h = Harness::start(|_| says("unused"), Timing::default()).await;
    outlet(&h).await;
    h.halt().await;
    assert!(h.service.chat_outlet(h.link.id).is_none());
}

#[tokio::test]
async fn a_routines_approval_is_asked_in_the_room_and_a_tap_answers_it() {
    let mut h = Harness::start(|_| says("unused"), Timing::default()).await;
    h.connector.set_home(room());
    let (_, from) = routine_run(&h, "Tidy up");

    let asking = tokio::spawn(outlet(&h).await.confirm(
        from,
        "destructive".into(),
        "Book the dentist".into(),
        true,
    ));
    let (question, reference) = h.connector.wait_for_question().await;
    assert!(
        question.markdown.starts_with("_From your routine **Tidy up**:_"),
        "{}",
        question.markdown
    );
    assert!(question.markdown.contains("Book the dentist"));
    assert!(question.choices.contains(&Choice::Later), "it can be parked");

    h.choose(&reference, Choice::Yes).await;
    let answer = within("the answer", asking).await.unwrap();
    assert_eq!(answer, karmayoga_service::agent::ConfirmAnswer::Confirm);
    h.finish().await;
    assert!(h.connector.typing().is_empty(), "nothing in this room was typing");
    assert_eq!(h.conversations().len(), 1, "no turn ran here");
}

#[tokio::test]
async fn a_routines_own_question_takes_the_next_message_as_its_answer() {
    let mut h = Harness::start(|_| says("unused"), Timing::default()).await;
    h.connector.set_home(room());
    let (_, from) = routine_run(&h, "Morning brief");

    let asking = tokio::spawn(outlet(&h).await.ask(
        from,
        "Which slot for the dentist?".into(),
        vec!["Tuesday 9:00".into(), "Thursday 14:00".into()],
    ));
    h.connector.wait_for_sent(1).await;
    let question = &h.connector.sent()[0];
    assert!(question.contains("Which slot for the dentist?"), "{question}");
    assert!(question.contains("2. Thursday 14:00"), "{question}");

    h.say("2").await;
    let answer = within("the answer", asking).await.unwrap();
    assert_eq!(answer.as_deref(), Some("Thursday 14:00"));
    h.finish().await;
    assert_eq!(h.connector.sent().len(), 1, "the answer started no turn and got no reply");
}

#[tokio::test]
async fn stop_while_a_routine_waits_withdraws_its_question() {
    let mut h = Harness::start(|_| says("unused"), Timing::default()).await;
    h.connector.set_home(room());
    let (_, from) = routine_run(&h, "Morning brief");

    let asking = tokio::spawn(outlet(&h).await.ask(from, "Anything to add?".into(), Vec::new()));
    h.connector.wait_for_sent(1).await;
    h.say("!stop").await;
    let answer = within("the answer", asking).await.unwrap();
    assert_eq!(answer, None);
    h.connector.wait_for_sent(2).await;
    h.finish().await;
    assert_eq!(h.connector.sent()[1], "Stopped.");
}

#[tokio::test]
async fn a_routines_question_nobody_answers_comes_back_empty_and_says_so() {
    let timing = Timing { confirm_timeout: Duration::from_millis(200), ..Timing::default() };
    let mut h = Harness::start(|_| says("unused"), timing).await;
    h.connector.set_home(room());
    let (_, from) = routine_run(&h, "Morning brief");

    let answer =
        within("the answer", outlet(&h).await.ask(from, "Anything?".into(), Vec::new())).await;
    assert_eq!(answer, None);
    h.connector.wait_for_sent(2).await;
    h.finish().await;
    assert!(h.connector.sent()[1].starts_with("No answer"), "{:?}", h.connector.sent());
}

// ---- pictures ------------------------------------------------------------------
//
// A phone sends a photo and the question about it as two messages more often
// than as one. The relay holds a picture sent with nothing written beside it
// for what is said next, and sends it alone only once nothing more comes.

/// A two-pixel PNG: small enough to write out, and a real picture, so the
/// service's own check that it can be read passes.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x02, 0x00, 0x00, 0x00, 0xfd, 0xd4, 0x9a,
    0x73, 0x00, 0x00, 0x00, 0x10, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x38, 0x51, 0xa1, 0x01,
    0x44, 0x0c, 0x10, 0x0a, 0x00, 0x29, 0xee, 0x05, 0xa1, 0x8c, 0x09, 0xce, 0x88, 0x00, 0x00, 0x00,
    0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn showed(caption: &str) -> Incoming {
    Incoming::Showed {
        chat: room(),
        picture: Picture { filename: "plant.png".into(), bytes: PNG.to_vec() },
        caption: caption.into(),
        at: Timestamp::now(),
    }
}

/// The person's messages in the one conversation, with how many pictures
/// each came with.
fn their_messages(h: &Harness) -> Vec<(String, usize)> {
    let conversations = h.conversations();
    assert_eq!(conversations.len(), 1);
    h.vault
        .messages(conversations[0].id)
        .unwrap()
        .into_iter()
        .filter(|m| m.role == Role::User)
        .map(|m| (m.content, m.attachments.len()))
        .collect()
}

#[tokio::test]
async fn a_picture_sent_alone_goes_with_the_question_typed_after_it() {
    let mut h = Harness::start(|_| says("A monstera."), Timing::default()).await;

    h.deliver(showed("")).await;
    h.say("what plant is this?").await;
    h.connector.wait_for_sent(1).await;
    h.finish().await;

    assert_eq!(h.connector.sent(), vec!["A monstera."], "one question, one answer");
    assert_eq!(their_messages(&h), vec![("what plant is this?".to_string(), 1)]);
}

#[tokio::test]
async fn a_captioned_picture_is_answered_at_once() {
    let timing = Timing { picture_wait: Duration::from_secs(3600), ..Timing::default() };
    let mut h = Harness::start(|_| says("A monstera."), timing).await;

    h.deliver(showed("what plant is this?")).await;
    h.connector.wait_for_sent(1).await;
    h.finish().await;

    assert_eq!(their_messages(&h), vec![("what plant is this?".to_string(), 1)]);
}

#[tokio::test]
async fn a_picture_nobody_says_anything_about_is_answered_on_its_own() {
    let timing = Timing { picture_wait: Duration::from_millis(200), ..Timing::default() };
    let mut h = Harness::start(|_| says("That looks like a monstera."), timing).await;

    h.deliver(showed("")).await;
    h.deliver(showed("")).await;
    h.connector.wait_for_sent(1).await;
    h.finish().await;

    assert_eq!(h.connector.sent(), vec!["That looks like a monstera."]);
    assert_eq!(their_messages(&h), vec![(String::new(), 2)], "both pictures, in one turn");
}

#[tokio::test]
async fn a_picture_that_cannot_be_read_is_said_to_be_so() {
    let mut h = Harness::start(|_| says("Never asked."), Timing::default()).await;

    h.deliver(Incoming::Showed {
        chat: room(),
        picture: Picture { filename: "IMG_0001.HEIC".into(), bytes: b"ftypheic....".to_vec() },
        caption: "what is this?".into(),
        at: Timestamp::now(),
    })
    .await;
    h.connector.wait_for_sent(1).await;
    h.finish().await;

    let sent = h.connector.sent();
    assert!(sent[0].contains("could not be read"), "{sent:?}");
}

// ---- calls -------------------------------------------------------------------

/// A call to the assistant: the relay half. The media and the voice session
/// need speech models and real sockets, so they are stood in for by
/// [`FakeCalls`], which keeps what it was given for the test to look at and
/// ends a call when it is told to. One test uses the real backend, to see a
/// vault with no speech set up refuse.
mod calls {
    use super::room;
    use super::support::{FakeConnector, FakeModel, Script, fake_model, matrix_link, says, world};
    use async_trait::async_trait;
    use futures::future::BoxFuture;
    use jiff::{SignedDuration, Timestamp};
    use karmayoga_chat::call::{CallEnd, CallError, ToCall};
    use karmayoga_chat::connector::{
        CallId, CallJoin, CallKey, ChatError, ChatId, Choice, Connector, HangUp, Incoming,
        MessageRef, Question,
    };
    use karmayoga_chat::relay::{self, CallBackend, Media, Timing};
    use karmayoga_core::chat::{ChatLink, Via};
    use karmayoga_core::{Conversation, ConversationId, ConversationQuery, Task, Vault};
    use karmayoga_service::Service;
    use karmayoga_service::agent::{AgentEvent, Sink};
    use karmayoga_service::chat::{CallDelivered, ChatOutlet, FromRun};
    use karmayoga_service::error::CommandResult;
    use karmayoga_service::voice::{VoiceEvent, VoiceInput, VoiceLink, VoiceOutput, VoiceState};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::{mpsc, oneshot, watch};
    use tokio::task::JoinHandle;

    /// What the connector was asked of a call.
    #[derive(Debug, Clone, PartialEq)]
    enum CallEvent {
        Rang,
        Joined { call: CallId },
        Ended { call: CallId, why: HangUp },
    }

    /// The ordinary fake connector, which also writes down what it is asked
    /// of calls.
    struct CallConnector {
        chat: Arc<FakeConnector>,
        calls: Mutex<Vec<CallEvent>>,
        refuse_joins: Mutex<bool>,
        refuse_rings: Mutex<bool>,
        /// When set, the owner hangs up as the call is being joined: this
        /// delivers their hang-up, gives the relay time to hear it, and then
        /// fails as a chat service that has forgotten the call would.
        hangs_up_during_join: Mutex<Option<mpsc::Sender<Incoming>>>,
    }

    #[async_trait]
    impl Connector for CallConnector {
        async fn listen(
            &self,
            inbox: mpsc::Sender<Incoming>,
            stop: watch::Receiver<bool>,
        ) -> Result<(), ChatError> {
            self.chat.listen(inbox, stop).await
        }
        async fn send(&self, chat: &ChatId, markdown: &str) -> Result<MessageRef, ChatError> {
            self.chat.send(chat, markdown).await
        }
        async fn ask(&self, chat: &ChatId, question: &Question) -> Result<MessageRef, ChatError> {
            self.chat.ask(chat, question).await
        }
        async fn home(&self) -> Option<ChatId> {
            self.chat.home().await
        }
        async fn typing(&self, chat: &ChatId, on: bool) -> Result<(), ChatError> {
            Connector::typing(&*self.chat, chat, on).await
        }
        async fn join_call(&self, _chat: &ChatId, call: &CallId) -> Result<CallJoin, ChatError> {
            if *self.refuse_joins.lock().unwrap() {
                return Err(ChatError::Transient("the server said no".into()));
            }
            let hang_up = self.hangs_up_during_join.lock().unwrap().take();
            if let Some(inbox) = hang_up {
                let ended =
                    Incoming::CallEnded { chat: room(), call: call.clone(), at: Timestamp::now() };
                let _ = inbox.send(ended).await;
                tokio::time::sleep(Duration::from_millis(150)).await;
                return Err(ChatError::Transient("no such call".into()));
            }
            self.calls.lock().unwrap().push(CallEvent::Joined { call: call.clone() });
            Ok(join_for(call))
        }
        async fn ring(&self, _chat: &ChatId) -> Result<CallId, ChatError> {
            if *self.refuse_rings.lock().unwrap() {
                return Err(ChatError::Transient("no calls here".into()));
            }
            self.calls.lock().unwrap().push(CallEvent::Rang);
            Ok(CallId("ring".into()))
        }
        async fn end_call(
            &self,
            _chat: &ChatId,
            call: &CallId,
            why: HangUp,
        ) -> Result<(), ChatError> {
            self.calls.lock().unwrap().push(CallEvent::Ended { call: call.clone(), why });
            Ok(())
        }
    }

    /// What the fake connector says a call's media is: a room named for the
    /// call, so a test can see the backend was handed the right one.
    fn join_for(call: &CallId) -> CallJoin {
        CallJoin {
            url: "wss://media.example.org".into(),
            token: format!("token for {}", call.0),
            caller: "@owner:example.org:PHONE".into(),
            own_key: CallKey {
                participant: "@bot:example.org:BOT".into(),
                index: 0,
                key: vec![0; 16],
            },
            wait_for_caller: true,
        }
    }

    fn key(index: u8) -> CallKey {
        CallKey { participant: "@owner:example.org:PHONE".into(), index, key: vec![index; 16] }
    }

    /// What [`FakeCalls`] opened a voice session with.
    #[derive(Clone)]
    struct Opened {
        service: Arc<Service>,
        conversation: ConversationId,
        via: Via,
        output: mpsc::UnboundedSender<VoiceOutput>,
    }

    /// A [`CallBackend`] with no models and no sockets.
    #[derive(Default)]
    struct FakeCalls {
        /// Set to make connecting the media fail, with this.
        connect_fails: Mutex<Option<String>>,
        /// Set to make opening the voice session panic, once.
        panic_on_open: AtomicBool,
        opened: Mutex<Vec<Opened>>,
        /// Every call connected, with the keys it was given.
        connected: Mutex<Vec<(CallJoin, Vec<CallKey>)>>,
        /// What the running call was told after it started.
        received: Mutex<Vec<ToCall>>,
        /// How many times a voice session was told to stop.
        stops: AtomicUsize,
        /// What a voice session was asked to say of its own.
        said: Mutex<Vec<String>>,
        /// Ends the running call, with how.
        finish: Mutex<Option<oneshot::Sender<CallEnd>>>,
    }

    impl FakeCalls {
        fn opened(&self) -> Vec<Opened> {
            self.opened.lock().unwrap().clone()
        }

        fn received(&self) -> Vec<ToCall> {
            self.received.lock().unwrap().clone()
        }

        /// End the call the way `end` says.
        fn finish(&self, end: CallEnd) {
            let tx = self.finish.lock().unwrap().take().expect("a call is running");
            let _ = tx.send(end);
        }

        /// What the voice session would tell the interface.
        fn report(&self, event: VoiceEvent) {
            let opened = self.opened();
            let _ =
                opened.last().expect("a session is open").output.send(VoiceOutput::Event(event));
        }

        /// Run a real turn on the session's conversation, as the session
        /// would for something the owner said, reporting it as the session
        /// does: its events, and `WaitingForClick` where a confirmation
        /// needs a click.
        fn start_turn(&self, text: &str) {
            let Opened { service, conversation, via, output } =
                self.opened().last().expect("a session is open").clone();
            let sink: Sink = Arc::new(move |event| {
                let confirms = matches!(event, AgentEvent::ConfirmationRequired { .. });
                let _ = output.send(VoiceOutput::Event(VoiceEvent::Agent { event }));
                if confirms {
                    let state = VoiceEvent::State { state: VoiceState::WaitingForClick };
                    let _ = output.send(VoiceOutput::Event(state));
                }
            });
            let text = text.to_string();
            tokio::spawn(async move {
                let _ = service
                    .chat_turn_with_files(via, conversation, text, Vec::new(), Vec::new(), sink)
                    .await;
            });
        }
    }

    /// [`FakeCalls`], as the relay is given it.
    struct Backend(Arc<FakeCalls>);

    impl CallBackend for Backend {
        fn open_voice(
            &self,
            service: Arc<Service>,
            conversation: ConversationId,
            via: Via,
        ) -> BoxFuture<'static, CommandResult<VoiceLink>> {
            let (input, mut heard) = mpsc::unbounded_channel();
            let (output_tx, output) = mpsc::unbounded_channel();
            let fake = self.0.clone();
            tokio::spawn(async move {
                while let Some(input) = heard.recv().await {
                    match input {
                        VoiceInput::Stop => {
                            fake.stops.fetch_add(1, Ordering::SeqCst);
                        }
                        VoiceInput::Say(text) => fake.said.lock().unwrap().push(text),
                        _ => {}
                    }
                }
            });
            let panics = self.0.panic_on_open.swap(false, Ordering::SeqCst);
            self.0.opened.lock().unwrap().push(Opened {
                service,
                conversation,
                via,
                output: output_tx,
            });
            Box::pin(async move {
                if panics {
                    panic!("the voice session fell over");
                }
                Ok(VoiceLink { input, output })
            })
        }

        fn connect(
            &self,
            join: CallJoin,
            keys: Vec<CallKey>,
        ) -> BoxFuture<'static, Result<Box<dyn Media>, CallError>> {
            let fake = self.0.clone();
            Box::pin(async move {
                if let Some(why) = fake.connect_fails.lock().unwrap().clone() {
                    return Err(CallError(why));
                }
                fake.connected.lock().unwrap().push((join, keys));
                let (tx, finished) = oneshot::channel();
                *fake.finish.lock().unwrap() = Some(tx);
                Ok(Box::new(FakeMedia { fake, finished }) as Box<dyn Media>)
            })
        }
    }

    struct FakeMedia {
        fake: Arc<FakeCalls>,
        finished: oneshot::Receiver<CallEnd>,
    }

    impl Media for FakeMedia {
        fn run(
            self: Box<Self>,
            voice: VoiceLink,
            mut to_call: mpsc::UnboundedReceiver<ToCall>,
            events: mpsc::UnboundedSender<VoiceEvent>,
        ) -> BoxFuture<'static, CallEnd> {
            Box::pin(async move {
                let FakeMedia { fake, mut finished } = *self;
                let VoiceLink { input, mut output } = voice;
                loop {
                    tokio::select! {
                        // What the session reported goes out before the call
                        // is told it has ended, as the real media does it.
                        biased;
                        told = to_call.recv() => {
                            let told = told.unwrap_or(ToCall::End);
                            fake.received.lock().unwrap().push(told.clone());
                            if told == ToCall::End {
                                let _ = input.send(VoiceInput::Stop);
                                return CallEnd::Ended;
                            }
                        }
                        out = output.recv() => match out {
                            Some(VoiceOutput::Event(event)) => {
                                let _ = events.send(event);
                            }
                            Some(_) => {}
                            None => return CallEnd::SessionEnded,
                        },
                        end = &mut finished => return end.unwrap_or(CallEnd::Ended),
                    }
                }
            })
        }
    }

    /// A relay running against a fake connector that takes calls, a fake
    /// model, and either the fake backend or the real one.
    struct Calling {
        service: Arc<Service>,
        vault: Arc<Vault>,
        link: ChatLink,
        connector: Arc<CallConnector>,
        fake: Arc<FakeCalls>,
        task: Task,
        inbox: Option<mpsc::Sender<Incoming>>,
        stop: watch::Sender<bool>,
        relay: Option<JoinHandle<()>>,
        _dir: tempfile::TempDir,
        _model: FakeModel,
    }

    impl Calling {
        async fn start(script: impl FnOnce(&Task) -> Script) -> Self {
            Self::start_with(script, true, Timing::default()).await
        }

        async fn start_with(
            script: impl FnOnce(&Task) -> Script,
            fake_backend: bool,
            timing: Timing,
        ) -> Self {
            let super::support::World { service, vault, dir } = world("http://127.0.0.1:1/v1");
            let task = Task::new("Book the dentist");
            vault.save_task(&task).unwrap();
            let model = fake_model(script(&task)).await;
            let mut models = vault.model_settings().unwrap();
            models.medium.connection.base_url = Some(model.endpoint.clone());
            vault.save_model_settings(&models).unwrap();

            let link = matrix_link();
            let connector = Arc::new(CallConnector {
                chat: FakeConnector::new(),
                calls: Mutex::new(Vec::new()),
                refuse_joins: Mutex::new(false),
                refuse_rings: Mutex::new(false),
                hangs_up_during_join: Mutex::new(None),
            });
            let fake = Arc::new(FakeCalls::default());
            let (inbox, inbox_rx) = mpsc::channel(32);
            let (stop, stop_rx) = watch::channel(false);
            let relay = if fake_backend {
                tokio::spawn(relay::run_with_calls(
                    service.clone(),
                    vault.clone(),
                    link.clone(),
                    connector.clone(),
                    inbox_rx,
                    stop_rx,
                    timing,
                    Arc::new(Backend(fake.clone())),
                ))
            } else {
                tokio::spawn(relay::run_with(
                    service.clone(),
                    vault.clone(),
                    link.clone(),
                    connector.clone(),
                    inbox_rx,
                    stop_rx,
                    timing,
                ))
            };
            Self {
                service,
                vault,
                link,
                connector,
                fake,
                task,
                inbox: Some(inbox),
                stop,
                relay: Some(relay),
                _dir: dir,
                _model: model,
            }
        }

        async fn deliver(&self, incoming: Incoming) {
            self.inbox
                .as_ref()
                .expect("the inbox is still open")
                .send(incoming)
                .await
                .expect("the relay is still reading its inbox");
        }

        async fn say(&self, text: &str) {
            self.deliver(Incoming::Said { chat: room(), text: text.into(), at: Timestamp::now() })
                .await;
        }

        async fn ring(&self, id: &str) {
            self.ring_until(id, Timestamp::now() + SignedDuration::from_secs(60)).await;
        }

        async fn ring_until(&self, id: &str, expires: Timestamp) {
            self.deliver(Incoming::Called {
                chat: room(),
                call: CallId(id.into()),
                at: Timestamp::now(),
                expires,
            })
            .await;
        }

        async fn keys(&self, id: &str, indexes: &[u8]) {
            self.deliver(Incoming::CallKeys {
                chat: room(),
                call: CallId(id.into()),
                keys: indexes.iter().map(|i| key(*i)).collect(),
                at: Timestamp::now(),
            })
            .await;
        }

        async fn hang_up(&self, id: &str) {
            self.deliver(Incoming::CallEnded {
                chat: room(),
                call: CallId(id.into()),
                at: Timestamp::now(),
            })
            .await;
        }

        fn calls(&self) -> Vec<CallEvent> {
            self.connector.calls.lock().unwrap().clone()
        }

        /// Wait until `done` holds, failing the test rather than hanging it.
        async fn until(&self, what: &str, done: impl Fn() -> bool) {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            while !done() {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "timed out waiting for {what}; calls {:?}, chat {:#?}",
                    self.calls(),
                    self.connector.chat.events()
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        /// Until `id` is joined on the chat service and its media is running:
        /// what answering a call is.
        async fn until_answered(&self, id: &str) {
            let call = CallId(id.into());
            let token = join_for(&call).token;
            self.until("the call to be answered", || {
                self.calls()
                    .iter()
                    .any(|e| matches!(e, CallEvent::Joined { call: c } if *c == call))
                    && self
                        .fake
                        .connected
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|(join, _)| join.token == token)
                    && self.fake.finish.lock().unwrap().is_some()
            })
            .await;
        }

        async fn until_ended(&self, id: &str, why: HangUp) {
            let ended = CallEvent::Ended { call: CallId(id.into()), why };
            self.until(&format!("the call to be hung up as {why:?}"), || {
                self.calls().contains(&ended)
            })
            .await;
        }

        async fn finish(&mut self) {
            drop(self.inbox.take());
            self.join().await;
        }

        async fn halt(&mut self) {
            self.stop.send_replace(true);
            self.join().await;
        }

        async fn join(&mut self) {
            if let Some(relay) = self.relay.take() {
                tokio::time::timeout(Duration::from_secs(30), relay)
                    .await
                    .expect("the relay ends")
                    .expect("the relay does not panic");
            }
        }

        fn conversations(&self) -> Vec<Conversation> {
            self.vault.conversations(&ConversationQuery::default()).unwrap()
        }
    }

    fn only_ended(calls: &[CallEvent], why: HangUp) -> bool {
        matches!(calls, [CallEvent::Ended { why: w, .. }] if *w == why)
    }

    #[tokio::test]
    async fn a_call_to_a_vault_with_no_speech_set_up_is_refused_and_the_owner_is_told_why() {
        let mut h = Calling::start_with(|_| says("unused"), false, Timing::default()).await;

        h.ring("c1").await;
        h.connector.chat.wait_for_sent(1).await;
        h.until_ended("c1", HangUp::Refused).await;
        h.finish().await;

        let sent = h.connector.chat.sent();
        assert!(sent[0].starts_with("I can't take calls yet: "), "{}", sent[0]);
        assert!(sent[0].len() > "I can't take calls yet: ".len(), "it says why: {}", sent[0]);
        assert!(only_ended(&h.calls(), HangUp::Refused), "never answered: {:?}", h.calls());
    }

    #[tokio::test]
    async fn a_call_is_joined_and_its_media_connected_on_the_chats_conversation() {
        let mut h = Calling::start(|_| says("unused")).await;

        h.ring("c1").await;
        h.until_answered("c1").await;
        h.until("the media to connect", || !h.fake.connected.lock().unwrap().is_empty()).await;

        assert_eq!(h.calls(), vec![CallEvent::Joined { call: CallId("c1".into()) }]);
        let opened = h.fake.opened();
        assert_eq!(opened.len(), 1);
        assert_eq!(opened[0].via, via_of_link(&h.link));
        let connected = h.fake.connected.lock().unwrap().clone();
        assert_eq!(connected[0].0, join_for(&CallId("c1".into())), "the room the join named");

        h.fake.finish(CallEnd::SessionEnded);
        h.until_ended("c1", HangUp::Finished).await;
        h.finish().await;
        assert!(h.connector.chat.sent().is_empty(), "a call that ended well says nothing");
    }

    fn via_of_link(link: &ChatLink) -> Via {
        Via { link: link.id, service: "Matrix".into(), chat: room().0 }
    }

    #[tokio::test]
    async fn keys_before_and_after_the_join_both_reach_the_call() {
        // A turn that never ends keeps the chat's worker busy, so the call
        // waits its turn and what the caller sends meanwhile is buffered.
        let mut h = Calling::start(|_| Script::Hangs).await;
        h.say("write me a very long essay").await;
        h.connector
            .chat
            .until("the turn to begin", |e| {
                e.iter().any(|e| matches!(e, super::Event::Typing { on: true, .. }))
            })
            .await;

        h.ring("c1").await;
        h.keys("c1", &[1, 2]).await;
        h.keys("other", &[7]).await;
        assert!(h.calls().is_empty(), "the call waits behind the turn");

        h.say("!stop").await;
        h.until("the media to connect", || !h.fake.connected.lock().unwrap().is_empty()).await;
        h.keys("c1", &[3]).await;
        h.until("the late key to reach the call", || !h.fake.received().is_empty()).await;

        let connected = h.fake.connected.lock().unwrap().clone();
        assert_eq!(connected.len(), 1);
        assert_eq!(connected[0].1, vec![key(1), key(2)]);
        assert_eq!(h.fake.received(), vec![ToCall::Keys(vec![key(3)])]);

        h.fake.finish(CallEnd::SessionEnded);
        h.finish().await;
    }

    #[tokio::test]
    async fn the_owner_hanging_up_ends_the_call() {
        let mut h = Calling::start(|_| says("unused")).await;
        h.ring("c1").await;
        h.until_answered("c1").await;

        h.hang_up("c1").await;
        h.until_ended("c1", HangUp::Finished).await;
        h.finish().await;

        assert_eq!(h.fake.received(), vec![ToCall::End]);
        assert_eq!(h.fake.stops.load(Ordering::SeqCst), 1, "and the session was stopped");
    }

    #[tokio::test]
    async fn a_hang_up_for_some_other_call_leaves_this_one_running() {
        let mut h = Calling::start(|_| says("unused")).await;
        h.ring("c1").await;
        h.until_answered("c1").await;

        h.hang_up("not-c1").await;
        // Something that arrives after it, to know it has been dealt with.
        h.keys("c1", &[5]).await;
        h.until("the key", || !h.fake.received().is_empty()).await;
        assert_eq!(h.fake.received(), vec![ToCall::Keys(vec![key(5)])]);

        h.fake.finish(CallEnd::SessionEnded);
        h.finish().await;
    }

    #[tokio::test]
    async fn stop_ends_a_running_call() {
        let mut h = Calling::start(|_| says("unused")).await;
        h.ring("c1").await;
        h.until_answered("c1").await;

        h.say("!stop").await;
        h.until_ended("c1", HangUp::Finished).await;
        h.finish().await;

        assert_eq!(h.fake.received(), vec![ToCall::End]);
        assert_eq!(h.connector.chat.sent(), vec!["Stopped."]);
    }

    #[tokio::test]
    async fn a_second_call_while_one_runs_is_refused_and_the_first_carries_on() {
        let mut h = Calling::start(|_| says("unused")).await;
        h.ring("c1").await;
        h.until_answered("c1").await;

        h.ring("c2").await;
        h.until_ended("c2", HangUp::Refused).await;
        h.connector.chat.wait_for_sent(1).await;
        assert_eq!(h.connector.chat.sent(), vec!["I'm already on a call."]);
        assert!(h.fake.received().is_empty(), "the first call was not disturbed");
        assert_eq!(h.fake.opened().len(), 1, "and no second session was opened");

        // The same call announced twice is not a second call.
        h.ring("c1").await;
        h.keys("c1", &[9]).await;
        h.until("the marker", || !h.fake.received().is_empty()).await;
        assert!(
            !h.calls()
                .contains(&CallEvent::Ended { call: CallId("c1".into()), why: HangUp::Refused })
        );

        h.fake.finish(CallEnd::SessionEnded);
        h.until_ended("c1", HangUp::Finished).await;
        h.finish().await;
    }

    #[tokio::test]
    async fn the_line_is_free_again_after_a_call() {
        let mut h = Calling::start(|_| says("unused")).await;
        h.ring("c1").await;
        h.until_answered("c1").await;
        h.fake.finish(CallEnd::SessionEnded);
        h.until_ended("c1", HangUp::Finished).await;

        h.ring("c2").await;
        h.until_answered("c2").await;
        h.fake.finish(CallEnd::SessionEnded);
        h.until_ended("c2", HangUp::Finished).await;
        h.finish().await;
        assert!(h.connector.chat.sent().is_empty());
    }

    #[tokio::test]
    async fn typing_after_a_call_carries_on_the_same_conversation() {
        let mut h = Calling::start(|_| says("Noted.")).await;
        h.ring("c1").await;
        h.until_answered("c1").await;
        let called_on = h.fake.opened()[0].conversation;
        h.fake.finish(CallEnd::SessionEnded);
        h.until_ended("c1", HangUp::Finished).await;

        h.say("hello again").await;
        h.connector.chat.wait_for_sent(1).await;
        h.finish().await;

        let conversations = h.conversations();
        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0].id, called_on, "the typed message went where the call was");
        assert_eq!(conversations[0].via, Some(via_of_link(&h.link)));
    }

    #[tokio::test]
    async fn a_call_made_after_typing_is_on_the_conversation_typing_started() {
        let mut h = Calling::start(|_| says("Noted.")).await;
        h.say("hello").await;
        h.connector.chat.wait_for_sent(1).await;
        let typed_in = h.conversations()[0].id;

        h.ring("c1").await;
        h.until_answered("c1").await;
        assert_eq!(h.fake.opened()[0].conversation, typed_in);

        h.fake.finish(CallEnd::SessionEnded);
        h.finish().await;
    }

    #[tokio::test]
    async fn a_confirmation_that_needs_a_tap_is_asked_in_the_chat_and_the_tap_answers_it() {
        let mut h = Calling::start(|task| super::deleting(task, "Deleted it.")).await;
        h.ring("c1").await;
        h.until_answered("c1").await;

        h.fake.start_turn("delete the dentist task");
        let (question, reference) = h.connector.chat.wait_for_question().await;
        assert!(question.markdown.contains("Book the dentist"), "{}", question.markdown);
        assert!(question.choices.starts_with(&[Choice::Yes, Choice::No]), "{:?}", question.choices);
        assert!(h.vault.task(h.task.id).is_ok(), "nothing is deleted before the answer");
        assert_eq!(
            h.connector.chat.typing(),
            Vec::<bool>::new(),
            "nobody is shown typing on a call"
        );

        h.deliver(Incoming::Chose {
            chat: room(),
            question: reference,
            choice: Choice::Yes,
            at: Timestamp::now(),
        })
        .await;
        h.until("the task to be deleted", || h.vault.task(h.task.id).is_err()).await;
        assert_eq!(h.connector.chat.questions().len(), 1);

        h.fake.finish(CallEnd::SessionEnded);
        h.finish().await;
    }

    fn requires(call_id: &str, subject: &str) -> VoiceEvent {
        VoiceEvent::Agent {
            event: AgentEvent::ConfirmationRequired {
                call_id: call_id.into(),
                name: "delete_task".into(),
                subject: subject.into(),
                arguments: serde_json::Value::Null,
                kind: "destructive".into(),
                can_park: false,
            },
        }
    }

    fn started(call_id: &str) -> VoiceEvent {
        VoiceEvent::Agent {
            event: AgentEvent::ToolStarted {
                call_id: call_id.into(),
                name: "delete_task".into(),
                arguments: serde_json::Value::Null,
            },
        }
    }

    #[tokio::test]
    async fn only_a_confirmation_that_waits_for_a_click_is_asked_in_the_chat() {
        let mut h = Calling::start(|_| says("unused")).await;
        h.ring("c1").await;
        h.until_answered("c1").await;

        // The voice takes this one itself, and the turn goes on.
        h.fake.report(requires("spoken", "the one answered aloud"));
        h.fake.report(VoiceEvent::State { state: VoiceState::Asking });
        h.fake.report(started("spoken"));
        h.fake.report(VoiceEvent::State { state: VoiceState::Thinking });
        // This one needs a tap.
        h.fake.report(requires("tap", "the one that needs a tap"));
        h.fake.report(VoiceEvent::State { state: VoiceState::WaitingForClick });

        let (question, _) = h.connector.chat.wait_for_question().await;
        assert!(question.markdown.contains("the one that needs a tap"), "{}", question.markdown);
        assert_eq!(h.connector.chat.questions().len(), 1, "the spoken one never reached the chat");

        // The turn moves on without it, and the next is asked in its own right.
        h.fake.report(started("tap"));
        h.fake.report(requires("tap2", "the next one"));
        h.fake.report(VoiceEvent::State { state: VoiceState::WaitingForClick });
        h.connector
            .chat
            .until("the next question", |e| {
                e.iter().filter(|e| matches!(e, super::Event::Asked { .. })).count() == 2
            })
            .await;

        h.fake.finish(CallEnd::SessionEnded);
        h.finish().await;
    }

    #[tokio::test]
    async fn a_typed_no_declines_a_confirmation_asked_for_a_call() {
        let mut h = Calling::start(|task| super::deleting(task, "Left it.")).await;
        h.ring("c1").await;
        h.until_answered("c1").await;

        h.fake.start_turn("delete the dentist task");
        h.connector.chat.wait_for_question().await;
        h.say("no").await;
        // The turn goes on to say it left it; wait for the turn's end.
        h.until("the turn to finish", || {
            !h.service.pending().is_running(h.fake.opened()[0].conversation)
        })
        .await;
        h.fake.finish(CallEnd::SessionEnded);
        h.finish().await;

        assert!(h.vault.task(h.task.id).is_ok(), "no is not a yes");
    }

    #[tokio::test]
    async fn a_call_that_cannot_reach_the_phone_says_how_to_fix_it() {
        let mut h = Calling::start(|_| says("unused")).await;
        h.ring("c1").await;
        h.until_answered("c1").await;

        h.fake.finish(CallEnd::Unreachable);
        h.until_ended("c1", HangUp::Unreachable).await;
        h.connector.chat.wait_for_sent(1).await;
        h.finish().await;

        let sent = h.connector.chat.sent();
        assert!(sent[0].contains("couldn't connect"), "{}", sent[0]);
        assert!(sent[0].contains("tailnet"), "{}", sent[0]);
    }

    #[tokio::test]
    async fn a_call_that_breaks_is_hung_up_as_failed() {
        let mut h = Calling::start(|_| says("unused")).await;
        h.ring("c1").await;
        h.until_answered("c1").await;

        h.fake.finish(CallEnd::Failed);
        h.until_ended("c1", HangUp::Failed).await;
        h.finish().await;
    }

    #[tokio::test]
    async fn media_that_cannot_connect_tells_the_owner_hangs_up_failed_and_stops_the_session() {
        let mut h = Calling::start(|_| says("unused")).await;
        *h.fake.connect_fails.lock().unwrap() = Some("the media server is not there".into());

        h.ring("c1").await;
        h.until_ended("c1", HangUp::Failed).await;
        h.connector.chat.wait_for_sent(1).await;
        h.until("the session to be stopped", || h.fake.stops.load(Ordering::SeqCst) == 1).await;
        h.finish().await;

        assert!(h.connector.chat.sent()[0].contains("the media server is not there"));
        assert_eq!(
            h.calls(),
            vec![
                CallEvent::Joined { call: CallId("c1".into()) },
                CallEvent::Ended { call: CallId("c1".into()), why: HangUp::Failed },
            ],
            "joined on the chat service, then left"
        );
    }

    #[tokio::test]
    async fn a_server_that_will_not_let_the_bot_join_fails_the_call() {
        let mut h = Calling::start(|_| says("unused")).await;
        *h.connector.refuse_joins.lock().unwrap() = true;

        h.ring("c1").await;
        h.until_ended("c1", HangUp::Failed).await;
        h.connector.chat.wait_for_sent(1).await;
        h.until("the session to be stopped", || h.fake.stops.load(Ordering::SeqCst) == 1).await;
        h.finish().await;
        assert!(h.connector.chat.sent()[0].contains("couldn't answer that call"));
    }

    #[tokio::test]
    async fn a_call_that_stopped_ringing_while_it_waited_is_not_answered() {
        let mut h = Calling::start(|_| says("unused")).await;
        h.ring_until("c1", Timestamp::now() - SignedDuration::from_secs(5)).await;
        h.until_ended("c1", HangUp::Refused).await;
        h.connector.chat.wait_for_sent(1).await;
        h.finish().await;

        assert!(h.connector.chat.sent()[0].contains("couldn't pick up"));
        assert!(h.fake.opened().is_empty(), "no session was opened for it");
    }

    #[tokio::test]
    async fn a_call_the_owner_gave_up_on_while_it_waited_is_not_answered() {
        let mut h = Calling::start(|_| Script::Hangs).await;
        h.say("write me a very long essay").await;
        h.connector
            .chat
            .until("the turn to begin", |e| {
                e.iter().any(|e| matches!(e, super::Event::Typing { on: true, .. }))
            })
            .await;

        h.ring("c1").await;
        h.hang_up("c1").await;
        h.say("!stop").await;
        h.until_ended("c1", HangUp::Refused).await;
        h.finish().await;

        assert!(h.fake.opened().is_empty(), "no session was opened for it");
        assert!(
            !h.calls().iter().any(|e| matches!(e, CallEvent::Joined { .. })),
            "{:?}",
            h.calls()
        );
    }

    #[tokio::test]
    async fn the_relay_stopping_hangs_up_a_running_call() {
        let mut h = Calling::start(|_| says("unused")).await;
        h.ring("c1").await;
        h.until_answered("c1").await;

        h.halt().await;

        assert_eq!(h.fake.received(), vec![ToCall::End]);
        assert!(
            h.calls()
                .contains(&CallEvent::Ended { call: CallId("c1".into()), why: HangUp::Finished })
        );
        assert!(h.connector.chat.sent().is_empty(), "nothing is said as the relay stops");
    }

    #[tokio::test]
    async fn the_inbox_closing_ends_a_running_call_too() {
        let mut h = Calling::start(|_| says("unused")).await;
        h.ring("c1").await;
        h.until_answered("c1").await;

        h.finish().await;

        assert_eq!(h.fake.received(), vec![ToCall::End]);
        assert!(
            h.calls()
                .contains(&CallEvent::Ended { call: CallId("c1".into()), why: HangUp::Finished })
        );
    }

    #[tokio::test]
    async fn a_redial_is_taken_after_a_queued_call_was_hung_up() {
        let mut h = Calling::start(|_| Script::Hangs).await;
        h.say("write me a very long essay").await;
        h.connector
            .chat
            .until("the turn to begin", |e| {
                e.iter().any(|e| matches!(e, super::Event::Typing { on: true, .. }))
            })
            .await;

        h.ring("c1").await;
        h.hang_up("c1").await;
        h.ring("c2").await;
        h.say("!stop").await;
        h.until_answered("c2").await;

        assert!(
            !h.calls()
                .contains(&CallEvent::Ended { call: CallId("c2".into()), why: HangUp::Refused }),
            "{:?}",
            h.calls()
        );
        assert_eq!(h.connector.chat.sent(), vec!["Stopped."], "never told it was busy");
        h.fake.finish(CallEnd::SessionEnded);
        h.finish().await;
    }

    #[tokio::test]
    async fn a_queued_call_that_rang_out_does_not_keep_the_line() {
        let mut h = Calling::start(|_| Script::Hangs).await;
        h.say("write me a very long essay").await;
        h.connector
            .chat
            .until("the turn to begin", |e| {
                e.iter().any(|e| matches!(e, super::Event::Typing { on: true, .. }))
            })
            .await;

        h.ring_until("c1", Timestamp::now() + SignedDuration::from_millis(200)).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        h.ring("c2").await;
        h.say("!stop").await;
        h.until_answered("c2").await;

        assert!(
            !h.connector.chat.sent().iter().any(|m| m.contains("already on a call")),
            "{:?}",
            h.connector.chat.sent()
        );
        assert!(
            !h.calls()
                .contains(&CallEvent::Ended { call: CallId("c2".into()), why: HangUp::Refused })
        );
        h.fake.finish(CallEnd::SessionEnded);
        h.finish().await;
    }

    #[tokio::test]
    async fn the_relay_stopping_hangs_up_a_call_that_is_still_queued() {
        let mut h = Calling::start(|_| Script::Hangs).await;
        h.say("write me a very long essay").await;
        h.connector
            .chat
            .until("the turn to begin", |e| {
                e.iter().any(|e| matches!(e, super::Event::Typing { on: true, .. }))
            })
            .await;
        h.ring("c1").await;
        // The relay reads `stop` before its inbox, so let it read the call
        // first; there is nothing to see of a call that is merely queued.
        tokio::time::sleep(Duration::from_millis(150)).await;

        h.halt().await;

        assert_eq!(
            h.calls(),
            vec![CallEvent::Ended { call: CallId("c1".into()), why: HangUp::Refused }]
        );
        assert!(h.fake.opened().is_empty(), "and it was never answered");
    }

    #[tokio::test]
    async fn a_confirmation_replaced_by_another_is_declined_not_left_waiting() {
        let mut h = Calling::start(|task| super::deleting(task, "Left it.")).await;
        h.ring("c1").await;
        h.until_answered("c1").await;
        let conversation = h.fake.opened()[0].conversation;

        h.fake.start_turn("delete the dentist task");
        h.connector.chat.wait_for_question().await;
        assert!(h.service.pending().is_running(conversation));

        // The session moves on to another confirmation while the first is
        // still a card. Nobody will answer the first now, so the relay does.
        h.fake.report(requires("next", "the next one"));
        h.connector
            .chat
            .until("the next question", |e| {
                e.iter().filter(|e| matches!(e, super::Event::Asked { .. })).count() == 2
            })
            .await;
        h.until("the first turn to be let go of", || !h.service.pending().is_running(conversation))
            .await;

        assert!(h.vault.task(h.task.id).is_ok(), "declined, so nothing was deleted");
        h.fake.finish(CallEnd::SessionEnded);
        h.finish().await;
    }

    #[tokio::test]
    async fn a_call_whose_worker_panics_is_hung_up_failed_and_the_line_comes_free() {
        let mut h = Calling::start(|_| says("unused")).await;
        h.fake.panic_on_open.store(true, Ordering::SeqCst);

        h.ring("c1").await;
        h.until_ended("c1", HangUp::Failed).await;
        h.connector.chat.wait_for_sent(1).await;

        h.ring("c2").await;
        h.until_answered("c2").await;
        h.fake.finish(CallEnd::SessionEnded);
        h.until_ended("c2", HangUp::Finished).await;
        h.finish().await;
    }

    #[tokio::test]
    async fn a_session_that_ended_in_error_fails_the_call_and_the_owner_is_told() {
        let mut h = Calling::start(|_| says("unused")).await;
        h.ring("c1").await;
        h.until_answered("c1").await;

        h.fake.report(VoiceEvent::Ended { error: Some("the speech model fell over".into()) });
        h.fake.finish(CallEnd::SessionEnded);
        h.until_ended("c1", HangUp::Failed).await;
        h.connector.chat.wait_for_sent(1).await;
        h.finish().await;

        assert!(
            h.connector.chat.sent()[0].contains("the speech model fell over"),
            "{:?}",
            h.connector.chat.sent()
        );
    }

    #[tokio::test]
    async fn a_session_that_ended_as_asked_is_still_a_goodbye() {
        let mut h = Calling::start(|_| says("unused")).await;
        h.ring("c1").await;
        h.until_answered("c1").await;

        h.fake.report(VoiceEvent::Ended { error: None });
        h.fake.finish(CallEnd::SessionEnded);
        h.until_ended("c1", HangUp::Finished).await;
        h.finish().await;
        assert!(h.connector.chat.sent().is_empty());
    }

    #[tokio::test]
    async fn the_owner_hanging_up_as_the_bot_joins_is_not_reported_as_a_failure() {
        let mut h = Calling::start(|_| says("unused")).await;
        *h.connector.hangs_up_during_join.lock().unwrap() = h.inbox.clone();

        h.ring("c1").await;
        h.until_ended("c1", HangUp::Refused).await;
        h.until("the session to be stopped", || h.fake.stops.load(Ordering::SeqCst) == 1).await;
        h.finish().await;

        assert!(h.connector.chat.sent().is_empty(), "{:?}", h.connector.chat.sent());
        assert!(only_ended(&h.calls(), HangUp::Refused), "{:?}", h.calls());
    }

    // ---- calls this side places ----

    /// A routine run's transcript and who it is from, for a call made with
    /// its result; and the outlet the relay registers, once it has.
    async fn calling_with_a_run(h: &Calling) -> (FromRun, Arc<dyn ChatOutlet>) {
        h.connector.chat.set_home(room());
        let thread = Conversation::for_run(karmayoga_core::RoutineRunId::new(), "Morning brief");
        h.vault.save_conversation(&thread).unwrap();
        let from = FromRun { routine: "Morning brief".into(), conversation: Some(thread.id) };
        let outlet = loop {
            if let Some(outlet) = h.service.chat_outlet(h.link.id) {
                break outlet;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        (from, outlet)
    }

    const MARKDOWN: &str = "**Morning brief**\n\nThree things are due.";
    const SPOKEN: &str = "This is your Morning brief. Three things are due.";

    #[tokio::test]
    async fn a_routine_calls_the_owner_and_opens_with_its_result_in_its_conversation() {
        let mut h = Calling::start(|_| says("unused")).await;
        let (from, outlet) = calling_with_a_run(&h).await;
        let conversation = from.conversation;
        let call = tokio::spawn(outlet.call(from, MARKDOWN.into(), SPOKEN.into()));

        h.until("the owner to be rung", || h.calls().contains(&CallEvent::Rang)).await;
        // They pick up: their device joining, as any call.
        h.ring("c1").await;
        let delivered = call.await.unwrap();
        assert_eq!(delivered, Ok(CallDelivered::PickedUp));
        h.until_answered("c1").await;
        h.until("the opening to be said", || !h.fake.said.lock().unwrap().is_empty()).await;

        assert_eq!(*h.fake.said.lock().unwrap(), [SPOKEN]);
        assert_eq!(h.fake.opened()[0].conversation, conversation.unwrap(), "the run's own");

        h.fake.finish(CallEnd::SessionEnded);
        h.until_ended("c1", HangUp::Finished).await;
        h.finish().await;
        // Said, and written into the chat as everything said on a call is --
        // once, and not as a result that could not be said.
        assert_eq!(h.connector.chat.sent(), [MARKDOWN], "{:?}", h.connector.chat.sent());
    }

    #[tokio::test]
    async fn what_is_said_on_a_call_is_written_into_the_chat() {
        let mut h = Calling::start(|_| says("unused")).await;
        h.ring("c1").await;
        h.until_answered("c1").await;

        h.fake.report(VoiceEvent::Heard { text: "what's on today?".into() });
        h.fake.report(VoiceEvent::State { state: VoiceState::Thinking });
        for event in [
            AgentEvent::Started { message_id: "m1".into() },
            AgentEvent::Delta { text: "Just the dentist at three.".into() },
            AgentEvent::Finished { message_id: "m1".into() },
        ] {
            h.fake.report(VoiceEvent::Agent { event });
        }
        h.fake.report(VoiceEvent::State { state: VoiceState::Speaking });
        h.fake.report(VoiceEvent::State { state: VoiceState::Listening });
        h.connector.chat.until("the reply", |e| e.iter().filter(|e| is_sent(e)).count() == 2).await;
        // Talked over: only what was heard is written.
        h.fake.report(VoiceEvent::Heard { text: "and tomorrow?".into() });
        for event in [
            AgentEvent::Started { message_id: "m2".into() },
            AgentEvent::Delta { text: "Tomorrow is free. Friday is not.".into() },
            AgentEvent::Finished { message_id: "m2".into() },
        ] {
            h.fake.report(VoiceEvent::Agent { event });
        }
        h.fake.report(VoiceEvent::ReplyCut {
            message_id: "m2".into(),
            text: "Tomorrow is free.".into(),
        });

        h.fake.finish(CallEnd::Ended);
        h.until_ended("c1", HangUp::Finished).await;
        h.finish().await;
        let sent = h.connector.chat.sent();
        assert_eq!(sent.len(), 4, "{sent:?}");
        assert!(sent[0].starts_with("> ") && sent[0].contains("what's on today?"), "{sent:?}");
        assert_eq!(sent[1], "Just the dentist at three.");
        assert!(sent[2].starts_with("> ") && sent[2].contains("and tomorrow?"), "{sent:?}");
        assert_eq!(sent[3], "Tomorrow is free.");
    }

    fn is_sent(event: &super::Event) -> bool {
        matches!(event, super::Event::Sent { .. })
    }

    #[tokio::test]
    async fn a_call_nobody_picks_up_stops_ringing_and_the_result_is_written() {
        let timing = Timing { ring_for: Duration::from_millis(300), ..Timing::default() };
        let mut h = Calling::start_with(|_| says("unused"), true, timing).await;
        let (from, outlet) = calling_with_a_run(&h).await;

        let delivered = outlet.call(from, MARKDOWN.into(), SPOKEN.into()).await;
        assert_eq!(delivered, Ok(CallDelivered::Messaged { why: "they did not pick up".into() }));
        assert_eq!(
            h.calls(),
            [
                CallEvent::Rang,
                CallEvent::Ended { call: CallId("ring".into()), why: HangUp::Unreachable }
            ],
            "the ringing is stopped"
        );
        let sent = h.connector.chat.sent();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("you didn't pick up") && sent[0].ends_with(MARKDOWN), "{sent:?}");
        assert!(h.fake.opened().is_empty(), "no voice session for nobody");
        h.finish().await;
    }

    #[tokio::test]
    async fn a_routine_does_not_call_while_another_call_runs_and_writes_instead() {
        let mut h = Calling::start(|_| says("unused")).await;
        h.ring("c1").await;
        h.until_answered("c1").await;

        let (from, outlet) = calling_with_a_run(&h).await;
        let delivered = tokio::spawn(outlet.call(from, MARKDOWN.into(), SPOKEN.into()));
        // Long enough for the job to reach the dispatcher, which decides
        // there and then, while the call still has the line. Nothing outside
        // shows the moment it has.
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Written, not rung: after the call they are on, as anything else
        // for this chat is.
        h.fake.finish(CallEnd::SessionEnded);
        h.until_ended("c1", HangUp::Finished).await;
        assert_eq!(
            delivered.await.unwrap(),
            Ok(CallDelivered::Messaged { why: "another call had the line".into() })
        );
        assert!(!h.calls().contains(&CallEvent::Rang), "the owner was not rung after it");
        let sent = h.connector.chat.sent();
        assert!(sent.iter().any(|m| m.contains("already on a call") && m.ends_with(MARKDOWN)));
        h.finish().await;
    }

    #[tokio::test]
    async fn a_call_they_pick_up_that_cannot_be_joined_stops_ringing() {
        let mut h = Calling::start(|_| says("unused")).await;
        *h.connector.refuse_joins.lock().unwrap() = true;
        let (from, outlet) = calling_with_a_run(&h).await;
        let delivered = tokio::spawn(outlet.call(from, MARKDOWN.into(), SPOKEN.into()));

        h.until("the owner to be rung", || h.calls().contains(&CallEvent::Rang)).await;
        h.ring("c1").await;
        // Picked up, but never said: the result is written after all, and
        // the run is told so rather than that it was delivered.
        assert_eq!(
            delivered.await.unwrap(),
            Ok(CallDelivered::Messaged { why: "the call they picked up did not go ahead".into() })
        );
        h.until_ended("c1", HangUp::Failed).await;
        // The membership that rang them is taken down too: joining, which
        // would have taken it over, never happened.
        h.until_ended("ring", HangUp::Finished).await;
        h.finish().await;

        let sent = h.connector.chat.sent();
        assert!(sent.iter().any(|m| m.contains("wouldn't let me join")), "{sent:?}");
        assert!(sent.iter().any(|m| m.ends_with(MARKDOWN)), "the result, written: {sent:?}");
    }

    #[tokio::test]
    async fn stop_stops_the_ringing_and_the_result_is_written() {
        let mut h = Calling::start(|_| says("unused")).await;
        let (from, outlet) = calling_with_a_run(&h).await;
        let delivered = tokio::spawn(outlet.call(from, MARKDOWN.into(), SPOKEN.into()));
        h.until("the owner to be rung", || h.calls().contains(&CallEvent::Rang)).await;

        h.say("!stop").await;
        assert_eq!(
            delivered.await.unwrap(),
            Ok(CallDelivered::Messaged { why: "the call was stopped".into() })
        );
        h.until_ended("ring", HangUp::Finished).await;
        h.finish().await;
        let sent = h.connector.chat.sent();
        assert!(sent.contains(&"Stopped.".to_string()), "{sent:?}");
        assert!(sent.iter().any(|m| m.contains("stopped calling") && m.ends_with(MARKDOWN)));
    }

    #[tokio::test]
    async fn a_link_that_cannot_ring_writes_the_result_and_says_why() {
        let mut h = Calling::start(|_| says("unused")).await;
        *h.connector.refuse_rings.lock().unwrap() = true;
        let (from, outlet) = calling_with_a_run(&h).await;

        let delivered = outlet.call(from, MARKDOWN.into(), SPOKEN.into()).await;
        let Ok(CallDelivered::Messaged { why }) = delivered else {
            panic!("written instead: {delivered:?}");
        };
        assert!(why.contains("no calls here"), "{why}");
        assert!(h.connector.chat.sent()[0].ends_with(MARKDOWN));
        h.finish().await;
    }
}
