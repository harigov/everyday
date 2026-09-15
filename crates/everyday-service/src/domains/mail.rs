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
use crate::service::{Service, blocking};
use everyday_core::id::{AccountId, DraftId, MailMessageId, MailboxId, ThreadId};
use everyday_core::mail::{
    Category, Draft, Mailbox, Message, Op, OpKind, Origin, Thread, undo_send_delay,
};
use everyday_core::store::mail::{ThreadFilter, ThreadPage};
use everyday_mail::{compose, mime};
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
];
