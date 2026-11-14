//! The outbox executor: turning one durable [`Op`] into the IMAP and SMTP
//! calls that make it true on the server.
//!
//! `docs/plans/mail.md`'s "What an action does" draws the line this module
//! sits on exactly: the local write already happened, in the vault, before
//! this is ever called — [`execute`] has one job, which is to make the
//! server agree with it. It is deliberately pure over traits, the same
//! discipline [`crate::session::MailSession`] itself keeps: nothing here
//! reaches into a vault, seals a byte, or knows what an account record looks
//! like. What it needs beyond a live [`MailSession`] and something that can
//! send mail is a small, caller-supplied [`Lookups`] — the shape the plan's
//! own phase 3 section names: *"resolving a thread or message to (mailbox
//! name, uid) pairs, the account's special-use mailbox names, the Gmail
//! flag, and loading a draft plus the parent message and its raw bytes for
//! replies."* [`crates::service::outbox::drain_outbox`](../../karmayoga_service/outbox/index.html)
//! is what actually implements `Lookups` against a real vault and calls this
//! from the sync agent's per-account task; this module never has to know
//! that crate exists.
//!
//! # Gmail's folder rule, and what it means for `Archive` and `Label`
//!
//! [`crate::session`]'s own docs already settle this for reading: on an
//! account with `X-GM-EXT-1`, a mailbox is a label, not a folder, so there
//! is nothing to `MOVE` a message into or out of. The same rule governs
//! writing. [`Archive`](karmayoga_core::mail::OpKind::Archive) removes the
//! `\Inbox` label via [`MailSession::store_gmail_labels`] on Gmail, and
//! `MOVE`s into the account's Archive mailbox everywhere else.
//! [`Label`](karmayoga_core::mail::OpKind::Label) and
//! [`Unlabel`](karmayoga_core::mail::OpKind::Unlabel) go through the same
//! call unconditionally, because — per [`crate::session::Flags`]'s own
//! module docs — a Gmail label and an IMAP custom keyword are different
//! things, and this crate's data model only ever reads the former; a
//! `Label` op against a non-Gmail account is refused with
//! [`MailError::Unsupported`], which the drain loop treats as the permanent
//! failure it is.
//!
//! # Errors: [`is_retryable`] is the whole story
//!
//! [`MailError`]'s own docs already say which of its four shapes means "try
//! again" — only [`MailError::Network`] — and which mean the request itself,
//! or the credential behind it, needs a person's attention. [`is_retryable`]
//! is that one-line reading, named so the drain loop never has to
//! re-derive it from the variant names by hand.

use karmayoga_core::id::{BlobId, DraftId, MailMessageId, MailboxId, ThreadId};
use karmayoga_core::mail::{
    Address as CoreAddress, Draft, DraftState, MailboxRole, Op, OpKind, OpTarget,
};

use crate::compose::{self, Built, Outgoing, OutgoingAttachment};
use crate::mime;
use crate::session::{Flags, MailError, MailSession, Result, Uid, UidSet};
use crate::smtp::SendReceipt;

/// Where one message currently lives on the server: the mailbox's own
/// name — verbatim, ready to hand to [`MailSession::select`] — and its uid
/// inside that mailbox.
///
/// A message can have more than one of these at once: a Gmail message
/// filed under two labels is one message, in `All Mail`, with two rows in
/// `message_mailboxes`, hence two [`Located`]s; a non-Gmail message
/// ordinarily has exactly one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub mailbox: String,
    pub uid: Uid,
}

/// Everything [`execute`] needs to know that is not "how do I speak IMAP" —
/// the caller's own bookkeeping, handed in as a trait so a test can fake it
/// and so this crate never has to import a vault to run. One implementation,
/// `crates/karmayoga-service/src/outbox.rs`'s, backs it with a real
/// [`karmayoga_core::Vault`]; this crate only ever sees the trait.
pub trait Lookups {
    /// Every `(mailbox, uid)` any message of `thread` is currently filed
    /// under. What [`OpKind::MarkRead`] and its four flag-changing siblings,
    /// [`OpKind::Archive`] and [`OpKind::Trash`] all resolve a
    /// thread-targeted op against.
    fn thread_locations(&self, thread: ThreadId) -> Result<Vec<Located>>;

    /// As [`Lookups::thread_locations`], for a single message — what an
    /// [`OpTarget::Message`] op resolves against.
    fn message_locations(&self, message: MailMessageId) -> Result<Vec<Located>>;

    /// The remote name of this account's mailbox for `role`, or `None` when
    /// the account was never seen to have one (a provider with no Archive
    /// folder, say). [`OpKind::Archive`] and [`OpKind::Trash`] both read
    /// this on a non-Gmail account; [`OpKind::Send`] and
    /// [`OpKind::AppendDraft`] read it for `Sent` and `Drafts`.
    fn special_use(&self, role: MailboxRole) -> Result<Option<String>>;

    /// The remote name of an arbitrary mailbox by its vault id — what
    /// [`OpKind::Move`] resolves its destination against, since a move can
    /// target any folder the account has, not only a special-use one.
    fn mailbox_name(&self, mailbox: MailboxId) -> Result<String>;

    /// What an arbitrary mailbox is for, or `None` once the vault no longer
    /// has it -- what a delete forever checks again when it runs, since it
    /// can wait days to: the mailbox it names must still be a Trash or Spam.
    fn mailbox_role(&self, mailbox: MailboxId) -> Result<Option<MailboxRole>>;

    /// Is this account Gmail — [`crate::session::Capabilities::gmail`],
    /// already known to the caller from the live session's own connect-time
    /// handshake. Handed in separately from the session because
    /// [`execute`] takes `&Op`, not the session, at the point it decides
    /// which branch of [`OpKind::Archive`] to run, and asking the session
    /// for its own capabilities mid-call would work just as well but would
    /// mean every fake in this module's tests had to model a live
    /// connection's capabilities rather than just answering a bool.
    fn is_gmail(&self) -> bool;

    /// The draft an [`OpKind::Send`] or [`OpKind::AppendDraft`] op names.
    fn draft(&self, id: DraftId) -> Result<Draft>;

    /// Persist the `Message-ID` [`send`] just minted for this draft's first
    /// send attempt, before it actually sends -- see that function's own
    /// docs on why this write happens ahead of the send rather than after
    /// it.
    fn set_draft_message_id(&self, id: DraftId, message_id: &str) -> Result<()>;

    /// Where this crate's own previous `APPEND` of `id` to Drafts landed,
    /// if there has been one — what [`OpKind::AppendDraft`] deletes before
    /// writing the fresh copy, so editing a draft ten times does not leave
    /// nine stale copies sitting in the account's Drafts folder.
    fn draft_server_copy(&self, id: DraftId) -> Result<Option<Located>>;

    /// The raw RFC 5322 bytes of the message `id` names — [`OpKind::Send`]
    /// and [`OpKind::AppendDraft`] read this only when the draft being sent
    /// or saved is a reply (`Draft::in_reply_to` is `Some`), to thread
    /// `In-Reply-To` and `References` through
    /// [`compose::reply_headers`].
    fn parent_raw(&self, id: MailMessageId) -> Result<Vec<u8>>;

    /// One attachment's raw bytes, by the [`BlobId`] a [`Draft::attachments`]
    /// entry names — the filename and MIME type travel on the entry itself
    /// (see [`karmayoga_core::mail::DraftAttachment`]), so this is the one
    /// thing about it a caller still has to fetch.
    fn attachment_bytes(&self, blob: BlobId) -> Result<Vec<u8>>;

    /// The domain half of the `Message-ID` [`compose::build`] mints for a
    /// freshly sent or appended message — see that function's own docs for
    /// why this crate never trusts a hostname for it.
    fn message_id_domain(&self) -> String;

    /// The name to put beside `address` in `From:` -- the account's, or the
    /// identity's that address belongs to. `None` sends the bare address.
    fn sender_name(&self, address: &str) -> Option<String> {
        let _ = address;
        None
    }

    /// The address a proxied remote image in a stored body originally
    /// pointed at -- `token` from that message's own `karmayoga://mail/img/`
    /// address. What [`compose::externalize_local_refs`] puts back into a
    /// quoted reply on its way out. `None` when it is not known, which
    /// drops the image; the default answers that, so a lookup with no store
    /// behind it sends a quote with its images left out rather than broken.
    fn remote_image_url(&self, _message: MailMessageId, _token: &str) -> Result<Option<String>> {
        Ok(None)
    }

    /// The `Message-ID` of this draft's last copy on the server, when it is
    /// known -- what finds that copy again in Gmail's Trash after
    /// [`delete_located`] has moved it there, to delete it for good.
    fn draft_server_copy_message_id(&self, _id: DraftId) -> Result<Option<String>> {
        Ok(None)
    }

    /// Message `id`'s own `Message-ID` header -- the same use, for a draft
    /// deleted from its thread ([`OpKind::DeleteForever`] from Drafts).
    fn message_id_header(&self, _id: MailMessageId) -> Result<Option<String>> {
        Ok(None)
    }
}

/// A place to send built mail, behind a trait for exactly the reason
/// [`MailSession`] is one: so a test can fake "accepted", "rejected" or
/// "the network is down" without opening a socket.
/// [`crate::smtp::SmtpTransport`] is the real implementation, blanket-impl'd
/// below.
///
/// `async fn` in a trait, on the same reasoning [`MailSession`]'s own docs
/// give: this crate calls it generically, against one concrete type chosen
/// once by the caller, never behind `dyn`.
#[allow(async_fn_in_trait)]
pub trait Sender {
    async fn send(&self, built: &Built) -> Result<SendReceipt>;
}

impl Sender for crate::smtp::SmtpTransport {
    async fn send(&self, built: &Built) -> Result<SendReceipt> {
        // Fully qualified rather than `self.send(built)`: an inherent method
        // of the same name on `SmtpTransport` itself would otherwise shadow
        // this trait method at the call site below and recurse forever, so
        // this spells out which one is meant rather than relying on method
        // resolution to keep picking the right one across an edit.
        crate::smtp::SmtpTransport::send(self, built).await
    }
}

/// Live connections and lookups for one [`execute`] call, or a short run of
/// them — one per account, held by the caller's own per-account task for as
/// long as the outbox has ops due.
///
/// Generic over all three rather than `&mut dyn MailSession` and `&dyn
/// Sender`: neither trait is object-safe (both use `async fn` in trait —
/// see each one's own docs for why that is the right call), so a trait
/// object is not on offer here regardless; the caller already holds one
/// concrete session and one concrete sender for the life of an account
/// task, and a generic context costs it nothing.
pub struct ExecContext<'a, S: MailSession, T: Sender, L: Lookups> {
    pub session: &'a mut S,
    pub sender: &'a T,
    pub lookups: &'a L,
}

/// What running an [`Op`] to completion hands back, beyond "it worked" —
/// only [`OpKind::Send`] and [`OpKind::AppendDraft`] have anything to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Executed {
    /// Every other kind: the flags changed, the label changed, the message
    /// moved, or — [`OpKind::Snooze`] — nothing had to happen at all.
    Ok,
    /// [`OpKind::Send`] succeeded: the `Message-ID` this crate minted for
    /// the outgoing message, so the caller can mark the draft
    /// [`karmayoga_core::mail::DraftState::Sent`] and recognise the copy
    /// that lands back through ordinary sync as this same send rather than
    /// a new incoming message. `sent_append_error` is `Some` when the
    /// message reached the server but this crate's own follow-up copy to
    /// Sent could not be made -- see [`send`]'s own docs for why that is
    /// still success, not a failure to retry.
    Sent { message_id: String, sent_append_error: Option<String> },
    /// [`OpKind::AppendDraft`] succeeded: the server's own uid for the
    /// fresh copy, when `APPENDUID` said so (see [`MailSession::append`]'s
    /// own docs for when it does not) — what the caller remembers as this
    /// draft's `draft_server_copy` for the *next* `AppendDraft` to delete.
    /// `message_id` is the fresh copy's own `Message-ID`, `None` when nothing
    /// was uploaded. `replaced` says the copy it replaced is off the server,
    /// or that there was none: when that failed, the old copy stays and is
    /// left for sync to show, rather than forgotten while still there.
    Appended { uid: Option<Uid>, message_id: Option<String>, replaced: bool },
}

/// Whether `err` is worth trying again. Exactly
/// [`MailError::Network`](crate::session::MailError::Network) — every other
/// shape means the credential, the request, or what this crate asked the
/// server for was wrong in a way that waiting does not fix. The drain loop
/// (`karmayoga-service`'s `outbox::drain_outbox`) is the only caller, and
/// this is written here, beside [`execute`], rather than left for that
/// module to re-derive from the variant names by hand.
pub fn is_retryable(err: &MailError) -> bool {
    matches!(err, MailError::Network(_))
}

