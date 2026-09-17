//! Storage for raw messages, sealed one at a time, without one file per
//! message.
//!
//! A hundred thousand messages is a hundred thousand `fsync`s and a hundred
//! thousand inodes if each becomes its own entry in
//! [`FileBlobStore`](crate::blobstore::FileBlobStore) -- fine for a photo
//! pasted into an entry once in a while, ruinous for what mail sync does on
//! its first pass through an inbox. [`PackStore`] is the other shape:
//! messages sealed individually, so reading one never decrypts its
//! neighbours, but *flushed* together, so writing a thousand of them costs
//! one `fsync` rather than a thousand.
//!
//! # Why this exists ahead of the mail domain
//!
//! `docs/plans/mail.md` calls this "phase 0 ... the part of this plan that
//! is not about mail at all", and lays out the design this module follows:
//! append-only pack files, each message sealed on its own, flushed once per
//! batch, with a compaction pass that rewrites a pack once a third of it is
//! dead. Nothing here names a message, an account record or a mailbox --
//! those arrive with the domain in a later phase -- but the shape a hundred
//! thousand raw messages need does not depend on any of that, so it is built
//! and proven first.
//!
//! # One trait, two backings
//!
//! The same split [`blobstore`](crate::blobstore) makes for attachments,
//! for the same reason. [`FilePackStore`] below is what a vault whose
//! database is on this machine wants: append-only files beside wherever the
//! blob store keeps its media, each one a sealed log the filesystem already
//! buffers and syncs. A vault whose database is a server has no local disk
//! to put a file on, so `everyday-store-sql` gives the same trait a
//! `mail_packs`-table answer instead -- see that crate's `packs` module --
//! and the two share nothing but the trait and the cipher underneath them.
//!
//! # The frame format
//!
//! One pack file is a sequence of independently sealed frames:
//!
//! ```text
//!   len (u32 LE) | sealed bytes (nonce || ciphertext || tag)
//!   len (u32 LE) | sealed bytes
//!   ...
//! ```
//!
//! `len` is the sealed length, in the clear -- the same trade every framing
//! format in this codebase makes, and it reveals nothing a directory listing
//! of individually-sealed attachments would not. Each frame is sealed with
//! associated data naming the pack and the frame's own offset (see
//! [`pack_aad`]), which is what makes [`FilePackStore::read`] a seek and an
//! open rather than a scan from the start, and what stops a frame moved or
//! duplicated within a pack -- or grafted from a different one -- decrypting
//! as though nothing had changed.
//!
//! # Recovering from a crash mid-write
//!
//! A batch is flushed once, at the end, after every frame in it has been
//! written -- so the only way a pack can hold a torn frame is a crash
//! between two of those writes. Recovering from that is the *write* path's
//! job alone: before it appends, [`FilePackStore`] walks the pack's frames
//! from the start, stops at the first one that does not fully fit in what
//! is on disk, and truncates away everything after that point. Every frame
//! before it was the last byte of a batch that finished and was flushed, so
//! it is kept; anything after it was never durable and is discarded, the
//! same way a half-written WAL record is discarded by any database that
//! replays one.
//!
//! Every *other* caller -- anything that only wants to know where a pack's
//! frames end, never mind writing to it -- must never truncate on the way
//! there. [`scan_frames`] is the read-only form: it walks the identical
//! length prefixes and reports where the walk stopped, but never opens the
//! file for writing and never shortens it. [`pack_dead_stats`],
//! [`FilePackStore::compaction_worthwhile`] and [`compact_one_pack`] all go
//! through it rather than the write path's `recover_valid_len`, and for a
//! reason sharper than "reads should not write": a frame's length prefix is
//! cleartext and unauthenticated, so a single corrupted byte in the
//! *middle* of an otherwise-intact pack desyncs this same walk exactly the
//! way a genuine torn tail does. Nothing about the walk itself can tell
//! "this is where a crash stopped" apart from "this is where a bit flip
//! sent us off the rails, with perfectly good frames still sitting later in
//! the file that the walk can no longer find." Before this was split, the
//! read path shared the write path's function -- and its truncation --
//! which meant a decrypt-nothing check like `compaction_worthwhile` could
//! silently discard every later frame in a pack, live rows and all, on
//! nothing more than a flipped bit. See [`scan_frames`]'s own docs for the
//! one heuristic this module uses to tell the two apart, and for why it
//! cannot always succeed: when it cannot, every caller here fails loudly
//! instead of guessing which half of the file to keep.
//!
//! # Data-safety invariants, stated plainly
//!
//! Compaction exists to reclaim space, never to risk a message -- the whole
//! module is written so that a bug anywhere else in the tree degrades to
//! "wasted disk space", not "lost mail". Four rules make that true:
//!
//! 1. **One writer per account, always.** [`PackStore::append_batch`] and
//!    [`PackStore::compact`] must never run concurrently for the same
//!    account -- see "the write claim" below for what enforces this today.
//!    A pack referenced by a row that has not committed yet must not exist
//!    from compaction's point of view; the two guards below exist *because*
//!    this promise, though true of every caller in this codebase, is not
//!    provable from the trait's types alone.
//! 2. **Compaction never deletes.** [`PackStore::compact`] only ever reads
//!    and writes fresh packs; [`PackStore::drop_packs`] is the only method
//!    that removes a byte, and only once a caller has durably repointed
//!    everything that named the old address. See "a two-step contract, not
//!    a one-step promise" on `compact` itself.
//! 3. **The orphan sweep only ever removes what three separate guards agree
//!    is unreachable.** A candidate pack must be absent from
//!    [`ReferencedSnapshot::referenced`], and never carry an id past its
//!    [`ReferencedSnapshot::high_water`] mark (created after the snapshot
//!    was taken, so the query could never have known about it). Past that:
//!    an empty `referenced` list is trusted only once every remaining
//!    candidate's own `.dead` bookkeeping independently proves it holds no
//!    live frames, or the whole sweep is refused for that call, not
//!    narrowed; a *non-empty* `referenced` list is trusted for every
//!    candidate except the account's single newest pack, which may still be
//!    open for more appends and so is never swept on absence from the list
//!    alone. See [`PackStore::compact`]'s own docs for the full reasoning
//!    behind each.
//! 4. **A crash at any point recovers on the next call.** Between
//!    `compact` returning and its caller committing the remap: every old
//!    address is untouched, so nothing is lost. Between committing the
//!    remap and calling `drop_packs`: the next `compact` call's orphan
//!    sweep reclaims the leftover pack itself, because by then it is
//!    genuinely unreferenced. Nothing here depends on a caller retrying in
//!    any particular way, only on the next `compact` call eventually
//!    happening.
//! 5. **A frame walk that cannot prove it reached a clean boundary is never
//!    trusted to decide what is dead, what is compactable, or what is safe
//!    to drop.** [`scan_frames`] reports `desynced` rather than a plain
//!    length whenever a frame's declared size points past the end of the
//!    file by more than any real write could produce -- the one case this
//!    module cannot tell apart from a genuine torn tail using length
//!    prefixes alone. [`pack_dead_stats`] turns that into an `Err`, which
//!    [`FilePackStore::compaction_worthwhile`] logs and skips past (that one
//!    pack contributes nothing to the answer, but every other pack in the
//!    account still does) and which [`compact_one_pack`] surfaces the same
//!    way a corrupt frame already made it fail -- caught by
//!    [`PackStore::compact`]'s own per-pack `Err` handling, so the pack is
//!    left exactly where it is rather than rewritten from a walk that may
//!    have missed live frames entirely.
//!
//! # The write claim
//!
//! Every caller in this codebase reaches [`PackStore::compact`] through
//! exactly one place: `everyday_service::mailsync::task`, between an
//! account's sync passes, only once that pass's own `append_batch` calls
//! have all returned and the outbox has drained -- see that module's own
//! docs for the sequencing. [`FilePackStore`] backs this with its own
//! coarse lock (below) as a safety net against two tasks in the *same*
//! process racing each other, not as the throughput design; nothing in this
//! module can enforce the invariant against a second *process* holding the
//! vault's write claim, which is why rule 3 above exists as well, not only
//! rule 1.

use crate::crypto::Cipher;
use crate::error::{Error, Result};
use crate::fsutil;
use crate::id::PackId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A snapshot of which packs an account's messages currently reference,
/// paired with a high-water mark that makes it safe for
/// [`PackStore::compact`]'s orphan sweep to act on even though nothing
/// durable freezes the moment it was taken.
///
/// # Why the pairing, not just the list
///
/// `referenced` alone cannot tell an orphan sweep "nothing has ever named
/// this pack" apart from "a message naming this pack has not committed
/// yet" -- both look identical: absent from the list. The sweep in
/// [`PackStore::compact`] resolves the ambiguity by never touching a pack
/// newer than `high_water`, or the account's current, possibly still-open
/// pack, regardless of what `referenced` says -- see that method's own
/// docs for the whole of the guard. `high_water` is what makes that
/// possible: it is read from the pack store itself, *before* `referenced`
/// is queried, so any pack created from that instant on is guaranteed to
/// sort after it.
///
/// # Building one
///
/// [`crate::store::mail::MailStore::referenced_snapshot`] is the blessed
/// way: it reads a [`PackStore::high_water_mark`] first and only then
/// queries [`crate::store::mail::MailStore::referenced_pack_ids`], which is
/// the ordering this type's whole safety argument depends on.
/// [`ReferencedSnapshot::new`] is public too -- mainly for tests that need
/// to construct a deliberately stale or empty snapshot to prove a guard
/// holds -- but every production caller in this codebase goes through the
/// trait method instead.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReferencedSnapshot {
    referenced: BTreeSet<PackId>,
    high_water: Option<PackId>,
}

impl ReferencedSnapshot {
    /// Build a snapshot directly from a reference list and a high-water
    /// mark. See the type's own docs for why
    /// [`crate::store::mail::MailStore::referenced_snapshot`] is the
    /// ordinary way to get one instead.
    pub fn new(referenced: impl IntoIterator<Item = PackId>, high_water: Option<PackId>) -> Self {
        Self { referenced: referenced.into_iter().collect(), high_water }
    }

    /// Every pack id this snapshot says is currently referenced.
    pub fn referenced(&self) -> &BTreeSet<PackId> {
        &self.referenced
    }

