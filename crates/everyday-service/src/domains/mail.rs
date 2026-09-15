//! Mail: mailboxes, threads and the messages in them, and -- from phase 3 --
//! every write a person makes: the actions on a batch of threads, and the
//! whole life of a draft.
//!
//! `list_mailboxes`, `list_threads` and `get_thread` are phase 2's
//! read-only trio and are also the two calls phase 5's `list_threads` and
//! `read_thread` tools will end up wrapping, so their shape was worth
//! getting right before anything else here existed.
//!
//! Everything below them is phase 3, built on exactly two vault entry
//! points per `docs/plans/mail.md`'s "What an action does": one write that
//! changes a thread's local rows *and* enqueues the `Op` telling the account
//! task to make the server agree
//! ([`everyday_core::Vault::apply_thread_ops`]), and the draft lifecycle's
//! own atomic pairs. Every write here is `Origin::Person` -- the only
//! origin a person's own click or keystroke can ever be -- which is also
//! why [`Service::check_mail_rate_limit`] is a visible no-op on every call
//! site below: `Origin::Person` is never rate limited (see
//! [`everyday_core::mail::Origin::is_rate_limited`]), and the call is made
//! anyway so that this is the one enqueue path phase 5's `Assistant` and
//! `Mcp` origins step onto later, rather than a second path that has to
//! remember to add the check when they do.
//!
//! Every write that touches an account's outbox also wakes that account's
//! sync task with [`Service::notify_outbox`], so undo send's countdown and
//! an archive's confirmation do not wait for the next poll.

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Change, Kind};
use crate::service::{Service, blocking};
use everyday_core::Vault;
use everyday_core::id::{AccountId, DraftId, MailMessageId, MailboxId, OpId, ThreadId};
use everyday_core::mail::{
    AttendeeResponse, Category, Draft, DraftCalendarPart, InviteMethod, Mailbox, Message, Op,
    OpKind, OpTarget, Origin, Thread, undo_send_delay,
};
use everyday_core::store::mail::{ThreadFilter, ThreadPage};
use everyday_mail::{compose, invite, mime};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailboxesQuery {
    pub account: AccountId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListThreads {
    pub mailbox: MailboxId,
    #[serde(default)]
    pub filter: ThreadFilter,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default = "default_page_size")]
    pub limit: u32,
}

/// What a thread list asks for when a caller does not say -- large enough
/// that a virtual list's first paint has plenty to scroll, small enough that
/// decrypting one page is not itself the delay it exists to avoid.
fn default_page_size() -> u32 {
    50
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRef {
    pub id: ThreadId,
}

/// A thread and every message in it -- what opening one reads. Bodies are
/// not included; the interface asks for each with `get_body` as it draws
/// them, once that command exists.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDetail {
    pub thread: Thread,
    pub messages: Vec<Message>,
}

// ---- batch thread actions ------------------------------------------------

/// What every plain "do this to a batch of threads" command takes --
/// `mark_read`, `mark_unread`, `star`, `unstar`, `archive`, `trash` and
/// `unsnooze`. `move_to_mailbox`, `label`, `unlabel` and `snooze` reuse the
/// same `threads` field inside their own, slightly larger argument structs
/// below.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadIds {
    pub threads: Vec<ThreadId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveThreads {
    pub threads: Vec<ThreadId>,
    pub to: MailboxId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelThreads {
    pub threads: Vec<ThreadId>,
    pub label: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnoozeThreads {
    pub threads: Vec<ThreadId>,
    pub until: Timestamp,
}

/// Apply `kind`'s local effect to `threads` and enqueue one [`OpKind`] op
/// per thread, all as [`Origin::Person`] -- the one shared body every batch
/// action command below wraps. See the module docs for why
/// [`Service::check_mail_rate_limit`] is called here even though a person's
/// own action never trips it.
async fn batch_op(
    svc: Arc<Service>,
    threads: Vec<ThreadId>,
    kind: OpKind,
) -> CommandResult<Vec<Op>> {
    let origin = Origin::Person;
    svc.check_mail_rate_limit(&origin, "person")?;
    let vault = svc.require()?;
    let ops = blocking(move || Ok(vault.apply_thread_ops(&threads, kind, origin)?)).await?;
    let cache = svc.mail_unread_cache();
    for account in ops.iter().map(|op| op.account_id).collect::<BTreeSet<_>>() {
        svc.notify_outbox(account);
        // Every batch action passes through here, including the five that
        // cannot move a thread across the read/unread line (star, label,
        // snooze and their opposites) -- invalidating regardless is the
        // cheap, always-correct choice `mailsync::unread_cache`'s module
        // docs describe; the alternative is a `match` on `kind` that has to
        // be kept in step with `OpKind::is_flag_change` by hand.
        if let Some(cache) = &cache {
            cache.invalidate(account);
        }
    }
    Ok(ops)
}

async fn mark_read(svc: Arc<Service>, _ctx: Ctx, args: ThreadIds) -> CommandResult<Vec<Op>> {
    batch_op(svc, args.threads, OpKind::MarkRead).await
}

async fn mark_unread(svc: Arc<Service>, _ctx: Ctx, args: ThreadIds) -> CommandResult<Vec<Op>> {
    batch_op(svc, args.threads, OpKind::MarkUnread).await
}

async fn star(svc: Arc<Service>, _ctx: Ctx, args: ThreadIds) -> CommandResult<Vec<Op>> {
    batch_op(svc, args.threads, OpKind::Star).await
}

async fn unstar(svc: Arc<Service>, _ctx: Ctx, args: ThreadIds) -> CommandResult<Vec<Op>> {
    batch_op(svc, args.threads, OpKind::Unstar).await
}

async fn archive(svc: Arc<Service>, _ctx: Ctx, args: ThreadIds) -> CommandResult<Vec<Op>> {
    batch_op(svc, args.threads, OpKind::Archive).await
}

async fn trash(svc: Arc<Service>, _ctx: Ctx, args: ThreadIds) -> CommandResult<Vec<Op>> {
    batch_op(svc, args.threads, OpKind::Trash).await
}

async fn move_to_mailbox(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: MoveThreads,
) -> CommandResult<Vec<Op>> {
    batch_op(svc, args.threads, OpKind::Move { to: args.to }).await
}

async fn label(svc: Arc<Service>, _ctx: Ctx, args: LabelThreads) -> CommandResult<Vec<Op>> {
    batch_op(svc, args.threads, OpKind::Label { label: args.label }).await
}

async fn unlabel(svc: Arc<Service>, _ctx: Ctx, args: LabelThreads) -> CommandResult<Vec<Op>> {
    batch_op(svc, args.threads, OpKind::Unlabel { label: args.label }).await
}

async fn snooze(svc: Arc<Service>, _ctx: Ctx, args: SnoozeThreads) -> CommandResult<Vec<Op>> {
    batch_op(svc, args.threads, OpKind::Snooze { until: args.until }).await
}

/// Bring a thread back from snooze early. No outbox op -- see
/// [`everyday_core::Vault::release_snooze`]'s own docs for why: snooze never
/// told the server anything, so there is nothing to tell it is over either.
async fn unsnooze(svc: Arc<Service>, _ctx: Ctx, args: ThreadIds) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || {
        for thread in &args.threads {
            vault.release_snooze(*thread)?;
        }
        Ok(())
    })
    .await
}

