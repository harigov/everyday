//! Storage for mail: mailboxes, messages, threads, bodies, drafts and the
//! outbox.
//!
//! `docs/plans/mail.md` calls this phase 2's storage half, plus the
//! draft/outbox records phase 3 needs early. Eight tables in one trait
//! (compare [`library::LibraryStore`](super::library), which is three) --
//! justified by how tightly they depend on each other: an ingested header
//! updates a thread's aggregates in the same write, a flag change updates
//! the same aggregates by a different door, and a removal can delete a
//! thread outright. Splitting mailboxes from messages from threads across
//! separate traits would not remove that coupling, only hide it behind a
//! seam nothing crosses cleanly.
//!
//! # Two views of a thread, on purpose
//!
//! [`MailStore::thread`] answers with a [`Thread`]'s own aggregates --
//! `message_count`, `unread_count`, `last_date` -- counted over *every*
//! message in it, in whichever mailboxes they are filed under. A mailbox's
//! own list, [`MailStore::list_threads`], pages over `thread_mailboxes`
//! instead, which keeps a *second*, per-mailbox count of the same thread:
//! how many of its messages are actually filed in *this* mailbox, and when
//! the most recent of those arrived. The two are allowed to disagree -- a
//! thread with a reply that landed only in Sent, or a label applied to one
//! message but not the rest, must not make every mailbox it touches jump to
//! the top of its own list. [`MailStore::ingest`],
//! [`MailStore::update_flags`], [`MailStore::update_labels`] and
//! [`MailStore::remove_uids`] all recompute both views, in the same write,
//! every time either one could have changed.
//!
//! # Clear columns
//!
//! What an index is built from, and nothing else: a mailbox's role and its
//! sync cursors (a folder called "Sent" is not a secret, and a sync task has
//! to read them before it has decrypted anything); a message's thread,
//! date, flags (packed, see [`crate::mail::MessageFlags::bits`]), size,
//! category and pack address; a thread's own aggregates; an op's account,
//! state, origin *kind* and `not_before`; a draft's account, state, origin
//! kind and `in_reply_to`. Every subject, every address, every body, every
//! label name and every folder's actual name stay sealed -- see
//! `everyday-store-sql`'s own module docs for the exact table, which this
//! trait's implementation must agree with column for column.

use crate::error::Result;
use crate::id::{AccountId, BlobId, DraftId, MailMessageId, MailboxId, OpId, ThreadId};
use crate::mail::{
    Body, Category, ContactBook, Draft, Invite, Mailbox, Message, MessageFlags, Op,
    RemoteImageSettings, Thread,
};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// One header the sync engine fetched, on its way into [`MailStore::ingest`].
///
/// `message` already carries its resolved [`crate::mail::Message::thread_id`]
/// -- threading itself (the server's own thread id where one exists, JWZ
/// where it does not) is `everyday-mail`'s job, done before this is built,
/// not this trait's. `mailbox` and `uid` are where the sync engine found it,
/// which is what lets the same physical message arrive twice under two
/// different Gmail labels without being two rows: `ingest` upserts by
/// `message.id`, and writes one `message_mailboxes` row per distinct
/// `(mailbox, uid)` an [`IngestMessage`] named for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestMessage {
    pub message: Message,
    pub mailbox: MailboxId,
    pub uid: u32,
}

/// Filters for [`MailStore::list_threads`]. `None` on any field means "do
/// not filter on this at all"; every set field is ANDed with the rest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ThreadFilter {
    /// `Some(true)` for only threads with an unread message in this
    /// mailbox; `Some(false)` for only fully-read ones.
    pub unread: Option<bool>,
    pub category: Option<Category>,
    /// `Some(true)` for only snoozed threads (`snoozed_until` in the
    /// future); `Some(false)` for only threads that are not.
    pub snoozed: Option<bool>,
}

/// One page of [`Thread`]s, keyset-paged -- see `everyday-store-sql::keyset`
/// for why a mailbox pages this way rather than by offset.
///
/// `next_cursor` is opaque, exactly the way
/// [`KeyCursor::encode`](../../everyday_store_sql/keyset/struct.KeyCursor.html#method.encode)
/// is: a caller holds the string and hands it back as `cursor` on the next
/// call, and never inspects it. `None` means this was the last page. The
/// type carrying it is `String`, not the store crate's own cursor type,
/// because this trait lives in a crate that `everyday-store-sql` depends on,
/// never the other way round.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPage {
    pub threads: Vec<Thread>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// The persistence contract for mail.
