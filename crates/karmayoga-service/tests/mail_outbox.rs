//! Mail's write surface and the outbox drain loop, end to end against a real
//! SQLite vault.
//!
//! `karmayoga-core`'s conformance suite already checks the storage layer in
//! isolation; what is worth checking here, once, through the command layer
//! and `crates/karmayoga-service/src/outbox.rs` together, is the handful of
//! behaviours `docs/plans/mail.md`'s phase 3 section is explicit about and
//! that only show up when several pieces move in the same call: an
//! optimistic local write that a permanent failure actually reverts, undo
//! send's window closing for real, a debounce that only fires once, a
//! snooze that ends itself, and a batch action's change event naming every
//! thread it touched.

use std::sync::{Arc, Mutex};

use jiff::{SignedDuration, Timestamp};
use karmayoga_core::account::{Account, Provider};
use karmayoga_core::id::{AccountId, DraftId, MailMessageId, MailboxId, PackId, ThreadId};
use karmayoga_core::mail::{
    Address, AttendeeResponse, CategorySource, Draft, DraftCalendarPart, DraftState, Invite,
    InviteMethod, Mailbox, MailboxRole, Message, MessageFlags, Op, OpKind, OpState, OpTarget,
    Origin,
};
use karmayoga_core::packstore::PackRef;
use karmayoga_core::store::mail::IngestMessage;
use karmayoga_mail::compose::Built;
use karmayoga_mail::outbox::Sender;
use karmayoga_mail::session::{
    Capabilities, Changes, Flags, IdleEvent, MailError, MailSession, MailboxState, RawStream,
    RemoteHeader, RemoteMailbox, Result as SessionResult, SyncCursor, Uid, UidSet,
};
use karmayoga_service::ctx::Ctx;
use karmayoga_service::events::{Change, EventSink, Notification};
use karmayoga_service::outbox::{drain_outbox, release_due_snoozes};
use karmayoga_service::{Service, error::CommandError};
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
/// returns the thread id [`karmayoga_core::mail::Thread`]s are read back by.
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
        category_source: CategorySource::Rules,
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
    /// What [`FakeSession::search_message_id`] answers, keyed by the bare
    /// id a test seeded -- standing in for a message a previous attempt (or
    /// a crash-recovered one) already put on the server, in whichever
    /// mailbox the test names.
    found_message_ids: std::collections::HashMap<String, Uid>,
    /// Runs in the middle of an `APPEND` -- what a test uses to have something
    /// else happen while one is on its way to the server.
    during_append: Option<Box<dyn FnMut() + Send>>,
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
    async fn expunge(&mut self) -> SessionResult<()> {
        self.calls.push(format!("expunge on {:?}", self.selected));
        Ok(())
    }
    async fn append(
        &mut self,
        mailbox: &str,
        _raw: &[u8],
        flags: Flags,
    ) -> SessionResult<Option<Uid>> {
        self.calls.push(format!("append to {mailbox} ({flags:?})"));
        if let Some(during) = &mut self.during_append {
            during();
        }
        Ok(Some(1))
    }
    async fn search_message_id(
        &mut self,
        mailbox: &str,
        message_id: &str,
    ) -> SessionResult<Option<Uid>> {
        self.calls.push(format!("search_message_id {message_id} in {mailbox}"));
        Ok(self.found_message_ids.get(message_id).copied())
    }
    async fn idle(&mut self, _stop: tokio::sync::watch::Receiver<()>) -> SessionResult<IdleEvent> {
        Ok(IdleEvent::Stopped)
    }
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
}

#[derive(Default)]
struct FakeSender {
    /// A one-shot failure a test can seed to drive a `Send` op into
    /// `drain_outbox`'s permanent-failure branch without a real SMTP
    /// server to reject it -- `MailError::Server` and the like are never
    /// retryable (see `karmayoga_mail::outbox::is_retryable`), so this is
    /// enough to exercise `on_permanent_failure`'s own revert.
    fail: Option<MailError>,
    /// How many times [`Sender::send`] was actually asked to send
    /// something -- what a test checks to prove a cancelled or discarded
    /// `Send` op never reaches here at all, not only that its own state
    /// looks right.
    sent: std::sync::Mutex<u32>,
    /// The last message handed to [`Sender::send`], sent or not.
    raw: std::sync::Mutex<Vec<u8>>,
}

