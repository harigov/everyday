//! The records a synced mailbox is made of.
//!
//! Seven kinds of row, matching `docs/plans/mail.md`'s data model exactly:
//! [`Mailbox`] (a folder or a Gmail label), [`Message`] (one fetched
//! header, plus a pointer to its raw bytes in the pack store), [`Body`] (the
//! sanitised, threaded-apart text the reader and the assistant both read),
//! [`Thread`] (what a list actually shows), [`Draft`] (a message being
//! written, whoever is writing it), [`Op`] (one entry in the outbox — see
//! [`crate::mail::outbox`]), and [`Origin`] (who asked for an `Op`, which is
//! what lets a thread say "archived by the assistant").

use crate::id::{AccountId, BlobId, DraftId, MailMessageId, MailboxId, OpId, ThreadId};
use crate::packstore::PackRef;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

// ---- addresses --------------------------------------------------------

/// One name-and-address pair, as it appears in a `From`, `To`, `Cc`, `Bcc`
/// or `Reply-To` header.
///
/// `name` is often empty — a bare `someone@example.com` with no display
/// name is a perfectly ordinary header — so it is not an `Option`: an empty
/// string already says "no name was given" without a second way to spell
/// the same fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Address {
    pub name: String,
    pub email: String,
}

impl Address {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self { name: name.into(), email: email.into() }
    }

    /// Bare address, no display name — the common case in a synthetic
    /// fixture and the fallback when a header carries no name at all.
    pub fn bare(email: impl Into<String>) -> Self {
        Self { name: String::new(), email: email.into() }
    }
}

// ---- mailboxes ----------------------------------------------------------

/// What a folder or Gmail label is *for*, so the interface can find "the"
/// inbox, sent folder or trash without matching on a server-supplied name
/// that might be `INBOX`, `Sent Items`, `[Gmail]/Sent Mail`, or anything
/// else a provider chooses to call it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MailboxRole {
    Inbox,
    Sent,
    Drafts,
    Archive,
    Trash,
    Spam,
    /// Gmail's "All Mail" — every message exactly once, labels aside.
    All,
    Other,
}

impl MailboxRole {
    pub const ALL: [MailboxRole; 8] = [
        MailboxRole::Inbox,
        MailboxRole::Sent,
        MailboxRole::Drafts,
        MailboxRole::Archive,
        MailboxRole::Trash,
        MailboxRole::Spam,
        MailboxRole::All,
        MailboxRole::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MailboxRole::Inbox => "inbox",
            MailboxRole::Sent => "sent",
            MailboxRole::Drafts => "drafts",
            MailboxRole::Archive => "archive",
            MailboxRole::Trash => "trash",
            MailboxRole::Spam => "spam",
            MailboxRole::All => "all",
            MailboxRole::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }
}

/// A folder, or a Gmail label, synced from one account.
///
/// The three sync-cursor fields — [`uidvalidity`](Mailbox::uidvalidity),
/// [`uidnext`](Mailbox::uidnext), [`highest_modseq`](Mailbox::highest_modseq)
/// — are exactly the three the plan's schema section names, and they live on
/// the record rather than in a table of their own because a mailbox is
/// already the one row per folder a sync task reads before it opens a
/// connection. `uidvalidity` changing between two syncs is what tells the
/// engine every `uid` it remembers for this mailbox may now point at a
/// different message — see [`crate::store::mail::MailStore::reset_mailbox`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mailbox {
    pub id: MailboxId,
    pub account_id: AccountId,
    /// The name the server gave it — `INBOX`, `[Gmail]/Sent Mail`, a
    /// personal label. Sealed: see the module docs.
    pub remote_name: String,
    pub role: MailboxRole,
    pub uidvalidity: u32,
    pub uidnext: u32,
    pub highest_modseq: u64,
}

impl Mailbox {
    pub fn new(account_id: AccountId, remote_name: impl Into<String>, role: MailboxRole) -> Self {
        Self {
            id: MailboxId::new(),
            account_id,
            remote_name: remote_name.into(),
            role,
            uidvalidity: 0,
            uidnext: 0,
            highest_modseq: 0,
        }
    }
}

