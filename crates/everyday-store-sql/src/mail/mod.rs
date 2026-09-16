//! Mail: mailboxes, messages, threads, bodies, drafts and the outbox.
//!
//! See [`everyday_core::store::mail`] for the trait this implements and the
//! two-views-of-a-thread design it is built around, and this crate's own
//! module docs for the exact clear columns. The heavy, multi-table writes
//! (ingest, a flag or label change, removal, a `UIDVALIDITY` reset) live in
//! [`write`], because [`MailStore`]'s `impl` has to be one block and that
//! block would otherwise be the longest file in the crate; what stays here
//! is the [`Record`] shape of each table and every read.

mod threads;
mod write;

#[cfg(any(test, feature = "testing"))]
pub use write::run_recategorize_staleness_regression;

use everyday_core::error::Result;
use everyday_core::id::{
    AccountId, BlobId, DraftId, MailMessageId, MailboxId, OpId, PackId, ThreadId,
};
use everyday_core::mail::{
    Body, Category, CategoryMatch, CategoryRules, ContactBook, Draft, Invite, Mailbox, Message,
    MessageFlags, Op, OpTarget, RemoteImageSettings, Thread,
};
use everyday_core::packstore::PackRef;
use everyday_core::store::mail::{
    IngestMessage, MailStore, ThreadFilter, ThreadPage, body_aad, category_rules_aad, contacts_aad,
    draft_aad, mailbox_aad, message_aad, op_aad, remote_image_settings_aad, thread_aad,
};
use jiff::Timestamp;

use crate::conn::{Sql, SqlExt, ToValue, Value};
use crate::record::Record;
use crate::{SqlStore, from_us, to_us, vals};

impl Record for Mailbox {
    const TABLE: &'static str = "mailboxes";
    const KIND: &'static str = "mailbox";
    type Id = MailboxId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        mailbox_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("account_id", self.account_id.to_string().to_value()),
            ("role", self.role.as_str().to_value()),
            ("uidvalidity", i64::from(self.uidvalidity).to_value()),
            ("uidnext", i64::from(self.uidnext).to_value()),
            ("highest_modseq", (self.highest_modseq as i64).to_value()),
        ]
    }
}

impl Record for Message {
    const TABLE: &'static str = "mail_messages";
    const KIND: &'static str = "message";
    type Id = MailMessageId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        message_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("account_id", self.account_id.to_string().to_value()),
            ("thread_id", self.thread_id.to_string().to_value()),
            ("date_us", to_us(self.date).to_value()),
            ("flags", self.flags.bits().to_value()),
            ("has_attachments", self.has_attachments.to_value()),
            ("size", (self.size as i64).to_value()),
            ("category", self.category.map(|c| c.as_str()).to_value()),
            ("pack_id", self.pack.pack.to_string().to_value()),
            ("pack_offset", (self.pack.offset as i64).to_value()),
            ("pack_len", i64::from(self.pack.len).to_value()),
        ]
    }
}

impl Record for Thread {
    const TABLE: &'static str = "threads";
    const KIND: &'static str = "thread";
    type Id = ThreadId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        thread_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("account_id", self.account_id.to_string().to_value()),
            ("last_date_us", to_us(self.last_date).to_value()),
            ("unread", i64::from(self.unread_count).to_value()),
            ("category", self.category.map(|c| c.as_str()).to_value()),
            ("snoozed_until_us", self.snoozed_until.map(to_us).to_value()),
        ]
    }
}

