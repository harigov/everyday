//! The write half of the mail domain: ingest, flag and label changes,
//! removal, and the two kinds of reset a `UIDVALIDITY` change or a deleted
//! mailbox both boil down to -- plus the one function all four share,
//! [`recompute_thread`], which is what keeps a [`Thread`]'s own aggregates
//! and its per-mailbox rows in `thread_mailboxes` from ever drifting apart
//! from the `messages` and `message_mailboxes` rows they are counted from.
//!
//! # Why a free function, not a method
//!
//! [`MailStore`] is one trait, so its `impl` has to be one block (Rust
//! refuses two `impl MailStore for SqlStore` blocks in the same crate) --
//! which is exactly the file-length problem splitting the module is meant to
//! solve. The way out is the one every other multi-table domain in this
//! crate that needed it already took ([`crate::purpose::forget_purposes`],
//! [`crate::purpose::set_purpose`]): the heavy logic lives here as plain
//! functions taking `&SqlStore` and, where they share a transaction with
//! sibling writes, `&mut dyn Sql` directly, and [`super::MailStore`]'s
//! `impl` block is a one-line call into each.

use std::collections::{BTreeMap, BTreeSet};

use everyday_core::error::{Error, Result};
use everyday_core::id::{AccountId, MailMessageId, MailboxId, ThreadId};
use everyday_core::mail::{Address, Invite, Message, MessageFlags, Thread};
use everyday_core::store::mail::{IngestMessage, message_aad, thread_aad};

use crate::conn::{Sql, SqlExt, Value};
use crate::record::upsert_stmt;
use crate::{SqlStore, from_us, placeholders, vals};

/// A batch of [`IngestMessage`]s closes at this many rows -- "a few hundred,
/// committed per batch," per the plan's own words for the first sync pass.
/// Unlike [`crate::record::upsert_batched`] this does not also cap by
/// bytes: each batch here does more work per row (a `message_mailboxes`
/// row, and a recompute per distinct thread touched) than a plain upsert
/// does, so the row count alone already bounds how long one batch holds the
/// writer.
const INGEST_BATCH_ROWS: usize = 500;

/// See [`everyday_core::store::mail::MailStore::ingest`].
pub(super) fn ingest(
    store: &SqlStore,
    _account: AccountId,
    messages: Vec<IngestMessage>,
) -> Result<()> {
    if messages.is_empty() {
        return Ok(());
    }
    for batch in messages.chunks(INGEST_BATCH_ROWS) {
        let mut conn = store.write();
        let mut tx = conn.begin()?;
        let mut touched: BTreeMap<ThreadId, Vec<&Message>> = BTreeMap::new();
        for im in batch {
            let sealed = store.seal(&message_aad(im.message.id), &im.message)?;
            let (sql, args) = upsert_stmt(&im.message, sealed);
            tx.execute(&sql, &args)?;
            tx.execute(
                "INSERT INTO message_mailboxes (message_id, mailbox_id, uid) VALUES (?1, ?2, ?3)
                 ON CONFLICT (message_id, mailbox_id) DO UPDATE SET uid = ?3",
                &vals![im.message.id.to_string(), im.mailbox.to_string(), i64::from(im.uid)],
            )?;
            touched.entry(im.message.thread_id).or_default().push(&im.message);
        }
        for (thread_id, hints) in &touched {
            recompute_thread(store, tx.as_mut(), *thread_id, hints)?;
        }
        tx.commit()?;
    }
    Ok(())
}

/// See [`everyday_core::store::mail::MailStore::update_flags`].
pub(super) fn update_flags(
    store: &SqlStore,
    mailbox: MailboxId,
    uid: u32,
    flags: MessageFlags,
) -> Result<()> {
    let mut conn = store.write();
    let mut tx = conn.begin()?;
    let Some((mid, mut message)) = message_at(store, tx.as_mut(), mailbox, uid)? else {
        return Ok(()); // a uid this store never ingested: a no-op, per the trait's docs
    };
    let thread_id = message.thread_id;
    message.flags = flags;
    let sealed = store.seal(&message_aad(mid), &message)?;
    let (sql, args) = upsert_stmt(&message, sealed);
    tx.execute(&sql, &args)?;
    recompute_thread(store, tx.as_mut(), thread_id, &[])?;
    tx.commit()
}

