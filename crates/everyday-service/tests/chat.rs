//! Chat links, through the command layer, and a chat message as a turn of the
//! assistant.
//!
//! Three things are checked, none of which needs a chat server:
//!
//! * the four commands (`chat_links`, `new_chat_link`, `save_chat_link`,
//!   `remove_chat_link`), against a real vault, with what a client may and may
//!   not be told about a link's sign-in;
//! * `reconcile` -- what runs, what stops, and whether the vault keeps its key
//!   -- against a fake [`ChatRunner`] that records when it is started and when
//!   it is told to stop, standing in for `everyday-chat`;
//! * `Service::chat_turn`, against a fake model on loopback, the pattern
//!   `tests/routines.rs` uses: the conversation it creates, what the model is
//!   told, and the one thing a chat turn does differently from the rail's --
//!   a destructive call always stops to ask.

use everyday_core::chat::{ChatSecret, Via};
use everyday_core::{ChatLinkId, ConversationId, Task, Vault};
use everyday_service::agent::{AgentEvent, ConfirmAnswer, Sink};
use everyday_service::chat::{ChatRunner, ChatState, state_dir, store_passphrase, task_key};
use everyday_service::ctx::{Ctx, Scope};
use everyday_service::events::{Change, EventSink, Kind, Op};
use everyday_service::supervisor::{Outcome, Supervisor, TaskFuture, TaskState};
use everyday_service::{Service, error::CommandError};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

#[allow(dead_code)]
mod support;

// ---- fixtures ---------------------------------------------------------------

async fn call(svc: &Arc<Service>, name: &str, args: Value) -> Value {
    svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"))
}

async fn fails(svc: &Arc<Service>, name: &str, args: Value) -> CommandError {
    svc.call(Ctx::local(), name, args).await.expect_err("expected a failure")
}

/// A filled-in link, not yet saved: what the settings form would send.
async fn a_link(svc: &Arc<Service>) -> Value {
    let mut link = call(svc, "new_chat_link", json!({})).await;
    link["service"]["homeserver"] = json!("http://127.0.0.1:6167");
    link["service"]["userId"] = json!("@everyday:example.org");
    link["service"]["owner"] = json!("@owner:example.org");
    link
}

fn id_of(link: &Value) -> ChatLinkId {
    link["id"].as_str().unwrap().parse().unwrap()
}

/// Poll until `ok` holds, or give up with a message saying what never did.
async fn eventually(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..300 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never happened: {what}");
}

/// A password-protected vault, for the tests about locking.
const PASSWORD: &str = "correct horse battery staple";

fn encrypted_service() -> (Arc<Service>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = everyday_core::VaultConfig {
        name: "Test".into(),
        backend: "sqlite".into(),
        settings: Default::default(),
        password: Some(PASSWORD.into()),
        kdf: everyday_core::crypto::KdfParams::insecure_fast(),
        auto_lock_seconds: 900,
        forget_key_seconds: 0,
    };
    let vault = everyday_vault::create(dir.path(), config).unwrap();
    vault.save_journal(&everyday_core::Journal::new("Test")).unwrap();
    let svc = Arc::new(Service::new());
    svc.set(vault);
    (svc, dir)
}

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

// ---- the commands -----------------------------------------------------------

#[tokio::test]
async fn a_new_link_is_switched_on_empty_and_not_yet_saved() {
    let (svc, _dir) = support::vault::service(None);

    let link = call(&svc, "new_chat_link", json!({})).await;
    assert_eq!(link["enabled"], true);
    assert_eq!(link["service"]["kind"], "matrix");
    assert_eq!(link["service"]["homeserver"], "");
    let again = call(&svc, "new_chat_link", json!({})).await;
    assert_ne!(link["id"], again["id"], "every blank link is its own");

    let listed = call(&svc, "chat_links", json!({})).await;
    assert_eq!(listed, json!([]), "minting is not saving");
}