#[allow(async_fn_in_trait)]
impl Sender for FakeSender {
    async fn send(&self, built: &Built) -> SessionResult<karmayoga_mail::smtp::SendReceipt> {
        *self.raw.lock().unwrap() = built.raw.clone();
        if let Some(err) = &self.fail {
            return Err(err.clone());
        }
        *self.sent.lock().unwrap() += 1;
        Ok(karmayoga_mail::smtp::SendReceipt {
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
    let sender = FakeSender::default();
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

// ---- deleting forever, and emptying Trash ----------------------------------

/// One message of `thread`'s, filed in `mailbox` at `uid` -- several of these
/// make one conversation filed in more than one place.
fn seed_in_thread(
    svc: &Arc<Service>,
    account: AccountId,
    thread_id: ThreadId,
    mailbox: MailboxId,
    uid: u32,
) -> MailMessageId {
    let vault = svc.get().unwrap();
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
        subject: "Part of a conversation".into(),
        snippet: String::new(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 128,
        category: None,
        category_source: CategorySource::Rules,
        pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
        invite: None,
    };
    vault.ingest_mail(account, vec![IngestMessage { message, mailbox, uid }]).unwrap();
    message_id
}

fn listed(threads: &Value) -> Vec<String> {
    let mut ids: Vec<String> = threads["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn emptying_trash_deletes_what_is_in_trash_and_nothing_else() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    let trash = seed_mailbox(&svc, account, "Trash", MailboxRole::Trash);
    // One message of this conversation trashed, its reply still in the Inbox.
    let split = ThreadId::new();
    let trashed_part = seed_in_thread(&svc, account, split, trash, 5);
    let reply = seed_in_thread(&svc, account, split, inbox, 1);
    let trashed = seed_message(&svc, account, trash, 6);
    let kept = seed_message(&svc, account, inbox, 2);

    let emptied = call(&svc, "empty_mailbox", json!({ "mailbox": trash })).await;
    assert_eq!(emptied, json!(2));

    let in_trash = call(&svc, "list_threads", json!({ "mailbox": trash })).await;
    assert!(listed(&in_trash).is_empty(), "emptied at once, ahead of the server");
    let in_inbox = call(&svc, "list_threads", json!({ "mailbox": inbox })).await;
    let mut expected = vec![split.to_string(), kept.to_string()];
    expected.sort();
    assert_eq!(listed(&in_inbox), expected, "what is in the Inbox stays");
    assert!(!listed(&in_inbox).contains(&trashed.to_string()));

    let mut session = FakeSession {
        capabilities: Capabilities { uidplus: true, ..Default::default() },
        ..Default::default()
    };
    let sender = FakeSender::default();
    let report = drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert_eq!((report.attempted, report.done, report.failed), (2, 2, 0), "{report:?}");
    assert_eq!(
        session.calls,
        ["select Trash", "delete 5:6 on Some(\"Trash\")"],
        "one SELECT and one delete for the lot, and the Inbox copy never touched"
    );

    // Gone from the vault as soon as the server agrees, not at the next sync.
    let vault = svc.get().unwrap();
    assert!(vault.mail_message_locations(trashed_part).unwrap().is_empty());
    assert_eq!(vault.mail_message_locations(reply).unwrap(), vec![(inbox, 1)]);
}

#[tokio::test]
async fn only_trash_and_spam_can_be_emptied_or_deleted_from_forever() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    let trash = seed_mailbox(&svc, account, "Trash", MailboxRole::Trash);
    let thread = seed_message(&svc, account, inbox, 1);
    let in_trash = seed_message(&svc, account, trash, 2);

    let err = fails(&svc, "empty_mailbox", json!({ "mailbox": inbox })).await;
    assert_eq!(err.code, "invalid");
    // Refused whole, before anything is written, though Trash was named too.
    let err = fails(
        &svc,
        "delete_forever",
        json!({ "threads": [thread, in_trash], "from": [trash, inbox] }),
    )
    .await;
    assert_eq!(err.code, "invalid");

    let still = call(&svc, "list_threads", json!({ "mailbox": inbox })).await;
    assert_eq!(listed(&still), vec![thread.to_string()]);
    let still = call(&svc, "list_threads", json!({ "mailbox": trash })).await;
    assert_eq!(listed(&still), vec![in_trash.to_string()]);
    assert!(svc.get().unwrap().due_ops(account, Timestamp::now(), 10).unwrap().is_empty());
}

#[tokio::test]
async fn deleting_forever_covers_every_mailbox_named_and_a_failure_brings_it_back() {
    let (svc, _dir) = service();
    let first = seed_account(&svc);
    let first_inbox = seed_mailbox(&svc, first, "INBOX", MailboxRole::Inbox);
    let first_trash = seed_mailbox(&svc, first, "Trash", MailboxRole::Trash);
    // A second mailbox taken for Trash, the way a server without SPECIAL-USE
    // can have both "Trash" and "Deleted Messages".
    let also_trash = seed_mailbox(&svc, first, "Deleted Messages", MailboxRole::Trash);
    let second = seed_account(&svc);
    let second_trash = seed_mailbox(&svc, second, "Deleted Items", MailboxRole::Trash);
    let a = seed_message(&svc, first, first_trash, 3);
    let b = seed_message(&svc, second, second_trash, 4);
    let c = seed_message(&svc, first, also_trash, 5);
    // Nothing of this one is in Trash: no op, and no marker left to hide it
    // there once something of it is.
    let inbox_only = seed_message(&svc, first, first_inbox, 1);
    // Not a thread at all any more: skipped, not a failure.
    let missing = ThreadId::new();

    let ops = call(
        &svc,
        "delete_forever",
        json!({
            "threads": [a, b, c, inbox_only, missing],
            "from": [first_trash, also_trash, second_trash],
        }),
    )
    .await;
    let ops: Vec<Op> = serde_json::from_value(ops).unwrap();
    let from: Vec<(ThreadId, MailboxId)> = ops
        .iter()
        .map(|op| match (&op.target, &op.kind) {
            (OpTarget::Thread(t), OpKind::DeleteForever { from, messages }) => {
                assert_eq!(messages.len(), 1, "{op:?}");
                (*t, *from)
            }
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(from, vec![(a, first_trash), (c, also_trash), (b, second_trash)]);

    let gone = call(&svc, "list_threads", json!({ "mailbox": first_trash })).await;
    assert!(listed(&gone).is_empty());

    // A refusal from the server is permanent: the thread comes back.
    let mut session = FakeSession {
        fail_next: Some(MailError::Server("NO [CANNOT] not allowed".into())),
        capabilities: Capabilities { uidplus: true, ..Default::default() },
        ..FakeSession::default()
    };
    let sender = FakeSender::default();
    let report = drain_outbox(&svc, first, &mut session, &sender).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let back = call(&svc, "list_threads", json!({ "mailbox": first_trash })).await;
    assert_eq!(listed(&back), vec![a.to_string()]);
}

/// A reply filed in Trash after the person said yes is not theirs to lose:
/// the op names the messages that were there, and runs on those alone.
#[tokio::test]
async fn deleting_forever_leaves_what_arrived_after_it_was_asked() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let trash = seed_mailbox(&svc, account, "Trash", MailboxRole::Trash);
    let thread = ThreadId::new();
    seed_in_thread(&svc, account, thread, trash, 7);

    call(&svc, "delete_forever", json!({ "threads": [thread], "from": [trash] })).await;
    let later = seed_in_thread(&svc, account, thread, trash, 8);

    let mut session = FakeSession {
        capabilities: Capabilities { uidplus: true, ..Default::default() },
        ..Default::default()
    };
    let sender = FakeSender::default();
    let report = drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert_eq!(report.done, 1, "{report:?}");
    assert_eq!(session.calls, ["select Trash", "delete 7 on Some(\"Trash\")"]);
    assert_eq!(svc.get().unwrap().mail_message_locations(later).unwrap(), vec![(trash, 8)]);
}

/// Undoing a failed delete forever puts the thread back in Trash and
/// nowhere else: an archive of its Inbox half, still waiting, stays hidden.
#[tokio::test]
async fn a_failed_delete_forever_undoes_only_its_own_hiding() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    let trash = seed_mailbox(&svc, account, "Trash", MailboxRole::Trash);
    let split = ThreadId::new();
    seed_in_thread(&svc, account, split, trash, 5);
    seed_in_thread(&svc, account, split, inbox, 1);

    let vault = svc.get().unwrap();
    vault.apply_thread_ops(&[split], OpKind::Archive, Origin::Person).unwrap();
    let ops = vault.delete_forever(&[split], &[trash], Origin::Person).unwrap();
    vault.revert_thread_op(split, &ops[0].kind).unwrap();

    let in_trash = call(&svc, "list_threads", json!({ "mailbox": trash })).await;
    assert_eq!(listed(&in_trash), vec![split.to_string()], "back in Trash");
    let in_inbox = call(&svc, "list_threads", json!({ "mailbox": inbox })).await;
    assert!(listed(&in_inbox).is_empty(), "the archive still waiting keeps it out of the Inbox");
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
    let op_id: karmayoga_core::id::OpId =
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

/// Finding 4's other half: `save_draft` -- what the compose window's own
/// save goes through, `Origin::Person` always -- clears
/// `Draft::recipients_changed_by`, even when this particular save never
/// touches `to`/`cc`/`bcc` at all. The flag exists to say "look again
/// before you send"; once the person has looked (by being in compose,
/// saving anything), it has done its job.
#[tokio::test]
async fn a_save_clears_the_recipients_changed_flag_only_when_it_changes_the_recipients() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let mut draft = call(&svc, "new_draft", json!({ "account": account })).await;
    draft["to"] = json!([{ "name": "", "email": "bob@example.com" }]);
    draft["subject"] = json!("Hello");
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;
    let draft_id = draft["id"].as_str().unwrap().to_string();

    // Simulate what an assistant's or MCP's own `update_draft` would have
    // left behind: the sealed flag set, naming who.
    let vault = svc.get().unwrap();
    let mut stored = vault.draft(draft_id.parse().unwrap()).unwrap();
    stored.recipients_changed_by = Some(Origin::Mcp { client: "claude".into() });
    vault.save_draft(&stored).unwrap();
    assert!(
        vault.draft(draft_id.parse().unwrap()).unwrap().recipients_changed_by.is_some(),
        "the fixture set it"
    );

    // The person saves from compose, through the `save_draft` command --
    // touching only the subject, never the recipients.
    let mut person_edit = serde_json::to_value(&stored).unwrap();
    person_edit["subject"] = json!("Hello, edited");
    call(&svc, "save_draft", json!({ "draft": person_edit })).await;

    // The mark survives, because this save did not touch the recipients.
    // It has to: compose autosaves on a timer while a person types, and
    // flushes once more immediately before sending, so a mark cleared by
    // any save at all is a mark that has always been cleared by the time
    // it would have been worth reading. What it exists to say -- somebody
    // other than you put an address on this message -- is true until the
    // person themselves changes the addresses.
    assert!(
        vault.draft(draft_id.parse().unwrap()).unwrap().recipients_changed_by.is_some(),
        "a save that leaves the recipients alone must not clear the mark naming who changed them"
    );

    // Changing the recipients is what clears it: the person has now seen
    // and overridden whatever was put there.
    let mut person_changes_recipients = serde_json::to_value(&stored).unwrap();
    person_changes_recipients["to"] = json!([{ "name": "", "email": "carol@example.com" }]);
    call(&svc, "save_draft", json!({ "draft": person_changes_recipients })).await;

    assert!(
        vault.draft(draft_id.parse().unwrap()).unwrap().recipients_changed_by.is_none(),
        "a person's own change to the recipients clears the mark"
    );
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

// ---- throwing a draft away with its autosave still on the way --------------

/// A draft with words in it and somebody to send it to, saved once: exactly
/// one `AppendDraft` waiting in the outbox, and a Drafts mailbox for it to go.
async fn draft_with_an_append_waiting(svc: &Arc<Service>, account: AccountId) -> Value {
    seed_mailbox(svc, account, "Drafts", MailboxRole::Drafts);
    let mut draft = call(svc, "new_draft", json!({ "account": account })).await;
    draft["to"] = json!([{ "name": "", "email": "someone@example.com" }]);
    draft["bodyHtml"] = json!("<p>Words I thought better of</p>");
    call(svc, "save_draft", json!({ "draft": draft.clone() })).await;
    draft
}

/// What the bin in an empty reply comes to. `new_draft` stores nothing, so this
/// used to answer "Draft <id> not found" until an autosave had made a row.
#[tokio::test]
async fn a_draft_nobody_saved_is_discarded_without_complaint_and_can_come_back() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let draft = call(&svc, "new_draft", json!({ "account": account })).await;

    call(&svc, "discard_draft", json!({ "id": draft["id"] })).await;
    assert!(
        call(&svc, "list_drafts", json!({ "account": account }))
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );

    // Undo: there was nothing to bring back, and the save makes it.
    call(&svc, "restore_draft", json!({ "id": draft["id"] })).await;
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;
    let saved = call(&svc, "list_drafts", json!({ "account": account })).await;
    assert_eq!(saved.as_array().unwrap().len(), 1);
    assert_eq!(saved[0]["id"], draft["id"]);
    assert_eq!(saved[0]["state"]["type"], "editing");
}

/// A draft with no row has nothing to discard. A draft whose queued send has
/// gone missing is damage, and saying it was discarded would leave it queued.
#[tokio::test]
async fn a_queued_draft_with_its_op_missing_is_not_reported_as_discarded() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let draft = call(&svc, "new_draft", json!({ "account": account })).await;
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;
    let id: DraftId = serde_json::from_value(draft["id"].clone()).unwrap();
    let vault = svc.get().unwrap();
    vault
        .with_draft(id, |draft| {
            draft.state = DraftState::Queued { op: karmayoga_core::id::OpId::new() };
            true
        })
        .unwrap();

    let refused = fails(&svc, "discard_draft", json!({ "id": id })).await;

    assert_eq!(refused.code, "not_found");
    assert!(matches!(vault.draft(id).unwrap().state, DraftState::Queued { .. }), "left as it was");
}

#[tokio::test]
async fn a_draft_discarded_before_its_append_comes_due_never_reaches_the_server() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let draft = draft_with_an_append_waiting(&svc, account).await;

    call(&svc, "discard_draft", json!({ "id": draft["id"] })).await;

    let mut session = FakeSession::default();
    drain_outbox(&svc, account, &mut session, &FakeSender::default()).await.unwrap();
    assert!(!session.calls.iter().any(|c| c.contains("append")), "{:?}", session.calls);
}

#[tokio::test]
async fn a_draft_discarded_while_its_append_is_on_the_way_is_deleted_again() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let draft = draft_with_an_append_waiting(&svc, account).await;
    let id: DraftId = draft["id"].as_str().unwrap().parse().unwrap();

    // The moment `Vault::discard_draft` sees no server copy to delete, because
    // the upload making one has not finished.
    let vault = svc.get().unwrap();
    let mut session = FakeSession {
        during_append: Some(Box::new(move || {
            vault.discard_draft(id).unwrap();
        })),
        ..FakeSession::default()
    };
    let sender = FakeSender::default();
    drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert!(session.calls.iter().any(|c| c.contains("append to Drafts")), "{:?}", session.calls);

    // What the upload left behind is followed by its removal, or the words
    // would stay in the server's Drafts folder with nothing left to delete them.
    drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert!(session.calls.iter().any(|c| c.starts_with("delete")), "{:?}", session.calls);
}

#[tokio::test]
async fn a_restored_draft_is_the_same_record_and_can_be_sent() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let draft = draft_with_an_append_waiting(&svc, account).await;
    let id: DraftId = draft["id"].as_str().unwrap().parse().unwrap();
    let vault = svc.get().unwrap();
    vault
        .with_draft(id, |d| {
            d.recipients_changed_by = Some(Origin::Mcp { client: "claude".into() });
            true
        })
        .unwrap();

