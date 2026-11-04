//! The thread lists: one mailbox's, keyset-paged over the index the plan
//! names by hand; several mailboxes' merged into one, the same way (the
//! unified inbox); and one account's, by category.

use std::collections::HashSet;

use everyday_core::error::Result;
use everyday_core::id::{AccountId, MailboxId, ThreadId};
use everyday_core::mail::{Category, Thread};
use everyday_core::store::mail::{ThreadFilter, ThreadPage, thread_aad};

use super::write::IN_CHUNK;
use crate::conn::{Sql, Value, Where};
use crate::keyset::{Dir, KeyCursor};
use crate::{SqlStore, from_us, to_us};

/// See [`everyday_core::store::mail::MailStore::list_threads`]: the
/// one-mailbox case of [`list_threads_across`], and written as exactly that
/// so the two can never disagree about a filter or the order.
pub(super) fn list_threads(
    store: &SqlStore,
    mailbox: MailboxId,
    filter: &ThreadFilter,
    cursor: Option<&str>,
    limit: u32,
) -> Result<ThreadPage> {
    list_threads_across(store, &[mailbox], filter, cursor, limit)
}

/// See [`everyday_core::store::mail::MailStore::list_threads_across`].
///
/// The same query [`list_threads`] always ran, with `tm.mailbox_id = ?`
/// widened to `IN (...)`: the keyset is still `(tm.last_date_us DESC,
/// tm.thread_id ASC)`, so rows from every listed mailbox interleave by date
/// exactly as one mailbox's rows already did. One mailbox is still written
/// `=` rather than a one-element `IN`, so the query plan the inbox has
/// always had -- straight down `thread_mailboxes (mailbox_id, last_date_us
/// DESC, thread_id)` -- is the one it keeps.
pub(super) fn list_threads_across(
    store: &SqlStore,
    mailboxes: &[MailboxId],
    filter: &ThreadFilter,
    cursor: Option<&str>,
    limit: u32,
) -> Result<ThreadPage> {
    let Some(w) = listed_in(mailboxes, filter) else {
        return Ok(ThreadPage::default());
    };
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

/// What makes a thread one of `mailboxes`' listed under `filter`, over
/// `thread_mailboxes tm JOIN threads t`. `None` for no mailboxes at all:
/// `Where::in_list` treats an empty list as no condition, every mailbox in the
/// vault, which is the opposite of what an empty selection means here.
fn listed_in(mailboxes: &[MailboxId], filter: &ThreadFilter) -> Option<Where> {
    let mut w = match mailboxes {
        [] => return None,
        [one] => Where::new().eq("tm.mailbox_id", one.to_string()),
        many => Where::new().in_list("tm.mailbox_id", many.iter().map(|m| m.to_string())),
    };
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
    if let Some(before) = filter.last_before {
        w = w.lt("t.last_date_us", to_us(before));
    }
    if let Some(starred) = filter.starred {
        w = w.eq("tm.starred", starred);
    }
    if let Some(attached) = filter.has_attachments {
        w = w.eq("tm.has_attachments", attached);
    }
    Some(w)
}

/// See [`everyday_core::store::mail::MailStore::threads_listed`].
pub(super) fn threads_listed(
    store: &SqlStore,
    mailboxes: &[MailboxId],
    filter: &ThreadFilter,
    threads: &[ThreadId],
) -> Result<Vec<ThreadId>> {
    let mut listed = HashSet::new();
    for chunk in threads.chunks(IN_CHUNK) {
        let Some(w) = listed_in(mailboxes, filter) else { break };
        let (where_sql, args) =
            w.in_list("tm.thread_id", chunk.iter().map(|t| t.to_string())).finish();
        let rows = store.read().query(
            &format!(
                "SELECT DISTINCT tm.thread_id FROM thread_mailboxes tm
                 JOIN threads t ON t.id = tm.thread_id WHERE {where_sql}"
            ),
            &args,
        )?;
        for row in &rows {
            listed.insert(
                row.text(0)?
                    .parse::<ThreadId>()
                    .map_err(|e| everyday_core::error::Error::Invalid(e.to_string()))?,
            );
        }
    }
    Ok(listed.into_iter().collect())
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
/// source table), and decrypt the result into a page. Shared by every
/// listing above, which differ only in their `WHERE` and which table's
/// `id` column they select.
///
/// A thread id already on this page is skipped rather than decrypted and
/// returned twice. Only [`list_threads_across`] can produce one -- a thread
/// filed in two of the mailboxes it was asked about, which is one
/// `thread_mailboxes` row each -- and only within a page can it be caught
/// here: the second row may well land on a later page, which is why that
/// method's own contract leaves dedupe across pages to its caller. The
/// skipped row still moves the cursor, so paging past it neither repeats
/// nor loses anything else.
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
    let mut seen: HashSet<ThreadId> = HashSet::with_capacity(rows.len());
    let mut last_key: Option<(i64, String)> = None;
    for row in &rows {
        let thread_id: ThreadId =
            row.text(0)?.parse().map_err(|e: <ThreadId as std::str::FromStr>::Err| {
                everyday_core::error::Error::Invalid(e.to_string())
            })?;
        let last_date_us = row.i64(1)?;
        last_key = Some((last_date_us, thread_id.to_string()));
        if !seen.insert(thread_id) {
            continue;
        }
        let data = row.bytes(2)?;
        let mut thread: Thread = store.unseal(&thread_aad(thread_id), &data)?;
        // The date that placed the row here, so what it says cannot disagree
        // with the rows around it. A draft or mail filed elsewhere can make the
        // thread's own newer, without having moved it.
        thread.last_date = from_us(last_date_us);
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
