//! The sync passes: headers, bodies (with attachments folded in, since both
//! need the same raw bytes), and the discovery-plus-both-passes sweep that
//! serves the first sync and every steady-state wake alike.
//!
//! # Why "three passes" is two functions
//!
//! `docs/plans/mail.md` describes headers, bodies and attachments as three
//! passes. Here, attachments are extracted inside the bodies pass rather
//! than as a separate walk over the mailbox: the plan's own words say the
//! attachments pass "only extracts" from bytes "already inside the raw
//! message", and [`bodies_pass`] already has those bytes in hand, decoded,
//! for exactly as long as one message's processing takes. A third pass
//! would mean fetching -- or re-decoding from the pack store -- the same raw
//! bytes a second time for no reason this crate could find.
//!
//! # Resumability, without a numeric cursor
//!
//! Neither pass trusts a "how far did we get" counter. [`sync_headers`]
//! asks the vault which UIDs it already has (via
//! [`everyday_core::Vault::mail_uid_set`]) and only asks the server for the
//! rest; [`bodies_pass`] asks which of *those* still carry
//! [`crate::mailsync::ingest::pending_pack_ref`]'s sentinel and only fetches
//! raw bytes for them. A process killed mid-batch leaves some messages
//! ingested and some not, some bodies fetched and some not -- and the next
//! attempt's first query already excludes everything that landed, so
//! nothing is re-ingested as a duplicate and nothing already-fetched is
//! fetched twice. This is also what makes the same two functions serve the
//! first sync and a steady-state wake: a wake is simply a sync with very
//! little left to do.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use everyday_core::id::{AccountId, MailMessageId, MailboxId};
use everyday_core::mail::categorize::{self, CategorizeInput};
use everyday_core::mail::{Body, MailboxRole, Message, PartRef};
use everyday_core::packstore::{PackRef, PackStore};
use everyday_core::store::mail::IngestMessage;
use everyday_core::{MailDoc, MailSearch, Vault};
use everyday_mail::mime::Disposition;
use everyday_mail::session::{MailSession, Result as SessionResult, SyncCursor, Uid, UidSet};
use futures::StreamExt;

use crate::mailsync::discovery::{self, LabelMailboxes, SyncedMailbox};
use crate::mailsync::ingest::{self, ThreadIndex};
use crate::mailsync::status::{Phase, StatusRegistry};

/// Headers are fetched in batches of about this many UIDs -- "a few hundred"
/// per the plan, matching the size a `UID FETCH` line stays comfortably
/// short at.
const HEADER_BATCH_SIZE: usize = 500;

/// Bodies are fetched in batches of about this many messages at once.
///
/// The plan asks for batches "bounded by bytes" rather than by count; this
/// bounds by count instead, which is simpler and -- since
/// [`everyday_mail::imap::ImapSession::raw`] already sub-batches its own
/// `BODY.PEEK[]` fetches at twenty messages internally -- still keeps any
/// one round trip small. Revisit if a real mailbox's attachment-heavy
/// messages ever make a hundred of them at once too much to hold in memory
/// between the fetch and the blocking-pool pass below.
const BODY_BATCH_SIZE: usize = 100;

/// How many of the newest known uids on Gmail's All Mail
/// [`refresh_gmail_labels`]'s fallback re-checks every pass, regardless of
/// what `changes_since` reported moved. See that function's own docs.
const GMAIL_LABEL_FALLBACK_N: usize = 50;

/// What every pass needs, gathered once by [`crate::mailsync::task::run_account`]
/// so a test can build the same shape without a live connection.
pub struct SyncContext<'a> {
    pub vault: &'a Arc<Vault>,
    pub account_id: AccountId,
    pub packs: Arc<dyn PackStore>,
    pub index: Arc<dyn MailSearch>,
    pub statuses: &'a StatusRegistry,
    pub attachment_cap_bytes: Option<u64>,
    /// Paces [`bodies_pass`]'s calls to [`MailSearch::commit`] -- see
    /// [`CommitPacer`]. Built once by whoever builds the rest of this
    /// context and reused across every `sync_once` call for the life of
    /// the account task, so the cadence's five-second window is a fact
    /// about the account's sync history as a whole, not reset on every
    /// wake.
    pub index_commit: CommitPacer,
    /// Invalidated by [`sync_headers`] whenever it ingests or applies flag
    /// changes -- see `crate::mailsync::unread_cache`'s module docs. `None`
    /// is a valid answer, not a bug: a caller with nothing to invalidate
    /// (most tests in [`super::tests`]) simply never sees a stale read,
    /// because nothing here ever cached one for them either.
    pub unread_cache: Option<Arc<crate::mailsync::unread_cache::UnreadCache>>,
    /// Updated by [`sync_headers`] as it ingests, and persisted once at the
    /// end of [`sync_once`] -- see `crate::mailsync::contacts`'s module
    /// docs. `None` on the same terms `unread_cache` is.
    pub contacts: Option<Arc<crate::mailsync::contacts::ContactIndex>>,
    /// Every address this account answers to -- its own and every
    /// [`everyday_core::account::Identity`]'s, lower-cased -- what
    /// [`process_body`] hands to [`everyday_mail::invite::parse_invite`] so
    /// [`everyday_core::mail::Invite::my_response`] can be filled in without
    /// a second read of the account row for every message.
    pub identities: Vec<String>,
}