    /// The newest pack id that existed when this snapshot's high-water mark
    /// was captured, or `None` if the account had no packs at all yet.
    pub fn high_water(&self) -> Option<PackId> {
        self.high_water
    }
}

/// A length-prefixed frame's length field, in bytes.
const FRAME_PREFIX_LEN: u64 = 4;

/// A pack stops taking new messages once it reaches this size and a fresh
/// one is started instead.
///
/// Bounded packs are what makes compaction cheap: rewriting a pack means
/// decrypting and re-sealing everything still live in it, and a vault whose
/// packs could grow forever would make that cost unbounded too. 64 MiB holds
/// tens of thousands of ordinary messages -- comfortably more than one
/// account's IMAP fetch produces in one sync batch -- while staying small
/// enough that a full rewrite is milliseconds of work, not seconds.
pub const PACK_ROTATE_BYTES: u64 = 64 * 1024 * 1024;

/// Where one message lives: which pack, and where in it.
///
/// This is the whole address -- `read` needs nothing else to find and open
/// the bytes it names. `account` is carried on the reference itself, rather
/// than passed alongside it the way it is to [`PackStore::append_batch`],
/// because a reference has to be able to answer "where am I" entirely on its
/// own: it is what `messages.pack_id`, `pack_offset` and `pack_len` will
/// hold once the mail domain exists to have a `messages` table at all (see
/// the schema section of `docs/plans/mail.md`), long after the
/// [`PackStore`] that minted it is out of scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PackRef {
    /// The account this pack belongs to, exactly as it was passed to
    /// [`PackStore::append_batch`].
    pub account: String,
    /// Which pack -- which file, on a file-backed store; which row's
    /// identity, on a table-backed one.
    pub pack: PackId,
    /// Byte offset of the sealed message, *after* any framing prefix a
    /// backend uses. A table-backed store, which frames nothing, leaves
    /// this `0`.
    pub offset: u64,
    /// Sealed length in bytes.
    pub len: u32,
}

/// Storage for raw messages, sealed on their own but flushed together.
///
/// See the module docs for the shape this exists to give: one `fsync` per
/// batch rather than one per message, and a read that touches only the
/// message asked for.
pub trait PackStore: Send + Sync {
    /// Seal and append every message in `messages`, in order, to `account`'s
    /// current pack, flushing once when the whole batch has been written.
    ///
    /// An empty `messages` touches no storage at all and returns an empty
    /// vector -- there is no batch to flush.
    fn append_batch(&self, account: &str, messages: &[&[u8]]) -> Result<Vec<PackRef>>;

    /// The one message `r` names, decrypted, without touching anything
    /// sealed beside it.
    fn read(&self, r: &PackRef) -> Result<Vec<u8>>;

    /// Mark every message in `refs` as no longer worth keeping.
    ///
    /// Does not reclaim the space itself -- a pack is an append-only log, not
    /// something a single dead frame can be cut out of in place -- that is
    /// what [`PackStore::compact`] is for. Marking a message dead twice, or
    /// one this store has already forgotten, is not an error.
    fn mark_dead(&self, refs: &[PackRef]) -> Result<()>;

    /// Delete every pack `account` has, whole -- what deleting the account
    /// itself calls, after its `mailboxes`/`mail_messages`/... rows are
    /// gone, so nothing is left pointing at bytes this removes. Deleting an
    /// account that never had any packs (never synced, or already deleted)
    /// is not an error.
    fn delete_account(&self, account: &str) -> Result<()>;

    /// The newest pack id that exists for `account` right now, or `None` if
    /// it has no packs at all -- read *before*
    /// [`crate::store::mail::MailStore::referenced_pack_ids`] is queried,
    /// which is what [`ReferencedSnapshot::high_water`] then protects: any
    /// pack [`PackStore::append_batch`] creates after this call returns is
    /// guaranteed to carry an id that sorts after it. Cheap on every
    /// backend -- a directory listing or a single `MAX`, never a decrypt.
    fn high_water_mark(&self, account: &str) -> Result<Option<PackId>>;

    /// A decrypt-nothing estimate of whether [`PackStore::compact`] would
    /// actually do anything useful for `account` right now: some pack is at
    /// least a third dead by [`PackStore::mark_dead`]'s own bookkeeping, or
    /// an orphan pack -- one `snapshot` no longer names, left over from an
    /// earlier call interrupted before [`PackStore::drop_packs`] ran --
    /// exists and is old enough and settled enough for the orphan sweep to
    /// reach (see `compact`'s own guards). Reads pack listings, `.dead`
    /// markers and frame *length* headers only; nothing this walks is ever
    /// opened with the cipher, so a caller may check this on every
    /// sync-pass wake without cost.
    ///
    /// Always `false` on a row-backed store: [`PackStore::mark_dead`]
    /// already deletes the row outright there, so nothing is ever left to
    /// reclaim -- see `everyday_store_sql::packs`'s own module docs.
    fn compaction_worthwhile(&self, account: &str, snapshot: &ReferencedSnapshot) -> Result<bool>;

    /// Rewrite every pack of `account`'s that is at least a third dead,
    /// dropping what [`PackStore::mark_dead`] marked and keeping everything
    /// else -- and, first, reclaim any pack already left behind by a
    /// previous call that never finished being cleaned up.
    ///
    /// `should_continue` is polled once between packs -- never mid-rewrite
    /// of a single one -- so a caller with a large compaction to run on a
    /// blocking thread can stop promptly when its own stop signal fires
    /// without ever leaving a pack half-rewritten: whatever `remap` and
    /// `obsolete` this returns cover only work that is already fully
    /// durable, on the same terms a full run's result does. Pass `&|| true`
    /// to never stop early.
    ///
    /// # A two-step contract, not a one-step promise
    ///
    /// This method **never deletes anything**. Earlier, it deleted each old
    /// pack the moment its replacement was written -- which is safe against
    /// a crash *inside* one pack's rewrite (the old file is still there
    /// until the new one is fully durable), but not against a crash *after*
    /// `compact` returns and before the caller has finished persisting the
    /// remap somewhere else that also names the old address (a `messages`
    /// row's `pack_id`/`pack_offset`/`pack_len`, most of all). That second
    /// window used to lose mail for good: the old pack was already gone,
    /// and nothing durable yet pointed at the new one.
    ///
    /// The fix splits the work three ways, and the whole point is that
    /// nothing here ever deletes a pack a live reference might still name:
    ///
    /// 1. `compact` writes every replacement pack, durably, and returns the
    ///    remap plus the list of packs it made obsolete. It touches no old
    ///    pack at all.
    /// 2. The caller commits that remap -- durably, in one transaction --
    ///    to whatever else pointed at the old addresses.
    /// 3. Only then does the caller call [`PackStore::drop_packs`] on the
    ///    obsolete list from step 1.
    ///
    /// A crash between 1 and 2 leaves every message still reachable at its
    /// old, untouched address -- `compact` changed nothing durable yet. A
    /// crash between 2 and 3 leaves the old packs sitting on disk,
    /// unreferenced by anything any more; the *next* `compact` call reclaims
    /// them as part of its own first step, below, so nothing is lost and
    /// nothing but disk space is wasted in between.
    ///
    /// # Orphan sweep, first -- guarded three separate ways
    ///
    /// Before rewriting anything, `compact` considers deleting every pack
    /// `account` has that is not named in `snapshot.referenced()`. Three
    /// independent guards decide whether a given candidate actually goes:
    ///
    /// 1. **Never one past the high-water mark.** A pack younger than
    ///    `snapshot.high_water()` was created after the reference query
    ///    ran, so that query could never have named it even if it holds
    ///    nothing but live, freshly committed messages -- see
    ///    [`ReferencedSnapshot`]'s own docs. Applies to every candidate,
    ///    unconditionally.
    /// 2. **An empty `snapshot.referenced()` proves nothing by itself.**
    ///    The pre-fix behaviour treated every existing pack as an orphan
    ///    the moment `referenced` came back empty, which made a caller
    ///    whose query happened to come back empty -- a bug, a bad
    ///    connection, a wrong account id -- wipe out every message the
    ///    account had. Now: when `referenced` is empty, every candidate
    ///    that guard 1 leaves standing must *also* prove, from its own
    ///    `.dead` bookkeeping alone (no cipher touched), that it holds no
    ///    live frames at all. If even one candidate fails that proof, the
    ///    *whole* sweep for this call is refused -- nothing is deleted, not
    ///    even candidates that did pass -- because an empty list over live
    ///    data means the input itself is not trusted, not that everything
    ///    else happens to be orphaned. `snapshot` genuinely reflecting zero
    ///    live messages -- an emptied account -- still reclaims everything,
    ///    the same as before: every candidate's `.dead` file lists every
    ///    frame it has.
    /// 3. **A *non-empty* `snapshot.referenced()` still never sweeps the
    ///    account's single newest pack.** [`FilePackStore::current_pack`]
    ///    reuses that pack for the next `append_batch` until it is full, so
    ///    its absence from an otherwise-trustworthy `referenced` list is
    ///    exactly what a message committed moments after the reference
    ///    query ran, into that same pack, would look like -- the one
    ///    ambiguity `high_water` alone cannot resolve, since the pack
    ///    itself already existed when the mark was taken. Guard 2 already
    ///    covers this when `referenced` is empty (a genuinely dead newest
    ///    pack proves itself through its own bookkeeping instead), so this
    ///    guard only adds anything when `referenced` is not.
    ///
    /// Every pack this sweep removes is folded into the returned
    /// [`CompactionResult::obsolete`] too, so a caller has one list to hand
    /// [`PackStore::drop_packs`] -- redundant for these (they are already
    /// gone) but harmless, since deleting an already-deleted pack is not an
    /// error.
    ///
    /// # Callers must serialise with `append_batch`
    ///
    /// `snapshot` must be built while nothing is concurrently appending new
    /// packs for `account` -- true of every caller today, which reaches
    /// this through the one writer the vault's own write claim already
    /// promises (the same "existing write claim" [`FilePackStore`]'s own
    /// lock is a safety net for, not a throughput design, per its own
    /// docs). The three guards above are what keep a momentary breach of
    /// that promise from losing anything regardless.
    ///
    /// # Partial failure
    ///
    /// A pack that fails to compact -- most likely a corrupt frame -- is
    /// skipped and left exactly as it was, rather than aborting the whole
    /// call: every other pack in `account`, including ones already
    /// rewritten earlier in the same call, still compacts and is still
    /// included in the result. A skipped pack's messages remain fully
    /// readable at their old, untouched addresses; nothing about them is
    /// lost, only the space they would have reclaimed.
    ///
    /// A pack under the third-dead threshold is left exactly where it is
    /// and contributes nothing to the result.
    fn compact(
        &self,
        account: &str,
        snapshot: &ReferencedSnapshot,
        should_continue: &dyn Fn() -> bool,
    ) -> Result<CompactionResult>;

