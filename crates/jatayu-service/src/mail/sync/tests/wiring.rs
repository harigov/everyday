use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use jatayu_core::mail::{Address, Draft, MailboxRole, OpKind, OpState, Origin};
use jatayu_core::store::mail::ThreadFilter;
use jatayu_mail::compose::Built;
use jatayu_mail::session::{GmailMeta, Role};
use jatayu_mail::smtp::SendReceipt;
use jiff::Timestamp;

use crate::events::{Change, EventSink, Kind};
use crate::mail::sync::discovery::LabelMailboxes;
use crate::mail::sync::ingest::ThreadIndex;
use crate::mail::sync::passes::{self, SyncContext};

use super::fixtures::*;
/// The contact index learns from ingest: a `Sent` message's recipients
/// count as "sent to", and everyone else's `From` counts as "received
/// from" -- see `passes::sync_headers`'s own wiring.
#[tokio::test]
async fn a_sync_pass_teaches_the_contact_index_from_sent_and_received_mail() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            raw_message(
                "from-alice@example.com",
                None,
                "Alice <alice@example.com>",
                "Hello",
                "01 Jan 2024 10:00:00 +0000",
                "hi",
            ),
            flags_seen(),
            None,
        );
        let sent_to_bob = b"Message-ID: <to-bob@example.com>\r\n\
From: me@example.com\r\n\
To: Bob <bob@example.com>\r\n\
Subject: Re: plans\r\n\
Date: 01 Jan 2024 10:05:00 +0000\r\n\
Content-Type: text/plain\r\n\r\nsounds good\r\n"
            .to_vec();
        s.append("Sent", sent_to_bob, flags_seen(), None);
    }
    let mut session = FakeMailSession::new(server);
    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let index = svc.mail_contacts().unwrap();
    let suggestions = index.suggest("", 10);
    let by_email: HashMap<&str, &jatayu_core::mail::Address> =
        suggestions.iter().map(|a| (a.email.as_str(), a)).collect();
    assert!(
        by_email.contains_key("alice@example.com"),
        "received-from must be learned: {suggestions:?}"
    );
    assert!(by_email.contains_key("bob@example.com"), "sent-to must be learned: {suggestions:?}");
}

/// Regression: the account's own address used to be recorded as a
/// correspondent -- a message from oneself (received) or to oneself
/// (sent, e.g. a BCC-to-self) climbing straight up one's own autocomplete
/// -- and a Drafts folder, full of messages *from* the account itself, was
/// fair game for "received from" too, drafts' own recipients included,
/// since the whole contact-recording block ran regardless of which
/// mailbox a header came from.
#[tokio::test]
async fn the_accounts_own_address_and_its_drafts_are_never_recorded_as_a_contact() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        s.mailbox("Drafts", Some(Role::Drafts));
        // A real correspondent, both ways -- must still be learned.
        s.append(
            "INBOX",
            raw_message(
                "from-alice@example.com",
                None,
                "Alice <alice@example.com>",
                "Hello",
                "01 Jan 2024 10:00:00 +0000",
                "hi",
            ),
            flags_seen(),
            None,
        );
        // A note to self, landing in the inbox: `From` is the account's
        // own address.
        let received_from_self = b"Message-ID: <self-received@example.com>\r\n\
From: me@example.com\r\n\
To: me@example.com\r\n\
Subject: Note to self\r\n\
Date: 01 Jan 2024 10:01:00 +0000\r\n\
Content-Type: text/plain\r\n\r\nremember this\r\n"
            .to_vec();
        s.append("INBOX", received_from_self, flags_seen(), None);
        // A BCC-to-self style send: the account's own address is one of
        // the `To` addresses of its own Sent copy.
        let sent_to_self = b"Message-ID: <self-sent@example.com>\r\n\
From: me@example.com\r\n\
To: me@example.com\r\n\
Subject: Reminder\r\n\
Date: 01 Jan 2024 10:02:00 +0000\r\n\
Content-Type: text/plain\r\n\r\nping\r\n"
            .to_vec();
        s.append("Sent", sent_to_self, flags_seen(), None);
        // A draft: entirely from the account itself, to someone real --
        // neither side should ever be recorded from a Drafts folder.
        let draft = b"Message-ID: <draft-only@example.com>\r\n\
From: me@example.com\r\n\
To: someone-else@example.com\r\n\
Subject: Draft\r\n\
Date: 01 Jan 2024 10:03:00 +0000\r\n\
Content-Type: text/plain\r\n\r\ndraft body\r\n"
            .to_vec();
        s.append("Drafts", draft, flags_seen(), None);
    }
    let mut session = FakeMailSession::new(server);
    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: vec!["me@example.com".to_string()],
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let index = svc.mail_contacts().unwrap();
    let suggestions = index.suggest("", 10);
    let emails: Vec<&str> = suggestions.iter().map(|a| a.email.as_str()).collect();
    assert!(
        emails.contains(&"alice@example.com"),
        "a real correspondent must still be learned: {emails:?}"
    );
    assert!(
        !emails.contains(&"me@example.com"),
        "the account's own address must never be recorded as a contact: {emails:?}"
    );
    assert!(
        !emails.contains(&"someone-else@example.com"),
        "a draft's own recipient must not be recorded: {emails:?}"
    );
}