/// Paces how often [`bodies_pass`] commits the search index, per the plan's
/// own cadence: "the index... about every 2,000 docs or 5 seconds,
/// whichever comes first". `sync_once` also commits once, unconditionally,
/// at the very end of a pass, so nothing indexed is ever left uncommitted
/// for longer than the shorter of this cadence and one pass's length.
///
/// # Why paced at all, rather than "every batch" or "once at the end"
///
/// Every batch (what this replaces) is simplest, but it means a tantivy
/// `commit` -- which fsyncs a new segment -- runs once per
/// [`BODY_BATCH_SIZE`] messages: for a giant first sync, thousands of small
/// commits where a few hundred larger ones would do. Once at the end alone
/// would leave a search box unable to find a message that arrived minutes
/// ago on a big mailbox, which the plan's own "a search: 150ms for the
/// first page" budget is measured against a *searchable* index, not one
/// still waiting for a pass to finish. The 2,000/5s cadence is the balance
/// the plan already struck; this is that balance, not a new one.
///
/// Interior-mutable because [`SyncContext`] is shared, immutably, across
/// every call [`bodies_pass`] makes for however many mailboxes one pass
/// touches, and across every pass this account task ever runs.
pub struct CommitPacer(Mutex<PacerState>);

struct PacerState {
    docs_since_commit: u64,
    since: Instant,
}

/// Docs since the last commit, above which [`CommitPacer::record`] says to
/// commit regardless of how little time has passed.
const COMMIT_DOC_THRESHOLD: u64 = 2_000;
/// Time since the last commit, above which [`CommitPacer::record`] says to
/// commit regardless of how few docs have landed.
const COMMIT_TIME_THRESHOLD: Duration = Duration::from_secs(5);

impl CommitPacer {
    pub fn new() -> Self {
        Self(Mutex::new(PacerState { docs_since_commit: 0, since: Instant::now() }))
    }

    /// Note that `added` more docs were just indexed, and say whether the
    /// caller should commit now.
    fn record(&self, added: u64) -> bool {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.docs_since_commit += added;
        let due = state.docs_since_commit >= COMMIT_DOC_THRESHOLD
            || state.since.elapsed() >= COMMIT_TIME_THRESHOLD;
        if due {
            state.docs_since_commit = 0;
            state.since = Instant::now();
        }
        due
    }
}

impl Default for CommitPacer {
    fn default() -> Self {
        Self::new()
    }
}

/// Discover `ctx.account_id`'s mailboxes and run [`sync_headers`] then
/// [`bodies_pass`] over every one of them, inbox first. What one first sync
/// and one steady-state wake both are, from the caller's side.
///
/// Reaping each mailbox's vanished uids ([`reap_vanished`]) is its own
/// third phase, after every mailbox has ingested its own new headers --
/// not folded into the same loop [`sync_headers`] runs in. A message moved
/// from one mailbox to another between two passes is `vanished` in the
/// first and a `new_uids` header in the second; reaping the first
/// mailbox's copy before the second has had a chance to rematch it by
/// `Message-ID` would delete the only row [`ingest::resolve_header`] could
/// have rematched against, minting a fresh id -- and a fresh body
/// download -- for a message this account already has in full. See
/// [`sync_headers`]'s own docs.
pub async fn sync_once<S: MailSession>(
    ctx: &SyncContext<'_>,
    session: &mut S,
    labels: &mut LabelMailboxes,
    threads: &mut ThreadIndex,
) -> SessionResult<Vec<SyncedMailbox>> {
    let mut mailboxes = discovery::discover(ctx.vault, ctx.account_id, session).await?;
    let mut vanished_by_mailbox = Vec::with_capacity(mailboxes.len());
    for mailbox in &mut mailboxes {
        let vanished = sync_headers(ctx, session, mailbox, labels, threads).await?;
        vanished_by_mailbox.push(vanished);
    }
    for (mailbox, vanished) in mailboxes.iter().zip(&vanished_by_mailbox) {
        reap_vanished(ctx, mailbox, vanished);
    }
    for mailbox in &mailboxes {
        bodies_pass(ctx, session, mailbox).await?;
    }
    let _ = ctx.index.commit();
    if let Some(contacts) = &ctx.contacts {
        contacts.persist_if_dirty(ctx.vault);
    }
    Ok(mailboxes)
}