/// See [`everyday_core::store::mail::MailStore::update_labels`].
pub(super) fn update_labels(
    store: &SqlStore,
    mailbox: MailboxId,
    uid: u32,
    labels: Vec<String>,
) -> Result<()> {
    let mut conn = store.write();
    let mut tx = conn.begin()?;
    let Some((mid, mut message)) = message_at(store, tx.as_mut(), mailbox, uid)? else {
        return Ok(());
    };
    let thread_id = message.thread_id;
    message.labels = labels;
    let sealed = store.seal(&message_aad(mid), &message)?;
    let (sql, args) = upsert_stmt(&message, sealed);
    tx.execute(&sql, &args)?;
    // Labels do not change a thread's counts or dates, but the plan is
    // explicit that a label change "updates the thread rows" -- recomputing
    // is what keeps that promise true even once a category rule someday
    // reads a label, rather than leaving today's cheap case looking correct
    // by accident.
    recompute_thread(store, tx.as_mut(), thread_id, &[])?;
    tx.commit()
}

/// The message filed as `uid` in `mailbox`, decrypted, or `None`.
fn message_at(
    store: &SqlStore,
    tx: &mut dyn Sql,
    mailbox: MailboxId,
    uid: u32,
) -> Result<Option<(MailMessageId, Message)>> {
    let row = tx.query_opt(
        "SELECT m.id, m.data FROM mail_messages m
         JOIN message_mailboxes mm ON mm.message_id = m.id
         WHERE mm.mailbox_id = ?1 AND mm.uid = ?2",
        &vals![mailbox.to_string(), i64::from(uid)],
    )?;
    let Some(row) = row else { return Ok(None) };
    let mid: MailMessageId = row
        .text(0)?
        .parse()
        .map_err(|e: <MailMessageId as std::str::FromStr>::Err| Error::Invalid(e.to_string()))?;
    let message: Message = store.unseal(&message_aad(mid), &row.bytes(1)?)?;
    Ok(Some((mid, message)))
}

/// The message named `id`, decrypted, or `None` -- the by-id counterpart of
/// [`message_at`], for the optimistic writes below that are keyed by a
/// message id the caller already holds rather than by a `(mailbox, uid)`
/// pair the sync engine would have to have confirmed first.
fn message_by_id(store: &SqlStore, tx: &mut dyn Sql, id: MailMessageId) -> Result<Option<Message>> {
    let Some(row) =
        tx.query_opt("SELECT data FROM mail_messages WHERE id = ?1", &vals![id.to_string()])?
    else {
        return Ok(None);
    };
    Ok(Some(store.unseal(&message_aad(id), &row.bytes(0)?)?))
}

/// See [`everyday_core::store::mail::MailStore::set_message_flags`].
pub(super) fn set_message_flags(
    store: &SqlStore,
    id: MailMessageId,
    flags: MessageFlags,
) -> Result<()> {
    let mut conn = store.write();
    let mut tx = conn.begin()?;
    let Some(mut message) = message_by_id(store, tx.as_mut(), id)? else {
        return Ok(()); // deleted since the caller last looked: nothing to revert
    };
    let thread_id = message.thread_id;
    message.flags = flags;
    let sealed = store.seal(&message_aad(id), &message)?;
    let (sql, args) = upsert_stmt(&message, sealed);
    tx.execute(&sql, &args)?;
    recompute_thread(store, tx.as_mut(), thread_id, &[])?;
    tx.commit()
}

/// See [`everyday_core::store::mail::MailStore::set_message_labels`].
pub(super) fn set_message_labels(
    store: &SqlStore,
    id: MailMessageId,
    labels: Vec<String>,
) -> Result<()> {
    let mut conn = store.write();
    let mut tx = conn.begin()?;
    let Some(mut message) = message_by_id(store, tx.as_mut(), id)? else {
        return Ok(());
    };
    let thread_id = message.thread_id;
    message.labels = labels;
    let sealed = store.seal(&message_aad(id), &message)?;
    let (sql, args) = upsert_stmt(&message, sealed);
    tx.execute(&sql, &args)?;
    recompute_thread(store, tx.as_mut(), thread_id, &[])?;
    tx.commit()
}

/// See [`everyday_core::store::mail::MailStore::set_message_invite`]. No
/// [`recompute_thread`] call, unlike [`set_message_flags`] and
/// [`set_message_labels`] just above: an invitation is not one of the
/// aggregates a thread row keeps.
pub(super) fn set_message_invite(
    store: &SqlStore,
    id: MailMessageId,
    invite: Option<Invite>,
) -> Result<()> {
    let mut conn = store.write();
    let mut tx = conn.begin()?;
    let Some(mut message) = message_by_id(store, tx.as_mut(), id)? else {
        return Ok(()); // deleted since the caller last looked: nothing to update
    };
    message.invite = invite;
    let sealed = store.seal(&message_aad(id), &message)?;
    let (sql, args) = upsert_stmt(&message, sealed);
    tx.execute(&sql, &args)?;
    tx.commit()
}

