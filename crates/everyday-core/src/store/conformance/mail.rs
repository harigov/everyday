//! The mail half of the conformance suite.
//!
//! Handed the whole [`JournalStore`], the same as the purpose, tracking and
//! note suites, because the cascade worth checking -- an account delete
//! taking every mail row with it -- reaches into a store this domain does
//! not own.

use super::*;
use crate::id::{AccountId, MailMessageId, MailboxId, PackId, ThreadId};
use crate::mail::{
    Address, Body, Draft, Mailbox, MailboxRole, Message, MessageFlags, Op, OpKind, OpState,
    OpTarget, Origin, PartRef,
};
use crate::packstore::PackRef as MailPackRef;
use crate::store::mail::{IngestMessage, MailStore, ThreadFilter};
use jiff::{SignedDuration, Timestamp};

/// Everything a backend must do with mail: ingest, the two thread lists,
/// flag and label changes, removal, the outbox, drafts, a `UIDVALIDITY`
/// reset, and the one cascade this suite does not own -- an account delete
/// taking every row above with it.
pub fn run_mail_suite(store: &dyn JournalStore) {
    eprintln!("--- mail conformance suite ---");

    mail_starts_empty(store);
    ingest_then_list(store);
    threads_span_two_mailboxes(store);
    keyset_paging_a_thousand_threads_has_no_duplicates_or_gaps(store);
    flag_change_updates_unread_counts(store);
    removal_shrinks_a_thread_and_deletes_an_empty_one(store);
    op_queue_ordering_and_not_before(store);
    optimistic_writes_and_snooze_round_trip(store);
    draft_round_trips(store);
    a_uidvalidity_reset_forgets_uids_but_keeps_messages(store);
    account_delete_cascades_every_mail_row(store);

    eprintln!("--- mail suite passed ---");
}

fn mail_store(store: &dyn JournalStore) -> &dyn MailStore {
    store.mail().expect("the mail suite needs a mail store")
}

/// Delete everything belonging to `account` -- what every test below uses to
/// leave the store as it found it, and what
/// [`account_delete_cascades_every_mail_row`] checks directly rather than
/// merely relying on for cleanup.
fn cleanup_account(store: &dyn JournalStore, account: AccountId) {
    store
        .accounts()
        .expect("the mail suite needs an account store, for the cascade it deletes by")
        .delete_account(account)
        .expect("cleanup delete_account");
}

fn message(
    account: AccountId,
    thread: ThreadId,
    subject: &str,
    from: &str,
    date: Timestamp,
) -> Message {
    let id = MailMessageId::new();
    Message {
        id,
        account_id: account,
        thread_id: thread,
        message_id_header: format!("<{id}@conformance.example>"),
        date,
        from: Address::bare(from),
        to: Vec::new(),
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: subject.into(),
        snippet: String::new(),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 128,
        category: None,
        pack: MailPackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
    }
}

fn mail_starts_empty(store: &dyn JournalStore) {
    let account = AccountId::new();
    assert!(mail_store(store).list_mailboxes(account).unwrap().is_empty());
    assert!(mail_store(store).list_drafts(account).unwrap().is_empty());
}

fn ingest_then_list(store: &dyn JournalStore) {
    let m = mail_store(store);
    let account = AccountId::new();
    let mailbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
    m.put_mailbox(&mailbox).unwrap();

    let thread_id = ThreadId::new();
    let msg = message(account, thread_id, "Hello", "sender@example.com", Timestamp::now());
    m.ingest(account, vec![IngestMessage { message: msg.clone(), mailbox: mailbox.id, uid: 1 }])
        .unwrap();

    let page = m.list_threads(mailbox.id, &ThreadFilter::default(), None, 10).unwrap();
    assert_eq!(page.threads.len(), 1);
    assert_eq!(page.threads[0].id, thread_id);
    assert_eq!(page.threads[0].subject, "Hello");
    assert_eq!(page.threads[0].message_count, 1);
    assert_eq!(page.threads[0].unread_count, 1, "a fresh message starts unread");
    assert!(page.threads[0].participants.iter().any(|a| a.email == "sender@example.com"));

    let (thread, messages) = m.thread(thread_id).unwrap();
    assert_eq!(thread.id, thread_id);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].id, msg.id);

    let found = m.message_by_uid(mailbox.id, 1).unwrap().expect("uid 1 was just ingested");
    assert_eq!(found.id, msg.id);
    assert_eq!(m.get_message(msg.id).unwrap().id, msg.id);

    cleanup_account(store, account);
}