#[tokio::test]
async fn draining_an_archive_moves_the_message_on_the_server_and_completes_the_op() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            raw_message(
                "archive-me@example.com",
                None,
                "alice@example.com",
                "Please file this",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
    }

    let mut session = FakeMailSession::new(server.clone());
    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let inbox = vault
        .mailboxes(account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap();
    let page = vault.list_threads(inbox.id, &ThreadFilter::default(), None, 10).unwrap();
    let thread_id = page.threads[0].id;

    let op =
        vault.apply_thread_ops(&[thread_id], OpKind::Archive, Origin::Person).unwrap().remove(0);

    let sender = FakeSender::default();
    let report =
        crate::mail::outbox::drain_outbox(&svc, account_id, &mut session, &sender).await.unwrap();
    assert_eq!(report.done, 1, "{report:?}");
    assert_eq!(report.failed, 0, "{report:?}");
    assert_eq!(vault.op(op.id).unwrap().state, OpState::Done);

    let s = server.lock().unwrap();
    assert!(
        s.mailboxes["INBOX"].messages.is_empty(),
        "the message must have left the inbox on the server"
    );
    assert_eq!(
        s.mailboxes["Archive"].messages.len(),
        1,
        "and landed in the account's Archive mailbox"
    );
}

/// Regression: Quick Cleanup's Delete queues a trash op per conversation,
/// and the drain used to run them one at a time -- a `SELECT` and a `MOVE`
/// each, which on a large Gmail account came to about twelve seconds a
/// conversation. A thousand took over three hours to reach the server, and
/// until each one had, Trash stayed empty. A run of them goes as one move
/// now, and the next sync finds them all in Trash.
#[tokio::test]
async fn a_bulk_delete_goes_to_trash_as_one_move_and_the_next_sync_finds_it_there() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = gmail_server();
    const THREADS: u64 = 40;
    {
        let mut s = server.lock().unwrap();
        for n in 1..=THREADS {
            s.append(
                "All Mail",
                raw_message(
                    &format!("newsletter-{n}@example.com"),
                    None,
                    "The Weekly <weekly@example.com>",
                    &format!("Issue {n}"),
                    "01 Jan 2024 10:00:00 +0000",
                    "x",
                ),
                flags_seen(),
                Some(GmailMeta { thrid: n, msgid: n, labels: vec!["\\Inbox".into()] }),
            );
        }
    }

    let mut session = FakeMailSession::new(server.clone());
    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let sync = async |session: &mut FakeMailSession| {
        let mut labels = LabelMailboxes::new(&vault, account_id);
        let mut threads = ThreadIndex::new();
        passes::sync_once(&ctx, session, &mut labels, &mut threads).await.unwrap();
    };
    sync(&mut session).await;

    let mailbox = |role| {
        vault.mailboxes(account_id).unwrap().into_iter().find(|m| m.role == role).unwrap().id
    };
    let listed = |role| {
        vault.list_threads(mailbox(role), &ThreadFilter::default(), None, 100).unwrap().threads
    };
    let inbox: Vec<_> = listed(MailboxRole::Inbox).iter().map(|t| t.id).collect();
    assert_eq!(inbox.len(), THREADS as usize);

    let ops = vault.apply_thread_ops(&inbox, OpKind::Trash, Origin::Person).unwrap();
    let before = session.commands().len();
    let report =
        crate::mail::outbox::drain_outbox(&svc, account_id, &mut session, &FakeSender::default())
            .await
            .unwrap();
    assert_eq!((report.done, report.failed), (THREADS as u32, 0), "{report:?}");
    assert!(ops.iter().all(|op| vault.op(op.id).unwrap().state == OpState::Done));
    assert_eq!(
        session.commands()[before..],
        ["select All Mail".to_string(), format!("move {THREADS} All Mail -> Trash")],
        "one SELECT and one MOVE for the lot"
    );
    assert_eq!(s_message_count(&server, "Trash"), THREADS as usize);
    assert_eq!(s_message_count(&server, "All Mail"), 0);

    sync(&mut session).await;
    assert_eq!(listed(MailboxRole::Trash).len(), THREADS as usize, "Trash shows what was deleted");
    assert!(listed(MailboxRole::Inbox).is_empty(), "and the Inbox does not");
    assert!(
        vault.mail_uid_set(mailbox(MailboxRole::Inbox)).unwrap().is_empty(),
        "nor is anything still filed there, to come back with the next reply"
    );
    assert!(listed(MailboxRole::Trash).iter().all(|t| t.message_count == 1), "and none is doubled");
}

