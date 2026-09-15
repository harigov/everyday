//! The trait the sync engine talks to, and the small vocabulary it is
//! written in — a UID, a set of them, a mailbox's state, what changed since
//! last time.
//!
//! # Why this is our own trait, not a library's
//!
//! `async-imap` (see [`crate::imap`]) is what actually opens a socket, but
//! nothing above this module is allowed to know that. The sync engine that
//! will be built on top of [`MailSession`] is written against the shape
//! below, and only against it: the three-pass first sync, the cursor a
//! mailbox remembers between runs, the outbox that drains actions — none of
//! it mentions `async-imap`, `imap-proto` or a socket.
//!
//! That inversion is not architectural tidiness for its own sake; it is
//! bought for two specific reasons.
//!
//! * **The library underneath is a decision that can change without the
//!   engine noticing.** `async-imap` is pre-1.0 and does not yet speak
//!   QRESYNC, so [`MailSession::changes_since`] on [`crate::imap::ImapSession`]
//!   builds `Changes` out of `CONDSTORE` and a `UID SEARCH` diff — see that
//!   method's docs for exactly how. An adapter over `io-imap` — which does
//!   speak QRESYNC and hands back `VANISHED` ranges directly — would fill in
//!   the same [`Changes`] from less work, and the engine would not need a
//!   single line changed to benefit from it.
//! * **A provider does not have to speak IMAP at all.** A Gmail REST
//!   adapter, when one is written, maps `history.list` onto the same
//!   [`Changes`] shape; a CalDAV-flavoured mail host, if such a thing ever
//!   mattered, would do the same. The trait is the seam where "provider" and
//!   "engine" separate, so every provider after the first is an adapter
//!   rather than a fork.
//!
//! None of the fields below are secret in the vault's sense — a UID, a flag,
//! a mod-sequence are meaningless outside a live connection — so nothing
//! here needs sealing. What *is* sensitive is [`Credential`], which is why
//! it does not implement `Debug` in the usual way and why its secrets are
//! wiped from memory when dropped.

use std::collections::BTreeSet;
use std::fmt;

use futures::stream::BoxStream;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// An IMAP UID: unique within one mailbox for as long as that mailbox's
/// `UIDVALIDITY` holds, and meaningless once it changes. See [`SyncCursor`].
pub type Uid = u32;

/// A `MODSEQ`, RFC 7162's mod-sequence: it only ever increases for a
/// mailbox, and a message's own value tells a `CONDSTORE`-aware client
/// whether it has changed since the last cursor.
pub type ModSeq = u64;

/// A compact, ordered set of [`Uid`]s: exactly what an IMAP sequence-set
/// literal (`3,5:9,42`) already is, kept as merged inclusive ranges rather
/// than one entry per message.
///
/// A hundred thousand messages named individually would be a hundred
/// thousand entries in every `UID FETCH`, `UID STORE` and `UID SEARCH` this
/// crate sends; almost every real set is mostly contiguous (a mailbox synced
/// in UID order, a `SEARCH` result over a live folder), so a handful of
/// ranges is the common case and the sparse case still round-trips exactly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UidSet {
    /// Sorted, non-overlapping, non-adjacent inclusive ranges.
    ranges: Vec<(Uid, Uid)>,
}