// ---- messages -------------------------------------------------------------

/// The IMAP flags this application tracks, plus Gmail's own read/unread
/// notion (`\Seen`), packed as a small set of booleans rather than a
/// `Vec<String>` of keyword strings: these five are the only ones anything
/// here ever reads or writes, and a fixed shape is what lets
/// [`MessageFlags::bits`] pack them into the clear `messages.flags` column
/// the unread count is summed from without decrypting a single row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageFlags {
    pub seen: bool,
    pub answered: bool,
    pub flagged: bool,
    pub draft: bool,
    pub deleted: bool,
}

impl MessageFlags {
    const SEEN: i64 = 1 << 0;
    const ANSWERED: i64 = 1 << 1;
    const FLAGGED: i64 = 1 << 2;
    const DRAFT: i64 = 1 << 3;
    const DELETED: i64 = 1 << 4;

    /// Unread is the absence of `\Seen`, never a field of its own — a
    /// message cannot be both seen and unread, and a second boolean that
    /// could disagree with the first is a bug waiting to happen.
    pub fn unread(&self) -> bool {
        !self.seen
    }

    /// Pack into the integer a store's clear `flags` column holds, so an
    /// unread count is `WHERE flags & 1 = 0` rather than a decrypt of every
    /// row in the mailbox.
    pub fn bits(&self) -> i64 {
        let mut b = 0;
        if self.seen {
            b |= Self::SEEN;
        }
        if self.answered {
            b |= Self::ANSWERED;
        }
        if self.flagged {
            b |= Self::FLAGGED;
        }
        if self.draft {
            b |= Self::DRAFT;
        }
        if self.deleted {
            b |= Self::DELETED;
        }
        b
    }

    pub fn from_bits(b: i64) -> Self {
        Self {
            seen: b & Self::SEEN != 0,
            answered: b & Self::ANSWERED != 0,
            flagged: b & Self::FLAGGED != 0,
            draft: b & Self::DRAFT != 0,
            deleted: b & Self::DELETED != 0,
        }
    }
}

/// Gmail-specific identifiers, carried as plain optional data rather than as
/// a second, Gmail-only record shape. `thread_id` is `X-GM-THRID`: when
/// present it is what threading trusts over JWZ (see
/// `everyday-mail::threading`); `message_id` is `X-GM-MSGID`, Gmail's own
/// stable id for the message, independent of which mailbox or label it is
/// filed under.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GmailMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

/// A closed set of split-inbox categories, small and fixed rather than
/// free text, because [`Message::category`] and [`Thread::category`] are
/// both clear columns — see `crate::store::mail`. A user-named category
/// (phase 7 of the plan: "the ones the user names") cannot be one of these
/// without leaking its name into the clear the way a goal's name is kept out
/// of `purposes` by being a pointer rather than a string; that side table is
/// phase 7's to add, alongside the model rules that assign one. What exists
/// now is the fixed set phase 7's own preview names, so `Option<Category>`
/// has somewhere to live before the rules that fill it do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Category {
    Important,
    Other,
    Newsletter,
    Notification,
}

impl Category {
    pub const ALL: [Category; 4] =
        [Category::Important, Category::Other, Category::Newsletter, Category::Notification];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Important => "important",
            Category::Other => "other",
            Category::Newsletter => "newsletter",
            Category::Notification => "notification",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

/// One fetched message: its headers, its flags, and a pointer to where its
/// raw bytes live in the pack store. Never the body — see [`Body`], sealed
/// and stored apart, so opening a thread list never has to decrypt the text
/// of every message in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: MailMessageId,
    pub account_id: AccountId,
    pub thread_id: ThreadId,
    /// The `Message-ID` header, used to rematch a message against a fresh
    /// UID after a `UIDVALIDITY` reset — see
    /// [`crate::store::mail::MailStore::message_by_message_id_header`].
    pub message_id_header: String,
    pub date: Timestamp,
    pub from: Address,
    #[serde(default)]
    pub to: Vec<Address>,
    #[serde(default)]
    pub cc: Vec<Address>,
    #[serde(default)]
    pub bcc: Vec<Address>,
    #[serde(default)]
    pub reply_to: Vec<Address>,
    pub subject: String,
    /// A short, plain-text preview computed once at sync — what a list row
    /// shows without opening the body.
    #[serde(default)]
    pub snippet: String,
    pub flags: MessageFlags,
    /// Gmail labels, or IMAP keywords on a server that has them. Distinct
    /// from [`MailboxRole`]: a label is words a person chose, sealed the way
    /// every folder name is.
    #[serde(default)]
    pub labels: Vec<String>,
    pub has_attachments: bool,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
    pub pack: PackRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gmail: Option<GmailMeta>,
}

