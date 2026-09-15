//! A benchmark, not a test.
//!
//! Phase 0 of the mail plan asks for one number before mail exists to
//! produce it: what does writing a mailbox-sized batch of rows, and reading
//! the first (and a later) page back, actually cost through the real
//! sealing/batching path in `everyday_store_sql::record::upsert_many` and
//! the keyset path in `everyday_store_sql::keyset::page_after`? This writes
//! a hundred thousand synthetic rows through `tasks` -- a real,
//! already-migrated table with a real clear column to order by, standing in
//! for `messages`, which does not exist yet. Nothing about `upsert_many` or
//! `page_after` knows or cares that the table is not a mailbox.
//!
//! `#[ignore]`d because a hundred-thousand-row write takes real seconds and
//! belongs to nobody's `cargo test`. Run it on purpose, in release (a debug
//! build's AEAD is not what production pays):
//!
//! ```text
//! cargo test -p everyday-store-sqlite --release a_hundred_thousand_rows -- --ignored --nocapture
//! ```
//!
//! # Read the printed read time against `tasks`, not against mail
//!
//! `tasks` has no index on `created_us` — nothing in this crate needs one
//! today — so the read half of this number is a full scan and sort of a
//! hundred thousand rows, not what `page_after` costs *with* the covering
//! index a real mail listing would have (`thread_mailboxes (mailbox_id,
//! last_date_us DESC, thread_id)`, named in the mail plan's schema). The
//! write half is the honest number either way: sealing and batching do not
//! care what index exists. Re-run against a table with
//! `CREATE INDEX ... (created_us, id)` once one exists, for the number that
//! actually matters.

use super::SqliteStore;
use everyday_core::crypto::{AeadCipher, Cipher, SecretKey};
use everyday_core::store::StoreContext;
use everyday_core::store::tasks::TaskStore;
use everyday_core::task::Task;
use everyday_store_sql::conn::Value;
use everyday_store_sql::keyset::{Dir, KeyCursor};
use std::sync::Arc;
use std::time::Instant;

const ROWS: usize = 100_000;
/// Bytes of `notes` on every synthetic row -- roughly a plain-text email
/// body's worth, so the byte cap in `upsert_many`'s batching, not only the
/// row cap, is actually the one closing most batches during this run.
const NOTE_BYTES: usize = 2_000;

#[test]
#[ignore = "writes 100,000 rows; run explicitly and read the printed timings"]
fn a_hundred_thousand_rows_written_and_the_first_page_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let cipher: Arc<dyn Cipher> = Arc::new(AeadCipher::new(&SecretKey::from_bytes([9u8; 32])));
    let store = SqliteStore::open(StoreContext::new(dir.path(), cipher)).unwrap();

    let base = jiff::Timestamp::now();
    let rows: Vec<Task> = (0..ROWS)
        .map(|i| {
            let mut t = Task::new(format!("synthetic message {i}"));
            t.notes = "x".repeat(NOTE_BYTES);
            t.created_at = base + jiff::SignedDuration::from_micros(i as i64);
            t
        })
        .collect();

    let write_started = Instant::now();
    store.put_tasks(&rows).unwrap();
    let write_elapsed = write_started.elapsed();

    let read_started = Instant::now();
    let mut sql = "SELECT id, created_us FROM tasks WHERE 1=1".to_string();
    let mut args: Vec<Value> = Vec::new();
    store
        .page_after(&mut sql, &mut args, &[("created_us", Dir::Asc), ("id", Dir::Asc)], None, 50)
        .unwrap();
    let first_page = store.with_read(|c| c.query(&sql, &args)).unwrap();
    let read_elapsed = read_started.elapsed();

    // A second page, from a cursor built off the first, so the number
    // reported also covers "the next page after a hundred thousand rows"
    // and not only "the very first rows in an otherwise-empty table" -- the
    // case that resembles opening a synced mailbox back up.
    let last = first_page.last().unwrap();
    let cursor =
        KeyCursor::new(vec![Value::Int(last.i64(1).unwrap()), Value::Text(last.text(0).unwrap())])
            .unwrap();
    let mut sql2 = "SELECT id, created_us FROM tasks WHERE 1=1".to_string();
    let mut args2: Vec<Value> = Vec::new();
    let next_page_started = Instant::now();
    store
        .page_after(
            &mut sql2,
            &mut args2,
            &[("created_us", Dir::Asc), ("id", Dir::Asc)],
            Some(&cursor),
            50,
        )
        .unwrap();
    let second_page = store.with_read(|c| c.query(&sql2, &args2)).unwrap();
    let next_page_elapsed = next_page_started.elapsed();

    assert_eq!(first_page.len(), 50);
    assert_eq!(second_page.len(), 50);

    println!(
        "upsert_many: {ROWS} rows ({NOTE_BYTES} bytes of notes each) in {write_elapsed:?} \
         ({:.0} rows/s)",
        ROWS as f64 / write_elapsed.as_secs_f64(),
    );
    println!("page_after, first page of 50: {read_elapsed:?}");
    println!("page_after, next page of 50, from a cursor: {next_page_elapsed:?}");
}
