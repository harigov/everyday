//! The thread lists: one mailbox's, keyset-paged over the index the plan
//! names by hand; several mailboxes' merged into one, the same way (the
//! unified inbox); and one account's, by category.

use std::collections::HashSet;

use jatayu_core::error::Result;
use jatayu_core::id::{AccountId, MailboxId, ThreadId};
use jatayu_core::mail::{Category, Thread};
use jatayu_core::store::mail::{ThreadFilter, ThreadPage, thread_aad};

use super::write::IN_CHUNK;
use crate::conn::{Row, Sql, SqlExt, Value, Where};
use crate::keyset::{Dir, KeyCursor};
use crate::{SqlStore, from_us, to_us};

/// The order every mailbox list runs in: newest activity first, ties broken
/// by thread id. Within one mailbox a thread has one row, so this is total.
const LISTED: &[(&str, Dir)] = &[("tm.last_date_us", Dir::Desc), ("tm.thread_id", Dir::Asc)];

/// The most mailboxes [`list_threads_across`] merges as one `UNION ALL` arm
/// each. SQLite refuses a compound `SELECT` of more than 500 terms (its
/// `SQLITE_MAX_COMPOUND_SELECT`), and every arm is a walk of its own; past
/// this the single `IN (...)` walk is used instead. Asking for every mailbox
/// of an account with hundreds of labels or folders, as linking a reply by
/// its subject does, lands there.
const UNION_ARMS_MOST: usize = 64;

/// See [`jatayu_core::store::mail::MailStore::list_threads`]: the
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

/// See [`jatayu_core::store::mail::MailStore::list_threads_across`].
///
/// One mailbox walks `thread_mailboxes (mailbox_id, last_date_us DESC,
/// thread_id)` straight down, past the cursor, and stops at `limit`.
///
/// Several are that same walk once per mailbox, each stopping at `limit`,
/// merged by date in one `UNION ALL` and cut to `limit` again -- the same
/// rows, in the same order, that widening `tm.mailbox_id = ?` to `IN (...)`
/// over the same keyset finds: each mailbox's rows are a run of the merged
/// order, so none of the first `limit` overall can be past the first `limit`
/// of its own mailbox. The `IN` form read and sorted every row of every
/// mailbox, sealed thread and all, to keep fifty of them; it is still what
/// runs for more than [`UNION_ARMS_MOST`] mailboxes.
pub(super) fn list_threads_across(
    store: &SqlStore,
    mailboxes: &[MailboxId],
    filter: &ThreadFilter,
    cursor: Option<&str>,
    limit: u32,
) -> Result<ThreadPage> {
    if mailboxes.is_empty() {
        return Ok(ThreadPage::default());
    }
    let cursor = cursor.map(KeyCursor::decode).transpose()?;
    // A mailbox named twice is one mailbox, as it is to `IN (...)`.
    let mut distinct = HashSet::with_capacity(mailboxes.len());
    let mailboxes: Vec<MailboxId> =
        mailboxes.iter().copied().filter(|m| distinct.insert(*m)).collect();
    let (sql, args) = match mailboxes.as_slice() {
        [] => return Ok(ThreadPage::default()),
        // One mailbox, or more than one `UNION ALL` should hold: the single
        // keyset walk over `tm.mailbox_id = ?` or `IN (...)`, whose page is
        // the same rows in the same order.
        all if all.len() == 1 || all.len() > UNION_ARMS_MOST => {
            let (where_sql, mut args) = listed_in(all, filter).expect("mailboxes").finish();
            let mut sql = format!(
                "SELECT tm.thread_id, tm.last_date_us, t.data FROM thread_mailboxes tm
                 JOIN threads t ON t.id = tm.thread_id WHERE {where_sql}"
            );
            store.page_after(&mut sql, &mut args, LISTED, cursor.as_ref(), limit)?;
            (sql, args)
        }
        many => {
            // The filter's own placeholders come first, and every arm names
            // them again rather than binding a copy each.
            let (filter_sql, mut args) = filtered(Where::new(), filter).finish();
            let mut arms = Vec::with_capacity(many.len());
            for (i, mailbox) in many.iter().enumerate() {
                args.push(Value::Text(mailbox.to_string()));
                let mut arm = format!(
                    "SELECT tm.thread_id AS thread_id, tm.last_date_us AS last_date_us, \
                     t.data AS data FROM thread_mailboxes tm \
                     JOIN threads t ON t.id = tm.thread_id \
                     WHERE tm.mailbox_id = ?{} AND {filter_sql}",
                    args.len()
                );
                store.page_after(&mut arm, &mut args, LISTED, cursor.as_ref(), limit)?;
                arms.push(format!("SELECT thread_id, last_date_us, data FROM ({arm}) AS arm{i}"));
            }
            let mut sql = arms.join(" UNION ALL ");
            sql.push_str(" ORDER BY last_date_us DESC, thread_id ASC");
            sql.push_str(&store.dialect.limit_offset(Some(limit), 0));
            (sql, args)
        }
    };
    let rows = store.read().query(&sql, &args)?;
    decode_page(store, &rows, limit)
}