// ---- drafts --------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewDraft {
    pub account: AccountId,
    #[serde(default)]
    pub in_reply_to: Option<MailMessageId>,
    #[serde(default)]
    pub forward_of: Option<MailMessageId>,
    #[serde(default)]
    pub reply_all: bool,
}

/// A [`ParsedMessage`](mime::ParsedMessage) carrying only what
/// [`compose::quote_html`] actually reads (`from` and
/// `date`), built from the already-decrypted [`Message`] row rather than a
/// second parse of the raw bytes -- the local record already has both
/// fields, in the clear the moment the vault has decrypted it, and asking
/// for the parent's raw bytes here would mean touching the pack store for
/// something `quote_html` was never going to look at.
fn quote_source(parent: &Message) -> mime::ParsedMessage {
    mime::ParsedMessage {
        from: vec![mime::Address {
            name: if parent.from.name.is_empty() { None } else { Some(parent.from.name.clone()) },
            email: Some(parent.from.email.clone()),
        }],
        date: Some(parent.date),
        ..mime::ParsedMessage::default()
    }
}

async fn new_draft(svc: Arc<Service>, _ctx: Ctx, args: NewDraft) -> CommandResult<Draft> {
    let vault = svc.require()?;
    blocking(move || {
        let account = vault.account(args.account)?;
        let mut draft = Draft::new(args.account, account.address.clone(), Origin::Person);

        let own: std::collections::HashSet<String> = std::iter::once(&account.address)
            .chain(account.identities.iter().map(|i| &i.address))
            .map(|a| a.to_lowercase())
            .collect();

        let parent_id = args.in_reply_to.or(args.forward_of);
        if let Some(parent_id) = parent_id {
            let parent = vault.mail_message(parent_id)?;
            let body = vault.body(parent_id)?;
            let quoted = compose::quote_html(&quote_source(&parent), &body.html_sanitised);

            if args.in_reply_to.is_some() {
                draft.in_reply_to = Some(parent_id);
                draft.subject = compose::reply_subject(&parent.subject);
                draft.to = vec![parent.from.clone()];
                if args.reply_all {
                    for addr in parent.to.iter().chain(parent.cc.iter()) {
                        let already =
                            draft.to.iter().any(|a| a.email.eq_ignore_ascii_case(&addr.email));
                        if !already && !own.contains(&addr.email.to_lowercase()) {
                            draft.cc.push(addr.clone());
                        }
                    }
                }
            } else {
                draft.subject = compose::forward_subject(&parent.subject);
            }
            draft.body_html = quoted;
        }

        Ok(draft)
    })
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveDraft {
    pub draft: Draft,
}

/// Save `draft` locally, and enqueue an `AppendDraft` op when this draft has
/// not appended to the server in the last thirty seconds -- the coalescing
/// [`Service::draft_append_due`] decides, since that timer is session
/// state, not a fact the vault write itself can answer.
async fn save_draft(svc: Arc<Service>, _ctx: Ctx, args: SaveDraft) -> CommandResult<()> {
    let mut draft = args.draft;
    draft.updated_at = Timestamp::now();
    let append = svc.draft_append_due(draft.id, draft.updated_at);
    let vault = svc.require()?;
    let op =
        blocking(move || Ok(vault.save_draft_and_append(&draft, append, Origin::Person)?)).await?;
    if let Some(op) = op {
        svc.notify_outbox(op.account_id);
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftRef {
    pub id: DraftId,
}

async fn discard_draft(svc: Arc<Service>, _ctx: Ctx, args: DraftRef) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || {
        vault.discard_draft(args.id)?;
        Ok(())
    })
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendDraft {
    pub id: DraftId,
    /// Undo send's window, five to thirty seconds, clamped by
    /// [`undo_send_delay`]. Ignored when `send_at` is given.
    #[serde(default)]
    pub delay_seconds: Option<u32>,
    /// Send later: queue for a specific, possibly distant, moment instead
    /// of the undo-send window.
    #[serde(default)]
    pub send_at: Option<Timestamp>,
}

async fn send_draft(svc: Arc<Service>, _ctx: Ctx, args: SendDraft) -> CommandResult<Draft> {
    let origin = Origin::Person;
    svc.check_mail_rate_limit(&origin, "person")?;
    let vault = svc.require()?;
    let (draft, op) = blocking(move || {
        let draft = vault.draft(args.id)?;
        if draft.to.is_empty() && draft.cc.is_empty() && draft.bcc.is_empty() {
            return Err(CommandError::new(
                codes::INVALID,
                "a message needs at least one recipient",
            ));
        }
        let not_before =
            args.send_at.unwrap_or_else(|| Timestamp::now() + undo_send_delay(args.delay_seconds));
        Ok(vault.queue_draft_send(args.id, not_before, origin)?)
    })
    .await?;
    svc.notify_outbox(op.account_id);
    Ok(draft)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoSend {
    pub draft_id: DraftId,
}

/// Cancel a queued send, provided its undo window has not closed -- see
/// [`everyday_core::Vault::undo_send`] for exactly when it refuses.
async fn undo_send(svc: Arc<Service>, _ctx: Ctx, args: UndoSend) -> CommandResult<Draft> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.undo_send(args.draft_id, Timestamp::now())?)).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftsQuery {
    pub account: AccountId,
}

async fn list_drafts(svc: Arc<Service>, _ctx: Ctx, args: DraftsQuery) -> CommandResult<Vec<Draft>> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.drafts(args.account)?)).await
}

