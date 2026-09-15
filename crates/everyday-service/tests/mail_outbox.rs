//! Mail's write surface and the outbox drain loop, end to end against a real
//! SQLite vault.
//!
//! `everyday-core`'s conformance suite already checks the storage layer in
//! isolation; what is worth checking here, once, through the command layer
//! and `crates/everyday-service/src/outbox.rs` together, is the handful of
//! behaviours `docs/plans/mail.md`'s phase 3 section is explicit about and
//! that only show up when several pieces move in the same call: an
//! optimistic local write that a permanent failure actually reverts, undo
//! send's window closing for real, a debounce that only fires once, a
//! snooze that ends itself, and a batch action's change event naming every
//! thread it touched.

use std::sync::{Arc, Mutex};

use everyday_core::account::{Account, Provider};
use everyday_core::id::{AccountId, MailMessageId, MailboxId, PackId, ThreadId};
use everyday_core::mail::{Address, Mailbox, MailboxRole, Message, MessageFlags, OpState};
use everyday_core::packstore::PackRef;
use everyday_core::store::mail::IngestMessage;
use everyday_mail::compose::Built;
use everyday_mail::outbox::Sender;
use everyday_mail::session::{
    Capabilities, Changes, Flags, IdleEvent, MailError, MailSession, MailboxState, RawStream,
    RemoteHeader, RemoteMailbox, Result as SessionResult, SyncCursor, Uid, UidSet,
};
use everyday_service::ctx::Ctx;
use everyday_service::events::{Change, EventSink};
use everyday_service::outbox::{drain_outbox, release_due_snoozes};
use everyday_service::{Service, error::CommandError};
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

#[allow(dead_code)]
mod support;

fn service() -> (Arc<Service>, tempfile::TempDir) {
    support::vault::service(None)
}

async fn call(svc: &Arc<Service>, name: &str, args: Value) -> Value {
    svc.call(Ctx::local(), name, args).await.unwrap_or_else(|e| panic!("{name}: {e}"))
}