/// `SELECT` `mailbox`, diff it against what the vault already has, and
/// ingest headers for everything new -- in batches of
/// [`HEADER_BATCH_SIZE`], newest first, committing each batch as it lands.
/// See the module docs for why "committing" needs no cursor field beyond
/// what [`everyday_core::Vault::ingest`] itself already made durable.
///
/// Returns the uids `changes_since` reported vanished from `mailbox`,
/// *not yet removed* -- see [`reap_vanished`], which [`sync_once`] calls
/// once every mailbox in the round has ingested its own new headers, for
/// why deleting them here, inline, would be too early.
pub async fn sync_headers<S: MailSession>(
    ctx: &SyncContext<'_>,
    session: &mut S,
    mailbox: &mut SyncedMailbox,
    labels: &mut LabelMailboxes,
    threads: &mut ThreadIndex,
) -> SessionResult<UidSet> {
    let state = session.select(&mailbox.remote_name).await?;

    // A `UIDVALIDITY` change: forget this mailbox's membership and start
    // fresh, rematching by `Message-ID` as headers arrive rather than
    // trusting a uid that now means something else -- see
    // `crate::mailsync::ingest`'s module docs.
    //
    // `just_reset` also has to stay `true` across a crash. The naive
    // condition -- `mailbox.row.uidvalidity != 0 && ... != state.uidvalidity`
    // -- is only ever true for the one attempt that *notices* the change;
    // `reset_mailbox` durably zeroes `uidvalidity` (see its own docs), so a
    // process killed anywhere in the headers loop below leaves the row's
    // `UIDVALIDITY` at `0` on the very next attempt, which this naive
    // condition cannot tell apart from a mailbox that has simply never
    // been synced -- and a mailbox this function has never finished
    // syncing gets `force_db_rematch = false`, silently minting a fresh id
    // (and a fresh body download) for every message the crash left
    // stranded, while the original row survives, orphaned, forever (see
    // `resolve_header`'s own docs and this bug's regression test below).
    //
    // `resuming_an_interrupted_reset` closes that gap using only fields
    // this row already has: `uidnext == 0` is otherwise true only for a
    // mailbox that has never once reached this function's own completion
    // at the bottom, which is the only place that ever writes a real,
    // `IMAP`-legal `UIDNEXT` (never `0` for a mailbox that has ever been
    // `SELECT`ed). Pairing that with `uidvalidity != 0` rules out "never
    // synced" (whose `UIDVALIDITY` is still `0` too), leaving exactly "a
    // reset started, stamped its new `UIDVALIDITY` right away, and this
    // attempt never reached the bottom of this function to say it
    // finished."
    let uidvalidity_just_changed =
        mailbox.row.uidvalidity != 0 && mailbox.row.uidvalidity != state.uidvalidity;
    let resuming_an_interrupted_reset = mailbox.row.uidvalidity != 0 && mailbox.row.uidnext == 0;
    let just_reset = uidvalidity_just_changed || resuming_an_interrupted_reset;
    if just_reset {
        // Idempotent either way: a fresh reset wipes membership that is
        // there to wipe, and a resumed one wipes membership `reset_mailbox`
        // already emptied last time.
        let _ = ctx.vault.reset_mailbox(mailbox.row.id);
        mailbox.row.uidvalidity = state.uidvalidity;
        mailbox.row.uidnext = 0;
        mailbox.row.highest_modseq = 0;
        // Durable *now*, not deferred to this function's own save at the
        // bottom: this is what keeps `uidnext == 0` (and so `just_reset`)
        // true across a crash, right up until a full pass actually
        // finishes and this function's own completion below writes a real
        // `uidnext` over it.
        let _ = ctx.vault.save_mailbox(&mailbox.row);
    }
    if mailbox.row.uidvalidity == 0 {
        mailbox.row.uidvalidity = state.uidvalidity;
    }

    let cursor = SyncCursor {
        uidvalidity: mailbox.row.uidvalidity,
        // Unused by every adapter today (see `SyncCursor`'s own docs): what
        // actually decides which uids are "new" is the vault's own
        // membership, read fresh below, not this field.
        highest_uid_seen: 0,
        highestmodseq: (mailbox.row.highest_modseq != 0).then_some(mailbox.row.highest_modseq),
    };
    let known: UidSet =
        ctx.vault.mail_uid_set(mailbox.row.id).unwrap_or_default().into_iter().collect();
    let changes = session.changes_since(&cursor, &known).await?;

    // Reaping `changes.vanished` happens later, once every mailbox in this
    // round has had a chance to ingest its own new headers -- see
    // `sync_once`'s own docs for why the order matters: a message moved to
    // another mailbox between two passes is `vanished` here and `new_uids`
    // there, and deleting it *here* first would mean the mailbox that is
    // about to receive it has nothing left to rematch by `Message-ID`
    // against, minting a fresh id (and a fresh body download) for a
    // message this account already has in full.

    // Every reported flag is written back only when it actually differs
    // from what is already stored. Without `CONDSTORE` (see
    // `ImapSession::changes_since`'s non-`CONDSTORE` branch),
    // `flag_changes` is the *current* flags of every known uid, changed or
    // not, so skipping an unchanged one is what keeps a mailbox that saw no
    // real flag activity from paying for a full rewrite -- a decrypt, a
    // re-seal, an upsert and a thread recompute per message -- on every
    // single pass.
    let mut any_flag_actually_changed = false;
    for &(uid, flags, _modseq) in &changes.flag_changes {
        let new_flags = ingest::mail_flags(flags);
        let unchanged = ctx
            .vault
            .message_by_uid(mailbox.row.id, uid)
            .ok()
            .flatten()
            .is_some_and(|m| m.flags == new_flags);
        if unchanged {
            continue;
        }
        any_flag_actually_changed = true;
        let _ = ctx.vault.update_message_flags(mailbox.row.id, uid, new_flags);
    }

    // Gmail labels, on All Mail: see `refresh_gmail_labels`'s own docs for
    // why a plain flag diff never notices a message archived, or
    // relabelled, in another client, and for the two mechanisms below.
    // `gmail_labels_changed` feeds `changed` below: the fallback re-check
    // exists precisely for changes `changes_since` said nothing about, so
    // without this the unread cache would stay stale exactly when this
    // fallback is the only thing that noticed anything at all.
    let mut gmail_labels_changed = false;
    if session.capabilities().gmail && mailbox.row.role == MailboxRole::All {
        let changed_uids: UidSet = changes.flag_changes.iter().map(|&(uid, _, _)| uid).collect();
        if !changed_uids.is_empty()
            && let Ok(changed) =
                refresh_gmail_labels(ctx, session, mailbox, &changed_uids, labels).await
        {
            gmail_labels_changed |= changed;
        }

        let mut newest: Vec<Uid> = known.iter().collect();
        newest.sort_unstable_by(|a, b| b.cmp(a));
        newest.truncate(GMAIL_LABEL_FALLBACK_N);
        let fallback: UidSet = newest.into_iter().filter(|u| !changed_uids.contains(*u)).collect();
        if !fallback.is_empty()
            && let Ok(changed) =
                refresh_gmail_labels(ctx, session, mailbox, &fallback, labels).await
        {
            gmail_labels_changed |= changed;
        }
    }

    let new_uids: Vec<Uid> = changes.new_uids.iter().collect();
    let total = new_uids.len() as u64;
    let mut done = 0u64;
    ctx.statuses.set_phase(ctx.account_id, Phase::Headers, done, total);

    // The rules-based half of the split inbox -- see
    // `everyday_core::mail::categorize`. Loaded once per mailbox rather than
    // once per message: it is one small sealed row, and every message this
    // call ingests is categorised against the same corrections.
    let category_rules = ctx.vault.category_rules(ctx.account_id).unwrap_or_default();

    for batch in discovery::newest_first_chunks(new_uids, HEADER_BATCH_SIZE) {
        let uid_set: UidSet = batch.iter().copied().collect();
        let mut headers = session.headers(&uid_set).await?;
        // Newest first within the batch too, matching the plan's ordering.
        headers.sort_by(|a, b| b.uid.cmp(&a.uid).then(b.internal_date.cmp(&a.internal_date)));

        let mut ingest_batch = Vec::with_capacity(headers.len());
        for header in &headers {
            let mut resolved = match ingest::resolve_header(
                ctx.vault,
                ctx.account_id,
                header,
                threads,
                just_reset,
            ) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        uid = header.uid,
                        "could not parse a message header; skipping it"
                    );
                    continue;
                }
            };
            // The categorisation hook -- the whole of `passes.rs`'s part in
            // the split inbox. See `categorize_new_message`'s own docs for
            // what it does and, as importantly, what it deliberately leaves
            // alone.
            categorize_new_message(&mut resolved, &category_rules, ctx.contacts.as_deref());
            ingest_batch.push(IngestMessage {
                message: resolved.message.clone(),
                mailbox: mailbox.row.id,
                uid: header.uid,
            });
            // The contact index: only for a message this account has never
            // stored before, so a steady-state resync (or a `UIDVALIDITY`
            // reset's rematch) never double-counts one it has already
            // learned from. `Sent` is the plain-IMAP case; Gmail never
            // selects a folder called Sent at all (everything physically
            // lives in All Mail -- see `discovery`'s own docs), so its own
            // `\Sent` label is the same signal there.
            // Drafts are excluded outright: a Drafts folder (or, on Gmail,
            // a message carrying `\Draft`) is full of messages *from* the
            // account itself, and recording the person's own address as
            // someone they correspond with would climb their own address
            // straight up their own autocomplete.
            let is_draft = mailbox.row.role == MailboxRole::Drafts
                || header.gmail.as_ref().is_some_and(|g| g.labels.iter().any(|l| l == "\\Draft"));
            if resolved.is_new
                && !is_draft
                && let Some(contacts) = &ctx.contacts
            {
                let msg = &resolved.message;
                let is_sent = mailbox.row.role == MailboxRole::Sent
                    || header
                        .gmail
                        .as_ref()
                        .is_some_and(|g| g.labels.iter().any(|l| l == "\\Sent"));
                // Never the account's own address, on either side: a Sent
                // message's own `To`/`Cc` can still name the account itself
                // (a message someone sent to their own address on purpose),
                // and a received message can arrive `From` an alias this
                // very account also answers to.
                let is_own_address =
                    |email: &str| ctx.identities.iter().any(|i| i.eq_ignore_ascii_case(email));
                if is_sent {
                    for addr in msg.to.iter().chain(msg.cc.iter()) {
                        if !is_own_address(&addr.email) {
                            contacts.record_sent_to(&addr.email, &addr.name);
                        }
                    }
                } else if !is_own_address(&msg.from.email) {
                    contacts.record_received_from(&msg.from.email, &msg.from.name);
                }
            }
            // The Gmail folder rule: All Mail carries every physical
            // message, and its own `X-GM-LABELS` is where `\Inbox` and
            // every user label come from -- see `crate::mailsync::discovery`.
            if mailbox.row.role == MailboxRole::All
                && let Some(gmail) = &header.gmail
            {
                for label in &gmail.labels {
                    let label_row = labels.resolve(ctx.vault, label);
                    ingest_batch.push(IngestMessage {
                        message: resolved.message.clone(),
                        mailbox: label_row.id,
                        uid: header.uid,
                    });
                }
            }
        }
        if let Err(e) = ctx.vault.ingest_mail(ctx.account_id, ingest_batch) {
            tracing::warn!(error = %e, "could not ingest a batch of headers");
        }

        // `batch.len()`, not `headers.len()`: a uid the `SEARCH` at the top
        // of this function saw but that vanished (an expunge racing this
        // very sync) before the `FETCH` above ran is simply absent from
        // `headers`, and counting only what came back would leave `done`
        // permanently short of `total` -- a progress bar stuck just under
        // 100%. Every uid this batch *asked for* is accounted for either
        // way, ingested or not.
        done += batch.len() as u64;
        ctx.statuses.set_phase(ctx.account_id, Phase::Headers, done, total);
        // Yields between batches, per the plan's throttling rule, so a
        // giant first sync never starves whatever else this runtime is
        // doing between one `UID FETCH` and the next.
        tokio::task::yield_now().await;
    }

    mailbox.row.uidnext = state.uidnext;
    if let Some(modseq) = state.highestmodseq {
        mailbox.row.highest_modseq = modseq;
    }
    let _ = ctx.vault.save_mailbox(&mailbox.row);

    // Any of these can move a thread across the read/unread line -- a new
    // message, a flag another client actually changed, one this account no
    // longer has at all, or a Gmail label the fallback re-check above
    // caught that `changes_since` itself said nothing about -- so the
    // cached answer to `unread_counts` is stale the moment any of them is
    // non-empty. See `crate::mailsync::unread_cache`'s module docs. Built
    // from `any_flag_actually_changed`, not `!changes.flag_changes.is_empty()`:
    // without `CONDSTORE`, that list is every known uid's *current* flags,
    // changed or not, and invalidating on it regardless would mean the
    // cache never survives a single non-`CONDSTORE` pass.
    let changed = !changes.new_uids.is_empty()
        || any_flag_actually_changed
        || !changes.vanished.is_empty()
        || gmail_labels_changed;
    if changed && let Some(cache) = &ctx.unread_cache {
        cache.invalidate(ctx.account_id);
    }

    Ok(changes.vanished)
}

