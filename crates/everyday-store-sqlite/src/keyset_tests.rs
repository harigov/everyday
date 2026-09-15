//! `page_after` against a real database.
//!
//! Everything about the shape of the generated SQL is checked once, for
//! both dialects, in `everyday_store_sql::keyset`'s own unit tests, without
//! a database at all. What only a real database can check is that the SQL
//! actually *works* -- that a page boundary landing inside a tie does not
//! duplicate or drop a row, which is the one failure mode text-comparison
//! cannot see.

use super::SqliteStore;
use everyday_core::crypto::{AeadCipher, Cipher, SecretKey};
use everyday_core::id::TaskId;
use everyday_core::store::StoreContext;
use everyday_core::store::tasks::TaskStore;
use everyday_core::task::Task;
use everyday_store_sql::conn::Value;
use everyday_store_sql::keyset::{Dir, KeyCursor};
use std::path::Path;
use std::sync::Arc;

/// A store of its own, rather than reaching into `tests::ctx` -- these
/// checks are about `page_after`, not about the encrypted/plain split
/// that module's helper exists for, so they take the simplest cipher that
/// still exercises real sealing.
fn ctx(root: &Path) -> StoreContext {
    StoreContext::new(
        root,
        Arc::new(AeadCipher::new(&SecretKey::from_bytes([4u8; 32]))) as Arc<dyn Cipher>,
    )
}

/// Ninety-seven rows in groups of ten sharing one `created_us`, paged
/// through seven at a time, so a page boundary is forced to land inside a
/// tie more than once. `tasks` is a real, already-migrated table with a
/// real clear column to tie on -- there is no bespoke schema standing in
/// for a mailbox here, because `page_after` does not know or care that it
/// is not one.
#[test]
fn every_row_is_paged_exactly_once_despite_ties() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path())).unwrap();

    let base = jiff::Timestamp::now();
    let tasks: Vec<Task> = (0..97u32)
        .map(|i| {
            let mut t = Task::new(format!("task {i}"));
            t.created_at = base + jiff::SignedDuration::from_secs(i64::from(i / 10));
            t
        })
        .collect();
    store.put_tasks(&tasks).unwrap();

    let mut seen: Vec<TaskId> = Vec::new();
    let mut cursor: Option<KeyCursor> = None;
    loop {
        let mut sql = "SELECT id, created_us FROM tasks WHERE 1=1".to_string();
        let mut args = Vec::new();
        store
            .page_after(
                &mut sql,
                &mut args,
                &[("created_us", Dir::Asc), ("id", Dir::Asc)],
                cursor.as_ref(),
                7,
            )
            .unwrap();
        let rows = store.with_read(|c| c.query(&sql, &args)).unwrap();
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            seen.push(row.text(0).unwrap().parse().unwrap());
        }
        let last = rows.last().unwrap();
        cursor = Some(
            KeyCursor::new(vec![
                Value::Int(last.i64(1).unwrap()),
                Value::Text(last.text(0).unwrap()),
            ])
            .unwrap(),
        );
        if rows.len() < 7 {
            break;
        }
    }

    assert_eq!(seen.len(), 97, "every row must be seen, and none twice");
    let mut seen_sorted = seen.clone();
    seen_sorted.sort();
    let mut expected: Vec<TaskId> = tasks.iter().map(|t| t.id).collect();
    expected.sort();
    assert_eq!(seen_sorted, expected, "the set of rows paged must be exactly the set written");

    // And the order within a page, and across the page boundary, must agree
    // with `(created_us, id)` ascending -- ties broken by id.
    let mut by_key: Vec<(i64, String)> =
        tasks.iter().map(|t| (t.created_at.as_microsecond(), t.id.to_string())).collect();
    by_key.sort();
    let expected_order: Vec<String> = by_key.into_iter().map(|(_, id)| id).collect();
    let seen_order: Vec<String> = seen.iter().map(TaskId::to_string).collect();
    assert_eq!(
        seen_order, expected_order,
        "paging must preserve the sort order across page boundaries"
    );
}

/// A cursor built from a page that does not exist -- past the end of the
/// table -- is not an error; it is simply an empty next page, the same
/// answer offset paging gives for an offset past the end.
#[test]
fn paging_past_the_end_is_an_empty_page_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path())).unwrap();
    store.put_task(&Task::new("the only task")).unwrap();

    let last = store.list_tasks(&Default::default()).unwrap().into_iter().next().unwrap();
    let cursor =
        KeyCursor::new(vec![Value::Int(i64::MAX), Value::Text(last.id.to_string())]).unwrap();

    let mut sql = "SELECT id FROM tasks WHERE 1=1".to_string();
    let mut args = Vec::new();
    store
        .page_after(
            &mut sql,
            &mut args,
            &[("created_us", Dir::Asc), ("id", Dir::Asc)],
            Some(&cursor),
            10,
        )
        .unwrap();
    let rows = store.with_read(|c| c.query(&sql, &args)).unwrap();
    assert!(rows.is_empty());
}

/// The same query text `everyday_store_sql::keyset`'s own unit tests
/// predict, run against SQLite specifically -- the placeholder numbering
/// this crate's `Dialect::bind` rewrites for Postgres is left untouched
/// here, since a SQLite connection binds `?N` exactly as written.
#[test]
fn placeholder_numbering_survives_a_where_clause_built_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(ctx(dir.path())).unwrap();
    let mut a = Task::new("a");
    a.priority = everyday_core::task::Priority::High;
    let mut b = Task::new("b");
    b.priority = everyday_core::task::Priority::Low;
    store.put_tasks(&[a.clone(), b.clone()]).unwrap();

    // `priority = ?1` already bound, so the keyset predicate below must
    // continue at `?2` -- exactly the case
    // `placeholder_numbering_continues_from_where_the_query_already_reached`
    // checks without a database, checked here with one.
    let mut sql = "SELECT id FROM tasks WHERE priority = ?1".to_string();
    let mut args = vec![Value::Int(everyday_core::task::Priority::High.rank())];
    store
        .page_after(&mut sql, &mut args, &[("created_us", Dir::Asc), ("id", Dir::Asc)], None, 10)
        .unwrap();
    let rows = store.with_read(|c| c.query(&sql, &args)).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].text(0).unwrap(), a.id.to_string());
}
