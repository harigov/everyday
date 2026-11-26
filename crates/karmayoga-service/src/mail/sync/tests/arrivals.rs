// ---------------------------------------------------------------------
// How fast new mail is stored and announced: the quick pass on a wake from
// `IDLE`, mail that lands while a pass is running, and the minute's pass
// leaving a mailbox alone when nothing in it has changed.
// ---------------------------------------------------------------------

use std::sync::{Arc, Mutex};

use karmayoga_core::account::MailNotify;
use karmayoga_core::id::AccountId;
use karmayoga_core::mail::MailboxRole;
use karmayoga_core::store::mail::ThreadFilter;
use karmayoga_mail::session::{Flags, GmailMeta};

use crate::events::{EventSink, Notification};
use crate::mail::sync::status::Phase;

use super::fixtures::*;
use super::wiring::settle;

const DATE: &str = "01 Jan 2024 10:00:00 +0000";

/// Every notification posted while this is the service's sink.
#[derive(Default)]
struct Told(Mutex<Vec<Notification>>);

impl EventSink for Told {
    fn notify(&self, notification: Notification) {
        self.0.lock().unwrap().push(notification);
    }
}

impl Told {
    fn bodies(&self) -> Vec<String> {
        self.0.lock().unwrap().iter().filter_map(|n| n.body.clone()).collect()
    }
}

/// Start the account task against `session`, told about everything new, and
/// wait until it has finished its first pass and holds `IDLE`.
async fn running(
    svc: &Arc<crate::service::Service>,
    vault: &Arc<karmayoga_core::Vault>,
    account_id: AccountId,
    session: FakeMailSession,
) -> (tokio::sync::watch::Sender<bool>, tokio::task::JoinHandle<crate::supervisor::TaskResult>) {
    let mut account = vault.account(account_id).unwrap();
    account.mail_notify = MailNotify::All;
    vault.save_account(&account).unwrap();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let (svc_task, vault_task) = (svc.clone(), vault.clone());
    let handle = tokio::spawn(async move {
        crate::mail::sync::task::run_account_with(
            svc_task,
            vault_task,
            account_id,
            stop_rx,
            move |_account, _credential| {
                let session = session.clone();
                async move { Ok(session) }
            },
            |_account, _svc, _vault| FakeSender::default(),
        )
        .await
    });
    idling(svc, account_id).await;
    (stop_tx, handle)
}

async fn idling(svc: &Arc<crate::service::Service>, account_id: AccountId) {
    let statuses = svc.mail_statuses().unwrap();
    settle(|| {
        statuses.all().iter().any(|p| p.account_id == account_id && p.phase == Phase::Idling)
    })
    .await;
}

/// The subjects of every thread listed in `role`.
fn subjects(
    vault: &karmayoga_core::Vault,
    account_id: AccountId,
    role: MailboxRole,
) -> Vec<String> {
    let mailbox =
        vault.mailboxes(account_id).unwrap().into_iter().find(|m| m.role == role).unwrap();
    let page = vault.list_threads(mailbox.id, &ThreadFilter::default(), None, 50).unwrap();
    page.threads.into_iter().map(|t| t.subject).collect()
}

/// The fake's commands from `from` on.
fn since(session: &FakeMailSession, from: usize) -> Vec<String> {
    session.commands()[from..].to_vec()
}

/// A message arriving while the task holds `IDLE` on Gmail's All Mail is
/// fetched by uid -- not by listing the whole mailbox, every other mailbox and
/// every body first -- filed under the Inbox label, and announced, before the
/// full pass that follows sees to the rest. The clock never moves: the poll
/// has nothing to do with it.
#[tokio::test(start_paused = true)]
async fn mail_that_arrives_while_idle_is_stored_and_announced_before_the_full_pass() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let told = Arc::new(Told::default());
    svc.set_events(told.clone());
    let server = gmail_server();
    server.lock().unwrap().append(
        "All Mail",
        raw_message("old@example.com", None, "ada@example.com", "Old", DATE, "x"),
        Flags::SEEN,
        Some(GmailMeta { thrid: 1, msgid: 1, labels: vec!["\\Inbox".into()] }),
    );
    let session = FakeMailSession::new(server.clone()).blocking_idle().waking_on_mail();
    let idles = session.idle_selections();
    let (stop_tx, handle) = running(&svc, &vault, account_id, session.clone()).await;
    // "Idling" is said a moment before the `SELECT` that starts `IDLE`.
    settle(|| idles.lock().unwrap().len() == 1).await;
    let before = session.commands().len();

    let uid = server.lock().unwrap().append(
        "All Mail",
        raw_message("new@example.com", None, "ada@example.com", "Lunch?", DATE, "Friday?"),
        Flags::NONE,
        Some(GmailMeta { thrid: 2, msgid: 2, labels: vec!["\\Inbox".into()] }),
    );
    // The quick pass has the body by the time it announces: the first words
    // are under the subject.
    settle(|| told.bodies().iter().any(|b| b == "Lunch?\nFriday?")).await;
    assert!(subjects(&vault, account_id, MailboxRole::Inbox).contains(&"Lunch?".to_string()));
    let commands = since(&session, before);
    assert_eq!(
        commands.first().map(String::as_str),
        Some(format!("search {uid}:*").as_str()),
        "what arrived is asked for by uid, straight out of IDLE: {commands:?}"
    );

    idling(&svc, account_id).await;
    stop_tx.send(true).unwrap();
    handle.await.unwrap().unwrap();
}