#[tokio::test]
async fn draining_a_send_appends_the_sent_copy_and_marks_the_draft_sent() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    let mut session = FakeMailSession::new(server.clone());

    // A sync first, empty mailboxes and all: `special_use(Sent)` -- which
    // `jatayu_mail::outbox::send` reads to know where to `APPEND` its own
    // copy -- answers from the vault's own `mailboxes` table, which nothing
    // populates before the first sync has discovered them.
    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let mut draft = Draft::new(account_id, "me@example.com", Origin::Person);
    draft.to = vec![Address::bare("bob@example.com")];
    draft.subject = "Hello".into();
    draft.body_html = "<p>Hi</p>".into();
    vault.save_draft(&draft).unwrap();
    let (draft, op) = vault.queue_draft_send(draft.id, Timestamp::now(), Origin::Person).unwrap();

    let sender = FakeSender::default();
    let report =
        crate::mail::outbox::drain_outbox(&svc, account_id, &mut session, &sender).await.unwrap();
    assert_eq!(report.done, 1, "{report:?}");
    assert_eq!(sender.sent.lock().unwrap().len(), 1, "the sender must have been asked to send it");
    assert_eq!(vault.op(op.id).unwrap().state, OpState::Done);
    assert_eq!(vault.draft(draft.id).unwrap().state, jatayu_core::mail::DraftState::Sent);

    let s = server.lock().unwrap();
    assert_eq!(
        s.mailboxes["Sent"].messages.len(),
        1,
        "a non-Gmail server needs its own Sent copy appended"
    );
}

/// The regression for the duplicate-send risk `crate::mail::outbox::already_sent`
/// exists to close: a `Send` op recovered from `InFlight` whose draft's
/// (stable) `Message-ID` is already in Sent must be recognised as already
/// sent, not sent again.
#[tokio::test]
async fn recovering_a_send_already_on_the_server_is_not_resent() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    let mut session = FakeMailSession::new(server.clone());

    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    // A draft whose `Message-ID` was already stamped -- what
    // `jatayu_mail::outbox::send` does before it ever calls the sender,
    // so it survives a crash between the two.
    let mut draft = Draft::new(account_id, "me@example.com", Origin::Person);
    draft.to = vec![Address::bare("bob@example.com")];
    draft.subject = "Already sent".into();
    draft.body_html = "<p>Hi</p>".into();
    draft.message_id = Some("stable-id@example.com".into());
    vault.save_draft(&draft).unwrap();
    let (draft, op) = vault.queue_draft_send(draft.id, Timestamp::now(), Origin::Person).unwrap();

    // The crash this test simulates happened *after* SMTP accepted the
    // message: the server genuinely has a Sent copy under that exact
    // `Message-ID`, and the op is stranded `InFlight` because this process
    // never got to say so.
    {
        let mut s = server.lock().unwrap();
        s.append(
            "Sent",
            raw_message(
                "stable-id@example.com",
                None,
                "me@example.com",
                "Already sent",
                "01 Jan 2024 10:00:00 +0000",
                "hi",
            ),
            flags_seen(),
            None,
        );
    }
    let mut stranded = vault.op(op.id).unwrap();
    stranded.transition_to(OpState::InFlight).unwrap();
    vault.update_op(&stranded).unwrap();

    crate::mail::outbox::recover_inflight_ops(&svc, account_id, &mut session).await.unwrap();

    assert_eq!(
        vault.op(op.id).unwrap().state,
        OpState::Done,
        "already on the server -- recovered as done, not requeued to send again"
    );
    assert_eq!(vault.draft(draft.id).unwrap().state, jatayu_core::mail::DraftState::Sent);

    // And, proof this genuinely skipped sending rather than merely getting
    // lucky: nothing is left pending to drain, and the sender below is
    // never asked to send anything.
    let sender = FakeSender::default();
    let report =
        crate::mail::outbox::drain_outbox(&svc, account_id, &mut session, &sender).await.unwrap();
    assert_eq!(report.attempted, 0, "{report:?}");
    assert!(sender.sent.lock().unwrap().is_empty(), "must not have been sent a second time");
    assert_eq!(
        s_message_count(&server, "Sent"),
        1,
        "still exactly the one copy the server already had"
    );
}

fn s_message_count(server: &Arc<Mutex<FakeServer>>, mailbox: &str) -> usize {
    server.lock().unwrap().mailboxes[mailbox].messages.len()
}