/// Run one [`Op`] against a live session, per its [`OpKind`]. See the
/// module docs for the Gmail/non-Gmail split on `Archive` and why `Label`
/// and `Unlabel` are Gmail-only.
pub async fn execute<S: MailSession, T: Sender, L: Lookups>(
    op: &Op,
    ctx: &mut ExecContext<'_, S, T, L>,
) -> Result<Executed> {
    match &op.kind {
        OpKind::MarkRead => flags(op, ctx, Flags::SEEN, Flags::NONE).await.map(|()| Executed::Ok),
        OpKind::MarkUnread => flags(op, ctx, Flags::NONE, Flags::SEEN).await.map(|()| Executed::Ok),
        OpKind::Star => flags(op, ctx, Flags::FLAGGED, Flags::NONE).await.map(|()| Executed::Ok),
        OpKind::Unstar => flags(op, ctx, Flags::NONE, Flags::FLAGGED).await.map(|()| Executed::Ok),
        OpKind::Label { label } => label_op(op, ctx, label, true).await.map(|()| Executed::Ok),
        OpKind::Unlabel { label } => label_op(op, ctx, label, false).await.map(|()| Executed::Ok),
        // One op is a batch of one: see `execute_relocations`.
        OpKind::Archive | OpKind::Trash | OpKind::Move { .. } | OpKind::DeleteForever { .. } => {
            execute_relocations(std::slice::from_ref(op), ctx)
                .await
                .pop()
                .unwrap_or(Ok(Executed::Ok))
        }
        // Local only, per the plan's phase 5 section on snooze: nothing on
        // the server has anything called "snoozed", so there is nothing to
        // tell it. The op still exists, and still completes, so the outbox
        // and the audit trail (`Origin`) agree with the local record.
        OpKind::Snooze { .. } => Ok(Executed::Ok),
        OpKind::Send => send(op, ctx).await,
        OpKind::AppendDraft => append_draft(op, ctx).await,
        OpKind::DiscardDraft => discard_draft_op(op, ctx).await.map(|()| Executed::Ok),
    }
}

// ---- resolving a target -----------------------------------------------

async fn locations<S: MailSession, T: Sender, L: Lookups>(
    op: &Op,
    ctx: &ExecContext<'_, S, T, L>,
) -> Result<Vec<Located>> {
    match op.target {
        OpTarget::Thread(id) => ctx.lookups.thread_locations(id),
        OpTarget::Message(id) => ctx.lookups.message_locations(id),
        OpTarget::Draft(_) => Err(MailError::Protocol("this op kind cannot target a draft".into())),
    }
}

/// The locations an archive, trash or move acts on: every place the
/// thread's messages live except Sent, Drafts and the destination itself --
/// [`Sources::AllBut`], worked out once by [`execute_relocations`] for every
/// op it runs. A delete forever acts on the one mailbox it names
/// ([`Sources::Only`]) and nowhere else.
///
/// Not "every location", because a thread's reply sitting in Sent, or a
/// draft's own copy, must never be relocated just because the conversation
/// was archived, and a move from a mailbox to itself is refused by some
/// servers. And not "only the Inbox" either, because moving an archived
/// thread back to the Inbox, or trashing it from Archive or a custom folder,
/// is an ordinary thing to ask -- restricting to one source mailbox made
/// both silently do nothing. On Gmail, where Sent and Drafts are labels over
/// the one All Mail copy, the exclusion is of those folders' own selected
/// copies, which is exactly what keeps a sent reply's Sent entry in place.
async fn relocatable<S: MailSession, T: Sender, L: Lookups>(
    op: &Op,
    ctx: &ExecContext<'_, S, T, L>,
    sources: &Sources,
) -> Result<Vec<Located>> {
    let all = match &op.kind {
        // What was there when the person said yes, wherever its thread has
        // got to since: a reply filed in Trash afterwards is not in the list,
        // and a message gone already has nowhere left to be.
        OpKind::DeleteForever { messages, .. } => {
            let mut all = Vec::new();
            for &message in messages {
                all.extend(ctx.lookups.message_locations(message)?);
            }
            all
        }
        _ => locations(op, ctx).await?,
    };
    Ok(all
        .into_iter()
        .filter(|loc| match sources {
            Sources::AllBut(excluded) => !excluded.contains(&loc.mailbox),
            Sources::Only(mailbox) => loc.mailbox == *mailbox,
        })
        .collect())
}

/// Which of an op's locations [`execute_relocations`] acts on.
enum Sources {
    /// Every one but these mailboxes'.
    AllBut(Vec<String>),
    /// This mailbox's alone.
    Only(String),
}

/// What [`execute_relocations`] sends for each chunk of uids.
enum Command {
    /// `MOVE` into this mailbox.
    MoveTo(String),
    /// Drop the `\Inbox` label: Gmail's archive.
    DropInboxLabel,
    /// [`MailSession::delete`]: flag `\Deleted` and expunge. `expunge_all`
    /// follows it with a plain `EXPUNGE` on a server without `UIDPLUS`: only
    /// in a Trash or Spam, where whatever else is flagged was on its way out.
    Delete { expunge_all: bool },
}

/// Group a target's locations by mailbox — every [`MailSession`] verb that
/// touches a set of uids acts on whichever mailbox is currently `SELECT`ed,
/// so a target spread across two mailboxes (a Gmail message under two
/// labels, most often) needs one `SELECT` and one call per mailbox, not one
/// call with a uid set that mixes two mailboxes' numbering.
fn group_by_mailbox(locations: Vec<Located>) -> Vec<(String, UidSet)> {
    let mut map: std::collections::BTreeMap<String, UidSet> = std::collections::BTreeMap::new();
    for loc in locations {
        map.entry(loc.mailbox).or_default().insert(loc.uid);
    }
    map.into_iter().collect()
}

// ---- flag and label changes --------------------------------------------

async fn flags<S: MailSession, T: Sender, L: Lookups>(
    op: &Op,
    ctx: &mut ExecContext<'_, S, T, L>,
    add: Flags,
    remove: Flags,
) -> Result<()> {
    for (mailbox, uids) in group_by_mailbox(locations(op, ctx).await?) {
        ctx.session.select(&mailbox).await?;
        ctx.session.store_flags(&uids, add, remove).await?;
    }
    Ok(())
}

/// [`OpKind::Label`] and [`OpKind::Unlabel`], always through
/// [`MailSession::store_gmail_labels`] — see the module docs on why a label
/// is a Gmail concept in this crate's data model, never an IMAP custom
/// keyword. On a non-Gmail account this surfaces
/// [`MailError::Unsupported`], checked against [`Lookups::is_gmail`] before
/// this crate ever asks the session to try — the same client-side refusal
/// [`execute_relocations`] gives a missing special-use mailbox, rather than
/// leaving it to whatever an adapter's own [`MailSession::store_gmail_labels`]
/// happens to do with a label on a server that never advertised
/// `X-GM-EXT-1`. The drain loop reads either as permanent.
async fn label_op<S: MailSession, T: Sender, L: Lookups>(
    op: &Op,
    ctx: &mut ExecContext<'_, S, T, L>,
    label: &str,
    add: bool,
) -> Result<()> {
    if !ctx.lookups.is_gmail() {
        return Err(MailError::Unsupported("Gmail labels"));
    }
    let labels = [label.to_string()];
    for (mailbox, uids) in group_by_mailbox(locations(op, ctx).await?) {
        ctx.session.select(&mailbox).await?;
        if add {
            ctx.session.store_gmail_labels(&uids, &labels, &[]).await?;
        } else {
            ctx.session.store_gmail_labels(&uids, &[], &labels).await?;
        }
    }
    Ok(())
}

// ---- archive, trash, move -----------------------------------------------

/// The most uids one archive, trash or move command names. A sequence set
/// past a server's own line limit -- Dovecot's default is 64 KB -- comes
/// back `BAD`, which is permanent; the same bound, for the same reason, as
/// `imap.rs`'s `FLAG_REFETCH_BATCH_SIZE`.
const RELOCATE_CHUNK: usize = 500;

/// Whether `kind` is one [`execute_relocations`] runs: the three that move
/// a thread somewhere, and deleting it forever, each of which is one command
/// on the server however many messages it names.
pub fn relocates(kind: &OpKind) -> bool {
    matches!(
        kind,
        OpKind::Archive | OpKind::Trash | OpKind::Move { .. } | OpKind::DeleteForever { .. }
    )
}

/// Can ops of kinds `a` and `b` go to the server as one run? The same kind
/// with the same destination -- or, deleting forever, the same mailbox: the
/// messages each names are its own, and are what the run gathers up.
pub fn same_run(a: &OpKind, b: &OpKind) -> bool {
    match (a, b) {
        (OpKind::DeleteForever { from: x, .. }, OpKind::DeleteForever { from: y, .. }) => x == y,
        _ => a == b,
    }
}

/// Run thread-targeted ops of one relocating kind -- every one a `Trash`,
/// say, or every one a `Move` to the same mailbox -- as one: a `SELECT` and
/// a command per source mailbox, for all of them together, rather than per
/// op. Each op's own outcome comes back, in order.
///
/// What a bulk action needs. Quick Cleanup's Delete queues an op per
/// conversation, hundreds at once, and run one at a time each cost two round
/// trips. On a large Gmail account, where a `SELECT` of All Mail and a
/// `MOVE` out of it take about six seconds apiece, a thousand conversations
/// took over three hours to reach the server, and none of them was in Trash
/// until it had.
///
/// What each kind does on the server:
///
/// * **Archive**, on Gmail: drop the `\Inbox` label. The message stays in
///   All Mail under every other label it had, which is what archiving means
///   there. Everywhere else, a `MOVE` into the account's Archive mailbox --
///   created ahead of time by whatever registers the account (this crate
///   never creates a mailbox itself) -- and [`MailError::Unsupported`] when
///   the account was never seen to have one.
/// * **Trash**: a `MOVE` into the account's Trash, the same on Gmail and off
///   it, because Gmail's Trash is a real mailbox rather than a label.
/// * **Move**: a `MOVE` into the mailbox the op names.
/// * **DeleteForever**: [`MailSession::delete`] of the copies the op's
///   messages have in the mailbox it names, and of no other copy. That
///   mailbox must still be a Trash, Spam, Drafts or All Mail when the op
///   runs, or the op fails for good (the last two only ever for drafts,
///   which go to Trash on Gmail, see [`delete_located`]); one the vault no longer has took its mail with it, and the op
///   is done. On Gmail, deleting from either is how a message is deleted for
///   good. Without `UIDPLUS` a plain [`MailSession::expunge`] follows, since
///   in a Trash or Spam what else is flagged `\Deleted` was on its way out
///   too, and stopping at the flag would leave the mail on the server.
///
/// Only [`relocatable`] locations are touched; see its docs for why a
/// thread's copy in Sent must not move with the rest of it.
///
/// Where every op's messages live is resolved first. An op whose lookup
/// fails gets that failure and is left out of the commands -- a thread
/// deleted locally since it was queued must not take the others down with
/// it. A command that fails stops the run: every op whose uids had all gone
/// in commands that succeeded is done, and the rest get the failure, to be
/// retried or given up on one by one by the caller. Running a retried op's
/// uids again is harmless, since a `UID` command skips uids no longer there.
pub async fn execute_relocations<S: MailSession, T: Sender, L: Lookups>(
    ops: &[Op],
    ctx: &mut ExecContext<'_, S, T, L>,
) -> Vec<Result<Executed>> {
    let Some(first) = ops.first() else { return Vec::new() };
    if !relocates(&first.kind) || ops.iter().any(|op| !same_run(&op.kind, &first.kind)) {
        let mixed = MailError::Protocol("ops run together must be one relocating kind".into());
        return vec![Err(mixed); ops.len()];
    }

    let (command, sources) = match plan(&first.kind, ctx.lookups) {
        Ok(Some(plan)) => plan,
        Ok(None) => return vec![Ok(Executed::Ok); ops.len()],
        Err(e) => return vec![Err(e); ops.len()],
    };
    // A delete without `UIDPLUS` is only done once the `EXPUNGE` after it is.
    let expunge_after = matches!(command, Command::Delete { expunge_all: true })
        && !ctx.session.capabilities().uidplus;

    let mut outcomes: Vec<Option<Result<Executed>>> = vec![None; ops.len()];
    let mut owned: Vec<Vec<Located>> = vec![Vec::new(); ops.len()];
    let mut by_mailbox: std::collections::BTreeMap<String, UidSet> =
        std::collections::BTreeMap::new();
    for (i, op) in ops.iter().enumerate() {
        match relocatable(op, ctx, &sources).await {
            Ok(locations) => {
                for loc in &locations {
                    by_mailbox.entry(loc.mailbox.clone()).or_default().insert(loc.uid);
                }
                owned[i] = locations;
            }
            Err(e) => outcomes[i] = Some(Err(e)),
        }
    }

    let inbox_label = [String::from("\\Inbox")];
    let mut carried: std::collections::HashSet<(String, Uid)> = std::collections::HashSet::new();
    let mut failure = None;
    'mailboxes: for (mailbox, uids) in by_mailbox {
        if let Err(e) = ctx.session.select(&mailbox).await {
            failure = Some(e);
            break;
        }
        let mut flagged = Vec::new();
        for chunk in uids.chunks(RELOCATE_CHUNK) {
            let sent = match &command {
                Command::MoveTo(dest) => ctx.session.move_to(&chunk, dest).await,
                Command::DropInboxLabel => {
                    ctx.session.store_gmail_labels(&chunk, &[], &inbox_label).await
                }
                Command::Delete { .. } => ctx.session.delete(&chunk).await,
            };
            if let Err(e) = sent {
                failure = Some(e);
                break 'mailboxes;
            }
            if expunge_after {
                flagged.extend(chunk.iter());
            } else {
                carried.extend(chunk.iter().map(|uid| (mailbox.clone(), uid)));
            }
        }
        if expunge_after {
            if let Err(e) = ctx.session.expunge().await {
                failure = Some(e);
                break;
            }
            carried.extend(flagged.into_iter().map(|uid| (mailbox.clone(), uid)));
        }
    }

    // A draft deleted from Gmail's Drafts or All Mail went to Trash, which
    // would list its live conversation for 30 days: deleted from there too.
    if let (OpKind::DeleteForever { .. }, Command::MoveTo(trash), None) =
        (&first.kind, &command, &failure)
    {
        let mut message_ids = Vec::new();
        for (op, outcome) in ops.iter().zip(&outcomes) {
            if let (OpKind::DeleteForever { messages, .. }, None) = (&op.kind, outcome) {
                for &message in messages {
                    if let Ok(Some(id)) = ctx.lookups.message_id_header(message) {
                        message_ids.push(id);
                    }
                }
            }
        }
        purge_from_trash(ctx, trash, message_ids).await;
    }

    outcomes
        .into_iter()
        .zip(owned)
        .map(|(outcome, locations)| {
            outcome.unwrap_or_else(|| match &failure {
                Some(e)
                    if !locations
                        .iter()
                        .all(|loc| carried.contains(&(loc.mailbox.clone(), loc.uid))) =>
                {
                    Err(e.clone())
                }
                _ => Ok(Executed::Ok),
            })
        })
        .collect()
}