// ---- bodies -----------------------------------------------------------

/// One part of a message's raw bytes: an attachment, or an inline image
/// referenced by `cid:`.
///
/// `blob` starts `None` and is filled in once the attachment pass (the
/// plan's third sync pass) has extracted, deduplicated and stored the bytes
/// — which is also the moment [`crate::store::JournalStore::collect_garbage`]
/// starts to count it as a live reference. See `crate::store`'s blob-walk for
/// why this type, not [`Message`], is what that walk reaches into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartRef {
    /// The `Content-ID` a `cid:` URL in the sanitised HTML rewrites to, for
    /// an inline image. `None` for an ordinary attachment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    pub mime_type: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<BlobId>,
}

/// One remote image `everyday_mail::sanitize::sanitize` proxied rather than
/// fetched, recorded so the protocol handler can decide, later and per
/// request, whether to actually reach the sender's server.
///
/// This is `everyday-core`'s own copy of the shape
/// `everyday_mail::sanitize::RemoteImage` computes at sync — deliberately a
/// separate type rather than the sanitiser's own struct stored directly:
/// this crate has no dependency on `everyday-mail` (see this module's docs
/// on why the split exists at all), and a sealed record's shape belongs to
/// the crate that stores it, not the crate that happened to compute it
/// first. The sync engine converts one into the other on its way into
/// [`Body::remote_images`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteImage {
    /// The address the message actually pointed at. Never shown to the
    /// person without them asking, and never reached until
    /// `everyday-service::mailview` clears it against the per-sender
    /// allow-list and the SSRF check — see the plan's "Rendering a message".
    pub original_url: String,
    /// `blake3(original_url)`, hex-encoded — what the sanitised HTML's
    /// `everyday://mail/img/{token}` and this record both use, so an
    /// attacker who reads the sealed row learns nothing the rewritten
    /// markup did not already say.
    pub token: String,
    /// Set once the image has actually been fetched and cached: the blob
    /// holding its bytes, content-addressed like every other attachment.
    /// `None` until the first time someone allows it, which is what lets a
    /// reopened message skip the network entirely once it has been shown
    /// once — see [`crate::store::mail::MailStore::put_body`]'s caller in
    /// `mailview::remote_image`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_blob: Option<BlobId>,
}

/// The sanitised, threaded-apart text of one message.
///
/// A row of its own, keyed by [`MailMessageId`] rather than a field on
/// [`Message`], so that a thread list — which needs `Message` for its
/// sender, subject and flags — never has to decrypt a single body to draw
/// itself. `text`, once `quoted_ranges` and `signature_range` are cut out of
/// it, is exactly what a tool returns to the assistant: computed once here,
/// at sync, so that no tool ever parses HTML or has to guess where a quote
/// begins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Body {
    pub message_id: MailMessageId,
    pub html_sanitised: String,
    pub text: String,
    /// Byte ranges of `text` that are quoted from an earlier message in the
    /// thread, for the "show quoted text" toggle.
    #[serde(default)]
    pub quoted_ranges: Vec<(u32, u32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_range: Option<(u32, u32)>,
    #[serde(default)]
    pub parts: Vec<PartRef>,
    /// Every remote image `everyday_mail::sanitize::sanitize` found and
    /// proxied in `html_sanitised`, for `everyday-service::mailview` to
    /// answer an `everyday://mail/img/{token}` request against — see
    /// `docs/plans/mail.md`'s "Rendering a message".
    ///
    /// `#[serde(default)]` and nothing stronger: `Body` is sealed and shared
    /// between the sync engine that writes it and the service that reads it
    /// back, so a row written before this field existed must still decode —
    /// as an empty list, which is exactly what "this message was synced
    /// before remote images were tracked" should mean. Adding a field is
    /// safe here in a way that renaming or removing one would not be; see
    /// this module's own docs on why every record in it treats a sealed
    /// payload as a thing an older build might still hand back.
    #[serde(default)]
    pub remote_images: Vec<RemoteImage>,
}