/// Actually remove every uid [`sync_headers`] reported vanished from
/// `mailbox`, once every mailbox in this round has had its own chance to
/// ingest new headers first -- see [`sync_once`]'s own docs for why the
/// order matters and [`sync_headers`]'s own docs for what deferring this
/// closes.
fn reap_vanished(ctx: &SyncContext<'_>, mailbox: &SyncedMailbox, vanished: &UidSet) {
    if vanished.is_empty() {
        return;
    }
    let uids: Vec<Uid> = vanished.iter().collect();
    if let Ok(removed) = ctx.vault.remove_mail_uids(mailbox.row.id, &uids) {
        reap_dead_messages(ctx, &removed);
    }
}

/// The whole of `passes.rs`'s part in the split inbox: fill in `resolved`'s
/// category if, and only if, it does not have one yet.
///
/// "Does not have one yet" is deliberately the entire condition. A message
/// arriving for the first time has `category: None` (see
/// [`ingest::resolve_header`]), so this is exactly the fresh-message case
/// the plan's own words ask for -- "hook it into ingest so every new thread
/// gets a category." A message that has *already* been categorised --
/// re-ingested here because a second Gmail label named it, or resolved from
/// [`ThreadIndex`]'s own memory after a `UIDVALIDITY` reset -- keeps
/// whatever it already had, which is what stops this from quietly
/// overwriting a model's answer or a person's own correction (both of which
/// write through [`everyday_core::Vault::set_mail_message_category`] and
/// [`everyday_core::Vault::correct_mail_category`] respectively, never
/// through this path) the moment the same message is seen under a second
/// mailbox.
///
/// Always on, and sends nothing anywhere -- see
/// `everyday_core::mail::categorize`'s own module docs for the whole of the
/// rules this runs.
fn categorize_new_message(
    resolved: &mut ingest::HeaderIngest,
    rules: &everyday_core::mail::CategoryRules,
    contacts: Option<&crate::mailsync::contacts::ContactIndex>,
) {
    if resolved.message.category.is_some() {
        return;
    }
    let input = CategorizeInput {
        from: &resolved.message.from.email,
        list_id: resolved.list_id.as_deref(),
        list_unsubscribe: resolved.list_unsubscribe.as_deref(),
        precedence: resolved.precedence.as_deref(),
        auto_submitted: resolved.auto_submitted.as_deref(),
        gmail_labels: &resolved.message.labels,
        ever_written_to: contacts.is_some_and(|c| c.has_sent_to(&resolved.message.from.email)),
    };
    resolved.message.category = Some(categorize::categorize(&input, rules));
    resolved.message.category_source = everyday_core::mail::CategorySource::Rules;
}

