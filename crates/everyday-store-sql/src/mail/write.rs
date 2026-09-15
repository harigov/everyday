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
use everyday_core::id::{AccountId, MailMessageId, MailboxId, PackId, ThreadId};
use everyday_core::mail::{
    Address, Category, CategoryRules, Invite, Message, MessageFlags, Thread,
    categorize::{self, CategorizeInput},
};
use everyday_core::packstore::PackRef;
use everyday_core::store::mail::{IngestMessage, MailStore, message_aad, thread_aad};

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

/// The number of `?` placeholders any `IN (...)` this module builds allows
/// itself, whatever list it is built from.
///
/// SQLite refuses a prepared statement with more than 32,766 bound
/// parameters; Postgres, 65,535. A mailbox -- or a single removal call --
/// can hold tens of thousands of messages (`docs/plans/mail.md`'s own speed
/// budget names a hundred thousand), which is well past the smaller of the
/// two limits if every uid, message id or mailbox id in the list became its
/// own placeholder in one statement. 500 leaves a wide margin under either
/// limit while keeping the round-trip count for even the largest mailbox in
/// the low hundreds, not the tens of thousands a one-row-at-a-time loop
/// would need.
const IN_CHUNK: usize = 500;

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
            let already_filed = tx
                .query_opt(
                    "SELECT 1 FROM message_mailboxes WHERE message_id = ?1 AND mailbox_id = ?2",
                    &vals![im.message.id.to_string(), im.mailbox.to_string()],
                )?
                .is_some();
            tx.execute(
                "INSERT INTO message_mailboxes (message_id, mailbox_id, uid) VALUES (?1, ?2, ?3)
                 ON CONFLICT (message_id, mailbox_id) DO UPDATE SET uid = ?3",
                &vals![im.message.id.to_string(), im.mailbox.to_string(), i64::from(im.uid)],
            )?;
            if !already_filed {
                // Mail newly arriving in a mailbox a thread was hidden from --
                // a reply landing in the Inbox of an archived conversation --
                // brings the thread back, as every mail client does. The
                // hide was about the messages that were there when it was
                // asked for, not about the conversation for ever.
                tx.execute(
                    "DELETE FROM hidden_thread_mailboxes WHERE thread_id = ?1 AND mailbox_id = ?2",
                    &vals![im.message.thread_id.to_string(), im.mailbox.to_string()],
                )?;
            }
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

/// See [`everyday_core::store::mail::MailStore::set_message_category`].
pub(super) fn set_message_category(
    store: &SqlStore,
    id: MailMessageId,
    category: Category,
) -> Result<()> {
    let mut conn = store.write();
    let mut tx = conn.begin()?;
    let Some(mut message) = message_by_id(store, tx.as_mut(), id)? else {
        return Ok(());
    };
    let thread_id = message.thread_id;
    message.category = Some(category);
    let sealed = store.seal(&message_aad(id), &message)?;
    let (sql, args) = upsert_stmt(&message, sealed);
    tx.execute(&sql, &args)?;
    recompute_thread(store, tx.as_mut(), thread_id, &[])?;
    tx.commit()
}

