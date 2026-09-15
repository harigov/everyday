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
//! replies."* [`crates::service::outbox::drain_outbox`](../../everyday_service/outbox/index.html)
//! is what actually implements `Lookups` against a real vault and calls this
//! from the sync agent's per-account task; this module never has to know
//! that crate exists.
//!
//! # Gmail's folder rule, and what it means for `Archive` and `Label`
//!
//! [`crate::session`]'s own docs already settle this for reading: on an
//! account with `X-GM-EXT-1`, a mailbox is a label, not a folder, so there
//! is nothing to `MOVE` a message into or out of. The same rule governs
//! writing. [`Archive`](everyday_core::mail::OpKind::Archive) removes the
//! `\Inbox` label via [`MailSession::store_gmail_labels`] on Gmail, and
//! `MOVE`s into the account's Archive mailbox everywhere else.
//! [`Label`](everyday_core::mail::OpKind::Label) and
//! [`Unlabel`](everyday_core::mail::OpKind::Unlabel) go through the same
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

use everyday_core::id::{BlobId, DraftId, MailMessageId, MailboxId, ThreadId};
use everyday_core::mail::{Address as CoreAddress, Draft, MailboxRole, Op, OpKind, OpTarget};

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
/// `crates/everyday-service/src/outbox.rs`'s, backs it with a real
/// [`everyday_core::Vault`]; this crate only ever sees the trait.
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
    /// (see [`everyday_core::mail::DraftAttachment`]), so this is the one
    /// thing about it a caller still has to fetch.
    fn attachment_bytes(&self, blob: BlobId) -> Result<Vec<u8>>;

    /// The domain half of the `Message-ID` [`compose::build`] mints for a
    /// freshly sent or appended message — see that function's own docs for
    /// why this crate never trusts a hostname for it.
    fn message_id_domain(&self) -> String;
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
    /// [`everyday_core::mail::DraftState::Sent`] and recognise the copy
    /// that lands back through ordinary sync as this same send rather than
    /// a new incoming message.
    Sent { message_id: String },
    /// [`OpKind::AppendDraft`] succeeded: the server's own uid for the
    /// fresh copy, when `APPENDUID` said so (see [`MailSession::append`]'s
    /// own docs for when it does not) — what the caller remembers as this
    /// draft's `draft_server_copy` for the *next* `AppendDraft` to delete.
    Appended { uid: Option<Uid> },
}

/// Whether `err` is worth trying again. Exactly
/// [`MailError::Network`](crate::session::MailError::Network) — every other
/// shape means the credential, the request, or what this crate asked the
/// server for was wrong in a way that waiting does not fix. The drain loop
/// (`everyday-service`'s `outbox::drain_outbox`) is the only caller, and
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
        OpKind::Archive => archive(op, ctx).await.map(|()| Executed::Ok),
        OpKind::Trash => trash(op, ctx).await.map(|()| Executed::Ok),
        OpKind::Move { to } => move_op(op, ctx, *to).await.map(|()| Executed::Ok),
        // Local only, per the plan's phase 5 section on snooze: nothing on
        // the server has anything called "snoozed", so there is nothing to
        // tell it. The op still exists, and still completes, so the outbox
        // and the audit trail (`Origin`) agree with the local record.
        OpKind::Snooze { .. } => Ok(Executed::Ok),
        OpKind::Send => send(op, ctx).await,
        OpKind::AppendDraft => append_draft(op, ctx).await,
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
/// [`archive`] and [`trash`] give a missing special-use mailbox, rather than
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

/// On Gmail, dropping the `\Inbox` label — the message stays in `All Mail`
/// under every other label it already had, exactly what "archive" means on
/// that provider. Everywhere else, a `MOVE` into the account's Archive
/// mailbox, created ahead of time by whatever registers the account (this
/// crate never creates a mailbox itself); [`MailError::Unsupported`] when
/// this account has never been seen to have one.
async fn archive<S: MailSession, T: Sender, L: Lookups>(
    op: &Op,
    ctx: &mut ExecContext<'_, S, T, L>,
) -> Result<()> {
    let locs = locations(op, ctx).await?;
    if ctx.lookups.is_gmail() {
        let inbox_label = [String::from("\\Inbox")];
        for (mailbox, uids) in group_by_mailbox(locs) {
            ctx.session.select(&mailbox).await?;
            ctx.session.store_gmail_labels(&uids, &[], &inbox_label).await?;
        }
        return Ok(());
    }
    let Some(dest) = ctx.lookups.special_use(MailboxRole::Archive)? else {
        return Err(MailError::Unsupported("an Archive mailbox"));
    };
    for (mailbox, uids) in group_by_mailbox(locs) {
        ctx.session.select(&mailbox).await?;
        ctx.session.move_to(&uids, &dest).await?;
    }
    Ok(())
}