impl UidSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn single(uid: Uid) -> Self {
        Self { ranges: vec![(uid, uid)] }
    }

    /// One range covering every uid from `first` to `last`, inclusive.
    pub fn range(first: Uid, last: Uid) -> Self {
        if first > last {
            return Self::new();
        }
        Self { ranges: vec![(first, last)] }
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// How many UIDs this set names, not how many ranges it is stored as.
    pub fn len(&self) -> u64 {
        self.ranges.iter().map(|&(lo, hi)| u64::from(hi - lo) + 1).sum()
    }

    pub fn contains(&self, uid: Uid) -> bool {
        self.ranges
            .binary_search_by(|&(lo, hi)| {
                if uid < lo {
                    std::cmp::Ordering::Greater
                } else if uid > hi {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok()
    }

    /// Add one uid, merging it into a neighbouring range where it touches
    /// one. Kept simple (a linear scan, not a tree) because a set this crate
    /// builds is assembled once from a sorted or near-sorted source — a
    /// `SEARCH` result, a `FETCH` stream — and never mutated one uid at a
    /// time at scale.
    pub fn insert(&mut self, uid: Uid) {
        match self.ranges.binary_search_by_key(&uid, |&(lo, _)| lo) {
            Ok(_) => {}
            Err(i) => {
                let touches_prev = i > 0
                    && self.ranges[i - 1].1.saturating_add(1) >= uid
                    && self.ranges[i - 1].1 < uid;
                let already_in_prev = i > 0 && self.ranges[i - 1].1 >= uid;
                if already_in_prev {
                    // already covered
                } else if touches_prev && i < self.ranges.len() && self.ranges[i].0 == uid + 1 {
                    // merges the gap between the previous and the next range
                    self.ranges[i - 1].1 = self.ranges[i].1;
                    self.ranges.remove(i);
                } else if touches_prev {
                    self.ranges[i - 1].1 = uid;
                } else if i < self.ranges.len() && self.ranges[i].0 == uid + 1 {
                    self.ranges[i].0 = uid;
                } else {
                    self.ranges.insert(i, (uid, uid));
                }
            }
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = Uid> + '_ {
        self.ranges.iter().flat_map(|&(lo, hi)| lo..=hi)
    }

    /// Split into chunks of at most `size` UIDs, each chunk itself a
    /// [`UidSet`] so it still renders as ranges. Used to bound how much a
    /// single `FETCH` asks for at once — see [`crate::imap::ImapSession::raw`].
    pub fn chunks(&self, size: usize) -> Vec<UidSet> {
        assert!(size > 0);
        let mut out = Vec::new();
        let mut current = UidSet::new();
        let mut count = 0usize;
        for uid in self.iter() {
            current.insert(uid);
            count += 1;
            if count == size {
                out.push(std::mem::take(&mut current));
                count = 0;
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
        out
    }

    /// The IMAP sequence-set syntax for this set, e.g. `"3,5:9,42"`. Empty
    /// only when the set is empty, which no command should be sent for.
    pub fn to_imap(&self) -> String {
        self.ranges
            .iter()
            .map(|&(lo, hi)| if lo == hi { lo.to_string() } else { format!("{lo}:{hi}") })
            .collect::<Vec<_>>()
            .join(",")
    }
}

impl FromIterator<Uid> for UidSet {
    fn from_iter<I: IntoIterator<Item = Uid>>(iter: I) -> Self {
        let mut sorted: Vec<Uid> = iter.into_iter().collect();
        sorted.sort_unstable();
        sorted.dedup();
        let mut ranges: Vec<(Uid, Uid)> = Vec::new();
        for uid in sorted {
            match ranges.last_mut() {
                Some((_, hi)) if *hi + 1 == uid => *hi = uid,
                _ => ranges.push((uid, uid)),
            }
        }
        Self { ranges }
    }
}

impl From<BTreeSet<Uid>> for UidSet {
    fn from(set: BTreeSet<Uid>) -> Self {
        set.into_iter().collect()
    }
}

/// The five flags this crate tracks, as a bitset rather than a `Vec<Flag>`:
/// they are compared, unioned and stored per message often enough (every
/// `changes_since`, every `store_flags`) that a copyable `u8` earns its
/// keep over an allocation.
///
/// `\Recent` is deliberately absent — it is session-scoped by the protocol
/// itself and cannot be set or cleared by a client, so keeping it here would
/// invite someone to try. Custom keywords (a server-defined label stored as
/// an IMAP flag rather than a Gmail label) are out of scope for the same
/// reason `\Recent` is: nothing in this plan's data model reads one. Gmail's
/// labels arrive separately, as [`GmailMeta::labels`].
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags(u8);

impl Flags {
    pub const NONE: Flags = Flags(0);
    pub const SEEN: Flags = Flags(1 << 0);
    pub const ANSWERED: Flags = Flags(1 << 1);
    pub const FLAGGED: Flags = Flags(1 << 2);
    pub const DELETED: Flags = Flags(1 << 3);
    pub const DRAFT: Flags = Flags(1 << 4);

    /// Every flag named above, in the order the IMAP wire form lists them,
    /// paired with the bit and the atom `STORE` and `FETCH` use for it.
    const ALL: [(Flags, &'static str); 5] = [
        (Flags::SEEN, "\\Seen"),
        (Flags::ANSWERED, "\\Answered"),
        (Flags::FLAGGED, "\\Flagged"),
        (Flags::DELETED, "\\Deleted"),
        (Flags::DRAFT, "\\Draft"),
    ];

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: Flags) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Flags) {
        self.0 &= !other.0;
    }

    /// Parse the `Flags` attribute of a `FETCH` response: one string per
    /// flag, case-insensitively, ignoring anything not in [`Self::ALL`]
    /// (custom keywords and `\Recent`, see the type's docs).
    pub fn from_imap<'a>(atoms: impl IntoIterator<Item = &'a str>) -> Flags {
        let mut flags = Flags::NONE;
        for atom in atoms {
            if let Some((bit, _)) =
                Self::ALL.iter().find(|(_, name)| name.eq_ignore_ascii_case(atom))
            {
                flags.insert(*bit);
            }
        }
        flags
    }

    /// The parenthesised flag list a `STORE` or `APPEND` command sends, e.g.
    /// `"(\\Seen \\Flagged)"`, or `None` for an empty set — callers should
    /// skip the command entirely rather than send empty parentheses.
    pub fn to_imap_list(self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let atoms: Vec<&str> = Self::ALL
            .iter()
            .filter(|(bit, _)| self.contains(*bit))
            .map(|(_, name)| *name)
            .collect();
        Some(format!("({})", atoms.join(" ")))
    }
}

impl fmt::Debug for Flags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = Self::ALL
            .iter()
            .filter(|(bit, _)| self.contains(*bit))
            .map(|(_, name)| *name)
            .collect();
        write!(f, "Flags({})", names.join(","))
    }
}

impl std::ops::BitOr for Flags {
    type Output = Flags;
    fn bitor(self, rhs: Flags) -> Flags {
        Flags(self.0 | rhs.0)
    }
}

/// What a mailbox is *for*, decided from `SPECIAL-USE` (RFC 6154) where the
/// server advertises it and, for Gmail, a name fallback — see
/// [`crate::imap::ImapSession::mailboxes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    Inbox,
    Sent,
    Drafts,
    Archive,
    Trash,
    Spam,
    /// Gmail's "All Mail": every message the account has, independent of
    /// label. Phase 2's Gmail folder rule fetches this one instead of every
    /// labelled mailbox — see [`crate::imap::ImapSession::mailboxes`].
    All,
    Other,
}

/// One mailbox as `LIST` describes it, before anything has been `SELECT`ed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMailbox {
    /// The name as the server spells it — `INBOX`, `[Gmail]/Sent Mail`,
    /// `Archives/2024` — used verbatim in every later command against it.
    pub name: String,
    /// The character this server uses to separate levels of a mailbox's
    /// hierarchy, e.g. `Some('/')`. `None` means the name has no hierarchy.
    pub delimiter: Option<char>,
    /// The raw `LIST` attributes (`\HasChildren`, `\Noselect`, `\Marked`,
    /// the `SPECIAL-USE` ones), kept as the server's own atoms rather than
    /// parsed into a closed enum: the mailbox tree in the interface wants to
    /// show `\Noselect` in its own way, and a new attribute IANA registers
    /// tomorrow should not need a new variant here to be visible.
    pub attributes: Vec<String>,
    pub special_use: Option<Role>,
}