impl Record for Draft {
    const TABLE: &'static str = "drafts";
    const KIND: &'static str = "draft";
    type Id = DraftId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        draft_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("account_id", self.account_id.to_string().to_value()),
            ("in_reply_to", self.in_reply_to.map(|id| id.to_string()).to_value()),
            ("state", self.state.as_str().to_value()),
            ("origin", self.origin.kind().to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl Record for Op {
    const TABLE: &'static str = "ops";
    const KIND: &'static str = "op";
    type Id = OpId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        op_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("account_id", self.account_id.to_string().to_value()),
            ("state", self.state.as_str().to_value()),
            ("origin", self.origin.kind().to_value()),
            ("not_before_us", to_us(self.not_before).to_value()),
            // `NULL` for an op whose `OpTarget` is not `Thread` (a `Draft`
            // today; `Message` is never actually minted -- see
            // `crate::schema`'s own docs on this column) -- what
            // `ops_for_thread` reads back to answer "what has recently
            // happened to this thread", including a failed op, so a thread
            // can say "Couldn't archive: {error}" beside itself.
            (
                "thread_id",
                match self.target {
                    OpTarget::Thread(id) => Some(id.to_string()),
                    OpTarget::Message(_) | OpTarget::Draft(_) => None,
                }
                .to_value(),
            ),
        ]
    }
}

impl MailStore for SqlStore {
    // ---- mailboxes ----------------------------------------------------------

    fn list_mailboxes(&self, account: AccountId) -> Result<Vec<Mailbox>> {
        let rows = self.read().records(
            "SELECT id, data FROM mailboxes WHERE account_id = ?1 ORDER BY id",
            &vals![account.to_string()],
        )?;
        self.collect(rows, mailbox_aad)
    }

    fn get_mailbox(&self, id: MailboxId) -> Result<Mailbox> {
        self.get(id)
    }

    fn put_mailbox(&self, mailbox: &Mailbox) -> Result<()> {
        self.upsert(mailbox)
    }

    fn delete_mailbox(&self, id: MailboxId) -> Result<Vec<(MailMessageId, PackRef)>> {
        write::delete_mailbox(self, id)
    }

    // ---- bulk header ingest ------------------------------------------------

    fn ingest(&self, account: AccountId, messages: Vec<IngestMessage>) -> Result<()> {
        write::ingest(self, account, messages)
    }

    // ---- flag / label changes, and removal -----------------------------------

    fn update_flags(&self, mailbox: MailboxId, uid: u32, flags: MessageFlags) -> Result<()> {
        write::update_flags(self, mailbox, uid, flags)
    }

    fn update_labels(&self, mailbox: MailboxId, uid: u32, labels: Vec<String>) -> Result<()> {
        write::update_labels(self, mailbox, uid, labels)
    }

    fn remove_uids(
        &self,
        mailbox: MailboxId,
        uids: &[u32],
    ) -> Result<Vec<(MailMessageId, PackRef)>> {
        write::remove_uids(self, mailbox, uids)
    }

    // ---- the inbox query ----------------------------------------------------