/// What a run of `kind` sends, and which locations it sends it for --
/// `None` when there is nothing left to send at all.
fn plan<L: Lookups>(kind: &OpKind, lookups: &L) -> Result<Option<(Command, Sources)>> {
    if let OpKind::DeleteForever { from, .. } = kind {
        return match lookups.mailbox_role(*from)? {
            None => Ok(None),
            Some(role) if role.empties() => Ok(Some((
                Command::Delete { expunge_all: true },
                Sources::Only(lookups.mailbox_name(*from)?),
            ))),
            // Drafts thrown away from a thread: copies another device saved,
            // or old autosaves, from Drafts or (on Gmail, archived there by
            // an expunge) All Mail -- see `Vault::delete_draft_copies`.
            // Gmail's Trash, as `delete_located` explains.
            Some(role @ (MailboxRole::Drafts | MailboxRole::All)) => {
                let trash = if lookups.is_gmail() {
                    lookups.special_use(MailboxRole::Trash)?
                } else {
                    None
                };
                let command = match trash {
                    Some(trash) => Command::MoveTo(trash),
                    // Drafts expunged as `delete_located` explains; an All
                    // Mail off Gmail is no place to expunge for others.
                    None => Command::Delete { expunge_all: role == MailboxRole::Drafts },
                };
                Ok(Some((command, Sources::Only(lookups.mailbox_name(*from)?))))
            }
            Some(_) => Err(MailError::Protocol(format!(
                "{} is no longer a Trash, Spam, Drafts or All Mail, so nothing in it is deleted for good",
                lookups.mailbox_name(*from)?
            ))),
        };
    }
    // Gmail's archive is a label change in place, so it has no destination
    // to leave out of the sources.
    let gmail_archive = matches!(kind, OpKind::Archive) && lookups.is_gmail();
    let destination = if gmail_archive { None } else { Some(destination(kind, lookups)?) };
    let mut excluded = excluded_sources(lookups)?;
    excluded.extend(destination.clone());
    let command = match destination {
        Some(dest) => Command::MoveTo(dest),
        None => Command::DropInboxLabel,
    };
    Ok(Some((command, Sources::AllBut(excluded))))
}

/// Where an archive (off Gmail), a trash or a move sends its messages: the
/// remote name of the account's Archive or Trash, or of the mailbox a move
/// names.
fn destination<L: Lookups>(kind: &OpKind, lookups: &L) -> Result<String> {
    match kind {
        OpKind::Archive => lookups
            .special_use(MailboxRole::Archive)?
            .ok_or(MailError::Unsupported("an Archive mailbox")),
        OpKind::Trash => lookups
            .special_use(MailboxRole::Trash)?
            .ok_or(MailError::Unsupported("a Trash mailbox")),
        OpKind::Move { to } => lookups.mailbox_name(*to),
        _ => Err(MailError::Protocol("not an archive, trash or move".into())),
    }
}

/// The account's Sent and Drafts, by remote name, whichever it has: the
/// copies [`relocatable`] never moves.
fn excluded_sources<L: Lookups>(lookups: &L) -> Result<Vec<String>> {
    Ok([lookups.special_use(MailboxRole::Sent)?, lookups.special_use(MailboxRole::Drafts)?]
        .into_iter()
        .flatten()
        .collect())
}

// ---- sending and drafts -------------------------------------------------

/// The draft a `Send` or `AppendDraft` op names, turned into
/// [`compose::Outgoing`] — shared between the two because a saved draft and
/// a sent one are built from exactly the same fields; only the flags they
/// are written with, and which mailbox they land in, differ.
async fn outgoing<S: MailSession, T: Sender, L: Lookups>(
    draft: &Draft,
    ctx: &ExecContext<'_, S, T, L>,
) -> Result<Outgoing> {
    let (in_reply_to, references) = match draft.in_reply_to {
        Some(parent_id) => {
            let raw = ctx.lookups.parent_raw(parent_id)?;
            let parsed = mime::parse(&raw).map_err(|e| MailError::Protocol(e.to_string()))?;
            compose::reply_headers(&parsed)
        }
        None => (None, Vec::new()),
    };

    // A picture pasted into the message goes inline, where it was put; every
    // other attachment as a file. The same bytes attached as a file *and*
    // pasted are two entries with one blob, and the compose sheet keeps the
    // pasted one last: that one goes inline, the others as files.
    let blobs: Vec<String> = draft.attachments.iter().map(|a| a.blob.to_string()).collect();
    let (html, inlined) = compose::inline_pictures(&draft.body_html, &blobs);
    let last_of: std::collections::HashMap<&String, usize> =
        blobs.iter().enumerate().map(|(i, blob)| (blob, i)).collect();
    let mut attachments = Vec::with_capacity(draft.attachments.len());
    for (i, (a, blob)) in draft.attachments.iter().zip(&blobs).enumerate() {
        let bytes = ctx.lookups.attachment_bytes(a.blob)?;
        let inline = inlined.contains(blob) && last_of.get(blob) == Some(&i);
        attachments.push(OutgoingAttachment {
            filename: a.filename.clone(),
            content_type: a.mime_type.clone(),
            bytes,
            inline_cid: inline.then(|| compose::picture_cid(blob)),
        });
    }

    Ok(Outgoing {
        from: address(&match ctx.lookups.sender_name(&draft.identity) {
            Some(name) => CoreAddress { name, email: draft.identity.clone() },
            None => CoreAddress::bare(draft.identity.clone()),
        }),
        to: draft.to.iter().map(address).collect(),
        cc: draft.cc.iter().map(address).collect(),
        bcc: draft.bcc.iter().map(address).collect(),
        reply_to: Vec::new(),
        subject: draft.subject.clone(),
        // A quoted parent's images point at this app until here -- see
        // `externalize_local_refs`.
        html: compose::externalize_local_refs(&html, |message, token| {
            let message = message.parse().ok()?;
            ctx.lookups.remote_image_url(message, token).ok().flatten()
        }),
        text: None,
        attachments,
        in_reply_to,
        references,
        message_id_domain: ctx.lookups.message_id_domain(),
        calendar: draft.calendar_part.as_ref().map(|part| compose::OutgoingCalendar {
            method: part.method.clone(),
            ics: part.ics.clone(),
        }),
        // Never set here: [`send`] is the one caller that ever needs a
        // stable id, and it sets this field itself on the [`Outgoing`] this
        // returns before handing it to [`compose::build`]. `append_draft`
        // takes what this leaves -- `None`, a fresh id every autosave.
        message_id: None,
    })
}

fn address(a: &CoreAddress) -> compose::Address {
    if a.name.is_empty() {
        compose::Address::new(a.email.clone())
    } else {
        compose::Address::named(a.name.clone(), a.email.clone())
    }
}

/// Build, send, and — everywhere but Gmail, which saves its own copy —
/// `APPEND` to Sent. Returns the minted `Message-ID` so the caller can mark
/// the draft sent and recognise the copy that lands back through ordinary
/// sync.
///
/// # A stable `Message-ID` across retries
///
/// `draft.message_id` is filled in, and persisted through
/// [`Lookups::set_draft_message_id`], the first time this draft is ever
/// built for sending -- before the message is actually sent, so a crash
/// between the two still leaves the id recorded. Every later attempt at
/// the same draft (a retryable failure, a `Send` op recovered from
/// `InFlight`) reads the same field back and reuses it rather than minting
/// another, which is what lets recovery ask the server "did the id I would
/// send with already arrive?" and get an answer that actually means
/// something -- see `crates/karmayoga-service/src/outbox.rs`'s recovery
/// path, the one caller that reads this back.
///
/// # Once `ctx.sender.send` returns `Ok`, this can no longer fail
///
/// The message has, at that point, actually reached the server -- the one
/// thing a `Send` op exists to make true. Everything after it is this
/// crate's own bookkeeping copy, not the send itself, and the drain loop's
/// only two responses to an `Err` are "retry" and "permanently failed,
/// reverse the local change": neither is survivable here, because retrying
/// means asking `ctx.sender` to send the same message again, and reversing
/// means telling the person their message was never sent when it was. So a
/// failure appending to Sent, or even finding out where Sent is, is caught
/// rather than propagated, and carried back as a note on the success value
/// instead -- see [`Executed::Sent`].
///
/// # A `Network` error does not mean the message was never sent
///
/// `ctx.sender.send` can fail with [`MailError::Network`] *after* the
/// server has already accepted the message -- a socket that drops between
/// `DATA`'s final `.` and the `250` reply looks, from here, identical to
/// one that drops before the server ever saw a byte of it, and neither
/// `lettre` (see `crate::smtp::classify`'s own docs) nor this crate can
/// tell the two apart. The drain loop's only answer to a retryable error is
/// "try again", which would resend a message that already arrived. So
/// every retry of a `Send` whose draft already carries a stamped
/// `message_id` -- meaning some earlier attempt got at least as far as
/// minting one, which only happens right before this function's own call to
/// `ctx.sender.send` -- asks the server first, through
/// [`already_delivered`], the same question
/// `crates/karmayoga-service/src/outbox.rs`'s crash-recovery path already
/// asks for the same reason. A message a server that does not save its own
/// Sent copy (see [`crate::smtp::needs_sent_append`]) has not yet had this
/// crate's own `AppendDraft`-equivalent append cannot be found this way --
/// the search comes back empty either way, and this crate resends rather
/// than risk silently dropping a message nobody confirmed reached anywhere.
/// A duplicate is the smaller mistake, on the same reasoning
/// [`already_delivered`]'s own docs give.
async fn send<S: MailSession, T: Sender, L: Lookups>(
    op: &Op,
    ctx: &mut ExecContext<'_, S, T, L>,
) -> Result<Executed> {
    let OpTarget::Draft(id) = op.target else {
        return Err(MailError::Protocol("a Send op must target a draft".into()));
    };
    let mut draft = ctx.lookups.draft(id)?;
    // The last line of defence against discarding a draft racing its own
    // pending `Send`: `karmayoga_core::Vault::discard_draft` cancels the op
    // before it ever becomes due, but a `Send` already `InFlight` at the
    // moment of discard is not cancelled -- see that method's own docs --
    // so it can still reach here with the draft already `Discarded`
    // underneath it. Refusing outright, rather than sending a message the
    // person no longer wants sent, is a permanent failure: retrying would
    // only ask the same question again, and there is no local state left
    // to reverse (`Discarded` is where `on_permanent_failure` would have
    // sent it back to `Editing` from, and a discarded draft has no
    // "editing" to return to).
    if matches!(draft.state, DraftState::Discarded) {
        return Err(MailError::Protocol("this draft was discarded before it could be sent".into()));
    }
    let is_retry = draft.message_id.is_some();
    if draft.message_id.is_none() {
        let minted = compose::generate_message_id(&ctx.lookups.message_id_domain());
        ctx.lookups.set_draft_message_id(id, &minted)?;
        draft.message_id = Some(minted);
    }
    let message_id = draft.message_id.clone().expect("just ensured Some above");

    if is_retry && already_delivered(ctx, &message_id).await {
        remove_drafts_server_copy(ctx, id).await;
        return Ok(Executed::Sent { message_id, sent_append_error: None });
    }

    let mut out = outgoing(&draft, ctx).await?;
    out.message_id = Some(message_id);
    let built = compose::build(&out).map_err(|e| MailError::Protocol(e.to_string()))?;
    ctx.sender.send(&built).await?;

    // The send itself succeeded, so a draft's own stale copy in Drafts --
    // if this crate ever `AppendDraft`ed one -- is done, and telling the
    // server so is best-effort on the same reasoning `append_sent_copy`
    // already gets: nothing about a failure to tidy up after a message that
    // has already gone out should turn into a retry (which would resend it)
    // or a reversal (which would tell the person it never sent).
    remove_drafts_server_copy(ctx, id).await;

    let sent_append_error = match append_sent_copy(ctx, &built).await {
        Ok(()) => None,
        Err(e) => {
            tracing::warn!(
                error = %e,
                message_id = %built.message_id,
                "sent, but could not append the Sent copy"
            );
            Some(e.to_string())
        }
    };
    Ok(Executed::Sent { message_id: built.message_id, sent_append_error })
}

