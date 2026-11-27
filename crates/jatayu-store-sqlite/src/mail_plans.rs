//! The query plans mail's hot reads depend on, asked of the bundled SQLite.
//!
//! Each of these was once a walk of a whole mailbox or account that an index
//! or a join order turned into a seek. A plan is the planner's to change --
//! a new SQLite, statistics -- and nothing about the answers would say so,
//! only the time they take, so the plans themselves are what is checked.
//! The SQL here is the shape each query in `jatayu-store-sql`'s mail module
//! runs; a change to one there is a reason to look at its twin here.

use super::SqliteStore;
use jatayu_core::crypto::{Cipher, NullCipher};
use jatayu_core::store::StoreContext;
use jatayu_store_sql::SqlStore;
use jatayu_store_sql::conn::Value;
use std::sync::Arc;

fn store(dir: &std::path::Path) -> SqlStore {
    let cipher: Arc<dyn Cipher> = Arc::new(NullCipher);
    SqliteStore::open(StoreContext::new(dir, cipher)).unwrap()
}

/// The planner's account of `sql`, one line per step.
fn plan(store: &SqlStore, sql: &str, args: &[Value]) -> String {
    store
        .with_read(|c| c.query(&format!("EXPLAIN QUERY PLAN {sql}"), args))
        .unwrap()
        .iter()
        .map(|row| row.text(3).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

fn text(s: &str) -> Value {
    Value::Text(s.to_string())
}

#[test]
fn pending_bodies_reads_only_what_is_pending() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let plan = plan(
        &store,
        "SELECT m.id, mm.uid, m.data FROM mail_messages m
         CROSS JOIN message_mailboxes mm
         WHERE mm.message_id = m.id AND mm.mailbox_id = ?1 AND m.pack_len = 0
         ORDER BY m.date_us DESC LIMIT 50 OFFSET 0",
        &[text("mailbox")],
    );
    assert!(plan.contains("mail_messages_pending_bodies"), "{plan}");
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
}

#[test]
fn a_window_of_an_accounts_mail_is_a_range_of_one_index() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let plan = plan(
        &store,
        "SELECT m.id, m.data FROM mail_messages m
         WHERE m.account_id = ?1 AND m.date_us >= ?2 AND m.date_us < ?3
           AND NOT EXISTS (
               SELECT 1 FROM message_mailboxes mm
               JOIN mailboxes mb ON mb.id = mm.mailbox_id
               WHERE mm.message_id = m.id AND mb.role = 'spam'
           )
         ORDER BY m.date_us, m.id",
        &[text("account"), Value::Int(0), Value::Int(1)],
    );
    assert!(plan.contains("mail_messages_by_account_date (account_id=? AND date_us>?"), "{plan}");
}

#[test]
fn category_counts_never_read_a_thread_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let plan = plan(
        &store,
        "SELECT t.category, CAST(COUNT(*) AS BIGINT),
                CAST(COALESCE(SUM(CASE WHEN tm.unread > 0 THEN 1 ELSE 0 END), 0) AS BIGINT)
         FROM thread_mailboxes tm
         JOIN threads t ON t.id = tm.thread_id
         WHERE tm.mailbox_id = ?1 AND t.snoozed_until_us IS NULL
         GROUP BY t.category",
        &[text("mailbox")],
    );
    assert!(plan.contains("COVERING INDEX threads_by_id_snooze_cat"), "{plan}");
}

#[test]
fn a_category_list_passes_over_other_threads_in_the_index() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let plan = plan(
        &store,
        "SELECT tm.thread_id, tm.last_date_us, t.data FROM thread_mailboxes tm
         JOIN threads t ON t.id = tm.thread_id
         WHERE 1=1 AND tm.mailbox_id = ?1 AND t.category = ?2 AND t.snoozed_until_us IS NULL
         ORDER BY tm.last_date_us DESC, tm.thread_id ASC LIMIT 50 OFFSET 0",
        &[text("mailbox"), text("priority")],
    );
    assert!(plan.contains("thread_mailboxes_by_mailbox"), "{plan}");
    assert!(plan.contains("threads_by_id_snooze_cat"), "{plan}");
    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
}

#[test]
fn snoozed_threads_are_found_through_their_own_index() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let plan = plan(
        &store,
        "SELECT tm.thread_id, t.unread FROM threads t CROSS JOIN thread_mailboxes tm
         WHERE tm.thread_id = t.id AND t.snoozed_until_us >= ?2
           AND 1=1 AND tm.mailbox_id = ?1 AND t.snoozed_until_us IS NOT NULL
         ORDER BY tm.last_date_us DESC, tm.thread_id ASC LIMIT 200 OFFSET 0",
        &[text("mailbox"), Value::Int(i64::MIN)],
    );
    assert!(plan.contains("threads_by_snoozed"), "{plan}");
    assert!(!plan.contains("SCAN t"), "{plan}");
}

#[test]
fn several_mailboxes_are_merged_from_each_ones_own_walk() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let arm = |n: usize, i: usize| {
        format!(
            "SELECT thread_id, last_date_us, data FROM (\
             SELECT tm.thread_id AS thread_id, tm.last_date_us AS last_date_us, \
             t.data AS data FROM thread_mailboxes tm JOIN threads t ON t.id = tm.thread_id \
             WHERE tm.mailbox_id = ?{n} AND 1=1 AND t.snoozed_until_us IS NULL \
             ORDER BY tm.last_date_us DESC, tm.thread_id ASC LIMIT 50 OFFSET 0) AS arm{i}"
        )
    };
    let sql = format!(
        "{} UNION ALL {} ORDER BY last_date_us DESC, thread_id ASC LIMIT 50 OFFSET 0",
        arm(1, 0),
        arm(2, 1)
    );
    let plan = plan(&store, &sql, &[text("a"), text("b")]);
    assert_eq!(plan.matches("thread_mailboxes_by_mailbox (mailbox_id=?)").count(), 2, "{plan}");
    assert!(!plan.contains("SCAN tm"), "{plan}");
}
