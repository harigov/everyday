//! Mail: mailboxes, messages, threads, bodies, drafts and the outbox.
//!
//! On exactly the terms every other domain in this module is: an optional
//! store, reached through [`Vault::with_domain`], refusing to mutate when
//! the vault holds no write claim. See [`crate::store::mail`] for the trait
//! this mirrors and the two-views-of-a-thread design behind it.
//!
//! Phase 2 wired the read-only trio (`list_mailboxes`, `list_threads`,
//! `get_thread`). Phase 3 adds the write surface: the batch thread actions
//! (`apply_thread_ops`/`revert_thread_op`) and the draft lifecycle
//! (`queue_draft_send`/`undo_send`/`save_draft_and_append`/`discard_draft`)
//! `everyday_service::domains::mail`'s write commands are built on. What is
//! still missing is the sync engine's own write path -- ingest, flag
//! confirmation, reset -- which arrives with the account task that drives
//! it.

use super::Vault;
use super::session::{Domain, pick_domain};
use crate::error::{Error, Result};
use crate::id::{AccountId, DraftId, MailMessageId, MailboxId, OpId, ThreadId};
use crate::mail::{
    Body, Category, CategoryMatch, CategoryRules, ContactBook, Draft, DraftState, Mailbox,
    MailboxRole, Message, MessageFlags, Op, OpKind, OpState, OpTarget, Origin, RemoteImageSettings,
    Thread, apply_optimistic,
};
use crate::packstore::{PackRef, PackStore};
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

    /// Reach the backend's own pack store, for a backend that keeps raw
    /// messages in its tables rather than beside itself on disk -- Postgres,
    /// chiefly. See [`crate::store::JournalStore::mail_packs`] for why this
    /// is the exception rather than the rule: a local backend's pack store
    /// is opened directly from [`Vault::store_root`] and never reaches this
    /// method at all.
    pub fn with_mail_packs<T>(&self, f: impl FnOnce(&dyn PackStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Mail, |s| s.mail_packs().map(f))
    }

    /// A [`crate::packstore::ReferencedSnapshot`] safe to hand
    /// [`PackStore::compact`] for `account` -- see
    /// [`MailStore::referenced_snapshot`] for how it is built. What
    /// `everyday_service::mailsync::task` calls before every compaction
    /// attempt.
    pub fn mail_referenced_snapshot(
        &self,
        packs: &dyn PackStore,
        account: AccountId,
    ) -> Result<crate::packstore::ReferencedSnapshot> {
        self.with_mail(|m| m.referenced_snapshot(packs, account))
    }

    /// Rewrite the pack address of every message [`PackStore::compact`]
    /// moved -- see [`MailStore::remap_packs`] for the whole contract this
    /// is the vault-level entry point for.
    pub fn remap_packs(&self, account: AccountId, remap: &[(PackRef, PackRef)]) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.remap_packs(account, remap))
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

    /// See [`crate::store::mail::MailStore::delete_mailbox`] for what the
    /// returned pairs are and why: whoever calls this is expected to tell
    /// the pack store and the search index to forget each one, the same way
    /// `everyday_service::mailsync::passes` already does for
    /// [`Vault::remove_mail_uids`]'s own pairs.
    pub fn delete_mailbox(&self, id: MailboxId) -> Result<Vec<(MailMessageId, PackRef)>> {
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

    /// See [`crate::store::mail::MailStore::remove_uids`] for what the
    /// returned pairs are: every message this removal made genuinely dead,
    /// paired with the pack address it was stored under. The caller is
    /// expected to mark each one dead in the pack store and drop it from
    /// the search index -- see `everyday_service::mailsync::passes`'s own
    /// callers of this method for where that actually happens.
    pub fn remove_mail_uids(
        &self,
        mailbox: MailboxId,
        uids: &[u32],
    ) -> Result<Vec<(MailMessageId, PackRef)>> {
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

    /// Fold `others` into `keep` -- see
    /// [`crate::store::mail::MailStore::merge_threads`].
    pub fn merge_mail_threads(&self, keep: ThreadId, others: &[ThreadId]) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.merge_threads(keep, others))
    }

    pub fn mail_message(&self, id: MailMessageId) -> Result<Message> {
        self.with_mail(|m| m.get_message(id))
    }

    // ---- bodies ------------------------------------------------------------

    pub fn save_body(&self, body: &Body) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.put_body(body))
    }

    /// [`Vault::save_body`] for a whole batch -- see
    /// [`MailStore::put_bodies`] for why the bodies pass calls this instead
    /// of a loop.
    pub fn save_bodies(&self, bodies: &[Body]) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.put_bodies(bodies))
    }

    pub fn body(&self, message_id: MailMessageId) -> Result<Body> {
        self.with_mail(|m| m.get_body(message_id))
    }

    // ---- drafts ------------------------------------------------------------

    pub fn save_draft(&self, draft: &Draft) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.put_draft(draft))
    }

    pub fn draft(&self, id: DraftId) -> Result<Draft> {
        self.with_mail(|m| m.get_draft(id))
    }

    pub fn drafts(&self, account: AccountId) -> Result<Vec<Draft>> {
        self.with_mail(|m| m.list_drafts(account))
    }

    pub fn delete_draft(&self, id: DraftId) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.delete_draft(id))
    }

    /// Read `id`'s current [`Draft`], let `f` mutate exactly the fields it
    /// names, and write the result back -- all under the one write guard
    /// [`Vault::write`] takes for the whole call, so nothing else saving
    /// this same draft (a person's own debounced autosave, landing on the
    /// blocking pool at the same moment, most often) can interleave between
    /// this read and this write and lose either side's change.
    ///
    /// `f` returns whether it actually changed anything; a `false` skips
    /// the write (and the `updated_at` bump that would otherwise
    /// accompany it) entirely, which is what lets a caller check a
    /// condition -- "is this draft still queued under the op I think it
    /// is" -- and walk away cleanly when it no longer holds, exactly the
    /// way the bare read-then-save each of `everyday_service::outbox`'s
    /// `set_draft_message_id`, `on_success`'s `Appended` arm and
    /// `mark_draft_sent` used to before this existed, and the very race
    /// [`Vault::apply_thread_ops`], [`Vault::queue_draft_send`] and
    /// [`Vault::undo_send`] already avoid by holding the same guard for
    /// their own multi-step writes. Returns the draft either way -- a
    /// caller that skipped the write often still wants to see what is
    /// actually there.
    pub fn with_draft(&self, id: DraftId, f: impl FnOnce(&mut Draft) -> bool) -> Result<Draft> {
        self.writable()?;
        self.write(|u| {
            let mail = pick_domain(u.store.as_ref(), Domain::Mail, |s| s.mail())?;
            let mut draft = mail.get_draft(id)?;
            if f(&mut draft) {
                draft.updated_at = Timestamp::now();
                mail.put_draft(&draft)?;
            }
            Ok(draft)
        })
    }

    /// Set message `id`'s [`crate::mail::Invite`] directly -- what
    /// `everyday_service::domains::mail::respond_to_invite` calls once the
    /// reply is queued, so [`crate::mail::Invite::my_response`] reflects the
    /// answer immediately rather than waiting for a resync of the sent
    /// reply's own copy.
    pub fn set_message_invite(
        &self,
        id: MailMessageId,
        invite: Option<crate::mail::Invite>,
    ) -> Result<()> {
        self.writable()?;
        self.with_mail(move |m| m.set_message_invite(id, invite))
    }

    /// Undo `respond_to_invite`'s own optimistic write to `message_id`'s
    /// [`crate::mail::Invite::my_response`], once the RSVP it queued has
    /// failed permanently or been cancelled and the organiser was never
    /// actually told. Clears the field back to `None` -- "not yet
    /// responded" -- rather than restoring whatever it held before: nothing
    /// durable remembers that earlier value (an [`Op`] is a table row with
    /// no room for a snapshot of a message it does not even target), and
    /// `None` is the truthful state once the one write that would have
    /// justified anything else never reached the server. A message that
    /// carries no invitation, or none any more, is left alone -- there is
    /// nothing to revert, and this must never manufacture one.
    pub fn revert_invite_response(&self, message_id: MailMessageId) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| {
            let message = m.get_message(message_id)?;
            let Some(mut invite) = message.invite else { return Ok(()) };
            invite.my_response = None;
            m.set_message_invite(message_id, Some(invite))
        })
    }

    // ---- the outbox ----------------------------------------------------------

    pub fn enqueue_op(&self, op: &Op) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.enqueue_op(op))
    }

    pub fn due_ops(&self, account: AccountId, now: Timestamp, limit: u32) -> Result<Vec<Op>> {
        self.with_mail(|m| m.due_ops(account, now, limit))
    }

    /// The earliest `not_before` among `account`'s still-pending ops --
    /// what the account task sleeps until, on top of its poll and `IDLE`
    /// waits, so a send-at or undo-send op is drained the moment its window
    /// expires. See [`MailStore::next_pending_op_at`].
    pub fn next_pending_op_at(&self, account: AccountId) -> Result<Option<Timestamp>> {
        self.with_mail(|m| m.next_pending_op_at(account))
    }

    /// Every op of `account`'s stranded [`crate::mail::OpState::InFlight`]
    /// by a crash mid-drain. See [`MailStore::in_flight_ops`].
    pub fn in_flight_ops(&self, account: AccountId) -> Result<Vec<Op>> {
        self.with_mail(|m| m.in_flight_ops(account))
    }

    pub fn update_op(&self, op: &Op) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.update_op(op))
    }

    pub fn op(&self, id: OpId) -> Result<Op> {
        self.with_mail(|m| m.get_op(id))
    }

    /// Every op of one origin kind -- `"person"`, `"assistant"`, `"routine"`
    /// or `"mcp"` -- most recently due first. What "what did the assistant
    /// do" reads.
    pub fn ops_by_origin(&self, kind: &str, limit: u32) -> Result<Vec<Op>> {
        self.with_mail(|m| m.ops_by_origin(kind, limit))
    }

    /// A thread's own "recent actions" -- see [`MailStore::ops_for_thread`].
    pub fn ops_for_thread(&self, thread: ThreadId, limit: u32) -> Result<Vec<Op>> {
        self.with_mail(|m| m.ops_for_thread(thread, limit))
    }

    // ---- resolution and reset ------------------------------------------------

    /// Every `(mailbox, uid)` pair message `id` is currently filed under --
    /// what the outbox executor's `Lookups` implementation resolves a
    /// target against.
    pub fn mail_message_locations(&self, id: MailMessageId) -> Result<Vec<(MailboxId, u32)>> {
        self.with_mail(|m| m.message_locations(id))
    }

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

    /// Every message in `mailbox` still waiting for its body, with its uid
    /// in that mailbox, newest first, capped at `limit` -- see
    /// [`crate::store::mail::MailStore::pending_bodies`].
    pub fn mail_pending_bodies(
        &self,
        mailbox: MailboxId,
        limit: u32,
    ) -> Result<Vec<(Message, u32)>> {
        self.with_mail(|m| m.pending_bodies(mailbox, limit))
    }

    pub fn reset_mailbox(&self, mailbox: MailboxId) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.reset_mailbox(mailbox))
    }

    // ---- unread counts --------------------------------------------------------

    pub fn mail_unread_counts(&self, account: AccountId) -> Result<Vec<(MailboxId, u64)>> {
        self.with_mail(|m| m.unread_counts(account))
    }

    // ---- remote-image permissions --------------------------------------------

    pub fn remote_image_settings(&self) -> Result<RemoteImageSettings> {
        self.with_mail(|m| m.remote_image_settings())
    }

    pub fn save_remote_image_settings(&self, settings: &RemoteImageSettings) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.put_remote_image_settings(settings))
    }

    /// The sealed contact index -- see
    /// [`crate::store::mail::MailStore::contacts`].
    pub fn mail_contacts(&self) -> Result<ContactBook> {
        self.with_mail(|m| m.contacts())
    }

    pub fn save_mail_contacts(&self, book: &ContactBook) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.put_contacts(book))
    }

    // ---- categorisation -----------------------------------------------------

    pub fn category_rules(&self, account: AccountId) -> Result<CategoryRules> {
        self.with_mail(|m| m.category_rules(account))
    }

    pub fn save_category_rules(&self, account: AccountId, rules: &CategoryRules) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.put_category_rules(account, rules))
    }

    /// Record a sender-address correction for `account` and sweep only the
    /// messages it names through it — see
    /// [`crate::store::mail::MailStore::correct_category`] for what "sweep"
    /// costs, why it is accepted here, and why it reaches only `sender`'s
    /// own mail rather than the whole account. Returns how many messages
    /// changed, for a caller that wants to say so.
    pub fn correct_mail_category(
        &self,
        account: AccountId,
        sender: &str,
        category: Category,
    ) -> Result<u32> {
        self.writable()?;
        self.with_mail(|m| {
            let mut rules = m.category_rules(account)?;
            rules.set_sender(sender, category);
            m.put_category_rules(account, &rules)?;
            m.correct_category(account, CategoryMatch::Sender(sender.to_string()), category)
        })
    }

    /// Re-run categorisation over `account`'s mail from scratch, against its
    /// current corrections — the one-off `recategorize_mail` backfill.
    pub fn recategorize_mail(&self, account: AccountId) -> Result<u32> {
        self.writable()?;
        self.with_mail(|m| {
            let rules = m.category_rules(account)?;
            m.recategorize(account, &rules)
        })
    }

    /// Set one message's category directly, from a model's one-off answer —
    /// see [`crate::store::mail::MailStore::set_message_category`] for why
    /// this is deliberately not [`Vault::correct_mail_category`].
    pub fn set_mail_message_category(&self, id: MailMessageId, category: Category) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.set_message_category(id, category))
    }

    /// See
    /// [`crate::store::mail::MailStore::set_thread_ai_categorize_asked`].
    pub fn set_thread_ai_categorize_asked(
        &self,
        thread: ThreadId,
        message_count: u32,
    ) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.set_thread_ai_categorize_asked(thread, message_count))
    }

    /// See
    /// [`crate::store::mail::MailStore::set_thread_ai_auto_draft_asked`].
    pub fn set_thread_ai_auto_draft_asked(
        &self,
        thread: ThreadId,
        message_count: u32,
    ) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.set_thread_ai_auto_draft_asked(thread, message_count))
    }

    // ---- releasing a snooze -----------------------------------------------

    /// Every thread, across every account, whose snooze has passed `now` --
    /// what the minute scheduler reads once a tick to decide which threads
    /// return to the inbox. See [`Vault::release_snooze`] for the write
    /// half.
    pub fn due_snoozed_threads(&self, now: Timestamp, limit: u32) -> Result<Vec<ThreadId>> {
        self.with_mail(|m| m.due_snoozed_threads(now, limit))
    }

    /// Bring one snoozed thread back to the inbox -- clears
    /// `Thread::snoozed_until`. No outbox op: snooze never told the server
    /// anything to begin with, so there is nothing for the server to be
    /// told is over either.
    pub fn release_snooze(&self, thread: ThreadId) -> Result<()> {
        self.writable()?;
        self.with_mail(|m| m.set_thread_snoozed_until(thread, None))
    }

    // ---- batch thread actions ----------------------------------------------
    //
    // The one entry point every write command in `everyday_service::domains::mail`
    // that acts on a batch of threads goes through: `docs/plans/mail.md`'s
    // "What an action does" asks for one vault write per action, covering
    // both the optimistic local change and the `Op` that tells the account
    // task to make the server agree. A single [`Vault::write`] closure below
    // is what makes that literally one write -- the vault's own state lock
    // is held for the whole batch, so nothing else reading through this
    // `Vault` ever observes a thread whose local row has changed but whose
    // op has not been enqueued yet, or the other way round.

    /// Apply `kind`'s local effect to every thread in `threads`, and enqueue
    /// one [`Op`] per thread with `origin`. Returns the enqueued ops, so a
    /// command can hand back change ids without a second read.
    ///
    /// What "local effect" means depends on `kind`:
    ///
    /// * [`OpKind::MarkRead`], `MarkUnread`, `Star`, `Unstar`, `Label` and
    ///   `Unlabel` — [`apply_optimistic`] applied to every message of the
    ///   thread in turn, written back through
    ///   [`MailStore::set_message_flags`] or
    ///   [`MailStore::set_message_labels`].
    /// * [`OpKind::Archive`], `Trash` and `Move` — the thread is hidden from
    ///   the account's Inbox mailbox (see
    ///   [`MailStore::hide_thread_from_mailbox`]) if it is filed there. A
    ///   thread reached through a view other than the inbox is not
    ///   otherwise touched locally; the sync engine's own ingest of the
    ///   op's eventual confirmation is what actually files it under its new
    ///   home.
    /// * [`OpKind::Snooze`] — [`MailStore::set_thread_snoozed_until`].
    ///
    /// [`OpKind::Send`] and [`OpKind::AppendDraft`] are refused with
    /// [`Error::Invalid`]: both target a [`crate::id::DraftId`], never a
    /// thread, and belong to [`Vault::queue_draft_send`] and
    /// [`Vault::save_draft_and_append`] instead.
    pub fn apply_thread_ops(
        &self,
        threads: &[ThreadId],
        kind: OpKind,
        origin: Origin,
    ) -> Result<Vec<Op>> {
        if matches!(kind, OpKind::Send | OpKind::AppendDraft | OpKind::DiscardDraft) {
            return Err(Error::Invalid(format!("{kind:?} does not target a thread")));
        }
        self.writable()?;
        self.write(|u| {
            let mail = pick_domain(u.store.as_ref(), Domain::Mail, |s| s.mail())?;
            let mut ops = Vec::with_capacity(threads.len());
            for &thread_id in threads {
                let (thread, messages) = mail.thread(thread_id)?;
                apply_local_effect(mail, &thread, &messages, &kind)?;
                let op = Op::new(
                    thread.account_id,
                    kind.clone(),
                    OpTarget::Thread(thread_id),
                    origin.clone(),
                );
                mail.enqueue_op(&op)?;
                ops.push(op);
            }
            Ok(ops)
        })
    }

    /// Undo [`Vault::apply_thread_ops`]'s local effect for one thread, once
    /// its op has failed permanently -- see
    /// [`crate::mail::outbox::apply_optimistic`]'s own docs on why a
    /// flag-changing op's exact undo is its logical opposite (`MarkRead`
    /// undoes with `MarkUnread`, and so on) rather than a snapshot carried
    /// forward from enqueue time: an [`Op`] is a table row that can survive
    /// a restart, and nothing on it holds a message-by-message "what it was
    /// before" to replay days later.
    pub fn revert_thread_op(&self, thread: ThreadId, kind: &OpKind) -> Result<()> {
        self.writable()?;
        self.write(|u| {
            let mail = pick_domain(u.store.as_ref(), Domain::Mail, |s| s.mail())?;
            match inverse_of(kind) {
                Inverse::Flag(inverse) => {
                    let (_, messages) = mail.thread(thread)?;
                    for mut message in messages {
                        apply_local_effect_one(mail, &mut message, &inverse)?;
                    }
                    Ok(())
                }
                Inverse::RestoreMailboxes => mail.restore_thread_mailboxes(thread),
                Inverse::ClearSnooze => mail.set_thread_snoozed_until(thread, None),
                Inverse::None => Ok(()),
            }
        })
    }

    // ---- sending and saving a draft -----------------------------------------

    /// `draft`'s local write and its `Send` op, together: sets
    /// `Draft::state` to [`DraftState::Queued`] naming the freshly enqueued
    /// op, whose `not_before` is `not_before` — the undo-send window, or a
    /// later moment for send-later. Returns the updated draft and the op.
    pub fn queue_draft_send(
        &self,
        draft_id: DraftId,
        not_before: Timestamp,
        origin: Origin,
    ) -> Result<(Draft, Op)> {
        self.writable()?;
        self.write(|u| {
            let mail = pick_domain(u.store.as_ref(), Domain::Mail, |s| s.mail())?;
            let mut draft = mail.get_draft(draft_id)?;
            if !matches!(draft.state, DraftState::Editing) {
                return Err(Error::Invalid(format!(
                    "a draft in state {:?} cannot be queued to send",
                    draft.state
                )));
            }
            let op = Op::new(draft.account_id, OpKind::Send, OpTarget::Draft(draft_id), origin)
                .not_before(not_before);
            mail.enqueue_op(&op)?;
            draft.state = DraftState::Queued { op: op.id };
            draft.updated_at = Timestamp::now();
            mail.put_draft(&draft)?;
            Ok((draft, op))
        })
    }

    /// Cancel a queued send, provided its op is still [`OpState::Pending`],
    /// has never actually been attempted, and its `not_before` has not yet
    /// passed -- undo send's whole implementation.
    ///
    /// "Never actually been attempted" is its own check, separate from the
    /// window: the drain loop's retry arm
    /// (`everyday_service::outbox::drain_outbox`) returns a `Send` op that
    /// failed with a *retryable* error to exactly `Pending`, with
    /// `not_before` pushed out by the backoff schedule (thirty seconds up
    /// to an hour) -- so `state == Pending && now < not_before` alone would
    /// reopen the undo-send window for up to an hour after an attempt that
    /// may already have reached the SMTP server. `everyday_mail::outbox::send`'s
    /// own docs explain why: a `Network` error after `DATA`'s final `.`
    /// looks, from here, identical to one before the server ever saw a
    /// byte, so an op with `attempts > 0` (bumped the moment any attempt
    /// fails) or a draft that already carries a minted `message_id`
    /// (stamped, and persisted, immediately before that same attempt) has
    /// to be treated as "may have gone out" for good -- undo can no longer
    /// promise the thing it promises. Past that point the account task may
    /// already be mid-send anyway, so this refuses with [`Error::Invalid`]
    /// rather than risk racing it; the draft stays `Queued` and the caller
    /// is told plainly why, with a distinct message for each reason a
    /// person might otherwise mistake for "it's too late" -- an op that
    /// simply failed and was never sent has nothing to undo, which is a
    /// different thing to hear than "this may already be sent".
    ///
    /// `message_id` is deliberately left on `draft` when this *does*
    /// succeed: the checks above already guarantee it is `None` whenever
    /// undo is still possible (a `Send` op that has never been attempted
    /// never reached the point in `send` that mints one), so there is
    /// nothing to clear. Clearing it defensively here would cost the very
    /// guarantee `docs/plans/mail.md`'s stable-`Message-ID` design exists
    /// for: a *later* send of this same draft, after a genuine edit, still
    /// deserves a fresh id of its own the first time `send` builds it, and
    /// it gets one -- `send` only reuses `draft.message_id` when it is
    /// already `Some`.
    pub fn undo_send(&self, draft_id: DraftId, now: Timestamp) -> Result<Draft> {
        self.writable()?;
        self.write(|u| {
            let mail = pick_domain(u.store.as_ref(), Domain::Mail, |s| s.mail())?;
            let mut draft = mail.get_draft(draft_id)?;
            let DraftState::Queued { op: op_id } = draft.state else {
                return Err(Error::Invalid("this draft is not queued to send".into()));
            };
            let mut op = mail.get_op(op_id)?;

            if !matches!(op.state, OpState::Pending) {
                let message = match &op.state {
                    OpState::InFlight => {
                        "this message is being sent right now and cannot be undone"
                    }
                    OpState::Cancelled => "this send has already been cancelled",
                    OpState::Failed { permanent: true, .. } => {
                        "this send already failed and was never sent; there is nothing to undo"
                    }
                    // `Pending` is excluded by the outer `matches!`, and
                    // `Failed { permanent: false }` never actually reaches
                    // `Queued`'s own op -- a retryable failure goes back to
                    // `Pending`, not `Failed` -- but a future caller of
                    // `transition_to` is not this match's business to
                    // predict, so it still gets a true answer rather than
                    // a wildcard borrowing `Done`'s.
                    OpState::Done => "this message has already been sent",
                    OpState::Failed { permanent: false, .. } | OpState::Pending => {
                        "the undo-send window has passed; this message may already be sent"
                    }
                };
                return Err(Error::Invalid(message.into()));
            }
            if op.attempts > 0 || draft.message_id.is_some() {
                return Err(Error::Invalid(
                    "this message has already been attempted and may have reached the server; \
                     it can no longer be undone"
                        .into(),
                ));
            }
            if now >= op.not_before {
                return Err(Error::Invalid(
                    "the undo-send window has passed; this message may already be sent".into(),
                ));
            }

            op.transition_to(OpState::Cancelled)?;
            mail.update_op(&op)?;
            draft.state = DraftState::Editing;
            draft.updated_at = Timestamp::now();
            mail.put_draft(&draft)?;
            Ok(draft)
        })
    }

    /// Save `draft`, and — when `append` says to — enqueue an
    /// [`OpKind::AppendDraft`] in the same write. The debounce that decides
    /// `append` (no more than once every thirty seconds per draft) is
    /// `everyday_service::domains::mail::save_draft`'s job, not this one's:
    /// it needs to remember *when* a draft last appended across calls,
    /// which is session state, not a vault write.
    pub fn save_draft_and_append(
        &self,
        draft: &Draft,
        append: bool,
        origin: Origin,
    ) -> Result<Option<Op>> {
        self.writable()?;
        self.write(|u| {
            let mail = pick_domain(u.store.as_ref(), Domain::Mail, |s| s.mail())?;
            mail.put_draft(draft)?;
            if !append {
                return Ok(None);
            }
            let op =
                Op::new(draft.account_id, OpKind::AppendDraft, OpTarget::Draft(draft.id), origin);
            mail.enqueue_op(&op)?;
            Ok(Some(op))
        })
    }

    /// Discard `draft`: [`DraftState::Discarded`], one write, plus an
    /// [`OpKind::DiscardDraft`] op when the draft ever had a
    /// [`Draft::server_copy`] -- a copy an earlier `AppendDraft` left on the
    /// server does not go away just because this process stops thinking
    /// about the draft locally, so the account task is told to remove it.
    /// A draft discarded before its first autosave ever reached the server
    /// has nothing to delete, so no op is enqueued for it. Uses `draft`'s
    /// own [`Draft::origin`] for the op's, on the same reasoning
    /// [`Vault::apply_thread_ops`]'s callers already lean on: whoever wrote
    /// the draft is who this bookkeeping op is done on behalf of, and it
    /// keeps this method's own signature exactly what every existing caller
    /// already passes.
    pub fn discard_draft(&self, draft_id: DraftId) -> Result<(Draft, Option<Op>)> {
        self.writable()?;
        self.write(|u| {
            let mail = pick_domain(u.store.as_ref(), Domain::Mail, |s| s.mail())?;
            let mut draft = mail.get_draft(draft_id)?;
            // A discard racing a still-pending `Send` must win: otherwise
            // the op the person just asked to cancel by discarding drains
            // anyway, mailing the very message they discarded, while the
            // draft row itself already reads `Discarded`. This is exactly
            // [`Vault::undo_send`]'s own `Pending` -> `Cancelled`
            // transition, reached from a different door -- an op that has
            // moved past `Pending` (already `InFlight`, or further) is left
            // alone, on the same reasoning `undo_send` refuses past that
            // point: the account task may already be mid-send, and racing
            // it here would not stop anything. `send`'s own executor
            // (`everyday_mail::outbox::send`) refuses to run at all against
            // a `Discarded` draft regardless, as the last line of defence
            // for exactly that race.
            if let DraftState::Queued { op: op_id } = draft.state {
                let mut queued = mail.get_op(op_id)?;
                if matches!(queued.state, OpState::Pending) {
                    queued.transition_to(OpState::Cancelled)?;
                    mail.update_op(&queued)?;
                }
            }
            draft.state = DraftState::Discarded;
            draft.updated_at = Timestamp::now();
            mail.put_draft(&draft)?;
            let op = match &draft.server_copy {
                Some(_) => {
                    let op = Op::new(
                        draft.account_id,
                        OpKind::DiscardDraft,
                        OpTarget::Draft(draft.id),
                        draft.origin.clone(),
                    );
                    mail.enqueue_op(&op)?;
                    Some(op)
                }
                None => None,
            };
            Ok((draft, op))
        })
    }
}