/// See [`everyday_core::store::mail::MailStore::hide_thread_from_mailbox`].
pub(super) fn hide_thread_from_mailbox(
    store: &SqlStore,
    thread: ThreadId,
    mailbox: MailboxId,
) -> Result<()> {
    let mut conn = store.write();
    let mut tx = conn.begin()?;
    tx.execute(
        "DELETE FROM thread_mailboxes WHERE thread_id = ?1 AND mailbox_id = ?2",
        &vals![thread.to_string(), mailbox.to_string()],
    )?;
    tx.commit()
}

/// See [`everyday_core::store::mail::MailStore::restore_thread_mailboxes`].
///
/// Just [`recompute_thread_mailboxes`] in its own transaction:
/// [`hide_thread_from_mailbox`] never touched `message_mailboxes`, so
/// recomputing from it reconstructs exactly the row that was hidden, with
/// no undo snapshot to have kept anywhere.
pub(super) fn restore_thread_mailboxes(store: &SqlStore, thread: ThreadId) -> Result<()> {
    let mut conn = store.write();
    let mut tx = conn.begin()?;
    recompute_thread_mailboxes(tx.as_mut(), thread)?;
    tx.commit()
}

/// See [`everyday_core::store::mail::MailStore::set_thread_snoozed_until`].
pub(super) fn set_thread_snoozed_until(
    store: &SqlStore,
    thread: ThreadId,
    until: Option<jiff::Timestamp>,
) -> Result<()> {
    let mut conn = store.write();
    let mut tx = conn.begin()?;
    let Some(row) =
        tx.query_opt("SELECT data FROM threads WHERE id = ?1", &vals![thread.to_string()])?
    else {
        return Ok(());
    };
    let mut t: Thread = store.unseal(&thread_aad(thread), &row.bytes(0)?)?;
    t.snoozed_until = until;
    let sealed = store.seal(&thread_aad(thread), &t)?;
    let (sql, args) = upsert_stmt(&t, sealed);
    tx.execute(&sql, &args)?;
    tx.commit()
}

/// See [`everyday_core::store::mail::MailStore::merge_threads`].
///
/// Re-seals every message moving out of `others`, not a bare `UPDATE` of
/// the clear `thread_id` column: `Message::thread_id` is also part of the
/// sealed `data` this row carries (the `Record` impl for `Message`, above,
/// writes `thread_id` from the in-memory value on every upsert), so a
/// caller that later decrypts one of these rows -- `thread`,
/// `message_by_uid`, every reader -- must see the same answer the clear
/// column does. [`recompute_thread`] for
/// `keep` and for each of `others` in turn is what then deletes `others`'
/// own rows: with every message moved away, each recompute counts zero
/// messages left under that id and takes the "nothing left of this thread
/// anywhere" branch it already has for an ordinary removal that empties a
/// thread.
pub(super) fn merge_threads(store: &SqlStore, keep: ThreadId, others: &[ThreadId]) -> Result<()> {
    if others.is_empty() {
        return Ok(());
    }
    let mut conn = store.write();
    let mut tx = conn.begin()?;
    for &other in others {
        if other == keep {
            continue; // never asked to merge a thread into itself
        }
        let rows = tx.records(
            "SELECT id, data FROM mail_messages WHERE thread_id = ?1",
            &vals![other.to_string()],
        )?;
        for (id, data) in rows {
            let mid: MailMessageId =
                id.parse().map_err(|e: <MailMessageId as std::str::FromStr>::Err| {
                    Error::Invalid(e.to_string())
                })?;
            let mut message: Message = store.unseal(&message_aad(mid), &data)?;
            message.thread_id = keep;
            let sealed = store.seal(&message_aad(mid), &message)?;
            let (sql, args) = upsert_stmt(&message, sealed);
            tx.execute(&sql, &args)?;
        }
    }
    recompute_thread(store, tx.as_mut(), keep, &[])?;
    for &other in others {
        if other != keep {
            recompute_thread(store, tx.as_mut(), other, &[])?;
        }
    }
    tx.commit()
}