/// The mailboxes worth asking [`already_delivered`]'s question of, in the
/// order tried. On Gmail, All Mail alone already holds a copy of every
/// message the account has ever sent or received, so there is nothing a
/// second role would find that the first did not. Off Gmail there is no
/// such single place: the *only* thing that ever puts a sent copy in Sent
/// on a plain IMAP account is this crate's own [`append_sent_copy`], which
/// runs *after* [`Sender::send`] -- so a crash or a dropped socket between
/// SMTP accepting the message and that append leaves Sent with nothing to
/// find, even though the message genuinely went out. Widening the search to
/// wherever the server itself might have filed the message -- Sent if the
/// append did land, All Mail on a provider that keeps one under another
/// name, Inbox for a message the account sent to itself -- catches those
/// without changing what a miss means: still "not found, or could not
/// check", never proof of absence.
pub fn search_roles(is_gmail: bool) -> &'static [MailboxRole] {
    if is_gmail {
        &[MailboxRole::All]
    } else {
        &[MailboxRole::Sent, MailboxRole::All, MailboxRole::Inbox]
    }
}

/// Whether `message_id` is already on the server -- asked directly, via
/// [`MailSession::search_message_id`], because the one thing a `Network`
/// error genuinely cannot say is whether the server's response this crate
/// never saw was actually an acceptance. Tried against every mailbox
/// [`search_roles`] names for this account, in order, stopping at the first
/// hit -- see that function's own docs for why more than one is worth
/// asking off Gmail, and `crates/karmayoga-service/src/outbox.rs`'s
/// `already_sent` for the crash-recovery counterpart of this exact
/// question, which searches the same roles.
///
/// `false` when every role comes back empty, cannot be resolved to a
/// mailbox name, or fails to search: the conservative answer sends again
/// rather than risking a message that reached nowhere being treated as
/// delivered. This still leaves one real gap, inherent to the question
/// rather than to how many mailboxes it asks: between the moment SMTP
/// accepts a message and the moment this crate's own Sent append (or the
/// server's own copy, on Gmail) actually lands, there is no server-side
/// evidence anywhere to find, for any role. A crash or a dropped socket
/// inside that narrow window still resends -- deliberately: a duplicate a
/// person can see and delete is a smaller mistake than a message this
/// crate silently decided not to send.
async fn already_delivered<S: MailSession, T: Sender, L: Lookups>(
    ctx: &mut ExecContext<'_, S, T, L>,
    message_id: &str,
) -> bool {
    for &role in search_roles(ctx.lookups.is_gmail()) {
        let Ok(Some(mailbox)) = ctx.lookups.special_use(role) else { continue };
        if matches!(ctx.session.search_message_id(&mailbox, message_id).await, Ok(Some(_))) {
            return true;
        }
    }
    false
}

/// The half of [`send`] that can fail without the send itself having
/// failed -- split out so `send` can catch exactly this and nothing else.
async fn append_sent_copy<S: MailSession, T: Sender, L: Lookups>(
    ctx: &mut ExecContext<'_, S, T, L>,
    built: &Built,
) -> Result<()> {
    if crate::smtp::needs_sent_append(ctx.session.capabilities())
        && let Some(sent) = ctx.lookups.special_use(MailboxRole::Sent)?
    {
        ctx.session.append(&sent, &built.raw, Flags::SEEN).await?;
    }
    Ok(())
}

/// Delete `id`'s own last `AppendDraft` copy from Drafts, if it ever had
/// one -- what both a successful [`send`] and [`discard_draft_op`] leave
/// behind otherwise. Best-effort: see [`send`]'s own docs on why a failure
/// here must never turn into a retry or a reversal of an action that has
/// already, genuinely, happened.
async fn remove_drafts_server_copy<S: MailSession, T: Sender, L: Lookups>(
    ctx: &mut ExecContext<'_, S, T, L>,
    id: DraftId,
) {
    let Ok(Some(copy)) = ctx.lookups.draft_server_copy(id) else { return };
    let message_id = ctx.lookups.draft_server_copy_message_id(id).ok().flatten();
    if let Err(e) = delete_located(ctx, &copy, message_id.as_deref()).await {
        tracing::warn!(error = %e, draft = %id, "could not remove a draft's stale server copy");
    }
}

/// Take one copy of a draft off the server: what [`append_draft`] does to the
/// copy it replaces, and [`remove_drafts_server_copy`] and
/// [`discard_draft_op`] to the last one.
///
/// On Gmail into Trash. A message expunged from Drafts there is, under the
/// account's default "when expunged" setting, archived into All Mail rather
/// than deleted, so every autosave's old copy used to pile up as mail of its
/// own. Trash empties itself after 30 days. Elsewhere it is deleted outright.
async fn delete_located<S: MailSession, T: Sender, L: Lookups>(
    ctx: &mut ExecContext<'_, S, T, L>,
    located: &Located,
    message_id: Option<&str>,
) -> Result<()> {
    ctx.session.select(&located.mailbox).await?;
    let uids = UidSet::single(located.uid);
    if ctx.lookups.is_gmail()
        && let Some(trash) = ctx.lookups.special_use(MailboxRole::Trash)?
    {
        ctx.session.move_to(&uids, &trash).await?;
        purge_from_trash(ctx, &trash, message_id.into_iter().map(str::to_string).collect()).await;
        return Ok(());
    }
    ctx.session.delete(&uids).await?;
    // Without `UIDPLUS` the delete only flagged it. In Drafts, what else is
    // flagged is drafts somebody threw away, so expunging the lot is what
    // every client there wants -- and a copy left flagged would sync back.
    if !ctx.session.capabilities().uidplus {
        ctx.session.expunge().await?;
    }
    Ok(())
}

/// Delete for good what a draft deletion just moved into Gmail's Trash, found
/// there by `Message-ID`: left in Trash, each copy carried its conversation's
/// thread id and put a live conversation in the Trash list for 30 days.
/// Best-effort: the copy is out of Drafts either way, and Trash empties
/// itself.
async fn purge_from_trash<S: MailSession, T: Sender, L: Lookups>(
    ctx: &mut ExecContext<'_, S, T, L>,
    trash: &str,
    message_ids: Vec<String>,
) {
    for message_id in message_ids {
        let found = ctx.session.search_message_id(trash, &message_id).await;
        if let Ok(Some(uid)) = found
            && let Err(e) = ctx.session.delete(&UidSet::single(uid)).await
        {
            tracing::warn!(error = %e, "could not delete a draft's copy from Trash");
        }
    }
}

/// [`OpKind::DiscardDraft`]'s whole job: delete a discarded draft's own
/// server copy, if it still has one. [`karmayoga_core::Vault::discard_draft`]
/// only ever enqueues this op when [`Draft::server_copy`] was `Some` at the
/// moment of discarding, but a second discard racing the first (a stale
/// compose window, a double click) could still find it already gone by the
/// time this runs -- treated the same as success, not an error, since the
/// end state either way is exactly what was asked for: nothing left on the
/// server.
async fn discard_draft_op<S: MailSession, T: Sender, L: Lookups>(
    op: &Op,
    ctx: &mut ExecContext<'_, S, T, L>,
) -> Result<()> {
    let OpTarget::Draft(id) = op.target else {
        return Err(MailError::Protocol("a DiscardDraft op must target a draft".into()));
    };
    // Brought back before this came due (`Vault::restore_draft`): the copy on
    // the server is that draft's again.
    if !matches!(ctx.lookups.draft(id)?.state, DraftState::Discarded) {
        return Ok(());
    }
    let Some(copy) = ctx.lookups.draft_server_copy(id)? else { return Ok(()) };
    let message_id = ctx.lookups.draft_server_copy_message_id(id)?;
    delete_located(ctx, &copy, message_id.as_deref()).await
}

/// `APPEND` a fresh copy of the draft to the account's Drafts mailbox, then
/// take the previous copy off the server when this crate has appended one
/// before -- see [`delete_located`]. In that order: deleting first left the
/// server with no copy at all whenever the upload failed. Only flagging it
/// `\Deleted` left every autosave's copy on a server that never expunges,
/// and in All Mail on Gmail, each one synced back as a message of its own.
/// [`MailSession::delete`] expunges only that uid, never what another
/// client flagged.
///
/// A server without `UIDPLUS` names no uid for the fresh copy, so it is
/// looked for by its `Message-ID`: without it, the next autosave would have
/// nothing to replace and the copies would pile up there instead.
async fn append_draft<S: MailSession, T: Sender, L: Lookups>(
    op: &Op,
    ctx: &mut ExecContext<'_, S, T, L>,
) -> Result<Executed> {
    let OpTarget::Draft(id) = op.target else {
        return Err(MailError::Protocol("an AppendDraft op must target a draft".into()));
    };
    let draft = ctx.lookups.draft(id)?;
    // An autosave's append still waiting when the draft was thrown away.
    // `Vault::discard_draft` asks for a server copy to be deleted only when it
    // knows of one, and this one does not exist yet: uploading it now would
    // leave the words on the server with nothing left to remove them. Only
    // ever skipped, never a failure, since nothing is owed to anybody.
    if matches!(draft.state, DraftState::Discarded) {
        return Ok(Executed::Appended { uid: None, message_id: None, replaced: true });
    }
    let Some(drafts_mailbox) = ctx.lookups.special_use(MailboxRole::Drafts)? else {
        return Err(MailError::Unsupported("a Drafts mailbox"));
    };

    let built = build(&draft, ctx).await?;
    let uid = match ctx.session.append(&drafts_mailbox, &built.raw, Flags::DRAFT).await? {
        Some(uid) => Some(uid),
        // Best-effort: not finding it costs one extra copy, as it always did.
        None => {
            ctx.session.search_message_id(&drafts_mailbox, &built.message_id).await.ok().flatten()
        }
    };

    // Only now the old copy goes: deleted first, a failed upload left the
    // server with no copy of the draft at all. A copy that will not go is
    // left where it is, and said so.
    let replaced = match ctx.lookups.draft_server_copy(id)? {
        Some(old) => {
            let old_id = ctx.lookups.draft_server_copy_message_id(id)?;
            match delete_located(ctx, &old, old_id.as_deref()).await {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(error = %e, draft = %id, "could not remove a draft's previous copy");
                    false
                }
            }
        }
        None => true,
    };
    Ok(Executed::Appended { uid, message_id: Some(built.message_id), replaced })
}