// ---- responding to a calendar invitation ----------------------------------

/// The three RSVPs `respond_to_invite`'s own client can ask for. A smaller
/// type than [`AttendeeResponse`] on purpose: `needsAction` is a state an
/// attendee's own answer *starts* in, never one a person clicks their way
/// back into, so it is not a legal value on the wire here at all rather than
/// something this handler would have to notice and reject at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InviteResponse {
    Accepted,
    Tentative,
    Declined,
}

impl InviteResponse {
    fn as_attendee_response(self) -> AttendeeResponse {
        match self {
            InviteResponse::Accepted => AttendeeResponse::Accepted,
            InviteResponse::Tentative => AttendeeResponse::Tentative,
            InviteResponse::Declined => AttendeeResponse::Declined,
        }
    }

    /// The subject line's own verb, capitalised -- "Accepted: Standup".
    fn subject_prefix(self) -> &'static str {
        match self {
            InviteResponse::Accepted => "Accepted",
            InviteResponse::Tentative => "Tentative",
            InviteResponse::Declined => "Declined",
        }
    }

    /// The body line's own verb -- "Hari has accepted: Standup".
    fn verb(self) -> &'static str {
        match self {
            InviteResponse::Accepted => "accepted",
            InviteResponse::Tentative => "tentatively accepted",
            InviteResponse::Declined => "declined",
        }
    }
}

/// The bridge from the `respond_to_invite` *tool*'s own answer --
/// [`AttendeeResponse`], the only spelling `everyday-core` can name, since
/// it cannot depend on this crate to borrow [`InviteResponse`] -- onto the
/// one this file's shared body actually wants. Infallible in practice: the
/// tool's own schema offers only `accepted`, `tentative` and `declined`
/// (see `agent::tools::mail::parse_response`), so `NeedsAction` reaching
/// here would mean that schema was bypassed, which this refuses rather than
/// silently answering an invitation nobody asked to answer.
impl TryFrom<AttendeeResponse> for InviteResponse {
    type Error = CommandError;