/// Diff and apply Gmail's own label set for `uids` in `mailbox`, which must
/// be All Mail on a Gmail account.
///
/// # Why All Mail needs this and a plain flag diff is not enough
///
/// `changes_since`'s `flag_changes` promises to report a change to the five
/// IMAP flags this crate tracks (`\Seen`, `\Answered`, `\Flagged`,
/// `\Draft`, `\Deleted`) -- see [`crate::mailsync::ingest::mail_flags`]. A
/// Gmail label, `\Inbox` most of all, is not one of those flags; losing it
/// -- a message archived in another client -- changes nothing
/// `changes_since` was ever asked to diff. Gmail does bump a message's own
/// `MODSEQ` on a label change the same way it does on a flag change, which
/// is why `changes_since`'s own CONDSTORE diff already tells this pass
/// *which* uids moved (in `flag_changes`, whether their IMAP flags actually
/// differed or not) -- what this function adds is asking those uids what
/// their labels are now, which `changes_since` has no way to answer on its
/// own.
///
/// # The fallback, and why one exists at all
///
/// The plan's own risk table does not promise every server -- or a future
/// Gmail change -- keeps bumping `MODSEQ` for a label-only change forever.
/// [`sync_headers`] also calls this, unconditionally, for
/// [`GMAIL_LABEL_FALLBACK_N`] of the newest known uids on every pass,
/// whether or not `changes_since` reported anything about them -- a small,
/// bounded re-check that catches a label change CONDSTORE ever missed
/// within, at worst, a few poll cycles. "Newest" is approximated by uid,
/// which is monotonically non-decreasing with arrival on every server this
/// crate has met; a real per-message date would need a decrypt this pass
/// is specifically trying to avoid paying for on every wake.
///
/// # Why this touches mailbox membership, not only `Message::labels`
///
/// `Message::labels` is the informational copy of a message's label set;
/// what a mailbox's own list actually pages over is `message_mailboxes` --
/// one row per label, minted the first time a header ingest sees it (see
/// [`LabelMailboxes`] and `sync_headers`'s own header-ingest loop, which
/// this mirrors). Calling only [`everyday_core::Vault::update_message_labels`]
/// would update the field a search or a tool reads without moving the
/// message out of the Inbox label's own list -- exactly the symptom this
/// function exists to fix, so it diffs the old label set against the new
/// one and adds or removes the matching `message_mailboxes` row for each
/// side of the difference.
///
/// Returns whether anything actually changed for any of `uids` -- what the
/// two call sites in [`sync_headers`] OR together into `gmail_labels_changed`
/// for the unread-cache invalidation at the bottom of that function.
async fn refresh_gmail_labels<S: MailSession>(
    ctx: &SyncContext<'_>,
    session: &mut S,
    mailbox: &SyncedMailbox,
    uids: &UidSet,
    labels: &mut LabelMailboxes,
) -> SessionResult<bool> {
    let headers = session.headers(uids).await?;
    let mut changed_anything = false;
    for header in &headers {
        let Some(gmail) = &header.gmail else { continue };
        let Ok(Some(current)) = ctx.vault.message_by_uid(mailbox.row.id, header.uid) else {
            continue;
        };
        let old: std::collections::BTreeSet<&str> =
            current.labels.iter().map(String::as_str).collect();
        let new: std::collections::BTreeSet<&str> =
            gmail.labels.iter().map(String::as_str).collect();
        if old == new {
            continue;
        }
        // Computed as owned strings, up front, rather than kept as the
        // `old`/`new` borrows of `current.labels`/`gmail.labels` above:
        // `current` is about to be given its own corrected `labels` below,
        // and the borrow checker will not allow that while `old` is still
        // in scope borrowing the very field being replaced.
        let removed_labels: Vec<String> = old.difference(&new).map(|s| s.to_string()).collect();
        let added_labels: Vec<String> = new.difference(&old).map(|s| s.to_string()).collect();
        changed_anything = true;

        let _ = ctx.vault.update_message_labels(mailbox.row.id, header.uid, gmail.labels.clone());
        // The row this pass re-ingests under each *added* label must carry
        // the label set `update_message_labels` just stored, not the one
        // `current` was read with -- otherwise the ingest below overwrites
        // the very write just above with the stale copy, reverting the
        // label it was supposed to add and leaving this pass to repeat the
        // same "change" forever. See this function's own regression test.
        let mut current = current;
        current.labels = gmail.labels.clone();

        for removed_label in &removed_labels {
            let label_row = labels.resolve(ctx.vault, removed_label);
            if let Ok(removed) = ctx.vault.remove_mail_uids(label_row.id, &[header.uid]) {
                reap_dead_messages(ctx, &removed);
            }
        }
        for added_label in &added_labels {
            let label_row = labels.resolve(ctx.vault, added_label);
            let _ = ctx.vault.ingest_mail(
                ctx.account_id,
                vec![IngestMessage {
                    message: current.clone(),
                    mailbox: label_row.id,
                    uid: header.uid,
                }],
            );
        }
    }
    Ok(changed_anything)
}