/// A Gmail label: the same physical message, filed under two mailboxes at
/// once. One row in `messages`, two in `message_mailboxes`, and the thread
/// must be reachable -- and correctly counted -- from either mailbox's list.
fn threads_span_two_mailboxes(store: &dyn JournalStore) {
    let m = mail_store(store);
    let account = AccountId::new();
    let inbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
    let label = Mailbox::new(account, "Work", MailboxRole::Other);
    m.put_mailbox(&inbox).unwrap();
    m.put_mailbox(&label).unwrap();

    let thread_id = ThreadId::new();
    let msg = message(account, thread_id, "Labelled", "a@example.com", Timestamp::now());
    m.ingest(
        account,
        vec![
            IngestMessage { message: msg.clone(), mailbox: inbox.id, uid: 1 },
            IngestMessage { message: msg.clone(), mailbox: label.id, uid: 1 },
        ],
    )
    .unwrap();

    let locations = m.message_locations(msg.id).unwrap();
    assert_eq!(locations.len(), 2, "one physical message, two mailbox locations");
    assert!(locations.contains(&(inbox.id, 1)));
    assert!(locations.contains(&(label.id, 1)));

    let in_inbox = m.list_threads(inbox.id, &ThreadFilter::default(), None, 10).unwrap();
    let in_label = m.list_threads(label.id, &ThreadFilter::default(), None, 10).unwrap();
    assert_eq!(in_inbox.threads.len(), 1);
    assert_eq!(in_label.threads.len(), 1);
    assert_eq!(in_inbox.threads[0].id, thread_id);
    assert_eq!(in_label.threads[0].id, thread_id);

    let (thread, messages) = m.thread(thread_id).unwrap();
    assert_eq!(messages.len(), 1, "one physical message, filed under two mailboxes");
    assert_eq!(thread.message_count, 1);

    cleanup_account(store, account);
}

fn keyset_paging_a_thousand_threads_has_no_duplicates_or_gaps(store: &dyn JournalStore) {
    let m = mail_store(store);
    let account = AccountId::new();
    let mailbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
    m.put_mailbox(&mailbox).unwrap();

    let base = Timestamp::now();
    let mut ingest = Vec::with_capacity(1000);
    let mut ids = std::collections::HashSet::new();
    for i in 0..1000u32 {
        let thread_id = ThreadId::new();
        ids.insert(thread_id);
        let date = base + SignedDuration::from_secs(i64::from(i));
        let msg = message(account, thread_id, &format!("thread {i}"), "a@example.com", date);
        ingest.push(IngestMessage { message: msg, mailbox: mailbox.id, uid: i + 1 });
    }
    for chunk in ingest.chunks(137) {
        m.ingest(account, chunk.to_vec()).unwrap();
    }

    let mut seen = std::collections::HashSet::new();
    let mut cursor: Option<String> = None;
    loop {
        let page =
            m.list_threads(mailbox.id, &ThreadFilter::default(), cursor.as_deref(), 47).unwrap();
        if page.threads.is_empty() {
            break;
        }
        for t in &page.threads {
            assert!(ids.contains(&t.id), "an unexpected thread id was returned");
            assert!(seen.insert(t.id), "thread {:?} was returned twice across pages", t.id);
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen.len(), 1000, "every thread must be seen exactly once, with no gap");

    cleanup_account(store, account);
}

fn flag_change_updates_unread_counts(store: &dyn JournalStore) {
    let m = mail_store(store);
    let account = AccountId::new();
    let mailbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
    m.put_mailbox(&mailbox).unwrap();

    let thread_id = ThreadId::new();
    let msg = message(account, thread_id, "Unread", "a@example.com", Timestamp::now());
    m.ingest(account, vec![IngestMessage { message: msg.clone(), mailbox: mailbox.id, uid: 5 }])
        .unwrap();

    let counts = m.unread_counts(account).unwrap();
    let unread = |counts: &[(MailboxId, u64)]| {
        counts.iter().find(|(id, _)| *id == mailbox.id).map(|(_, n)| *n).unwrap()
    };
    assert_eq!(unread(&counts), 1);

    let mut flags = msg.flags;
    flags.seen = true;
    m.update_flags(mailbox.id, 5, flags).unwrap();

    assert_eq!(unread(&m.unread_counts(account).unwrap()), 0);
    let (thread, _) = m.thread(thread_id).unwrap();
    assert_eq!(thread.unread_count, 0);

    // A uid this store never ingested is a no-op, not an error.
    m.update_flags(mailbox.id, 999, flags).unwrap();

    cleanup_account(store, account);
}

fn removal_shrinks_a_thread_and_deletes_an_empty_one(store: &dyn JournalStore) {
    let m = mail_store(store);
    let account = AccountId::new();
    let mailbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
    m.put_mailbox(&mailbox).unwrap();

    let thread_id = ThreadId::new();
    let first = message(account, thread_id, "one", "a@example.com", Timestamp::now());
    let second = message(account, thread_id, "two", "b@example.com", Timestamp::now());
    m.ingest(
        account,
        vec![
            IngestMessage { message: first.clone(), mailbox: mailbox.id, uid: 10 },
            IngestMessage { message: second.clone(), mailbox: mailbox.id, uid: 11 },
        ],
    )
    .unwrap();
    assert_eq!(m.thread(thread_id).unwrap().0.message_count, 2);

    m.remove_uids(mailbox.id, &[10]).unwrap();
    let (thread, messages) = m.thread(thread_id).unwrap();
    assert_eq!(thread.message_count, 1, "removal shrinks the thread");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].id, second.id);

    m.remove_uids(mailbox.id, &[11]).unwrap();
    assert!(m.thread(thread_id).is_err(), "a thread with nothing left in it is deleted");
    assert!(
        m.list_threads(mailbox.id, &ThreadFilter::default(), None, 10).unwrap().threads.is_empty()
    );

    // Removing again, and removing an empty set, are both no-ops.
    m.remove_uids(mailbox.id, &[10, 11]).unwrap();
    m.remove_uids(mailbox.id, &[]).unwrap();

    cleanup_account(store, account);
}