    call(&svc, "discard_draft", json!({ "id": id })).await;
    assert!(matches!(vault.draft(id).unwrap().state, DraftState::Discarded));
    call(&svc, "restore_draft", json!({ "id": id })).await;

    let back = vault.draft(id).unwrap();
    assert!(matches!(back.state, DraftState::Editing));
    assert!(
        back.recipients_changed_by.is_some(),
        "being binned and brought back is nobody having looked at the recipients"
    );

    // Saved again as it was: still nobody having changed them.
    call(&svc, "save_draft", json!({ "draft": serde_json::to_value(&back).unwrap() })).await;
    assert!(vault.draft(id).unwrap().recipients_changed_by.is_some());

    // Restoring what is not discarded is no change at all.
    let before = vault.draft(id).unwrap().updated_at;
    call(&svc, "restore_draft", json!({ "id": id })).await;
    assert_eq!(vault.draft(id).unwrap().updated_at, before);

    call(&svc, "send_draft", json!({ "id": id, "delaySeconds": 5 })).await;
    assert!(matches!(vault.draft(id).unwrap().state, DraftState::Queued { .. }));
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
    let op_id: karmayoga_core::id::OpId =
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
    karmayoga_service::outbox::recover_inflight_ops(&svc, account, &mut session).await.unwrap();

    let recovered = vault.op(op_id).unwrap();
    assert_eq!(recovered.state, OpState::Pending, "a stranded op must go back to work");
    assert!(recovered.attempts > attempts_before, "recovery counts as a failed attempt");

    // Recovery did not just flip a state bit: the op is genuinely runnable
    // again.
    let sender = FakeSender::default();
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
        .find(|c| matches!(c.kind, karmayoga_service::events::Kind::Thread))
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
    // this test does not need to prove twice given `karmayoga-mail`'s own
    // executor tests already cover it.
    let due =
        vault.due_ops(account, Timestamp::now() + SignedDuration::from_secs(3_600), 10).unwrap();
    assert_eq!(due.len(), 1);
}

// ---- Gmail label rows are never selectable mailboxes -----------------------

/// The regression for "Gmail label rows are treated as selectable
/// mailboxes": a thread whose message lives in All Mail and is also filed
/// under the `\Inbox` label and a user label ("Work") must, on archive and
/// mark-read, only ever touch All Mail. Neither label mailbox is ever
/// `SELECT`ed, and neither is stored to as if it named a real mailbox.
#[tokio::test]
async fn gmail_archive_and_mark_read_never_touch_a_label_mailbox() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let all_mail = seed_mailbox(&svc, account, "[Gmail]/All Mail", MailboxRole::All);
    let inbox_label = seed_mailbox(&svc, account, "\\Inbox", MailboxRole::Inbox);
    let work_label = seed_mailbox(&svc, account, "Work", MailboxRole::Other);

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
        subject: "A labelled message".into(),
        snippet: String::new(),
        flags: MessageFlags::default(),
        labels: vec!["Work".into()],
        has_attachments: false,
        size: 128,
        category: None,
        category_source: CategorySource::Rules,
        pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
        invite: None,
    };
    // One physical message, filed under three mailbox rows at once -- All
    // Mail, where it lives, and the two label memberships a real Gmail sync
    // would also record. The label rows' own `uid`s are deliberately
    // different from All Mail's so a test that wrongly acted on one fails
    // loudly rather than by coincidence agreeing.
    vault
        .ingest_mail(
            account,
            vec![
                IngestMessage { message: message.clone(), mailbox: all_mail, uid: 42 },
                IngestMessage { message: message.clone(), mailbox: inbox_label, uid: 1 },
                IngestMessage { message, mailbox: work_label, uid: 1 },
            ],
        )
        .unwrap();

    vault.apply_thread_ops(&[thread_id], OpKind::MarkRead, Origin::Person).unwrap();
    vault.apply_thread_ops(&[thread_id], OpKind::Archive, Origin::Person).unwrap();

    let mut session = FakeSession {
        capabilities: Capabilities { gmail: true, ..Default::default() },
        ..Default::default()
    };
    let sender = FakeSender::default();
    let report = drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert_eq!(report.attempted, 2, "{report:?}");
    assert_eq!(report.done, 2, "{report:?}");
    assert_eq!(report.failed, 0, "{report:?}");

    assert!(
        session.calls.iter().any(|c| c.contains("select [Gmail]/All Mail")),
        "All Mail must be selected: {:?}",
        session.calls
    );
    assert!(
        !session.calls.iter().any(|c| c.contains("select \\Inbox") || c.contains("select Work")),
        "a label mailbox must never be selected: {:?}",
        session.calls
    );
}

// ---- backoff: the first retry waits thirty seconds, not sixty --------------

/// The regression for the backoff off-by-one: `attempts` must be read by
/// [`karmayoga_core::mail::backoff_for_attempt`] *before* it is incremented,
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
    let op_id: karmayoga_core::id::OpId =
        ops.as_array().unwrap()[0]["id"].as_str().unwrap().parse().unwrap();

    let mut session = FakeSession {
        fail_next: Some(MailError::Network("connection reset".into())),
        ..Default::default()
    };
    let sender = FakeSender::default();
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
    let draft_id: karmayoga_core::id::DraftId = draft["id"].as_str().unwrap().parse().unwrap();
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;

    let mut session = FakeSession::default();
    let sender = FakeSender::default();
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
    let draft_id: karmayoga_core::id::DraftId = draft["id"].as_str().unwrap().parse().unwrap();

    let mut session = FakeSession::default();
    let sender = FakeSender::default();
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
    let draft_id: karmayoga_core::id::DraftId = draft["id"].as_str().unwrap().parse().unwrap();
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;

    let mut session = FakeSession::default();
    let sender = FakeSender::default();
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

fn vault_draft(svc: &Arc<Service>, id: karmayoga_core::id::DraftId) -> karmayoga_core::mail::Draft {
    svc.get().unwrap().draft(id).unwrap()
}

// ---- Finding 1: discarding a draft cancels its own pending send -----------