impl Body {
    /// `text`, with everything in [`Body::quoted_ranges`] and
    /// [`Body::signature_range`] cut out — what a tool actually reads.
    /// Ranges are byte offsets into `text` and are clamped to its length, so
    /// a range computed against a slightly different encoding does not
    /// panic on a body it is handed against.
    pub fn model_text(&self) -> String {
        let len = self.text.len();
        let mut cuts: Vec<(usize, usize)> = self
            .quoted_ranges
            .iter()
            .map(|&(a, b)| (a as usize, b as usize))
            .chain(self.signature_range.map(|(a, b)| (a as usize, b as usize)))
            .map(|(a, b)| (a.min(len), b.min(len)))
            .filter(|(a, b)| a < b)
            .collect();
        cuts.sort_unstable();

        let mut out = String::with_capacity(len);
        let mut pos = 0usize;
        for (start, end) in cuts {
            let start = start.max(pos);
            if start > pos {
                out.push_str(safe_slice(&self.text, pos, start));
            }
            pos = pos.max(end);
        }
        if pos < len {
            out.push_str(safe_slice(&self.text, pos, len));
        }
        out
    }
}

/// `s[start..end]`, nudged inward to the nearest character boundary rather
/// than panicking — a cut range came from sync-time byte offsets, and a
/// multibyte character sitting on one must not crash a tool call reading it
/// back out.
fn safe_slice(s: &str, start: usize, end: usize) -> &str {
    let start = (start..=end).find(|&i| s.is_char_boundary(i)).unwrap_or(end);
    let end = (start..=end).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(start);
    &s[start..end]
}

// ---- threads ------------------------------------------------------------

/// What a thread list actually shows: never a message body.
///
/// `last_date`, `message_count` and `unread_count` are aggregates over every
/// message in the thread, in every mailbox it happens to be filed under —
/// see `crate::store::mail` for how `thread_mailboxes` keeps a *per-mailbox*
/// view of the same thread for the inbox's own ordering, which can
/// legitimately differ (a message that landed only in Archive should not
/// move a thread up the Inbox).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: ThreadId,
    pub account_id: AccountId,
    pub subject: String,
    #[serde(default)]
    pub participants: Vec<Address>,
    pub last_date: Timestamp,
    pub message_count: u32,
    pub unread_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<Timestamp>,
}

// ---- drafts ---------------------------------------------------------------

/// Where a [`Draft`] is in its life. `Queued` carries the [`OpId`] of the
/// outbox entry sending it, so the compose window can show "sending…" and
/// still be the same record if the send fails and reverts to `Editing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DraftState {
    Editing,
    Queued { op: OpId },
    Sent,
    Discarded,
}

impl DraftState {
    /// The discriminant a clear `drafts.state` column holds — never the
    /// `OpId` inside `Queued`, which stays inside the sealed payload.
    pub fn as_str(&self) -> &'static str {
        match self {
            DraftState::Editing => "editing",
            DraftState::Queued { .. } => "queued",
            DraftState::Sent => "sent",
            DraftState::Discarded => "discarded",
        }
    }
}

/// One file attached to a [`Draft`]: the blob holding its bytes, and the
/// name and MIME type [`everyday_mail::outbox::execute`]'s `Send` and
/// `AppendDraft` arms need to build a real MIME part from it, rather than
/// the placeholder `"attachment"` / `application/octet-stream` a bare
/// [`BlobId`] would leave them guessing at. Filled in wherever a draft's
/// attachment is first uploaded — the compose window's own attach command —
/// so the outbox executor never has to invent either.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftAttachment {
    pub blob: BlobId,
    pub filename: String,
    pub mime_type: String,
}