/// Regression: `run_account_with` used to `IDLE` on whatever mailbox
/// `sync_once`'s own last pass happened to leave selected -- never the
/// inbox, since `discovery::discover` always sorts the inbox (All Mail, on
/// Gmail) first and every other mailbox is therefore visited *after* it.
/// Here, only Trash has a message of its own, so `bodies_pass`'s own
/// `SELECT`, called once per mailbox with anything still pending, is
/// issued for Trash and nothing after it -- exactly the shape that leaves
/// a real IMAP session sitting on the wrong mailbox when `run_account_with`
/// asks for `IDLE` without first pointing the session back at the inbox.
#[tokio::test]
async fn idle_is_issued_on_the_inbox_not_whatever_mailbox_was_selected_last() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = gmail_server();
    {
        let mut s = server.lock().unwrap();
        s.append(
            "All Mail",
            raw_message(
                "idle-target@example.com",
                None,
                "a@example.com",
                "Hi",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            Some(GmailMeta { thrid: 1, msgid: 1, labels: vec!["\\Inbox".into()] }),
        );
        // The only other mailbox with anything in it -- so it is the one
        // `bodies_pass` reselects last, and (without the fix) the one
        // `IDLE` would be issued on.
        s.append(
            "Trash",
            raw_message(
                "trash-target@example.com",
                None,
                "a@example.com",
                "Bye",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
    }
    let session = FakeMailSession::new(server).blocking_idle();
    let idle_selections = session.idle_selections();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);

    let svc_task = svc.clone();
    let vault_task = vault.clone();
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

    settle(|| !idle_selections.lock().unwrap().is_empty()).await;
    stop_tx.send(true).unwrap();
    handle.await.unwrap().unwrap();

    let selections = idle_selections.lock().unwrap();
    assert_eq!(
        selections.first().cloned().flatten().as_deref(),
        Some("All Mail"),
        "IDLE must be issued on All Mail, Gmail's own inbox-equivalent -- not Trash, \
         which is where sync_once's own last pass would otherwise have left the session: {selections:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_notify_wakes_the_idle_loop_promptly_rather_than_waiting_for_the_poll() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            raw_message(
                "notify-me@example.com",
                None,
                "alice@example.com",
                "Wake up",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
    }
    let session = FakeMailSession::new(server).blocking_idle();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);

    let svc_task = svc.clone();
    let vault_task = vault.clone();
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

    // Let the task connect, run its first sync, drain its (empty) outbox,
    // and settle into `IDLE` -- `block_idle` means it parks there rather
    // than returning, so only `stop` or a notification can move it on.
    settle(|| {
        matches!(
            svc.mail_statuses()
                .unwrap()
                .all()
                .iter()
                .find(|p| p.account_id == account_id)
                .map(|p| p.phase),
            Some(crate::mail::sync::status::Phase::Idling)
        )
    })
    .await;

    let inbox = vault
        .mailboxes(account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap();
    let page = vault.list_threads(inbox.id, &ThreadFilter::default(), None, 10).unwrap();
    let thread_id = page.threads[0].id;
    let op =
        vault.apply_thread_ops(&[thread_id], OpKind::Archive, Origin::Person).unwrap().remove(0);

    svc.notify_outbox(account_id);
    // Not a single virtual millisecond is advanced from here on: the clock
    // stays exactly where `start_paused = true` left it. The account's sync
    // interval is a minute at its shortest and `OUTBOX_RETRY_INTERVAL` three
    // seconds, so the only way this op can reach `Done` without either timer
    // ever firing is the notify itself having woken the `IDLE` `select!` --
    // which is exactly the latency this test exists to prove.
    settle(|| vault.op(op.id).map(|o| o.state == OpState::Done).unwrap_or(false)).await;

    stop_tx.send(true).unwrap();
    handle.await.unwrap().unwrap();
}

/// The regression for the bug [`crate::mail::sync::task::sleep_until_due`] fixes: a
/// send-at op queued ten seconds out must drain at about ten seconds, not
/// whenever the account's sync interval next comes round -- set to an hour
/// here, so the poll cannot rescue a wrong fix inside this test's budget.
///
/// No paused clock here, unlike this module's other timing tests: `not_before`
/// due-ness is decided by [`jiff::Timestamp::now`] (the real wall clock),
/// never by `tokio::time`'s virtual one -- `next_pending_wake` converts a
/// real duration into a virtual sleep exactly once, the same trade
/// `crate::token_cache::deadline_from` makes, but the *due* check itself in
/// `crate::mail::outbox::drain_outbox` reads the wall clock fresh on every drain.
/// So this test spends ten real seconds proving it, bounded well short of
/// the sync interval by [`settle_up_to`]'s own timeout, which is the one
/// thing a wrong fix (falling back to the poll) cannot pass short of
/// genuinely waiting an hour.
#[tokio::test]
async fn a_send_at_op_drains_at_its_own_time_not_the_poll_interval() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let mut account = vault.account(account_id).unwrap();
    account.sync_minutes = Some(60);
    vault.save_account(&account).unwrap();
    let server = plain_server();
    let session = FakeMailSession::new(server).blocking_idle();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);

    let svc_task = svc.clone();
    let vault_task = vault.clone();
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

    settle(|| {
        matches!(
            svc.mail_statuses()
                .unwrap()
                .all()
                .iter()
                .find(|p| p.account_id == account_id)
                .map(|p| p.phase),
            Some(crate::mail::sync::status::Phase::Idling)
        )
    })
    .await;

    let start = std::time::Instant::now();
    let mut draft = Draft::new(account_id, "me@example.com", Origin::Person);
    draft.to = vec![Address::bare("bob@example.com")];
    draft.subject = "Later".into();
    draft.body_html = "<p>Later</p>".into();
    vault.save_draft(&draft).unwrap();
    let not_before = Timestamp::now() + jiff::SignedDuration::from_secs(10);
    let (_, op) = vault.queue_draft_send(draft.id, not_before, Origin::Person).unwrap();
    // What the real `send_draft` command does right after `queue_draft_send`
    // -- see `domains::mail`'s own wiring -- so this test enqueues the op
    // exactly the way a person actually would, rather than relying on the
    // supervisor loop stumbling onto it by some other path. It also proves
    // the fix is doing the work, not this call: the notify only tells the
    // loop to notice a not-yet-due op and compute when it will be, per
    // `crate::mail::sync::task`'s own module docs on its fourth `select!` arm.
    svc.notify_outbox(account_id);

    settle_up_to(std::time::Duration::from_secs(60), || {
        vault.op(op.id).map(|o| o.state == OpState::Done).unwrap_or(false)
    })
    .await;
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "took {elapsed:?} to drain a ten-second delay -- it waited for the poll"
    );

    stop_tx.send(true).unwrap();
    handle.await.unwrap().unwrap();
}