/// See [`everyday_core::store::mail::MailStore::hide_thread_from_mailbox`].
///
/// Deletes `thread`'s row from `mailbox`'s own `thread_mailboxes` list, as
/// the very first version of this function did, but also records a marker
/// in `hidden_thread_mailboxes` naming the pair -- the fix for "archiving a
/// thread doesn't survive a flag change". [`message_mailboxes`] is
/// deliberately left untouched: it is what the outbox executor's own
/// `Lookups` resolves an `Archive`, `Trash` or `Move` op against to find
/// which `(mailbox, uid)` to actually tell the server about (see
/// `everyday_service::outbox::VaultLookups` and
/// `everyday_mail::outbox::archive`), so deleting it here -- before the op
/// has even reached a connection -- would leave the op with nothing to act
/// on. The marker is what [`recompute_thread_mailboxes`] now consults
/// before it would otherwise rebuild the very row this call just deleted:
/// every flag or label write ends there, which is exactly what used to
/// undo an archive the moment the next star or mark-read ran. Gmail's own
/// "archive" -- dropping the `\Inbox` label's membership -- becomes durably
/// true only once the op executes; until then this is the client's honest
/// picture of what it has *asked for*, kept apart from `message_mailboxes`,
/// which stays the client's honest picture of what the server has actually
/// confirmed. When the sync engine later observes the real server-side
/// move, its own `remove_uids`/`ingest` calls update `message_mailboxes`
/// for real -- see [`remove_uids_tx`] -- and this marker is left stale but
/// harmless: with the row it was suppressing already gone for good, there
/// is nothing left for [`recompute_thread_mailboxes`] to need suppressing.
/// A thread with no row for `mailbox` is a no-op past the marker insert,
/// which is itself idempotent.
pub(super) fn hide_thread_from_mailbox(
    store: &SqlStore,
    thread: ThreadId,
    mailbox: MailboxId,
) -> Result<()> {
    let mut conn = store.write();
    let mut tx = conn.begin()?;
    let (thread_s, mailbox_s) = (thread.to_string(), mailbox.to_string());
    tx.execute(
        "INSERT INTO hidden_thread_mailboxes (thread_id, mailbox_id) VALUES (?1, ?2)
         ON CONFLICT (thread_id, mailbox_id) DO NOTHING",
        &vals![thread_s.clone(), mailbox_s.clone()],
    )?;
    tx.execute(
        "DELETE FROM thread_mailboxes WHERE thread_id = ?1 AND mailbox_id = ?2",
        &vals![thread_s, mailbox_s],
    )?;
    tx.commit()
}