fn op_queue_ordering_and_not_before(store: &dyn JournalStore) {
    let m = mail_store(store);
    let account = AccountId::new();
    let now = Timestamp::now();

    let due_first =
        Op::new(account, OpKind::Archive, OpTarget::Thread(ThreadId::new()), Origin::Person)
            .not_before(now - SignedDuration::from_secs(20));
    let due_second =
        Op::new(account, OpKind::Star, OpTarget::Thread(ThreadId::new()), Origin::Person)
            .not_before(now - SignedDuration::from_secs(5));
    let not_yet_due = Op::new(
        account,
        OpKind::Trash,
        OpTarget::Thread(ThreadId::new()),
        Origin::Assistant { conversation: "conv-1".into() },
    )
    .not_before(now + SignedDuration::from_secs(1_000));
    for op in [&due_first, &due_second, &not_yet_due] {
        m.enqueue_op(op).unwrap();
    }

    let due = m.due_ops(account, now, 10).unwrap();
    assert_eq!(due.len(), 2, "the far-future op must not be due yet");
    assert_eq!(due[0].id, due_first.id, "oldest not_before comes first");
    assert_eq!(due[1].id, due_second.id);
    assert_eq!(m.get_op(due_first.id).unwrap().id, due_first.id);

    let assistants = m.ops_by_origin("assistant", 10).unwrap();
    assert!(assistants.iter().any(|o| o.id == not_yet_due.id));
    assert!(
        assistants.iter().all(|o| o.id != due_first.id),
        "a person's op is not assistant-origin"
    );

    // Transitioning an op out of `Pending` takes it out of `due_ops`.
    let mut in_flight = due_first.clone();
    in_flight.transition_to(OpState::InFlight).unwrap();
    m.update_op(&in_flight).unwrap();
    let due = m.due_ops(account, now, 10).unwrap();
    assert_eq!(due.len(), 1, "an in-flight op is no longer pending-due");
    assert_eq!(due[0].id, due_second.id);

    cleanup_account(store, account);
}