/// A server that refuses the quick search costs only the quick path: the full
/// pass after it stores the mail, and the account is not marked as failing
/// over a command it never needed.
#[tokio::test(start_paused = true)]
async fn a_server_that_refuses_the_quick_search_still_gets_its_mail() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let told = Arc::new(Told::default());
    svc.set_events(told.clone());
    let server = plain_server();
    server.lock().unwrap().append(
        "INBOX",
        raw_message("old@example.com", None, "ada@example.com", "Old", DATE, "x"),
        Flags::SEEN,
        None,
    );
    let session =
        FakeMailSession::new(server.clone()).blocking_idle().waking_on_mail().refusing_uid_search();
    let idles = session.idle_selections();
    let (stop_tx, handle) = running(&svc, &vault, account_id, session.clone()).await;
    settle(|| idles.lock().unwrap().len() == 1).await;

    server.lock().unwrap().append(
        "INBOX",
        raw_message("new@example.com", None, "ada@example.com", "Refused", DATE, "x"),
        Flags::NONE,
        None,
    );
    // The full pass fetches the body of what it announces first, so its
    // notification says what the mail is about too.
    settle(|| told.bodies().iter().any(|b| b == "Refused\nx")).await;
    settle(|| idles.lock().unwrap().len() == 2).await;
    let statuses = svc.mail_statuses().unwrap();
    let progress = statuses.all().into_iter().find(|p| p.account_id == account_id).unwrap();
    assert_eq!(progress.last_error, None, "not an account error");
    assert!(session.commands().iter().any(|c| c.starts_with("search ")), "it was asked");

    stop_tx.send(true).unwrap();
    handle.await.unwrap().unwrap();
}

/// Mail that reaches the inbox after a pass has listed it, and before the task
/// selects it again to start `IDLE`, is in the count that `SELECT` takes, and
/// `IDLE` reports only what changes after it starts. It used to wait for the
/// next poll -- a minute, or an hour on an account set to sync hourly.
#[tokio::test(start_paused = true)]
async fn mail_that_lands_during_a_pass_is_fetched_before_idling() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let mut account = vault.account(account_id).unwrap();
    account.sync_minutes = Some(60);
    vault.save_account(&account).unwrap();
    let server = plain_server();
    server.lock().unwrap().append(
        "INBOX",
        raw_message("old@example.com", None, "ada@example.com", "Old", DATE, "x"),
        Flags::SEEN,
        None,
    );
    // `IDLE` here never reports anything: only the check before it can.
    let session = FakeMailSession::new(server.clone()).blocking_idle();
    let (stop_tx, handle) = running(&svc, &vault, account_id, session.clone()).await;

    // Delivered while the next pass is in Sent, after it has listed INBOX.
    server.lock().unwrap().deliver_when_selected(
        "Sent",
        "INBOX",
        raw_message("mid-pass@example.com", None, "ada@example.com", "Mid-pass", DATE, "x"),
    );
    svc.mail_statuses().unwrap().nudge(account_id);

    let inbox = || subjects(&vault, account_id, MailboxRole::Inbox);
    settle(|| inbox().contains(&"Mid-pass".to_string())).await;

    idling(&svc, account_id).await;
    stop_tx.send(true).unwrap();
    handle.await.unwrap().unwrap();
}

/// A mailbox whose `UIDNEXT`, `HIGHESTMODSEQ` and count all stand where the
/// last pass left them is not listed again: that listing is most of what a
/// quiet minute downloads on a big account. Anything changing brings it back.
#[tokio::test]
async fn a_quiet_mailbox_is_not_listed_again_until_something_in_it_changes() {
    let env = TestEnv::new();
    let server = plain_server();
    let uid = server.lock().unwrap().append(
        "INBOX",
        raw_message("first@example.com", None, "ada@example.com", "First", DATE, "x"),
        Flags::NONE,
        None,
    );
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;
    let listed = |session: &FakeMailSession, from: usize| -> Vec<String> {
        since(session, from).into_iter().filter(|c| c.starts_with("list ")).collect()
    };

    let before = session.commands().len();
    env.sync(&mut session).await;
    assert_eq!(listed(&session, before), Vec::<String>::new(), "nothing changed anywhere");

    // Read in another client.
    server.lock().unwrap().set_flags("INBOX", uid, Flags::SEEN);
    let before = session.commands().len();
    env.sync(&mut session).await;
    assert_eq!(listed(&session, before), ["list INBOX"]);
    let inbox = env
        .vault
        .mailboxes(env.account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap();
    let message = env.vault.message_by_uid(inbox.id, uid).unwrap().unwrap();
    assert!(!message.flags.unread(), "the flag change still lands");

    // Gone in another client.
    server.lock().unwrap().remove("INBOX", uid);
    let before = session.commands().len();
    env.sync(&mut session).await;
    assert_eq!(listed(&session, before), ["list INBOX"]);
    assert!(env.vault.mail_uid_set(inbox.id).unwrap().is_empty(), "and so does its going");

    // Something new.
    server.lock().unwrap().append(
        "Archive",
        raw_message("second@example.com", None, "ada@example.com", "Second", DATE, "x"),
        Flags::NONE,
        None,
    );
    let before = session.commands().len();
    env.sync(&mut session).await;
    assert_eq!(listed(&session, before), ["list Archive"]);
}