/// What [`reap_vanished`] and [`refresh_gmail_labels`] both call once
/// [`everyday_core::Vault::remove_mail_uids`] has told them which messages
/// just became genuinely dead -- no mailbox names them any more, not merely
/// the one this pass just touched (see that method's own docs). Marks each
/// one's pack frame dead, so [`PackStore::compact`] can reclaim it later,
/// and drops it from the search index.
///
/// Does not commit the index itself: [`sync_once`] already commits it
/// unconditionally once at the end of every pass (see that function's own
/// docs), which is what makes this deletion visible to a search without a
/// second commit here.
///
/// Best-effort, on the same terms every other write in these two passes
/// already is (see the `let _ =` beside every other vault call around this
/// one): a pack or index update that fails leaves an orphaned frame or a
/// stale hit behind, not a wrong answer to any read the vault's own
/// rows -- already updated by the `remove_mail_uids` call this follows --
/// disagree with.
fn reap_dead_messages(ctx: &SyncContext<'_>, removed: &[(MailMessageId, PackRef)]) {
    if removed.is_empty() {
        return;
    }
    let refs: Vec<PackRef> = removed.iter().map(|(_, r)| r.clone()).collect();
    if let Err(e) = ctx.packs.mark_dead(&refs) {
        tracing::warn!(error = %e, "could not mark a removed message's pack frame dead");
    }
    let keys: Vec<String> = removed.iter().map(|(id, _)| id.to_string()).collect();
    if let Err(e) = ctx.index.delete(&keys) {
        tracing::warn!(error = %e, "could not remove a message from the search index");
    }
}