/// What `SELECT` (or `SELECT (CONDSTORE)`) says about a mailbox the moment
/// it is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MailboxState {
    /// Changes whenever the server has renumbered UIDs from under a client
    /// — a migration, a corrupted index rebuilt — at which point every UID
    /// this crate remembers for the mailbox is meaningless. See
    /// [`Changes::uidvalidity_reset`].
    pub uidvalidity: u32,
    /// The UID the *next* appended message will get. Not itself a UID that
    /// exists yet.
    pub uidnext: Uid,
    /// Present only when the server supports `CONDSTORE` and the mailbox
    /// has ever had a flag change; `None` means "ask with a full `FETCH`
    /// instead of `CHANGEDSINCE`".
    pub highestmodseq: Option<ModSeq>,
    /// How many messages `EXISTS` reported at `SELECT` time.
    pub exists: u32,
}

/// What the engine remembers between two syncs of one mailbox, and hands
/// back to [`MailSession::changes_since`] to ask "what changed".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SyncCursor {
    pub uidvalidity: u32,
    /// The highest UID the engine has ever stored a message for. Everything
    /// above it, up to the mailbox's current `UIDNEXT`, is new.
    pub highest_uid_seen: Uid,
    pub highestmodseq: Option<ModSeq>,
}

/// What changed in the selected mailbox since a [`SyncCursor`].
#[derive(Debug, Clone, Default)]
pub struct Changes {
    /// UIDs the engine has not stored a message for yet.
    pub new_uids: UidSet,
    /// `(uid, flags as they are now, the modseq that produced this line)`
    /// for every UID `CHANGEDSINCE` reported. The modseq is `None` on a
    /// server without `CONDSTORE`, where this list is instead the full
    /// current flag state of every known UID — see
    /// [`crate::imap::ImapSession::changes_since`] for why.
    pub flag_changes: Vec<(Uid, Flags, Option<ModSeq>)>,
    /// UIDs the engine knew about that are no longer on the server.
    pub vanished: UidSet,
    /// `true` when `uidvalidity` no longer matches the cursor's: every field
    /// above is empty and meaningless, and the engine's only correct move is
    /// to treat this as a first sync, rematching what it already stored by
    /// `Message-ID` rather than by UID.
    pub uidvalidity_reset: bool,
}