async fn build<S: MailSession, T: Sender, L: Lookups>(
    draft: &Draft,
    ctx: &ExecContext<'_, S, T, L>,
) -> Result<Built> {
    let out = outgoing(draft, ctx).await?;
    compose::build(&out).map_err(|e| MailError::Protocol(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{
        Capabilities, Changes, IdleEvent, MailboxState, RawStream, RemoteHeader, RemoteMailbox,
        SyncCursor,
    };
    use jiff::Timestamp;
    use karmayoga_core::id::AccountId;
    use karmayoga_core::mail::{Draft, Origin};
    use std::collections::HashMap;

    // ---- a fake session, recording every call it was asked to make --------

    #[derive(Default)]
    struct FakeSession {
        capabilities: Capabilities,
        selected: Option<String>,
        calls: Vec<String>,
        appended: Vec<(String, Vec<u8>)>,
        fail_next: Option<MailError>,
        /// What [`FakeSession::search_message_id`] answers, keyed by the
        /// bare id a test seeded -- standing in for a message a previous
        /// attempt (or another client entirely) already put on the server.
        found_message_ids: HashMap<String, Uid>,
        /// A mailbox whose `SELECT` fails, and how -- for the batch tests,
        /// where which ops a failure reaches depends on which mailbox it
        /// happened in.
        fail_select: Option<(String, MailError)>,
        /// An `APPEND` answered with no `APPENDUID`, as a server without
        /// `UIDPLUS` answers.
        no_append_uid: bool,
    }

    impl FakeSession {
        fn gmail() -> Self {
            Self {
                capabilities: Capabilities { gmail: true, ..Capabilities::default() },
                ..Self::default()
            }
        }

        fn take(&mut self) -> Result<()> {
            if let Some(err) = self.fail_next.take() { Err(err) } else { Ok(()) }
        }
    }

    #[allow(async_fn_in_trait)]
    impl MailSession for FakeSession {
        async fn mailboxes(&mut self) -> Result<Vec<RemoteMailbox>> {
            Ok(Vec::new())
        }

        async fn select(&mut self, mailbox: &str) -> Result<MailboxState> {
            self.calls.push(format!("select {mailbox}"));
            if let Some((name, err)) = &self.fail_select
                && name == mailbox
            {
                return Err(err.clone());
            }
            self.selected = Some(mailbox.to_string());
            Ok(MailboxState::default())
        }

        async fn changes_since(
            &mut self,
            _cursor: &SyncCursor,
            _known: &UidSet,
        ) -> Result<Changes> {
            Ok(Changes::default())
        }

        async fn headers(&mut self, _uids: &UidSet) -> Result<Vec<RemoteHeader>> {
            Ok(Vec::new())
        }

        async fn raw(&mut self, _uids: &UidSet) -> Result<RawStream<'_>> {
            Ok(Box::pin(futures::stream::empty::<Result<(Uid, Vec<u8>)>>()))
        }

        async fn store_flags(&mut self, uids: &UidSet, add: Flags, remove: Flags) -> Result<()> {
            self.take()?;
            self.calls.push(format!(
                "store_flags {} on {:?} +{add:?} -{remove:?}",
                uids.to_imap(),
                self.selected
            ));
            Ok(())
        }

        async fn store_gmail_labels(
            &mut self,
            uids: &UidSet,
            add: &[String],
            remove: &[String],
        ) -> Result<()> {
            self.take()?;
            self.calls.push(format!(
                "store_gmail_labels {} on {:?} +{add:?} -{remove:?}",
                uids.to_imap(),
                self.selected
            ));
            Ok(())
        }

        async fn move_to(&mut self, uids: &UidSet, mailbox: &str) -> Result<()> {
            self.take()?;
            self.calls.push(format!("move_to {} -> {mailbox}", uids.to_imap()));
            Ok(())
        }

        async fn delete(&mut self, uids: &UidSet) -> Result<()> {
            self.take()?;
            self.calls.push(format!(
                "delete {} on {:?} (uidplus={})",
                uids.to_imap(),
                self.selected,
                self.capabilities.uidplus
            ));
            Ok(())
        }

        async fn expunge(&mut self) -> Result<()> {
            self.take()?;
            self.calls.push(format!("expunge on {:?}", self.selected));
            Ok(())
        }

        async fn append(&mut self, mailbox: &str, raw: &[u8], flags: Flags) -> Result<Option<Uid>> {
            self.take()?;
            self.calls.push(format!("append to {mailbox} ({flags:?})"));
            self.appended.push((mailbox.to_string(), raw.to_vec()));
            if self.no_append_uid {
                return Ok(None);
            }
            Ok(Some(self.appended.len() as u32))
        }

        async fn search_message_id(
            &mut self,
            _mailbox: &str,
            message_id: &str,
        ) -> Result<Option<Uid>> {
            self.calls.push(format!("search_message_id {message_id}"));
            Ok(self.found_message_ids.get(message_id).copied())
        }

        async fn idle(&mut self, _stop: tokio::sync::watch::Receiver<()>) -> Result<IdleEvent> {
            Ok(IdleEvent::Stopped)
        }

        fn capabilities(&self) -> &Capabilities {
            &self.capabilities
        }
    }

    // ---- a fake sender ------------------------------------------------------

    #[derive(Default)]
    struct FakeSender {
        sent: std::sync::Mutex<Vec<Built>>,
        fail: Option<MailError>,
    }

    #[allow(async_fn_in_trait)]
    impl Sender for FakeSender {
        async fn send(&self, built: &Built) -> Result<SendReceipt> {
            if let Some(err) = &self.fail {
                return Err(err.clone());
            }
            let receipt = SendReceipt {
                accepted: built.envelope_to.clone(),
                server_response: "250 Ok".to_string(),
            };
            self.sent.lock().unwrap().push(Built {
                raw: built.raw.clone(),
                message_id: built.message_id.clone(),
                envelope_from: built.envelope_from.clone(),
                envelope_to: built.envelope_to.clone(),
            });
            Ok(receipt)
        }
    }

    // ---- a fake set of lookups ------------------------------------------------

    struct FakeLookups {
        gmail: bool,
        thread_locations: HashMap<ThreadId, Vec<Located>>,
        special_use: HashMap<&'static str, String>,
        mailbox_names: HashMap<MailboxId, String>,
        mailbox_roles: HashMap<MailboxId, MailboxRole>,
        // `RefCell`, not a plain map: `Lookups::set_draft_message_id` takes
        // `&self`, the same as every other method on the trait, so this is
        // the one field a test can still mutate through a shared reference
        // -- the same interior-mutability trade `FakeSender::sent` already
        // makes with a `Mutex`, chosen here as a `RefCell` since nothing in
        // this module's tests touches a `FakeLookups` from more than one
        // task at once.
        drafts: std::cell::RefCell<HashMap<DraftId, Draft>>,
        server_copies: HashMap<DraftId, Located>,
        parents: HashMap<MailMessageId, Vec<u8>>,
        /// Threads the vault no longer has: looking one up fails, the way a
        /// thread deleted locally after its op was queued does.
        gone: std::collections::HashSet<ThreadId>,
        message_locations: HashMap<MailMessageId, Vec<Located>>,
        /// `Message-ID`s by draft (its last copy) and by message.
        copy_ids: HashMap<DraftId, String>,
        message_ids: HashMap<MailMessageId, String>,
    }

    impl FakeLookups {
        fn new(gmail: bool) -> Self {
            Self {
                gmail,
                thread_locations: HashMap::new(),
                special_use: HashMap::new(),
                mailbox_names: HashMap::new(),
                mailbox_roles: HashMap::new(),
                drafts: std::cell::RefCell::new(HashMap::new()),
                server_copies: HashMap::new(),
                parents: HashMap::new(),
                gone: std::collections::HashSet::new(),
                message_locations: HashMap::new(),
                copy_ids: HashMap::new(),
                message_ids: HashMap::new(),
            }
        }

        fn with_thread(mut self, thread: ThreadId, locations: Vec<Located>) -> Self {
            self.thread_locations.insert(thread, locations);
            self
        }

        fn with_special_use(mut self, role: MailboxRole, name: &str) -> Self {
            self.special_use.insert(role_key(role), name.to_string());
            self
        }

        fn with_draft(self, draft: Draft) -> Self {
            self.drafts.borrow_mut().insert(draft.id, draft);
            self
        }

        fn with_parent(mut self, id: MailMessageId, raw: Vec<u8>) -> Self {
            self.parents.insert(id, raw);
            self
        }

        fn with_mailbox(mut self, id: MailboxId, name: &str, role: MailboxRole) -> Self {
            self.mailbox_names.insert(id, name.to_string());
            self.mailbox_roles.insert(id, role);
            self
        }

        fn with_message(mut self, id: MailMessageId, locations: Vec<Located>) -> Self {
            self.message_locations.insert(id, locations);
            self
        }
    }

    fn role_key(role: MailboxRole) -> &'static str {
        role.as_str()
    }

    impl Lookups for FakeLookups {
        fn thread_locations(&self, thread: ThreadId) -> Result<Vec<Located>> {
            if self.gone.contains(&thread) {
                return Err(MailError::Protocol("no such thread".into()));
            }
            Ok(self.thread_locations.get(&thread).cloned().unwrap_or_default())
        }

        fn message_locations(&self, message: MailMessageId) -> Result<Vec<Located>> {
            Ok(self.message_locations.get(&message).cloned().unwrap_or_default())
        }

        fn special_use(&self, role: MailboxRole) -> Result<Option<String>> {
            Ok(self.special_use.get(role_key(role)).cloned())
        }

        fn mailbox_name(&self, mailbox: MailboxId) -> Result<String> {
            self.mailbox_names
                .get(&mailbox)
                .cloned()
                .ok_or_else(|| MailError::Protocol("unknown mailbox".into()))
        }

        fn mailbox_role(&self, mailbox: MailboxId) -> Result<Option<MailboxRole>> {
            Ok(self.mailbox_roles.get(&mailbox).copied())
        }

        fn is_gmail(&self) -> bool {
            self.gmail
        }

        fn draft(&self, id: DraftId) -> Result<Draft> {
            self.drafts
                .borrow()
                .get(&id)
                .cloned()
                .ok_or_else(|| MailError::Protocol("no such draft".into()))
        }

        fn set_draft_message_id(&self, id: DraftId, message_id: &str) -> Result<()> {
            let mut drafts = self.drafts.borrow_mut();
            let draft =
                drafts.get_mut(&id).ok_or_else(|| MailError::Protocol("no such draft".into()))?;
            draft.message_id = Some(message_id.to_string());
            Ok(())
        }

        fn draft_server_copy(&self, id: DraftId) -> Result<Option<Located>> {
            Ok(self.server_copies.get(&id).cloned())
        }

        fn draft_server_copy_message_id(&self, id: DraftId) -> Result<Option<String>> {
            Ok(self.copy_ids.get(&id).cloned())
        }

        fn message_id_header(&self, id: MailMessageId) -> Result<Option<String>> {
            Ok(self.message_ids.get(&id).cloned())
        }

        fn parent_raw(&self, id: MailMessageId) -> Result<Vec<u8>> {
            self.parents.get(&id).cloned().ok_or_else(|| MailError::Protocol("no parent".into()))
        }

        fn attachment_bytes(&self, _blob: BlobId) -> Result<Vec<u8>> {
            Err(MailError::Protocol("no attachments in this fake".into()))
        }

        fn message_id_domain(&self) -> String {
            "example.com".to_string()
        }
    }

    fn op(account: AccountId, kind: OpKind, target: OpTarget) -> Op {
        Op::new(account, kind, target, Origin::Person)
    }

    fn simple_draft(account: AccountId) -> Draft {
        let mut d = Draft::new(account, "me@example.com", Origin::Person);
        d.to = vec![CoreAddress::bare("bob@example.com")];
        d.subject = "Hello".into();
        d.body_html = "<p>Hi</p>".into();
        d
    }

    // ---- flag changes ---------------------------------------------------

    #[tokio::test]
    async fn mark_read_stores_seen_on_every_located_copy() {
        let account = AccountId::new();
        let thread = ThreadId::new();
        let locations = vec![
            Located { mailbox: "INBOX".into(), uid: 1 },
            Located { mailbox: "Work".into(), uid: 7 },
        ];
        let lookups = FakeLookups::new(false).with_thread(thread, locations);
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let op = op(account, OpKind::MarkRead, OpTarget::Thread(thread));
        let outcome = execute(&op, &mut ctx).await.unwrap();

        assert_eq!(outcome, Executed::Ok);
        assert!(session.calls.iter().any(|c| c.contains("select INBOX")));
        assert!(session.calls.iter().any(|c| c.contains("select Work")));
        assert!(session.calls.iter().any(|c| c.contains("+Flags(\\Seen)")));
    }

    #[tokio::test]
    async fn star_and_unstar_use_the_flagged_bit() {
        let account = AccountId::new();
        let thread = ThreadId::new();
        let lookups = FakeLookups::new(false)
            .with_thread(thread, vec![Located { mailbox: "INBOX".into(), uid: 1 }]);
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        {
            let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };
            execute(&op(account, OpKind::Star, OpTarget::Thread(thread)), &mut ctx).await.unwrap();
        }
        assert!(session.calls.iter().any(|c| c.contains("+Flags(\\Flagged)")));

        {
            let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };
            execute(&op(account, OpKind::Unstar, OpTarget::Thread(thread)), &mut ctx)
                .await
                .unwrap();
        }
        assert!(session.calls.iter().any(|c| c.contains("-Flags(\\Flagged)")));
    }

    // ---- archive: Gmail vs everyone else ----------------------------------

    #[tokio::test]
    async fn gmail_archive_removes_only_the_inbox_label() {
        let account = AccountId::new();
        let thread = ThreadId::new();
        let lookups = FakeLookups::new(true)
            .with_thread(thread, vec![Located { mailbox: "[Gmail]/All Mail".into(), uid: 3 }])
            .with_special_use(MailboxRole::All, "[Gmail]/All Mail");
        let mut session = FakeSession::gmail();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let outcome = execute(&op(account, OpKind::Archive, OpTarget::Thread(thread)), &mut ctx)
            .await
            .unwrap();
        assert_eq!(outcome, Executed::Ok);
        // `remove` is a `&[String]` formatted with the standard library's own
        // `Debug`, which escapes the label's leading backslash -- checking
        // for `Inbox` alone (rather than pinning the exact escaping) is what
        // this assertion actually cares about.
        assert!(
            session.calls.iter().any(|c| c.contains("store_gmail_labels") && c.contains("Inbox"))
        );
        assert!(!session.calls.iter().any(|c| c.starts_with("move_to")));
    }

    #[tokio::test]
    async fn non_gmail_archive_moves_into_the_archive_mailbox() {
        let account = AccountId::new();
        let thread = ThreadId::new();
        let lookups = FakeLookups::new(false)
            .with_thread(thread, vec![Located { mailbox: "INBOX".into(), uid: 5 }])
            .with_special_use(MailboxRole::Inbox, "INBOX")
            .with_special_use(MailboxRole::Archive, "Archive");
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        execute(&op(account, OpKind::Archive, OpTarget::Thread(thread)), &mut ctx).await.unwrap();
        assert!(session.calls.iter().any(|c| c.contains("move_to 5 -> Archive")));
    }

    #[tokio::test]
    async fn archive_without_an_archive_mailbox_is_a_permanent_failure() {
        let account = AccountId::new();
        let thread = ThreadId::new();
        let lookups = FakeLookups::new(false)
            .with_thread(thread, vec![Located { mailbox: "INBOX".into(), uid: 5 }])
            .with_special_use(MailboxRole::Inbox, "INBOX");
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let err = execute(&op(account, OpKind::Archive, OpTarget::Thread(thread)), &mut ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, MailError::Unsupported(_)));
        assert!(!is_retryable(&err));
    }

    /// The regression for "archive and trash move the user's own sent
    /// replies": a thread with a message in the Inbox and the person's own
    /// reply already sitting in Sent must only move the Inbox copy.
    /// Archiving never touches Sent, and never issues a pointless move from
    /// a mailbox to itself.
    #[tokio::test]
    async fn archiving_a_thread_leaves_its_sent_reply_in_sent() {
        let account = AccountId::new();
        let thread = ThreadId::new();
        let lookups = FakeLookups::new(false)
            .with_thread(
                thread,
                vec![
                    Located { mailbox: "INBOX".into(), uid: 5 },
                    Located { mailbox: "Sent".into(), uid: 11 },
                ],
            )
            .with_special_use(MailboxRole::Inbox, "INBOX")
            .with_special_use(MailboxRole::Sent, "Sent")
            .with_special_use(MailboxRole::Archive, "Archive");
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        execute(&op(account, OpKind::Archive, OpTarget::Thread(thread)), &mut ctx).await.unwrap();

        assert!(session.calls.iter().any(|c| c.contains("move_to 5 -> Archive")));
        assert!(
            !session.calls.iter().any(|c| c.contains("Sent")),
            "the Sent copy must never be selected or moved: {:?}",
            session.calls
        );
    }

    /// Archiving a thread whose only location is already Archive itself
    /// (nothing left in the Inbox) must not issue a move from Archive to
    /// Archive -- there is nothing to do, so nothing is done.
    #[tokio::test]
    async fn archiving_an_already_archived_thread_issues_no_move() {
        let account = AccountId::new();
        let thread = ThreadId::new();
        let lookups = FakeLookups::new(false)
            .with_thread(thread, vec![Located { mailbox: "Archive".into(), uid: 5 }])
            .with_special_use(MailboxRole::Inbox, "INBOX")
            .with_special_use(MailboxRole::Archive, "Archive");
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        execute(&op(account, OpKind::Archive, OpTarget::Thread(thread)), &mut ctx).await.unwrap();
        assert!(session.calls.is_empty(), "{:?}", session.calls);
    }

    /// Trashing a thread from Archive moves the Archive copy: the source is
    /// wherever the thread is, not only the Inbox.
    #[tokio::test]
    async fn trashing_an_archived_thread_moves_it_out_of_archive() {
        let account = AccountId::new();
        let thread = ThreadId::new();
        let lookups = FakeLookups::new(false)
            .with_thread(thread, vec![Located { mailbox: "Archive".into(), uid: 5 }])
            .with_special_use(MailboxRole::Inbox, "INBOX")
            .with_special_use(MailboxRole::Archive, "Archive")
            .with_special_use(MailboxRole::Trash, "Trash");
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        execute(&op(account, OpKind::Trash, OpTarget::Thread(thread)), &mut ctx).await.unwrap();
        assert!(
            session.calls.iter().any(|c| c.contains("move_to 5 -> Trash")),
            "{:?}",
            session.calls
        );
    }

    // ---- label / unlabel: Gmail-only ---------------------------------------

    #[tokio::test]
    async fn label_uses_gmail_labels() {
        let account = AccountId::new();
        let thread = ThreadId::new();
        let lookups = FakeLookups::new(true)
            .with_thread(thread, vec![Located { mailbox: "[Gmail]/All Mail".into(), uid: 9 }]);
        let mut session = FakeSession::gmail();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let kind = OpKind::Label { label: "Work".into() };
        execute(&op(account, kind, OpTarget::Thread(thread)), &mut ctx).await.unwrap();
        assert!(
            session.calls.iter().any(|c| c.contains("store_gmail_labels") && c.contains("Work"))
        );
    }

    #[tokio::test]
    async fn label_on_a_non_gmail_account_is_a_permanent_failure() {
        let account = AccountId::new();
        let thread = ThreadId::new();
        let lookups = FakeLookups::new(false)
            .with_thread(thread, vec![Located { mailbox: "INBOX".into(), uid: 9 }]);
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let kind = OpKind::Label { label: "Work".into() };
        let err =
            execute(&op(account, kind, OpTarget::Thread(thread)), &mut ctx).await.unwrap_err();
        assert!(matches!(err, MailError::Unsupported(_)));
        assert!(!is_retryable(&err));
    }

    // ---- several relocations at once ---------------------------------------

    fn at(mailbox: &str, uid: Uid) -> Located {
        Located { mailbox: mailbox.into(), uid }
    }

    fn trashes(account: AccountId, threads: &[ThreadId]) -> Vec<Op> {
        threads.iter().map(|&t| op(account, OpKind::Trash, OpTarget::Thread(t))).collect()
    }

    #[tokio::test]
    async fn several_trashes_go_as_one_select_and_one_move_per_mailbox() {
        let account = AccountId::new();
        let (a, b, c) = (ThreadId::new(), ThreadId::new(), ThreadId::new());
        let lookups = FakeLookups::new(false)
            .with_thread(a, vec![at("INBOX", 1)])
            // The person's own reply stays in Sent, as it does for one op.
            .with_thread(b, vec![at("INBOX", 2), at("Sent", 7)])
            .with_thread(c, vec![at("Work", 9)])
            .with_special_use(MailboxRole::Sent, "Sent")
            .with_special_use(MailboxRole::Trash, "Trash");
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let outcomes = execute_relocations(&trashes(account, &[a, b, c]), &mut ctx).await;
        assert_eq!(outcomes, vec![Ok(Executed::Ok), Ok(Executed::Ok), Ok(Executed::Ok)]);
        assert_eq!(
            session.calls,
            ["select INBOX", "move_to 1:2 -> Trash", "select Work", "move_to 9 -> Trash"]
        );
    }

    #[tokio::test]
    async fn a_long_run_is_split_so_no_command_outgrows_a_server_line_limit() {
        let account = AccountId::new();
        let threads: Vec<ThreadId> = (0..1200).map(|_| ThreadId::new()).collect();
        let mut lookups = FakeLookups::new(false).with_special_use(MailboxRole::Trash, "Trash");
        for (uid, &t) in (1..).step_by(2).zip(&threads) {
            lookups = lookups.with_thread(t, vec![at("INBOX", uid)]);
        }
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let outcomes = execute_relocations(&trashes(account, &threads), &mut ctx).await;
        assert!(outcomes.iter().all(Result::is_ok));
        assert_eq!(session.calls.iter().filter(|c| c.starts_with("select")).count(), 1);
        assert_eq!(
            session.calls.iter().filter(|c| c.starts_with("move_to")).count(),
            3,
            "500 + 500 + 200: {:?}",
            session.calls.iter().map(|c| c.len()).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn a_thread_that_is_gone_fails_alone() {
        let account = AccountId::new();
        let (a, b, c) = (ThreadId::new(), ThreadId::new(), ThreadId::new());
        let mut lookups = FakeLookups::new(false)
            .with_thread(a, vec![at("INBOX", 1)])
            .with_thread(c, vec![at("INBOX", 3)])
            .with_special_use(MailboxRole::Trash, "Trash");
        lookups.gone.insert(b);
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let outcomes = execute_relocations(&trashes(account, &[a, b, c]), &mut ctx).await;
        assert!(outcomes[0].is_ok() && outcomes[2].is_ok(), "{outcomes:?}");
        assert!(matches!(outcomes[1], Err(MailError::Protocol(_))), "{outcomes:?}");
        assert_eq!(session.calls, ["select INBOX", "move_to 1,3 -> Trash"]);
    }

    #[tokio::test]
    async fn a_failed_command_fails_only_the_ops_not_already_carried() {
        let account = AccountId::new();
        let (a, b, c) = (ThreadId::new(), ThreadId::new(), ThreadId::new());
        let lookups = FakeLookups::new(false)
            .with_thread(a, vec![at("INBOX", 1)])
            .with_thread(b, vec![at("Work", 9)])
            // Half carried before the failure: not done.
            .with_thread(c, vec![at("INBOX", 2), at("Work", 10)])
            .with_special_use(MailboxRole::Trash, "Trash");
        let mut session = FakeSession {
            fail_select: Some(("Work".into(), MailError::Network("reset".into()))),
            ..FakeSession::default()
        };
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let outcomes = execute_relocations(&trashes(account, &[a, b, c]), &mut ctx).await;
        let network = Err(MailError::Network("reset".into()));
        assert_eq!(outcomes, vec![Ok(Executed::Ok), network.clone(), network]);
    }

    #[tokio::test]
    async fn several_gmail_archives_drop_the_inbox_label_together() {
        let account = AccountId::new();
        let (a, b) = (ThreadId::new(), ThreadId::new());
        let lookups = FakeLookups::new(true)
            .with_thread(a, vec![at("[Gmail]/All Mail", 4)])
            .with_thread(b, vec![at("[Gmail]/All Mail", 5)]);
        let mut session = FakeSession::gmail();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let ops: Vec<Op> =
            [a, b].iter().map(|&t| op(account, OpKind::Archive, OpTarget::Thread(t))).collect();
        let outcomes = execute_relocations(&ops, &mut ctx).await;
        assert!(outcomes.iter().all(Result::is_ok), "{outcomes:?}");
        assert_eq!(session.calls.len(), 2, "{:?}", session.calls);
        assert!(session.calls[1].starts_with("store_gmail_labels 4:5"), "{:?}", session.calls);
    }

    fn forever(
        account: AccountId,
        from: MailboxId,
        thread: ThreadId,
        messages: &[MailMessageId],
    ) -> Op {
        let kind = OpKind::DeleteForever { from, messages: messages.to_vec() };
        op(account, kind, OpTarget::Thread(thread))
    }

    fn uidplus() -> FakeSession {
        FakeSession {
            capabilities: Capabilities { uidplus: true, ..Capabilities::default() },
            ..FakeSession::default()
        }
    }

    /// Only the copies the op's messages have in the mailbox it names go: a
    /// reply still in the Inbox stays, and so does a message filed in Trash
    /// after the person said yes, which the op does not name.
    #[tokio::test]
    async fn deleting_forever_deletes_only_what_it_was_asked_to() {
        let account = AccountId::new();
        let trash = MailboxId::new();
        let (a, b) = (ThreadId::new(), ThreadId::new());
        let (first, later, alone) =
            (MailMessageId::new(), MailMessageId::new(), MailMessageId::new());
        let lookups = FakeLookups::new(false)
            .with_mailbox(trash, "Trash", MailboxRole::Trash)
            .with_message(first, vec![at("INBOX", 1), at("Trash", 40), at("Sent", 7)])
            .with_message(later, vec![at("Trash", 44)])
            .with_message(alone, vec![at("Trash", 41)]);
        let mut session = uidplus();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let ops = [forever(account, trash, a, &[first]), forever(account, trash, b, &[alone])];
        let outcomes = execute_relocations(&ops, &mut ctx).await;
        assert_eq!(outcomes, vec![Ok(Executed::Ok), Ok(Executed::Ok)]);
        assert_eq!(
            session.calls,
            ["select Trash", "delete 40:41 on Some(\"Trash\") (uidplus=true)"]
        );
    }

    /// On Gmail the Spam copy is its own message, apart from All Mail: that
    /// is the one deleted.
    #[tokio::test]
    async fn gmail_deletes_forever_from_its_own_spam() {
        let account = AccountId::new();
        let spam = MailboxId::new();
        let message = MailMessageId::new();
        let lookups = FakeLookups::new(true)
            .with_mailbox(spam, "[Gmail]/Spam", MailboxRole::Spam)
            .with_message(message, vec![at("[Gmail]/All Mail", 3), at("[Gmail]/Spam", 12)]);
        let mut session = FakeSession {
            capabilities: Capabilities { gmail: true, uidplus: true, ..Capabilities::default() },
            ..FakeSession::default()
        };
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        execute(&forever(account, spam, ThreadId::new(), &[message]), &mut ctx).await.unwrap();
        assert_eq!(session.calls.len(), 2, "{:?}", session.calls);
        assert_eq!(session.calls[0], "select [Gmail]/Spam");
        assert!(session.calls[1].starts_with("delete 12 "), "{:?}", session.calls);
    }

    /// Without `UIDPLUS`, flagging alone would leave the mail on the server:
    /// a plain `EXPUNGE` follows, and only then is the op done.
    #[tokio::test]
    async fn without_uidplus_a_delete_forever_expunges_the_mailbox() {
        let account = AccountId::new();
        let trash = MailboxId::new();
        let message = MailMessageId::new();
        let lookups = FakeLookups::new(false)
            .with_mailbox(trash, "Trash", MailboxRole::Trash)
            .with_message(message, vec![at("Trash", 9)]);
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        execute(&forever(account, trash, ThreadId::new(), &[message]), &mut ctx).await.unwrap();
        assert_eq!(
            session.calls,
            [
                "select Trash",
                "delete 9 on Some(\"Trash\") (uidplus=false)",
                "expunge on Some(\"Trash\")"
            ]
        );
    }

    /// A message deleted already -- by another client, or an earlier
    /// attempt -- and a mailbox the vault no longer has both send nothing,
    /// and both are done: what was asked for is already true.
    #[tokio::test]
    async fn deleting_forever_what_is_already_gone_is_done() {
        let account = AccountId::new();
        let (trash, vanished) = (MailboxId::new(), MailboxId::new());
        let lookups = FakeLookups::new(false).with_mailbox(trash, "Trash", MailboxRole::Trash);
        let mut session = uidplus();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let gone = MailMessageId::new();
        let outcome = execute(&forever(account, trash, ThreadId::new(), &[gone]), &mut ctx).await;
        assert_eq!(outcome, Ok(Executed::Ok));
        let outcome =
            execute(&forever(account, vanished, ThreadId::new(), &[gone]), &mut ctx).await;
        assert_eq!(outcome, Ok(Executed::Ok));
        assert!(session.calls.is_empty(), "{:?}", session.calls);
    }

    /// The mailbox stopped being a Trash while the op waited: nothing in it
    /// is deleted, and the op fails for good so the thread comes back.
    #[tokio::test]
    async fn a_mailbox_that_is_no_longer_trash_is_not_deleted_from() {
        let account = AccountId::new();
        let folder = MailboxId::new();
        let message = MailMessageId::new();
        let lookups = FakeLookups::new(false)
            .with_mailbox(folder, "Old Trash", MailboxRole::Other)
            .with_message(message, vec![at("Old Trash", 3)]);
        let mut session = uidplus();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let err = execute(&forever(account, folder, ThreadId::new(), &[message]), &mut ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, MailError::Protocol(_)), "{err:?}");
        assert!(!is_retryable(&err));
        assert!(session.calls.is_empty(), "{:?}", session.calls);
    }

    #[tokio::test]
    async fn ops_of_different_kinds_are_not_run_together() {
        let account = AccountId::new();
        let thread = ThreadId::new();
        let lookups = FakeLookups::new(false)
            .with_thread(thread, vec![at("INBOX", 1)])
            .with_special_use(MailboxRole::Trash, "Trash")
            .with_special_use(MailboxRole::Archive, "Archive");
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let ops = [
            op(account, OpKind::Trash, OpTarget::Thread(thread)),
            op(account, OpKind::Archive, OpTarget::Thread(thread)),
        ];
        let outcomes = execute_relocations(&ops, &mut ctx).await;
        assert!(outcomes.iter().all(|o| matches!(o, Err(MailError::Protocol(_)))));
        assert!(session.calls.is_empty(), "{:?}", session.calls);
    }

    // ---- trash and move -----------------------------------------------------

    #[tokio::test]
    async fn trash_moves_into_the_trash_mailbox() {
        let account = AccountId::new();
        let thread = ThreadId::new();
        let lookups = FakeLookups::new(false)
            .with_thread(thread, vec![Located { mailbox: "INBOX".into(), uid: 2 }])
            .with_special_use(MailboxRole::Inbox, "INBOX")
            .with_special_use(MailboxRole::Trash, "Trash");
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        execute(&op(account, OpKind::Trash, OpTarget::Thread(thread)), &mut ctx).await.unwrap();
        assert!(session.calls.iter().any(|c| c.contains("move_to 2 -> Trash")));
    }

    #[tokio::test]
    async fn move_resolves_the_destination_by_mailbox_id() {
        let account = AccountId::new();
        let thread = ThreadId::new();
        let dest = MailboxId::new();
        let mut lookups = FakeLookups::new(false)
            .with_thread(thread, vec![Located { mailbox: "INBOX".into(), uid: 4 }])
            .with_special_use(MailboxRole::Inbox, "INBOX");
        lookups.mailbox_names.insert(dest, "Projects".to_string());
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        execute(&op(account, OpKind::Move { to: dest }, OpTarget::Thread(thread)), &mut ctx)
            .await
            .unwrap();
        assert!(session.calls.iter().any(|c| c.contains("move_to 4 -> Projects")));
    }

    // ---- snooze: a true no-op -----------------------------------------------

    #[tokio::test]
    async fn snooze_touches_the_server_not_at_all() {
        let account = AccountId::new();
        let thread = ThreadId::new();
        let lookups = FakeLookups::new(false);
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let kind = OpKind::Snooze { until: Timestamp::now() };
        let outcome =
            execute(&op(account, kind, OpTarget::Thread(thread)), &mut ctx).await.unwrap();
        assert_eq!(outcome, Executed::Ok);
        assert!(session.calls.is_empty());
    }

    // ---- send: Sent-append on and off ---------------------------------------

    #[tokio::test]
    async fn sending_appends_to_sent_when_the_server_does_not_do_it_itself() {
        let account = AccountId::new();
        let draft = simple_draft(account);
        let id = draft.id;
        let lookups =
            FakeLookups::new(false).with_draft(draft).with_special_use(MailboxRole::Sent, "Sent");
        let mut session = FakeSession::default(); // not Gmail: needs_sent_append is true
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let outcome =
            execute(&op(account, OpKind::Send, OpTarget::Draft(id)), &mut ctx).await.unwrap();
        match outcome {
            Executed::Sent { message_id, sent_append_error } => {
                assert!(message_id.ends_with("@example.com"));
                assert_eq!(sent_append_error, None);
            }
            other => panic!("expected Sent, got {other:?}"),
        }
        assert_eq!(sender.sent.lock().unwrap().len(), 1);
        assert_eq!(session.appended.len(), 1);
        assert_eq!(session.appended[0].0, "Sent");
    }

    #[tokio::test]
    async fn gmail_sending_does_not_append_its_own_sent_copy() {
        let account = AccountId::new();
        let draft = simple_draft(account);
        let id = draft.id;
        let lookups =
            FakeLookups::new(true).with_draft(draft).with_special_use(MailboxRole::Sent, "Sent");
        let mut session = FakeSession::gmail();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        execute(&op(account, OpKind::Send, OpTarget::Draft(id)), &mut ctx).await.unwrap();
        assert_eq!(sender.sent.lock().unwrap().len(), 1);
        assert!(session.appended.is_empty(), "Gmail already saved its own Sent copy");
    }

    #[tokio::test]
    async fn sending_a_reply_threads_in_reply_to_and_references() {
        let account = AccountId::new();
        let mut draft = simple_draft(account);
        let parent_id = MailMessageId::new();
        draft.in_reply_to = Some(parent_id);
        let id = draft.id;
        let parent_raw = b"Message-ID: <parent@example.com>\r\n\
From: Alice <alice@example.com>\r\n\
Subject: Hi\r\n\
Date: Mon, 1 Jan 2024 00:00:00 +0000\r\n\
\r\n\
Original body.\r\n"
            .to_vec();
        let lookups = FakeLookups::new(false)
            .with_draft(draft)
            .with_special_use(MailboxRole::Sent, "Sent")
            .with_parent(parent_id, parent_raw);
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        execute(&op(account, OpKind::Send, OpTarget::Draft(id)), &mut ctx).await.unwrap();
        let sent = sender.sent.lock().unwrap();
        let raw = String::from_utf8_lossy(&sent[0].raw);
        assert!(raw.contains("In-Reply-To: <parent@example.com>"), "{raw}");
        assert!(raw.contains("References: <parent@example.com>"), "{raw}");
    }

    /// The regression for the bug this module's `send` docs describe: once
    /// `ctx.sender.send` has succeeded, a failure appending the Sent copy
    /// must come back as `Ok(Executed::Sent { .. })` with a note, never as
    /// an `Err` the drain loop could turn into a retry (sending the message
    /// again) or a permanent failure (telling the person it was never sent).
    #[tokio::test]
    async fn a_failed_sent_append_does_not_fail_the_send() {
        let account = AccountId::new();
        let draft = simple_draft(account);
        let id = draft.id;
        let lookups =
            FakeLookups::new(false).with_draft(draft).with_special_use(MailboxRole::Sent, "Sent");
        let mut session = FakeSession {
            fail_next: Some(MailError::Server("550 mailbox full".into())),
            ..FakeSession::default()
        };
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let outcome =
            execute(&op(account, OpKind::Send, OpTarget::Draft(id)), &mut ctx).await.unwrap();
        match outcome {
            Executed::Sent { sent_append_error, .. } => {
                assert!(sent_append_error.is_some(), "the append failure must be noted, not lost");
            }
            other => panic!("expected Sent, got {other:?}"),
        }
        assert_eq!(
            sender.sent.lock().unwrap().len(),
            1,
            "the message must have been sent exactly once"
        );
    }

    #[tokio::test]
    async fn a_rejected_send_is_a_permanent_failure_the_draft_can_report() {
        let account = AccountId::new();
        let draft = simple_draft(account);
        let id = draft.id;
        let lookups = FakeLookups::new(false).with_draft(draft);
        let mut session = FakeSession::default();
        let sender = FakeSender {
            fail: Some(MailError::Server("550 5.1.1 no such user".into())),
            ..FakeSender::default()
        };
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let err =
            execute(&op(account, OpKind::Send, OpTarget::Draft(id)), &mut ctx).await.unwrap_err();
        assert!(matches!(err, MailError::Server(_)));
        assert!(!is_retryable(&err));
    }

    /// The regression for "draft server copies leak": once a `Send` has
    /// actually reached the server, the draft's own last `AppendDraft` copy
    /// in Drafts must be removed, not left to sit there forever.
    #[tokio::test]
    async fn sending_a_draft_deletes_its_own_stale_drafts_copy() {
        let account = AccountId::new();
        let draft = simple_draft(account);
        let id = draft.id;
        let mut lookups = FakeLookups::new(false)
            .with_draft(draft)
            .with_special_use(MailboxRole::Sent, "Sent")
            .with_special_use(MailboxRole::Drafts, "Drafts");
        lookups.server_copies.insert(id, Located { mailbox: "Drafts".into(), uid: 7 });
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        execute(&op(account, OpKind::Send, OpTarget::Draft(id)), &mut ctx).await.unwrap();

        assert!(
            session.calls.iter().any(|c| c.contains("select Drafts")),
            "must select Drafts to remove the stale copy: {:?}",
            session.calls
        );
        assert!(
            session.calls.iter().any(|c| c.contains("delete 7")),
            "must delete the draft's own server copy after sending: {:?}",
            session.calls
        );
    }

    /// The regression for the double-send risk a `Network` error after SMTP
    /// `DATA` opens up: a retried `Send` whose draft's `message_id` is
    /// already on the server must be recognised as delivered, not sent a
    /// second time.
    #[tokio::test]
    async fn a_retried_send_already_on_the_server_is_not_sent_twice() {
        let account = AccountId::new();
        let mut draft = simple_draft(account);
        draft.message_id = Some("already-there@example.com".into());
        let id = draft.id;
        let lookups =
            FakeLookups::new(false).with_draft(draft).with_special_use(MailboxRole::Sent, "Sent");
        let mut session = FakeSession::default();
        session.found_message_ids.insert("already-there@example.com".into(), 3);
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let outcome =
            execute(&op(account, OpKind::Send, OpTarget::Draft(id)), &mut ctx).await.unwrap();
        match outcome {
            Executed::Sent { message_id, sent_append_error } => {
                assert_eq!(message_id, "already-there@example.com");
                assert_eq!(sent_append_error, None);
            }
            other => panic!("expected Sent, got {other:?}"),
        }
        assert!(sender.sent.lock().unwrap().is_empty(), "must not have been sent a second time");
    }

    /// The other half: a draft's very first `Send` attempt has never
    /// reached the server before, so there is nothing yet for
    /// `already_delivered` to usefully ask about.
    #[tokio::test]
    async fn a_first_send_never_checks_whether_it_already_arrived() {
        let account = AccountId::new();
        let draft = simple_draft(account);
        let id = draft.id;
        let lookups =
            FakeLookups::new(false).with_draft(draft).with_special_use(MailboxRole::Sent, "Sent");
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        execute(&op(account, OpKind::Send, OpTarget::Draft(id)), &mut ctx).await.unwrap();
        assert!(
            !session.calls.iter().any(|c| c.contains("search_message_id")),
            "a first attempt has nothing to check: {:?}",
            session.calls
        );
        assert_eq!(sender.sent.lock().unwrap().len(), 1);
    }

    /// The regression for "a discarded draft's queued send still goes
    /// out": `send`'s own defensive check must refuse a draft the vault
    /// already marked `Discarded`, rather than build and send it anyway.
    /// This is the last line of defence for the race
    /// `karmayoga_core::Vault::discard_draft`'s own docs describe -- a
    /// `Send` op already `InFlight` at the moment of discard is not
    /// cancelled there, so it can still reach this executor.
    #[tokio::test]
    async fn sending_a_discarded_draft_is_refused() {
        let account = AccountId::new();
        let mut draft = simple_draft(account);
        draft.state = karmayoga_core::mail::DraftState::Discarded;
        let id = draft.id;
        let lookups =
            FakeLookups::new(false).with_draft(draft).with_special_use(MailboxRole::Sent, "Sent");
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let err =
            execute(&op(account, OpKind::Send, OpTarget::Draft(id)), &mut ctx).await.unwrap_err();
        assert!(matches!(err, MailError::Protocol(_)), "must be a permanent failure: {err:?}");
        assert!(!is_retryable(&err));
        assert!(sender.sent.lock().unwrap().is_empty(), "a discarded draft must never be sent");
    }

    /// The regression for "the already-delivered check can never succeed on
    /// a non-Gmail account": a message the server actually filed under
    /// Inbox (a self-send, say) rather than Sent must still be recognised,
    /// not resent, once the search is widened past Sent alone.
    #[tokio::test]
    async fn a_retried_send_is_found_via_a_widened_search_when_sent_lacks_it() {
        let account = AccountId::new();
        let mut draft = simple_draft(account);
        draft.message_id = Some("already-there@example.com".into());
        let id = draft.id;
        // No `Sent` special-use registered at all -- only Inbox -- so this
        // can only pass if `already_delivered` tries more than the one
        // role it used to.
        let lookups =
            FakeLookups::new(false).with_draft(draft).with_special_use(MailboxRole::Inbox, "INBOX");
        let mut session = FakeSession::default();
        session.found_message_ids.insert("already-there@example.com".into(), 9);
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let outcome =
            execute(&op(account, OpKind::Send, OpTarget::Draft(id)), &mut ctx).await.unwrap();
        assert!(matches!(outcome, Executed::Sent { .. }), "{outcome:?}");
        assert!(
            sender.sent.lock().unwrap().is_empty(),
            "must not resend a message the widened search already found"
        );
    }

    // ---- append draft: coalescing the previous server copy ------------------

    #[tokio::test]
    async fn appending_a_draft_deletes_the_previous_server_copy_first() {
        let account = AccountId::new();
        let draft = simple_draft(account);
        let id = draft.id;
        let mut lookups = FakeLookups::new(false)
            .with_draft(draft)
            .with_special_use(MailboxRole::Drafts, "Drafts");
        lookups.server_copies.insert(id, Located { mailbox: "Drafts".into(), uid: 41 });
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let outcome = execute(&op(account, OpKind::AppendDraft, OpTarget::Draft(id)), &mut ctx)
            .await
            .unwrap();
        assert!(matches!(outcome, Executed::Appended { uid: Some(_), replaced: true, .. }));
        let at = |prefix: &str| session.calls.iter().position(|c| c.starts_with(prefix));
        // Expunged, not only flagged: a flagged copy stayed on the server and
        // synced back as a message of its own, one per autosave. Without
        // UIDPLUS the delete only flags it, so Drafts is expunged as well.
        let (append, delete) = (at("append to Drafts").unwrap(), at("delete 41").unwrap());
        assert!(append < delete, "the fresh copy goes up first: {:?}", session.calls);
        assert!(at("expunge on Some(\"Drafts\")").is_some(), "{:?}", session.calls);
    }

    /// The old copy refusing to go leaves the fresh one in place and says so,
    /// rather than failing the op and uploading a third.
    #[tokio::test]
    async fn an_old_copy_that_will_not_go_is_reported_not_retried() {
        let account = AccountId::new();
        let draft = simple_draft(account);
        let id = draft.id;
        let mut lookups = FakeLookups::new(false)
            .with_draft(draft)
            .with_special_use(MailboxRole::Drafts, "Drafts");
        lookups.server_copies.insert(id, Located { mailbox: "Old Drafts".into(), uid: 41 });
        let mut session = FakeSession {
            fail_select: Some(("Old Drafts".into(), MailError::Network("dropped".into()))),
            ..FakeSession::default()
        };
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let outcome = execute(&op(account, OpKind::AppendDraft, OpTarget::Draft(id)), &mut ctx)
            .await
            .unwrap();
        assert!(
            matches!(outcome, Executed::Appended { uid: Some(_), replaced: false, .. }),
            "{outcome:?}"
        );
        assert_eq!(session.appended.len(), 1);
    }

    /// Gmail archives what is expunged from Drafts into All Mail by default,
    /// so the old copy goes to Trash instead.
    #[tokio::test]
    async fn on_gmail_the_previous_copy_goes_to_trash() {
        let account = AccountId::new();
        let draft = simple_draft(account);
        let id = draft.id;
        let mut lookups = FakeLookups::new(true)
            .with_draft(draft)
            .with_special_use(MailboxRole::Drafts, "[Gmail]/Drafts")
            .with_special_use(MailboxRole::Trash, "[Gmail]/Trash");
        lookups.server_copies.insert(id, Located { mailbox: "[Gmail]/Drafts".into(), uid: 41 });
        lookups.copy_ids.insert(id, "<old@example.com>".into());
        let mut session = FakeSession::gmail();
        session.found_message_ids.insert("<old@example.com>".into(), 77);
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        execute(&op(account, OpKind::AppendDraft, OpTarget::Draft(id)), &mut ctx).await.unwrap();
        assert!(
            session.calls.iter().any(|c| c == "move_to 41 -> [Gmail]/Trash"),
            "{:?}",
            session.calls
        );
        // And out of Trash, found by its `Message-ID`, or its conversation
        // would be listed in Trash while the reply is being written.
        assert!(session.calls.iter().any(|c| c.starts_with("delete 77")), "{:?}", session.calls);
        assert!(!session.calls.iter().any(|c| c.starts_with("delete 41")), "{:?}", session.calls);
    }

    /// A draft deleted from its thread on Gmail goes to Trash and then out of
    /// it; elsewhere it is deleted, with Drafts expunged without UIDPLUS.
    #[tokio::test]
    async fn deleting_a_draft_copy_from_drafts() {
        let account = AccountId::new();
        let drafts = MailboxId::new();
        let thread = ThreadId::new();
        let copy = MailMessageId::new();
        let lookups = |gmail| {
            let mut l = FakeLookups::new(gmail)
                .with_mailbox(drafts, "Drafts", MailboxRole::Drafts)
                .with_special_use(MailboxRole::Trash, "Trash")
                .with_message(copy, vec![at("Drafts", 5)]);
            l.message_ids.insert(copy, "<copy@example.com>".into());
            l
        };
        let sender = FakeSender::default();

        let gmail = lookups(true);
        let mut session = FakeSession::gmail();
        session.found_message_ids.insert("<copy@example.com>".into(), 90);
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &gmail };
        let ran = execute_relocations(&[forever(account, drafts, thread, &[copy])], &mut ctx).await;
        assert!(ran[0].is_ok(), "{ran:?}");
        let calls = &session.calls;
        assert!(calls.iter().any(|c| c == "move_to 5 -> Trash"), "{calls:?}");
        assert!(calls.iter().any(|c| c.starts_with("delete 90")), "{calls:?}");

        let plain = lookups(false);
        let mut session = FakeSession::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &plain };
        let ran = execute_relocations(&[forever(account, drafts, thread, &[copy])], &mut ctx).await;
        assert!(ran[0].is_ok(), "{ran:?}");
        let calls = &session.calls;
        assert!(calls.iter().any(|c| c.starts_with("delete 5")), "{calls:?}");
        assert!(calls.iter().any(|c| c.starts_with("expunge")), "{calls:?}");
    }

    /// No `APPENDUID`: the fresh copy is found by its `Message-ID`, so the
    /// next autosave has a copy to replace.
    #[tokio::test]
    async fn a_copy_appended_without_a_uid_is_looked_up() {
        let account = AccountId::new();
        let draft = simple_draft(account);
        let id = draft.id;
        let lookups = FakeLookups::new(false)
            .with_draft(draft)
            .with_special_use(MailboxRole::Drafts, "Drafts");
        let mut session = FakeSession { no_append_uid: true, ..FakeSession::default() };
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let outcome = execute(&op(account, OpKind::AppendDraft, OpTarget::Draft(id)), &mut ctx)
            .await
            .unwrap();
        assert!(matches!(outcome, Executed::Appended { uid: None, .. }), "nothing seeded to find");
        assert!(
            session.calls.iter().any(|c| c.starts_with("search_message_id")),
            "{:?}",
            session.calls
        );
    }

    #[tokio::test]
    async fn appending_a_first_draft_never_looks_for_a_previous_copy() {
        let account = AccountId::new();
        let draft = simple_draft(account);
        let id = draft.id;
        let lookups = FakeLookups::new(false)
            .with_draft(draft)
            .with_special_use(MailboxRole::Drafts, "Drafts");
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        execute(&op(account, OpKind::AppendDraft, OpTarget::Draft(id)), &mut ctx).await.unwrap();
        assert!(!session.calls.iter().any(|c| c.contains("\\Deleted") || c.starts_with("delete")));
        assert_eq!(session.appended.len(), 1);
    }

    // ---- discard draft: deleting its server copy -----------------------------

    /// The other half of "draft server copies leak": discarding a draft
    /// that had reached the server at least once must remove that copy too.
    #[tokio::test]
    async fn discarding_a_draft_deletes_its_server_copy() {
        let account = AccountId::new();
        let mut draft = simple_draft(account);
        draft.state = karmayoga_core::mail::DraftState::Discarded;
        let id = draft.id;
        let mut lookups = FakeLookups::new(false).with_draft(draft);
        lookups.server_copies.insert(id, Located { mailbox: "Drafts".into(), uid: 9 });
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let outcome = execute(&op(account, OpKind::DiscardDraft, OpTarget::Draft(id)), &mut ctx)
            .await
            .unwrap();
        assert_eq!(outcome, Executed::Ok);
        assert!(session.calls.iter().any(|c| c.contains("select Drafts")));
        assert!(session.calls.iter().any(|c| c.contains("delete 9")));
    }

    /// `Vault::restore_draft` ran before the op came due: the copy stays,
    /// since the draft it belongs to is back.
    #[tokio::test]
    async fn a_discard_left_over_from_a_restored_draft_deletes_nothing() {
        let account = AccountId::new();
        let draft = simple_draft(account);
        let id = draft.id;
        let mut lookups = FakeLookups::new(false).with_draft(draft);
        lookups.server_copies.insert(id, Located { mailbox: "Drafts".into(), uid: 9 });
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        execute(&op(account, OpKind::DiscardDraft, OpTarget::Draft(id)), &mut ctx).await.unwrap();
        assert!(session.calls.is_empty(), "{:?}", session.calls);
    }

    /// An append queued by an autosave, reaching the front of the queue after
    /// the draft was thrown away: nothing goes up, or nothing would ever come
    /// down again.
    #[tokio::test]
    async fn appending_a_discarded_draft_uploads_nothing() {
        let account = AccountId::new();
        let mut draft = simple_draft(account);
        draft.state = karmayoga_core::mail::DraftState::Discarded;
        let id = draft.id;
        let lookups = FakeLookups::new(false)
            .with_draft(draft)
            .with_special_use(MailboxRole::Drafts, "Drafts");
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let outcome = execute(&op(account, OpKind::AppendDraft, OpTarget::Draft(id)), &mut ctx)
            .await
            .unwrap();
        assert!(matches!(outcome, Executed::Appended { uid: None, .. }));
        assert!(session.calls.is_empty(), "{:?}", session.calls);
        assert!(session.appended.is_empty());
    }

    #[tokio::test]
    async fn discarding_a_draft_with_no_server_copy_touches_the_server_not_at_all() {
        let account = AccountId::new();
        let draft = simple_draft(account);
        let id = draft.id;
        let lookups = FakeLookups::new(false).with_draft(draft);
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        execute(&op(account, OpKind::DiscardDraft, OpTarget::Draft(id)), &mut ctx).await.unwrap();
        assert!(session.calls.is_empty(), "{:?}", session.calls);
    }

    // ---- retryability ---------------------------------------------------

    #[test]
    fn only_network_errors_are_retryable() {
        assert!(is_retryable(&MailError::Network("timeout".into())));
        assert!(!is_retryable(&MailError::Auth("bad password".into())));
        assert!(!is_retryable(&MailError::Protocol("bug".into())));
        assert!(!is_retryable(&MailError::Server("550".into())));
        assert!(!is_retryable(&MailError::Unsupported("Gmail labels")));
    }
}