/// See [`everyday_core::store::mail::MailStore::restore_thread_mailboxes`].
///
/// The exact inverse of [`hide_thread_from_mailbox`]: every marker it left
/// in `hidden_thread_mailboxes` for `thread` is cleared, and
/// [`recompute_thread_mailboxes`] rebuilds `thread_mailboxes` from
/// `message_mailboxes` -- which [`hide_thread_from_mailbox`] never touched
/// in the first place, so this reconstructs exactly the row that call hid,
/// uid and all, with no snapshot to have kept anywhere.
pub(super) fn restore_thread_mailboxes(store: &SqlStore, thread: ThreadId) -> Result<()> {
    let mut conn = store.write();
    let mut tx = conn.begin()?;
    tx.execute(
        "DELETE FROM hidden_thread_mailboxes WHERE thread_id = ?1",
        &vals![thread.to_string()],
    )?;
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
pub(super) fn remove_uids(
    store: &SqlStore,
    mailbox: MailboxId,
    uids: &[u32],
) -> Result<Vec<(MailMessageId, PackRef)>> {
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let mut conn = store.write();
    let mut tx = conn.begin()?;
    let removed = remove_uids_tx(store, tx.as_mut(), mailbox, uids)?;
    tx.commit()?;
    Ok(removed)
}

/// The shared body of [`remove_uids`], [`reset_mailbox`] and
/// [`delete_mailbox`]: drop every named `(mailbox, uid)` membership, delete
/// any message that no mailbox names any more (body included), and
/// recompute every thread touched.
///
/// Every `IN (...)` this builds is chunked to [`IN_CHUNK`] items at a time
/// -- `uids` alone can be tens of thousands long (a mailbox holding that
/// many messages, or `delete_mailbox` handing this every uid it ever
/// tracked), and one placeholder per item past either backend's own limit
/// is exactly what used to make this fail; see [`IN_CHUNK`]'s own docs.
///
/// Returns every message that became genuinely dead -- no mailbox names it
/// any more -- paired with the pack address it was stored under, read back
/// before the row naming it is deleted (afterwards, nothing else remembers
/// it). A message still filed under a *different* mailbox (a Gmail label
/// the removal did not touch) is not dead, and is not in the result: see
/// [`everyday_core::store::mail::MailStore::remove_uids`]'s own docs for why
/// only "no mailbox left at all" counts.
fn remove_uids_tx(
    store: &SqlStore,
    tx: &mut dyn Sql,
    mailbox: MailboxId,
    uids: &[u32],
) -> Result<Vec<(MailMessageId, PackRef)>> {
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let mailbox_s = mailbox.to_string();

    // Every `(message_id, thread_id)` pair a removed uid named, gathered
    // chunk by chunk so the two statements below never bind more than
    // `IN_CHUNK` uids at once.
    let mut touched: Vec<(String, String)> = Vec::with_capacity(uids.len());
    for chunk in uids.chunks(IN_CHUNK) {
        let holes = placeholders(2, chunk.len());
        let mut args = vec![Value::Text(mailbox_s.clone())];
        args.extend(chunk.iter().map(|u| Value::Int(i64::from(*u))));

        let rows = tx.query(
            &format!(
                "SELECT mm.message_id, m.thread_id FROM message_mailboxes mm
                 JOIN mail_messages m ON m.id = mm.message_id
                 WHERE mm.mailbox_id = ?1 AND mm.uid IN ({holes})"
            ),
            &args,
        )?;
        for row in rows {
            touched.push((row.text(0)?, row.text(1)?));
        }
        tx.execute(
            &format!("DELETE FROM message_mailboxes WHERE mailbox_id = ?1 AND uid IN ({holes})"),
            &args,
        )?;
    }
    if touched.is_empty() {
        return Ok(Vec::new());
    }

    // A message stays alive as long as *any* mailbox still names it -- one
    // Gmail label removed while another still holds the same physical
    // message is not what "dead" means here. Batch the check, again in
    // chunks, rather than one `COUNT(*)` round trip per touched message:
    // the common case is a mailbox holding tens of thousands of messages
    // that share no other mailbox at all, and that case must not cost tens
    // of thousands of round trips just because it used to cost one
    // over-wide `IN (...)` instead.
    let touched_ids: Vec<String> = touched.iter().map(|(id, _)| id.clone()).collect();
    let mut still_has_a_mailbox: BTreeSet<String> = BTreeSet::new();
    for chunk in touched_ids.chunks(IN_CHUNK) {
        let holes = placeholders(1, chunk.len());
        let args: Vec<Value> = chunk.iter().cloned().map(Value::Text).collect();
        let rows = tx.query(
            &format!(
                "SELECT DISTINCT message_id FROM message_mailboxes WHERE message_id IN ({holes})"
            ),
            &args,
        )?;
        for row in rows {
            still_has_a_mailbox.insert(row.text(0)?);
        }
    }

    let mut orphan_threads: BTreeSet<ThreadId> = BTreeSet::new();
    let mut orphan_ids: Vec<String> = Vec::new();
    for (message_id, thread_id) in &touched {
        if let Ok(tid) = thread_id.parse::<ThreadId>() {
            orphan_threads.insert(tid);
        }
        if !still_has_a_mailbox.contains(message_id) {
            orphan_ids.push(message_id.clone());
        }
    }

    // Read each dead message's pack address before deleting its row --
    // `mail_messages` is the only place that address lives, so this is the
    // last moment it can be read at all.
    let mut removed = Vec::with_capacity(orphan_ids.len());
    for chunk in orphan_ids.chunks(IN_CHUNK) {
        let holes = placeholders(1, chunk.len());
        let args: Vec<Value> = chunk.iter().cloned().map(Value::Text).collect();
        let rows = tx.query(
            &format!(
                "SELECT id, account_id, pack_id, pack_offset, pack_len FROM mail_messages \
                 WHERE id IN ({holes})"
            ),
            &args,
        )?;
        for row in rows {
            let id: MailMessageId =
                row.text(0)?.parse().map_err(|e: <MailMessageId as std::str::FromStr>::Err| {
                    Error::Invalid(e.to_string())
                })?;
            let account = row.text(1)?;
            let pack: PackId = row
                .text(2)?
                .parse()
                .map_err(|e: <PackId as std::str::FromStr>::Err| Error::Invalid(e.to_string()))?;
            let offset = row.i64(3)? as u64;
            let len = row.i64(4)? as u32;
            removed.push((id, PackRef { account, pack, offset, len }));
        }
        // No mailbox holds this message any more: it, and its body, are
        // gone -- an `EXPUNGE`d message, or the last Gmail label removed.
        tx.execute(&format!("DELETE FROM bodies WHERE message_id IN ({holes})"), &args)?;
        tx.execute(&format!("DELETE FROM mail_messages WHERE id IN ({holes})"), &args)?;
    }

    for thread_id in orphan_threads {
        recompute_thread(store, tx, thread_id, &[])?;
    }
    Ok(removed)
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
///
/// The `uid` scan below is a plain `WHERE mailbox_id = ?1`, not an
/// `IN (...)` -- reading every uid a mailbox holds never binds more than
/// one parameter, however many rows come back, so [`IN_CHUNK`] has nothing
/// to do here. [`remove_uids_tx`], which this hands the whole list to, is
/// what chunks the uids themselves once it builds its own `IN (...)`
/// clauses over them.
pub(super) fn delete_mailbox(
    store: &SqlStore,
    id: MailboxId,
) -> Result<Vec<(MailMessageId, PackRef)>> {
    let mut conn = store.write();
    let mut tx = conn.begin()?;
    let uids: Vec<u32> = tx
        .query("SELECT uid FROM message_mailboxes WHERE mailbox_id = ?1", &vals![id.to_string()])?
        .into_iter()
        .map(|r| Ok(r.i64(0)? as u32))
        .collect::<Result<_>>()?;
    let removed = remove_uids_tx(store, tx.as_mut(), id, &uids)?;
    tx.execute("DELETE FROM thread_mailboxes WHERE mailbox_id = ?1", &vals![id.to_string()])?;
    tx.execute("DELETE FROM mailboxes WHERE id = ?1", &vals![id.to_string()])?;
    tx.commit()?;
    Ok(removed)
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
///
/// # `thread.category`
///
/// Always overwritten here, from whichever message in the thread now has
/// the latest `date_us` -- a thread's category is never its own decision,
/// only a mirror of its newest message's, the same way `last_date` already
/// is. That is what makes `set_thread_category`'s correction reach the
/// thread at all: it (through [`crate::mail::CategoryRules`] and
/// [`MailStore::recategorize`](everyday_core::store::mail::MailStore::recategorize))
/// only ever sets a *message's* category, and this is the one place that
/// answer becomes what a thread list actually shows.
pub(super) fn recompute_thread(
    store: &SqlStore,
    tx: &mut dyn Sql,
    thread_id: ThreadId,
    hints: &[&Message],
) -> Result<()> {
    let agg = tx.query_opt(
        "SELECT COUNT(*), COALESCE(SUM(CASE WHEN flags & 1 = 0 THEN 1 ELSE 0 END), 0), \
         COALESCE(MAX(date_us), 0), \
         (SELECT category FROM mail_messages WHERE thread_id = ?1 ORDER BY date_us DESC LIMIT 1), \
         COALESCE(MAX(CASE WHEN flags & 4 != 0 THEN 1 ELSE 0 END), 0), \
         COALESCE(MAX(CASE WHEN has_attachments THEN 1 ELSE 0 END), 0), \
         (SELECT id FROM mail_messages WHERE thread_id = ?1 ORDER BY date_us DESC LIMIT 1) \
         FROM mail_messages WHERE thread_id = ?1",
        &vals![thread_id.to_string()],
    )?;
    let (count, unread, last_date_us, category, starred, has_attachments, newest_id) = match &agg {
        Some(row) => (
            row.i64(0)?,
            row.i64(1)?,
            row.i64(2)?,
            row.opt_text(3)?.and_then(|c| Category::parse(&c)),
            row.i64(4)? != 0,
            row.i64(5)? != 0,
            row.opt_text(6)?,
        ),
        None => (0, 0, 0, None, false, false, None),
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
    thread.category = category;
    thread.starred = starred;
    thread.has_attachments = has_attachments;
    thread.snippet = newest_snippet(store, tx, hints, newest_id.as_deref())?;

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
            snippet: String::new(),
            starred: false,
            has_attachments: false,
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
        snippet: String::new(),
        starred: false,
        has_attachments: false,
    })
}

/// The snippet [`recompute_thread`] writes into [`Thread::snippet`]: the
/// newest message's own, read from `hints` when it is already in hand (the
/// caller just wrote it, in this same call -- true of every ingest, which is
/// also how the body pass's snippet reaches a thread, since `process_body`
/// sets [`Message::snippet`] and then hands the updated message back through
/// [`everyday_core::store::mail::MailStore::ingest`] as a hint), or by a
/// single extra decrypt of the newest message otherwise. Bounded to at most
/// one decrypt per recompute, whatever the thread's size -- the same
/// "extend, don't add a second pass" trade [`recompute_thread`]'s own docs
/// describe.
fn newest_snippet(
    store: &SqlStore,
    tx: &mut dyn Sql,
    hints: &[&Message],
    newest_id: Option<&str>,
) -> Result<String> {
    let Some(newest_id) = newest_id else { return Ok(String::new()) };
    if let Some(m) = hints.iter().find(|m| m.id.to_string() == newest_id) {
        return Ok(m.snippet.clone());
    }
    let mid: MailMessageId = newest_id
        .parse()
        .map_err(|e: <MailMessageId as std::str::FromStr>::Err| Error::Invalid(e.to_string()))?;
    Ok(message_by_id(store, tx, mid)?.map(|m| m.snippet).unwrap_or_default())
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
    // A marker whose mailbox no longer holds any of the thread's messages has
    // done its job: the server-side move landed and sync removed the
    // membership. Left in place it would hide the thread from that mailbox
    // for ever, including from mail that arrives there later.
    tx.execute(
        "DELETE FROM hidden_thread_mailboxes WHERE thread_id = ?1 AND mailbox_id NOT IN ( \
           SELECT mm.mailbox_id FROM message_mailboxes mm \
           JOIN mail_messages m ON m.id = mm.message_id WHERE m.thread_id = ?2 \
         )",
        &vals![thread_id.to_string(), thread_id.to_string()],
    )?;
    // The `NOT IN` sub-select is what keeps this from rebuilding a row
    // `hide_thread_from_mailbox` just deleted on purpose:
    // `message_mailboxes` still names the mailbox (the op has not reached a
    // server yet, so it must), but `hidden_thread_mailboxes` says the
    // person -- or the assistant, or a routine -- already asked to have it
    // hidden. See that function's own docs for the whole design.
    let rows = tx.query(
        "SELECT mm.mailbox_id, COALESCE(SUM(CASE WHEN m.flags & 1 = 0 THEN 1 ELSE 0 END), 0), \
         COALESCE(MAX(m.date_us), 0) \
         FROM message_mailboxes mm JOIN mail_messages m ON m.id = mm.message_id \
         WHERE m.thread_id = ?1 \
           AND mm.mailbox_id NOT IN ( \
             SELECT mailbox_id FROM hidden_thread_mailboxes WHERE thread_id = ?2 \
           ) \
         GROUP BY mm.mailbox_id",
        &vals![thread_id.to_string(), thread_id.to_string()],
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
        // Not chunked to `IN_CHUNK`, unlike every other `IN (...)` this
        // module builds: `live` is one row per *mailbox* a single thread's
        // messages are currently filed under, which even a message under
        // every Gmail label anyone has ever created stays orders of
        // magnitude under either backend's parameter limit. It also could
        // not be chunked correctly if it ever grew that large -- a `NOT IN`
        // does not decompose across several statements the way an `IN`
        // does, since each chunk would delete everything the *other*
        // chunks were about to keep.
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

/// See [`everyday_core::store::mail::MailStore::recategorize`].
///
/// A full decrypt of every message `account` has, on the accepted terms that
/// function's own docs give -- the same trade
/// [`everyday_core::store::mail::MailStore::message_by_message_id_header`]
/// already makes for a full-account scan. Batched at [`INGEST_BATCH_ROWS`]
/// rows per transaction, the same size `ingest` itself batches at, so a
/// large mailbox's backfill never holds the vault's single writer for the
/// length of the whole account.
pub(super) fn recategorize(
    store: &SqlStore,
    account: AccountId,
    rules: &CategoryRules,
) -> Result<u32> {
    // `MailStore::contacts` is a trait method, brought into scope by the
    // `use` above -- the contact book is vault-wide, so it is read once
    // rather than once per batch.
    let contacts = store.contacts()?;
    let rows = store.read().records(
        "SELECT id, data FROM mail_messages WHERE account_id = ?1",
        &vals![account.to_string()],
    )?;

    let mut touched: BTreeSet<ThreadId> = BTreeSet::new();
    let mut changed = 0u32;
    for batch in rows.chunks(INGEST_BATCH_ROWS) {
        let mut conn = store.write();
        let mut tx = conn.begin()?;
        for (id, data) in batch {
            let mid: MailMessageId =
                id.parse().map_err(|e: <MailMessageId as std::str::FromStr>::Err| {
                    Error::Invalid(e.to_string())
                })?;
            let mut message: Message = store.unseal(&message_aad(mid), data)?;
            let input = CategorizeInput {
                from: &message.from.email,
                list_id: None,
                list_unsubscribe: None,
                precedence: None,
                auto_submitted: None,
                gmail_labels: &message.labels,
                ever_written_to: contacts.has_sent_to(&message.from.email),
            };
            let category = categorize::categorize(&input, rules);
            if message.category != Some(category) {
                message.category = Some(category);
                let sealed = store.seal(&message_aad(mid), &message)?;
                let (sql, args) = upsert_stmt(&message, sealed);
                tx.execute(&sql, &args)?;
                touched.insert(message.thread_id);
                changed += 1;
            }
        }
        tx.commit()?;
    }

    for thread_id in touched {
        let mut conn = store.write();
        let mut tx = conn.begin()?;
        recompute_thread(store, tx.as_mut(), thread_id, &[])?;
        tx.commit()?;
    }
    Ok(changed)
}

/// See [`everyday_core::store::mail::MailStore::remap_packs`].
///
/// One transaction, on the same "all or nothing" terms every other
/// multi-row write in this module keeps: a caller that sees this return
/// `Ok` may safely call
/// [`everyday_core::packstore::PackStore::drop_packs`] on the packs `remap`
/// moved messages out of, per that method's own two-step contract.
pub(super) fn remap_packs(
    store: &SqlStore,
    account: AccountId,
    remap: &[(PackRef, PackRef)],
) -> Result<()> {
    if remap.is_empty() {
        return Ok(());
    }
    let mut conn = store.write();
    let mut tx = conn.begin()?;
    for (old, new) in remap {
        let row = tx.query_opt(
            "SELECT id, data FROM mail_messages
             WHERE account_id = ?1 AND pack_id = ?2 AND pack_offset = ?3 AND pack_len = ?4",
            &vals![
                account.to_string(),
                old.pack.to_string(),
                old.offset as i64,
                i64::from(old.len)
            ],
        )?;
        // Not there any more -- the message this pair named was deleted (an
        // `EXPUNGE`, a removed Gmail label with nowhere else left) between
        // `compact` returning and this call running. There is nothing left
        // for the remap to reach; the pack it pointed at is already about
        // to be dropped along with everything else `compact` obsoleted.
        let Some(row) = row else { continue };
        let mid: MailMessageId =
            row.text(0)?.parse().map_err(|e: <MailMessageId as std::str::FromStr>::Err| {
                Error::Invalid(e.to_string())
            })?;
        let mut message: Message = store.unseal(&message_aad(mid), &row.bytes(1)?)?;
        message.pack = new.clone();
        let sealed = store.seal(&message_aad(mid), &message)?;
        let (sql, args) = upsert_stmt(&message, sealed);
        tx.execute(&sql, &args)?;
    }
    tx.commit()
}