/// Gmail's extra identifiers and labels for one message, present only when
/// the server advertised `X-GM-EXT-1` and the adapter asked for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmailMeta {
    /// `X-GM-THRID`: the same value on every message Gmail considers part of
    /// one conversation, which is a better signal than JWZ threading for an
    /// account that has one, and the reason [`crate::imap::ImapSession::headers`]
    /// goes to the trouble of fetching it at all — see that method's docs.
    pub thrid: u64,
    /// `X-GM-MSGID`: Gmail's own message id, stable across a `Message-ID`
    /// header that some senders omit or duplicate.
    pub msgid: u64,
    /// `X-GM-LABELS`, verbatim: `\Inbox`, `\Important`, `\Starred` and every
    /// label the person created, all in one place because Gmail mailboxes
    /// are really one label each — see the module docs on the Gmail folder
    /// rule in `docs/plans/mail.md`.
    pub labels: Vec<String>,
}

/// One message as `FETCH` describes it: enough to store a row and index it,
/// not yet the parsed MIME tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteHeader {
    pub uid: Uid,
    pub flags: Flags,
    pub internal_date: Timestamp,
    /// `RFC822.SIZE`, the size of the whole message as the server counts it.
    pub size: u32,
    pub gmail: Option<GmailMeta>,
    /// The raw bytes of `BODY.PEEK[HEADER]` — every header line, nothing
    /// parsed out of it yet.
    ///
    /// `ENVELOPE` was the other option RFC 3501 offers and is not what this
    /// fetches: it is the server's own parse of the header into named
    /// fields, which means a second, different parser (the server's) sits
    /// between the bytes the sender wrote and [`crate::mime`], and a header
    /// this crate wants but `ENVELOPE` does not carry (`List-Unsubscribe`,
    /// `References`, a nonstandard `X-` header a threading rule keys off)
    /// would need a second round trip to fetch anyway. Raw bytes, parsed
    /// once in Rust by [`crate::mime`], is one parser and one fetch.
    pub header: Vec<u8>,
}