    /// Delete every pack named in `packs`, outright -- the second half of
    /// [`PackStore::compact`]'s two-step contract; see that method's own
    /// docs for when this may be called. Deleting a pack that does not
    /// exist -- already gone, or never did -- is not an error, so a caller
    /// need not track what it has already asked for.
    fn drop_packs(&self, account: &str, packs: &[PackId]) -> Result<()>;
}

/// What one [`PackStore::compact`] call accomplished: the remap for every
/// message that moved, and every pack now safe to
/// [`PackStore::drop_packs`] -- but *only* once the caller has durably
/// committed `remap` to whatever else named the old addresses. See
/// [`PackStore::compact`]'s own docs for the whole of this contract.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactionResult {
    pub remap: Vec<(PackRef, PackRef)>,
    pub obsolete: Vec<PackId>,
    /// How many packs were actually read and rewritten into a fresh
    /// replacement -- distinct from `obsolete`'s length, which also counts
    /// pure orphan-sweep deletes that were never rewritten at all. What the
    /// account task's summary log line calls "packs rewritten".
    pub packs_rewritten: usize,
    /// A best-effort estimate, in bytes, of what this call will free once
    /// its caller finishes the two-step contract and calls
    /// [`PackStore::drop_packs`]: for a rewritten pack, the old pack's size
    /// minus the new one's; for a pure orphan sweep, the whole old pack.
    /// Never load-bearing -- only ever logged -- so a backend that cannot
    /// cheaply measure it (a row-backed store, say) may always leave this
    /// `0`.
    pub bytes_reclaimed: u64,
}

impl CompactionResult {
    /// Nothing moved and nothing is obsolete -- what a `referenced` sweep
    /// that finds no orphans, over packs all under the dead threshold,
    /// answers with.
    pub fn is_empty(&self) -> bool {
        self.remap.is_empty() && self.obsolete.is_empty()
    }
}

/// Associated data binding a sealed frame to the one pack and offset it was
/// written at.
///
/// Binding the offset, not just the pack, is what makes a frame's identity
/// track where it actually lives: two frames written to the same pack seal
/// under different associated data even though they share a `PackId`, so
/// one cannot be copied over the other -- or read at the wrong offset after
/// some future bug got the arithmetic wrong -- and have it open.
pub fn pack_aad(pack: PackId, offset: u64) -> Vec<u8> {
    format!("everyday.mailpack.v1:{pack}:{offset}").into_bytes()
}

/// Append-only pack files beside wherever a local vault keeps its media.
///
/// One subdirectory per account, named by a hash of the account string
/// rather than the string itself -- the same reason [`FileBlobStore`] fans
/// blobs out by content hash rather than by anything meaningful: an account
/// id is not secret, but there is no reason for a directory listing to say
/// anything at all, and hashing costs nothing an opaque UUID would not
/// already have cost. Within that directory, packs are named by their
/// [`PackId`] and rotate once they reach [`PACK_ROTATE_BYTES`]; see the
/// module docs for the frame format inside one.
///
/// [`FileBlobStore`]: crate::blobstore::FileBlobStore
pub struct FilePackStore {
    root: PathBuf,
    cipher: Arc<dyn Cipher>,
    /// Serialises every method against every other, on this store. A single
    /// coarse lock rather than one per account: the vault this exists for
    /// already promises exactly one writer at a time -- see "the existing
    /// write claim" in `docs/plans/mail.md`'s risk table -- so this is a
    /// safety net against two tasks in the *same* process racing each
    /// other, not a throughput design.
    lock: Mutex<()>,
    /// [`PACK_ROTATE_BYTES`] everywhere but this crate's own tests, which
    /// shrink it so that exercising rotation and compaction writes kilobytes
    /// rather than several 64 MB packs at once into a shared temp directory.
    rotate_bytes: u64,
}

impl std::fmt::Debug for FilePackStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilePackStore").field("root", &self.root).finish()
    }
}

impl FilePackStore {
    /// Packs rooted at `root`, sealed with `cipher`. `root` is created if it
    /// does not exist, exactly as [`FileBlobStore::open`] creates its own
    /// directory.
    ///
    /// [`FileBlobStore::open`]: crate::blobstore::FileBlobStore::open
    pub fn open(root: impl Into<PathBuf>, cipher: Arc<dyn Cipher>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(|e| Error::io(&root, e))?;
        Ok(Self { root, cipher, lock: Mutex::new(()), rotate_bytes: PACK_ROTATE_BYTES })
    }

    /// The same store with a smaller rotation threshold, for tests.
    #[cfg(test)]
    fn with_rotate_bytes(mut self, bytes: u64) -> Self {
        self.rotate_bytes = bytes;
        self
    }

    fn account_dir(&self, account: &str) -> PathBuf {
        self.root.join(blake3::hash(account.as_bytes()).to_hex().to_string())
    }

    /// Every pack that exists for an account, oldest first. `PackId`s are
    /// UUIDv7, so sorting the ids sorts by creation time with no extra work.
    fn list_packs(dir: &Path) -> Result<Vec<PackId>> {
        let mut out = Vec::new();
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(Error::io(dir, e)),
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(stem) = name.strip_suffix(".pack")
                && let Ok(id) = PackId::parse(stem)
            {
                out.push(id);
            }
        }
        out.sort();
        Ok(out)
    }

    /// The pack an [`FilePackStore::append_batch`] call should write to:
    /// the newest existing one, if it has room, otherwise a fresh one.
    ///
    /// Recovers the newest pack's valid length on the way, which is what
    /// turns "reopen after a crash" into "keep appending after a crash":
    /// see [`recover_valid_len`].
    fn current_pack(&self, dir: &Path) -> Result<(PackId, PathBuf, u64)> {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        if let Some(pack) = Self::list_packs(dir)?.into_iter().next_back() {
            let path = dir.join(format!("{pack}.pack"));
            let valid_len = recover_valid_len(&path)?;
            if valid_len < self.rotate_bytes {
                return Ok((pack, path, valid_len));
            }
        }
        let pack = PackId::new();
        let path = dir.join(format!("{pack}.pack"));
        File::create(&path).map_err(|e| Error::io(&path, e))?;
        Ok((pack, path, 0))
    }
}

