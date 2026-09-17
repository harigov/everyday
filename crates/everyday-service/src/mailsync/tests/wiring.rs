use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use everyday_core::mail::{Address, Draft, MailboxRole, OpKind, OpState, Origin};
use everyday_core::store::mail::ThreadFilter;
use everyday_mail::session::{GmailMeta, Role};
use jiff::Timestamp;

use crate::mailsync::discovery::LabelMailboxes;
use crate::mailsync::ingest::ThreadIndex;
use crate::mailsync::passes::{self, SyncContext};

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
    let by_email: HashMap<&str, &everyday_core::mail::Address> =
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
        crate::outbox::drain_outbox(&svc, account_id, &mut session, &sender).await.unwrap();
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

#[tokio::test]
async fn draining_a_send_appends_the_sent_copy_and_marks_the_draft_sent() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    let mut session = FakeMailSession::new(server.clone());

    // A sync first, empty mailboxes and all: `special_use(Sent)` -- which
    // `everyday_mail::outbox::send` reads to know where to `APPEND` its own
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
        crate::outbox::drain_outbox(&svc, account_id, &mut session, &sender).await.unwrap();
    assert_eq!(report.done, 1, "{report:?}");
    assert_eq!(sender.sent.lock().unwrap().len(), 1, "the sender must have been asked to send it");
    assert_eq!(vault.op(op.id).unwrap().state, OpState::Done);
    assert_eq!(vault.draft(draft.id).unwrap().state, everyday_core::mail::DraftState::Sent);

    let s = server.lock().unwrap();
    assert_eq!(
        s.mailboxes["Sent"].messages.len(),
        1,
        "a non-Gmail server needs its own Sent copy appended"
    );
}

/// The regression for the duplicate-send risk `crate::outbox::already_sent`
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
    // `everyday_mail::outbox::send` does before it ever calls the sender,
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

    crate::outbox::recover_inflight_ops(&svc, account_id, &mut session).await.unwrap();

    assert_eq!(
        vault.op(op.id).unwrap().state,
        OpState::Done,
        "already on the server -- recovered as done, not requeued to send again"
    );
    assert_eq!(vault.draft(draft.id).unwrap().state, everyday_core::mail::DraftState::Sent);

    // And, proof this genuinely skipped sending rather than merely getting
    // lucky: nothing is left pending to drain, and the sender below is
    // never asked to send anything.
    let sender = FakeSender::default();
    let report =
        crate::outbox::drain_outbox(&svc, account_id, &mut session, &sender).await.unwrap();
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
        crate::mailsync::task::run_account_with(
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
        crate::mailsync::task::run_account_with(
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
            Some(crate::mailsync::status::Phase::Idling)
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
    // stays exactly where `start_paused = true` left it. `POLL_INTERVAL`
    // is five minutes and `OUTBOX_RETRY_INTERVAL` three seconds, so the
    // only way this op can reach `Done` without either timer ever firing
    // is the notify itself having woken the `IDLE` `select!` -- which is
    // exactly the latency this test exists to prove.
    settle(|| vault.op(op.id).map(|o| o.state == OpState::Done).unwrap_or(false)).await;

    stop_tx.send(true).unwrap();
    handle.await.unwrap().unwrap();
}

/// The regression for the bug [`crate::mailsync::task::sleep_until_due`] fixes: a
/// send-at op queued ten seconds out must drain at about ten seconds, not
/// at `POLL_INTERVAL` (five minutes).
///
/// No paused clock here, unlike this module's other timing tests: `not_before`
/// due-ness is decided by [`jiff::Timestamp::now`] (the real wall clock),
/// never by `tokio::time`'s virtual one -- `next_pending_wake` converts a
/// real duration into a virtual sleep exactly once, the same trade
/// `crate::token_cache::deadline_from` makes, but the *due* check itself in
/// `crate::outbox::drain_outbox` reads the wall clock fresh on every drain.
/// So this test spends ten real seconds proving it, bounded well short of
/// `POLL_INTERVAL` by [`settle_up_to`]'s own timeout, which is the one
/// thing a wrong fix (falling back to `POLL_INTERVAL`) cannot pass short of
/// genuinely waiting five minutes.
#[tokio::test]
async fn a_send_at_op_drains_at_its_own_time_not_the_poll_interval() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    let session = FakeMailSession::new(server).blocking_idle();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);

    let svc_task = svc.clone();
    let vault_task = vault.clone();
    let handle = tokio::spawn(async move {
        crate::mailsync::task::run_account_with(
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
            Some(crate::mailsync::status::Phase::Idling)
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
    // `crate::mailsync::task`'s own module docs on its fourth `select!` arm.
    svc.notify_outbox(account_id);

    settle_up_to(std::time::Duration::from_secs(60), || {
        vault.op(op.id).map(|o| o.state == OpState::Done).unwrap_or(false)
    })
    .await;
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "took {elapsed:?} to drain a ten-second delay -- far too close to the five-minute poll"
    );

    stop_tx.send(true).unwrap();
    handle.await.unwrap().unwrap();
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
async fn settle(f: impl Fn() -> bool) {
    for _ in 0..500 {
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
