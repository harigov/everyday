//! The two thread lists: one mailbox's, keyset-paged over the index the
//! plan names by hand, and one account's, by category.

use everyday_core::error::Result;
use everyday_core::id::{AccountId, MailboxId, ThreadId};
use everyday_core::mail::{Category, Thread};
use everyday_core::store::mail::{ThreadFilter, ThreadPage, thread_aad};

use crate::SqlStore;
use crate::conn::{Sql, Value, Where};
use crate::keyset::{Dir, KeyCursor};

/// See [`everyday_core::store::mail::MailStore::list_threads`].
pub(super) fn list_threads(
    store: &SqlStore,
    mailbox: MailboxId,
    filter: &ThreadFilter,
    cursor: Option<&str>,
    limit: u32,
) -> Result<ThreadPage> {
    let mut w = Where::new().eq("tm.mailbox_id", mailbox.to_string());
    match filter.unread {
        Some(true) => w = w.gte("tm.unread", 1i64),
        Some(false) => w = w.eq("tm.unread", 0i64),
        None => {}
    }
    if let Some(category) = filter.category {
        w = w.eq("t.category", category.as_str());
    }
    match filter.snoozed {
        Some(true) => w = w.not_null("t.snoozed_until_us"),
        Some(false) => w = w.is_null("t.snoozed_until_us"),
        None => {}
    }
    let (where_sql, mut args) = w.finish();
    let mut sql = format!(
        "SELECT tm.thread_id, tm.last_date_us, t.data FROM thread_mailboxes tm
         JOIN threads t ON t.id = tm.thread_id WHERE {where_sql}"
    );
    page_and_run(
        store,
        &mut sql,
        &mut args,
        &[("tm.last_date_us", Dir::Desc), ("tm.thread_id", Dir::Asc)],
        cursor,
        limit,
    )
}

/// See [`everyday_core::store::mail::MailStore::threads_in_category`].
pub(super) fn threads_in_category(
    store: &SqlStore,
    account: AccountId,
    category: Category,
    cursor: Option<&str>,
    limit: u32,
) -> Result<ThreadPage> {
    let (where_sql, mut args) = Where::new()
        .eq("account_id", account.to_string())
        .eq("category", category.as_str())
        .finish();
    let mut sql = format!("SELECT id, last_date_us, data FROM threads WHERE {where_sql}");
    page_and_run(
        store,
        &mut sql,
        &mut args,
        &[("last_date_us", Dir::Desc), ("id", Dir::Asc)],
        cursor,
        limit,
    )
}

/// Append the keyset predicate for `order`, run the three-column query
/// `sql` names (`id, last_date_us, data`, in that order, whatever the
/// source table), and decrypt the result into a page. Shared by both
/// listings above, which differ only in their `WHERE` and which table's
/// `id` column they select.
fn page_and_run(
    store: &SqlStore,
    sql: &mut String,
    args: &mut Vec<Value>,
    order: &[(&str, Dir)],
    cursor: Option<&str>,
    limit: u32,
) -> Result<ThreadPage> {
    let cursor = cursor.map(KeyCursor::decode).transpose()?;
    store.page_after(sql, args, order, cursor.as_ref(), limit)?;

    let rows = store.read().query(sql, args)?;
    let mut threads = Vec::with_capacity(rows.len());
    let mut last_key: Option<(i64, String)> = None;
    for row in &rows {
        let thread_id: ThreadId =
            row.text(0)?.parse().map_err(|e: <ThreadId as std::str::FromStr>::Err| {
                everyday_core::error::Error::Invalid(e.to_string())
            })?;
        let last_date_us = row.i64(1)?;
        let data = row.bytes(2)?;
        let thread: Thread = store.unseal(&thread_aad(thread_id), &data)?;
        last_key = Some((last_date_us, thread_id.to_string()));
        threads.push(thread);
    }

    // A page shorter than `limit` is certainly the last one; a full page
    // might or might not be, so a cursor is always handed back for it and
    // the next call simply comes back empty once there truly is no more.
    let next_cursor = if (rows.len() as u32) < limit {
        None
    } else {
        last_key
            .map(|(d, id)| KeyCursor::new(vec![Value::Int(d), Value::Text(id)]).map(|c| c.encode()))
            .transpose()?
    };
    Ok(ThreadPage { threads, next_cursor })
}