/// The optimistic-write half phase 3 adds: setting a message's flags or
/// labels directly by id (not by `(mailbox, uid)`, the sync engine's own
/// vocabulary), hiding and restoring a thread's place in one mailbox, and
/// the snooze clock the minute scheduler reads.
fn optimistic_writes_and_snooze_round_trip(store: &dyn JournalStore) {
    let m = mail_store(store);
    let account = AccountId::new();
    let inbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
    m.put_mailbox(&inbox).unwrap();

    let thread_id = ThreadId::new();
    let msg = message(account, thread_id, "optimistic", "a@example.com", Timestamp::now());
    m.ingest(account, vec![IngestMessage { message: msg.clone(), mailbox: inbox.id, uid: 1 }])
        .unwrap();

    // Flags and labels, set directly by message id.
    let mut flags = msg.flags;
    flags.seen = true;
    flags.flagged = true;
    m.set_message_flags(msg.id, flags).unwrap();
    let (_, messages) = m.thread(thread_id).unwrap();
    assert!(messages[0].flags.seen && messages[0].flags.flagged);

    m.set_message_labels(msg.id, vec!["Work".into()]).unwrap();
    let (_, messages) = m.thread(thread_id).unwrap();
    assert_eq!(messages[0].labels, vec!["Work".to_string()]);

    // A message id this store has never ingested is a no-op, not an error.
    m.set_message_flags(MailMessageId::new(), MessageFlags::default()).unwrap();
    m.set_message_labels(MailMessageId::new(), Vec::new()).unwrap();

    // Hiding a thread from a mailbox removes it from that mailbox's list
    // without touching the durable `message_mailboxes` mapping, so
    // restoring recomputes exactly what was hidden.
    let page = m.list_threads(inbox.id, &ThreadFilter::default(), None, 10).unwrap();
    assert_eq!(page.threads.len(), 1);
    m.hide_thread_from_mailbox(thread_id, inbox.id).unwrap();
    let page = m.list_threads(inbox.id, &ThreadFilter::default(), None, 10).unwrap();
    assert!(page.threads.is_empty(), "archiving hides the thread from the inbox list");
    m.restore_thread_mailboxes(thread_id).unwrap();
    let page = m.list_threads(inbox.id, &ThreadFilter::default(), None, 10).unwrap();
    assert_eq!(page.threads.len(), 1, "restoring brings it back");

    // Snoozing sets `Thread::snoozed_until`, and the thread appears in
    // `due_snoozed_threads` once that moment has passed.
    let now = Timestamp::now();
    m.set_thread_snoozed_until(thread_id, Some(now + SignedDuration::from_secs(1))).unwrap();
    let (thread, _) = m.thread(thread_id).unwrap();
    assert!(thread.snoozed_until.is_some());
    assert!(
        m.due_snoozed_threads(now, 10).unwrap().is_empty(),
        "not due until its own moment has passed"
    );
    let due = m.due_snoozed_threads(now + SignedDuration::from_secs(2), 10).unwrap();
    assert!(due.contains(&thread_id));

    m.set_thread_snoozed_until(thread_id, None).unwrap();
    let (thread, _) = m.thread(thread_id).unwrap();
    assert!(thread.snoozed_until.is_none(), "clearing releases the snooze");

    cleanup_account(store, account);
}

fn draft_round_trips(store: &dyn JournalStore) {
    let m = mail_store(store);
    let account = AccountId::new();

    let mut draft =
        Draft::new(account, "me@example.com", Origin::Assistant { conversation: "conv-9".into() });
    draft.to = vec![Address::new("Someone", "someone@example.com")];
    draft.subject = "A draft".into();
    draft.body_html = "<p>Hello</p>".into();
    m.put_draft(&draft).unwrap();

    let listed = m.list_drafts(account).unwrap();
    assert_eq!(listed, vec![draft.clone()]);
    assert_eq!(m.get_draft(draft.id).unwrap(), draft);

    m.delete_draft(draft.id).unwrap();
    assert!(m.list_drafts(account).unwrap().is_empty());
    m.delete_draft(draft.id).unwrap(); // deleting again is a no-op

    cleanup_account(store, account);
}

fn a_uidvalidity_reset_forgets_uids_but_keeps_messages(store: &dyn JournalStore) {
    let m = mail_store(store);
    let account = AccountId::new();
    let mailbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
    m.put_mailbox(&mailbox).unwrap();

    let thread_id = ThreadId::new();
    let msg = message(account, thread_id, "survives a reset", "a@example.com", Timestamp::now());
    m.ingest(account, vec![IngestMessage { message: msg.clone(), mailbox: mailbox.id, uid: 1 }])
        .unwrap();
    assert!(m.message_by_uid(mailbox.id, 1).unwrap().is_some());

    m.reset_mailbox(mailbox.id).unwrap();

    assert!(m.message_by_uid(mailbox.id, 1).unwrap().is_none(), "the uid mapping is forgotten");
    assert!(m.uid_set(mailbox.id).unwrap().is_empty());

    // The message itself, and its thread, survive: this is what lets a
    // rematch by `Message-ID` re-attach it under a fresh uid.
    let rematched = m
        .message_by_message_id_header(account, &msg.message_id_header)
        .unwrap()
        .expect("a reset must not delete the message it is rematching");
    assert_eq!(rematched.id, msg.id);

    m.ingest(account, vec![IngestMessage { message: msg.clone(), mailbox: mailbox.id, uid: 2 }])
        .unwrap();
    assert_eq!(m.message_by_uid(mailbox.id, 2).unwrap().unwrap().id, msg.id);

    cleanup_account(store, account);
}