/// Where this crate's own last `APPEND` of a [`Draft`] to the account's
/// Drafts mailbox landed — `mailbox`, the server's own folder name, and
/// `uid`, `APPENDUID`'s answer for it. Persisted on the draft itself, rather
/// than kept in memory on the service that ran the append, so that
/// [`everyday_mail::outbox::execute`]'s `AppendDraft` arm still finds the
/// stale copy to delete after a restart — see that function's own module
/// docs for the trade a memory-only version used to make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftServerCopy {
    pub mailbox: String,
    pub uid: u32,
}

/// A message being written — by the person, by the assistant asked to, or
/// by the auto-draft pass phase 7 adds. See the plan's data model for why
/// this is a record with three possible writers rather than compose-box
/// state: whoever writes it, it is edited in the same editor and sent by
/// the same [`Op`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub id: DraftId,
    pub account_id: AccountId,
    /// Which of the account's identities this is sent as — an address from
    /// `Account::identities`, or the account's own.
    pub identity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<MailMessageId>,
    #[serde(default)]
    pub to: Vec<Address>,
    #[serde(default)]
    pub cc: Vec<Address>,
    #[serde(default)]
    pub bcc: Vec<Address>,
    pub subject: String,
    pub body_html: String,
    #[serde(default)]
    pub attachments: Vec<DraftAttachment>,
    /// Where this draft's last `AppendDraft` landed on the server, if it has
    /// ever had one. `#[serde(default)]` so a draft sealed before this field
    /// existed still decodes — as `None`, exactly what "never appended yet"
    /// already means — the same forward-compatibility every field added to
    /// a sealed record here keeps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_copy: Option<DraftServerCopy>,
    pub origin: Origin,
    pub state: DraftState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Draft {
    pub fn new(account_id: AccountId, identity: impl Into<String>, origin: Origin) -> Self {
        let now = Timestamp::now();
        Self {
            id: DraftId::new(),
            account_id,
            identity: identity.into(),
            in_reply_to: None,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: String::new(),
            body_html: String::new(),
            attachments: Vec::new(),
            server_copy: None,
            origin,
            state: DraftState::Editing,
            created_at: now,
            updated_at: now,
        }
    }
}

// ---- the outbox -----------------------------------------------------------

/// Who asked for an [`Op`]. The one place every path to a mail server meets
/// — see the plan's "What an action does" — which is what lets a thread say
/// "archived by the assistant" and lets an unattended run's report list
/// exactly what it touched.
///
/// Only the variant's own name is ever a clear column (`ops.origin`,
/// `drafts.origin`); `conversation`, `run` and `client` stay inside the
/// sealed payload, the same trade every other pointer in this crate makes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Origin {
    Person,
    Assistant { conversation: String },
    Routine { run: String },
    Mcp { client: String },
}

impl Origin {
    /// The clear-column discriminant. Stable, and never containing the
    /// conversation id, run id or client name that make the full variant
    /// identifying.
    pub fn kind(&self) -> &'static str {
        match self {
            Origin::Person => "person",
            Origin::Assistant { .. } => "assistant",
            Origin::Routine { .. } => "routine",
            Origin::Mcp { .. } => "mcp",
        }
    }

    /// Is this a caller [`crate::mail::rate_limit`] applies to?
    ///
    /// Exactly `Assistant` and `Mcp`, per the plan's own words ("ops from
    /// `Assistant` and `Mcp` are rate-limited per turn and per minute") —
    /// deliberately *not* `Routine`, which is unattended in a different
    /// sense (phase 5's "unattended runs never send") but is not the flood
    /// risk a model turn or a chatty external client is: a routine enqueues
    /// at most the handful of ops its own trigger fires, never in a tight
    /// loop a person is driving.
    pub fn is_rate_limited(&self) -> bool {
        matches!(self, Origin::Assistant { .. } | Origin::Mcp { .. })
    }
}