/// A `MOVE` into the account's Trash mailbox — the same shape on Gmail and
/// off it, because Gmail's own Trash is a real mailbox (unlike Inbox), so
/// there is no label-only branch here the way [`archive`] needs one.
async fn trash<S: MailSession, T: Sender, L: Lookups>(
    op: &Op,
    ctx: &mut ExecContext<'_, S, T, L>,
) -> Result<()> {
    let Some(dest) = ctx.lookups.special_use(MailboxRole::Trash)? else {
        return Err(MailError::Unsupported("a Trash mailbox"));
    };
    for (mailbox, uids) in group_by_mailbox(locations(op, ctx).await?) {
        ctx.session.select(&mailbox).await?;
        ctx.session.move_to(&uids, &dest).await?;
    }
    Ok(())
}

async fn move_op<S: MailSession, T: Sender, L: Lookups>(
    op: &Op,
    ctx: &mut ExecContext<'_, S, T, L>,
    to: MailboxId,
) -> Result<()> {
    let dest = ctx.lookups.mailbox_name(to)?;
    for (mailbox, uids) in group_by_mailbox(locations(op, ctx).await?) {
        ctx.session.select(&mailbox).await?;
        ctx.session.move_to(&uids, &dest).await?;
    }
    Ok(())
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

    let mut attachments = Vec::with_capacity(draft.attachments.len());
    for a in &draft.attachments {
        let bytes = ctx.lookups.attachment_bytes(a.blob)?;
        attachments.push(OutgoingAttachment {
            filename: a.filename.clone(),
            content_type: a.mime_type.clone(),
            bytes,
            inline_cid: None,
        });
    }

    Ok(Outgoing {
        from: address(&CoreAddress::bare(draft.identity.clone())),
        to: draft.to.iter().map(address).collect(),
        cc: draft.cc.iter().map(address).collect(),
        bcc: draft.bcc.iter().map(address).collect(),
        reply_to: Vec::new(),
        subject: draft.subject.clone(),
        html: draft.body_html.clone(),
        text: None,
        attachments,
        in_reply_to,
        references,
        message_id_domain: ctx.lookups.message_id_domain(),
        calendar: draft.calendar_part.as_ref().map(|part| compose::OutgoingCalendar {
            method: part.method.clone(),
            ics: part.ics.clone(),
        }),
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
async fn send<S: MailSession, T: Sender, L: Lookups>(
    op: &Op,
    ctx: &mut ExecContext<'_, S, T, L>,
) -> Result<Executed> {
    let OpTarget::Draft(id) = op.target else {
        return Err(MailError::Protocol("a Send op must target a draft".into()));
    };
    let draft = ctx.lookups.draft(id)?;
    let built = build(&draft, ctx).await?;
    ctx.sender.send(&built).await?;

    if crate::smtp::needs_sent_append(ctx.session.capabilities())
        && let Some(sent) = ctx.lookups.special_use(MailboxRole::Sent)?
    {
        ctx.session.append(&sent, &built.raw, Flags::SEEN).await?;
    }
    Ok(Executed::Sent { message_id: built.message_id })
}

/// `APPEND` a fresh copy of the draft to the account's Drafts mailbox,
/// deleting the previous copy first when this crate has appended one
/// before. Deletion is `\Deleted` via [`MailSession::store_flags`] rather
/// than an outright removal — the trait offers no `EXPUNGE` of its own, on
/// the same reasoning [`MailSession::move_to`]'s fallback path already
/// accepts: a client that marks and moves on, leaving the actual expunge to
/// whichever ordinary sync pass owns that mailbox next, never risks
/// discarding a `\Deleted` message another client marked for its own
/// reasons and has not gotten around to expunging yet.
async fn append_draft<S: MailSession, T: Sender, L: Lookups>(
    op: &Op,
    ctx: &mut ExecContext<'_, S, T, L>,
) -> Result<Executed> {
    let OpTarget::Draft(id) = op.target else {
        return Err(MailError::Protocol("an AppendDraft op must target a draft".into()));
    };
    let draft = ctx.lookups.draft(id)?;
    let Some(drafts_mailbox) = ctx.lookups.special_use(MailboxRole::Drafts)? else {
        return Err(MailError::Unsupported("a Drafts mailbox"));
    };

    if let Some(old) = ctx.lookups.draft_server_copy(id)? {
        ctx.session.select(&old.mailbox).await?;
        ctx.session.store_flags(&UidSet::single(old.uid), Flags::DELETED, Flags::NONE).await?;
    }

    let built = build(&draft, ctx).await?;
    let uid = ctx.session.append(&drafts_mailbox, &built.raw, Flags::DRAFT).await?;
    Ok(Executed::Appended { uid })
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
    use everyday_core::id::AccountId;
    use everyday_core::mail::{Draft, Origin};
    use jiff::Timestamp;
    use std::collections::HashMap;

    // ---- a fake session, recording every call it was asked to make --------

    #[derive(Default)]
    struct FakeSession {
        capabilities: Capabilities,
        selected: Option<String>,
        calls: Vec<String>,
        appended: Vec<(String, Vec<u8>)>,
        fail_next: Option<MailError>,
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
            self.selected = Some(mailbox.to_string());
            self.calls.push(format!("select {mailbox}"));
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

        async fn append(&mut self, mailbox: &str, raw: &[u8], flags: Flags) -> Result<Option<Uid>> {
            self.take()?;
            self.calls.push(format!("append to {mailbox} ({flags:?})"));
            self.appended.push((mailbox.to_string(), raw.to_vec()));
            Ok(Some(self.appended.len() as u32))
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
        drafts: HashMap<DraftId, Draft>,
        server_copies: HashMap<DraftId, Located>,
        parents: HashMap<MailMessageId, Vec<u8>>,
    }

    impl FakeLookups {
        fn new(gmail: bool) -> Self {
            Self {
                gmail,
                thread_locations: HashMap::new(),
                special_use: HashMap::new(),
                mailbox_names: HashMap::new(),
                drafts: HashMap::new(),
                server_copies: HashMap::new(),
                parents: HashMap::new(),
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

        fn with_draft(mut self, draft: Draft) -> Self {
            self.drafts.insert(draft.id, draft);
            self
        }

        fn with_parent(mut self, id: MailMessageId, raw: Vec<u8>) -> Self {
            self.parents.insert(id, raw);
            self
        }
    }

    fn role_key(role: MailboxRole) -> &'static str {
        role.as_str()
    }

    impl Lookups for FakeLookups {
        fn thread_locations(&self, thread: ThreadId) -> Result<Vec<Located>> {
            Ok(self.thread_locations.get(&thread).cloned().unwrap_or_default())
        }

        fn message_locations(&self, _message: MailMessageId) -> Result<Vec<Located>> {
            Ok(Vec::new())
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

        fn is_gmail(&self) -> bool {
            self.gmail
        }

        fn draft(&self, id: DraftId) -> Result<Draft> {
            self.drafts.get(&id).cloned().ok_or_else(|| MailError::Protocol("no such draft".into()))
        }

        fn draft_server_copy(&self, id: DraftId) -> Result<Option<Located>> {
            Ok(self.server_copies.get(&id).cloned())
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
            .with_thread(thread, vec![Located { mailbox: "[Gmail]/All Mail".into(), uid: 3 }]);
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
            .with_thread(thread, vec![Located { mailbox: "INBOX".into(), uid: 5 }]);
        let mut session = FakeSession::default();
        let sender = FakeSender::default();
        let mut ctx = ExecContext { session: &mut session, sender: &sender, lookups: &lookups };

        let err = execute(&op(account, OpKind::Archive, OpTarget::Thread(thread)), &mut ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, MailError::Unsupported(_)));
        assert!(!is_retryable(&err));
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

    // ---- trash and move -----------------------------------------------------

    #[tokio::test]
    async fn trash_moves_into_the_trash_mailbox() {
        let account = AccountId::new();
        let thread = ThreadId::new();
        let lookups = FakeLookups::new(false)
            .with_thread(thread, vec![Located { mailbox: "INBOX".into(), uid: 2 }])
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
            .with_thread(thread, vec![Located { mailbox: "INBOX".into(), uid: 4 }]);
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
            Executed::Sent { message_id } => assert!(message_id.ends_with("@example.com")),
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
        assert!(matches!(outcome, Executed::Appended { uid: Some(_) }));
        assert!(session.calls.iter().any(|c| c.contains("select Drafts")));
        assert!(
            session.calls.iter().any(|c| c.contains("store_flags 41") && c.contains("\\Deleted"))
        );
        assert!(session.calls.iter().any(|c| c.contains("append to Drafts")));
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
        assert!(!session.calls.iter().any(|c| c.contains("\\Deleted")));
        assert_eq!(session.appended.len(), 1);
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