/// A fake sender that notes, each time it is asked to send, how many full
/// passes the session had begun by then -- each starts by listing the
/// mailboxes, `discover` in the fake's command log -- and how long that log
/// was.
#[derive(Clone)]
struct PassCountingSender {
    log: Arc<Mutex<Vec<String>>>,
    passes_at_send: Arc<Mutex<Vec<usize>>>,
    log_at_send: Arc<Mutex<Vec<usize>>>,
}

impl PassCountingSender {
    fn new(session: &FakeMailSession) -> Self {
        Self {
            log: session.command_log(),
            passes_at_send: Arc::default(),
            log_at_send: Arc::default(),
        }
    }

    fn passes(&self) -> usize {
        self.log.lock().unwrap().iter().filter(|c| *c == "discover").count()
    }
}

impl jatayu_mail::outbox::Sender for PassCountingSender {
    async fn send(&self, built: &Built) -> jatayu_mail::session::Result<SendReceipt> {
        let passes = self.passes();
        self.passes_at_send.lock().unwrap().push(passes);
        let logged = self.log.lock().unwrap().len();
        self.log_at_send.lock().unwrap().push(logged);
        Ok(SendReceipt { accepted: built.envelope_to.clone(), server_response: "250 Ok".into() })
    }
}

/// The account's task over `session`, sending with `sender`, handed back
/// once it has finished its first pass and is waiting.
async fn spawn_idling(
    svc: &Arc<crate::service::Service>,
    vault: &Arc<jatayu_core::Vault>,
    account_id: jatayu_core::id::AccountId,
    session: FakeMailSession,
    sender: PassCountingSender,
) -> (tokio::sync::watch::Sender<bool>, tokio::task::JoinHandle<crate::supervisor::TaskResult>) {
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
            move |_account, _svc, _vault| sender,
        )
        .await
    });
    settle(|| {
        matches!(
            svc.mail_statuses()
                .unwrap()
                .all()
                .iter()
                .find(|p| p.account_id == account_id)
                .map(|p| p.phase),
            Some(crate::mail::sync::status::Phase::Idling)
        )
    })
    .await;
    (stop_tx, handle)
}

/// A reply to bob, queued to send `in_secs` from now.
fn queue_send(
    vault: &jatayu_core::Vault,
    account_id: jatayu_core::id::AccountId,
    in_secs: i64,
) -> jatayu_core::mail::Op {
    let mut draft = Draft::new(account_id, "me@example.com", Origin::Person);
    draft.to = vec![Address::bare("bob@example.com")];
    draft.subject = "Soon".into();
    draft.body_html = "<p>Soon</p>".into();
    vault.save_draft(&draft).unwrap();
    let not_before = Timestamp::now() + jiff::SignedDuration::from_secs(in_secs);
    vault.queue_draft_send(draft.id, not_before, Origin::Person).unwrap().1
}

/// The regression for the reply that sat at "Sending…" for three minutes on
/// Nov 19 2026: pressing Send woke the task inside the undo window, it found
/// nothing due yet, and ran a full pass before it came back for the send --
/// every send waited for one, and that one took 2 m 40 s. A turn the outbox
/// wakes now only drains, and the send goes as its window ends with no pass
/// begun in between.
///
/// Wall-clock seconds, as in `a_send_at_op_drains_at_its_own_time_not_the_poll_interval`:
/// due-ness is read from `jiff::Timestamp::now`.
#[tokio::test]
async fn a_send_goes_as_its_undo_window_ends_without_waiting_for_a_pass() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let session = FakeMailSession::new(plain_server()).blocking_idle();
    let sender = PassCountingSender::new(&session);
    let (stop_tx, handle) = spawn_idling(&svc, &vault, account_id, session, sender.clone()).await;
    let passes = sender.passes();

    let op = queue_send(&vault, account_id, 2);
    // What `send_draft` does as it queues the send, inside the undo window.
    svc.notify_outbox(account_id);
    settle_up_to(std::time::Duration::from_secs(30), || {
        vault.op(op.id).map(|o| o.state == OpState::Done).unwrap_or(false)
    })
    .await;

    assert_eq!(
        *sender.passes_at_send.lock().unwrap(),
        vec![passes],
        "no full pass may begin between pressing Send and the send"
    );
    stop_tx.send(true).unwrap();
    handle.await.unwrap().unwrap();
}