/// What an [`Op`] asks the account task to do. Named exactly after the
/// plan's list, with data only where the action needs it: a destination
/// mailbox to move to, a label's name, a time to wake a snoozed thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum OpKind {
    MarkRead,
    MarkUnread,
    Star,
    Unstar,
    Archive,
    Trash,
    Move { to: MailboxId },
    Label { label: String },
    Unlabel { label: String },
    Snooze { until: Timestamp },
    Send,
    AppendDraft,
}

impl OpKind {
    /// Does this op change [`Message::flags`] or [`Message::labels`] the
    /// moment it is enqueued, ahead of the server confirming it? Read by
    /// [`crate::mail::outbox::apply_optimistic`] — see its docs for why a
    /// `Move`, `Snooze`, `Send` or `AppendDraft` is deliberately not one of
    /// these: each changes something [`apply_optimistic`] does not reach
    /// (mailbox membership, a thread's own state, a draft's state), which is
    /// the sync engine's job once it exists, not this pure function's.
    ///
    /// [`apply_optimistic`]: crate::mail::outbox::apply_optimistic
    pub fn is_flag_change(&self) -> bool {
        matches!(
            self,
            OpKind::MarkRead
                | OpKind::MarkUnread
                | OpKind::Star
                | OpKind::Unstar
                | OpKind::Label { .. }
                | OpKind::Unlabel { .. }
        )
    }
}

/// What a target of an [`Op`] is — a whole thread, one message within it, or
/// a draft being sent or appended.
// Adjacently tagged (`tag`/`content`), not internally tagged like the
// other enums in this module: an internal tag needs to inject its `"type"`
// key into a JSON *object*, and a newtype variant holding an id -- these are
// all typed ids, which serialise as a bare string -- has no object to inject
// it into. `OpKind` and the rest are all struct-shaped variants, where an
// internal tag works fine; this is the one enum in the module whose
// variants are newtypes over a single scalar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "id", rename_all = "camelCase")]
pub enum OpTarget {
    Thread(ThreadId),
    Message(MailMessageId),
    Draft(DraftId),
}

/// Where one [`Op`] is in its life. See [`crate::mail::outbox`] for the
/// legal transitions between these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum OpState {
    Pending,
    InFlight,
    Done,
    /// `permanent` is what tells the drain loop to stop retrying and, per
    /// the plan, reverse the local change rather than leave it disagreeing
    /// with the server forever. A transient failure (the network is down)
    /// is `permanent: false` and goes back to `Pending` with a longer
    /// backoff instead of landing here.
    Failed {
        permanent: bool,
        message: String,
    },
    Cancelled,
}

impl OpState {
    pub fn as_str(&self) -> &'static str {
        match self {
            OpState::Pending => "pending",
            OpState::InFlight => "in_flight",
            OpState::Done => "done",
            OpState::Failed { .. } => "failed",
            OpState::Cancelled => "cancelled",
        }
    }
}

/// One entry in the outbox: the local write already happened; this is what
/// still has to reach the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Op {
    pub id: OpId,
    pub account_id: AccountId,
    pub kind: OpKind,
    pub target: OpTarget,
    /// Not before this instant. Undo-send and send-later are both this
    /// field, set further out — see [`crate::mail::outbox::undo_send_delay`].
    pub not_before: Timestamp,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub state: OpState,
    pub origin: Origin,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Op {
    pub fn new(account_id: AccountId, kind: OpKind, target: OpTarget, origin: Origin) -> Self {
        let now = Timestamp::now();
        Self {
            id: OpId::new(),
            account_id,
            kind,
            target,
            not_before: now,
            attempts: 0,
            last_error: None,
            state: OpState::Pending,
            origin,
            created_at: now,
            updated_at: now,
        }
    }

    /// The same op, waking no earlier than `not_before` — what queuing a
    /// send builds on top of [`Op::new`].
    pub fn not_before(mut self, not_before: Timestamp) -> Self {
        self.not_before = not_before;
        self
    }
}

// ---- remote-image permissions ----------------------------------------------