/// What makes a thread one of `mailboxes`' listed under `filter`, over
/// `thread_mailboxes tm JOIN threads t`. `None` for no mailboxes at all:
/// `Where::in_list` treats an empty list as no condition, every mailbox in the
/// vault, which is the opposite of what an empty selection means here.
fn listed_in(mailboxes: &[MailboxId], filter: &ThreadFilter) -> Option<Where> {
    let w = match mailboxes {
        [] => return None,
        [one] => Where::new().eq("tm.mailbox_id", one.to_string()),
        many => Where::new().in_list("tm.mailbox_id", many.iter().map(|m| m.to_string())),
    };
    Some(filtered(w, filter))
}

/// `filter`'s own conditions, over `thread_mailboxes tm JOIN threads t`,
/// added to `w`.
fn filtered(mut w: Where, filter: &ThreadFilter) -> Where {
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
    w
}

/// See [`jatayu_core::store::mail::MailStore::listed_unread`].
///
/// [`list_threads`]' own `WHERE` and order, selecting the clear `t.unread`
/// (which every write of a thread keeps equal to its sealed
/// `unread_count`) instead of the sealed row, all on one reader. A list of
/// only snoozed threads is driven from `threads_by_snoozed` instead of
/// down the mailbox: snoozed threads are a handful, and walking the mailbox
/// to find them visited every row it has. The extra `>=` against the
/// smallest value is true of every non-null date, so it changes no answer;
/// it is what lets the index be used for the `IS NOT NULL` beside it.
pub(super) fn listed_unread(
    store: &SqlStore,
    lists: &[(MailboxId, ThreadFilter)],
    limit: u32,
) -> Result<Vec<Vec<(ThreadId, u32)>>> {
    let mut queries = Vec::with_capacity(lists.len());
    for (mailbox, filter) in lists {
        let (where_sql, mut args) = listed_in(&[*mailbox], filter).expect("one mailbox").finish();
        let mut sql = if filter.snoozed == Some(true) {
            args.push(Value::Int(i64::MIN));
            format!(
                "SELECT tm.thread_id, t.unread FROM threads t CROSS JOIN thread_mailboxes tm
                 WHERE tm.thread_id = t.id AND t.snoozed_until_us >= ?{} AND {where_sql}",
                args.len()
            )
        } else {
            format!(
                "SELECT tm.thread_id, t.unread FROM thread_mailboxes tm
                 JOIN threads t ON t.id = tm.thread_id WHERE {where_sql}"
            )
        };
        store.page_after(&mut sql, &mut args, LISTED, None, limit)?;
        queries.push((sql, args));
    }
    let mut conn = store.read();
    queries
        .iter()
        .map(|(sql, args)| {
            conn.query(sql, args)?
                .iter()
                .map(|row| Ok((thread_id(row, 0)?, row.i64(1)? as u32)))
                .collect()
        })
        .collect()
}

/// See [`jatayu_core::store::mail::MailStore::list_thread_keys`]:
/// [`list_threads`]' query, cursor and all, with no sealed column read.
pub(super) fn list_thread_keys(
    store: &SqlStore,
    mailbox: MailboxId,
    filter: &ThreadFilter,
    cursor: Option<&str>,
    limit: u32,
) -> Result<jatayu_core::store::mail::ThreadKeyPage> {
    let cursor = cursor.map(KeyCursor::decode).transpose()?;
    let (where_sql, mut args) = listed_in(&[mailbox], filter).expect("one mailbox").finish();
    let mut sql = format!(
        "SELECT tm.thread_id, tm.last_date_us FROM thread_mailboxes tm
         JOIN threads t ON t.id = tm.thread_id WHERE {where_sql}"
    );
    store.page_after(&mut sql, &mut args, LISTED, cursor.as_ref(), limit)?;
    let rows = store.read().query(&sql, &args)?;
    let mut keys = Vec::with_capacity(rows.len());
    for row in &rows {
        keys.push((thread_id(row, 0)?, from_us(row.i64(1)?)));
    }
    let next = next_cursor(rows.len(), limit, rows.last().map(|r| (r.i64(1), thread_id(r, 0))))?;
    Ok((keys, next))
}