pub trait MailStore: Send + Sync {
    // ---- mailboxes --------------------------------------------------------

    /// Every mailbox synced for `account`, in no particular order -- a
    /// handful to a few dozen rows, so sorting for display is the caller's
    /// business, the same contract [`AccountStore::list_accounts`]
    /// (`super::accounts::AccountStore::list_accounts`) makes.
    fn list_mailboxes(&self, account: AccountId) -> Result<Vec<Mailbox>>;

    fn get_mailbox(&self, id: MailboxId) -> Result<Mailbox>;

    /// Insert or replace, including its sync cursors. Implementations must
    /// be idempotent.
    fn put_mailbox(&self, mailbox: &Mailbox) -> Result<()>;

    /// Delete the mailbox and every `message_mailboxes` /
    /// `thread_mailboxes` row naming it, recomputing (and, where a thread's
    /// last member elsewhere is also gone, deleting) every thread this
    /// touches. Messages that are members of another mailbox too --
    /// Gmail's All Mail, most often -- survive; a message with no mailbox
    /// left at all is deleted along with its body.
    fn delete_mailbox(&self, id: MailboxId) -> Result<()>;

    // ---- bulk header ingest ------------------------------------------------

    /// Upsert every message in `messages`, their `message_mailboxes` rows,
    /// and recompute every thread and `thread_mailboxes` row any of them
    /// touched -- see the module docs for what "recompute" means for each.
    ///
    /// One call is one unit of durability from the caller's point of view,
    /// but not necessarily one transaction: a first sync hands this
    /// thousands of headers, and an implementation may batch internally
    /// (see `everyday-store-sql`'s `upsert_batched`) so a giant mailbox
    /// never holds the vault's single writer for the length of the whole
    /// call. `messages` empty is a no-op.
    fn ingest(&self, account: AccountId, messages: Vec<IngestMessage>) -> Result<()>;

    // ---- flag / label changes, and removal, by (mailbox, uid) --------------

    /// Change the flags of the message filed as `uid` in `mailbox`,
    /// recomputing the thread it belongs to and every `thread_mailboxes` row
    /// naming it. A `(mailbox, uid)` this store has never ingested is not an
    /// error -- the sync engine's own diff can name a uid that arrived and
    /// left between two syncs -- and is a no-op.
    fn update_flags(&self, mailbox: MailboxId, uid: u32, flags: MessageFlags) -> Result<()>;

    /// Replace the label set of the message filed as `uid` in `mailbox`.
    /// Whole-set rather than add/remove, the same contract
    /// [`crate::account::Account::identities`]'s callers already keep for a
    /// list a caller has fully in hand: the sync engine reads the server's
    /// `X-GM-LABELS` (or IMAP keywords) whole and hands the whole set back.
    fn update_labels(&self, mailbox: MailboxId, uid: u32, labels: Vec<String>) -> Result<()>;

    /// Remove every message named by `(mailbox, uid)` in `uids` from that
    /// mailbox -- an `EXPUNGE`, or a Gmail label removed. Recomputes the
    /// thread and every `thread_mailboxes` row each touched message
    /// belonged to; a message left with no mailbox at all is deleted, body
    /// and all. `uids` empty is a no-op.
    fn remove_uids(&self, mailbox: MailboxId, uids: &[u32]) -> Result<()>;

    // ---- the inbox query ----------------------------------------------------