/// The standing allow-list a remote image is checked against: senders and
/// domains someone has said yes to for good, kept sealed in the vault
/// exactly the way `AgentSettings` is (`crate::store::agent`) — see
/// `everyday-service::mailview` for the per-message *one-off* allowance,
/// which is deliberately not part of this record at all, because it does
/// not outlive the session that granted it.
///
/// One vault-wide list rather than one per account: a person who trusts
/// `newsletter@example.com` on one mailbox trusts the same address on
/// another, and a second copy of the same list per account would only be
/// somewhere for the two to quietly disagree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteImageSettings {
    /// Exact sender addresses, lower-cased for comparison — see
    /// [`RemoteImageSettings::allows`].
    #[serde(default)]
    pub senders: Vec<String>,
    /// Domains a sender's address may end in — `example.com` allows
    /// `anyone@example.com` and `anyone@mail.example.com`.
    #[serde(default)]
    pub domains: Vec<String>,
}

impl RemoteImageSettings {
    /// Does this settled allow-list clear `sender` to load its images?
    ///
    /// Case-insensitive on both sides — mail headers are not consistent
    /// about casing, and a person typing an address into the settings pane
    /// should not have to match it exactly. A domain matches the sender's
    /// address by suffix on a label boundary (`example.com` matches
    /// `mail.example.com` but not `evilexample.com`), never by a bare
    /// substring.
    pub fn allows(&self, sender: &str) -> bool {
        let sender = sender.trim().to_ascii_lowercase();
        if self.senders.iter().any(|s| s.eq_ignore_ascii_case(&sender)) {
            return true;
        }
        let Some((_, host)) = sender.rsplit_once('@') else {
            return false;
        };
        self.domains.iter().any(|d| {
            let d = d.trim().to_ascii_lowercase();
            !d.is_empty() && (host == d || host.ends_with(&format!(".{d}")))
        })
    }

    /// `sender` added if it is not already there. Case preserved for
    /// display, matched case-insensitively by [`RemoteImageSettings::allows`].
    pub fn allow_sender(&mut self, sender: &str) {
        let sender = sender.trim();
        if !sender.is_empty() && !self.senders.iter().any(|s| s.eq_ignore_ascii_case(sender)) {
            self.senders.push(sender.to_string());
        }
    }

    pub fn allow_domain(&mut self, domain: &str) {
        let domain = domain.trim();
        if !domain.is_empty() && !self.domains.iter().any(|d| d.eq_ignore_ascii_case(domain)) {
            self.domains.push(domain.to_string());
        }
    }

