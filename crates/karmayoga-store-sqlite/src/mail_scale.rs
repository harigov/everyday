//! A benchmark, not a test -- the mail-shaped sibling of `scale.rs`.
//!
//! Where `scale.rs` proves the *groundwork* (`upsert_many`, `page_after`)
//! against a stand-in table, this exercises the real thing: a hundred
//! thousand synthetic messages, ingested through
//! [`MailStore::ingest`](karmayoga_core::store::mail::MailStore::ingest) in
//! batches of about five thousand, into one mailbox -- and then times the
//! four reads the plan's speed budget actually names: the first inbox page,
//! a later page reached by cursor, an unread count, and opening one thread.
//!
//! `#[ignore]`d for the reason `scale.rs` is: a hundred-thousand-row write
//! is real seconds of work and belongs to nobody's ordinary `cargo test`.
//! Run it on purpose, in release (a debug build's AEAD and JSON encoding are
//! not what production pays):
//!
//! ```text
//! cargo test -p karmayoga-store-sqlite --release a_hundred_thousand_messages -- --ignored --nocapture
//! ```

use super::SqliteStore;
use karmayoga_core::crypto::{AeadCipher, Cipher, SecretKey};
use karmayoga_core::id::{AccountId, PackId, ThreadId};
use karmayoga_core::mail::{Address, CategorySource, Mailbox, MailboxRole, Message, MessageFlags};
use karmayoga_core::packstore::PackRef;
use karmayoga_core::store::StoreContext;
use karmayoga_core::store::mail::{IngestMessage, MailStore, ThreadFilter};
use std::sync::Arc;
use std::time::Instant;

const ROWS: usize = 100_000;
const BATCH: usize = 5_000;
const PAGE_SIZE: u32 = 50;

fn synthetic_message(account: AccountId, i: usize, base: jiff::Timestamp) -> Message {
    let id = karmayoga_core::id::MailMessageId::new();
    Message {
        id,
        account_id: account,
        thread_id: ThreadId::new(), // one message, one thread -- an ordinary inbox's common case
        message_id_header: format!("<{id}@scale.example>"),
        date: base + jiff::SignedDuration::from_micros(i as i64),
        from: Address::new("A Sender", format!("sender{}@example.com", i % 500)),
        to: vec![Address::bare("me@example.com")],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: format!("synthetic message {i}"),
        snippet: "x".repeat(120),
        flags: MessageFlags::default(),
        labels: Vec::new(),
        has_attachments: false,
        size: 2_000,
        category: None,
        category_source: CategorySource::Rules,
        pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
        gmail: None,
        invite: None,
    }
}