/// What [`Vault::revert_thread_op`] undoes a given [`OpKind`] with.
enum Inverse {
    Flag(OpKind),
    RestoreMailboxes,
    ClearSnooze,
    /// [`OpKind::Snooze`]'s own moment has not been reverted-into by
    /// anything but [`ClearSnooze`] above; this arm exists only so the
    /// match in [`Vault::revert_thread_op`] is total without a wildcard
    /// hiding a future [`OpKind`] this function has not been taught yet.
    None,
}

fn inverse_of(kind: &OpKind) -> Inverse {
    match kind {
        OpKind::MarkRead => Inverse::Flag(OpKind::MarkUnread),
        OpKind::MarkUnread => Inverse::Flag(OpKind::MarkRead),
        OpKind::Star => Inverse::Flag(OpKind::Unstar),
        OpKind::Unstar => Inverse::Flag(OpKind::Star),
        OpKind::Label { label } => Inverse::Flag(OpKind::Unlabel { label: label.clone() }),
        OpKind::Unlabel { label } => Inverse::Flag(OpKind::Label { label: label.clone() }),
        OpKind::Archive | OpKind::Trash | OpKind::Move { .. } => Inverse::RestoreMailboxes,
        OpKind::Snooze { .. } => Inverse::ClearSnooze,
        OpKind::Send | OpKind::AppendDraft | OpKind::DiscardDraft => Inverse::None,
    }
}