#[tokio::test]
async fn a_link_is_saved_listed_and_removed() {
    let (svc, _dir) = support::vault::service(None);
    let link = a_link(&svc).await;

    let saved = call(&svc, "save_chat_link", json!({ "link": link })).await;
    assert_eq!(saved["link"]["id"], link["id"]);
    assert_eq!(saved["link"]["service"]["userId"], "@everyday:example.org");
    assert_eq!(saved["signedIn"], false);
    assert_eq!(saved["hasPassword"], false);
    // Nothing installed a runner on this service, so nothing is running it,
    // and the pane is told why rather than left with a blank.
    assert_eq!(saved["status"]["state"], "off");
    assert!(!saved["status"]["detail"].as_str().unwrap().is_empty());

    let listed = call(&svc, "chat_links", json!({})).await;
    let listed = listed.as_array().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["link"]["id"], link["id"]);

    // Saved again, it is updated rather than added, and keeps the moment it
    // was first saved whatever the client says that was.
    let mut edited = link.clone();
    edited["service"]["owner"] = json!("@someone.else:example.org");
    edited["createdAt"] = json!("2000-01-01T00:00:00Z");
    let resaved = call(&svc, "save_chat_link", json!({ "link": edited })).await;
    assert_eq!(resaved["link"]["service"]["owner"], "@someone.else:example.org");
    assert_eq!(resaved["link"]["createdAt"], saved["link"]["createdAt"]);
    assert_eq!(call(&svc, "chat_links", json!({})).await.as_array().unwrap().len(), 1);

    call(&svc, "remove_chat_link", json!({ "id": link["id"] })).await;
    assert_eq!(call(&svc, "chat_links", json!({})).await, json!([]));
    let gone = fails(&svc, "remove_chat_link", json!({ "id": link["id"] })).await;
    assert_eq!(gone.code, "not_found", "there is nothing left to remove");
}

#[tokio::test]
async fn saving_and_removing_a_link_tell_every_window_the_settings_changed() {
    let (svc, _dir) = support::vault::service(None);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let link = a_link(&svc).await;

    call(&svc, "save_chat_link", json!({ "link": link })).await;
    call(&svc, "remove_chat_link", json!({ "id": link["id"] })).await;

    let changes = events.take();
    assert_eq!(changes.len(), 2, "one change for each, and nothing else: {changes:?}");
    for change in changes {
        assert_eq!((change.kind, change.op), (Kind::Settings, Op::Updated));
        assert_eq!(change.origin.as_deref(), Some("local"));
    }
}

#[tokio::test]
async fn an_invalid_link_is_refused_and_nothing_is_stored() {
    let (svc, _dir) = support::vault::service(None);
    let mut link = a_link(&svc).await;

    link["service"]["homeserver"] = json!("example.org:6167");
    let e = fails(&svc, "save_chat_link", json!({ "link": link, "password": "pw" })).await;
    assert_eq!(e.code, "invalid");
    assert!(e.message.contains("https://"), "{}", e.message);

    link["service"]["homeserver"] = json!("http://127.0.0.1:6167");
    link["service"]["userId"] = json!("everyday");
    assert_eq!(fails(&svc, "save_chat_link", json!({ "link": link })).await.code, "invalid");

    // The bot answering as the person it answers is the one slip that would
    // make it reply to itself.
    link["service"]["userId"] = json!("@owner:example.org");
    assert_eq!(fails(&svc, "save_chat_link", json!({ "link": link })).await.code, "invalid");

    assert_eq!(call(&svc, "chat_links", json!({})).await, json!([]));
}

#[tokio::test]
async fn a_password_makes_has_password_true_and_is_never_sent_back() {
    let (svc, _dir) = support::vault::service(None);
    let link = a_link(&svc).await;

    let saved =
        call(&svc, "save_chat_link", json!({ "link": link, "password": "hunter2-hunter2" })).await;
    assert_eq!(saved["hasPassword"], true);
    assert_eq!(saved["signedIn"], false);
    let listed = call(&svc, "chat_links", json!({})).await;
    assert_eq!(listed[0]["hasPassword"], true);
    assert!(
        !saved.to_string().contains("hunter2") && !listed.to_string().contains("hunter2"),
        "the password goes in and does not come out"
    );

    // The secret itself is in the vault, where the connector will read it.
    let vault = svc.get().unwrap();
    let secret = vault.chat_secret(id_of(&link)).unwrap();
    assert_eq!(secret.password.as_deref(), Some("hunter2-hunter2"));

    // A form that leaves the box empty, or blank, changes nothing.
    for password in [json!(null), json!(""), json!("   ")] {
        let again =
            call(&svc, "save_chat_link", json!({ "link": link, "password": password })).await;
        assert_eq!(again["hasPassword"], true, "{password}");
    }
    assert_eq!(
        vault.chat_secret(id_of(&link)).unwrap().password.as_deref(),
        Some("hunter2-hunter2")
    );

    // Removing the link takes its secret with it.
    call(&svc, "remove_chat_link", json!({ "id": link["id"] })).await;
    assert_eq!(vault.chat_secret(id_of(&link)).unwrap(), ChatSecret::default());
}