/// The regression for "discarding a draft does not cancel its queued
/// send": once a draft has been queued to send, discarding it must cancel
/// that op outright, so the message the person just discarded is never
/// actually mailed.
#[tokio::test]
async fn discarding_a_queued_draft_cancels_its_pending_send() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let mut draft = call(&svc, "new_draft", json!({ "account": account })).await;
    draft["to"] = json!([{ "name": "", "email": "bob@example.com" }]);
    draft["subject"] = json!("Hello");
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;
    let draft_id: karmayoga_core::id::DraftId = draft["id"].as_str().unwrap().parse().unwrap();

    let sent = call(&svc, "send_draft", json!({ "id": draft_id })).await;
    let op_id: karmayoga_core::id::OpId = sent["state"]["op"].as_str().unwrap().parse().unwrap();

    call(&svc, "discard_draft", json!({ "id": draft_id })).await;

    let vault = svc.get().unwrap();
    let op = vault.op(op_id).unwrap();
    assert_eq!(op.state, OpState::Cancelled, "discarding a queued draft must cancel its send");

    // Draining the outbox afterwards must never actually send it -- a
    // cancelled op is simply not due any more.
    let mut session = FakeSession::default();
    let sender = FakeSender::default();
    drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert_eq!(*sender.sent.lock().unwrap(), 0, "a discarded draft's send must never go out");
}

/// The other half: [`karmayoga_mail::outbox::send`]'s own defensive check.
/// A `Send` op already `InFlight` at the moment of discard is not
/// cancelled by [`karmayoga_core::Vault::discard_draft`] (the account task
/// may already be mid-send) -- this proves the executor itself refuses to
/// mail a draft that has since been marked `Discarded`, the last line of
/// defence for that race. Exercised here by racing the vault directly,
/// since reproducing the real timing would need a session that pauses
/// mid-`execute`.
#[tokio::test]
async fn a_discarded_draft_is_never_sent_even_if_its_op_survives() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    // A Drafts mailbox, so the ordinary `AppendDraft` op `save_draft` also
    // queues succeeds cleanly -- this test's own business is the `Send`
    // op's own refusal, not an unrelated failure from having nowhere to
    // append to.
    seed_mailbox(&svc, account, "Drafts", MailboxRole::Drafts);
    let mut draft = call(&svc, "new_draft", json!({ "account": account })).await;
    draft["to"] = json!([{ "name": "", "email": "bob@example.com" }]);
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;
    let draft_id: karmayoga_core::id::DraftId = draft["id"].as_str().unwrap().parse().unwrap();

    let sent = call(&svc, "send_draft", json!({ "id": draft_id })).await;
    let op_id: karmayoga_core::id::OpId = sent["state"]["op"].as_str().unwrap().parse().unwrap();

    // Simulate the race: the draft is discarded, but its `Send` op is left
    // exactly as `send_draft` queued it -- standing in for an op that was
    // `InFlight` (and so left alone by `discard_draft`'s own cancellation)
    // at the moment of discard. Backdated past the undo-send window it
    // would otherwise still be sitting inside, the same way other tests in
    // this file age an op to make it due without a real sleep.
    let vault = svc.get().unwrap();
    let mut op = vault.op(op_id).unwrap();
    op.not_before = Timestamp::now() - SignedDuration::from_secs(1);
    vault.update_op(&op).unwrap();
    let mut stored = vault.draft(draft_id).unwrap();
    stored.state = DraftState::Discarded;
    vault.save_draft(&stored).unwrap();

    let mut session = FakeSession::default();
    let sender = FakeSender::default();
    let report = drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert_eq!(report.failed, 1, "the executor must refuse a Discarded draft: {report:?}");
    assert_eq!(*sender.sent.lock().unwrap(), 0, "it must never actually be sent");
    let op = vault.op(op_id).unwrap();
    assert!(matches!(op.state, OpState::Failed { permanent: true, .. }));
}

// ---- Finding 2: undo send closes for good after an attempt -----------------

/// The regression for "the undo-send window reopens after every retryable
/// failure": once an attempt has actually been made (`attempts > 0`), undo
/// must refuse even though the op is back to `Pending` with `now` still
/// short of its new `not_before` -- exactly the shape a retryable failure
/// leaves behind.
#[tokio::test]
async fn undo_send_refuses_once_the_send_has_been_attempted() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let mut draft = call(&svc, "new_draft", json!({ "account": account })).await;
    draft["to"] = json!([{ "name": "", "email": "bob@example.com" }]);
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;
    let draft_id: karmayoga_core::id::DraftId = draft["id"].as_str().unwrap().parse().unwrap();

    let sent = call(&svc, "send_draft", json!({ "id": draft_id })).await;
    let op_id: karmayoga_core::id::OpId = sent["state"]["op"].as_str().unwrap().parse().unwrap();

    let vault = svc.get().unwrap();
    let mut op = vault.op(op_id).unwrap();
    op.attempts = 1;
    op.not_before = Timestamp::now() + SignedDuration::from_secs(30);
    vault.update_op(&op).unwrap();
    // `send`'s own stable-id write, the other half of "has been attempted".
    let mut stored = vault.draft(draft_id).unwrap();
    stored.message_id = Some("already-tried@example.com".into());
    vault.save_draft(&stored).unwrap();

    let err = fails(&svc, "undo_send", json!({ "draftId": draft_id })).await;
    assert_eq!(err.code, "invalid");
    assert!(
        err.message.contains("already been attempted"),
        "must say plainly that an attempt was made, not merely that the window passed: {}",
        err.message
    );
    assert_eq!(
        vault.draft(draft_id).unwrap().state.as_str(),
        "queued",
        "a refused undo must leave the send exactly as queued"
    );
}

/// `undo_send`'s refusal must say something different for a send that was
/// already cancelled -- distinct from "the window has passed", which
/// wrongly implies the message might have gone out.
#[tokio::test]
async fn undo_send_on_an_already_cancelled_op_says_so_distinctly() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let mut draft = call(&svc, "new_draft", json!({ "account": account })).await;
    draft["to"] = json!([{ "name": "", "email": "bob@example.com" }]);
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;
    let draft_id: karmayoga_core::id::DraftId = draft["id"].as_str().unwrap().parse().unwrap();

    let sent = call(&svc, "send_draft", json!({ "id": draft_id })).await;
    let op_id: karmayoga_core::id::OpId = sent["state"]["op"].as_str().unwrap().parse().unwrap();
    let vault = svc.get().unwrap();
    let mut op = vault.op(op_id).unwrap();
    op.transition_to(OpState::Cancelled).unwrap();
    vault.update_op(&op).unwrap();

    let err = fails(&svc, "undo_send", json!({ "draftId": draft_id })).await;
    assert!(err.message.contains("already been cancelled"), "{}", err.message);
}

/// ...and a different message again for a send that already failed for
/// good: there is nothing to undo, which is not the same thing as "too
/// late".
#[tokio::test]
async fn undo_send_on_a_permanently_failed_op_says_so_distinctly() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let mut draft = call(&svc, "new_draft", json!({ "account": account })).await;
    draft["to"] = json!([{ "name": "", "email": "bob@example.com" }]);
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;
    let draft_id: karmayoga_core::id::DraftId = draft["id"].as_str().unwrap().parse().unwrap();

    let sent = call(&svc, "send_draft", json!({ "id": draft_id })).await;
    let op_id: karmayoga_core::id::OpId = sent["state"]["op"].as_str().unwrap().parse().unwrap();
    let vault = svc.get().unwrap();
    let mut op = vault.op(op_id).unwrap();
    op.transition_to(OpState::InFlight).unwrap();
    op.transition_to(OpState::Failed { permanent: true, message: "rejected".into() }).unwrap();
    vault.update_op(&op).unwrap();

    let err = fails(&svc, "undo_send", json!({ "draftId": draft_id })).await;
    assert!(err.message.contains("there is nothing to undo"), "{}", err.message);
}

// ---- Finding 3: a widened already-sent search catches a crashed send -----

/// The regression for "the already-sent check can never succeed on a
/// non-Gmail account": a message a recovered `Send` op's own draft names
/// must be found even when the server filed it somewhere other than Sent
/// -- here, Inbox, with no Sent mailbox registered for the account at
/// all -- or crash recovery resends a message that already went out.
#[tokio::test]
async fn recovering_a_crashed_send_finds_it_via_a_widened_search() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);

    let mut draft = call(&svc, "new_draft", json!({ "account": account })).await;
    draft["to"] = json!([{ "name": "", "email": "bob@example.com" }]);
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;
    let draft_id: karmayoga_core::id::DraftId = draft["id"].as_str().unwrap().parse().unwrap();

    let vault = svc.get().unwrap();
    let mut stored = vault.draft(draft_id).unwrap();
    stored.message_id = Some("crashed@example.com".into());
    vault.save_draft(&stored).unwrap();
    let op = Op::new(account, OpKind::Send, OpTarget::Draft(draft_id), Origin::Person);
    vault.enqueue_op(&op).unwrap();
    let mut inflight = op.clone();
    inflight.transition_to(OpState::InFlight).unwrap();
    vault.update_op(&inflight).unwrap();

    let mut session = FakeSession::default();
    session.found_message_ids.insert("crashed@example.com".into(), 5);
    karmayoga_service::outbox::recover_inflight_ops(&svc, account, &mut session).await.unwrap();

    let recovered = vault.op(op.id).unwrap();
    assert_eq!(
        recovered.state,
        OpState::Done,
        "found via the widened search, so recovery must not resend it: {recovered:?}"
    );
    assert_eq!(vault.draft(draft_id).unwrap().state, DraftState::Sent);
}

