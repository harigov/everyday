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
use crate::mail::{Body, Category, Draft, Mailbox, Message, MessageFlags, Op, Thread};
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

    // ---- bodies ------------------------------------------------------------

    fn put_body(&self, body: &Body) -> Result<()>;

    fn get_body(&self, message_id: MailMessageId) -> Result<Body>;

    // ---- drafts ------------------------------------------------------------

    fn put_draft(&self, draft: &Draft) -> Result<()>;

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

    /// Every op whose [`crate::mail::Origin::kind`] is `kind` -- `"assistant"`,
    /// `"mcp"`, `"routine"`, `"person"` -- most recently updated first,
    /// capped at `limit`. What answers "what did the assistant do": the
    /// caller reads `origin` off each returned op for the conversation or
    /// client it names, since only the kind is a clear column.
    fn ops_by_origin(&self, kind: &str, limit: u32) -> Result<Vec<Op>>;

    // ---- resolution and reset ------------------------------------------------

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