impl PackStore for FilePackStore {
    fn append_batch(&self, account: &str, messages: &[&[u8]]) -> Result<Vec<PackRef>> {
        if messages.is_empty() {
            return Ok(Vec::new());
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let dir = self.account_dir(account);
        let (pack, path, mut offset) = self.current_pack(&dir)?;
        let mut file =
            OpenOptions::new().append(true).open(&path).map_err(|e| Error::io(&path, e))?;

        let mut refs = Vec::with_capacity(messages.len());
        for msg in messages {
            let frame_offset = offset + FRAME_PREFIX_LEN;
            let sealed = self.cipher.seal(&pack_aad(pack, frame_offset), msg)?;
            let len = sealed.len() as u32;
            file.write_all(&(len).to_le_bytes()).map_err(|e| Error::io(&path, e))?;
            file.write_all(&sealed).map_err(|e| Error::io(&path, e))?;
            offset = frame_offset + len as u64;
            refs.push(PackRef { account: account.to_string(), pack, offset: frame_offset, len });
        }
        // One flush for the whole batch, which is the entire point: a
        // thousand messages cost one `fsync`, not a thousand.
        file.sync_all().map_err(|e| Error::io(&path, e))?;
        // Only needed the first time a pack is created, but cheap and
        // idempotent to call regardless -- see `fsutil::sync_dir`.
        fsutil::sync_dir(&dir);
        Ok(refs)
    }

    fn read(&self, r: &PackRef) -> Result<Vec<u8>> {
        let path = self.account_dir(&r.account).join(format!("{}.pack", r.pack));
        let mut file = File::open(&path).map_err(|e| Error::io(&path, e))?;
        file.seek(SeekFrom::Start(r.offset)).map_err(|e| Error::io(&path, e))?;
        let mut sealed = vec![0u8; r.len as usize];
        file.read_exact(&mut sealed).map_err(|_| {
            Error::Invalid(format!("pack {} is truncated at offset {}", r.pack, r.offset))
        })?;
        self.cipher.open(&pack_aad(r.pack, r.offset), &sealed)
    }

    fn mark_dead(&self, refs: &[PackRef]) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut by_pack: BTreeMap<(String, PackId), Vec<u64>> = BTreeMap::new();
        for r in refs {
            by_pack.entry((r.account.clone(), r.pack)).or_default().push(r.offset);
        }
        for ((account, pack), offsets) in by_pack {
            let dir = self.account_dir(&account);
            let path = dir.join(format!("{pack}.dead"));
            let mut text = String::new();
            for offset in offsets {
                text.push_str(&offset.to_string());
                text.push('\n');
            }
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| Error::io(&path, e))?;
            file.write_all(text.as_bytes()).map_err(|e| Error::io(&path, e))?;
            file.sync_all().map_err(|e| Error::io(&path, e))?;
        }
        Ok(())
    }

    fn delete_account(&self, account: &str) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let dir = self.account_dir(account);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::io(&dir, e)),
        }
        fsutil::sync_dir(&self.root);
        Ok(())
    }

    fn high_water_mark(&self, account: &str) -> Result<Option<PackId>> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let dir = self.account_dir(account);
        Ok(Self::list_packs(&dir)?.into_iter().next_back())
    }

    fn compaction_worthwhile(&self, account: &str, snapshot: &ReferencedSnapshot) -> Result<bool> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let dir = self.account_dir(account);
        let packs = Self::list_packs(&dir)?;
        let newest = packs.last().copied();

        for pack in &packs {
            if !snapshot.referenced().contains(pack) {
                // An estimate, not a replay of `compact`'s own all-or-
                // nothing empty-`referenced` pre-scan: this may say
                // "worthwhile" for a call that turns out to refuse its
                // whole sweep once it actually checks every candidate.
                // Harmless -- the caller just attempts a `compact` that
                // logs a warning and reclaims nothing, rather than skips
                // it outright.
                if orphan_sweep_candidate(*pack, newest, snapshot) {
                    return Ok(true);
                }
                continue;
            }
            match pack_dead_stats(&dir, *pack) {
                Ok((total, dead_count)) => {
                    if total > 0 && dead_count * 3 >= total {
                        return Ok(true);
                    }
                }
                Err(e) => {
                    // A desynced frame walk (see `FrameScan::desynced`),
                    // most likely -- this is a decrypt-nothing estimate,
                    // documented to be cheap enough to call on every
                    // sync-pass wake, so one unreadable pack must not
                    // abort the whole account's check. It contributes
                    // nothing toward "worthwhile" either way; every other
                    // pack still gets its say.
                    tracing::warn!(
                        account,
                        pack = %pack,
                        error = %e,
                        "skipping a pack with an unreadable frame walk while checking \
                         whether compaction is worthwhile"
                    );
                }
            }
        }
        Ok(false)
    }

    fn compact(
        &self,
        account: &str,
        snapshot: &ReferencedSnapshot,
        should_continue: &dyn Fn() -> bool,
    ) -> Result<CompactionResult> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let dir = self.account_dir(account);
        let packs = Self::list_packs(&dir)?;
        let newest = packs.last().copied();
        let mut result = CompactionResult::default();

        // Guard 2: an empty `referenced` list is only trusted once every
        // high-water-eligible, unreferenced pack proves -- from its own
        // `.dead` bookkeeping alone -- that it genuinely holds no live
        // frames. One that cannot means the whole sweep for this call is
        // refused; see `PackStore::compact`'s own docs.
        if snapshot.referenced().is_empty() {
            for &pack in &packs {
                if !sweep_eligible(pack, snapshot.high_water()) {
                    continue;
                }
                if !pack_is_fully_dead(&dir, pack)? {
                    tracing::warn!(
                        account,
                        pack = %pack,
                        "refusing an orphan sweep: `referenced` was empty but a pack \
                         still holds live frames"
                    );
                    return Ok(result);
                }
            }
        }

        for pack in packs {
            if !should_continue() {
                break;
            }
            if snapshot.referenced().contains(&pack) {
                match compact_one_pack(&self.cipher, &dir, account, pack) {
                    Ok(Some((_new_pack, pack_remap, bytes_reclaimed))) => {
                        // `_new_pack` (named only for `compact_one_pack`'s
                        // own return shape) is brand new and, by
                        // definition, not yet in `snapshot` -- nothing has
                        // committed a reference to it yet, that being
                        // exactly what the caller does next. It is never
                        // treated as an orphan by *this* call: `packs`,
                        // above, was already read before this pack
                        // existed, so this loop never reaches it at all.
                        result.remap.extend(pack_remap);
                        result.obsolete.push(pack);
                        result.packs_rewritten += 1;
                        result.bytes_reclaimed += bytes_reclaimed;
                    }
                    Ok(None) => {
                        // Fewer than a third dead (or nothing in the pack
                        // at all): leave it exactly where it is.
                    }
                    Err(e) => {
                        // A corrupt frame, most likely. This one pack is
                        // left untouched -- its messages are still fully
                        // readable at their existing addresses -- and
                        // every other pack in this account, including ones
                        // already compacted earlier in this same call, is
                        // unaffected: `result` so far is not discarded,
                        // and the loop carries on.
                        tracing::warn!(
                            account,
                            pack = %pack,
                            error = %e,
                            "skipping a pack that failed to compact"
                        );
                    }
                }
                continue;
            }
            if !orphan_sweep_candidate(pack, newest, snapshot) {
                // Either past the high-water mark, or -- `referenced` being
                // non-empty -- the account's single newest pack: never
                // treated as an orphan, no matter what
                // `snapshot.referenced()` says. See `compact`'s own docs
                // for both guards.
                continue;
            }
            // Not referenced, and every guard above has cleared it: safe
            // to reclaim outright.
            if let Ok(len) = std::fs::metadata(dir.join(format!("{pack}.pack"))) {
                result.bytes_reclaimed += len.len();
            }
            result.obsolete.push(pack);
        }
        Ok(result)
    }

    fn drop_packs(&self, account: &str, packs: &[PackId]) -> Result<()> {
        if packs.is_empty() {
            return Ok(());
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let dir = self.account_dir(account);
        for pack in packs {
            let path = dir.join(format!("{pack}.pack"));
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::io(&path, e)),
            }
            // The dead-offset marker beside it, if any -- a pack that was
            // never compacted (a pure orphan sweep target) never had one.
            let _ = std::fs::remove_file(dir.join(format!("{pack}.dead")));
        }
        fsutil::sync_dir(&dir);
        Ok(())
    }
}

/// Guard 1 of [`PackStore::compact`]'s orphan sweep, alone: never a pack
/// younger than `high_water` (created after the snapshot that fed this call
/// was taken). Applies to every sweep candidate unconditionally, including
/// the ones [`FilePackStore::compact`]'s own empty-`referenced` pre-scan
/// checks before trusting an empty list at all.
fn sweep_eligible(pack: PackId, high_water: Option<PackId>) -> bool {
    high_water.is_some_and(|hw| pack <= hw)
}

/// Is `pack` safe for [`FilePackStore::compact`]'s orphan sweep to actually
/// remove -- guards 1 and 3 from that method's own docs (guard 2, the
/// empty-`referenced` fully-dead proof, is checked once up front by
/// `compact`'s own pre-scan, which this assumes already passed if
/// `snapshot.referenced()` is empty), shared with
/// [`FilePackStore::compaction_worthwhile`] so the two never disagree.
/// Assumes `pack` is already known absent from `snapshot.referenced()`.
fn orphan_sweep_candidate(
    pack: PackId,
    newest: Option<PackId>,
    snapshot: &ReferencedSnapshot,
) -> bool {
    if !sweep_eligible(pack, snapshot.high_water()) {
        return false;
    }
    if snapshot.referenced().is_empty() {
        // Guard 2 already proved this fully dead, or `compact` would have
        // refused the whole sweep before this was ever reached.
        return true;
    }
    // Guard 3: `referenced` is non-empty and trustworthy for every other
    // pack, but the account's single newest pack may still be mid-write
    // into by an `append_batch` whose message rows have not committed yet
    // -- its absence from the list is exactly what that race looks like,
    // and nothing here can tell the two apart. Left alone; reconsidered
    // next call, once a later append has rotated a newer pack in ahead of
    // it.
    Some(pack) != newest
}

/// What [`compact_one_pack`] found: the fresh pack it wrote, the remap from
/// every old address in `pack` to its new one there, and how many bytes the
/// rewrite freed (the old pack's valid length minus the new one's, floored
/// at zero).
type PackCompaction = (PackId, Vec<(PackRef, PackRef)>, u64);

/// The compaction of one pack, in isolation: read its live frames, reseal
/// each into a fresh pack, and write that pack durably. Returns `Ok(None)`
/// for a pack under the dead threshold (left alone, not an error), and
/// `Err` for one this could not read cleanly -- a corrupt frame, most often
/// -- which [`FilePackStore::compact`] catches and skips rather than
/// letting fail the whole call. Never deletes the old pack; that is
/// [`FilePackStore::drop_packs`]'s job, once the caller has committed the
/// remap this returns.
fn compact_one_pack(
    cipher: &Arc<dyn Cipher>,
    dir: &Path,
    account: &str,
    pack: PackId,
) -> Result<Option<PackCompaction>> {
    let path = dir.join(format!("{pack}.pack"));
    let dead_path = dir.join(format!("{pack}.dead"));
    // Read-only: `compact_one_pack` must never truncate the pack it is
    // about to rewrite (that is the write path's job, at open time, on a
    // pack about to be appended to -- not compaction's). A desynced walk
    // means this pack must not be treated as compactable at all -- the
    // `Err` here is caught by `FilePackStore::compact`'s own per-pack
    // handling, the same as any other corrupt frame, and the pack is left
    // exactly where it is.
    let scan = scan_frames(&path)?;
    if scan.desynced {
        return Err(Error::Invalid(format!(
            "pack {pack} has a frame length prefix that does not look like a torn \
             tail from a crash; refusing to compact it, since the walk may have lost \
             track of live frames past that point"
        )));
    }
    let valid_len = scan.valid_len;
    let dead = read_dead_offsets(&dead_path)?;
    let offsets = frame_offsets(&path, valid_len)?;
    let total = offsets.len() as u64;
    let live: Vec<(u64, u32)> =
        offsets.into_iter().filter(|(offset, _)| !dead.contains(offset)).collect();
    if total == 0 || (total - live.len() as u64) * 3 < total {
        return Ok(None);
    }

    let mut file = File::open(&path).map_err(|e| Error::io(&path, e))?;
    let new_pack = PackId::new();
    let mut rewritten = Vec::new();
    let mut new_offset = 0u64;
    let mut pack_remap = Vec::with_capacity(live.len());
    for (offset, len) in live {
        file.seek(SeekFrom::Start(offset)).map_err(|e| Error::io(&path, e))?;
        let mut sealed = vec![0u8; len as usize];
        file.read_exact(&mut sealed).map_err(|e| Error::io(&path, e))?;
        let plain = cipher.open(&pack_aad(pack, offset), &sealed)?;

        let new_frame_offset = new_offset + FRAME_PREFIX_LEN;
        let resealed = cipher.seal(&pack_aad(new_pack, new_frame_offset), &plain)?;
        let new_len = resealed.len() as u32;
        rewritten.extend_from_slice(&new_len.to_le_bytes());
        rewritten.extend_from_slice(&resealed);
        new_offset = new_frame_offset + new_len as u64;

        pack_remap.push((
            PackRef { account: account.to_string(), pack, offset, len },
            PackRef {
                account: account.to_string(),
                pack: new_pack,
                offset: new_frame_offset,
                len: new_len,
            },
        ));
    }

    // Durable -- via `write_atomic` -- before this returns, and the old
    // pack is never touched here at all: see `PackStore::compact`'s own
    // docs for why deleting it is now a separate, later step.
    let new_path = dir.join(format!("{new_pack}.pack"));
    fsutil::write_atomic(&new_path, &rewritten, &fsutil::unique_tag())?;
    fsutil::sync_dir(dir);

    let bytes_reclaimed = valid_len.saturating_sub(rewritten.len() as u64);
    Ok(Some((new_pack, pack_remap, bytes_reclaimed)))
}