#[tokio::test]
async fn changing_the_bots_account_drops_its_session_and_its_files() {
    let (svc, _dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let link = a_link(&svc).await;
    let id = id_of(&link);
    call(&svc, "save_chat_link", json!({ "link": link })).await;

    // What a connector that had signed in would have left behind.
    let signed_in = ChatSecret {
        password: None,
        session: Some(json!({ "user_id": "@everyday:example.org", "access_token": "t" })),
        recovery_key: Some("EsT1 recovery key".into()),
    };
    vault.save_chat_secret(id, &signed_in).unwrap();
    let files = state_dir(&vault, id);
    std::fs::create_dir_all(&files).unwrap();
    std::fs::write(files.join("matrix-sdk-state.sqlite3"), b"keys").unwrap();
    let listed = call(&svc, "chat_links", json!({})).await;
    assert_eq!(listed[0]["signedIn"], true);

    // The person it answers, and a trailing slash on the address, are not a
    // different account: the sign-in is still good for it.
    let mut same = link.clone();
    same["service"]["owner"] = json!("@someone.else:example.org");
    same["service"]["homeserver"] = json!("http://127.0.0.1:6167/");
    let kept = call(&svc, "save_chat_link", json!({ "link": same })).await;
    assert_eq!(kept["signedIn"], true);
    assert_eq!(vault.chat_secret(id).unwrap(), signed_in);
    assert!(files.join("matrix-sdk-state.sqlite3").exists());

    // A different bot account is a different device with different keys.
    let mut other = same.clone();
    other["service"]["userId"] = json!("@everyday2:example.org");
    let dropped = call(&svc, "save_chat_link", json!({ "link": other })).await;
    assert_eq!(dropped["signedIn"], false);
    let secret = vault.chat_secret(id).unwrap();
    assert!(secret.session.is_none());
    assert!(secret.recovery_key.is_none());
    assert!(!files.exists(), "the old account's encryption keys are gone");
}

#[tokio::test]
async fn managing_chat_links_needs_the_assistants_scope() {
    let (svc, _dir) = support::vault::service(None);
    let link = a_link(&svc).await;
    let reader = Ctx { scopes: vec![Scope::Journals], ..Ctx::local() };

    for (name, args) in [
        ("chat_links", json!({})),
        ("new_chat_link", json!({})),
        ("save_chat_link", json!({ "link": link })),
        ("remove_chat_link", json!({ "id": link["id"] })),
    ] {
        let e = svc.call(reader.clone(), name, args).await.expect_err(name);
        assert_eq!(e.code, "forbidden", "{name}");
    }
    let agent = Ctx { scopes: vec![Scope::Agent], ..Ctx::local() };
    assert!(svc.call(agent, "chat_links", json!({})).await.is_ok());
}

// ---- where a link keeps its things -------------------------------------------

#[tokio::test]
async fn a_links_files_and_store_passphrase_are_its_own() {
    let (svc, dir) = support::vault::service(None);
    let vault = svc.get().unwrap();
    let (a, b) = (ChatLinkId::new(), ChatLinkId::new());

    assert_eq!(state_dir(&vault, a), vault.store_root().join("chat").join(a.to_string()));
    assert!(state_dir(&vault, a).starts_with(dir.path()), "inside the vault, not in a config dir");
    assert_ne!(state_dir(&vault, a), state_dir(&vault, b));

    let (pa, pa_again, pb) = (
        store_passphrase(&vault, a).unwrap(),
        store_passphrase(&vault, a).unwrap(),
        store_passphrase(&vault, b).unwrap(),
    );
    assert_eq!(pa, pa_again, "the same link opens its store the same way every time");
    assert_ne!(pa, pb, "and no two links share one");
    assert_eq!(pa.len(), 64, "a 256-bit key, in hex");
    assert!(pa.chars().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn a_locked_vault_gives_no_store_passphrase() {
    let (svc, _dir) = encrypted_service();
    let vault = svc.get().unwrap();
    let link = ChatLinkId::new();
    assert!(store_passphrase(&vault, link).is_ok());

    vault.lock();
    let e = store_passphrase(&vault, link).unwrap_err();
    assert_eq!(e.code(), "locked");
}

// ---- running links -----------------------------------------------------------

/// What a [`FakeRunner`] saw, kept apart from it so a test can read it after
/// the runner has been handed to the service.
#[derive(Default)]
struct Seen {
    starts: Mutex<Vec<ChatLinkId>>,
    stops: Mutex<Vec<ChatLinkId>>,
}

impl Seen {
    fn started(&self, link: ChatLinkId) -> usize {
        self.starts.lock().unwrap().iter().filter(|l| **l == link).count()
    }

    fn stopped(&self, link: ChatLinkId) -> usize {
        self.stops.lock().unwrap().iter().filter(|l| **l == link).count()
    }
}

/// A runner that does nothing until it is told to stop, like a connector
/// that has signed in and is waiting for a message.
struct FakeRunner(Arc<Seen>);

impl ChatRunner for FakeRunner {
    fn run(
        &self,
        _service: Arc<Service>,
        _vault: Arc<Vault>,
        link: ChatLinkId,
        mut stop: watch::Receiver<bool>,
    ) -> TaskFuture {
        let seen = self.0.clone();
        seen.starts.lock().unwrap().push(link);
        Box::pin(async move {
            let _ = stop.wait_for(|stopped| *stopped).await;
            seen.stops.lock().unwrap().push(link);
            Ok(Outcome::Done)
        })
    }
}

/// A service with a runner installed on a supervisor of its own, and the
/// vault's idle timer on, so keeping the key shows.
fn running_service() -> (Arc<Service>, Arc<Seen>, Arc<Collector>, tempfile::TempDir) {
    let (svc, dir) = support::vault::service(None);
    svc.get().unwrap().set_forget_key(600).unwrap();
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    svc.set_supervisor(Arc::new(Supervisor::new(svc.events())));
    let seen = Arc::new(Seen::default());
    svc.set_chat_runner(Arc::new(FakeRunner(seen.clone())));
    (svc, seen, events, dir)
}

#[tokio::test]
async fn a_switched_on_link_runs_a_switched_off_one_stops_and_the_vault_keeps_its_key() {
    let (svc, seen, events, _dir) = running_service();
    let vault = svc.get().unwrap();
    assert!(
        vault.seconds_until_forget_key().is_some(),
        "with no link on, the idle timer is counting down"
    );

    let link = a_link(&svc).await;
    let id = id_of(&link);
    let saved = call(&svc, "save_chat_link", json!({ "link": link })).await;
    assert_eq!(saved["status"]["state"], "connecting", "started, and has not said anything yet");
    eventually("the link to start", || seen.started(id) == 1).await;
    assert_eq!(vault.seconds_until_forget_key(), None, "a bot is waiting on the key");
    assert!(matches!(svc.supervisor().state(&task_key(id)), Some(TaskState::Running)));

    // The connector says where it has got to; the pane is told, and told by
    // the same change the supervisor uses for the task.
    events.take();
    svc.report_chat_status(id, ChatState::Online, "Signed in as @everyday:example.org.");
    let changes = events.take();
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!((changes[0].kind, changes[0].op), (Kind::BackgroundTask, Op::Updated));
    assert_eq!(changes[0].id.as_deref(), Some(task_key(id).as_str()));
    assert_eq!(changes[0].origin, None);
    let listed = call(&svc, "chat_links", json!({})).await;
    assert_eq!(listed[0]["status"]["state"], "online");
    assert_eq!(listed[0]["status"]["detail"], "Signed in as @everyday:example.org.");

    // Switched off, it is stopped -- and waited for -- before the save
    // returns, and the key goes back on its timer.
    let mut off = link.clone();
    off["enabled"] = json!(false);
    let saved = call(&svc, "save_chat_link", json!({ "link": off })).await;
    assert_eq!(seen.stopped(id), 1);
    assert_eq!(saved["status"]["state"], "off");
    assert_eq!(seen.started(id), 1, "and not started again");
    assert!(vault.seconds_until_forget_key().is_some());

    // Switched back on, it starts afresh.
    let saved = call(&svc, "save_chat_link", json!({ "link": link })).await;
    assert_eq!(saved["status"]["state"], "connecting");
    eventually("the link to start again", || seen.started(id) == 2).await;
    assert_eq!(vault.seconds_until_forget_key(), None);

    // Removed, it stops for good.
    call(&svc, "remove_chat_link", json!({ "id": link["id"] })).await;
    assert_eq!(seen.stopped(id), 2);
    assert_eq!(svc.supervisor().state(&task_key(id)), Some(TaskState::Stopped));
    assert!(vault.seconds_until_forget_key().is_some());
}

#[tokio::test]
async fn saving_a_link_restarts_it_so_new_settings_take_effect() {
    let (svc, seen, _events, _dir) = running_service();
    let link = a_link(&svc).await;
    let id = id_of(&link);
    call(&svc, "save_chat_link", json!({ "link": link })).await;
    eventually("the link to start", || seen.started(id) == 1).await;

    let mut edited = link.clone();
    edited["freshAfterMinutes"] = json!(0);
    call(&svc, "save_chat_link", json!({ "link": edited })).await;

    assert_eq!(seen.stopped(id), 1, "the old task was ended");
    eventually("a new task to start", || seen.started(id) == 2).await;
}

#[tokio::test]
async fn saving_one_link_leaves_another_running_untouched() {
    let (svc, seen, _events, _dir) = running_service();
    let first = a_link(&svc).await;
    let mut second = a_link(&svc).await;
    second["service"]["userId"] = json!("@everyday-two:example.org");
    let (a, b) = (id_of(&first), id_of(&second));

    call(&svc, "save_chat_link", json!({ "link": first })).await;
    eventually("the first link to start", || seen.started(a) == 1).await;
    call(&svc, "save_chat_link", json!({ "link": second })).await;
    eventually("the second link to start", || seen.started(b) == 1).await;

    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(seen.started(a), 1, "the first was not restarted by the second's save");
    assert_eq!(seen.stopped(a), 0);

    call(&svc, "remove_chat_link", json!({ "id": second["id"] })).await;
    assert_eq!(seen.stopped(b), 1);
    assert_eq!(seen.stopped(a), 0, "or by its removal");
    assert!(matches!(svc.supervisor().state(&task_key(a)), Some(TaskState::Running)));
}

#[tokio::test]
async fn installing_the_runner_starts_the_links_already_saved() {
    // `everyday serve --keychain`: the vault is open and unlocked before the
    // shell gets as far as handing over the runner, and unlocking is not
    // going to happen again.
    let (svc, _dir) = support::vault::service(None);
    svc.get().unwrap().set_forget_key(600).unwrap();
    svc.set_supervisor(Arc::new(Supervisor::new(svc.events())));
    let link = a_link(&svc).await;
    let id = id_of(&link);
    call(&svc, "save_chat_link", json!({ "link": link })).await;
    assert_eq!(
        svc.get().unwrap().seconds_until_forget_key(),
        None,
        "the key is kept for a link that is on, even before anything can run it"
    );

    let seen = Arc::new(Seen::default());
    svc.set_chat_runner(Arc::new(FakeRunner(seen.clone())));
    eventually("the saved link to start", || seen.started(id) == 1).await;
}

#[tokio::test]
async fn locking_stops_the_bot_and_unlocking_starts_it_again_once() {
    let (svc, _dir) = encrypted_service();
    svc.set_supervisor(Arc::new(Supervisor::new(svc.events())));
    let seen = Arc::new(Seen::default());
    svc.set_chat_runner(Arc::new(FakeRunner(seen.clone())));
    let vault = svc.get().unwrap();

    let link = a_link(&svc).await;
    let id = id_of(&link);
    call(&svc, "save_chat_link", json!({ "link": link })).await;
    eventually("the link to start", || seen.started(id) == 1).await;

    // Locking by hand ends it: a locked vault has no key for a bot to answer
    // with.
    vault.lock();
    svc.locked().await;
    assert_eq!(seen.stopped(id), 1);
    assert_eq!(svc.supervisor().state(&task_key(id)), Some(TaskState::Stopped));

    vault.unlock(Some(PASSWORD)).unwrap();
    svc.unlocked();
    eventually("the link to start after the unlock", || seen.started(id) == 2).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(seen.started(id), 2, "started once, not once by each way it can be asked");
}

#[tokio::test]
async fn a_link_switched_off_while_locked_is_not_brought_back_by_the_unlock() {
    let (svc, _dir) = encrypted_service();
    svc.set_supervisor(Arc::new(Supervisor::new(svc.events())));
    let seen = Arc::new(Seen::default());
    svc.set_chat_runner(Arc::new(FakeRunner(seen.clone())));
    let vault = svc.get().unwrap();

    let link = a_link(&svc).await;
    let id = id_of(&link);
    call(&svc, "save_chat_link", json!({ "link": link })).await;
    eventually("the link to start", || seen.started(id) == 1).await;
    let mut off = link.clone();
    off["enabled"] = json!(false);
    call(&svc, "save_chat_link", json!({ "link": off })).await;
    assert_eq!(seen.stopped(id), 1);

    vault.lock();
    svc.locked().await;
    vault.unlock(Some(PASSWORD)).unwrap();
    svc.unlocked();

    // The supervisor kept the factory of a task that was stopped, so the
    // unlock may start it for a moment; `reconcile` is what ends that. Either
    // way nothing is left running afterwards.
    eventually("a link that is off to be stopped", || {
        !matches!(
            svc.supervisor().state(&task_key(id)),
            Some(TaskState::Starting | TaskState::Running | TaskState::Backoff { .. })
        )
    })
    .await;
    assert_eq!(seen.started(id), seen.stopped(id), "everything that started was stopped");
}

#[tokio::test]
async fn a_read_only_copy_of_the_vault_runs_nothing() {
    // The write claim is what lets a process answer the phone: a reply is a
    // write, and two windows must not both give one. A second process on a
    // vault that is already open for writing reads, and so does not start
    // the links.
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
    let writer = everyday_vault::create(dir.path(), config).unwrap();
    let svc = Arc::new(Service::new());
    let writer = svc.set(writer);
    let link = {
        let mut link = everyday_core::chat::ChatLink::new_matrix();
        link.service = everyday_core::chat::ChatService::Matrix(everyday_core::chat::MatrixLink {
            homeserver: "http://127.0.0.1:6167".into(),
            user_id: "@everyday:example.org".into(),
            owner: "@owner:example.org".into(),
        });
        link
    };
    writer
        .save_chat_settings(&everyday_core::chat::ChatSettings { links: vec![link.clone()] })
        .unwrap();

    let reader = everyday_vault::open(dir.path()).unwrap();
    if reader.is_writable() {
        // Another process's claim is an OS lock this platform did not give
        // us; there is no read-only copy to test with.
        return;
    }
    reader.unlock(None).unwrap();
    let second = Arc::new(Service::new());
    let seen = Arc::new(Seen::default());
    second.set_chat_runner(Arc::new(FakeRunner(seen.clone())));
    let _reader = second.set(reader);
    second.unlocked();

    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(seen.started(link.id), 0);
    assert_eq!(second.supervisor().state(&task_key(link.id)), None);
}

// ---- a chat message as a turn -------------------------------------------------

/// What the fake model does when it is asked.
#[derive(Clone)]
enum Script {
    /// One reply, no tools.
    Says(String),
    /// Ask for a tool by name with these arguments, then say something.
    Calls { tool: String, arguments: String, then: String },
}

fn says(text: &str) -> Script {
    Script::Says(text.to_string())
}

fn calls(tool: &str, arguments: String, then: &str) -> Script {
    Script::Calls { tool: tool.to_string(), arguments, then: then.to_string() }
}

/// A model on loopback, and every request it was sent. Dropping the handle
/// stops it.
struct FakeModel {
    endpoint: String,
    requests: Arc<Mutex<Vec<String>>>,
    _shutdown: watch::Sender<bool>,
}

/// Read one HTTP request whole: its head, then as many bytes of body as
/// `Content-Length` promised. A single `read` returns whatever has arrived,
/// and a request carrying every tool's schema is big enough to arrive in
/// pieces.
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
        if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&data[..end]).to_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if data.len() >= end + 4 + length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&data).into_owned()
}

