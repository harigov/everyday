//! The relay, end to end, against a fake chat service and a fake model.
//!
//! What the pure functions in `relay.rs` cannot say: that the pieces are wired
//! together the way the design describes. A message becomes one reply; a
//! destructive call stops to ask, and the answer -- a tap, or typed words --
//! decides what happens to the thing it was about; `!new`, `!stop` and a
//! message too old to act on do what they say. The model is a `TcpListener` on
//! loopback speaking the Chat Completions shape, the way
//! `everyday-service/tests/routines.rs` does it; the chat service is a
//! [`support::FakeConnector`] that writes down what it is asked to do.
//!
//! The second half is `run_link_with`: one link's life around the relay --
//! what the connector is given, what each way of ending means to the
//! supervisor, and what the settings pane is told.

#[allow(dead_code)]
mod support;

use everyday_chat::connector::{ChatError, Choice, Connector, Incoming, Picture, Setup};
use everyday_chat::relay::Timing;
use everyday_chat::run_link_with;
use everyday_core::agent::Role;
use everyday_core::chat::{ChatSecret, ChatSettings, Via};
use everyday_service::chat::{ChatState, FromRun};
use everyday_service::supervisor::Outcome;
use jiff::{SignedDuration, Timestamp};
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
fn saved_link(endpoint: &str) -> (support::World, everyday_core::chat::ChatLink) {
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
fn routine_run(h: &Harness, name: &str) -> (everyday_core::Conversation, FromRun) {
    let thread = everyday_core::Conversation::for_run(everyday_core::RoutineRunId::new(), name);
    h.vault.save_conversation(&thread).unwrap();
    let from = FromRun { routine: name.into(), conversation: Some(thread.id) };
    (thread, from)
}

/// The outlet the relay registers once it is running -- a moment after it is
/// spawned, so waited for.
async fn outlet(h: &Harness) -> Arc<dyn everyday_service::chat::ChatOutlet> {
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
    assert_eq!(answer, everyday_service::agent::ConfirmAnswer::Confirm);
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