/// See [`everyday_core::store::mail::MailStore::remove_uids`].
pub(super) fn remove_uids(store: &SqlStore, mailbox: MailboxId, uids: &[u32]) -> Result<()> {
    if uids.is_empty() {
        return Ok(());
    }
    let mut conn = store.write();
    let mut tx = conn.begin()?;
    remove_uids_tx(store, tx.as_mut(), mailbox, uids)?;
    tx.commit()
}

/// The shared body of [`remove_uids`], [`reset_mailbox`] and
/// [`delete_mailbox`]: drop every named `(mailbox, uid)` membership, delete
/// any message that no mailbox names any more (body included), and
/// recompute every thread touched.
fn remove_uids_tx(
    store: &SqlStore,
    tx: &mut dyn Sql,
    mailbox: MailboxId,
    uids: &[u32],
) -> Result<()> {
    if uids.is_empty() {
        return Ok(());
    }
    let holes = placeholders(2, uids.len());
    let mut args = vec![Value::Text(mailbox.to_string())];
    args.extend(uids.iter().map(|u| Value::Int(i64::from(*u))));

    let touched: Vec<(String, String)> = tx
        .query(
            &format!(
                "SELECT mm.message_id, m.thread_id FROM message_mailboxes mm
                 JOIN mail_messages m ON m.id = mm.message_id
                 WHERE mm.mailbox_id = ?1 AND mm.uid IN ({holes})"
            ),
            &args,
        )?
        .into_iter()
        .map(|r| Ok((r.text(0)?, r.text(1)?)))
        .collect::<Result<_>>()?;

    tx.execute(
        &format!("DELETE FROM message_mailboxes WHERE mailbox_id = ?1 AND uid IN ({holes})"),
        &args,
    )?;

    let mut orphan_threads: BTreeSet<ThreadId> = BTreeSet::new();
    for (message_id, thread_id) in &touched {
        let remaining = tx.scalar_i64(
            "SELECT COUNT(*) FROM message_mailboxes WHERE message_id = ?1",
            &vals![message_id.clone()],
        )?;
        if remaining == 0 {
            // No mailbox holds this message any more: it, and its body, are
            // gone -- an `EXPUNGE`d message, or the last Gmail label
            // removed.
            tx.execute("DELETE FROM bodies WHERE message_id = ?1", &vals![message_id.clone()])?;
            tx.execute("DELETE FROM mail_messages WHERE id = ?1", &vals![message_id.clone()])?;
        }
        if let Ok(tid) = thread_id.parse::<ThreadId>() {
            orphan_threads.insert(tid);
        }
    }
    for thread_id in orphan_threads {
        recompute_thread(store, tx, thread_id, &[])?;
    }
    Ok(())
}

/// See [`everyday_core::store::mail::MailStore::reset_mailbox`].
///
/// Deliberately **not** built on [`remove_uids_tx`], even though both start
/// by clearing `message_mailboxes` rows: a `UIDVALIDITY` change means the
/// uids this store remembered may now name different messages, not that the
/// messages themselves are gone. `remove_uids_tx` deletes a message once no
/// mailbox names it any more, which is exactly right for a genuine
/// `EXPUNGE` and exactly wrong here -- it would destroy the very rows
/// [`everyday_core::store::mail::MailStore::message_by_message_id_header`]
/// exists to rematch against. So this only forgets the mapping: every
/// `message_mailboxes` row naming this mailbox, and every `thread_mailboxes`
/// row it was the reason for, while the `messages` rows -- and each
/// message's membership in *other* mailboxes -- are left exactly as they
/// were.
pub(super) fn reset_mailbox(store: &SqlStore, mailbox: MailboxId) -> Result<()> {
    let mut conn = store.write();
    let mut tx = conn.begin()?;
    tx.execute("DELETE FROM message_mailboxes WHERE mailbox_id = ?1", &vals![mailbox.to_string()])?;
    tx.execute("DELETE FROM thread_mailboxes WHERE mailbox_id = ?1", &vals![mailbox.to_string()])?;
    tx.execute(
        "UPDATE mailboxes SET uidvalidity = 0, uidnext = 0, highest_modseq = 0 WHERE id = ?1",
        &vals![mailbox.to_string()],
    )?;
    // The sealed copy of the cursors must agree with the clear columns just
    // rewritten, or a caller reading the record back would see cursors that
    // do not match what a fresh sync actually resumes from.
    if let Some(row) =
        tx.query_opt("SELECT data FROM mailboxes WHERE id = ?1", &vals![mailbox.to_string()])?
    {
        let mut mb: everyday_core::mail::Mailbox =
            store.unseal(&everyday_core::store::mail::mailbox_aad(mailbox), &row.bytes(0)?)?;
        mb.uidvalidity = 0;
        mb.uidnext = 0;
        mb.highest_modseq = 0;
        let sealed = store.seal(&everyday_core::store::mail::mailbox_aad(mailbox), &mb)?;
        let (sql, sql_args) = upsert_stmt(&mb, sealed);
        tx.execute(&sql, &sql_args)?;
    }
    tx.commit()
}