async fn fake_model(script: Script) -> FakeModel {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, mut rx) = watch::channel(false);
    let requests: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = requests.clone();

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
            let seen = seen.clone();
            asked += 1;
            let turn = asked;
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let request = read_request(&mut socket).await;
                seen.lock().unwrap().push(request);
                let body = match &script {
                    Script::Says(text) => reply(text, None),
                    Script::Calls { tool, arguments, then } => {
                        if turn == 1 {
                            reply("", Some((tool, arguments)))
                        } else {
                            reply(then, None)
                        }
                    }
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

    FakeModel { endpoint: format!("http://127.0.0.1:{port}/v1"), requests, _shutdown: tx }
}

/// One answer, as a stream of Chat Completions chunks.
///
/// The harness streams every turn, so this has to be `text/event-stream` with
/// `data:` frames rather than one JSON document -- a plain body is refused on
/// its content type before anything reads it.
fn reply(text: &str, call: Option<(&str, &str)>) -> String {
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

    match call {
        Some((tool, arguments)) => {
            frame(
                json!({
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
            frame(json!({}), Some("tool_calls"));
        }
        None => {
            frame(json!({ "role": "assistant", "content": text }), None);
            frame(json!({}), Some("stop"));
        }
    }
    out.push_str("data: [DONE]\n\n");
    out
}

/// A vault with a journal and an assistant pointed at `endpoint` -- with
/// `confirm_destructive` off, which is exactly what a chat turn must not
/// honour.
fn assistant_service(endpoint: &str) -> (Arc<Service>, tempfile::TempDir) {
    let (svc, dir) = support::vault::service(None);
    let settings = everyday_core::AgentSettings {
        enabled: true,
        // Everything is reckoned in UTC so the tests are about the chat
        // rather than about where the machine running them is.
        timezone: Some("UTC".into()),
        confirm_destructive: false,
        ..Default::default()
    };
    let vault = svc.get().unwrap();
    vault.save_agent_settings(&settings).unwrap();
    let mut models = everyday_core::llm::ModelSettings::default();
    models.medium.connection.base_url = Some(endpoint.to_string());
    models.medium.model.model = "scripted".into();
    vault.save_model_settings(&models).unwrap();
    (svc, dir)
}

fn via() -> Via {
    Via { link: ChatLinkId::new(), service: "Matrix".into(), chat: "!room:example.org".into() }
}

fn silent() -> Sink {
    Arc::new(|_| {})
}

#[tokio::test]
async fn a_chat_turn_creates_its_conversation_marked_with_where_it_came_from() {
    let model = fake_model(says("Two things are due today.")).await;
    let (svc, _dir) = assistant_service(&model.endpoint);
    let events = Arc::new(Collector::default());
    svc.set_events(events.clone());
    let vault = svc.get().unwrap();
    let (via, conversation) = (via(), ConversationId::new());

    let turned = svc
        .chat_turn(via.clone(), conversation, "What is due today?".into(), silent())
        .await
        .expect("a chat turn");
    assert_eq!(turned.text, "Two things are due today.");
    assert!(!turned.stopped);

    let stored = vault.conversation(conversation).expect("the turn made its conversation");
    assert_eq!(stored.via, Some(via.clone()));
    let said: Vec<_> =
        vault.messages(conversation).unwrap().into_iter().map(|m| m.content).collect();
    assert!(said.contains(&"What is due today?".to_string()), "{said:?}");
    assert!(said.contains(&"Two things are due today.".to_string()), "{said:?}");

    // A second message in the same chat carries on the same conversation,
    // and does not lose where it came from.
    svc.chat_turn(via.clone(), conversation, "And tomorrow?".into(), silent()).await.unwrap();
    assert_eq!(vault.conversation(conversation).unwrap().via, Some(via));

    // The thread changed, and the change says a chat did it, so a window
    // shows it rather than taking it for its own.
    let changes = events.take();
    let thread: Vec<_> = changes.iter().filter(|c| c.kind == Kind::Conversation).collect();
    assert_eq!(thread.len(), 2, "{changes:?}");
    for change in thread {
        assert_eq!(change.id.as_deref(), Some(conversation.to_string().as_str()));
        assert_eq!(change.origin.as_deref(), Some("chat"));
    }
}

#[tokio::test]
async fn a_chat_turn_tells_the_model_the_person_is_not_at_this_screen() {
    let model = fake_model(says("Done.")).await;
    let (svc, _dir) = assistant_service(&model.endpoint);

    svc.chat_turn(via(), ConversationId::new(), "hello".into(), silent()).await.unwrap();
    let asked = model.requests.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert!(
        asked[0].contains("They are writing to you from Matrix on another device"),
        "the chat's service is named in what the model is told"
    );
    assert!(asked[0].contains("links into the app will not open there"));

    // And the rail says nothing of the kind.
    svc.send_message(
        Ctx::local(),
        json!({ "conversationId": ConversationId::new().to_string(), "prompt": "hello" }),
        silent(),
    )
    .await
    .unwrap();
    let asked = model.requests.lock().unwrap().clone();
    assert_eq!(asked.len(), 2);
    assert!(!asked[1].contains("They are writing to you from"), "only a chat is told that");
}

/// The tier's reasoning effort reaches the request, and an unset one sends
/// nothing -- the endpoint's own default, which is what every vault saved
/// before there was a setting goes on getting.
#[tokio::test]
async fn the_assistant_asks_for_the_reasoning_effort_its_model_is_set_to() {
    let model = fake_model(says("Done.")).await;
    let (svc, _dir) = assistant_service(&model.endpoint);

    svc.chat_turn(via(), ConversationId::new(), "hello".into(), silent()).await.unwrap();
    let asked = model.requests.lock().unwrap().clone();
    assert!(!asked[0].contains("reasoning_effort"), "unset sends nothing");

    let vault = svc.get().unwrap();
    let mut models = vault.model_settings().unwrap();
    models.medium.model.reasoning_effort = Some(everyday_core::llm::ReasoningEffort::High);
    vault.save_model_settings(&models).unwrap();

    svc.chat_turn(via(), ConversationId::new(), "hello again".into(), silent()).await.unwrap();
    let asked = model.requests.lock().unwrap().clone();
    assert_eq!(asked.len(), 2);
    assert!(asked[1].contains(r#""reasoning_effort":"high""#), "{}", asked[1]);
}

/// Ask the assistant, through a chat, to delete a task it has been given, and
/// answer the question it asks with `answer`. What the turn said, what it
/// asked, and whether the task survived.
async fn delete_through_a_chat(
    answer: ConfirmAnswer,
) -> (everyday_service::agent::Turned, Vec<AgentEvent>, bool) {
    let (svc, dir) = assistant_service("http://127.0.0.1:1/v1");
    let vault = svc.get().unwrap();
    let task = Task::new("Buy paint");
    vault.save_task(&task).unwrap();

    let model = fake_model(calls(
        "delete_task",
        json!({ "task_id": task.id.to_string() }).to_string(),
        "All done.",
    ))
    .await;
    assert!(!vault.agent_settings().unwrap().confirm_destructive, "the desktop's setting is off");
    let mut models = vault.model_settings().unwrap();
    models.medium.connection.base_url = Some(model.endpoint.clone());
    vault.save_model_settings(&models).unwrap();

    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::default();
    let sink: Sink = {
        let events = events.clone();
        let pending = svc.pending();
        Arc::new(move |event: AgentEvent| {
            // Answered from inside the sink, the way the relay answers from
            // the chat: the gate has registered the question before it
            // announces it, so there is always somebody to receive this.
            if let AgentEvent::ConfirmationRequired { call_id, .. } = &event {
                assert!(pending.answer(call_id, answer), "nobody was waiting on the question");
            }
            events.lock().unwrap().push(event);
        })
    };
    let turned = svc
        .chat_turn(via(), ConversationId::new(), "Delete the paint task.".into(), sink)
        .await
        .expect("the turn finishes once the question is answered");
    let events = events.lock().unwrap().clone();
    let survived = vault.task(task.id).is_ok();
    drop((model, dir));
    (turned, events, survived)
}

#[tokio::test]
async fn a_chat_turn_asks_before_deleting_even_with_confirmation_off_and_a_no_stops_it() {
    let (turned, events, survived) = delete_through_a_chat(ConfirmAnswer::Decline).await;

    let asked: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ConfirmationRequired { name, subject, kind, .. } => {
                Some((name.as_str(), subject.as_str(), kind.as_str()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(asked, vec![("delete_task", "Buy paint", "destructive")]);
    assert!(survived, "declined, so nothing was deleted");
    assert_eq!(turned.text, "All done.", "and the turn went on to finish");
    assert!(matches!(events.last(), Some(AgentEvent::Finished { .. })), "{events:?}");
}

#[tokio::test]
async fn a_chat_turn_deletes_once_the_person_says_yes() {
    let (_turned, events, survived) = delete_through_a_chat(ConfirmAnswer::Confirm).await;

    assert!(events.iter().any(|e| matches!(e, AgentEvent::ConfirmationRequired { .. })));
    assert!(!survived, "confirmed, so it went");
    let ran = events.iter().any(
        |e| matches!(e, AgentEvent::ToolFinished { name, ok: true, .. } if name == "delete_task"),
    );
    assert!(ran, "{events:?}");
}

#[tokio::test]
async fn the_rail_still_honours_the_setting_that_a_chat_ignores() {
    // The contrast that gives the two tests above their meaning: the very
    // same assistant, with confirmation off, deletes without asking when
    // somebody is typing at the desk.
    let (svc, _dir) = assistant_service("http://127.0.0.1:1/v1");
    let vault = svc.get().unwrap();
    let task = Task::new("Buy paint");
    vault.save_task(&task).unwrap();
    let model = fake_model(calls(
        "delete_task",
        json!({ "task_id": task.id.to_string() }).to_string(),
        "Deleted.",
    ))
    .await;
    let mut models = vault.model_settings().unwrap();
    models.medium.connection.base_url = Some(model.endpoint.clone());
    vault.save_model_settings(&models).unwrap();

    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::default();
    let sink: Sink = {
        let events = events.clone();
        Arc::new(move |event| events.lock().unwrap().push(event))
    };
    svc.send_message(
        Ctx::local(),
        json!({
            "conversationId": ConversationId::new().to_string(),
            "prompt": "Delete the paint task.",
        }),
        sink,
    )
    .await
    .unwrap();

    let events = events.lock().unwrap();
    assert!(
        !events.iter().any(|e| matches!(e, AgentEvent::ConfirmationRequired { .. })),
        "nobody was asked: {events:?}"
    );
    assert!(vault.task(task.id).is_err(), "it was deleted");
}