    /// One mailbox's threads, newest activity in *this mailbox* first,
    /// keyset-paged over `thread_mailboxes (mailbox_id, last_date_us DESC,
    /// thread_id)` -- the index the plan names as the one the inbox reads.
    /// Returns [`Thread`] summaries only, never a message body.
    fn list_threads(
        &self,
        mailbox: MailboxId,
        filter: &ThreadFilter,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<ThreadPage>;

    /// Every thread of `account`'s carrying `category`, across every
    /// mailbox, newest first. What a split-inbox tab reads, once phase 7
    /// assigns categories -- the query works today against whatever
    /// `category` this store already holds.
    fn threads_in_category(
        &self,
        account: AccountId,
        category: Category,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<ThreadPage>;

    // ---- thread detail ------------------------------------------------------

    /// One thread's own record, and every message in it, oldest first --
    /// what opening a thread reads. Bodies are not included; a reader asks
    /// for each with [`MailStore::get_body`] as it draws them, which is what
    /// keeps opening a thread from paying for every message's text at once.
    fn thread(&self, id: ThreadId) -> Result<(Thread, Vec<Message>)>;

    /// Fold `others` into `keep`: every message currently filed under one
    /// of `others` moves to `keep`, `keep`'s own aggregates and
    /// `thread_mailboxes` rows are recomputed from the result, and every
    /// thread in `others` -- left with no messages at all -- is deleted
    /// along with its own `thread_mailboxes` rows, the same way
    /// [`MailStore::remove_uids`] already deletes a thread emptied by a
    /// removal.
    ///
    /// What a real threading `Merge` resolves to when a message's
    /// `References` chain names more than one thread this account already
    /// has -- two separate conversations turning out, on new evidence, to
    /// be the same one. `others` empty is a no-op.
    fn merge_threads(&self, keep: ThreadId, others: &[ThreadId]) -> Result<()>;

    /// One message by its own id, headers and flags only — never its body.
    /// What `everyday-service::mailview` reads to find a message's sender
    /// before it will answer a `remote_image` request for it: a permission
    /// check needs the `From` address, and a thread view already has a
    /// `Vec<Message>` in hand for everything else, so this exists only for
    /// the caller that has nothing but the id. Also what composing a reply or
    /// a forward reads a subject, quoted body and recipients from, and what
    /// the outbox executor's `Lookups` reads a reply's parent from.
    fn get_message(&self, id: MailMessageId) -> Result<Message>;

    // ---- bodies ------------------------------------------------------------

    fn put_body(&self, body: &Body) -> Result<()>;

    /// [`MailStore::put_body`] for a whole batch, in one transaction where
    /// the backend can offer one -- what the bodies pass calls instead of a
    /// loop, so committing a batch of a hundred messages is one fsync
    /// rather than a hundred. The default just loops; a backend with no
    /// notion of a multi-statement transaction loses nothing by not
    /// overriding it.
    fn put_bodies(&self, bodies: &[Body]) -> Result<()> {
        for body in bodies {
            self.put_body(body)?;
        }
        Ok(())
    }

    fn get_body(&self, message_id: MailMessageId) -> Result<Body>;

    // ---- drafts ------------------------------------------------------------

    fn put_draft(&self, draft: &Draft) -> Result<()>;

    /// One draft by id -- what the compose window opens, and what
    /// `send_draft`/`undo_send`/`discard_draft` all load before deciding
    /// whether the state transition they are asked for is legal.
    fn get_draft(&self, id: DraftId) -> Result<Draft>;

    /// Every draft of `account`'s, most recently touched first -- Drafts,
    /// unfiltered; the interface narrows by [`crate::mail::DraftState`]
    /// itself, the same way a shelf narrows by
    /// [`crate::library::ItemStatus`] in Rust rather than in a second query
    /// shape.
    fn list_drafts(&self, account: AccountId) -> Result<Vec<Draft>>;

    fn delete_draft(&self, id: DraftId) -> Result<()>;

    // ---- the outbox ----------------------------------------------------------

    fn enqueue_op(&self, op: &Op) -> Result<()>;

    /// Every op of `account`'s that is [`crate::mail::OpState::Pending`] and
    /// due -- `not_before <= now` -- oldest due first, capped at `limit`.
    /// What the account task's drain loop reads on every pass.
    fn due_ops(&self, account: AccountId, now: Timestamp, limit: u32) -> Result<Vec<Op>>;

    /// Replace an op whole -- a state transition, an attempt counted, a
    /// `last_error` recorded. Callers build the next value with
    /// [`crate::mail::Op::transition_to`] rather than this trait offering a
    /// narrower "just the state" update, on the same reasoning every other
    /// `save_*` in this crate takes a whole record: two writers narrowing
    /// different fields of the same op at once is not a thing the outbox
    /// ever does, since only the drain loop that holds it in flight ever
    /// writes to it.
    fn update_op(&self, op: &Op) -> Result<()>;

    /// One op by id -- what `undo_send` reads before it dares cancel it: it
    /// must see the *current* state and `not_before`, not the ones the
    /// caller happened to hold from when it was enqueued a second ago.
    fn get_op(&self, id: OpId) -> Result<Op>;

    /// Every op whose [`crate::mail::Origin::kind`] is `kind` -- `"assistant"`,
    /// `"mcp"`, `"routine"`, `"person"` -- most recently updated first,
    /// capped at `limit`. What answers "what did the assistant do": the
    /// caller reads `origin` off each returned op for the conversation or
    /// client it names, since only the kind is a clear column.
    fn ops_by_origin(&self, kind: &str, limit: u32) -> Result<Vec<Op>>;

    // ---- resolution and reset ------------------------------------------------

    /// Every `(mailbox, uid)` pair message `id` is currently filed under --
    /// the forward direction of [`MailStore::message_by_uid`], and what the
    /// outbox executor's `Lookups` implementation resolves a thread or
    /// message target against before it can `STORE`, `MOVE` or label
    /// anything on a live session. A Gmail message under two labels answers
    /// with two pairs; a plain IMAP message ordinarily answers with one.
    fn message_locations(&self, id: MailMessageId) -> Result<Vec<(MailboxId, u32)>>;

    /// The message filed as `uid` in `mailbox`, or `None`.
    fn message_by_uid(&self, mailbox: MailboxId, uid: u32) -> Result<Option<Message>>;

    /// The message in `account` whose `Message-ID` header is
    /// `message_id_header`, or `None`. What rematching after a
    /// `UIDVALIDITY` reset uses instead of trusting the uids the server
    /// handed out before: a message's `Message-ID` does not change when its
    /// uid does.
    fn message_by_message_id_header(
        &self,
        account: AccountId,
        message_id_header: &str,
    ) -> Result<Option<Message>>;

    /// Every uid this store believes is currently filed in `mailbox` --
    /// what a `UID SEARCH` diff (or QRESYNC's `VANISHED`, on a client that
    /// has it) is compared against to find what the server no longer has.
    fn uid_set(&self, mailbox: MailboxId) -> Result<Vec<u32>>;

    /// Every message in `mailbox` whose body has not been fetched yet, with
    /// its uid *in that mailbox*, newest first, capped at `limit` -- what
    /// the bodies pass reads instead of walking every uid `mailbox` has and
    /// decrypting each one to ask.
    ///
    /// "Not fetched yet" is `pack_len = 0` -- the sentinel a header ingested
    /// before its body arrives is stored with (a real sealed pack's length
    /// is never zero, even for an empty message, because the AEAD overhead
    /// alone is non-zero; `everyday-service`'s sync engine is the one place
    /// that writes and reads this sentinel by name, as
    /// `mailsync::ingest::pending_pack_ref`/`is_pending`) -- so this is a
    /// query over a clear column, the same terms [`MailStore::uid_set`] and
    /// [`MailStore::unread_counts`] already run on, not a decrypt of every
    /// row in the mailbox the way scanning [`MailStore::message_by_uid`]
    /// one uid at a time would be.
    fn pending_bodies(&self, mailbox: MailboxId, limit: u32) -> Result<Vec<(Message, u32)>>;

    /// Forget every `message_mailboxes` row naming `mailbox`, and reset its
    /// sync cursors to zero, without touching the messages themselves or any
    /// *other* mailbox's membership. What a `UIDVALIDITY` change asks for:
    /// the uids this store remembered for this mailbox may now point at
    /// different messages, so the safe answer is to forget the mapping and
    /// resync it, rematching by `Message-ID` where possible rather than
    /// trusting a uid that means something new. Threads and
    /// `thread_mailboxes` rows this mailbox touched are recomputed exactly
    /// as [`MailStore::remove_uids`] would recompute them for every uid it
    /// held.
    fn reset_mailbox(&self, mailbox: MailboxId) -> Result<()>;

    // ---- optimistic local writes -------------------------------------------
    //
    // What a person's own action writes locally, in the same vault write
    // that enqueues the `Op` telling the account task to make the server
    // agree -- see `docs/plans/mail.md`'s "What an action does" and
    // `everyday_service::domains::mail`, the one caller of every method
    // below. Each is keyed by the id the interface already holds *before*
    // any op has reached a server, which is why these are not simply
    // `update_flags`/`update_labels` again: those two are keyed by
    // `(mailbox, uid)`, the sync engine's own vocabulary for a change the
    // server has already confirmed, and a just-enqueued op has nothing of
    // the kind to offer yet.

    /// Set message `id`'s flags directly, ahead of the server confirming
    /// them -- the optimistic half of [`crate::mail::apply_optimistic`] for
    /// [`crate::mail::OpKind::MarkRead`], `MarkUnread`, `Star` and `Unstar`.
    /// Recomputes the thread it belongs to, on the same terms
    /// [`MailStore::update_flags`] does. A message id this store has never
    /// ingested is a no-op, not an error -- a permanently failed op's revert
    /// racing a message that was deleted by a concurrent sync must not
    /// itself fail.
    fn set_message_flags(&self, id: MailMessageId, flags: MessageFlags) -> Result<()>;

    /// As [`MailStore::set_message_flags`], for
    /// [`crate::mail::OpKind::Label`] and [`crate::mail::OpKind::Unlabel`].
    fn set_message_labels(&self, id: MailMessageId, labels: Vec<String>) -> Result<()>;

    /// Set message `id`'s [`crate::mail::Invite`] directly -- what
    /// `respond_to_invite` calls once it has queued the `REPLY`, so
    /// [`crate::mail::Invite::my_response`] reflects the answer immediately
    /// rather than waiting for the reply to round-trip back through sync. No
    /// thread recompute, unlike [`MailStore::set_message_flags`]: an
    /// invitation is not one of the aggregates a [`crate::mail::Thread`] row
    /// keeps. A message id this store has never ingested is a no-op, on the
    /// same terms `set_message_flags` already is.
    fn set_message_invite(&self, id: MailMessageId, invite: Option<Invite>) -> Result<()>;

    /// Hide `thread` from `mailbox`'s own list -- the optimistic half of
    /// [`crate::mail::OpKind::Archive`], [`crate::mail::OpKind::Trash`] and
    /// [`crate::mail::OpKind::Move`]: a `thread_mailboxes` row removed, with
    /// `message_mailboxes` -- the durable mapping the sync engine trusts --
    /// left exactly as it was, so this is reversible for free by
    /// [`MailStore::restore_thread_mailboxes`] rather than needing its own
    /// undo snapshot. A thread with no row for `mailbox` is a no-op.
    fn hide_thread_from_mailbox(&self, thread: ThreadId, mailbox: MailboxId) -> Result<()>;

    /// The exact inverse of [`MailStore::hide_thread_from_mailbox`]: recompute
    /// every `thread_mailboxes` row for `thread` from its current
    /// `message_mailboxes` rows, restoring whichever ones a permanently
    /// failed `Archive`, `Trash` or `Move` op hid.
    fn restore_thread_mailboxes(&self, thread: ThreadId) -> Result<()>;

    /// Set (or clear, with `None`) `thread`'s own `snoozed_until` -- the
    /// optimistic write behind [`crate::mail::OpKind::Snooze`], and also
    /// what the minute scheduler calls with `None` once a snooze's moment
    /// has passed (see [`MailStore::due_snoozed_threads`]), since bringing a
    /// thread back from snooze needs no server round trip at all -- snooze
    /// is local-only, per the plan's phase 7 section.
    fn set_thread_snoozed_until(&self, thread: ThreadId, until: Option<Timestamp>) -> Result<()>;

    /// Every thread, across every account, whose `snoozed_until` is set and
    /// has passed `now` -- what the minute scheduler reads to decide which
    /// threads return to the inbox this tick. Oldest-due first, capped at
    /// `limit` the same way [`MailStore::due_ops`] is, so one very large
    /// backlog of snoozes cannot make a single tick unbounded.
    fn due_snoozed_threads(&self, now: Timestamp, limit: u32) -> Result<Vec<ThreadId>>;

    // ---- unread counts --------------------------------------------------------

    /// Every mailbox of `account`'s, paired with how many of its threads
    /// are unread in it -- a clear-column aggregate over `thread_mailboxes`
    /// that decrypts nothing at all. What the app bar's badge and the
    /// mailbox list's counts both read.
    fn unread_counts(&self, account: AccountId) -> Result<Vec<(MailboxId, u64)>>;

    // ---- garbage collection --------------------------------------------------

    /// Every attachment or inline-image blob a synced message's [`Body`]
    /// still names, across every account -- what
    /// [`JournalStore::collect_garbage`](super::JournalStore::collect_garbage)
    /// treats as live. See `crate::mail::PartRef::blob`.
    fn attachment_blob_refs(&self) -> Result<Vec<BlobId>>;

    // ---- remote-image permissions --------------------------------------------

    /// The standing allow-list -- senders and domains someone has said yes
    /// to for good. `RemoteImageSettings::default()` (nobody allowed yet)
    /// when nothing has been saved, on the same reasoning
    /// `AgentStore::settings` returns a real, off default rather than an
    /// `Option` every caller would have to unwrap the same way.
    fn remote_image_settings(&self) -> Result<RemoteImageSettings>;

    fn put_remote_image_settings(&self, settings: &RemoteImageSettings) -> Result<()>;

    // ---- the contact index --------------------------------------------------

    /// The whole sealed contact book -- see [`crate::mail::ContactBook`]'s
    /// own docs for why this is one small row rather than a query over
    /// every message. `ContactBook::default()` (nobody written to or heard
    /// from yet) when nothing has been saved, on the same reasoning
    /// [`MailStore::remote_image_settings`] answers a real, empty default.
    fn contacts(&self) -> Result<ContactBook>;

    fn put_contacts(&self, book: &ContactBook) -> Result<()>;
}

// ---- associated data --------------------------------------------------

pub fn mailbox_aad(id: MailboxId) -> Vec<u8> {
    format!("everyday.mailbox.v1:{id}").into_bytes()
}

pub fn message_aad(id: MailMessageId) -> Vec<u8> {
    format!("everyday.mail_message.v1:{id}").into_bytes()
}

pub fn thread_aad(id: ThreadId) -> Vec<u8> {
    format!("everyday.thread.v1:{id}").into_bytes()
}

/// Bound to the message it belongs to, not to a `BodyId` of its own -- a
/// body has no id but the message's, the same one-to-one relationship
/// `entries.summary` has with `entries.data`.
pub fn body_aad(id: MailMessageId) -> Vec<u8> {
    format!("everyday.mail_body.v1:{id}").into_bytes()
}

pub fn draft_aad(id: DraftId) -> Vec<u8> {
    format!("everyday.draft.v1:{id}").into_bytes()
}

pub fn op_aad(id: OpId) -> Vec<u8> {
    format!("everyday.op.v1:{id}").into_bytes()
}

/// The one row of [`RemoteImageSettings`], sealed the way `AgentSettings`'s
/// singleton row is -- a fixed associated data rather than one keyed by an
/// id, because there is exactly one of these per vault.
pub fn remote_image_settings_aad() -> Vec<u8> {
    b"everyday.mail_remote_image_settings.v1".to_vec()
}

/// The one row of [`crate::mail::ContactBook`], sealed on the same terms
/// [`remote_image_settings_aad`] is.
pub fn contacts_aad() -> Vec<u8> {
    b"everyday.mail_contacts.v1".to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_aad_differs_by_id_and_by_kind() {
        let same = uuid::Uuid::now_v7();
        assert_ne!(mailbox_aad(MailboxId(same)), message_aad(MailMessageId(same)));
        assert_ne!(message_aad(MailMessageId(same)), thread_aad(ThreadId(same)));
        assert_ne!(thread_aad(ThreadId(same)), draft_aad(DraftId(same)));
        assert_ne!(draft_aad(DraftId(same)), op_aad(OpId(same)));
        assert_ne!(message_aad(MailMessageId(same)), body_aad(MailMessageId(same)));
        assert_ne!(mailbox_aad(MailboxId::new()), mailbox_aad(MailboxId::new()));
    }
}