    fn try_from(response: AttendeeResponse) -> Result<Self, Self::Error> {
        match response {
            AttendeeResponse::Accepted => Ok(InviteResponse::Accepted),
            AttendeeResponse::Tentative => Ok(InviteResponse::Tentative),
            AttendeeResponse::Declined => Ok(InviteResponse::Declined),
            AttendeeResponse::NeedsAction => {
                Err(CommandError::new(codes::INVALID, "needsAction is not an answer"))
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RespondToInvite {
    pub message_id: MailMessageId,
    pub response: InviteResponse,
    #[serde(default)]
    pub comment: Option<String>,
}

/// "Tue 10:00" for a timed event, "Tue, 12 Aug" for an all-day one -- the
/// clause the reply's own human-readable body line ends with. Read in this
/// process's own local zone: the text is generated once, now, and baked
/// into the message body exactly the way `compose::quote_html`'s "On {date}
/// wrote:" line already is, so there is no one zone that would stay correct
/// for every later reader of the thread either.
fn format_when(invite: &everyday_core::mail::Invite) -> String {
    let zoned = invite.start.to_zoned(jiff::tz::TimeZone::system());
    let fmt = if invite.all_day { "%a, %-d %b" } else { "%a %H:%M" };
    jiff::fmt::strtime::format(fmt, &zoned).unwrap_or_else(|_| invite.start.to_string())
}

/// Escapes the five characters that matter inside an HTML text node -- the
/// same small rule `everyday_mail::compose::escape_html` and
/// `everyday_core::mail::compose::escape_html` each keep their own copy of,
/// for a string that is, once again, untrusted: an invitation's own
/// `SUMMARY` and a comment a person typed.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Build and queue an iTIP `REPLY` to the invitation carried by `message_id`,
/// and update that message's own
/// [`everyday_core::mail::Invite::my_response`] so the thread reflects the
/// answer before the reply has gone anywhere.
///
/// Refuses when the message carries no invitation, when its method is
/// `CANCEL` or `REPLY` (nothing to answer), or when none of the account's
/// own addresses were ever invited.
///
/// The shared body behind the `respond_to_invite` command below (a person's
/// own click) and the `respond_to_invite` tool's own
/// [`invite_responder`](everyday_core::agent::tools::ToolContext::invite_responder)
/// hook, wired up in [`respond_to_invite_for_tool`] -- `everyday-core`
/// cannot depend on `everyday-mail` or `calcard`, the crates that actually
/// read a `text/calendar` part and build an iTIP reply, so the tool calls
/// back into this function through a closure rather than duplicating any of
/// it. Synchronous rather than `async` so that both callers can run it
/// directly on whichever blocking context they are already on: the command
/// wraps it in [`blocking`], and the tool's own hook is invoked from inside
/// `tools::dispatch`, which is already running on the blocking pool by the
/// time it reaches here (see `agent::run_tool` and `domains::meta::run_tool`).
///
/// Returns the account and thread this touched rather than notifying the
/// outbox or raising a `Change` itself, because the two callers differ in
/// what `Change::origin` -- the *device* that gets to skip its own reload --
/// should be, which is a property of the caller, not of this write.
fn respond_to_invite_inner(
    vault: &Vault,
    packs: &dyn everyday_core::packstore::PackStore,
    message_id: MailMessageId,
    response: InviteResponse,
    comment: Option<String>,
    origin: Origin,
) -> CommandResult<(AccountId, ThreadId)> {
    let message = vault.mail_message(message_id)?;
    let mut inv = message.invite.clone().ok_or_else(|| {
        CommandError::new(codes::INVALID, "this message carries no calendar invitation")
    })?;
    if matches!(inv.method, InviteMethod::Cancel | InviteMethod::Reply) {
        return Err(CommandError::new(
            codes::INVALID,
            "a cancelled invitation, or another reply, cannot be responded to",
        ));
    }

    let account = vault.account(message.account_id)?;
    let own: BTreeSet<String> = std::iter::once(account.address.clone())
        .chain(account.identities.iter().map(|i| i.address.clone()))
        .map(|a| a.to_lowercase())
        .collect();
    let Some(attendee) =
        inv.attendees.iter().find(|a| own.contains(&a.address.email.to_lowercase()))
    else {
        return Err(CommandError::new(
            codes::INVALID,
            "none of this account's addresses were invited to this event",
        ));
    };
    let responder = attendee.address.clone();

    let raw = packs.read(&message.pack)?;
    let calendar_bytes = mime::parse(&raw).ok().and_then(|p| p.calendar).ok_or_else(|| {
        CommandError::new(codes::INVALID, "this message's calendar part could not be read")
    })?;

    let attendee_response = response.as_attendee_response();
    let ics =
        invite::build_reply(&calendar_bytes, &responder, attendee_response, comment.as_deref())
            .ok_or_else(|| {
                CommandError::new(codes::INVALID, "could not build a reply to this invitation")
            })?;

    let mut draft = Draft::new(message.account_id, account.address.clone(), origin.clone());
    draft.to = vec![inv.organizer.clone()];
    draft.subject = format!("{}: {}", response.subject_prefix(), inv.summary);
    let who = if responder.name.is_empty() { &responder.email } else { &responder.name };
    draft.body_html = format!(
        "<p>{} has {}: {}, {}</p>",
        escape_html(who),
        response.verb(),
        escape_html(&inv.summary),
        escape_html(&format_when(&inv)),
    );
    draft.calendar_part = Some(DraftCalendarPart { method: "REPLY".to_string(), ics });

    vault.save_draft(&draft)?;
    let not_before = Timestamp::now() + undo_send_delay(None);
    vault.queue_draft_send(draft.id, not_before, origin)?;

    inv.my_response = Some(attendee_response);
    vault.set_message_invite(message.id, Some(inv))?;

    Ok((message.account_id, message.thread_id))
}

async fn respond_to_invite(
    svc: Arc<Service>,
    ctx: Ctx,
    args: RespondToInvite,
) -> CommandResult<()> {
    let origin = Origin::Person;
    svc.check_mail_rate_limit(&origin, "person")?;
    let vault = svc.require()?;
    let Some(packs) = svc.packs() else {
        return Err(CommandError::new(codes::INTERNAL, "the mail pack store is not open"));
    };

    let (account_id, thread_id) = blocking(move || {
        respond_to_invite_inner(
            &vault,
            packs.as_ref(),
            args.message_id,
            args.response,
            args.comment,
            origin,
        )
    })
    .await?;

    svc.notify_outbox(account_id);
    svc.events().changed(Change {
        kind: Kind::Thread,
        op: crate::events::Op::Updated,
        id: Some(thread_id.to_string()),
        ids: Vec::new(),
        origin: ctx.caller.origin().map(str::to_string),
    });
    Ok(())
}

/// Builds [`invite_responder`](everyday_core::agent::tools::ToolContext::invite_responder)'s
/// closure: the same [`respond_to_invite_inner`] the command above wraps,
/// called directly rather than through [`blocking`] because the tool
/// catalogue's own caller (`agent::run_tool`, `domains::meta::run_tool`)
/// has already dispatched onto the blocking pool by the time a tool body
/// runs. `Change::origin` is `None` here, not a device id: a tool call has
/// no `Ctx` of its own to read one from, and `None` reads as "no device to
/// spare a reload for", which is exactly right for a write nobody's own
/// open window issued.
///
/// Converts `everyday-core`'s own [`AttendeeResponse`] into this file's
/// [`InviteResponse`] and every error into [`everyday_core::error::Error`],
/// since a hook called from inside the core has to answer in the core's own
/// error type, not this crate's.
pub(crate) fn respond_to_invite_for_tool(
    svc: &Service,
    message_id: MailMessageId,
    response: AttendeeResponse,
    comment: Option<String>,
    origin: Origin,
) -> everyday_core::error::Result<()> {
    fn to_core_error(e: CommandError) -> everyday_core::error::Error {
        everyday_core::error::Error::Invalid(e.message)
    }

    let response: InviteResponse = response.try_into().map_err(to_core_error)?;
    let vault = svc.require().map_err(to_core_error)?;
    let packs = svc
        .packs()
        .ok_or(everyday_core::error::Error::Unsupported("the mail pack store is not open"))?;
    let (account_id, thread_id) =
        respond_to_invite_inner(&vault, packs.as_ref(), message_id, response, comment, origin)
            .map_err(to_core_error)?;

    svc.notify_outbox(account_id);
    svc.events().changed(Change {
        kind: Kind::Thread,
        op: crate::events::Op::Updated,
        id: Some(thread_id.to_string()),
        ids: Vec::new(),
        origin: None,
    });
    Ok(())
}

// ---- what an agent did with mail -------------------------------------------

/// The three kinds of caller `mail_actions_by_origin` can be asked about --
/// deliberately not `Origin::Person`, which is not "an agent" and has no row
/// in Settings → Sharing or the assistant's own settings to be listed under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentOriginKind {
    Mcp,
    Assistant,
    Routine,
}

impl AgentOriginKind {
    /// The clear `ops.origin` column's own spelling -- see
    /// [`everyday_core::mail::Origin::kind`].
    fn as_str(self) -> &'static str {
        match self {
            AgentOriginKind::Mcp => "mcp",
            AgentOriginKind::Assistant => "assistant",
            AgentOriginKind::Routine => "routine",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailActionsByOrigin {
    pub kind: AgentOriginKind,
    #[serde(default = "default_actions_limit")]
    pub limit: u32,
    /// The last page's own final `opId` -- rows strictly after it, in the
    /// same newest-first order. `None` reads as the first page.
    #[serde(default)]
    pub cursor: Option<OpId>,
}

fn default_actions_limit() -> u32 {
    50
}

/// Most rows a page ever hands back.
const MAX_ACTIONS_LIMIT: u32 = 200;

/// The most ops [`MailStore::ops_by_origin`](everyday_core::store::mail::MailStore::ops_by_origin)
/// is ever asked to read for one call to `mail_actions_by_origin`, cursor or
/// not.
///
/// That trait method takes no cursor of its own -- it is one op table shared
/// by every origin kind, ordered newest first, and a keyset cursor over it
/// is exactly the "Changes with ids" groundwork `docs/plans/mail.md`'s
/// phase 0 already generalised for the *thread* list
/// ([`crate::domains::mail::ListThreads::cursor`]), not for this smaller,
/// rarely-paged one. So the cursor is applied here instead: fetch a page
/// generous enough that a settings panel's list almost never needs a
/// second round trip, and find `cursor`'s own row in it by a linear scan.
/// A vault with more than this many pending or recent ops from one kind of
/// caller in flight at once has a problem this list is not the fix for.
const ACTIONS_FETCH_CAP: u32 = 500;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailActionByOrigin {
    pub op_id: OpId,
    pub kind: String,
    pub state: String,
    pub at: Timestamp,
    pub account: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ThreadId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The MCP client this op's own sealed [`Origin`] names -- `Some` only
    /// for `kind: "mcp"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    /// The conversation this op's own sealed `Origin` names -- `Some` only
    /// for `kind: "assistant"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    /// The routine run this op's own sealed `Origin` names -- `Some` only
    /// for `kind: "routine"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// What thread, and what to call it, `op` was actually about -- read off
/// whichever record its own [`OpTarget`] names. `None` for either half when
/// the record it would explain has since been deleted; a settings list
/// draws that as a plain, subjectless row rather than failing the whole
/// page over one stale op.
fn subject_of(vault: &Vault, op: &Op) -> (Option<ThreadId>, Option<String>) {
    match op.target {
        OpTarget::Thread(id) => match vault.thread(id) {
            Ok((thread, _)) => (Some(id), Some(thread.subject)),
            Err(_) => (Some(id), None),
        },
        OpTarget::Message(id) => match vault.mail_message(id) {
            Ok(message) => (Some(message.thread_id), Some(message.subject)),
            Err(_) => (None, None),
        },
        OpTarget::Draft(id) => match vault.draft(id) {
            Ok(draft) => {
                let thread_id =
                    draft.in_reply_to.and_then(|m| vault.mail_message(m).ok()).map(|m| m.thread_id);
                let subject = (!draft.subject.trim().is_empty()).then_some(draft.subject);
                (thread_id, subject)
            }
            Err(_) => (None, None),
        },
    }
}

/// What each connected MCP client, or the assistant, has done with mail --
/// `docs/plans/mail.md`'s risk table: "Settings → Sharing lists what each
/// MCP client did through `origin`." Reads
/// [`MailStore::ops_by_origin`](everyday_core::store::mail::MailStore::ops_by_origin)
/// for the one `kind` asked about, newest first, and resolves each op's own
/// thread or draft into a subject a person recognises rather than an id
/// they would have to look up.
async fn mail_actions_by_origin(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: MailActionsByOrigin,
) -> CommandResult<Vec<MailActionByOrigin>> {
    let vault = svc.require()?;
    let limit = args.limit.clamp(1, MAX_ACTIONS_LIMIT) as usize;
    blocking(move || {
        let ops = vault.ops_by_origin(args.kind.as_str(), ACTIONS_FETCH_CAP)?;
        let after_cursor = match args.cursor {
            None => 0,
            // A cursor naming a row no longer in the window (it fell off
            // the fetch cap, or was cleaned up) reads as "nothing more" --
            // the same "cannot page past a gone id" the keyset cursors
            // elsewhere in this crate already accept, rather than a hard
            // error over a settings panel scrolling a little further.
            Some(cursor) => match ops.iter().position(|op| op.id == cursor) {
                Some(idx) => idx + 1,
                None => ops.len(),
            },
        };

        let mut out = Vec::with_capacity(limit.min(ops.len()));
        for op in ops.into_iter().skip(after_cursor).take(limit) {
            let account = vault.account(op.account_id).map(|a| a.address).unwrap_or_default();
            let (thread_id, subject) = subject_of(&vault, &op);
            let (client, conversation, run) = match &op.origin {
                Origin::Person => (None, None, None),
                Origin::Assistant { conversation } => (None, Some(conversation.clone()), None),
                Origin::Routine { run } => (None, None, Some(run.clone())),
                Origin::Mcp { client } => (Some(client.clone()), None, None),
            };
            out.push(MailActionByOrigin {
                op_id: op.id,
                kind: op.origin.kind().to_string(),
                state: op.state.as_str().to_string(),
                at: op.updated_at,
                account,
                thread_id,
                subject,
                client,
                conversation,
                run,
                last_error: op.last_error,
            });
        }
        Ok(out)
    })
    .await
}

// ---- categorisation --------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetThreadCategory {
    pub threads: Vec<ThreadId>,
    pub category: Category,
}

/// Set `category` on every thread named, and record it as a standing
/// correction for each distinct sender among them — see
/// `everyday_core::vault::mail::Vault::correct_mail_category` for what
/// "record" actually does, including the sweep across that sender's other
/// threads the plan asks for ("as a batch, to that sender's existing
/// threads"). One correction per distinct `(account, sender)` pair among the
/// given threads, derived from each thread's own last message — the plan's
/// contract takes only a category, not a sender, so the sender a person
/// meant is the one the thread they clicked on actually shows.
async fn set_thread_category(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SetThreadCategory,
) -> CommandResult<()> {
    let vault = svc.require()?;
    blocking(move || {
        let mut senders: std::collections::BTreeMap<AccountId, BTreeSet<String>> =
            std::collections::BTreeMap::new();
        for &thread_id in &args.threads {
            let (thread, messages) = vault.thread(thread_id)?;
            let Some(last) = messages.last() else { continue };
            senders.entry(thread.account_id).or_default().insert(last.from.email.clone());
        }
        for (account_id, addresses) in senders {
            for address in addresses {
                vault.correct_mail_category(account_id, &address, args.category)?;
            }
        }
        Ok(())
    })
    .await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecategorizeMail {
    /// One account, or every mail-enabled account when omitted.
    #[serde(default)]
    pub account: Option<AccountId>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecategorizeResult {
    /// How many messages' category actually changed.
    pub changed: u32,
}

/// The one-off backfill: re-run the rules (and any correction already on
/// file) over mail that was ingested before those rules, or that correction,
/// existed. No `change:` on the command entry below — a backfill can touch
/// thousands of threads across every mailbox, and naming every one of them
/// on the wire would cost more than the refresh a person can ask for by hand
/// is worth.
async fn recategorize_mail(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: RecategorizeMail,
) -> CommandResult<RecategorizeResult> {
    let vault = svc.require()?;
    let changed = blocking(move || {
        let accounts = match args.account {
            Some(id) => vec![vault.account(id)?],
            None => vault.accounts()?,
        };
        let mut total = 0u32;
        for account in accounts.into_iter().filter(|a| a.services.mail) {
            total += vault.recategorize_mail(account.id)?;
        }
        Ok(total)
    })
    .await?;
    Ok(RecategorizeResult { changed })
}

// ---- summaries --------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummarizeThreadArgs {
    pub id: ThreadId,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    pub summary: String,
}

/// `summarize_thread`'s whole body is `everyday_service::mailai::summarize_thread`
/// — the gate, the cache and the model call all live there, beside the other
/// two model-assisted features, rather than here with the plain reads and
/// writes this file is otherwise made of.
async fn summarize_thread(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: SummarizeThreadArgs,
) -> CommandResult<ThreadSummary> {
    let summary = crate::mailai::summarize_thread(&svc, args.id).await?;
    Ok(ThreadSummary { summary })
}

async fn list_mailboxes(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: MailboxesQuery,
) -> CommandResult<Vec<Mailbox>> {
    let vault = svc.require()?;
    blocking(move || Ok(vault.mailboxes(args.account)?)).await
}

async fn list_threads(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ListThreads,
) -> CommandResult<ThreadPage> {
    let vault = svc.require()?;
    blocking(move || {
        Ok(vault.list_threads(args.mailbox, &args.filter, args.cursor.as_deref(), args.limit)?)
    })
    .await
}

async fn get_thread(svc: Arc<Service>, _ctx: Ctx, args: ThreadRef) -> CommandResult<ThreadDetail> {
    let vault = svc.require()?;
    blocking(move || {
        let (thread, messages) = vault.thread(args.id)?;
        Ok(ThreadDetail { thread, messages })
    })
    .await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_mailboxes", scope: Mail, effect: Read,
        args: MailboxesQuery, returns: "Mailbox[]",
        signature: &[("account", "AccountId", true)],
        run: list_mailboxes,
    },
    command! {
        name: "list_threads", scope: Mail, effect: Read,
        args: ListThreads, returns: "ThreadPage",
        signature: &[
            ("mailbox", "MailboxId", true),
            ("filter", "ThreadFilter", false),
            ("cursor", "string | null", false),
            ("limit", "number | null", false),
        ],
        run: list_threads,
    },
    command! {
        name: "get_thread", scope: Mail, effect: Read,
        args: ThreadRef, returns: "ThreadDetail",
        signature: &[("id", "ThreadId", true)],
        run: get_thread,
    },
    // ---- batch thread actions --------------------------------------------
    command! {
        name: "mark_read", scope: Mail, effect: Write,
        change: Thread/Updated,
        ids: |a: &ThreadIds| a.threads.iter().map(|t| t.to_string()).collect(),
        args: ThreadIds, returns: "Op[]",
        signature: &[("threads", "ThreadId[]", true)],
        run: mark_read,
    },
    command! {
        name: "mark_unread", scope: Mail, effect: Write,
        change: Thread/Updated,
        ids: |a: &ThreadIds| a.threads.iter().map(|t| t.to_string()).collect(),
        args: ThreadIds, returns: "Op[]",
        signature: &[("threads", "ThreadId[]", true)],
        run: mark_unread,
    },
    command! {
        name: "star", scope: Mail, effect: Write,
        change: Thread/Updated,
        ids: |a: &ThreadIds| a.threads.iter().map(|t| t.to_string()).collect(),
        args: ThreadIds, returns: "Op[]",
        signature: &[("threads", "ThreadId[]", true)],
        run: star,
    },
    command! {
        name: "unstar", scope: Mail, effect: Write,
        change: Thread/Updated,
        ids: |a: &ThreadIds| a.threads.iter().map(|t| t.to_string()).collect(),
        args: ThreadIds, returns: "Op[]",
        signature: &[("threads", "ThreadId[]", true)],
        run: unstar,
    },
    command! {
        name: "archive", scope: Mail, effect: Write,
        change: Thread/Updated,
        ids: |a: &ThreadIds| a.threads.iter().map(|t| t.to_string()).collect(),
        args: ThreadIds, returns: "Op[]",
        signature: &[("threads", "ThreadId[]", true)],
        run: archive,
    },
    command! {
        name: "trash", scope: Mail, effect: Write,
        change: Thread/Updated,
        ids: |a: &ThreadIds| a.threads.iter().map(|t| t.to_string()).collect(),
        args: ThreadIds, returns: "Op[]",
        signature: &[("threads", "ThreadId[]", true)],
        run: trash,
    },
    command! {
        name: "move_to_mailbox", scope: Mail, effect: Write,
        change: Thread/Updated,
        ids: |a: &MoveThreads| a.threads.iter().map(|t| t.to_string()).collect(),
        args: MoveThreads, returns: "Op[]",
        signature: &[("threads", "ThreadId[]", true), ("to", "MailboxId", true)],
        run: move_to_mailbox,
    },
    command! {
        name: "label", scope: Mail, effect: Write,
        change: Thread/Updated,
        ids: |a: &LabelThreads| a.threads.iter().map(|t| t.to_string()).collect(),
        args: LabelThreads, returns: "Op[]",
        signature: &[("threads", "ThreadId[]", true), ("label", "string", true)],
        run: label,
    },
    command! {
        name: "unlabel", scope: Mail, effect: Write,
        change: Thread/Updated,
        ids: |a: &LabelThreads| a.threads.iter().map(|t| t.to_string()).collect(),
        args: LabelThreads, returns: "Op[]",
        signature: &[("threads", "ThreadId[]", true), ("label", "string", true)],
        run: unlabel,
    },
    command! {
        name: "snooze", scope: Mail, effect: Write,
        change: Thread/Updated,
        ids: |a: &SnoozeThreads| a.threads.iter().map(|t| t.to_string()).collect(),
        args: SnoozeThreads, returns: "Op[]",
        signature: &[("threads", "ThreadId[]", true), ("until", "string", true)],
        run: snooze,
    },
    command! {
        name: "unsnooze", scope: Mail, effect: Write,
        change: Thread/Updated,
        ids: |a: &ThreadIds| a.threads.iter().map(|t| t.to_string()).collect(),
        args: ThreadIds, returns: "void",
        signature: &[("threads", "ThreadId[]", true)],
        run: unsnooze,
    },
    // ---- drafts -----------------------------------------------------------
    command! {
        name: "new_draft", scope: Mail, effect: Write,
        change: Draft/Created,
        args: NewDraft, returns: "Draft",
        signature: &[
            ("account", "AccountId", true),
            ("inReplyTo", "MailMessageId | null", false),
            ("forwardOf", "MailMessageId | null", false),
            ("replyAll", "boolean | null", false),
        ],
        run: new_draft,
    },
    command! {
        name: "save_draft", scope: Mail, effect: Write,
        change: Draft/Updated,
        id: |a: &SaveDraft| Some(a.draft.id.to_string()),
        args: SaveDraft, returns: "void",
        signature: &[("draft", "Draft", true)],
        run: save_draft,
    },
    command! {
        name: "discard_draft", scope: Mail, effect: Write,
        change: Draft/Updated,
        id: |a: &DraftRef| Some(a.id.to_string()),
        args: DraftRef, returns: "void",
        signature: &[("id", "DraftId", true)],
        run: discard_draft,
    },
    command! {
        name: "send_draft", scope: Mail, effect: Write,
        change: Draft/Updated,
        id: |a: &SendDraft| Some(a.id.to_string()),
        args: SendDraft, returns: "Draft",
        signature: &[
            ("id", "DraftId", true),
            ("delaySeconds", "number | null", false),
            ("sendAt", "string | null", false),
        ],
        run: send_draft,
    },
    command! {
        name: "undo_send", scope: Mail, effect: Write,
        change: Draft/Updated,
        id: |a: &UndoSend| Some(a.draft_id.to_string()),
        args: UndoSend, returns: "Draft",
        signature: &[("draftId", "DraftId", true)],
        run: undo_send,
    },
    command! {
        name: "list_drafts", scope: Mail, effect: Read,
        args: DraftsQuery, returns: "Draft[]",
        signature: &[("account", "AccountId", true)],
        run: list_drafts,
    },
    // ---- calendar invitations ---------------------------------------------
    command! {
        name: "respond_to_invite", scope: Mail, effect: Write,
        args: RespondToInvite, returns: "void",
        // `response` is really `'accepted' | 'tentative' | 'declined'` on the
        // wire -- see `InviteResponse`'s own `Deserialize` -- but `gen-api.mjs`
        // resolves every identifier a signature string names against
        // `ui/src/lib/types.ts`, which this crate does not write to (see
        // `docs/plans/mail.md`'s phase 6 split of labour). `string` here is
        // what keeps codegen working without a named union type only the UI
        // side can add; nothing about validation changes; a bad value is
        // still refused by serde before this command's body ever runs.
        signature: &[
            ("messageId", "MailMessageId", true),
            ("response", "string", true),
            ("comment", "string | null", false),
        ],
        run: respond_to_invite,
    },
    // ---- categorisation and summaries --------------------------------------
    command! {
        name: "set_thread_category", scope: Mail, effect: Write,
        change: Thread/Updated,
        ids: |a: &SetThreadCategory| a.threads.iter().map(|t| t.to_string()).collect(),
        args: SetThreadCategory, returns: "void",
        signature: &[("threads", "ThreadId[]", true), ("category", "MailCategory", true)],
        run: set_thread_category,
    },
    command! {
        name: "recategorize_mail", scope: Mail, effect: Write,
        args: RecategorizeMail, returns: "RecategorizeResult",
        signature: &[("account", "AccountId | null", false)],
        run: recategorize_mail,
    },
    command! {
        name: "summarize_thread", scope: Mail, effect: Read,
        args: SummarizeThreadArgs, returns: "ThreadSummary",
        signature: &[("id", "ThreadId", true)],
        run: summarize_thread,
    },
    // ---- what an agent did with mail ---------------------------------------
    command! {
        name: "mail_actions_by_origin", scope: Mail, effect: Read,
        args: MailActionsByOrigin, returns: "MailActionByOrigin[]",
        // `kind` is really `'mcp' | 'assistant' | 'routine'` on the wire --
        // see `AgentOriginKind`'s own `Deserialize` -- kept as `string` here
        // for the same `gen-api.mjs` reason `respond_to_invite`'s own
        // `response` field is above.
        signature: &[
            ("kind", "string", true),
            ("limit", "number | null", false),
            ("cursor", "string | null", false),
        ],
        run: mail_actions_by_origin,
    },
];