// ---- Finding 5: ops for the same thread drain in creation order -----------

/// The regression for "ops for the same target can execute out of order
/// after a retry": a `Label` pushed into the future by a simulated
/// transient failure must hold back a later-queued `Unlabel` for the same
/// thread, even though `Unlabel`'s own `not_before` is already due --
/// otherwise the unlabel drains first and undoes a label the server was
/// never even told to add. Once `Label`'s own backoff has elapsed, both
/// become due together and drain in the order they were created.
#[tokio::test]
async fn due_ops_preserves_creation_order_within_a_thread_across_calls() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let all_mail = seed_mailbox(&svc, account, "[Gmail]/All Mail", MailboxRole::All);
    let thread = seed_message(&svc, account, all_mail, 1);
    let vault = svc.get().unwrap();

    let label_ops = vault
        .apply_thread_ops(&[thread], OpKind::Label { label: "Work".into() }, Origin::Person)
        .unwrap();
    let mut label_op = label_ops[0].clone();
    // Simulate a transient failure's own backoff -- the exact shape
    // `drain_outbox`'s retry arm leaves behind.
    label_op.not_before = Timestamp::now() + SignedDuration::from_secs(30);
    vault.update_op(&label_op).unwrap();

    let unlabel_ops = vault
        .apply_thread_ops(&[thread], OpKind::Unlabel { label: "Work".into() }, Origin::Person)
        .unwrap();
    let unlabel_op = unlabel_ops[0].clone();

    let due_early = vault.due_ops(account, Timestamp::now(), 10).unwrap();
    assert!(
        due_early.is_empty(),
        "the later-queued Unlabel must wait for Label even though it is itself due: {due_early:?}"
    );

    let due_later =
        vault.due_ops(account, Timestamp::now() + SignedDuration::from_secs(31), 10).unwrap();
    assert_eq!(due_later.len(), 2, "{due_later:?}");
    assert_eq!(due_later[0].id, label_op.id, "Label must drain first, in creation order");
    assert_eq!(due_later[1].id, unlabel_op.id);
}

/// One stuck target must never block an unrelated one: a second thread's
/// own op, queued after the first thread's stuck op, is unaffected.
#[tokio::test]
async fn due_ops_holdback_is_scoped_to_one_thread() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let all_mail = seed_mailbox(&svc, account, "[Gmail]/All Mail", MailboxRole::All);
    let stuck_thread = seed_message(&svc, account, all_mail, 1);
    let other_thread = seed_message(&svc, account, all_mail, 2);
    let vault = svc.get().unwrap();

    let stuck_ops = vault
        .apply_thread_ops(&[stuck_thread], OpKind::Label { label: "Work".into() }, Origin::Person)
        .unwrap();
    let mut stuck_op = stuck_ops[0].clone();
    stuck_op.not_before = Timestamp::now() + SignedDuration::from_secs(30);
    vault.update_op(&stuck_op).unwrap();
    vault
        .apply_thread_ops(&[stuck_thread], OpKind::Unlabel { label: "Work".into() }, Origin::Person)
        .unwrap();

    let other_ops =
        vault.apply_thread_ops(&[other_thread], OpKind::MarkRead, Origin::Person).unwrap();

    let due = vault.due_ops(account, Timestamp::now(), 10).unwrap();
    assert_eq!(due.len(), 1, "{due:?}");
    assert_eq!(due[0].id, other_ops[0].id, "the unrelated thread's op must not be held back");
}

// ---- Finding 4: field-scoped draft writes never lose a concurrent one -----

/// The regression for "a draft read-modify-write outside a transaction
/// loses the user's typing or the minted Message-ID":
/// [`karmayoga_core::Vault::with_draft`] reads, mutates and writes under one
/// lock, so two callers racing to set different fields on the same draft
/// both survive, whichever actually runs first.
#[tokio::test]
async fn with_draft_serializes_two_concurrent_field_scoped_writes() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let draft = call(&svc, "new_draft", json!({ "account": account })).await;
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;
    let draft_id: karmayoga_core::id::DraftId = draft["id"].as_str().unwrap().parse().unwrap();
    let vault = svc.get().unwrap();

    let barrier = Arc::new(std::sync::Barrier::new(2));

    let vault_a = vault.clone();
    let barrier_a = barrier.clone();
    let a = std::thread::spawn(move || {
        barrier_a.wait();
        vault_a
            .with_draft(draft_id, |d| {
                d.message_id = Some("minted@example.com".into());
                true
            })
            .unwrap();
    });

    let vault_b = vault.clone();
    let barrier_b = barrier.clone();
    let b = std::thread::spawn(move || {
        barrier_b.wait();
        vault_b
            .with_draft(draft_id, |d| {
                d.subject = "Edited concurrently".into();
                true
            })
            .unwrap();
    });

    a.join().unwrap();
    b.join().unwrap();

    let after = vault.draft(draft_id).unwrap();
    assert_eq!(
        after.message_id.as_deref(),
        Some("minted@example.com"),
        "one concurrent field-scoped write must not be lost, whichever ran first"
    );
    assert_eq!(after.subject, "Edited concurrently", "nor must the other");
}

// ---- Finding 7: a rejected invite reply reverts its own RSVP -------------

/// The regression for "a permanently failed invite reply still shows as
/// answered": once the `Send` op behind an RSVP fails for good, the
/// invitation's own `my_response` must revert -- the organiser was never
/// actually told "Accepted".
#[tokio::test]
async fn a_permanently_failed_invite_reply_reverts_my_response() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    let thread = seed_message(&svc, account, inbox, 1);

    let vault = svc.get().unwrap();
    let (_, messages) = vault.thread(thread).unwrap();
    let message_id = messages[0].id;
    let invite = Invite {
        uid: "event-1".into(),
        method: InviteMethod::Request,
        summary: "Standup".into(),
        start: Timestamp::now(),
        end: Timestamp::now(),
        all_day: false,
        location: None,
        organizer: Address::bare("boss@example.com"),
        attendees: Vec::new(),
        my_response: Some(AttendeeResponse::Accepted),
        recurrence: None,
    };
    vault.set_message_invite(message_id, Some(invite)).unwrap();

    // The RSVP draft `respond_to_invite_inner` builds -- named at the
    // invitation it answers via `in_reply_to`, the same field an ordinary
    // reply threads under and the field this plumbing reads to find it
    // again. `calendar_part` is what actually marks this as an RSVP rather
    // than an ordinary reply -- see
    // `a_permanently_failed_ordinary_reply_does_not_touch_my_response`
    // below for the sibling case this distinguishes it from.
    let mut draft = Draft::new(account, "me@example.com", Origin::Person);
    draft.in_reply_to = Some(message_id);
    draft.to = vec![Address::bare("boss@example.com")];
    draft.subject = "Accepted: Standup".into();
    draft.calendar_part =
        Some(DraftCalendarPart { method: "REPLY".into(), ics: "BEGIN:VCALENDAR".into() });
    let draft_id = draft.id;
    vault.save_draft(&draft).unwrap();
    vault.queue_draft_send(draft_id, Timestamp::now(), Origin::Person).unwrap();

    let mut session = FakeSession::default();
    let sender = FakeSender {
        fail: Some(MailError::Server("550 5.1.1 no such user".into())),
        ..Default::default()
    };
    let report = drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");

    let after = vault.mail_message(message_id).unwrap();
    assert_eq!(
        after.invite.unwrap().my_response,
        None,
        "a rejected RSVP must not still say Accepted"
    );
    assert_eq!(vault.draft(draft_id).unwrap().state, DraftState::Editing);
}

