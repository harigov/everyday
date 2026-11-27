//! Passes that ask a mailbox only what changed since the last one -- see
//! `passes::sync_once_after` and `passes::reconcile_selected` -- and what a
//! pass with nothing to do says of itself.

use jatayu_core::id::MailboxId;
use jatayu_core::mail::MailboxRole;
use jatayu_mail::session::{Flags, MailSession};

use crate::mail::sync::discovery::{LabelMailboxes, SyncedMailbox};
use crate::mail::sync::ingest::ThreadIndex;
use crate::mail::sync::passes;
use crate::mail::sync::status::Phase;

use super::fixtures::*;
use super::wiring::settle;

const DATE: &str = "01 Jan 2024 10:00:00 +0000";

/// One pass, as the account task runs it: given what its last pass came back
/// with.
async fn sync_after(
    env: &TestEnv,
    session: &mut FakeMailSession,
    previous: &[SyncedMailbox],
) -> Vec<SyncedMailbox> {
    let mut labels = LabelMailboxes::new(&env.vault, env.account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once_after(&env.ctx(), session, &mut labels, &mut threads, &mut (), previous)
        .await
        .unwrap()
}

fn since(session: &FakeMailSession, from: usize) -> Vec<String> {
    session.commands()[from..].to_vec()
}

fn inbox(env: &TestEnv) -> MailboxId {
    env.vault
        .mailboxes(env.account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap()
        .id
}

fn message(name: &str) -> Vec<u8> {
    raw_message(&format!("{name}@example.com"), None, "ada@example.com", name, DATE, "x")
}

/// Once a pass has listed a mailbox whole, the next asks it only what changed:
/// `CHANGEDSINCE` for a flag, the uids from where the last pass left off for
/// new mail. Something gone is found only by a listing, and the counts send
/// the pass back to one.
#[tokio::test]
async fn a_pass_after_a_whole_listing_asks_only_what_changed() {
    let env = TestEnv::new();
    let server = plain_server();
    let first = server.lock().unwrap().append("INBOX", message("First"), Flags::NONE, None);
    let mut session = FakeMailSession::new(server.clone());

    // A task's first pass lists.
    let mut mailboxes = sync_after(&env, &mut session, &[]).await;
    assert!(session.commands().iter().any(|c| c == "list INBOX"));
    let inbox = inbox(&env);

    // Read in another client.
    server.lock().unwrap().set_flags("INBOX", first, Flags::SEEN);
    let before = session.commands().len();
    mailboxes = sync_after(&env, &mut session, &mailboxes).await;
    let asked = since(&session, before);
    assert!(!asked.iter().any(|c| c.starts_with("list ")), "{asked:?}");
    assert!(asked.iter().any(|c| c == "changedsince INBOX"), "{asked:?}");
    let read = env.vault.message_by_uid(inbox, first).unwrap().unwrap();
    assert!(!read.flags.unread(), "the flag still lands");

    // Something new.
    let second = server.lock().unwrap().append("INBOX", message("Second"), Flags::NONE, None);
    let before = session.commands().len();
    mailboxes = sync_after(&env, &mut session, &mailboxes).await;
    let asked = since(&session, before);
    assert!(!asked.iter().any(|c| c.starts_with("list ")), "{asked:?}");
    assert!(asked.contains(&format!("search {second}:*")), "{asked:?}");
    assert!(env.vault.message_by_uid(inbox, second).unwrap().is_some(), "and the mail lands");

    // Something gone.
    server.lock().unwrap().remove("INBOX", first);
    let before = session.commands().len();
    sync_after(&env, &mut session, &mailboxes).await;
    assert!(since(&session, before).iter().any(|c| c == "list INBOX"));
    assert_eq!(env.vault.mail_uid_set(inbox).unwrap(), vec![second]);
}

/// What the `SELECT` before `IDLE` finds when only a flag moved -- the app's
/// own action, or a phone's -- is read there, on that mailbox alone, and the
/// mod-sequence moved on once it is written. New mail is not for it to take.
#[tokio::test]
async fn a_flag_moved_before_idle_is_reconciled_on_the_selected_mailbox_alone() {
    let env = TestEnv::new();
    let server = plain_server();
    let first = server.lock().unwrap().append("INBOX", message("First"), Flags::NONE, None);
    let mut session = FakeMailSession::new(server.clone());
    let mut mailboxes = sync_after(&env, &mut session, &[]).await;
    let target = mailboxes.iter_mut().find(|m| m.row.role == MailboxRole::Inbox).unwrap();
    let inbox = target.row.id;
    let _ = env.statuses.take_changed(env.account_id);

    server.lock().unwrap().set_flags("INBOX", first, Flags::SEEN);
    let state = session.select("INBOX").await.unwrap();
    let before = session.commands().len();
    let mut labels = LabelMailboxes::new(&env.vault, env.account_id);
    let reconciled =
        passes::reconcile_selected(&env.ctx(), &mut session, target, &state, &mut labels).await;
    assert!(reconciled.unwrap());
    assert_eq!(since(&session, before), ["changedsince INBOX"]);
    assert!(!env.vault.message_by_uid(inbox, first).unwrap().unwrap().flags.unread());
    let modseq = state.highestmodseq.unwrap();
    assert_eq!(target.row.highest_modseq, modseq);
    assert_eq!(env.vault.mailbox(inbox).unwrap().highest_modseq, modseq);
    assert!(env.statuses.take_changed(env.account_id), "an open list hears of it");

    server.lock().unwrap().append("INBOX", message("Second"), Flags::NONE, None);
    let state = session.select("INBOX").await.unwrap();
    let reconciled =
        passes::reconcile_selected(&env.ctx(), &mut session, target, &state, &mut labels).await;
    assert!(!reconciled.unwrap(), "new mail is the full pass's");
}

/// A pass that finds nothing new never says it is syncing -- a spinner, and a
/// window asking every two seconds whether it is done -- and only one that
/// finds a folder nobody has seen says the mailboxes changed.
#[tokio::test]
async fn a_pass_with_nothing_new_never_says_it_is_syncing() {
    let env = TestEnv::new();
    let server = plain_server();
    server.lock().unwrap().append("INBOX", message("First"), Flags::NONE, None);
    let mut session = FakeMailSession::new(server.clone());
    let mailboxes = sync_after(&env, &mut session, &[]).await;
    assert!(env.statuses.take_mailboxes_changed(env.account_id), "every folder was new");

    env.statuses.set_phase(env.account_id, Phase::Idling, 0, 0);
    let mailboxes = sync_after(&env, &mut session, &mailboxes).await;
    let phase =
        || env.statuses.all().into_iter().find(|p| p.account_id == env.account_id).unwrap().phase;
    assert_eq!(phase(), Phase::Idling);
    assert!(!env.statuses.take_mailboxes_changed(env.account_id));

    // A folder made in another client.
    server.lock().unwrap().mailbox("Receipts", None);
    sync_after(&env, &mut session, &mailboxes).await;
    assert!(env.statuses.take_mailboxes_changed(env.account_id));
}

/// Read on a phone while the task was draining rather than in `IDLE`, which
/// hears only what changes after it starts: the `SELECT` before `IDLE` finds
/// the inbox's flags moved, and the flag is read there and then -- with no
/// full pass over every mailbox for it.
#[tokio::test(start_paused = true)]
async fn a_flag_changed_outside_idle_lands_without_a_full_pass() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    let uid = server.lock().unwrap().append("INBOX", message("Unread"), Flags::NONE, None);
    let session = FakeMailSession::new(server.clone()).blocking_idle();
    let idles = session.idle_selections();
    let log = session.command_log();
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
    settle(|| idles.lock().unwrap().len() == 1).await;
    let passes = || log.lock().unwrap().iter().filter(|c| *c == "discover").count();
    let before = passes();

    server.lock().unwrap().set_flags("INBOX", uid, Flags::SEEN);
    svc.notify_outbox(account_id);

    settle(|| idles.lock().unwrap().len() == 2).await;
    let inbox = vault
        .mailboxes(account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap()
        .id;
    assert!(!vault.message_by_uid(inbox, uid).unwrap().unwrap().flags.unread());
    assert_eq!(passes(), before, "a full pass for one flag");
    assert!(log.lock().unwrap().iter().any(|c| c == "changedsince INBOX"));

    stop_tx.send(true).unwrap();
    handle.await.unwrap().unwrap();
}

/// Mail that comes in while the flags are reconciled before `IDLE` is told of
/// only as an `EXISTS` in that `FETCH`'s answer, which is dropped, and `IDLE`,
/// begun after it, never hears of it again: the inbox is looked at once more
/// before `IDLE`, so the mail is not left for the sync interval.
#[tokio::test(start_paused = true)]
async fn mail_that_arrives_while_reconciling_is_not_left_for_the_interval() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    let uid = server.lock().unwrap().append("INBOX", message("Unread"), Flags::NONE, None);
    let session = FakeMailSession::new(server.clone()).blocking_idle();
    let idles = session.idle_selections();
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
    settle(|| idles.lock().unwrap().len() == 1).await;

    {
        let mut server = server.lock().unwrap();
        server.set_flags("INBOX", uid, Flags::SEEN);
        server.deliver_after_changes_asked("INBOX", message("Late"));
    }
    svc.notify_outbox(account_id);

    let inbox = vault
        .mailboxes(account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap()
        .id;
    settle(|| vault.mail_uid_set(inbox).unwrap().len() == 2).await;
    assert!(!vault.message_by_uid(inbox, uid).unwrap().unwrap().flags.unread());

    stop_tx.send(true).unwrap();
    handle.await.unwrap().unwrap();
}

/// A label first met while reconciling -- applied in another client, and seen
/// by the label re-check of the uids `CHANGEDSINCE` names -- is said to be a
/// new mailbox, for the account task to tell a window of, as a label a full
/// pass meets is.
#[tokio::test]
async fn a_label_first_met_while_reconciling_says_the_mailboxes_changed() {
    let env = TestEnv::new();
    let server = gmail_server();
    let uid = server.lock().unwrap().append(
        "All Mail",
        message("Receipt"),
        Flags::NONE,
        Some(jatayu_mail::session::GmailMeta {
            thrid: 1,
            msgid: 1,
            labels: vec!["\\Inbox".into()],
        }),
    );
    let mut session = FakeMailSession::new(server.clone());
    let mut mailboxes = sync_after(&env, &mut session, &[]).await;
    assert!(env.statuses.take_mailboxes_changed(env.account_id), "the first pass met every one");
    let target = mailboxes.iter_mut().find(|m| m.row.role == MailboxRole::All).unwrap();

    server.lock().unwrap().set_gmail_labels(
        "All Mail",
        uid,
        vec!["\\Inbox".into(), "Receipts".into()],
    );
    let state = session.select("All Mail").await.unwrap();
    let mut labels = LabelMailboxes::new(&env.vault, env.account_id);
    let reconciled =
        passes::reconcile_selected(&env.ctx(), &mut session, target, &state, &mut labels).await;
    assert!(reconciled.unwrap());
    assert!(
        env.vault.mailboxes(env.account_id).unwrap().iter().any(|m| m.remote_name == "Receipts")
    );
    assert!(env.statuses.take_mailboxes_changed(env.account_id));
}