/// Fetch raw bytes for every message in `mailbox` still carrying
/// [`ingest::pending_pack_ref`]'s sentinel, and turn each into a sealed pack
/// entry, a sanitised [`Body`] and a searchable [`MailDoc`].
///
/// The CPU-heavy half of each message -- MIME parsing, HTML sanitising, the
/// quoted/signature heuristics, attachment extraction -- runs on the
/// blocking pool, one batch at a time, so a first sync never runs that work
/// on the same task a command handler needs to make progress. Bounded by
/// [`BODY_BATCH_SIZE`] rather than a semaphore of its own: batches are
/// already processed one at a time, so the "bounded parallelism" the plan
/// asks for is, today, bounded at one batch in flight.
pub async fn bodies_pass<S: MailSession>(
    ctx: &SyncContext<'_>,
    session: &mut S,
    mailbox: &SyncedMailbox,
) -> SessionResult<()> {
    let pending = pending_messages(ctx.vault, mailbox.row.id);
    if pending.is_empty() {
        return Ok(());
    }
    // `sync_headers`, run for every mailbox just before this pass starts,
    // leaves the session `SELECT`ed on whichever mailbox it looked at last
    // -- almost never this one. `raw` fetches by uid within the *selected*
    // mailbox, so this has to point the session back here first.
    session.select(&mailbox.remote_name).await?;
    let total = pending.len() as u64;
    let mut done = 0u64;
    ctx.statuses.set_phase(ctx.account_id, Phase::Bodies, done, total);

    for batch in pending.chunks(BODY_BATCH_SIZE) {
        let uid_set: UidSet = batch.iter().map(|(_, uid)| *uid).collect();
        let mut raw_by_uid: HashMap<Uid, Vec<u8>> = HashMap::new();
        {
            let mut stream = session.raw(&uid_set).await?;
            while let Some(item) = stream.next().await {
                let (uid, bytes) = item?;
                raw_by_uid.insert(uid, bytes);
            }
        }

        let vault = ctx.vault.clone();
        let packs = ctx.packs.clone();
        let account_id = ctx.account_id;
        let mailbox_id = mailbox.row.id;
        let attachment_cap = ctx.attachment_cap_bytes;
        let identities = ctx.identities.clone();
        let batch_owned: Vec<(Message, Uid)> = batch.to_vec();
        let docs = tokio::task::spawn_blocking(move || {
            // Every message this batch actually got raw bytes for, in
            // order -- sealed in *one* `append_batch` call below rather
            // than one per message, which is the whole reason this batch
            // exists: `PackStore::append_batch` flushes once per call, so
            // a hundred separate calls here would cost a hundred `fsync`s
            // for what one batch of a hundred messages needs only one of.
            let present: Vec<(Message, Uid, &Vec<u8>)> = batch_owned
                .iter()
                .filter_map(|(message, uid)| {
                    raw_by_uid.get(uid).map(|raw| (message.clone(), *uid, raw))
                })
                .collect();
            let raws: Vec<&[u8]> = present.iter().map(|(_, _, raw)| raw.as_slice()).collect();
            let Ok(sealed) = packs.append_batch(&account_id.to_string(), &raws) else {
                return Vec::new();
            };

            let mut processed = Vec::with_capacity(present.len());
            for ((message, uid, raw), pack) in present.into_iter().zip(sealed) {
                processed.push(process_body(
                    vault.as_ref(),
                    uid,
                    message,
                    raw,
                    pack,
                    attachment_cap,
                    &identities,
                ));
            }
            // `message` in `processed` is the snapshot `pending_messages`
            // took *before* the network fetch above -- for a big first
            // sync, potentially long before it, since this batch may have
            // sat behind others. Anything that landed locally in the
            // meantime (a person reading the message, the model setting
            // its category) is not on it. Re-reading each row fresh here
            // and bringing forward only the four fields this pass actually
            // owns -- `pack`, `snippet`, `has_attachments`, `invite` --
            // rather than upserting the stale snapshot whole, is what
            // stops that write from reverting `flags`, `labels`,
            // `category` and `category_source` back to whatever the
            // headers pass originally saw. See this function's own
            // regression test.
            let refreshed: Vec<(IngestMessage, MailDoc)> = processed
                .iter()
                .map(|(message, uid, body)| {
                    let mut fresh =
                        vault.mail_message(message.id).unwrap_or_else(|_| message.clone());
                    fresh.pack = message.pack.clone();
                    fresh.snippet = message.snippet.clone();
                    fresh.has_attachments = message.has_attachments;
                    fresh.invite = message.invite.clone();
                    let doc = mail_doc(account_id, mailbox_id, &fresh, &body.model_text());
                    (IngestMessage { message: fresh, mailbox: mailbox_id, uid: *uid }, doc)
                })
                .collect();
            // One upsert for the whole batch rather than one per message --
            // `everyday-store-sql`'s own `upsert_batched` is what keeps this
            // off the vault's single writer lock for the length of a giant
            // first sync, the same reasoning `MailStore::ingest`'s own docs
            // give for accepting a `Vec` rather than one message at a time.
            let (ingests, docs): (Vec<IngestMessage>, Vec<MailDoc>) = refreshed.into_iter().unzip();
            let _ = vault.ingest_mail(account_id, ingests);
            let bodies: Vec<Body> = processed.iter().map(|(_, _, body)| body.clone()).collect();
            let _ = vault.save_bodies(&bodies);
            docs
        })
        .await
        .unwrap_or_default();

        let indexed = docs.len() as u64;
        if !docs.is_empty() {
            let _ = ctx.index.index(&docs);
            // Paced rather than committed every batch: about every 2,000
            // docs or every five seconds, whichever comes first, per the
            // plan's own cadence -- see `CommitPacer`. `sync_once` commits
            // once more, unconditionally, at the end of the whole pass, so
            // nothing indexed here is ever left uncommitted for longer than
            // that.
            if ctx.index_commit.record(indexed) {
                let _ = ctx.index.commit();
            }
        }

        done += batch.len() as u64;
        ctx.statuses.set_phase(ctx.account_id, Phase::Bodies, done, total);
        tokio::task::yield_now().await;
    }

    Ok(())
}

/// Every message in `mailbox` whose pack is still
/// [`ingest::pending_pack_ref`]'s sentinel, newest first.
///
/// Reads [`Vault::mail_pending_bodies`] -- a query over `mail_messages`'s
/// clear `pack_len` column -- rather than the shape this used to be: every
/// uid in the mailbox, decrypted one at a time, just to ask each one
/// whether it was still pending. A mailbox that is mostly *not* pending
/// (the ordinary steady state, once a first sync has finished) used to pay
/// for a full decrypt of every message in it on every single pass; this
/// pays only for the ones actually still waiting.
fn pending_messages(vault: &Vault, mailbox_id: MailboxId) -> Vec<(Message, Uid)> {
    // No mailbox has anywhere near this many messages still pending at
    // once in practice -- a first sync's headers pass alone is chunked at
    // `HEADER_BATCH_SIZE` -- so one call already gets everything this pass
    // needs; `limit` exists on the trait for a caller that wants to bound
    // one query's cost more tightly than "effectively unlimited".
    const PENDING_QUERY_LIMIT: u32 = 1_000_000;
    vault.mail_pending_bodies(mailbox_id, PENDING_QUERY_LIMIT).unwrap_or_default()
}