async fn fails(svc: &Arc<Service>, name: &str, args: Value) -> CommandError {
    svc.call(Ctx::local(), name, args).await.expect_err("expected a failure")
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

/// An account with nowhere to sync from -- these tests never open a real
/// socket, only the vault and the pure outbox state machine.
fn seed_account(svc: &Arc<Service>) -> AccountId {
    let vault = svc.get().unwrap();
    let account = Account::new(Provider::Custom, "me@example.com");
    let id = account.id;
    vault.save_account(&account).unwrap();
    id
}

fn seed_mailbox(
    svc: &Arc<Service>,
    account: AccountId,
    name: &str,
    role: MailboxRole,
) -> MailboxId {
    let vault = svc.get().unwrap();
    let mailbox = Mailbox::new(account, name, role);
    let id = mailbox.id;
    vault.save_mailbox(&mailbox).unwrap();
    id
}

/// One message, filed in `mailbox` at `uid`, in a thread of its own --
/// returns the thread id [`everyday_core::mail::Thread`]s are read back by.
fn seed_message(svc: &Arc<Service>, account: AccountId, mailbox: MailboxId, uid: u32) -> ThreadId {
    let vault = svc.get().unwrap();
    let thread_id = ThreadId::new();
    let message_id = MailMessageId::new();
    let message = Message {
        id: message_id,
        account_id: account,
        thread_id,
        message_id_header: format!("<{message_id}@example.com>"),
        date: Timestamp::now(),
        from: Address::bare("sender@example.com"),
        to: vec![Address::bare("me@example.com")],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: "A message".into(),
        snippet: String::new(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 128,
        category: None,
        pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
        invite: None,
    };
    vault.ingest_mail(account, vec![IngestMessage { message, mailbox, uid }]).unwrap();
    thread_id
}

// ---- a session and a sender good enough to drive `drain_outbox` -----------

#[derive(Default)]
struct FakeSession {
    capabilities: Capabilities,
    /// One-shot failure, consumed the first time any call below checks it --
    /// what lets a test drive `drain_outbox` into its retryable branch
    /// without a real socket to disconnect.
    fail_next: Option<MailError>,
    selected: Option<String>,
    /// Every call this fake was asked to make, in order -- what a test
    /// checks to prove something (a label mailbox, a Sent copy) was never
    /// touched, not only that the right thing was.
    calls: Vec<String>,
}

impl FakeSession {
    fn take_failure(&mut self) -> SessionResult<()> {
        match self.fail_next.take() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

#[allow(async_fn_in_trait)]
impl MailSession for FakeSession {
    async fn mailboxes(&mut self) -> SessionResult<Vec<RemoteMailbox>> {
        Ok(Vec::new())
    }
    async fn select(&mut self, mailbox: &str) -> SessionResult<MailboxState> {
        self.take_failure()?;
        self.selected = Some(mailbox.to_string());
        self.calls.push(format!("select {mailbox}"));
        Ok(MailboxState::default())
    }
    async fn changes_since(&mut self, _c: &SyncCursor, _k: &UidSet) -> SessionResult<Changes> {
        Ok(Changes::default())
    }
    async fn headers(&mut self, _uids: &UidSet) -> SessionResult<Vec<RemoteHeader>> {
        Ok(Vec::new())
    }
    async fn raw(&mut self, _uids: &UidSet) -> SessionResult<RawStream<'_>> {
        Ok(Box::pin(futures::stream::empty::<SessionResult<(Uid, Vec<u8>)>>()))
    }
    async fn store_flags(&mut self, uids: &UidSet, add: Flags, remove: Flags) -> SessionResult<()> {
        self.calls.push(format!(
            "store_flags {} on {:?} +{add:?} -{remove:?}",
            uids.to_imap(),
            self.selected
        ));
        Ok(())
    }
    async fn store_gmail_labels(
        &mut self,
        uids: &UidSet,
        add: &[String],
        remove: &[String],
    ) -> SessionResult<()> {
        self.calls.push(format!(
            "store_gmail_labels {} on {:?} +{add:?} -{remove:?}",
            uids.to_imap(),
            self.selected
        ));
        Ok(())
    }
    async fn move_to(&mut self, uids: &UidSet, mailbox: &str) -> SessionResult<()> {
        self.calls.push(format!("move_to {} on {:?} -> {mailbox}", uids.to_imap(), self.selected));
        Ok(())
    }
    async fn delete(&mut self, uids: &UidSet) -> SessionResult<()> {
        self.calls.push(format!("delete {} on {:?}", uids.to_imap(), self.selected));
        Ok(())
    }
    async fn append(
        &mut self,
        mailbox: &str,
        _raw: &[u8],
        flags: Flags,
    ) -> SessionResult<Option<Uid>> {
        self.calls.push(format!("append to {mailbox} ({flags:?})"));
        Ok(Some(1))
    }
    async fn search_message_id(
        &mut self,
        _mailbox: &str,
        _message_id: &str,
    ) -> SessionResult<Option<Uid>> {
        Ok(None)
    }
    async fn idle(&mut self, _stop: tokio::sync::watch::Receiver<()>) -> SessionResult<IdleEvent> {
        Ok(IdleEvent::Stopped)
    }
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
}

#[derive(Default)]
struct FakeSender;

#[allow(async_fn_in_trait)]
impl Sender for FakeSender {
    async fn send(&self, built: &Built) -> SessionResult<everyday_mail::smtp::SendReceipt> {
        Ok(everyday_mail::smtp::SendReceipt {
            accepted: built.envelope_to.clone(),
            server_response: "250 Ok".into(),
        })
    }
}

fn fake_error() -> MailError {
    MailError::Unsupported("an Archive mailbox")
}

// ---- optimistic archive, then revert on a permanent failure ---------------

#[tokio::test]
async fn archiving_hides_a_thread_and_a_permanent_failure_brings_it_back() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    // No Archive mailbox is ever registered for this account, so the
    // executor's `archive` branch (non-Gmail) refuses with
    // `MailError::Unsupported` -- a real, deterministic permanent failure,
    // not a fake one this test has to fabricate.
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    let thread = seed_message(&svc, account, inbox, 1);

    let before = call(&svc, "list_threads", json!({ "mailbox": inbox })).await;
    assert_eq!(before["threads"].as_array().unwrap().len(), 1);

    let ops = call(&svc, "archive", json!({ "threads": [thread] })).await;
    let op_id = ops.as_array().unwrap()[0]["id"].as_str().unwrap().to_string();

    // The optimistic write already hid it from the inbox list.
    let hidden = call(&svc, "list_threads", json!({ "mailbox": inbox })).await;
    assert!(
        hidden["threads"].as_array().unwrap().is_empty(),
        "archive hides the thread immediately"
    );

    let mut session = FakeSession::default();
    let sender = FakeSender;
    let report = drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert_eq!(report.attempted, 1);
    assert_eq!(report.failed, 1, "no Archive mailbox is a permanent failure");
    assert_eq!(report.done, 0);

    let restored = call(&svc, "list_threads", json!({ "mailbox": inbox })).await;
    assert_eq!(
        restored["threads"].as_array().unwrap().len(),
        1,
        "a permanent failure reverts the optimistic hide"
    );

    let vault = svc.get().unwrap();
    let op = vault.op(op_id.parse().unwrap()).unwrap();
    assert_eq!(op.state, OpState::Failed { permanent: true, message: fake_error().to_string() });
}

// ---- undo send, inside and outside the window ------------------------------

#[tokio::test]
async fn undo_send_works_inside_the_window_and_refuses_past_it() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);

    let mut draft = call(&svc, "new_draft", json!({ "account": account })).await;
    draft["to"] = json!([{ "name": "", "email": "bob@example.com" }]);
    draft["subject"] = json!("Hello");
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;
    let draft_id = draft["id"].as_str().unwrap().to_string();

    // Inside the window: undo succeeds and the draft is editable again.
    let sent = call(&svc, "send_draft", json!({ "id": draft_id })).await;
    assert!(matches!(sent["state"]["type"].as_str(), Some("queued")));
    let undone = call(&svc, "undo_send", json!({ "draftId": draft_id })).await;
    assert_eq!(undone["state"]["type"].as_str(), Some("editing"));

    // Past the window: `send_draft` again, then age the op's `not_before`
    // into the past directly -- the same state a real clock reaching it
    // would produce, without a test sleeping for it.
    let sent_again = call(&svc, "send_draft", json!({ "id": draft_id })).await;
    let op_id: everyday_core::id::OpId =
        sent_again["state"]["op"].as_str().unwrap().parse().unwrap();
    let vault = svc.get().unwrap();
    let mut op = vault.op(op_id).unwrap();
    op.not_before = Timestamp::now() - SignedDuration::from_secs(1);
    vault.update_op(&op).unwrap();

    let err = fails(&svc, "undo_send", json!({ "draftId": draft_id })).await;
    assert_eq!(err.code, "invalid");
    let still_queued = call(&svc, "list_drafts", json!({ "account": account })).await;
    let d = &still_queued.as_array().unwrap()[0];
    assert_eq!(d["state"]["type"].as_str(), Some("queued"), "past the window, the send stands");
}