/// Every frame offset and length currently in `path`, up to `valid_len`,
/// from a length-prefix-only walk -- no sealed bytes are ever read or
/// decrypted. Shared by [`compact_one_pack`]'s own rewrite and by
/// [`pack_dead_stats`], which every live/dead reasoning about a pack that
/// must not pay for a decrypt goes through.
fn frame_offsets(path: &Path, valid_len: u64) -> Result<Vec<(u64, u32)>> {
    let mut file = File::open(path).map_err(|e| Error::io(path, e))?;
    let mut offsets = Vec::new();
    let mut pos = 0u64;
    while pos + FRAME_PREFIX_LEN <= valid_len {
        file.seek(SeekFrom::Start(pos)).map_err(|e| Error::io(path, e))?;
        let mut len_buf = [0u8; FRAME_PREFIX_LEN as usize];
        file.read_exact(&mut len_buf).map_err(|e| Error::io(path, e))?;
        let len = u32::from_le_bytes(len_buf);
        let frame_offset = pos + FRAME_PREFIX_LEN;
        offsets.push((frame_offset, len));
        pos = frame_offset + len as u64;
    }
    Ok(offsets)
}

/// `pack`'s total frame count and how many of them `.dead` names -- a
/// decrypt-nothing summary shared by
/// [`FilePackStore::compaction_worthwhile`] and [`pack_is_fully_dead`]. A
/// pack that does not exist (already reclaimed, or never had a frame
/// written) reads as `(0, 0)`.
///
/// Reads the pack read-only, through [`scan_frames`] -- this must never
/// truncate, since it runs on every `compaction_worthwhile` check, which is
/// documented to cost nothing and touch nothing on every sync-pass wake.
/// Returns `Err` when the frame walk desynchronises (see
/// [`FrameScan::desynced`]) rather than reporting statistics computed from a
/// walk that may have lost track of live frames -- every caller here treats
/// that as "leave this pack alone," never as license to keep walking on bad
/// information.
fn pack_dead_stats(dir: &Path, pack: PackId) -> Result<(u64, u64)> {
    let path = dir.join(format!("{pack}.pack"));
    let dead_path = dir.join(format!("{pack}.dead"));
    let scan = scan_frames(&path)?;
    if scan.desynced {
        return Err(Error::Invalid(format!(
            "pack {pack} has a frame length prefix that does not look like a torn \
             tail from a crash; refusing to report dead-frame statistics computed \
             from a walk that may have lost track of live frames past that point"
        )));
    }
    let dead = read_dead_offsets(&dead_path)?;
    let offsets = frame_offsets(&path, scan.valid_len)?;
    let total = offsets.len() as u64;
    let dead_count = offsets.iter().filter(|(offset, _)| dead.contains(offset)).count() as u64;
    Ok((total, dead_count))
}

/// Does `pack` hold not one live frame, proved from its own `.dead`
/// bookkeeping alone? What [`FilePackStore::compact`]'s guard on an empty
/// `referenced` list asks of every sweep-eligible candidate before ever
/// trusting the list. A pack with zero frames at all answers `true`
/// trivially -- there is nothing live in it to miss.
fn pack_is_fully_dead(dir: &Path, pack: PackId) -> Result<bool> {
    let (total, dead_count) = pack_dead_stats(dir, pack)?;
    Ok(total == dead_count)
}

/// A sanity ceiling on one sealed frame's declared length, used only to tell
/// a genuine torn tail (the last frame in a pack, cut short mid-write by a
/// crash) apart from a length prefix corrupted into nonsense -- see
/// [`scan_frames`]'s own docs for why the two otherwise look identical to a
/// walk that only reads length prefixes.
///
/// `everyday-mail` keeps large attachments in the blob store rather than in
/// a raw message pack (see the module docs), so no real frame should
/// approach this; it exists purely to reject a `u32` that a single flipped
/// bit sent far outside anything a real write could ever have produced. Set
/// well above [`PACK_ROTATE_BYTES`] rather than at it, since a single
/// message larger than the rotation threshold still gets one frame, in a
/// pack that then simply exceeds the threshold once.
const MAX_PLAUSIBLE_FRAME_LEN: u64 = 512 * 1024 * 1024;

/// What walking `path`'s frames by their length prefixes, from the start,
/// found -- without touching the file at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameScan {
    /// Bytes accounted for by complete frames, back to back, from offset 0.
    valid_len: u64,
    /// The file's actual length on disk right now.
    total_len: u64,
    /// `true` once the walk stopped because a frame's declared length
    /// pointed further past the end of the file than any real write could
    /// produce (see [`MAX_PLAUSIBLE_FRAME_LEN`]) -- the one signal this
    /// walk has that it is looking at a corrupted length prefix rather than
    /// a torn tail from a crash. A pack in this state must never be
    /// truncated and must never be treated as compactable: the frames the
    /// walk lost track of when it desynced may still be sitting, perfectly
    /// intact, later in the file.
    desynced: bool,
}

impl FrameScan {
    /// The walk reached the end of the file with nothing left over: either
    /// every byte belongs to a complete frame, or what remains is too short
    /// to even be a length prefix. Either way there is no ambiguity to
    /// resolve -- what is past `valid_len`, if anything, is a genuine torn
    /// tail and nothing else.
    fn clean(&self) -> bool {
        self.valid_len == self.total_len
    }
}

/// Walk `path`'s frames by their length prefixes alone, from the start, and
/// report where the walk stopped -- without opening the file for writing or
/// shortening it. The read-only counterpart to `recover_valid_len` below;
/// every caller that only wants to know how much of a pack is trustworthy,
/// never mind writing to it, must come through here instead.
///
/// # Why this cannot always tell a crash apart from corruption
///
/// A frame's `len` is read from the frame itself, in the clear and
/// unauthenticated -- AEAD only proves the *sealed bytes* were not tampered
/// with once a frame is fully read, and this walk never gets that far for
/// one that does not fit. A single flipped byte in a length prefix
/// *anywhere* in an otherwise-intact pack desyncs the walk exactly the way
/// a genuine crash mid-write does: both stop with "the next frame does not
/// fully fit in what's on disk." What actually differs is what is true of
/// the bytes that follow: a crash leaves nothing coherent after the one
/// torn frame, while corruption in the middle leaves perfectly good frames
/// the walk can no longer find, because it jumped off track reading a
/// bogus length.
///
/// This function does not try to resynchronise and search for those frames
/// -- it draws exactly one line, using [`MAX_PLAUSIBLE_FRAME_LEN`]: a
/// declared length larger than any real write could ever produce marks the
/// walk [`FrameScan::desynced`] rather than merely short. It is a
/// conservative, one-sided check -- a corrupted length that still happens
/// to land within the file is not caught here, because nothing short of
/// authenticating the length itself could catch that -- but it is exactly
/// the shape of corruption the module's callers must never guess about:
/// silently discarding, or silently compacting away, frames a bit flip only
/// pretended did not exist.
fn scan_frames(path: &Path) -> Result<FrameScan> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FrameScan { valid_len: 0, total_len: 0, desynced: false });
        }
        Err(e) => return Err(Error::io(path, e)),
    };
    let total = file.metadata().map_err(|e| Error::io(path, e))?.len();

    let mut pos = 0u64;
    let mut desynced = false;
    loop {
        if pos + FRAME_PREFIX_LEN > total {
            // Not even a full length prefix left on disk: unambiguously
            // the literal end of whatever was durably written, whether
            // that is a clean end-of-file or a crash between two frames.
            break;
        }
        file.seek(SeekFrom::Start(pos)).map_err(|e| Error::io(path, e))?;
        let mut len_buf = [0u8; FRAME_PREFIX_LEN as usize];
        if file.read_exact(&mut len_buf).is_err() {
            break;
        }
        let len = u32::from_le_bytes(len_buf) as u64;
        let frame_end = pos + FRAME_PREFIX_LEN + len;
        if frame_end > total {
            if len > MAX_PLAUSIBLE_FRAME_LEN {
                desynced = true;
            }
            break;
        }
        pos = frame_end;
    }

    Ok(FrameScan { valid_len: pos, total_len: total, desynced })
}

/// Walk `path`'s frames from the start, and truncate away anything after the
/// last one that is fully there. The **write** path's own recovery,
/// called only when [`FilePackStore`] is about to append to a pack (see
/// [`FilePackStore::current_pack`]) -- every read-only caller must use
/// [`scan_frames`] instead, which reports the identical walk without ever
/// touching the file. See the module docs' "Recovering from a crash
/// mid-write" section for why the split matters.
///
/// Refuses to touch the file, returning `Err`, when the walk cannot tell a
/// torn tail apart from a corrupted length prefix (see [`scan_frames`] and
/// [`FrameScan::desynced`]) -- truncating in that case could discard frames
/// that are still perfectly intact later in the file. A pack that fails
/// this way needs a person, not a guess; nothing durable is lost by
/// refusing, since nothing here has touched the file yet.
///
/// Returns the number of bytes that are valid -- which, after this call
/// returns `Ok`, is also the file's length. A file that does not exist yet
/// has zero valid bytes and nothing to truncate.
///
/// This is an `O(frames already in the pack)` scan, run on every
/// [`FilePackStore::append_batch`] call against the pack being appended to.
/// That is deliberately not optimised further here: packs are bounded by
/// [`PACK_ROTATE_BYTES`], so the scan is bounded too, and it reads only the
/// four-byte length prefix of each frame rather than any sealed bytes. If
/// profiling ever says otherwise, the fix is a cached "known-good length"
/// rather than a change to what this function promises.
fn recover_valid_len(path: &Path) -> Result<u64> {
    let scan = scan_frames(path)?;
    if scan.desynced {
        return Err(Error::Invalid(format!(
            "pack at {} has a frame length prefix that cannot be a torn tail from a \
             crash -- refusing to truncate a pack that may still hold intact frames \
             past the point where the frame walk desynchronised; this pack needs \
             manual recovery before this account can append to it again",
            path.display()
        )));
    }
    if !scan.clean() {
        let trimmed = OpenOptions::new().write(true).open(path).map_err(|e| Error::io(path, e))?;
        trimmed.set_len(scan.valid_len).map_err(|e| Error::io(path, e))?;
        trimmed.sync_all().map_err(|e| Error::io(path, e))?;
    }
    Ok(scan.valid_len)
}