/// The regression this bug report actually describes: a plain reply in an
/// invitation's own thread sets `in_reply_to` just like an RSVP does --
/// that field only names "the message this threads under", not "the
/// invitation this answers" -- but never sets `calendar_part`. If a
/// permanent failure of that ordinary reply reverted `my_response` on the
/// strength of `in_reply_to` alone, sending "sounds good, see you there" in
/// the same thread and having it bounce would silently un-answer an
/// invitation the organiser already has a real "Accepted" for, sent days
/// earlier and never actually withdrawn.
#[tokio::test]
async fn a_permanently_failed_ordinary_reply_does_not_touch_my_response() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    let thread = seed_message(&svc, account, inbox, 1);

    let vault = svc.get().unwrap();
    let (_, messages) = vault.thread(thread).unwrap();
    let message_id = messages[0].id;
    let invite = Invite {
        uid: "event-1".into(),
        method: InviteMethod::Request,
        summary: "Standup".into(),
        start: Timestamp::now(),
        end: Timestamp::now(),
        all_day: false,
        location: None,
        organizer: Address::bare("boss@example.com"),
        attendees: Vec::new(),
        my_response: Some(AttendeeResponse::Accepted),
        recurrence: None,
    };
    vault.set_message_invite(message_id, Some(invite)).unwrap();

    // An ordinary reply in the same thread -- no `calendar_part`, unlike
    // the RSVP draft above.
    let mut draft = Draft::new(account, "me@example.com", Origin::Person);
    draft.in_reply_to = Some(message_id);
    draft.to = vec![Address::bare("boss@example.com")];
    draft.subject = "Re: Standup".into();
    draft.body_html = "<p>Sounds good, see you there.</p>".into();
    let draft_id = draft.id;
    vault.save_draft(&draft).unwrap();
    vault.queue_draft_send(draft_id, Timestamp::now(), Origin::Person).unwrap();

    let mut session = FakeSession::default();
    let sender = FakeSender {
        fail: Some(MailError::Server("550 5.1.1 no such user".into())),
        ..Default::default()
    };
    let report = drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");

    let after = vault.mail_message(message_id).unwrap();
    assert_eq!(
        after.invite.unwrap().my_response,
        Some(AttendeeResponse::Accepted),
        "an unrelated reply failing must not un-answer an invitation the organiser already has"
    );
    assert_eq!(vault.draft(draft_id).unwrap().state, DraftState::Editing);
}

// ---- Finding 8: a crash between an op's own terminal state and its --------
// ---- draft's own write is reconciled at startup ---------------------------

/// The regression for "a crash between marking an op Done and finishing
/// its side effects strands the draft forever": [`recover_inflight_ops`]
/// must also reconcile a draft left `Queued` naming an op that has already
/// reached a terminal state -- `Done` (the send actually went out) and a
/// permanent `Failed` (it did not, and never will) each need a different
/// answer.
#[tokio::test]
async fn a_crash_between_an_ops_terminal_state_and_its_draft_write_is_reconciled() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let vault = svc.get().unwrap();

    let mut sent_draft = Draft::new(account, "me@example.com", Origin::Person);
    sent_draft.to = vec![Address::bare("bob@example.com")];
    let sent_draft_id = sent_draft.id;
    vault.save_draft(&sent_draft).unwrap();
    let (_, sent_op) =
        vault.queue_draft_send(sent_draft_id, Timestamp::now(), Origin::Person).unwrap();
    let mut done = sent_op.clone();
    done.transition_to(OpState::InFlight).unwrap();
    done.transition_to(OpState::Done).unwrap();
    vault.update_op(&done).unwrap();

    let mut failed_draft = Draft::new(account, "me@example.com", Origin::Person);
    failed_draft.to = vec![Address::bare("carol@example.com")];
    let failed_draft_id = failed_draft.id;
    vault.save_draft(&failed_draft).unwrap();
    let (_, failed_op) =
        vault.queue_draft_send(failed_draft_id, Timestamp::now(), Origin::Person).unwrap();
    let mut failed = failed_op.clone();
    failed.transition_to(OpState::InFlight).unwrap();
    failed.transition_to(OpState::Failed { permanent: true, message: "rejected".into() }).unwrap();
    vault.update_op(&failed).unwrap();

    let mut session = FakeSession::default();
    karmayoga_service::outbox::recover_inflight_ops(&svc, account, &mut session).await.unwrap();

    assert_eq!(
        vault.draft(sent_draft_id).unwrap().state,
        DraftState::Sent,
        "a Done op's own draft must be reconciled to Sent"
    );
    assert_eq!(
        vault.draft(failed_draft_id).unwrap().state,
        DraftState::Editing,
        "a permanently failed op's own draft must not be stuck Queued forever"
    );
}

// ---- drafts in a thread -------------------------------------------------------

/// A draft's copy on the server, in `thread`, filed in `mailbox` at `uid`. A
/// `body` makes it a synced one; `deleted` flags it `\Deleted`, as an old
/// autosave is.
fn draft_copy(
    svc: &Arc<Service>,
    account: AccountId,
    thread_id: ThreadId,
    mailbox: MailboxId,
    uid: u32,
    deleted: bool,
    body: Option<&str>,
) -> MailMessageId {
    let vault = svc.get().unwrap();
    let id = MailMessageId::new();
    let message = Message {
        id,
        account_id: account,
        thread_id,
        message_id_header: format!("<{id}@example.com>"),
        date: Timestamp::now(),
        from: Address::bare("me@example.com"),
        to: vec![Address::bare("ann@example.com")],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: "Re: plans".into(),
        snippet: "The first line".into(),
        flags: MessageFlags { draft: true, deleted, ..MessageFlags::default() },
        labels: Vec::new(),
        has_attachments: false,
        size: 128,
        category: None,
        category_source: CategorySource::Rules,
        pack: PackRef {
            account: account.to_string(),
            pack: PackId::new(),
            offset: 0,
            len: if body.is_some() { 64 } else { 0 },
        },
        gmail: None,
        invite: None,
    };
    vault.ingest_mail(account, vec![IngestMessage { message, mailbox, uid }]).unwrap();
    if let Some(text) = body {
        vault
            .save_body(&karmayoga_core::mail::Body {
                message_id: id,
                html_sanitised: String::new(),
                text: text.to_string(),
                quoted_ranges: Vec::new(),
                signature_range: None,
                parts: Vec::new(),
                remote_images: Vec::new(),
            })
            .unwrap();
    }
    id
}

/// Drafts are cards, not mail. Two drafts no draft of this app's claims -- a
/// phone's and the web's, say -- are two cards, never one, so deleting or
/// editing one never touches the other. A copy flagged `\Deleted` is shown in
/// neither.
#[tokio::test]
async fn drafts_nobody_here_claims_are_a_card_each() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    let drafts = seed_mailbox(&svc, account, "Drafts", MailboxRole::Drafts);
    let thread = seed_message(&svc, account, inbox, 1);
    let phone = draft_copy(&svc, account, thread, drafts, 1, false, None);
    let web = draft_copy(&svc, account, thread, drafts, 2, false, None);
    draft_copy(&svc, account, thread, drafts, 3, true, None);

    let detail = call(&svc, "get_thread", json!({ "id": thread.to_string() })).await;
    assert_eq!(detail["messages"].as_array().unwrap().len(), 1, "drafts are not mail");
    let cards = detail["drafts"].as_array().unwrap();
    assert_eq!(cards.len(), 2, "{detail}");
    let copies: Vec<Vec<String>> = cards
        .iter()
        .map(|c| c["copies"].as_array().unwrap().iter().map(|m| m["id"].to_string()).collect())
        .collect();
    assert!(copies.iter().all(|c| c.len() == 1), "{copies:?}");
    let all: Vec<String> = copies.concat();
    assert!(all.contains(&json!(phone.to_string()).to_string()));
    assert!(all.contains(&json!(web.to_string()).to_string()));
}

