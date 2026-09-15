//! Mail: mailboxes, messages, threads, bodies, drafts and the outbox.
//!
//! On exactly the terms every other domain in this module is: an optional
//! store, reached through [`Vault::with_domain`], refusing to mutate when
//! the vault holds no write claim. See [`crate::store::mail`] for the trait
//! this mirrors and the two-views-of-a-thread design behind it.
//!
//! No service commands are wired to most of this yet -- deliberately.
//! `everyday_service::domains::mail` exposes only the read-only trio
//! (`list_mailboxes`, `list_threads`, `get_thread`) for now; the sync
//! engine, the outbox drain loop and the rest of the write surface arrive
//! with the phases that actually drive them. Every method below exists so
//! that work has somewhere to land without another pass through this file.

use super::Vault;
use super::session::Domain;
use crate::error::Result;
use crate::id::{AccountId, DraftId, MailMessageId, MailboxId, ThreadId};
use crate::mail::{Body, Category, Draft, Mailbox, Message, MessageFlags, Op, Thread};
use crate::store::mail::{IngestMessage, MailStore, ThreadFilter, ThreadPage};
use jiff::Timestamp;

impl Vault {
    /// Does this vault's backend store mail?
    pub fn supports_mail(&self) -> bool {
        self.with_mail(|_| Ok(())).is_ok()
    }

    fn with_mail<T>(&self, f: impl FnOnce(&dyn MailStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Mail, |s| s.mail().map(f))
    }

    // ---- mailboxes ----------------------------------------------------------

    pub fn mailboxes(&self, account: AccountId) -> Result<Vec<Mailbox>> {
        self.with_mail(|m| m.list_mailboxes(account))
    }

    pub fn mailbox(&self, id: MailboxId) -> Result<Mailbox> {
        self.with_mail(|m| m.get_mailbox(id))
    }

    pub fn save_mailbox(&self, mailbox: &Mailbox) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.put_mailbox(mailbox))
    }

    pub fn delete_mailbox(&self, id: MailboxId) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.delete_mailbox(id))
    }

    // ---- bulk header ingest ------------------------------------------------

    pub fn ingest_mail(&self, account: AccountId, messages: Vec<IngestMessage>) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.ingest(account, messages))
    }

    // ---- flag / label changes, and removal -----------------------------------

    pub fn update_message_flags(
        &self,
        mailbox: MailboxId,
        uid: u32,
        flags: MessageFlags,
    ) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.update_flags(mailbox, uid, flags))
    }

    pub fn update_message_labels(
        &self,
        mailbox: MailboxId,
        uid: u32,
        labels: Vec<String>,
    ) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.update_labels(mailbox, uid, labels))
    }

    pub fn remove_mail_uids(&self, mailbox: MailboxId, uids: &[u32]) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.remove_uids(mailbox, uids))
    }

    // ---- the inbox query ----------------------------------------------------

    pub fn list_threads(
        &self,
        mailbox: MailboxId,
        filter: &ThreadFilter,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<ThreadPage> {
        self.with_mail(|m| m.list_threads(mailbox, filter, cursor, limit))
    }

    pub fn threads_in_category(
        &self,
        account: AccountId,
        category: Category,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<ThreadPage> {
        self.with_mail(|m| m.threads_in_category(account, category, cursor, limit))
    }

    // ---- thread detail ------------------------------------------------------

    pub fn thread(&self, id: ThreadId) -> Result<(Thread, Vec<Message>)> {
        self.with_mail(|m| m.thread(id))
    }

    // ---- bodies ------------------------------------------------------------

    pub fn save_body(&self, body: &Body) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.put_body(body))
    }

    pub fn body(&self, message_id: MailMessageId) -> Result<Body> {
        self.with_mail(|m| m.get_body(message_id))
    }

    // ---- drafts ------------------------------------------------------------

    pub fn save_draft(&self, draft: &Draft) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.put_draft(draft))
    }

    pub fn drafts(&self, account: AccountId) -> Result<Vec<Draft>> {
        self.with_mail(|m| m.list_drafts(account))
    }

    pub fn delete_draft(&self, id: DraftId) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.delete_draft(id))
    }

    // ---- the outbox ----------------------------------------------------------

    pub fn enqueue_op(&self, op: &Op) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.enqueue_op(op))
    }

    pub fn due_ops(&self, account: AccountId, now: Timestamp, limit: u32) -> Result<Vec<Op>> {
        self.with_mail(|m| m.due_ops(account, now, limit))
    }

    pub fn update_op(&self, op: &Op) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.update_op(op))
    }

    /// Every op of one origin kind -- `"person"`, `"assistant"`, `"routine"`
    /// or `"mcp"` -- most recently due first. What "what did the assistant
    /// do" reads.
    pub fn ops_by_origin(&self, kind: &str, limit: u32) -> Result<Vec<Op>> {
        self.with_mail(|m| m.ops_by_origin(kind, limit))
    }

    // ---- resolution and reset ------------------------------------------------

    pub fn message_by_uid(&self, mailbox: MailboxId, uid: u32) -> Result<Option<Message>> {
        self.with_mail(|m| m.message_by_uid(mailbox, uid))
    }

    pub fn message_by_message_id_header(
        &self,
        account: AccountId,
        message_id_header: &str,
    ) -> Result<Option<Message>> {
        self.with_mail(|m| m.message_by_message_id_header(account, message_id_header))
    }

    pub fn mail_uid_set(&self, mailbox: MailboxId) -> Result<Vec<u32>> {
        self.with_mail(|m| m.uid_set(mailbox))
    }

    pub fn reset_mailbox(&self, mailbox: MailboxId) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.reset_mailbox(mailbox))
    }

    // ---- unread counts --------------------------------------------------------

    pub fn mail_unread_counts(&self, account: AccountId) -> Result<Vec<(MailboxId, u64)>> {
        self.with_mail(|m| m.unread_counts(account))
    }
}