/// The frame offsets `mark_dead` recorded for one pack, or an empty set if
/// nothing has ever been marked dead in it.
///
/// One offset per line, in the clear: it is a byte position, not a message,
/// and reveals nothing sealing would protect.
fn read_dead_offsets(path: &Path) -> Result<BTreeSet<u64>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(e) => return Err(Error::io(path, e)),
    };
    Ok(text.lines().filter_map(|l| l.parse().ok()).collect())
}

/// A shared conformance suite, the same shape
/// [`store::conformance`](crate::store::conformance) gives every
/// [`JournalStore`](crate::store::JournalStore) implementation -- kept here
/// rather than folded into that module because [`PackStore`] answers to no
/// domain and is reached directly, not through a `JournalStore` accessor.
///
/// Covers only what every implementation can be asked to prove through the
/// trait alone: a round trip, a batch large enough to matter, and
/// compaction's two promises -- live messages survive with new addresses,
/// and a pack under the dead threshold is left untouched. What it does not
/// cover is tampering and crash recovery, because proving either means
/// reaching *underneath* the trait at bytes a table-backed implementation
/// does not expose the same way a file does -- [`FilePackStore`]'s own tests
/// cover those for the file backing, and a table-backed one is expected to
/// write its own in the same spirit, reaching into its rows the way
/// [`FilePackStore`]'s reach into its files.
///
/// The store must hold no packs for `account` on entry, and is left with
/// none on success.
#[cfg(any(test, feature = "testing"))]
pub fn run_pack_store_suite(store: &dyn PackStore, account: &str) {
    eprintln!("--- pack store conformance suite ---");

    let msgs: Vec<Vec<u8>> =
        vec![b"hello".to_vec(), b"world, a little longer".to_vec(), Vec::new()];
    let borrowed: Vec<&[u8]> = msgs.iter().map(|m| m.as_slice()).collect();
    let refs = store.append_batch(account, &borrowed).unwrap();
    assert_eq!(refs.len(), msgs.len());
    for (r, m) in refs.iter().zip(&msgs) {
        assert_eq!(&store.read(r).unwrap(), m);
    }
    store.mark_dead(&refs).unwrap();
    // Nothing left alive at all: an empty `referenced` list is a genuine
    // input here, not a placeholder, and the orphan sweep it drives is
    // exactly what reclaims this pack -- every frame in it really is dead,
    // which is what lets the empty-`referenced` guard trust the sweep. See
    // [`PackStore::compact`]'s own docs on the two-step contract this suite
    // exercises throughout.
    let hw = store.high_water_mark(account).unwrap();
    let snapshot = ReferencedSnapshot::new([], hw);
    let result = store.compact(account, &snapshot, &|| true).unwrap();
    assert!(result.remap.is_empty(), "nothing live left to remap");
    store.drop_packs(account, &result.obsolete).unwrap();

    let bodies: Vec<Vec<u8>> = (0..1000).map(|i| format!("message {i}").into_bytes()).collect();
    let borrowed: Vec<&[u8]> = bodies.iter().map(|b| b.as_slice()).collect();
    let refs = store.append_batch(account, &borrowed).unwrap();
    assert_eq!(refs.len(), 1000);
    for (r, b) in refs.iter().zip(&bodies) {
        assert_eq!(&store.read(r).unwrap(), b);
    }

    // A third or more dead triggers a rewrite; the rest survive it, at a
    // reference [`PackStore::read`] can still open, holding exactly what was
    // written. `referenced` names every pack `refs` actually lives in, so
    // the orphan sweep does not pre-empt the rewrite this step means to
    // exercise.
    let (dead, live) = refs.split_at(400);
    let live_bodies = &bodies[400..];
    store.mark_dead(dead).unwrap();
    let referenced: Vec<PackId> = refs.iter().map(|r| r.pack).collect();
    let hw = store.high_water_mark(account).unwrap();
    let snapshot = ReferencedSnapshot::new(referenced, hw);
    let result = store.compact(account, &snapshot, &|| true).unwrap();
    let remap: std::collections::HashMap<PackRef, PackRef> = result.remap.into_iter().collect();
    let mut current_refs = Vec::with_capacity(live.len());
    for (old, body) in live.iter().zip(live_bodies) {
        let r = remap.get(old).cloned().unwrap_or_else(|| old.clone());
        assert_eq!(&store.read(&r).unwrap(), body);
        current_refs.push(r);
    }
    for old in dead {
        assert!(!remap.contains_key(old), "a dead message must not be remapped as though live");
    }
    // The caller's own commit, simulated: whatever pointed at the old
    // addresses now points at the new ones, so the packs `compact` named
    // obsolete are safe to reclaim.
    store.drop_packs(account, &result.obsolete).unwrap();

    // Every remaining message is dead too, now at its post-compaction
    // address: one last sweep, nothing referenced at all, reclaims
    // everything this suite created, leaving `account` exactly as this
    // function's own docs promise.
    store.mark_dead(&current_refs).unwrap();
    let hw = store.high_water_mark(account).unwrap();
    let snapshot = ReferencedSnapshot::new([], hw);
    let result = store.compact(account, &snapshot, &|| true).unwrap();
    store.drop_packs(account, &result.obsolete).unwrap();

    eprintln!("--- pack store suite passed ---");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{AeadCipher, NullCipher, SecretKey};

    fn store(dir: &Path, encrypted: bool) -> FilePackStore {
        let cipher: Arc<dyn Cipher> = if encrypted {
            Arc::new(AeadCipher::new(&SecretKey::from_bytes([4u8; 32])))
        } else {
            Arc::new(NullCipher)
        };
        FilePackStore::open(dir, cipher).unwrap().with_rotate_bytes(TEST_ROTATE_BYTES)
    }

    /// Small enough that a test filling a pack past rotation writes a few
    /// kilobytes, not 64 MB per pack.
    const TEST_ROTATE_BYTES: u64 = 4096;

    /// A [`ReferencedSnapshot`] built the honest way for a test that is not
    /// itself trying to prove a stale-snapshot guard: `referenced`, paired
    /// with `store`'s *current* high-water mark for `account`, read fresh
    /// right now.
    fn snapshot(
        store: &FilePackStore,
        account: &str,
        referenced: Vec<PackId>,
    ) -> ReferencedSnapshot {
        ReferencedSnapshot::new(referenced, store.high_water_mark(account).unwrap())
    }

    #[test]
    fn round_trips_a_handful_of_messages() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let msgs: Vec<&[u8]> = vec![b"From: a\r\n\r\nhello", b"From: b\r\n\r\nworld", b""];
        let refs = s.append_batch("acc-1", &msgs).unwrap();
        assert_eq!(refs.len(), 3);
        for (r, msg) in refs.iter().zip(&msgs) {
            assert_eq!(s.read(r).unwrap(), *msg);
        }
    }

    #[test]
    fn a_batch_of_a_thousand_round_trips_and_shares_one_pack() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let bodies: Vec<Vec<u8>> =
            (0..1000).map(|i| format!("message number {i}").into_bytes()).collect();
        let borrowed: Vec<&[u8]> = bodies.iter().map(|b| b.as_slice()).collect();

        let refs = s.append_batch("acc-big", &borrowed).unwrap();
        assert_eq!(refs.len(), 1000);
        assert!(refs.iter().all(|r| r.pack == refs[0].pack), "one small batch fits one pack");
        for (r, body) in refs.iter().zip(&bodies) {
            assert_eq!(&s.read(r).unwrap(), body);
        }
    }

    #[test]
    fn tampering_with_a_sealed_frame_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let refs = s.append_batch("acc-1", &[b"untouched".as_slice()]).unwrap();
        let r = &refs[0];

        let path = s.account_dir(&r.account).join(format!("{}.pack", r.pack));
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&path, bytes).unwrap();

        assert_eq!(s.read(r).unwrap_err().code(), "decrypt_failed");
    }

    #[test]
    fn a_frame_swapped_with_another_in_the_same_pack_is_detected() {
        // The associated data binds a frame to its own offset, not merely to
        // the pack, so physically swapping two same-sized frames -- the one
        // move that does not corrupt the framing -- must still fail to open
        // at either position. Mirrors
        // `blobstore::tests::chunks_cannot_be_swapped_between_blobs`.
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let refs = s.append_batch("acc-1", &[b"AAAAA".as_slice(), b"BBBBB".as_slice()]).unwrap();
        assert_eq!(refs[0].len, refs[1].len, "equal-length plaintexts seal to equal lengths");

        let path = s.account_dir("acc-1").join(format!("{}.pack", refs[0].pack));
        let mut bytes = std::fs::read(&path).unwrap();
        let (a, b) = (refs[0].offset as usize, refs[1].offset as usize);
        let len = refs[0].len as usize;
        let (frame_a, frame_b) = (bytes[a..a + len].to_vec(), bytes[b..b + len].to_vec());
        bytes[a..a + len].copy_from_slice(&frame_b);
        bytes[b..b + len].copy_from_slice(&frame_a);
        std::fs::write(&path, bytes).unwrap();

        assert_eq!(s.read(&refs[0]).unwrap_err().code(), "decrypt_failed");
        assert_eq!(s.read(&refs[1]).unwrap_err().code(), "decrypt_failed");
    }

    #[test]
    fn compaction_keeps_live_messages_and_remaps_their_refs() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let msgs: Vec<&[u8]> = (0..9).map(|_| b"x".as_slice()).collect();
        let refs = s.append_batch("acc-1", &msgs).unwrap();

        // A third or more dead: mark four of nine (>= 1/3) so compaction
        // actually runs.
        let dead: Vec<PackRef> = refs[..4].to_vec();
        let live: Vec<PackRef> = refs[4..].to_vec();
        s.mark_dead(&dead).unwrap();

        let referenced: Vec<PackId> = refs.iter().map(|r| r.pack).collect();
        let snap = snapshot(&s, "acc-1", referenced);
        let result = s.compact("acc-1", &snap, &|| true).unwrap();
        assert_eq!(result.remap.len(), live.len(), "only the live messages should move");

        let remap: std::collections::HashMap<PackRef, PackRef> = result.remap.into_iter().collect();
        for old in &live {
            let new = remap.get(old).expect("every live ref should be remapped");
            assert_ne!(new.pack, old.pack, "compaction must write a fresh pack");
            assert_eq!(s.read(new).unwrap(), b"x", "the message itself must survive the move");
            // Not dropped yet: the old address must still resolve too, since
            // `compact` itself never deletes -- see its own docs.
            assert_eq!(s.read(old).unwrap(), b"x", "the old address survives until `drop_packs`");
        }

        // Only once the caller's own commit is simulated -- by calling
        // `drop_packs` on what `compact` named obsolete -- does the old
        // address stop resolving.
        s.drop_packs("acc-1", &result.obsolete).unwrap();
        assert!(s.read(&dead[0]).is_err());
    }

    #[test]
    fn a_pack_under_the_dead_threshold_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let refs: Vec<PackRef> =
            s.append_batch("acc-1", &(0..9).map(|_| b"x".as_slice()).collect::<Vec<_>>()).unwrap();

        // Two of nine is under a third: nothing should move.
        s.mark_dead(&refs[..2]).unwrap();
        let referenced: Vec<PackId> = refs.iter().map(|r| r.pack).collect();
        let snap = snapshot(&s, "acc-1", referenced);
        let result = s.compact("acc-1", &snap, &|| true).unwrap();
        assert!(result.is_empty());
        for r in &refs[2..] {
            assert_eq!(s.read(r).unwrap(), b"x");
        }
    }

    /// Regression for "pack compaction can permanently lose mail": a corrupt
    /// frame in one pack used to abort `compact` outright with `?`, throwing
    /// away the remap it had already built for every pack processed earlier
    /// in the same call -- which, combined with the old per-pack delete,
    /// could mean an already-deleted pack with no surviving remap at all.
    /// The fix makes a per-pack failure skip that pack and keep going; this
    /// pins both halves down: the failure is contained, and everything
    /// before it survives.
    #[test]
    fn an_error_on_the_second_pack_keeps_the_first_packs_messages_readable() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);

        // Pack 1: a filler big enough to force the next `append_batch` to
        // rotate, plus three small live-to-be messages sharing it.
        let big = vec![0u8; (TEST_ROTATE_BYTES as usize) + 1];
        let pack1 = s
            .append_batch(
                "acc-1",
                &[big.as_slice(), b"p1-a".as_slice(), b"p1-b".as_slice(), b"p1-c".as_slice()],
            )
            .unwrap();
        // Pack 2: a fresh pack, rotated into because pack 1 is now over the
        // size limit.
        let pack2 = s
            .append_batch("acc-1", &[b"p2-a".as_slice(), b"p2-b".as_slice(), b"p2-c".as_slice()])
            .unwrap();
        assert_ne!(pack1[0].pack, pack2[0].pack, "the filler must have forced a rotation");

        // Pack 1: mark the filler and one small message dead (2 of 4, over a
        // third) so it is eligible to compact cleanly.
        s.mark_dead(&[pack1[0].clone(), pack1[1].clone()]).unwrap();
        let pack1_live = [pack1[2].clone(), pack1[3].clone()];

        // Pack 2: mark one of three dead (over a third, so it is eligible
        // too), then corrupt one of the *live* ones so decrypting it during
        // compaction fails.
        s.mark_dead(&[pack2[0].clone()]).unwrap();
        let corrupt_path = s.account_dir("acc-1").join(format!("{}.pack", pack2[1].pack));
        let mut bytes = std::fs::read(&corrupt_path).unwrap();
        let at = pack2[1].offset as usize;
        bytes[at] ^= 0xff;
        std::fs::write(&corrupt_path, bytes).unwrap();

        let referenced: Vec<PackId> = [pack1[0].pack, pack2[0].pack].to_vec();
        let snap = snapshot(&s, "acc-1", referenced);
        let result = s.compact("acc-1", &snap, &|| true).unwrap();

        // Pack 1 compacted cleanly, and is readable at either address, on
        // the same terms `compaction_keeps_live_messages_and_remaps_their_refs`
        // checks.
        let remap: std::collections::HashMap<PackRef, PackRef> = result.remap.into_iter().collect();
        for (old, body) in pack1_live.iter().zip([b"p1-b".as_slice(), b"p1-c".as_slice()]) {
            let new = remap.get(old).expect("pack 1's live messages must have been remapped");
            assert_ne!(new.pack, pack1[0].pack);
            assert_eq!(s.read(new).unwrap(), body);
            assert_eq!(s.read(old).unwrap(), body);
        }
        assert!(result.obsolete.contains(&pack1[0].pack), "pack 1 must be marked obsolete");

        // Pack 2 was skipped whole, corrupted frame and all: nothing in it
        // moved, and its still-good messages are exactly where they always
        // were.
        assert!(!result.obsolete.contains(&pack2[0].pack), "a failed pack must not be obsoleted");
        assert_eq!(s.read(&pack2[2]).unwrap(), b"p2-c", "an untouched live message still reads");
        assert!(s.read(&pack2[1]).is_err(), "the corrupted frame is still corrupted, as before");
    }

    /// Regression for "pack compaction can permanently lose mail": a crash
    /// between `compact` returning and the caller committing its remap used
    /// to be unrecoverable, because the old pack had already been deleted by
    /// `compact` itself. The fix makes `compact` delete nothing at all --
    /// this proves that holds even across a reopen, standing in for a
    /// restarted process.
    #[test]
    fn a_crash_between_compact_and_the_ref_commit_loses_nothing_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let refs =
            s.append_batch("acc-1", &(0..9).map(|_| b"x".as_slice()).collect::<Vec<_>>()).unwrap();
        s.mark_dead(&refs[..4]).unwrap();
        let live = &refs[4..];

        let referenced: Vec<PackId> = refs.iter().map(|r| r.pack).collect();
        let snap = snapshot(&s, "acc-1", referenced);
        let result = s.compact("acc-1", &snap, &|| true).unwrap();
        assert!(!result.remap.is_empty(), "the test needs compaction to have actually run");

        // No `remap_packs` commit, and no `drop_packs` -- exactly the crash
        // window this fix closes. "Reopening" stands in for a restarted
        // process finding the vault exactly as the crash left it.
        let reopened = store(dir.path(), true);
        let remap: std::collections::HashMap<PackRef, PackRef> = result.remap.into_iter().collect();
        for old in live {
            assert_eq!(reopened.read(old).unwrap(), b"x", "the old address must still resolve");
            let new = remap.get(old).expect("every live ref should be remapped");
            assert_eq!(reopened.read(new).unwrap(), b"x", "the new address must resolve too");
        }
    }

    /// Regression for "pack compaction can permanently lose mail": the
    /// orphan sweep half of the fix. A pack `compact` made obsolete, left
    /// undeleted by a crash before `drop_packs` ran, must be reclaimed the
    /// next time `compact` runs once the caller can prove -- via
    /// `referenced` -- that nothing points at it any more.
    #[test]
    fn orphan_sweep_reclaims_a_pack_left_behind_by_an_earlier_crash() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let refs =
            s.append_batch("acc-1", &(0..9).map(|_| b"x".as_slice()).collect::<Vec<_>>()).unwrap();
        s.mark_dead(&refs[..4]).unwrap();
        let live = &refs[4..];

        let referenced: Vec<PackId> = refs.iter().map(|r| r.pack).collect();
        let snap = snapshot(&s, "acc-1", referenced);
        let first = s.compact("acc-1", &snap, &|| true).unwrap();
        let remap: std::collections::HashMap<PackRef, PackRef> = first.remap.into_iter().collect();
        // Deliberately no `drop_packs` here -- the old pack is left behind,
        // exactly as a crash after the (simulated) remap commit would leave
        // it.

        // The next compaction pass, run as though the caller has by now
        // durably committed the remap: `referenced` names only the new
        // addresses, not the old pack `first` made obsolete. The fresh
        // high-water mark now covers the replacement pack too, which is
        // also the account's newest -- guard 3 protects *it*, not the
        // genuinely leftover one this sweep must still reach.
        let new_referenced: Vec<PackId> = live.iter().map(|old| remap[old].pack).collect();
        let second_snap = snapshot(&s, "acc-1", new_referenced);
        let second = s.compact("acc-1", &second_snap, &|| true).unwrap();
        assert!(
            second.obsolete.contains(&first.obsolete[0]),
            "the leftover pack from the earlier, uncommitted-and-undropped compaction \
             must be swept as an orphan"
        );

        s.drop_packs("acc-1", &second.obsolete).unwrap();
        for old in live {
            assert!(s.read(old).is_err(), "the orphaned old address must finally be gone");
            assert_eq!(s.read(&remap[old]).unwrap(), b"x", "the live address is untouched");
        }
    }

    /// Regression for "an empty `referenced` list deletes every pack": the
    /// pre-fix behaviour swept every existing pack the moment `referenced`
    /// came back empty, with no way to tell a genuinely emptied account
    /// apart from a caller whose query simply came back wrong. A pack still
    /// holding live, never-marked-dead frames must refuse the whole sweep.
    #[test]
    fn an_empty_referenced_snapshot_over_live_packs_deletes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let refs =
            s.append_batch("acc-1", &(0..9).map(|_| b"x".as_slice()).collect::<Vec<_>>()).unwrap();
        // Nothing marked dead at all: every frame is live.

        let hw = s.high_water_mark("acc-1").unwrap();
        let snap = ReferencedSnapshot::new([], hw);
        let result = s.compact("acc-1", &snap, &|| true).unwrap();
        assert!(result.is_empty(), "an empty `referenced` list over live data must sweep nothing");
        for r in &refs {
            assert_eq!(s.read(r).unwrap(), b"x", "every message must remain exactly where it was");
        }
    }

    /// The race from `packstore`'s own module docs, simulated deterministically
    /// through the API rather than with threads: a batch is appended (its
    /// pack durably exists), a snapshot is taken *before* the rows naming
    /// that pack would commit (so `referenced` omits it, exactly as a
    /// concurrent reference query would if it ran in that window), and the
    /// sweep runs on that stale snapshot. The freshly appended pack -- the
    /// account's newest -- must survive, because `referenced` is otherwise
    /// non-empty and trustworthy, which is exactly the case guard 3 exists
    /// for.
    #[test]
    fn the_newest_pack_survives_a_sweep_taken_before_its_rows_commit() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);

        // Pre-existing, already-committed mail: a filler forces this into
        // its own pack, which the batch below will rotate away from.
        let big = vec![0u8; (TEST_ROTATE_BYTES as usize) + 1];
        let settled = s.append_batch("acc-1", &[big.as_slice(), b"settled".as_slice()]).unwrap();

        // The racy batch: durably written, but -- in this simulation --
        // its owning rows have not committed yet, so it is deliberately
        // left out of `referenced` below.
        let fresh =
            s.append_batch("acc-1", &[b"fresh-1".as_slice(), b"fresh-2".as_slice()]).unwrap();
        assert_ne!(settled[0].pack, fresh[0].pack, "the filler must have forced a rotation");

        // `referenced` names only the settled pack -- non-empty, and
        // otherwise trustworthy -- while the fresh pack's rows are
        // (simulated as) still in flight.
        let hw = s.high_water_mark("acc-1").unwrap();
        let snap = ReferencedSnapshot::new([settled[1].pack], hw);
        let result = s.compact("acc-1", &snap, &|| true).unwrap();
        assert!(
            !result.obsolete.contains(&fresh[0].pack),
            "the account's newest pack must never be swept on absence from \
             `referenced` alone"
        );

        // "Committing the rows" now: every message, old and new, is still
        // exactly where it was.
        assert_eq!(s.read(&settled[0]).unwrap(), big);
        assert_eq!(s.read(&settled[1]).unwrap(), b"settled");
        assert_eq!(s.read(&fresh[0]).unwrap(), b"fresh-1");
        assert_eq!(s.read(&fresh[1]).unwrap(), b"fresh-2");
    }

    /// Guard 1 alone: a pack created strictly after the snapshot's
    /// high-water mark must never be swept, even under an empty
    /// `referenced` list that legitimately reclaims an older, fully dead
    /// pack in the same call.
    #[test]
    fn a_pack_newer_than_the_high_water_mark_is_never_swept() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);

        // A filler pushes this first pack over the rotate threshold
        // immediately, so the next `append_batch` is guaranteed a fresh
        // pack rather than reusing this one.
        let big = vec![0u8; (TEST_ROTATE_BYTES as usize) + 1];
        let old = s.append_batch("acc-1", &[big.as_slice(), b"old".as_slice()]).unwrap();
        s.mark_dead(&old).unwrap();
        // The mark is taken here -- before the second pack below exists.
        let hw = s.high_water_mark("acc-1").unwrap();

        let newer = s.append_batch("acc-1", &[b"newer".as_slice()]).unwrap();
        assert_ne!(old[0].pack, newer[0].pack, "the filler must have forced a rotation");

        // Nothing at all is referenced, but `hw` still lets the genuinely
        // old, fully dead pack be reclaimed -- proving the guard filters by
        // age, not by blanket refusal.
        let snap = ReferencedSnapshot::new([], hw);
        let result = s.compact("acc-1", &snap, &|| true).unwrap();
        assert!(result.obsolete.contains(&old[0].pack), "the pack at or before `hw` must be swept");
        assert!(
            !result.obsolete.contains(&newer[0].pack),
            "a pack created after `hw` must never be swept, referenced or not"
        );
        assert_eq!(s.read(&newer[0]).unwrap(), b"newer");
    }

    /// A stop signal honoured between packs, never mid-rewrite: every
    /// message stays readable however far through the sweep `compact`
    /// actually got.
    #[test]
    fn the_stop_signal_mid_compaction_leaves_every_message_readable() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);

        let big = vec![0u8; (TEST_ROTATE_BYTES as usize) + 1];
        let pack1 = s
            .append_batch("acc-1", &[big.as_slice(), b"p1-a".as_slice(), b"p1-b".as_slice()])
            .unwrap();
        let pack2 = s.append_batch("acc-1", &[b"p2-a".as_slice(), b"p2-b".as_slice()]).unwrap();
        assert_ne!(pack1[0].pack, pack2[0].pack);

        // Over a third dead in each, so both are eligible to rewrite.
        s.mark_dead(&[pack1[0].clone()]).unwrap();
        s.mark_dead(&[pack2[0].clone()]).unwrap();

        let referenced: Vec<PackId> = [pack1[0].pack, pack2[0].pack].to_vec();
        let snap = snapshot(&s, "acc-1", referenced);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let should_continue = || calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
        let result = s.compact("acc-1", &snap, &should_continue).unwrap();
        assert_eq!(result.packs_rewritten, 1, "the stop signal must cut the sweep short");

        // Whichever pack was reached, every message -- rewritten or not --
        // is still readable at its old address; `compact` never deletes.
        assert_eq!(s.read(&pack1[1]).unwrap(), b"p1-a");
        assert_eq!(s.read(&pack2[1]).unwrap(), b"p2-b");
    }

    #[test]
    fn reopening_after_a_crash_mid_record_recovers_every_complete_message() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let refs = s.append_batch("acc-1", &[b"first message".as_slice()]).unwrap();

        // Simulate a crash partway through writing a second frame: the
        // length prefix landed, but the sealed bytes after it did not.
        let path = s.account_dir("acc-1").join(format!("{}.pack", refs[0].pack));
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(&999u32.to_le_bytes());
        bytes.extend_from_slice(b"only some of the next fra");
        std::fs::write(&path, &bytes).unwrap();

        // "Reopening" -- a fresh store over the same directory, as a
        // restarted process would construct.
        let reopened = store(dir.path(), true);
        assert_eq!(
            reopened.read(&refs[0]).unwrap(),
            b"first message",
            "a message written and flushed before the crash must survive it"
        );

        // Appending after the crash must not leave the torn tail sitting in
        // the middle of the file: the next batch has to land where the last
        // complete frame ended, not after the garbage.
        let more = reopened.append_batch("acc-1", &[b"after the crash".as_slice()]).unwrap();
        assert_eq!(reopened.read(&more[0]).unwrap(), b"after the crash");
        assert_eq!(reopened.read(&refs[0]).unwrap(), b"first message", "still there afterwards");

        let on_disk = std::fs::read(&path).unwrap();
        assert!(
            !on_disk.windows(25).any(|w| w == b"only some of the next fra"),
            "the torn tail must be truncated away, not merely skipped over"
        );
    }

    #[test]
    fn packs_rotate_once_they_reach_the_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let big = vec![0u8; (TEST_ROTATE_BYTES as usize) + 1];

        let first = s.append_batch("acc-1", &[big.as_slice()]).unwrap();
        let second = s.append_batch("acc-1", &[b"tiny".as_slice()]).unwrap();
        assert_ne!(first[0].pack, second[0].pack, "a full pack must not take more writes");
        assert_eq!(s.read(&second[0]).unwrap(), b"tiny");
    }

    #[test]
    fn an_empty_batch_touches_no_storage() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        assert!(s.append_batch("acc-1", &[]).unwrap().is_empty());
        assert!(!s.account_dir("acc-1").exists());
    }

    #[test]
    fn a_pack_written_under_one_key_does_not_open_under_another() {
        let dir = tempfile::tempdir().unwrap();
        let refs = store(dir.path(), true).append_batch("acc-1", &[b"private".as_slice()]).unwrap();

        let other = FilePackStore::open(
            dir.path(),
            Arc::new(AeadCipher::new(&SecretKey::from_bytes([9u8; 32]))),
        )
        .unwrap();
        assert_eq!(other.read(&refs[0]).unwrap_err().code(), "decrypt_failed");
    }

    /// Regression for the corrupted-length-prefix data-loss bug: a bit flip
    /// in one frame's length prefix, well before the pack's own physical
    /// end, must never be treated as a torn tail from a crash. Before the
    /// fix, `compaction_worthwhile` -- documented to be cheap enough to call
    /// on every sync-pass wake, with nothing it walks ever opened with the
    /// cipher -- shared the write path's destructive `recover_valid_len`, so
    /// merely *checking* whether compaction was worthwhile could truncate
    /// away every later frame in the pack, live rows and all; `compact`
    /// would then rewrite the mostly-dead-looking survivors into a fresh
    /// pack and report the original one obsolete, for a caller to drop for
    /// good.
    #[test]
    fn a_corrupted_mid_file_length_prefix_is_never_truncated_or_compacted_away() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), true);
        let msgs: Vec<&[u8]> = vec![
            b"m0".as_slice(),
            b"m1".as_slice(),
            b"m2".as_slice(),
            b"m3".as_slice(),
            b"m4".as_slice(),
        ];
        let refs = s.append_batch("acc-1", &msgs).unwrap();

        // Corrupt frame 2's length prefix -- comfortably in the middle of
        // the pack, with two more genuinely intact frames still sitting
        // after it -- into a value no real write could ever have produced.
        let path = s.account_dir("acc-1").join(format!("{}.pack", refs[2].pack));
        let prefix_at = (refs[2].offset - FRAME_PREFIX_LEN) as usize;
        let mut bytes = std::fs::read(&path).unwrap();
        let original_len = bytes.len() as u64;
        bytes[prefix_at..prefix_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();

        let referenced: Vec<PackId> = refs.iter().map(|r| r.pack).collect();
        let snap = snapshot(&s, "acc-1", referenced);

        // (i) A worthwhile-check must not truncate the pack out from under
        // the frames after the corruption, no matter what it answers.
        let _ = s.compaction_worthwhile("acc-1", &snap).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            original_len,
            "checking whether compaction is worthwhile must never shrink a pack on disk"
        );

        // (ii) Compaction itself must skip the damaged pack rather than
        // rewrite-and-drop it: nothing here is safe to call live or dead
        // from a walk that lost track partway through.
        let result = s.compact("acc-1", &snap, &|| true).unwrap();
        assert!(
            !result.obsolete.contains(&refs[2].pack),
            "a pack whose frame walk desynchronised must never be dropped"
        );
        assert!(
            result.remap.iter().all(|(old, _)| old.pack != refs[2].pack),
            "a pack whose frame walk desynchronised must never be rewritten either"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            original_len,
            "compaction must not have touched the damaged pack's bytes at all"
        );

        // Every genuinely intact frame -- including the ones sitting past
        // the corruption, which the old behaviour would have discarded --
        // must still read exactly as it always did.
        assert_eq!(s.read(&refs[0]).unwrap(), b"m0");
        assert_eq!(s.read(&refs[1]).unwrap(), b"m1");
        assert_eq!(s.read(&refs[3]).unwrap(), b"m3");
        assert_eq!(s.read(&refs[4]).unwrap(), b"m4");
    }

    #[test]
    fn passes_the_shared_conformance_suite() {
        let dir = tempfile::tempdir().unwrap();
        run_pack_store_suite(&store(dir.path(), true), "acc-conformance");
    }
}