/// Carrying on with another device's draft takes its whole text, from the
/// plain text when it has no HTML, and refuses one whose body has not
/// synced. Only old autosaves flagged `\Deleted` are taken off the server
/// with it; anybody's draft that nobody flagged stays.
#[tokio::test]
async fn editing_another_devices_draft_keeps_every_word_and_every_other_draft() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    let drafts = seed_mailbox(&svc, account, "Drafts", MailboxRole::Drafts);
    let thread = seed_message(&svc, account, inbox, 1);

    let pending = draft_copy(&svc, account, thread, drafts, 1, false, None);
    let refused =
        fails(&svc, "edit_thread_draft", json!({ "copies": [pending.to_string()] })).await;
    assert_eq!(refused.code, "conflict", "{refused:?}");

    let long = "The first line\n\nand everything after it, which the snippet never had.";
    let other = draft_copy(&svc, account, thread, drafts, 2, false, None);
    let stale = draft_copy(&svc, account, thread, drafts, 3, true, Some("old"));
    let synced = draft_copy(&svc, account, thread, drafts, 4, false, Some(long));
    let draft = call(
        &svc,
        "edit_thread_draft",
        json!({ "copies": [other.to_string(), stale.to_string(), synced.to_string()] }),
    )
    .await;
    let html = draft["bodyHtml"].as_str().unwrap();
    assert!(html.contains("which the snippet never had"), "{html}");
    assert_eq!(draft["serverCopy"]["uid"], 4);

    let vault = svc.get().unwrap();
    let deleting: Vec<MailMessageId> = vault
        .ops_for_thread(thread, 10)
        .unwrap()
        .into_iter()
        .filter_map(|op| match op.kind {
            OpKind::DeleteForever { messages, .. } => Some(messages),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(deleting, vec![stale], "only the flagged old autosave goes");
}

// ---- a send that fails says why; sending from another account -------------

/// A draft with a recipient and a subject, queued to send and already due.
async fn queued_send(svc: &Arc<Service>, account: AccountId) -> DraftId {
    let mut draft = call(svc, "new_draft", json!({ "account": account })).await;
    draft["to"] = json!([{ "name": "", "email": "bob@example.com" }]);
    draft["subject"] = json!("Lunch on Friday");
    call(svc, "save_draft", json!({ "draft": draft.clone() })).await;
    let id: DraftId = draft["id"].as_str().unwrap().parse().unwrap();
    make_send_due(svc, id).await;
    id
}

async fn make_send_due(svc: &Arc<Service>, id: DraftId) {
    let sent = call(svc, "send_draft", json!({ "id": id })).await;
    let op_id: karmayoga_core::id::OpId = sent["state"]["op"].as_str().unwrap().parse().unwrap();
    let vault = svc.get().unwrap();
    let mut op = vault.op(op_id).unwrap();
    op.not_before = Timestamp::now() - SignedDuration::from_secs(1);
    vault.update_op(&op).unwrap();
}

/// What Hari saw: "Sending…", then the draft back, and no word of why.
#[tokio::test]
async fn a_send_that_fails_for_good_keeps_why_on_the_draft_and_says_so() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    seed_mailbox(&svc, account, "Drafts", MailboxRole::Drafts);
    let collector = Arc::new(Collector::default());
    svc.set_events(collector.clone());
    let id = queued_send(&svc, account).await;

    let sender = FakeSender {
        fail: Some(MailError::Server("550 5.7.1 Relaying denied".into())),
        ..FakeSender::default()
    };
    let mut session = FakeSession::default();
    let report = drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");

    let draft = vault_draft(&svc, id);
    assert_eq!(draft.state, DraftState::Editing);
    assert_eq!(draft.send_error.as_deref(), Some("550 5.7.1 Relaying denied"));
    {
        let notices = collector.notices.lock().unwrap();
        let notice = notices.iter().find(|n| n.title.starts_with("Not sent")).expect("a notice");
        assert_eq!(notice.title, "Not sent: Lunch on Friday");
        assert!(notice.body.as_deref().unwrap().contains("Relaying denied"), "{notice:?}");
    }

    // Asking again starts clean, and a send that works leaves nothing behind.
    make_send_due(&svc, id).await;
    assert_eq!(vault_draft(&svc, id).send_error, None);
    let report = drain_outbox(&svc, account, &mut session, &FakeSender::default()).await.unwrap();
    assert_eq!(report.done, 1, "{report:?}");
    let draft = vault_draft(&svc, id);
    assert_eq!(draft.state, DraftState::Sent);
    assert_eq!(draft.send_error, None);
}

#[tokio::test]
async fn a_send_being_retried_says_why_and_tells_once_it_has_gone_on_a_while() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let collector = Arc::new(Collector::default());
    svc.set_events(collector.clone());
    let id = queued_send(&svc, account).await;

    let sender = FakeSender {
        fail: Some(MailError::Network("connection timed out".into())),
        ..FakeSender::default()
    };
    let mut session = FakeSession::default();
    let vault = svc.get().unwrap();
    for attempt in 1..=6u32 {
        let report = drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
        assert_eq!(report.retried, 1, "attempt {attempt}: {report:?}");
        let draft = vault_draft(&svc, id);
        assert!(matches!(draft.state, DraftState::Queued { .. }), "still on its way");
        assert_eq!(draft.send_error.as_deref(), Some("network error: connection timed out"));
        let told = collector
            .notices
            .lock()
            .unwrap()
            .iter()
            .filter(|n| n.title == "Not sent yet: Lunch on Friday")
            .count();
        assert_eq!(told, usize::from(attempt >= 4), "attempt {attempt}");
        // Due again at once, rather than after the backoff.
        let DraftState::Queued { op } = draft.state else { unreachable!() };
        let mut op = vault.op(op).unwrap();
        op.not_before = Timestamp::now() - SignedDuration::from_secs(1);
        vault.update_op(&op).unwrap();
    }

    // Undo is refused (it may have gone out); stopping is not, and leaves the
    // id that lets a later send ask whether it did.
    let refused = fails(&svc, "undo_send", json!({ "draftId": id })).await;
    assert!(refused.message.contains("may have reached the server"), "{}", refused.message);
    let stopped = call(&svc, "stop_send", json!({ "draftId": id })).await;
    assert_eq!(stopped["state"]["type"], "editing");
    assert!(vault_draft(&svc, id).message_id.is_some());
    let report = drain_outbox(&svc, account, &mut session, &sender).await.unwrap();
    assert_eq!(report.attempted, 0, "nothing left to try: {report:?}");
}

#[tokio::test]
async fn a_send_not_yet_tried_cannot_be_stopped_only_undone() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let id = queued_send(&svc, account).await;
    let refused = fails(&svc, "stop_send", json!({ "draftId": id })).await;
    assert!(refused.message.contains("only a send that failed"), "{}", refused.message);
}

#[tokio::test]
async fn mail_goes_out_under_the_name_of_the_address_it_is_from() {
    let (svc, _dir) = service();
    let vault = svc.get().unwrap();
    let mut account = Account::new(Provider::Custom, "me@example.com");
    account.display_name = "Hari G".into();
    account.identities.push(karmayoga_core::account::Identity {
        name: "Hari at work".into(),
        address: "hari@work.example".into(),
        signature_html: String::new(),
    });
    let account_id = account.id;
    vault.save_account(&account).unwrap();

    let mut session = FakeSession::default();
    let sender = FakeSender::default();
    queued_send(&svc, account_id).await;
    drain_outbox(&svc, account_id, &mut session, &sender).await.unwrap();
    let raw = String::from_utf8_lossy(&sender.raw.lock().unwrap()).to_string();
    assert!(raw.contains("From: \"Hari G\" <me@example.com>"), "{raw}");

    let mut draft = call(&svc, "new_draft", json!({ "account": account_id })).await;
    draft["identity"] = json!("hari@work.example");
    draft["to"] = json!([{ "name": "", "email": "bob@example.com" }]);
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;
    make_send_due(&svc, draft["id"].as_str().unwrap().parse().unwrap()).await;
    drain_outbox(&svc, account_id, &mut session, &sender).await.unwrap();
    let raw = String::from_utf8_lossy(&sender.raw.lock().unwrap()).to_string();
    assert!(raw.contains("From: \"Hari at work\" <hari@work.example>"), "{raw}");
}

#[tokio::test]
async fn a_draft_moves_to_another_account_with_what_was_written() {
    let (svc, _dir) = service();
    let home = seed_account(&svc);
    seed_mailbox(&svc, home, "Drafts", MailboxRole::Drafts);
    let vault = svc.get().unwrap();
    let mut work = Account::new(Provider::Custom, "me@work.example");
    work.identities.push(karmayoga_core::account::Identity {
        name: "Support".into(),
        address: "help@work.example".into(),
        signature_html: String::new(),
    });
    let work_id = work.id;
    vault.save_account(&work).unwrap();

    let mut draft = call(&svc, "new_draft", json!({ "account": home })).await;
    draft["to"] = json!([{ "name": "", "email": "bob@example.com" }]);
    draft["subject"] = json!("Quote");
    call(&svc, "save_draft", json!({ "draft": draft.clone() })).await;
    let old_id: DraftId = draft["id"].as_str().unwrap().parse().unwrap();
    // Its autosave reached the home account's server.
    let mut session = FakeSession::default();
    drain_outbox(&svc, home, &mut session, &FakeSender::default()).await.unwrap();
    assert!(vault_draft(&svc, old_id).server_copy.is_some());

    // Written on since the last save: the move carries the window's copy.
    draft["bodyHtml"] = json!("<p>Here it is.</p>");
    let refused = fails(
        &svc,
        "move_draft",
        json!({ "draft": draft.clone(), "account": work_id, "identity": "me@example.com" }),
    )
    .await;
    assert!(refused.message.contains("not an address of"), "{}", refused.message);

    let moved = call(
        &svc,
        "move_draft",
        json!({ "draft": draft.clone(), "account": work_id, "identity": "HELP@work.example" }),
    )
    .await;
    let new_id: DraftId = moved["id"].as_str().unwrap().parse().unwrap();
    assert_ne!(new_id, old_id);
    let new = vault_draft(&svc, new_id);
    assert_eq!(new.account_id, work_id);
    assert_eq!(new.identity, "help@work.example");
    assert_eq!(new.subject, "Quote");
    assert_eq!(new.body_html, "<p>Here it is.</p>");
    assert_eq!(new.to, vec![Address::bare("bob@example.com")]);
    assert_eq!(new.state, DraftState::Editing);
    assert!(new.server_copy.is_none());

    // The old one is thrown away, and its copy on the home server with it.
    assert_eq!(vault_draft(&svc, old_id).state, DraftState::Discarded);
    let discards: Vec<Op> = vault
        .due_ops(home, Timestamp::now() + SignedDuration::from_secs(60), 10)
        .unwrap()
        .into_iter()
        .filter(|o| matches!(o.kind, OpKind::DiscardDraft) && o.state == OpState::Pending)
        .collect();
    assert_eq!(discards.len(), 1);
    assert_eq!(discards[0].target, OpTarget::Draft(old_id));

    // One never saved yet has nothing to throw away: what was written moves.
    let mut fresh = call(&svc, "new_draft", json!({ "account": home })).await;
    fresh["subject"] = json!("Not saved yet");
    let moved = call(
        &svc,
        "move_draft",
        json!({ "draft": fresh, "account": work_id, "identity": "me@work.example" }),
    )
    .await;
    let new = vault_draft(&svc, moved["id"].as_str().unwrap().parse().unwrap());
    assert_eq!((new.account_id, new.subject.as_str()), (work_id, "Not saved yet"));

    // Only a draft still being written moves.
    let again = fails(
        &svc,
        "move_draft",
        json!({ "draft": draft, "account": work_id, "identity": "me@work.example" }),
    )
    .await;
    assert!(again.message.contains("cannot move"), "{}", again.message);
}