#[tokio::test]
async fn send_draft_refuses_a_draft_with_no_recipient() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let draft = call(&svc, "new_draft", json!({ "account": account })).await;
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;
    let err = fails(&svc, "send_draft", json!({ "id": draft["id"] })).await;
    assert_eq!(err.code, "invalid");
}

// ---- draft append coalescing -----------------------------------------------

#[tokio::test]
async fn saving_a_draft_twice_quickly_enqueues_one_append() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let draft = call(&svc, "new_draft", json!({ "account": account })).await;

    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;

    let vault = svc.get().unwrap();
    let due =
        vault.due_ops(account, Timestamp::now() + SignedDuration::from_secs(3_600), 10).unwrap();
    assert_eq!(
        due.len(),
        1,
        "the second save within thirty seconds must not enqueue a second append"
    );
}

// ---- recovering an op stranded `InFlight` by a crash -----------------------

#[tokio::test]
async fn an_inflight_op_left_by_a_crash_is_recovered_and_drains() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    seed_mailbox(&svc, account, "Archive", MailboxRole::Archive);
    let thread = seed_message(&svc, account, inbox, 1);

    let ops = call(&svc, "archive", json!({ "threads": [thread] })).await;
    let op_id: everyday_core::id::OpId =
        ops.as_array().unwrap()[0]["id"].as_str().unwrap().parse().unwrap();

    let vault = svc.get().unwrap();
    // What a crash between `drain_outbox` claiming this op and recording
    // how it went leaves behind -- nothing else ever moves an op out of
    // `InFlight` again, per `crate::outbox`'s own module docs.
    let mut stranded = vault.op(op_id).unwrap();
    stranded.transition_to(OpState::InFlight).unwrap();
    vault.update_op(&stranded).unwrap();
    let attempts_before = stranded.attempts;

    let mut session = FakeSession::default();
    everyday_service::outbox::recover_inflight_ops(&svc, account, &mut session).await.unwrap();

    let recovered = vault.op(op_id).unwrap();
    assert_eq!(recovered.state, OpState::Pending, "a stranded op must go back to work");
    assert!(recovered.attempts > attempts_before, "recovery counts as a failed attempt");

    // Recovery did not just flip a state bit: the op is genuinely runnable
    // again.
    let sender = FakeSender;
    let report = drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert_eq!(report.done, 1, "{report:?}");
}