/// One message's raw bytes, already sealed by [`bodies_pass`]'s single
/// batched [`PackStore::append_batch`] call, turned into a sanitised body --
/// run on the blocking pool, which collects the results of a whole batch
/// before writing any of it, so a giant first sync costs one upsert per
/// batch rather than one per message. Infallible: a message that fails to
/// *parse* still gets an (empty) body rather than being skipped, since its
/// raw bytes are sealed either way and a blank preview is a smaller failure
/// than refetching for ever.
fn process_body(
    vault: &Vault,
    uid: Uid,
    mut message: Message,
    raw: &[u8],
    pack: everyday_core::packstore::PackRef,
    attachment_cap_bytes: Option<u64>,
    identities: &[String],
) -> (Message, Uid, Body) {
    message.pack = pack;

    let mut remote_images = Vec::new();
    // Phase 6's "invitations in mail": when this message carries a
    // `text/calendar` part, parse it into the banner a thread draws above
    // the message. Deliberately the smallest addition this match can carry
    // -- everything else in this function is unchanged from before phase 6.
    let mut invite = None;
    let (html_sanitised, plain, parts, has_attachments) = match everyday_mail::mime::parse(raw) {
        Ok(parsed) => {
            invite = parsed
                .calendar
                .as_deref()
                .and_then(|calendar| everyday_mail::invite::parse_invite(calendar, identities));
            let html_sanitised = parsed.html.as_deref().map_or_else(String::new, |html| {
                let rewrite = everyday_mail::sanitize::Rewrite::new(message.id.to_string());
                let sanitised = everyday_mail::sanitize::sanitize(html, &rewrite);
                // What `mailview::remote_image` looks a token up in: without
                // it, every image a message names would answer "no such
                // remote image" even once its sender is trusted.
                remote_images = sanitised
                    .remote_images
                    .into_iter()
                    .map(|r| everyday_core::mail::RemoteImage {
                        original_url: r.original_url,
                        token: r.token,
                        cached_blob: None,
                    })
                    .collect();
                sanitised.html
            });
            // The HTML part wins wherever there is one, even when the
            // message also carried a `text/plain` alternative -- and it is
            // read through `html_to_text` rather than taken as it stands.
            //
            // # Why the plain part is not simply preferred
            //
            // Whatever ends up here is what every reader that is not a
            // person sees: the snippet, the search index, and -- through
            // `Body::text` -- the assistant and MCP. A person sees
            // `html_sanitised` in the frame. If those two come from
            // different parts of the same message, a sender chooses what
            // each of them reads, which is the whole of the attack: ship a
            // benign `text/html` part and a hostile `text/plain` one, and
            // the instruction the person can never see is the only thing
            // the model is given. Deriving both from the same part closes
            // that, and `html_to_text` is what makes the derived text agree
            // with what the frame actually shows -- it drops
            // `display:none`, white-on-white and commented-out text, none
            // of which `sanitize` removes, because hiding text is not by
            // itself an XSS risk and the frame is entitled to render a
            // sender's own styling.
            //
            // The cost is that a carefully written `text/plain` alternative
            // is passed over for a machine rendering of the HTML beside it.
            // That is the right trade: the plain part is only better when
            // the sender is honest, and it is precisely a dishonest sender
            // this has to hold against.
            let plain = match parsed.html.as_deref() {
                Some(html) => everyday_mail::text::html_to_text(html),
                None => parsed.text.clone().unwrap_or_default(),
            };

            let mut parts = Vec::with_capacity(parsed.parts.len());
            let mut has_attachments = false;
            for part in &parsed.parts {
                if part.disposition == Disposition::Attachment {
                    has_attachments = true;
                }
                let within_cap = attachment_cap_bytes.is_none_or(|cap| (part.size as u64) <= cap);
                let blob = if within_cap {
                    everyday_mail::mime::part_bytes(raw, &part.part_id)
                        .ok()
                        .and_then(|bytes| vault.put_blob(&bytes).ok())
                } else {
                    None
                };
                parts.push(PartRef {
                    cid: part.content_id.clone(),
                    filename: part.filename.clone(),
                    mime_type: part.content_type.clone(),
                    size: part.size as u64,
                    blob,
                });
            }
            (html_sanitised, plain, parts, has_attachments)
        }
        Err(e) => {
            tracing::warn!(error = %e, message = %message.id, "could not parse a message body");
            (String::new(), String::new(), Vec::new(), false)
        }
    };

    let quoted_ranges: Vec<(u32, u32)> = everyday_mail::text::quoted_ranges(&plain)
        .into_iter()
        .map(|r| (r.start as u32, r.end as u32))
        .collect();
    let signature_range =
        everyday_mail::text::signature_range(&plain).map(|r| (r.start as u32, r.end as u32));

    message.snippet = everyday_mail::text::snippet(&plain);
    message.has_attachments = has_attachments;
    message.invite = invite;

    let body = Body {
        message_id: message.id,
        html_sanitised,
        text: plain,
        quoted_ranges,
        signature_range,
        parts,
        remote_images,
    };

    (message, uid, body)
}

/// A [`MailDoc`] for `message`, filed under `mailbox_id`. Shared by
/// [`process_body`] (which has just computed `body_text` itself) and
/// `crate::domains::mailsync::rebuild_mail_index` (which reads it back out
/// of an already-sealed [`Body`] via [`Body::model_text`]).
pub(crate) fn mail_doc(
    account_id: AccountId,
    mailbox_id: MailboxId,
    message: &Message,
    body_text: &str,
) -> MailDoc {
    MailDoc {
        message_key: message.id.to_string(),
        thread_key: message.thread_id.to_string(),
        account: account_id.to_string(),
        mailboxes: vec![mailbox_id.to_string()],
        from: display_address(&message.from),
        to: message.to.iter().map(display_address).collect::<Vec<_>>().join(", "),
        cc: message.cc.iter().map(display_address).collect::<Vec<_>>().join(", "),
        subject: message.subject.clone(),
        body_text: body_text.to_string(),
        labels: message.labels.clone(),
        date: message.date,
        has_attachment: message.has_attachments,
        unread: message.flags.unread(),
        starred: message.flags.flagged,
    }
}

fn display_address(a: &everyday_core::mail::Address) -> String {
    if a.name.is_empty() { a.email.clone() } else { format!("{} <{}>", a.name, a.email) }
}

#[cfg(test)]
mod pacer_tests {
    use super::*;

    #[test]
    fn commits_once_the_doc_threshold_is_reached() {
        let pacer = CommitPacer::new();
        assert!(!pacer.record(COMMIT_DOC_THRESHOLD - 1), "not due yet");
        assert!(pacer.record(1), "crossing the threshold is due");
        // The counter resets: a further single doc is not due again
        // immediately.
        assert!(!pacer.record(1));
    }

    #[test]
    fn commits_once_the_time_threshold_is_reached() {
        let pacer = CommitPacer::new();
        {
            let mut state = pacer.0.lock().unwrap();
            state.since = Instant::now() - COMMIT_TIME_THRESHOLD - Duration::from_millis(1);
        }
        assert!(pacer.record(1), "old enough to be due even with almost no docs");
    }

    #[test]
    fn a_single_doc_well_within_both_thresholds_is_not_due() {
        let pacer = CommitPacer::new();
        assert!(!pacer.record(1));
    }
}