fn account_delete_cascades_every_mail_row(store: &dyn JournalStore) {
    let m = mail_store(store);
    let account = AccountId::new();
    let mailbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
    m.put_mailbox(&mailbox).unwrap();

    let thread_id = ThreadId::new();
    let msg = message(account, thread_id, "cascade", "a@example.com", Timestamp::now());
    m.ingest(account, vec![IngestMessage { message: msg.clone(), mailbox: mailbox.id, uid: 1 }])
        .unwrap();
    let body = Body {
        message_id: msg.id,
        html_sanitised: "<p>hi</p>".into(),
        text: "hi".into(),
        quoted_ranges: Vec::new(),
        signature_range: None,
        parts: Vec::new(),
    };
    m.put_body(&body).unwrap();
    let draft = Draft::new(account, "me@example.com", Origin::Person);
    m.put_draft(&draft).unwrap();
    let op = Op::new(account, OpKind::Archive, OpTarget::Thread(thread_id), Origin::Person);
    m.enqueue_op(&op).unwrap();

    store.accounts().unwrap().delete_account(account).unwrap();

    assert!(m.list_mailboxes(account).unwrap().is_empty(), "the mailbox must not survive");
    assert!(m.thread(thread_id).is_err(), "the thread must not survive");
    assert!(m.get_body(msg.id).is_err(), "the body must not survive");
    assert!(m.list_drafts(account).unwrap().is_empty(), "the draft must not survive");
    assert!(
        m.due_ops(account, Timestamp::now() + SignedDuration::from_secs(3_600), 10)
            .unwrap()
            .is_empty(),
        "the op must not survive"
    );
    assert!(m.attachment_blob_refs().unwrap().is_empty());
}

/// Not part of [`run_mail_suite`], for the reason
/// [`library::garbage_collection_keeps_library_covers`](super::library::garbage_collection_keeps_library_covers)
/// is not part of the library suite: it is a question about the *journal*
/// store's [`JournalStore::collect_garbage`], not about mail's own trait.
/// See [`crate::mail::PartRef::blob`] and `MailStore::attachment_blob_refs`
/// for the walk this exercises.
pub(super) fn garbage_collection_learns_about_mail_attachments(store: &dyn JournalStore) {
    let Some(m) = store.mail() else { return };

    let account = AccountId::new();
    let mailbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
    m.put_mailbox(&mailbox).unwrap();
    let thread_id = ThreadId::new();
    let msg = message(account, thread_id, "attachment", "a@example.com", Timestamp::now());
    m.ingest(account, vec![IngestMessage { message: msg.clone(), mailbox: mailbox.id, uid: 1 }])
        .unwrap();

    let attached = store.put_blob(b"a photograph nobody has opened yet").unwrap();
    let orphan = store.put_blob(b"nobody's attachment").unwrap();

    let body = Body {
        message_id: msg.id,
        html_sanitised: String::new(),
        text: "see attached".into(),
        quoted_ranges: Vec::new(),
        signature_range: None,
        parts: vec![PartRef {
            cid: None,
            filename: Some("photo.jpg".into()),
            mime_type: "image/jpeg".into(),
            size: 13,
            blob: Some(attached),
        }],
    };
    m.put_body(&body).unwrap();

    let removed = store.collect_garbage(std::time::Duration::ZERO).unwrap();
    assert_eq!(removed, 1, "exactly the unreferenced blob should be collected");
    assert!(
        store.has_blob(attached).unwrap(),
        "an attachment a body still names is a live reference"
    );
    assert!(!store.has_blob(orphan).unwrap());

    // ...and it stops being one when the body goes -- via the account
    // cascade, the only way a body is removed today.
    store.accounts().unwrap().delete_account(account).unwrap();
    assert_eq!(store.collect_garbage(std::time::Duration::ZERO).unwrap(), 1);
    assert!(!store.has_blob(attached).unwrap(), "an attachment nothing points at is collectable");
}