// ---- snooze release ---------------------------------------------------------

#[tokio::test]
async fn a_snooze_whose_moment_has_passed_is_released() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    let thread = seed_message(&svc, account, inbox, 1);

    let past = Timestamp::now() - SignedDuration::from_secs(1);
    call(&svc, "snooze", json!({ "threads": [thread], "until": past.to_string() })).await;

    let vault = svc.get().unwrap();
    let (t, _) = vault.thread(thread).unwrap();
    assert!(t.snoozed_until.is_some());

    let released = release_due_snoozes(&svc).await.unwrap();
    assert_eq!(released, 1);

    let (t, _) = vault.thread(thread).unwrap();
    assert!(t.snoozed_until.is_none(), "a passed snooze is released");
}

// ---- batch mark read emits every thread's id -------------------------------

#[tokio::test]
async fn marking_a_batch_read_emits_every_threads_id() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    let one = seed_message(&svc, account, inbox, 1);
    let two = seed_message(&svc, account, inbox, 2);

    let collector = Arc::new(Collector::default());
    svc.set_events(collector.clone());

    call(&svc, "mark_read", json!({ "threads": [one, two] })).await;

    let changes = collector.changes.lock().unwrap();
    let thread_change = changes
        .iter()
        .find(|c| matches!(c.kind, everyday_service::events::Kind::Thread))
        .expect("mark_read raises a Thread change");
    let mut ids = thread_change.ids.clone();
    ids.sort();
    let mut expected = vec![one.to_string(), two.to_string()];
    expected.sort();
    assert_eq!(ids, expected);

    let vault = svc.get().unwrap();
    let (_, messages) = vault.thread(one).unwrap();
    assert!(messages[0].flags.seen);
    let (_, messages) = vault.thread(two).unwrap();
    assert!(messages[0].flags.seen);
}

#[tokio::test]
async fn unsnoozing_early_clears_it_with_no_outbox_op() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    let thread = seed_message(&svc, account, inbox, 1);

    let far_future = Timestamp::now() + SignedDuration::from_secs(3_600);
    call(&svc, "snooze", json!({ "threads": [thread], "until": far_future.to_string() })).await;
    call(&svc, "unsnooze", json!({ "threads": [thread] })).await;

    let vault = svc.get().unwrap();
    let (t, _) = vault.thread(thread).unwrap();
    assert!(t.snoozed_until.is_none());
    // The Snooze op itself is still there (unsnooze never touches the
    // outbox), and it is already what the plan calls "local only" -- the
    // drain loop will find it, run it as a no-op, and mark it done, which
    // this test does not need to prove twice given `everyday-mail`'s own
    // executor tests already cover it.
    let due =
        vault.due_ops(account, Timestamp::now() + SignedDuration::from_secs(3_600), 10).unwrap();
    assert_eq!(due.len(), 1);
}

// ---- backoff: the first retry waits thirty seconds, not sixty --------------

/// The regression for the backoff off-by-one: `attempts` must be read by
/// [`everyday_core::mail::backoff_for_attempt`] *before* it is incremented,
/// so a first-ever retryable failure waits the schedule's first entry
/// (thirty seconds), not its second (a minute).
#[tokio::test]
async fn a_first_retry_backs_off_thirty_seconds_not_sixty() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    seed_mailbox(&svc, account, "Archive", MailboxRole::Archive);
    let thread = seed_message(&svc, account, inbox, 1);

    let ops = call(&svc, "archive", json!({ "threads": [thread] })).await;
    let op_id: everyday_core::id::OpId =
        ops.as_array().unwrap()[0]["id"].as_str().unwrap().parse().unwrap();

    let mut session = FakeSession {
        fail_next: Some(MailError::Network("connection reset".into())),
        ..Default::default()
    };
    let sender = FakeSender;
    let before = Timestamp::now();
    let report = drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert_eq!(report.retried, 1, "{report:?}");

    let vault = svc.get().unwrap();
    let op = vault.op(op_id).unwrap();
    assert_eq!(op.attempts, 1);
    let waited = before.duration_until(op.not_before);
    assert!(
        waited >= SignedDuration::from_secs(25) && waited < SignedDuration::from_secs(45),
        "the first retry must back off about thirty seconds, not sixty: waited {waited:?}"
    );
}

// ---- a draft's server copy: preserved by autosave, removed by send/discard -