    /// Remove a sender or a domain, whichever was passed. A no-op if neither
    /// was on the list, which `mailview::revoke_remote_image_allowance`
    /// relies on rather than treating as an error — revoking something
    /// already gone is not a mistake worth failing a command over.
    pub fn revoke(&mut self, sender: Option<&str>, domain: Option<&str>) {
        if let Some(sender) = sender {
            self.senders.retain(|s| !s.eq_ignore_ascii_case(sender));
        }
        if let Some(domain) = domain {
            self.domains.retain(|d| !d.eq_ignore_ascii_case(domain));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_flags_round_trip_through_their_bit_packing() {
        let f = MessageFlags {
            seen: true,
            answered: false,
            flagged: true,
            draft: false,
            deleted: true,
        };
        assert_eq!(MessageFlags::from_bits(f.bits()), f);
        assert!(!f.unread());
        assert!(MessageFlags::default().unread(), "a fresh message starts unread");
    }

    #[test]
    fn every_bit_is_independent() {
        for seen in [false, true] {
            for answered in [false, true] {
                for flagged in [false, true] {
                    for draft in [false, true] {
                        for deleted in [false, true] {
                            let f = MessageFlags { seen, answered, flagged, draft, deleted };
                            assert_eq!(MessageFlags::from_bits(f.bits()), f);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn category_round_trips_through_its_wire_name() {
        for c in Category::ALL {
            assert_eq!(Category::parse(c.as_str()), Some(c));
        }
        assert_eq!(Category::parse("nonsense"), None);
    }

    #[test]
    fn mailbox_role_round_trips_through_its_wire_name() {
        for r in MailboxRole::ALL {
            assert_eq!(MailboxRole::parse(r.as_str()), Some(r));
        }
    }

    #[test]
    fn origin_kind_never_carries_the_identifying_field() {
        assert_eq!(Origin::Person.kind(), "person");
        assert_eq!(Origin::Assistant { conversation: "secret-thread".into() }.kind(), "assistant");
        assert_eq!(Origin::Routine { run: "secret-run".into() }.kind(), "routine");
        assert_eq!(Origin::Mcp { client: "secret-client".into() }.kind(), "mcp");
    }

    #[test]
    fn only_the_assistant_and_mcp_are_rate_limited() {
        assert!(!Origin::Person.is_rate_limited());
        assert!(Origin::Assistant { conversation: "x".into() }.is_rate_limited());
        assert!(!Origin::Routine { run: "x".into() }.is_rate_limited(), "a routine fires rarely");
        assert!(Origin::Mcp { client: "x".into() }.is_rate_limited());
    }

    #[test]
    fn draft_state_as_str_never_carries_the_op_id() {
        let op = OpId::new();
        let s = DraftState::Queued { op }.as_str();
        assert_eq!(s, "queued");
        assert!(!s.contains(&op.to_string()));
    }

    #[test]
    fn model_text_removes_quotes_and_the_signature() {
        let body = Body {
            message_id: MailMessageId::new(),
            html_sanitised: String::new(),
            text: "Sure, sounds good.\n\nOn Tue, wrote:\n> the original\n\n--\nSent from my phone"
                .into(),
            quoted_ranges: vec![(21, 49)],
            signature_range: Some((51, 74)),
            parts: Vec::new(),
            remote_images: Vec::new(),
        };
        let text = body.model_text();
        assert!(text.contains("Sure, sounds good."));
        assert!(!text.contains("the original"), "{text}");
        assert!(!text.contains("Sent from my phone"), "{text}");
    }

    #[test]
    fn model_text_with_no_cuts_is_the_whole_body() {
        let body = Body {
            message_id: MailMessageId::new(),
            html_sanitised: String::new(),
            text: "just this".into(),
            quoted_ranges: Vec::new(),
            signature_range: None,
            parts: Vec::new(),
            remote_images: Vec::new(),
        };
        assert_eq!(body.model_text(), "just this");
    }

    #[test]
    fn model_text_clamps_ranges_past_the_end_rather_than_panicking() {
        let body = Body {
            message_id: MailMessageId::new(),
            html_sanitised: String::new(),
            text: "short".into(),
            quoted_ranges: vec![(2, 9_999)],
            signature_range: None,
            parts: Vec::new(),
            remote_images: Vec::new(),
        };
        assert_eq!(body.model_text(), "sh");
    }

    #[test]
    fn model_text_never_panics_on_a_cut_landing_inside_a_multibyte_character() {
        // "café" -- the 'é' is two bytes, so a range ending at byte 4 (its
        // first byte) must not split it.
        let body = Body {
            message_id: MailMessageId::new(),
            html_sanitised: String::new(),
            text: "café shop".into(),
            quoted_ranges: vec![(4, 6)],
            signature_range: None,
            parts: Vec::new(),
            remote_images: Vec::new(),
        };
        // Nudged to a boundary rather than panicking; the exact split is
        // less important than surviving it.
        let _ = body.model_text();
    }

    #[test]
    fn is_flag_change_covers_exactly_the_ops_that_touch_a_message_in_place() {
        let flag_changes = [
            OpKind::MarkRead,
            OpKind::MarkUnread,
            OpKind::Star,
            OpKind::Unstar,
            OpKind::Label { label: "x".into() },
            OpKind::Unlabel { label: "x".into() },
        ];
        for kind in &flag_changes {
            assert!(kind.is_flag_change(), "{kind:?}");
        }
        let not_flag_changes = [
            OpKind::Archive,
            OpKind::Trash,
            OpKind::Move { to: MailboxId::new() },
            OpKind::Snooze { until: Timestamp::now() },
            OpKind::Send,
            OpKind::AppendDraft,
        ];
        for kind in &not_flag_changes {
            assert!(!kind.is_flag_change(), "{kind:?}");
        }
    }
}