/// See [`jatayu_core::store::mail::MailStore::threads_by_ids`].
pub(super) fn threads_by_ids(store: &SqlStore, ids: &[ThreadId]) -> Result<Vec<Thread>> {
    let mut out = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(IN_CHUNK) {
        let (where_sql, args) =
            Where::new().in_list("id", chunk.iter().map(|t| t.to_string())).finish();
        let rows = store
            .read()
            .records(&format!("SELECT id, data FROM threads WHERE {where_sql}"), &args)?;
        out.extend(store.collect::<Thread, _>(rows, thread_aad)?);
    }
    Ok(out)
}

/// See [`jatayu_core::store::mail::MailStore::threads_listed`].
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
            listed.insert(thread_id(row, 0)?);
        }
    }
    Ok(listed.into_iter().collect())
}

/// See [`jatayu_core::store::mail::MailStore::threads_in_category`].
pub(super) fn threads_in_category(
    store: &SqlStore,
    account: AccountId,
    category: Category,
    cursor: Option<&str>,
    limit: u32,
) -> Result<ThreadPage> {
    let cursor = cursor.map(KeyCursor::decode).transpose()?;
    let (where_sql, mut args) = Where::new()
        .eq("account_id", account.to_string())
        .eq("category", category.as_str())
        .finish();
    let mut sql = format!("SELECT id, last_date_us, data FROM threads WHERE {where_sql}");
    store.page_after(
        &mut sql,
        &mut args,
        &[("last_date_us", Dir::Desc), ("id", Dir::Asc)],
        cursor.as_ref(),
        limit,
    )?;
    let rows = store.read().query(&sql, &args)?;
    decode_page(store, &rows, limit)
}

/// Decrypt the rows of a three-column listing (`id, last_date_us, data`, in
/// that order, whatever the source table) into a page. Shared by every
/// listing above, which differ only in their `WHERE` and which table's `id`
/// column they select.
///
/// A thread id already on this page is skipped rather than decrypted and
/// returned twice. Only [`list_threads_across`] can produce one -- a thread
/// filed in two of the mailboxes it was asked about, which is one
/// `thread_mailboxes` row each -- and only within a page can it be caught
/// here: the second row may well land on a later page, which is why that
/// method's own contract leaves dedupe across pages to its caller. The
/// skipped row still moves the cursor, so paging past it neither repeats
/// nor loses anything else.
fn decode_page(store: &SqlStore, rows: &[Row], limit: u32) -> Result<ThreadPage> {
    let mut threads = Vec::with_capacity(rows.len());
    let mut seen: HashSet<ThreadId> = HashSet::with_capacity(rows.len());
    for row in rows {
        let thread_id = thread_id(row, 0)?;
        if !seen.insert(thread_id) {
            continue;
        }
        let last_date_us = row.i64(1)?;
        let data = row.bytes(2)?;
        let mut thread: Thread = store.unseal(&thread_aad(thread_id), &data)?;
        // The date that placed the row here, so what it says cannot disagree
        // with the rows around it. A draft or mail filed elsewhere can make the
        // thread's own newer, without having moved it.
        thread.last_date = from_us(last_date_us);
        threads.push(thread);
    }
    let next_cursor =
        next_cursor(rows.len(), limit, rows.last().map(|r| (r.i64(1), thread_id(r, 0))))?;
    Ok(ThreadPage { threads, next_cursor })
}

/// The cursor after a page of `rows` rows that ended on `last`. A page
/// shorter than `limit` is certainly the last one; a full page might or
/// might not be, so a cursor is always handed back for it and the next call
/// simply comes back empty once there truly is no more.
fn next_cursor(
    rows: usize,
    limit: u32,
    last: Option<(Result<i64>, Result<ThreadId>)>,
) -> Result<Option<String>> {
    if (rows as u32) < limit {
        return Ok(None);
    }
    let Some((date, id)) = last else { return Ok(None) };
    let cursor = KeyCursor::new(vec![Value::Int(date?), Value::Text(id?.to_string())])?;
    Ok(Some(cursor.encode()))
}

fn thread_id(row: &Row, i: usize) -> Result<ThreadId> {
    row.text(i)?.parse().map_err(|e: <ThreadId as std::str::FromStr>::Err| {
        jatayu_core::error::Error::Invalid(e.to_string())
    })
}