/// A pass about to start -- for a nudge here; for new mail or the sync
/// interval as often -- waits for a send due within seconds and sends it
/// first, rather than holding it for as long as the pass takes.
#[tokio::test]
async fn a_pass_about_to_start_lets_a_send_due_in_seconds_go_first() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let session = FakeMailSession::new(plain_server()).blocking_idle();
    let sender = PassCountingSender::new(&session);
    let (stop_tx, handle) = spawn_idling(&svc, &vault, account_id, session, sender.clone()).await;
    let passes = sender.passes();

    let op = queue_send(&vault, account_id, 2);
    svc.mail_statuses().unwrap().nudge(account_id);
    settle_up_to(std::time::Duration::from_secs(30), || {
        vault.op(op.id).map(|o| o.state == OpState::Done).unwrap_or(false)
    })
    .await;

    assert_eq!(
        *sender.passes_at_send.lock().unwrap(),
        vec![passes],
        "the nudged pass waits for the send"
    );
    // And the pass the nudge asked for still runs.
    settle_up_to(std::time::Duration::from_secs(10), || sender.passes() > passes).await;
    stop_tx.send(true).unwrap();
    handle.await.unwrap().unwrap();
}

/// A drain that moved mail is followed by a pass: off Gmail a moved message
/// has a new uid in the mailbox it went to, which only a pass learns, and the
/// next op on it needs. Here between two mailboxes neither of which is the
/// inbox, so nothing `IDLE` watches changed.
#[tokio::test(start_paused = true)]
async fn a_drain_that_moved_mail_is_followed_by_a_pass() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        s.mailbox("Trash", Some(Role::Trash));
        s.append(
            "Archive",
            raw_message(
                "filed@example.com",
                None,
                "ada@example.com",
                "Filed",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
    }
    let session = FakeMailSession::new(server.clone()).blocking_idle();
    let sender = PassCountingSender::new(&session);
    let (stop_tx, handle) = spawn_idling(&svc, &vault, account_id, session, sender.clone()).await;
    let passes = sender.passes();

    let archive = vault
        .mailboxes(account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Archive)
        .unwrap();
    let thread =
        vault.list_threads(archive.id, &ThreadFilter::default(), None, 10).unwrap().threads[0].id;
    let op = vault.apply_thread_ops(&[thread], OpKind::Trash, Origin::Person).unwrap().remove(0);
    svc.notify_outbox(account_id);

    settle(|| vault.op(op.id).map(|o| o.state == OpState::Done).unwrap_or(false)).await;
    settle(|| sender.passes() > passes).await;
    let trash = vault
        .mailboxes(account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Trash)
        .unwrap();
    settle(|| !vault.mail_uid_set(trash.id).unwrap().is_empty()).await;
    stop_tx.send(true).unwrap();
    handle.await.unwrap().unwrap();
}

/// Read on a phone while the task was draining rather than in `IDLE`, which
/// hears only what changes after it starts: the inbox's `HIGHESTMODSEQ` has
/// moved since the last pass, so a pass runs now rather than at the sync
/// interval.
#[tokio::test(start_paused = true)]
async fn an_inbox_changed_outside_idle_gets_a_pass_after_an_outbox_wake() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    let uid = server.lock().unwrap().append(
        "INBOX",
        raw_message(
            "unread@example.com",
            None,
            "ada@example.com",
            "Unread",
            "01 Jan 2024 10:00:00 +0000",
            "x",
        ),
        jatayu_mail::session::Flags::NONE,
        None,
    );
    let session = FakeMailSession::new(server.clone()).blocking_idle();
    let sender = PassCountingSender::new(&session);
    let (stop_tx, handle) = spawn_idling(&svc, &vault, account_id, session, sender.clone()).await;
    let passes = sender.passes();

    // The fake's `IDLE` hears nothing of a flag change: as if it happened
    // between one `IDLE` and the next.
    server.lock().unwrap().set_flags("INBOX", uid, jatayu_mail::session::Flags::SEEN);
    svc.notify_outbox(account_id);

    settle(|| sender.passes() > passes).await;
    let inbox = vault
        .mailboxes(account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap();
    settle(|| vault.message_by_uid(inbox.id, uid).unwrap().is_some_and(|m| !m.flags.unread()))
        .await;
    stop_tx.send(true).unwrap();
    handle.await.unwrap().unwrap();
}

/// A send that comes due while a pass is running goes between two of its
/// mailboxes, not once the whole pass is over -- and only sends: an archive
/// due as well waits for the drain after the pass, where it always ran.
#[tokio::test]
async fn a_send_due_during_a_pass_goes_between_two_mailboxes() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    server.lock().unwrap().append(
        "INBOX",
        raw_message(
            "hi@example.com",
            None,
            "ada@example.com",
            "Hi",
            "01 Jan 2024 10:00:00 +0000",
            "x",
        ),
        flags_seen(),
        None,
    );
    let mut session = FakeMailSession::new(server);
    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let inbox = vault
        .mailboxes(account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap();
    let thread =
        vault.list_threads(inbox.id, &ThreadFilter::default(), None, 10).unwrap().threads[0].id;
    let archive =
        vault.apply_thread_ops(&[thread], OpKind::Archive, Origin::Person).unwrap().remove(0);
    let send = queue_send(&vault, account_id, 0);

    let sender = PassCountingSender::new(&session);
    let start = session.commands().len();
    let mut send_due = crate::mail::sync::task::SendDueMail::new(&svc, account_id, &sender);
    passes::sync_once_with(&ctx, &mut session, &mut labels, &mut threads, &mut send_due)
        .await
        .unwrap();

    assert_eq!(vault.op(send.id).unwrap().state, OpState::Done);
    let log = session.commands();
    let at_send = sender.log_at_send.lock().unwrap()[0];
    let selected_before: Vec<_> =
        log[start..at_send].iter().filter(|c| c.starts_with("select ")).collect();
    assert_eq!(
        selected_before,
        ["select INBOX"],
        "sent once the first mailbox was in, not after the last: {log:?}"
    );
    assert_eq!(
        vault.op(archive.id).unwrap().state,
        OpState::Pending,
        "an archive waits for the drain after the pass"
    );
}