/// See [`everyday_core::store::mail::MailStore::delete_mailbox`].
pub(super) fn delete_mailbox(store: &SqlStore, id: MailboxId) -> Result<()> {
    let mut conn = store.write();
    let mut tx = conn.begin()?;
    let uids: Vec<u32> = tx
        .query("SELECT uid FROM message_mailboxes WHERE mailbox_id = ?1", &vals![id.to_string()])?
        .into_iter()
        .map(|r| Ok(r.i64(0)? as u32))
        .collect::<Result<_>>()?;
    remove_uids_tx(store, tx.as_mut(), id, &uids)?;
    tx.execute("DELETE FROM thread_mailboxes WHERE mailbox_id = ?1", &vals![id.to_string()])?;
    tx.execute("DELETE FROM mailboxes WHERE id = ?1", &vals![id.to_string()])?;
    tx.commit()
}

/// Recompute [`Thread`] `thread_id`'s own aggregates and every
/// `thread_mailboxes` row naming it, from the `messages` and
/// `message_mailboxes` rows that currently exist -- the one place all four
/// writers above (ingest, a flag change, a label change, a removal) meet, so
/// the two views of a thread the module docs describe can never drift apart
/// from each other.
///
/// `hints` are messages this same call just wrote, offered so that creating
/// a *new* thread can seed its subject and participants without a second
/// decrypt of anything: the caller already holds the one message that
/// justified minting the thread. An `update_flags` or `remove_uids` call,
/// which touches only a thread that must already exist, passes none.
pub(super) fn recompute_thread(
    store: &SqlStore,
    tx: &mut dyn Sql,
    thread_id: ThreadId,
    hints: &[&Message],
) -> Result<()> {
    let agg = tx.query_opt(
        "SELECT COUNT(*), COALESCE(SUM(CASE WHEN flags & 1 = 0 THEN 1 ELSE 0 END), 0), \
         COALESCE(MAX(date_us), 0) FROM mail_messages WHERE thread_id = ?1",
        &vals![thread_id.to_string()],
    )?;
    let (count, unread, last_date_us) = match &agg {
        Some(row) => (row.i64(0)?, row.i64(1)?, row.i64(2)?),
        None => (0, 0, 0),
    };

    if count == 0 {
        // Nothing left of this thread anywhere: it, and every per-mailbox
        // row naming it, are gone -- "removal... deleting an empty one."
        tx.execute("DELETE FROM threads WHERE id = ?1", &vals![thread_id.to_string()])?;
        tx.execute(
            "DELETE FROM thread_mailboxes WHERE thread_id = ?1",
            &vals![thread_id.to_string()],
        )?;
        return Ok(());
    }

    let existing =
        tx.query_opt("SELECT data FROM threads WHERE id = ?1", &vals![thread_id.to_string()])?;
    let mut thread: Thread = match existing {
        Some(row) => store.unseal(&thread_aad(thread_id), &row.bytes(0)?)?,
        None => seed_thread(store, tx, thread_id, hints)?,
    };
    // A new participant seen on any hint message joins the list; an
    // already-known one (by address, case-insensitively) is left alone
    // rather than duplicated or reordered.
    merge_participants(&mut thread.participants, hints);
    thread.last_date = from_us(last_date_us);
    thread.message_count = count as u32;
    thread.unread_count = unread as u32;

    let sealed = store.seal(&thread_aad(thread_id), &thread)?;
    let (sql, args) = upsert_stmt(&thread, sealed);
    tx.execute(&sql, &args)?;

    recompute_thread_mailboxes(tx, thread_id)?;
    Ok(())
}