/// The regression for "draft server copies leak" (b): an ordinary autosave
/// must never erase the server-owned fields -- `serverCopy` chief among
/// them -- that the last `AppendDraft` recorded, even though the client's
/// own copy of the draft never learns them and so resends without them.
#[tokio::test]
async fn saving_a_draft_again_does_not_erase_its_recorded_server_copy() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    seed_mailbox(&svc, account, "Drafts", MailboxRole::Drafts);

    let draft = call(&svc, "new_draft", json!({ "account": account })).await;
    let draft_id: everyday_core::id::DraftId = draft["id"].as_str().unwrap().parse().unwrap();
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;

    let mut session = FakeSession::default();
    let sender = FakeSender;
    let report = drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert_eq!(report.done, 1, "the AppendDraft must have run: {report:?}");

    let vault = svc.get().unwrap();
    let after_append = vault.draft(draft_id).unwrap();
    assert!(after_append.server_copy.is_some(), "AppendDraft must have recorded a server copy");

    // The client's own next autosave: it never learnt where the append
    // landed, so its JSON carries no `serverCopy` at all -- exactly the
    // stale write that used to overwrite the vault's own record of it.
    let mut stale = draft;
    stale["subject"] = json!("Edited a little more");
    assert!(stale.get("serverCopy").is_none(), "a client never sends this back");
    call(&svc, "save_draft", json!({ "draft": stale })).await;

    let after_second_save = vault.draft(draft_id).unwrap();
    assert_eq!(
        after_second_save.server_copy, after_append.server_copy,
        "an ordinary autosave must not erase the server-owned server_copy field"
    );
    assert_eq!(
        after_second_save.subject, "Edited a little more",
        "the edit itself must still land"
    );
}

/// (a), the send half: once a queued send actually reaches the server, the
/// draft's own stale copy in Drafts must be deleted.
#[tokio::test]
async fn sending_a_draft_removes_its_server_copy() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    seed_mailbox(&svc, account, "Drafts", MailboxRole::Drafts);
    seed_mailbox(&svc, account, "Sent", MailboxRole::Sent);

    let mut draft = call(&svc, "new_draft", json!({ "account": account })).await;
    draft["to"] = json!([{ "name": "", "email": "bob@example.com" }]);
    draft["subject"] = json!("Hello");
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;
    let draft_id: everyday_core::id::DraftId = draft["id"].as_str().unwrap().parse().unwrap();

    let mut session = FakeSession::default();
    let sender = FakeSender;
    // The AppendDraft first, giving the draft its own server copy to leak.
    let report = drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert_eq!(report.done, 1, "{report:?}");
    assert!(vault_draft(&svc, draft_id).server_copy.is_some());

    call(&svc, "send_draft", json!({ "id": draft_id })).await;
    let vault = svc.get().unwrap();
    let mut op = vault
        .due_ops(account, Timestamp::now() + SignedDuration::from_secs(3_600), 10)
        .unwrap()
        .into_iter()
        .find(|o| o.state != OpState::Done)
        .expect("the Send op");
    op.not_before = Timestamp::now() - SignedDuration::from_secs(1);
    vault.update_op(&op).unwrap();

    let report2 = drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert_eq!(report2.done, 1, "the Send must have run: {report2:?}");

    assert!(
        session.calls.iter().any(|c| c.contains("delete")),
        "the stale Drafts copy must be deleted after sending: {:?}",
        session.calls
    );
}

/// (a), the discard half: discarding a draft that had a server copy
/// enqueues its removal, and draining the outbox actually removes it.
#[tokio::test]
async fn discarding_a_draft_removes_its_server_copy() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    seed_mailbox(&svc, account, "Drafts", MailboxRole::Drafts);

    let draft = call(&svc, "new_draft", json!({ "account": account })).await;
    let draft_id: everyday_core::id::DraftId = draft["id"].as_str().unwrap().parse().unwrap();
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;

    let mut session = FakeSession::default();
    let sender = FakeSender;
    let report = drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert_eq!(report.done, 1, "the AppendDraft must have run: {report:?}");
    assert!(vault_draft(&svc, draft_id).server_copy.is_some());

    call(&svc, "discard_draft", json!({ "id": draft_id })).await;

    let report2 = drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert_eq!(report2.done, 1, "the DiscardDraft op must have run: {report2:?}");
    assert!(
        session.calls.iter().any(|c| c.contains("delete")),
        "discarding must delete the server copy: {:?}",
        session.calls
    );
}

fn vault_draft(svc: &Arc<Service>, id: everyday_core::id::DraftId) -> everyday_core::mail::Draft {
    svc.get().unwrap().draft(id).unwrap()
}