/// Once a send has gone, its copy in Drafts is out of the vault at once --
/// not left in the Drafts list until the next sync of Drafts noticed it gone
/// -- and an open window is told the draft changed, which it otherwise heard
/// only when a pass happened to bring in mail.
#[tokio::test]
async fn a_sent_drafts_copy_leaves_drafts_at_once_and_the_window_is_told() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let changes = Arc::new(Changes::default());
    svc.set_events(changes.clone());
    let server = plain_server();
    let uid = {
        let mut s = server.lock().unwrap();
        s.mailbox("Drafts", Some(Role::Drafts));
        s.append(
            "Drafts",
            raw_message(
                "copy-1@example.com",
                None,
                "me@example.com",
                "Soon",
                "01 Jan 2024 10:00:00 +0000",
                "Soon",
            ),
            jatayu_mail::session::Flags::DRAFT,
            None,
        )
    };
    let mut session = FakeMailSession::new(server.clone());
    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();
    let drafts = vault
        .mailboxes(account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Drafts)
        .unwrap();
    assert!(vault.message_by_uid(drafts.id, uid).unwrap().is_some(), "the copy synced");

    let mut draft = Draft::new(account_id, "me@example.com", Origin::Person);
    draft.to = vec![Address::bare("bob@example.com")];
    draft.subject = "Soon".into();
    draft.body_html = "<p>Soon</p>".into();
    draft.server_copy = Some(jatayu_core::mail::DraftServerCopy {
        mailbox: "Drafts".into(),
        uid,
        message_id: Some("copy-1@example.com".into()),
    });
    vault.save_draft(&draft).unwrap();
    vault.queue_draft_send(draft.id, Timestamp::now(), Origin::Person).unwrap();
    changes.0.lock().unwrap().clear();

    let report =
        crate::mail::outbox::drain_outbox(&svc, account_id, &mut session, &FakeSender::default())
            .await
            .unwrap();
    assert_eq!(report.done, 1, "{report:?}");

    assert_eq!(vault.draft(draft.id).unwrap().state, jatayu_core::mail::DraftState::Sent);
    assert!(s_message_count(&server, "Drafts") == 0, "off the server");
    assert!(vault.message_by_uid(drafts.id, uid).unwrap().is_none(), "and out of the vault");
    let told = changes.0.lock().unwrap().clone();
    assert!(
        told.iter()
            .any(|c| c.kind == Kind::Draft && c.id.as_deref() == Some(&draft.id.to_string())),
        "the window hears the draft changed: {told:?}"
    );
    assert!(
        told.iter().any(|c| c.kind == Kind::Thread && c.id.is_none()),
        "and reloads its lists: {told:?}"
    );
}

/// Every [`Change`] raised while this is the service's sink, kept for a test
/// to count.
#[derive(Default)]
struct Changes(Mutex<Vec<Change>>);

impl EventSink for Changes {
    fn changed(&self, change: Change) {
        self.0.lock().unwrap().push(change);
    }
}

impl Changes {
    fn threads(&self) -> Vec<Change> {
        self.0.lock().unwrap().iter().filter(|c| c.kind == Kind::Thread).cloned().collect()
    }
}