/// Build a brand-new [`Thread`] for a thread id that has never had a row --
/// the first message of a new conversation. Prefers `hints` (already in
/// hand, no extra decrypt); falls back to decrypting whatever this thread's
/// messages already are, for the rare caller that reaches a not-yet-created
/// thread with no hint of its own (a `reset_mailbox` or `remove_uids` call
/// racing a thread that somehow never got its row -- not expected in
/// ordinary operation, but a fallback that answers correctly costs one
/// query on a path that should never be hot).
fn seed_thread(
    store: &SqlStore,
    tx: &mut dyn Sql,
    thread_id: ThreadId,
    hints: &[&Message],
) -> Result<Thread> {
    let now = jiff::Timestamp::now();
    if let Some(first) = hints.first() {
        let mut participants = Vec::new();
        merge_participants(&mut participants, hints);
        return Ok(Thread {
            id: thread_id,
            account_id: first.account_id,
            subject: first.subject.clone(),
            participants,
            last_date: now,
            message_count: 0,
            unread_count: 0,
            category: None,
            snoozed_until: None,
        });
    }

    let rows = tx.records(
        "SELECT id, data FROM mail_messages WHERE thread_id = ?1 ORDER BY date_us",
        &vals![thread_id.to_string()],
    )?;
    let mut account_id = None;
    let mut subject = String::new();
    let mut participants = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (id, data) in &rows {
        let mid: MailMessageId = id
            .parse()
            .map_err(|e: <ThreadId as std::str::FromStr>::Err| Error::Invalid(e.to_string()))?;
        let m: Message = store.unseal(&message_aad(mid), data)?;
        if account_id.is_none() {
            account_id = Some(m.account_id);
        }
        if subject.is_empty() {
            subject.clone_from(&m.subject);
        }
        for addr in std::iter::once(&m.from).chain(m.to.iter()).chain(m.cc.iter()) {
            if seen.insert(addr.email.to_lowercase()) {
                participants.push(addr.clone());
            }
        }
    }
    Ok(Thread {
        id: thread_id,
        account_id: account_id.ok_or_else(|| {
            Error::Invalid(format!("thread {thread_id} has messages but no account"))
        })?,
        subject,
        participants,
        last_date: now,
        message_count: 0,
        unread_count: 0,
        category: None,
        snoozed_until: None,
    })
}

fn merge_participants(participants: &mut Vec<Address>, hints: &[&Message]) {
    let mut seen: std::collections::HashSet<String> =
        participants.iter().map(|a| a.email.to_lowercase()).collect();
    for m in hints {
        for addr in std::iter::once(&m.from).chain(m.to.iter()).chain(m.cc.iter()) {
            if seen.insert(addr.email.to_lowercase()) {
                participants.push(addr.clone());
            }
        }
    }
}

/// Recompute every `thread_mailboxes` row for `thread_id`, one per mailbox
/// any of its messages is currently filed in, and delete any row for a
/// mailbox that no longer holds one -- the per-mailbox half of
/// [`recompute_thread`].
fn recompute_thread_mailboxes(tx: &mut dyn Sql, thread_id: ThreadId) -> Result<()> {
    let rows = tx.query(
        "SELECT mm.mailbox_id, COALESCE(SUM(CASE WHEN m.flags & 1 = 0 THEN 1 ELSE 0 END), 0), \
         COALESCE(MAX(m.date_us), 0) \
         FROM message_mailboxes mm JOIN mail_messages m ON m.id = mm.message_id \
         WHERE m.thread_id = ?1 GROUP BY mm.mailbox_id",
        &vals![thread_id.to_string()],
    )?;
    let mut live = Vec::with_capacity(rows.len());
    for row in &rows {
        let mailbox_id = row.text(0)?;
        let unread = row.i64(1)?;
        let last_date_us = row.i64(2)?;
        tx.execute(
            "INSERT INTO thread_mailboxes (thread_id, mailbox_id, last_date_us, unread)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (thread_id, mailbox_id) DO UPDATE SET last_date_us = ?3, unread = ?4",
            &vals![thread_id.to_string(), mailbox_id.clone(), last_date_us, unread],
        )?;
        live.push(mailbox_id);
    }
    if live.is_empty() {
        tx.execute(
            "DELETE FROM thread_mailboxes WHERE thread_id = ?1",
            &vals![thread_id.to_string()],
        )?;
    } else {
        let holes = placeholders(2, live.len());
        let mut args = vec![Value::Text(thread_id.to_string())];
        args.extend(live.into_iter().map(Value::Text));
        tx.execute(
            &format!(
                "DELETE FROM thread_mailboxes WHERE thread_id = ?1 AND mailbox_id NOT IN ({holes})"
            ),
            &args,
        )?;
    }
    Ok(())
}