/// What woke an [`MailSession::idle`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleEvent {
    /// The server said something changed — new mail, an expunge, a flag —
    /// without saying what. The caller re-syncs with [`MailSession::changes_since`]
    /// rather than trying to read meaning into which untagged response
    /// arrived, which is the advice RFC 2177 itself gives: `IDLE` promises a
    /// wake-up, not a diff.
    Activity,
    /// The caller's `stop` channel fired; the `IDLE` was cleanly ended with
    /// `DONE` and the session is ready for another command.
    Stopped,
}

/// What this connection's server supports, read from `CAPABILITY` once at
/// connect time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    /// RFC 7162: `CHANGEDSINCE`, `HIGHESTMODSEQ`, the machinery
    /// [`MailSession::changes_since`] is built from.
    pub condstore: bool,
    /// RFC 7162's other half: `VANISHED`, not used by [`crate::imap`] today
    /// — see [`crate::imap::ImapSession::changes_since`] — but named here so
    /// a future adapter can report it honestly.
    pub qresync: bool,
    pub idle: bool,
    /// RFC 6851's `MOVE`, checked by [`crate::imap::ImapSession::move_to`]
    /// to choose between one command and the `COPY` / `STORE` / `EXPUNGE`
    /// fallback.
    pub move_: bool,
    /// RFC 4315: `APPENDUID` on a successful `APPEND`, which is the only
    /// way [`crate::imap::ImapSession::append`] can hand back a `Uid`
    /// without a second round trip.
    pub uidplus: bool,
    /// `X-GM-EXT-1`: Gmail's own extension, gating `X-GM-THRID`,
    /// `X-GM-MSGID`, `X-GM-LABELS` and the Gmail folder rule.
    pub gmail: bool,
    /// RFC 4978's `COMPRESS=DEFLATE`.
    pub compress: bool,
}

/// How to authenticate, once a transport is open.
///
/// Deliberately not `Debug` in the derive sense — see the manual impl below
/// — and its secrets are [`Zeroizing`] so they are overwritten, not just
/// dropped, when this value goes out of scope. A `Credential` lives only as
/// long as one connection attempt; nothing in this crate keeps one around.
#[derive(Clone)]
pub enum Credential {
    Password { user: String, pass: Zeroizing<String> },
    XOAuth2 { user: String, access_token: Zeroizing<String> },
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Credential::Password { user, .. } => {
                f.debug_struct("Password").field("user", user).field("pass", &"<redacted>").finish()
            }
            Credential::XOAuth2 { user, .. } => f
                .debug_struct("XOAuth2")
                .field("user", user)
                .field("access_token", &"<redacted>")
                .finish(),
        }
    }
}