/// A background pass writes under no command, so the account task itself
/// has to tell an open window when one brought something in -- once per
/// pass, as a batch, and not at all for a pass that found nothing. The same
/// three passes also pin two things a pass must leave alone or keep up to
/// date: an edit made in Settings while the task was running survives the
/// pass's own status write (`credential::mark_ok` used to save the copy the
/// task loaded at start, wiping it), and `sync_status` learns when the
/// account was last checked.
#[tokio::test]
async fn a_background_pass_that_brings_in_mail_tells_the_window_once() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        for i in 0..3 {
            s.append(
                "INBOX",
                raw_message(
                    &format!("first-{i}@example.com"),
                    None,
                    "alice@example.com",
                    &format!("Hello {i}"),
                    "01 Jan 2024 10:00:00 +0000",
                    "x",
                ),
                flags_seen(),
                None,
            );
        }
    }
    let changes = Arc::new(Changes::default());
    svc.set_events(changes.clone());
    let session = FakeMailSession::new(server.clone()).blocking_idle();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);

    let svc_task = svc.clone();
    let vault_task = vault.clone();
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

    let statuses = svc.mail_statuses().unwrap();
    let last_synced = || {
        let progress = statuses.all().into_iter().find(|p| p.account_id == account_id);
        progress.and_then(|p| p.last_synced_at)
    };

    // The first pass: three new threads, one announcement, and a batch one
    // -- no `id`, which is what makes a client reload the whole list.
    // Real seconds rather than `settle`'s handful of milliseconds: each of
    // these waits is a whole pass, bodies and all, on the blocking pool.
    let pass = std::time::Duration::from_secs(30);
    settle_up_to(pass, || last_synced().is_some()).await;
    let first = last_synced().unwrap();
    let announced = changes.threads();
    assert_eq!(announced.len(), 1, "one pass, one announcement: {announced:?}");
    assert!(announced[0].id.is_none() && announced[0].ids.is_empty(), "{announced:?}");

    // Settings, while the task runs.
    let mut edited = vault.account(account_id).unwrap();
    edited.display_name = "Work".into();
    edited.sync_minutes = Some(30);
    vault.save_account(&edited).unwrap();

    // A second pass with nothing new on the server says nothing.
    statuses.nudge(account_id);
    settle_up_to(pass, || last_synced().is_some_and(|t| t > first)).await;
    let second = last_synced().unwrap();
    assert_eq!(changes.threads().len(), 1, "a pass that changed nothing announced something");
    let account = vault.account(account_id).unwrap();
    assert_eq!(account.display_name, "Work", "the pass's status write wiped a Settings edit");
    assert_eq!(account.sync_minutes, Some(30), "the pass's status write wiped a Settings edit");
    assert!(account.last_synced_at.is_some_and(|t| t > first), "{:?}", account.last_synced_at);

    // New mail arrives: the next pass announces it, once.
    server.lock().unwrap().append(
        "INBOX",
        raw_message(
            "later@example.com",
            None,
            "bob@example.com",
            "Later",
            "02 Jan 2024 10:00:00 +0000",
            "y",
        ),
        flags_seen(),
        None,
    );
    statuses.nudge(account_id);
    settle_up_to(pass, || last_synced().is_some_and(|t| t > second)).await;
    assert_eq!(changes.threads().len(), 2, "{:?}", changes.threads());

    stop_tx.send(true).unwrap();
    handle.await.unwrap().unwrap();
}

/// A deleted account stays deleted: the pass's own status write re-reads
/// the account before saving it, and finding it gone, writes nothing --
/// rather than saving the copy the task started with and bringing it back.
#[tokio::test]
async fn a_status_write_does_not_bring_back_a_deleted_account() {
    let (_svc, vault, account_id, _dir) = service_test_env();
    let stale = vault.account(account_id).unwrap();
    vault.delete_account(account_id).unwrap();

    crate::mail::sync::credential::mark_ok(&vault, &stale, Timestamp::now());
    crate::mail::sync::credential::mark_error(&vault, &stale, "refused", Timestamp::now());
    crate::mail::sync::credential::mark_needs_sign_in(&vault, &stale, "expired", Timestamp::now());

    assert!(vault.account(account_id).is_err(), "a status write resurrected a deleted account");
}

/// Poll the runtime until `f` is true or `tries` yields have gone by,
/// without needing virtual time to move -- the same tolerance
/// `crate::supervisor`'s own tests give a task that only makes progress
/// between this test's `.await` points on a current-thread runtime.
///
/// A real, wall-clock sleep between yields as well as the yield itself: the
/// spawned account task's own `bodies_pass` finishes its work on
/// `tokio::task::spawn_blocking`'s real OS thread pool, doing genuine
/// SQLite writes -- work that takes actual wall-clock time regardless of
/// `start_paused`, which freezes only `tokio::time`'s virtual clock. A pure
/// `yield_now` spin can complete its whole budget of iterations in
/// microseconds, far faster than that thread pool can finish a single
/// `fsync`, and would never see it -- `std::thread::sleep`, not
/// `tokio::time::sleep`, is what actually waits here.
///
/// Five thousand tries -- about five real seconds at most -- where it was
/// five hundred: half a second covered a first sync's fsyncs and a tantivy
/// commit on a laptop, but not on a busy CI runner, where these tests then
/// failed "did not settle" with nothing actually wrong. Waiting longer
/// proves exactly as much: the virtual clock stays paused throughout (this
/// task is never idle, so it never auto-advances), so no timer can fire
/// however long the real wait, and a passing test still returns the moment
/// `f` holds.
pub(super) async fn settle(f: impl Fn() -> bool) {
    for _ in 0..5000 {
        if f() {
            return;
        }
        tokio::task::yield_now().await;
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(f(), "did not settle");
}

/// As [`settle`], but for a wait measured in real seconds rather than a
/// handful of milliseconds -- what a test that genuinely needs wall-clock
/// time to pass (a `not_before` some real duration out, checked against
/// `jiff::Timestamp::now`, which no paused `tokio::time` clock touches)
/// polls with instead.
async fn settle_up_to(timeout: std::time::Duration, f: impl Fn() -> bool) {
    let start = std::time::Instant::now();
    loop {
        if f() {
            return;
        }
        assert!(start.elapsed() < timeout, "did not settle within {timeout:?}");
        tokio::task::yield_now().await;
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