#[test]
#[ignore = "ingests 100,000 messages; run explicitly, in release, and read the printed timings"]
fn a_hundred_thousand_messages_against_the_speed_budget() {
    let dir = tempfile::tempdir().unwrap();
    let cipher: Arc<dyn Cipher> = Arc::new(AeadCipher::new(&SecretKey::from_bytes([11u8; 32])));
    let store = SqliteStore::open(StoreContext::new(dir.path(), cipher)).unwrap();

    let account = AccountId::new();
    let mailbox = Mailbox::new(account, "INBOX", MailboxRole::Inbox);
    store.put_mailbox(&mailbox).unwrap();

    let base = jiff::Timestamp::now();
    let ingest_started = Instant::now();
    for batch_start in (0..ROWS).step_by(BATCH) {
        let batch: Vec<IngestMessage> = (batch_start..(batch_start + BATCH).min(ROWS))
            .map(|i| IngestMessage {
                message: synthetic_message(account, i, base),
                mailbox: mailbox.id,
                uid: i as u32 + 1,
            })
            .collect();
        store.ingest(account, batch).unwrap();
    }
    let ingest_elapsed = ingest_started.elapsed();

    // The first inbox page.
    let first_page_started = Instant::now();
    let first_page =
        store.list_threads(mailbox.id, &ThreadFilter::default(), None, PAGE_SIZE).unwrap();
    let first_page_elapsed = first_page_started.elapsed();
    assert_eq!(first_page.threads.len() as u32, PAGE_SIZE);

    // Walk to the 50th page (unmeasured setup), then time reaching it from
    // its own cursor -- "the tenth 'next page' click" the keyset module's
    // own docs describe, thirty-nine clicks further in.
    let mut cursor = first_page.next_cursor;
    for _ in 0..48 {
        let page = store
            .list_threads(mailbox.id, &ThreadFilter::default(), cursor.as_deref(), PAGE_SIZE)
            .unwrap();
        cursor = page.next_cursor;
    }
    let fiftieth_page_started = Instant::now();
    let fiftieth_page = store
        .list_threads(mailbox.id, &ThreadFilter::default(), cursor.as_deref(), PAGE_SIZE)
        .unwrap();
    let fiftieth_page_elapsed = fiftieth_page_started.elapsed();
    assert_eq!(fiftieth_page.threads.len() as u32, PAGE_SIZE);

    // An unread count.
    let unread_started = Instant::now();
    let counts = store.unread_counts(account).unwrap();
    let unread_elapsed = unread_started.elapsed();
    assert_eq!(counts.iter().find(|(id, _)| *id == mailbox.id).unwrap().1, ROWS as u64);

    // Opening one thread.
    let some_thread = fiftieth_page.threads[0].id;
    let open_started = Instant::now();
    let (_, messages) = store.thread(some_thread).unwrap();
    let open_elapsed = open_started.elapsed();
    assert_eq!(messages.len(), 1);

    // The list's Starred filter at its worst: a hundred stars scattered over
    // the whole mailbox, so a page of them walks a good share of its rows,
    // and none at all, which walks every one to say so. The attachment filter
    // has the same shape, and reads the same rows. Then both again across two
    // mailboxes, as the unified Inbox asks: `IN` sorts everything it matches
    // rather than walking an index in order, so this is where a filter that
    // cost a probe per row would pay for all of them whatever it found.
    let every_thread = store.list_threads(mailbox.id, &ThreadFilter::default(), None, 100_000);
    for thread in every_thread.unwrap().threads.iter().step_by(ROWS / 100) {
        let (_, messages) = store.thread(thread.id).unwrap();
        let mut flags = messages[0].flags;
        flags.flagged = true;
        store.set_message_flags(messages[0].id, flags).unwrap();
    }
    let elsewhere = Mailbox::new(account, "Elsewhere", MailboxRole::Other);
    store.put_mailbox(&elsewhere).unwrap();
    let both = [mailbox.id, elsewhere.id];
    let starred = ThreadFilter { starred: Some(true), ..Default::default() };
    let attached = ThreadFilter { has_attachments: Some(true), ..Default::default() };

    let starred_started = Instant::now();
    let starred_page = store.list_threads(mailbox.id, &starred, None, PAGE_SIZE).unwrap();
    let starred_elapsed = starred_started.elapsed();
    assert_eq!(starred_page.threads.len() as u32, PAGE_SIZE);

    let none_started = Instant::now();
    let none_page = store.list_threads(mailbox.id, &attached, None, PAGE_SIZE).unwrap();
    let none_elapsed = none_started.elapsed();
    assert!(none_page.threads.is_empty());

    let across_started = Instant::now();
    let across_page = store.list_threads_across(&both, &starred, None, PAGE_SIZE).unwrap();
    let across_elapsed = across_started.elapsed();
    assert_eq!(across_page.threads.len() as u32, PAGE_SIZE);

    let across_none_started = Instant::now();
    let across_none = store.list_threads_across(&both, &attached, None, PAGE_SIZE).unwrap();
    let across_none_elapsed = across_none_started.elapsed();
    assert!(across_none.threads.is_empty());

    println!(
        "ingest: {ROWS} messages in batches of {BATCH} in {ingest_elapsed:?} \
         ({:.0} messages/s)",
        ROWS as f64 / ingest_elapsed.as_secs_f64(),
    );
    println!("first inbox page ({PAGE_SIZE} threads): {first_page_elapsed:?}");
    println!("50th page by cursor: {fiftieth_page_elapsed:?}");
    println!("unread count, {ROWS} messages: {unread_elapsed:?}");
    println!("opening one thread: {open_elapsed:?}");
    println!("first page of Starred, 1 in 1,000 starred: {starred_elapsed:?}");
    println!("Attachments with none in {ROWS} threads: {none_elapsed:?}");
    println!("the same across two mailboxes: {across_elapsed:?}, {across_none_elapsed:?}");

    // The speed budget: opening an already-synced thread, 100 ms; a mail
    // shortcut (which a page fetch stands in for here), 60 ms. An unread
    // count has no named budget of its own -- it is a whole-mailbox
    // aggregate, not a keypress -- so it is held to the 150 ms a search's
    // first page gets, the closest named number to "scan everything and add
    // it up." Printed regardless, so a miss is visible even when nobody set
    // `RUST_LOG`; asserted so a regression fails the run outright rather
    // than scrolling past in a wall of `cargo test --nocapture` output.
    assert!(open_elapsed.as_millis() < 100, "opening a thread missed the 100 ms budget");
    assert!(first_page_elapsed.as_millis() < 60, "the first inbox page missed the 60 ms budget");
    assert!(fiftieth_page_elapsed.as_millis() < 60, "a later page missed the 60 ms budget");
    assert!(unread_elapsed.as_millis() < 150, "the unread count missed the 150 ms search budget");
    // Toggling a filter is a mail shortcut (`g r`) and a click on a page fetch,
    // and a toggle stays on across restarts, so it is held to the same 60 ms.
    assert!(starred_elapsed.as_millis() < 60, "a Starred page missed the 60 ms budget");
    assert!(none_elapsed.as_millis() < 60, "a filter matching nothing missed the 60 ms budget");
    assert!(across_elapsed.as_millis() < 60, "Starred across mailboxes missed the 60 ms budget");
    assert!(
        across_none_elapsed.as_millis() < 60,
        "matching nothing across mailboxes missed it too"
    );
}