/// [`Vault::apply_thread_ops`]'s per-thread dispatch: what a flag-changing
/// kind does to every message, what an archive/trash/move kind does to the
/// thread's place in the account's Inbox, what a snooze kind does to the
/// thread itself.
fn apply_local_effect(
    mail: &dyn MailStore,
    thread: &Thread,
    messages: &[Message],
    kind: &OpKind,
) -> Result<()> {
    if kind.is_flag_change() {
        for message in messages {
            let mut message = message.clone();
            apply_local_effect_one(mail, &mut message, kind)?;
        }
        return Ok(());
    }
    match kind {
        OpKind::Archive | OpKind::Trash | OpKind::Move { .. } => {
            if let Some(inbox) = inbox_mailbox(mail, thread.account_id)? {
                mail.hide_thread_from_mailbox(thread.id, inbox)?;
            }
        }
        OpKind::Snooze { until } => {
            mail.set_thread_snoozed_until(thread.id, Some(*until))?;
        }
        // `is_flag_change` above already took MarkRead/MarkUnread/Star/
        // Unstar/Label/Unlabel; Send, AppendDraft and DiscardDraft never
        // reach this function -- `apply_thread_ops` refuses them before its
        // loop even starts.
        OpKind::MarkRead
        | OpKind::MarkUnread
        | OpKind::Star
        | OpKind::Unstar
        | OpKind::Label { .. }
        | OpKind::Unlabel { .. }
        | OpKind::Send
        | OpKind::AppendDraft
        | OpKind::DiscardDraft => unreachable!("handled above or refused before this is called"),
    }
    Ok(())
}

/// [`apply_optimistic`], written back to storage -- the one-message half of
/// [`apply_local_effect`], and the whole of what [`Vault::revert_thread_op`]
/// needs for a flag-changing kind.
fn apply_local_effect_one(
    mail: &dyn MailStore,
    message: &mut Message,
    kind: &OpKind,
) -> Result<()> {
    let Some(_undo) = apply_optimistic(kind, message) else { return Ok(()) };
    match kind {
        OpKind::Label { .. } | OpKind::Unlabel { .. } => {
            mail.set_message_labels(message.id, message.labels.clone())
        }
        _ => mail.set_message_flags(message.id, message.flags),
    }
}

fn inbox_mailbox(mail: &dyn MailStore, account: AccountId) -> Result<Option<MailboxId>> {
    Ok(mail
        .list_mailboxes(account)?
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .map(|m| m.id))
}