/// Every fallible operation in this crate returns this error. Four shapes,
/// named after what the sync engine must do about each — the same reasoning
/// `everyday_core::error::Error` uses for its own variants — because the
/// engine that calls this trait needs to know *that*, not which protocol
/// line produced it:
///
/// * [`MailError::Auth`] means the credential is no longer good; the
///   account should show "sign in again" rather than retry, because
///   retrying a wrong password only trains a server to rate-limit the
///   account.
/// * [`MailError::Network`] means nothing about the credential or the
///   request was wrong — a socket closed, a connect timed out — and a retry
///   with backoff is the right answer.
/// * [`MailError::Protocol`] means this crate and the server disagree about
///   IMAP itself: a response this crate could not parse, a command the
///   server rejected in a way that is this crate's bug to fix, not the
///   user's problem to solve.
/// * [`MailError::Server`] carries a `NO`/`BAD` response's own text,
///   verbatim, because some of those messages are the entire point — a
///   Microsoft 365 tenant that disabled IMAP says so in the `LOGIN`
///   response, and paraphrasing it would throw away the one sentence that
///   tells the person which admin setting to ask about.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MailError {
    #[error("sign-in failed: {0}")]
    Auth(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("{0}")]
    Server(String),
    #[error("the server does not support {0}")]
    Unsupported(&'static str),
}

pub type Result<T> = std::result::Result<T, MailError>;

/// A stream of raw message bytes, one item per requested UID, in the order
/// they were fetched rather than the order they were asked for — see
/// [`MailSession::raw`].
pub type RawStream<'a> = BoxStream<'a, Result<(Uid, Vec<u8>)>>;

/// The seam between the sync engine and a mail provider. See the module
/// docs for why this is our own trait.
///
/// Every method takes `&mut self` because a `MailSession` is one live
/// connection with server-side state (`SELECT` chooses a mailbox for every
/// later command until the next `SELECT`) — there is no useful reading to
/// do concurrently through the same session, and the engine that will drive
/// this trait holds one connection per account plus a second for `idle`.
///
/// Uses `async fn` in traits directly rather than `#[async_trait]`: it has
/// been stable since Rust 1.75, this trait is used generically (one
/// connection type at a time, never behind `dyn`), and the sync engine that
/// calls it is written against a concrete adapter chosen once at startup —
/// so none of `async_trait`'s reason to exist (boxing the future so the
/// trait stays object-safe) applies here.
///
/// The lint this triggers exists because a public trait's `async fn`
/// cannot promise its future is `Send` without a caller writing it out by
/// hand as `-> impl Future<Output = …> + Send`; every future this trait
/// produces is `Send` in practice, since `Self: Send` and every argument
/// and return type is too, but the engine that will hold these futures
/// across an `.await` (needed for the per-account task the plan describes)
/// is written in this crate, against this trait, so that promise never
/// needs to survive being handed to a stranger's generic code.
#[allow(async_fn_in_trait)]
pub trait MailSession: Send {
    /// Every mailbox the account has, from `LIST`.
    async fn mailboxes(&mut self) -> Result<Vec<RemoteMailbox>>;

    /// `SELECT` (or `SELECT (CONDSTORE)` when the server offers it), making
    /// `mailbox` the one every later call on this session acts on.
    async fn select(&mut self, mailbox: &str) -> Result<MailboxState>;

    /// What changed in the selected mailbox since `cursor`, diffed against
    /// `known_uids` — the engine's own record of which UIDs it has already
    /// stored a message for.
    ///
    /// The plan's sketch of this trait writes the signature as taking only
    /// a cursor; `known_uids` is added here because the diff the IMAP
    /// adapter builds it from — see
    /// [`crate::imap::ImapSession::changes_since`] — cannot know which UIDs
    /// vanished without being told which ones existed. An adapter with
    /// `VANISHED` (QRESYNC) would not need `known_uids` to compute
    /// `vanished`, but every adapter is given it for one signature both can
    /// implement.
    async fn changes_since(&mut self, cursor: &SyncCursor, known_uids: &UidSet) -> Result<Changes>;

    /// Header-sized metadata for a set of UIDs in the selected mailbox — see
    /// [`RemoteHeader`] for exactly what "header-sized" means and why.
    async fn headers(&mut self, uids: &UidSet) -> Result<Vec<RemoteHeader>>;