    fn list_threads(
        &self,
        mailbox: MailboxId,
        filter: &ThreadFilter,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<ThreadPage> {
        threads::list_threads(self, mailbox, filter, cursor, limit)
    }

    fn threads_in_category(
        &self,
        account: AccountId,
        category: everyday_core::mail::Category,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<ThreadPage> {
        threads::threads_in_category(self, account, category, cursor, limit)
    }

    // ---- thread detail ------------------------------------------------------

    fn thread(&self, id: ThreadId) -> Result<(Thread, Vec<Message>)> {
        let thread: Thread = self.get(id)?;
        let rows = self.read().records(
            "SELECT id, data FROM mail_messages WHERE thread_id = ?1 ORDER BY date_us",
            &vals![id.to_string()],
        )?;
        let messages: Vec<Message> = self.collect(rows, message_aad)?;
        Ok((thread, messages))
    }

    fn merge_threads(&self, keep: ThreadId, others: &[ThreadId]) -> Result<()> {
        write::merge_threads(self, keep, others)
    }

    fn get_message(&self, id: MailMessageId) -> Result<Message> {
        self.get(id)
    }

    // ---- bodies ------------------------------------------------------------
    //
    // Hand-written rather than through `Record`/`upsert`: that machinery
    // always names its primary key column `id`, and `bodies`' own primary
    // key is `message_id` -- the plan's schema, followed exactly. A body
    // has no clear column beyond it either, so there is nothing `Record`
    // would have bought here even if the column were named to fit.

    fn put_body(&self, body: &Body) -> Result<()> {
        let sealed = self.seal(&body_aad(body.message_id), body)?;
        self.write().execute(
            "INSERT INTO bodies (message_id, data) VALUES (?1, ?2)
             ON CONFLICT (message_id) DO UPDATE SET data = ?2",
            &vals![body.message_id.to_string(), sealed],
        )?;
        Ok(())
    }

    fn put_bodies(&self, bodies: &[Body]) -> Result<()> {
        if bodies.is_empty() {
            return Ok(());
        }
        let mut sealed = Vec::with_capacity(bodies.len());
        for body in bodies {
            sealed
                .push((body.message_id.to_string(), self.seal(&body_aad(body.message_id), body)?));
        }
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        for (message_id, data) in sealed {
            tx.execute(
                "INSERT INTO bodies (message_id, data) VALUES (?1, ?2)
                 ON CONFLICT (message_id) DO UPDATE SET data = ?2",
                &vals![message_id, data],
            )?;
        }
        tx.commit()
    }

    fn get_body(&self, message_id: MailMessageId) -> Result<Body> {
        let sealed = self
            .read()
            .sealed(
                "SELECT data FROM bodies WHERE message_id = ?1",
                &vals![message_id.to_string()],
            )?
            .ok_or_else(|| everyday_core::error::Error::not_found("body", message_id))?;
        self.unseal(&body_aad(message_id), &sealed)
    }

    // ---- drafts ------------------------------------------------------------

    fn put_draft(&self, draft: &Draft) -> Result<()> {
        self.upsert(draft)
    }

    fn get_draft(&self, id: DraftId) -> Result<Draft> {
        self.get(id)
    }

    fn list_drafts(&self, account: AccountId) -> Result<Vec<Draft>> {
        let rows = self.read().records(
            "SELECT id, data FROM drafts WHERE account_id = ?1 ORDER BY updated_us DESC",
            &vals![account.to_string()],
        )?;
        self.collect(rows, draft_aad)
    }

    fn delete_draft(&self, id: DraftId) -> Result<()> {
        self.delete_by_id::<Draft>(id)?;
        Ok(())
    }

    // ---- the outbox ----------------------------------------------------------

    fn enqueue_op(&self, op: &Op) -> Result<()> {
        self.upsert(op)
    }

    fn due_ops(&self, account: AccountId, now: Timestamp, limit: u32) -> Result<Vec<Op>> {
        // `not_before_us` alone does not mean "creation order": the drain
        // loop's own retry arm pushes a failed op's `not_before` forward by
        // its backoff, which can easily land *after* a younger op's own
        // `not_before` for the very same target -- an `Unlabel` queued a
        // moment after a `Label` that just failed transiently is due
        // *sooner* than the `Label` it must not race ahead of. The `NOT
        // EXISTS` below holds an op back for as long as an *earlier-created*
        // op against the same thread is still `pending` and not yet due
        // itself -- so a target's own ops never drain out of the order they
        // were queued in, no matter how their individual backoffs happen to
        // fall. It does *not* hold an op back merely because an earlier
        // sibling is *also* due this instant: both are then returned
        // together (see the `ORDER BY` below for why that is still safe),
        // exactly as before this existed, so two ops queued back to back
        // against the same thread still drain in the same `drain_outbox`
        // pass they always could. `id` is a UUIDv7
        // ([`crate::id`]'s own `new`), whose textual form sorts exactly in
        // creation order, so comparing it as ordinary text is enough; no
        // separate "created at" column is needed for this.
        //
        // Only `thread_id` -- populated for `OpTarget::Thread` alone, see
        // this file's own `Record for Op` impl -- can group ops by target
        // in SQL without decrypting every row, so this ordering guarantee
        // is thread-scoped: two ops against the same `Draft` (an
        // `AppendDraft` racing a `DiscardDraft`, say) are not held back by
        // each other here. `thread_id IS NULL` never matches itself in the
        // `NOT EXISTS` subquery -- SQL's `NULL = NULL` is never true -- so
        // a `Draft`-targeted op is always immediately eligible, exactly the
        // pre-existing behaviour for those.
        //
        // Holding an op back only ever affects other ops sharing its own
        // `thread_id`; an unrelated thread's ops are untouched by this
        // clause and drain exactly as before, so one stuck target can never
        // block the rest of the account's outbox.
        let mut sql = "SELECT id, data FROM ops o \
             WHERE o.account_id = ?1 AND o.state = 'pending' AND o.not_before_us <= ?2 \
             AND NOT EXISTS ( \
                 SELECT 1 FROM ops earlier \
                 WHERE earlier.thread_id = o.thread_id \
                   AND earlier.state = 'pending' \
                   AND earlier.id < o.id \
                   AND earlier.not_before_us > ?2 \
             )"
        .to_string();
        // `id ASC`, not `not_before_us ASC`: once both an earlier- and a
        // later-created op for the same thread are due in the same call
        // (the case the `NOT EXISTS` above lets through on purpose), their
        // `not_before` values alone can still be in the wrong order --
        // exactly the `Label`/`Unlabel` backoff shape this whole method's
        // docs describe -- while `id`, a creation-ordered UUIDv7, never is.
        // Ordering the *whole* page by `id` rather than only breaking ties
        // with it costs nothing else: `not_before_us <= ?2` has already
        // decided *which* ops are due this call, so this only ever reorders
        // ops that were already going to run in the same pass.
        self.page(&mut sql, "id ASC", Some(limit), 0);
        let rows = self.read().records(&sql, &vals![account.to_string(), to_us(now)])?;
        self.collect(rows, op_aad)
    }

    fn next_pending_op_at(&self, account: AccountId) -> Result<Option<Timestamp>> {
        let row = self.read().query_opt(
            "SELECT MIN(not_before_us) FROM ops WHERE account_id = ?1 AND state = 'pending'",
            &vals![account.to_string()],
        )?;
        Ok(row.and_then(|r| r.opt_i64(0).transpose()).transpose()?.map(from_us))
    }

    fn in_flight_ops(&self, account: AccountId) -> Result<Vec<Op>> {
        let mut sql =
            "SELECT id, data FROM ops WHERE account_id = ?1 AND state = 'in_flight'".to_string();
        self.page(&mut sql, "not_before_us ASC", None, 0);
        let rows = self.read().records(&sql, &vals![account.to_string()])?;
        self.collect(rows, op_aad)
    }

    fn update_op(&self, op: &Op) -> Result<()> {
        self.upsert(op)
    }

    fn get_op(&self, id: OpId) -> Result<Op> {
        self.get(id)
    }

    fn ops_by_origin(&self, kind: &str, limit: u32) -> Result<Vec<Op>> {
        let mut sql = "SELECT id, data FROM ops WHERE origin = ?1".to_string();
        self.page(&mut sql, "not_before_us DESC", Some(limit), 0);
        let rows = self.read().records(&sql, &vals![kind])?;
        self.collect(rows, op_aad)
    }

    fn ops_for_thread(&self, thread: ThreadId, limit: u32) -> Result<Vec<Op>> {
        let mut sql = "SELECT id, data FROM ops WHERE thread_id = ?1".to_string();
        self.page(&mut sql, "not_before_us DESC", Some(limit), 0);
        let rows = self.read().records(&sql, &vals![thread.to_string()])?;
        self.collect(rows, op_aad)
    }

    // ---- resolution and reset ------------------------------------------------

    fn message_locations(&self, id: MailMessageId) -> Result<Vec<(MailboxId, u32)>> {
        let rows = self.read().query(
            "SELECT mailbox_id, uid FROM message_mailboxes WHERE message_id = ?1",
            &vals![id.to_string()],
        )?;
        rows.into_iter()
            .map(|r| {
                let mailbox: MailboxId =
                    r.text(0)?.parse().map_err(|e: <MailboxId as std::str::FromStr>::Err| {
                        everyday_core::error::Error::Invalid(e.to_string())
                    })?;
                Ok((mailbox, r.i64(1)? as u32))
            })
            .collect()
    }

    fn message_by_uid(&self, mailbox: MailboxId, uid: u32) -> Result<Option<Message>> {
        let row = self.read().query_opt(
            "SELECT m.id, m.data FROM mail_messages m
             JOIN message_mailboxes mm ON mm.message_id = m.id
             WHERE mm.mailbox_id = ?1 AND mm.uid = ?2",
            &vals![mailbox.to_string(), i64::from(uid)],
        )?;
        let Some(row) = row else { return Ok(None) };
        let mid: MailMessageId =
            row.text(0)?.parse().map_err(|e: <MailMessageId as std::str::FromStr>::Err| {
                everyday_core::error::Error::Invalid(e.to_string())
            })?;
        Ok(Some(self.unseal(&message_aad(mid), &row.bytes(1)?)?))
    }

    fn message_by_message_id_header(
        &self,
        account: AccountId,
        message_id_header: &str,
    ) -> Result<Option<Message>> {
        // No clear column carries `Message-ID` -- see this trait's own docs
        // for why the plan's schema leaves it sealed, and why that is an
        // acceptable cost here: a `UIDVALIDITY` reset is rare, and this scan
        // is bounded to one account's messages, never the whole vault.
        let rows = self.read().records(
            "SELECT id, data FROM mail_messages WHERE account_id = ?1",
            &vals![account.to_string()],
        )?;
        for (id, data) in rows {
            let mid: MailMessageId =
                id.parse().map_err(|e: <MailMessageId as std::str::FromStr>::Err| {
                    everyday_core::error::Error::Invalid(e.to_string())
                })?;
            let message: Message = self.unseal(&message_aad(mid), &data)?;
            if message.message_id_header == message_id_header {
                return Ok(Some(message));
            }
        }
        Ok(None)
    }

    fn uid_set(&self, mailbox: MailboxId) -> Result<Vec<u32>> {
        let rows = self.read().query(
            "SELECT uid FROM message_mailboxes WHERE mailbox_id = ?1",
            &vals![mailbox.to_string()],
        )?;
        rows.into_iter().map(|r| Ok(r.i64(0)? as u32)).collect()
    }

    fn pending_bodies(&self, mailbox: MailboxId, limit: u32) -> Result<Vec<(Message, u32)>> {
        // `m.pack_len = 0` is the clear-column sentinel this trait's own
        // docs name -- see `MailStore::pending_bodies` -- so this is a
        // query over `mail_messages`' clear columns, exactly like
        // `uid_set` above, not a decrypt of every row `mailbox` has.
        let mut sql = "SELECT m.id, mm.uid, m.data FROM mail_messages m
             JOIN message_mailboxes mm ON mm.message_id = m.id
             WHERE mm.mailbox_id = ?1 AND m.pack_len = 0"
            .to_string();
        self.page(&mut sql, "m.date_us DESC", Some(limit), 0);
        let rows = self.read().query(&sql, &vals![mailbox.to_string()])?;
        rows.into_iter()
            .map(|r| {
                let mid: MailMessageId = r.text(0)?.parse().map_err(
                    |e: <MailMessageId as std::str::FromStr>::Err| {
                        everyday_core::error::Error::Invalid(e.to_string())
                    },
                )?;
                let uid = r.i64(1)? as u32;
                let message: Message = self.unseal(&message_aad(mid), &r.bytes(2)?)?;
                Ok((message, uid))
            })
            .collect()
    }

    fn reset_mailbox(&self, mailbox: MailboxId) -> Result<()> {
        write::reset_mailbox(self, mailbox)
    }

    // ---- optimistic local writes -------------------------------------------

    fn set_message_flags(&self, id: MailMessageId, flags: MessageFlags) -> Result<()> {
        write::set_message_flags(self, id, flags)
    }

    fn set_message_labels(&self, id: MailMessageId, labels: Vec<String>) -> Result<()> {
        write::set_message_labels(self, id, labels)
    }

    fn set_message_invite(&self, id: MailMessageId, invite: Option<Invite>) -> Result<()> {
        write::set_message_invite(self, id, invite)
    }

    fn set_message_category(
        &self,
        id: MailMessageId,
        category: everyday_core::mail::Category,
    ) -> Result<()> {
        write::set_message_category(self, id, category)
    }

    fn hide_thread_from_mailbox(&self, thread: ThreadId, mailbox: MailboxId) -> Result<()> {
        write::hide_thread_from_mailbox(self, thread, mailbox)
    }

    fn restore_thread_mailboxes(&self, thread: ThreadId) -> Result<()> {
        write::restore_thread_mailboxes(self, thread)
    }

    fn set_thread_snoozed_until(&self, thread: ThreadId, until: Option<Timestamp>) -> Result<()> {
        write::set_thread_snoozed_until(self, thread, until)
    }

    fn set_thread_ai_categorize_asked(&self, thread: ThreadId, message_count: u32) -> Result<()> {
        write::set_thread_ai_categorize_asked(self, thread, message_count)
    }

    fn set_thread_ai_auto_draft_asked(&self, thread: ThreadId, message_count: u32) -> Result<()> {
        write::set_thread_ai_auto_draft_asked(self, thread, message_count)
    }

    fn due_snoozed_threads(&self, now: Timestamp, limit: u32) -> Result<Vec<ThreadId>> {
        let mut sql = "SELECT id FROM threads WHERE snoozed_until_us IS NOT NULL \
             AND snoozed_until_us <= ?1"
            .to_string();
        self.page(&mut sql, "snoozed_until_us ASC", Some(limit), 0);
        let rows = self.read().query(&sql, &vals![to_us(now)])?;
        rows.into_iter()
            .map(|r| {
                r.text(0)?.parse().map_err(|e: <ThreadId as std::str::FromStr>::Err| {
                    everyday_core::error::Error::Invalid(e.to_string())
                })
            })
            .collect()
    }

    // ---- unread counts --------------------------------------------------------

    fn unread_counts(&self, account: AccountId) -> Result<Vec<(MailboxId, u64)>> {
        // `CAST(... AS BIGINT)`: summing a `BIGINT` column is exact in
        // SQLite and a `numeric` in Postgres, which this driver cannot
        // decode -- the same cast `purpose.rs` and `tasks.rs` already
        // reach for over `SUM(end_us - start_us)`.
        let rows = self.read().query(
            "SELECT mb.id, CAST(COALESCE(SUM(tm.unread), 0) AS BIGINT)
             FROM mailboxes mb
             LEFT JOIN thread_mailboxes tm ON tm.mailbox_id = mb.id
             WHERE mb.account_id = ?1
             GROUP BY mb.id",
            &vals![account.to_string()],
        )?;
        rows.into_iter()
            .map(|r| {
                let id: MailboxId =
                    r.text(0)?.parse().map_err(|e: <MailboxId as std::str::FromStr>::Err| {
                        everyday_core::error::Error::Invalid(e.to_string())
                    })?;
                Ok((id, r.u64(1)?))
            })
            .collect()
    }

    // ---- garbage collection --------------------------------------------------

    fn attachment_blob_refs(&self) -> Result<Vec<BlobId>> {
        let rows = self.read().records("SELECT message_id, data FROM bodies", &[])?;
        let mut out = Vec::new();
        for (id, data) in rows {
            let mid: MailMessageId =
                id.parse().map_err(|e: <MailMessageId as std::str::FromStr>::Err| {
                    everyday_core::error::Error::Invalid(e.to_string())
                })?;
            let body: Body = self.unseal(&body_aad(mid), &data)?;
            out.extend(body.parts.into_iter().filter_map(|p| p.blob));
        }
        Ok(out)
    }

    fn referenced_pack_ids(&self, account: AccountId) -> Result<Vec<PackId>> {
        let rows = self.read().query(
            "SELECT DISTINCT pack_id FROM mail_messages WHERE account_id = ?1",
            &vals![account.to_string()],
        )?;
        rows.into_iter()
            .map(|r| {
                r.text(0)?.parse().map_err(|e: <PackId as std::str::FromStr>::Err| {
                    everyday_core::error::Error::Invalid(e.to_string())
                })
            })
            .collect()
    }

    fn remap_packs(&self, account: AccountId, remap: &[(PackRef, PackRef)]) -> Result<()> {
        write::remap_packs(self, account, remap)
    }

    // ---- remote-image permissions --------------------------------------------
    //
    // Hand-written rather than through `Record`/`upsert`, on exactly
    // `agent_settings`' own reasoning (`crate::agent::AgentStore::settings`):
    // a fixed `id = 1` row is not a record with an id of its own, and there
    // is only ever one of these per vault.

    fn remote_image_settings(&self) -> Result<RemoteImageSettings> {
        let sealed =
            self.read().sealed("SELECT data FROM mail_remote_image_settings WHERE id = 1", &[])?;
        match sealed {
            Some(sealed) => self.unseal(&remote_image_settings_aad(), &sealed),
            None => Ok(RemoteImageSettings::default()),
        }
    }

    fn put_remote_image_settings(&self, settings: &RemoteImageSettings) -> Result<()> {
        let data = self.seal(&remote_image_settings_aad(), settings)?;
        self.write().execute(
            "INSERT INTO mail_remote_image_settings (id, data) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET data = ?1",
            &vals![data],
        )?;
        Ok(())
    }

    // ---- the contact index ----------------------------------------------------

    fn contacts(&self) -> Result<ContactBook> {
        let sealed = self.read().sealed("SELECT data FROM mail_contacts WHERE id = 1", &[])?;
        match sealed {
            Some(sealed) => self.unseal(&contacts_aad(), &sealed),
            None => Ok(ContactBook::default()),
        }
    }

    fn put_contacts(&self, book: &ContactBook) -> Result<()> {
        let data = self.seal(&contacts_aad(), book)?;
        self.write().execute(
            "INSERT INTO mail_contacts (id, data) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET data = ?1",
            &vals![data],
        )?;
        Ok(())
    }

    // ---- categorisation -------------------------------------------------

    fn category_rules(&self, account: AccountId) -> Result<CategoryRules> {
        let sealed = self.read().sealed(
            "SELECT data FROM mail_category_rules WHERE account_id = ?1",
            &vals![account.to_string()],
        )?;
        match sealed {
            Some(sealed) => self.unseal(&category_rules_aad(account), &sealed),
            None => Ok(CategoryRules::default()),
        }
    }

    fn put_category_rules(&self, account: AccountId, rules: &CategoryRules) -> Result<()> {
        let data = self.seal(&category_rules_aad(account), rules)?;
        self.write().execute(
            "INSERT INTO mail_category_rules (account_id, data) VALUES (?1, ?2)
             ON CONFLICT (account_id) DO UPDATE SET data = ?2",
            &vals![account.to_string(), data],
        )?;
        Ok(())
    }

    fn recategorize(&self, account: AccountId, rules: &CategoryRules) -> Result<u32> {
        write::recategorize(self, account, rules)
    }

    fn correct_category(
        &self,
        account: AccountId,
        target: CategoryMatch,
        category: Category,
    ) -> Result<u32> {
        write::correct_category(self, account, target, category)
    }
}