#[tokio::test]
async fn the_outbox_holds_what_failed_and_what_is_being_retried_and_scheduled_does_not() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    seed_mailbox(&svc, account, "Drafts", MailboxRole::Drafts);
    let mut session = FakeSession::default();
    // Each drain runs one new draft's autosave and its send, nothing older.
    let sent = queued_send(&svc, account).await;
    drain_outbox(&svc, account, &mut session, &FakeSender::default()).await.unwrap();
    let refused = FakeSender {
        fail: Some(MailError::Server("550 no such user".into())),
        ..FakeSender::default()
    };
    let failed = queued_send(&svc, account).await;
    drain_outbox(&svc, account, &mut session, &refused).await.unwrap();
    let down =
        FakeSender { fail: Some(MailError::Network("timed out".into())), ..FakeSender::default() };
    let retrying = queued_send(&svc, account).await;
    drain_outbox(&svc, account, &mut session, &down).await.unwrap();

    // A send later, never tried, is Scheduled and not in the Outbox.
    let mut later = call(&svc, "new_draft", json!({ "account": account })).await;
    later["to"] = json!([{ "name": "", "email": "later@example.com" }]);
    call(&svc, "save_draft", json!({ "draft": later.clone() })).await;
    let at = (Timestamp::now() + SignedDuration::from_hours(5)).to_string();
    call(&svc, "send_draft", json!({ "id": later["id"], "sendAt": at })).await;

    let outbox = call(&svc, "outbox", json!({})).await;
    let items = outbox.as_array().unwrap();
    let find = |id: DraftId| items.iter().find(|i| i["draft"]["id"] == id.to_string());
    assert_eq!(items.len(), 2, "{outbox:#}");
    assert!(find(sent).is_none());
    let gave_up = find(failed).expect("the refused one");
    assert_eq!(gave_up["draft"]["sendError"], "550 no such user");
    assert!(gave_up.get("nextAttempt").is_none());
    let trying = find(retrying).expect("the retried one");
    assert!(trying["nextAttempt"].is_string());
    assert_eq!(trying["attempts"], 1);

    let scheduled = call(&svc, "scheduled_sends", json!({})).await;
    let scheduled: Vec<&str> =
        scheduled.as_array().unwrap().iter().map(|s| s["draft"]["id"].as_str().unwrap()).collect();
    assert_eq!(scheduled, vec![later["id"].as_str().unwrap()]);
}

/// The race `/code-review` found: a drain reads a waiting send, the person
/// stops it, and the drain's own copy used to be written back as InFlight
/// over Cancelled -- sending what had just been stopped.
#[tokio::test]
async fn a_send_stopped_after_a_drain_read_it_is_not_claimed_or_sent() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    seed_mailbox(&svc, account, "Drafts", MailboxRole::Drafts);
    let id = queued_send(&svc, account).await;
    let down =
        FakeSender { fail: Some(MailError::Network("timed out".into())), ..FakeSender::default() };
    let mut session = FakeSession::default();
    drain_outbox(&svc, account, &mut session, &down).await.unwrap();

    let vault = svc.get().unwrap();
    let later = Timestamp::now() + SignedDuration::from_hours(2);
    let read: Vec<Op> = vault
        .due_ops(account, later, 10)
        .unwrap()
        .into_iter()
        .filter(|o| matches!(o.kind, OpKind::Send))
        .collect();
    assert_eq!(read.len(), 1, "the drain's copy, Pending");

    call(&svc, "stop_send", json!({ "draftId": id })).await;
    assert!(vault.claim_ops(&read).unwrap().is_empty(), "stopped wins");
    assert_eq!(vault.op(read[0].id).unwrap().state, OpState::Cancelled);
    assert_eq!(vault_draft(&svc, id).state, DraftState::Editing);
}

#[tokio::test]
async fn new_settings_bring_waiting_sends_forward_but_leave_one_in_flight() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let vault = svc.get().unwrap();
    let later = Timestamp::now() + SignedDuration::from_hours(1);
    let mut waiting =
        Op::new(account, OpKind::Send, OpTarget::Draft(DraftId::new()), Origin::Person)
            .not_before(later);
    waiting.attempts = 3;
    vault.enqueue_op(&waiting).unwrap();
    let mut sending =
        Op::new(account, OpKind::Send, OpTarget::Draft(DraftId::new()), Origin::Person)
            .not_before(later);
    sending.attempts = 3;
    vault.enqueue_op(&sending).unwrap();
    // A drain claimed this one; the bump must not put it back to Pending.
    let claimed = vault.claim_ops(&[sending.clone()]).unwrap();
    assert_eq!(claimed.len(), 1);

    let now = Timestamp::now();
    assert_eq!(vault.retry_sends_now(account, now).unwrap(), 1);
    assert!(vault.op(waiting.id).unwrap().not_before <= now);
    let sending = vault.op(sending.id).unwrap();
    assert_eq!(sending.state, OpState::InFlight);
    assert_eq!(sending.not_before, later);
}

/// A reply shows in its thread from the moment it is sent -- queued inside
/// its undo window, then sent -- until the server's copy, found by the
/// `Message-ID` the send gave it, is stored; then the copy is what shows.
#[tokio::test]
async fn a_sent_reply_shows_in_its_thread_until_its_copy_arrives() {
    let (svc, _dir) = service();
    let account = seed_account(&svc);
    let inbox = seed_mailbox(&svc, account, "INBOX", MailboxRole::Inbox);
    let sent = seed_mailbox(&svc, account, "Sent", MailboxRole::Sent);
    let thread = ThreadId::new();
    let parent = seed_in_thread(&svc, account, thread, inbox, 1);
    let vault = svc.get().unwrap();
    let reply_to_parent = || {
        let mut reply = Draft::new(account, "me@example.com", Origin::Person);
        reply.in_reply_to = Some(parent);
        reply.to = vec![Address::bare("sender@example.com")];
        vault.save_draft(&reply).unwrap();
        reply.id
    };
    let reply_id = reply_to_parent();
    let later = reply_to_parent();
    call(&svc, "send_draft", json!({ "id": reply_id })).await;
    let monday = Timestamp::now() + SignedDuration::from_hours(72);
    call(&svc, "send_draft", json!({ "id": later, "sendAt": monday })).await;
    let mut reply = vault.draft(reply_id).unwrap();
    let sending = |detail: &Value| -> Vec<String> {
        detail["sending"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_str().unwrap().to_string())
            .collect()
    };

    let detail = call(&svc, "get_thread", json!({ "id": thread.to_string() })).await;
    assert_eq!(
        sending(&detail),
        vec![reply.id.to_string()],
        "queued in its undo window: on its way; sent later: not yet"
    );

    reply.state = DraftState::Sent;
    reply.message_id = Some("reply-1@example.com".into());
    vault.save_draft(&reply).unwrap();
    let detail = call(&svc, "get_thread", json!({ "id": thread.to_string() })).await;
    assert_eq!(sending(&detail), vec![reply.id.to_string()], "sent, copy not here yet");

    let copy = seed_in_thread(&svc, account, thread, sent, 1);
    let mut message = vault.mail_message(copy).unwrap();
    message.message_id_header = "reply-1@example.com".into();
    vault.ingest_mail(account, vec![IngestMessage { message, mailbox: sent, uid: 2 }]).unwrap();
    let detail = call(&svc, "get_thread", json!({ "id": thread.to_string() })).await;
    assert!(sending(&detail).is_empty(), "the copy arrived: {detail}");
}