    /// The full raw bytes of each message in `uids`, streamed rather than
    /// collected, so the engine can start writing the pack store's first
    /// message before the last one has arrived. Batched internally — see
    /// the adapter's own docs for the batch size and why.
    async fn raw(&mut self, uids: &UidSet) -> Result<RawStream<'_>>;

    /// Add and remove flags on a set of UIDs in one round trip.
    async fn store_flags(&mut self, uids: &UidSet, add: Flags, remove: Flags) -> Result<()>;

    /// Add and remove Gmail labels (`X-GM-LABELS`) on a set of UIDs, in one
    /// round trip each way — the label equivalent of [`Self::store_flags`],
    /// and the one [`crate::outbox`] reaches for instead of it when
    /// [`Capabilities::gmail`] is set: Gmail's Inbox, and every other
    /// mailbox this crate would otherwise `SELECT` by name, is really a
    /// label, so archiving a Gmail message is `-X-GM-LABELS (\Inbox)`, never
    /// a `MOVE`. See `docs/plans/mail.md`'s "the Gmail folder rule".
    ///
    /// Added here rather than folded into [`Self::store_flags`] because a
    /// label is an arbitrary string a person or Gmail itself chose, not one
    /// of the five fixed [`Flags`] this trait already has a closed
    /// vocabulary for — the same reason [`GmailMeta::labels`] is its own
    /// `Vec<String>` rather than more bits in [`Flags`].
    ///
    /// The default implementation refuses with
    /// [`MailError::Unsupported`]`("Gmail labels")`, which is correct for
    /// every adapter that is not talking to Gmail and saves each of them
    /// from having to say so by hand; [`crate::imap::ImapSession`] is the
    /// one adapter that overrides it.
    async fn store_gmail_labels(
        &mut self,
        _uids: &UidSet,
        _add: &[String],
        _remove: &[String],
    ) -> Result<()> {
        Err(MailError::Unsupported("Gmail labels"))
    }

    /// Move a set of UIDs to another mailbox in the same account.
    async fn move_to(&mut self, uids: &UidSet, mailbox: &str) -> Result<()>;

    /// Append a message to `mailbox`, returning its new `Uid` when the
    /// server's response says so.
    ///
    /// `None` rather than a required `Uid`: without `UIDPLUS`
    /// (`Capabilities::uidplus`) the server's `APPEND` response has no way
    /// to say what UID the message landed at, and asking for one with a
    /// second command (a `SEARCH` for a `Message-ID` this crate just wrote)
    /// is a race against anything else appending to the same mailbox. The
    /// engine already re-syncs the mailbox after every outbox op completes,
    /// so a `None` here is filled in by the next ordinary sync rather than
    /// guessed at here.
    async fn append(&mut self, mailbox: &str, raw: &[u8], flags: Flags) -> Result<Option<Uid>>;

    /// Find a message by its `Message-ID` header in `mailbox`, `SELECT`ing
    /// it first -- `UID SEARCH HEADER Message-ID`, or its adapter's
    /// equivalent. `None` when nothing matches.
    ///
    /// The one caller is the outbox's recovery from a `Send` op left
    /// `InFlight` by a crash: the message may already have reached SMTP
    /// before the crash, in which case re-sending it would duplicate it, so
    /// recovery asks the server directly whether the draft's (stable, see
    /// [`crate::compose::Outgoing::message_id`]) id is already there rather
    /// than trusting local state alone, which is exactly what a crash mid
    /// drain means it cannot.
    async fn search_message_id(&mut self, mailbox: &str, message_id: &str) -> Result<Option<Uid>>;

    /// Block until something changes in the selected mailbox, or `stop`
    /// fires. Returns [`Err`] only for a connection problem; a clean stop is
    /// [`IdleEvent::Stopped`], not an error.
    async fn idle(&mut self, stop: tokio::sync::watch::Receiver<()>) -> Result<IdleEvent>;

    fn capabilities(&self) -> &Capabilities;
}
