//! Draining one account's outbox: the seam between
//! [`everyday_mail::outbox::execute`], which knows how to run one [`Op`]
//! against a live session, and the vault, which is the only place that
//! knows what a thread, a message or a draft actually is. Nothing in
//! `everyday-mail` imports a vault -- see that crate's `outbox` module docs
//! -- so this is where the two meet: [`VaultLookups`] answers
//! `everyday_mail::outbox::Lookups` by reading the vault, and
//! [`drain_outbox`] is the loop that fetches due ops, runs each one, and
//! writes back what happened.
//!
//! # How the sync agent should call this
//!
//! Three triggers, all cheap to get wrong in the same direction -- calling
//! too rarely, which is a slow inbox, not a bug:
//!
//! 1. **After connecting**, and **after every `IDLE` wake.** A
//!    `MailSession::idle` return is already the sync engine's cue to resync
//!    the mailbox; draining the outbox in the same breath means an op that
//!    has been sitting since the last wake goes out immediately rather than
//!    waiting for its own timer. `crate::mailsync::task::run_account_with`
//!    places its own call to [`drain_outbox`] at the top of its loop, which
//!    is both of these moments at once: the loop's first turn follows
//!    connecting directly, and every later turn follows an `IDLE` wake.
//! 2. **Whenever [`Service::outbox_notify`] fires.** A write command's
//!    [`Service::notify_outbox`] wakes the very `tokio::select!` an account
//!    task is almost certainly already blocked in between an `IDLE` and its
//!    next poll, so undo send's countdown and an archive's confirmation do
//!    not wait for either trigger above to come around on its own.
//! 3. **On a short timer while ops are pending.** [`DrainReport::pending`]
//!    says whether this call's `due_ops` page came back full -- the account
//!    task keeps calling [`drain_outbox`] back-to-back for as long as it
//!    does, then falls back to a slower timer (a few seconds is plenty;
//!    `not_before` is what actually paces retries and undo-send) once it
//!    comes back short. See `crate::mailsync::task::drain_until_caught_up`.
//!
//! # Recovering from a crash mid-drain
//!
//! [`OpState::InFlight`] means "a drain claimed this op and has not yet
//! said how it went" -- ordinarily a moment inside one loop iteration of
//! [`drain_outbox`], between the `transition_to(InFlight)` near its top and
//! the `Done`/`Pending`/`Failed` at its bottom. A crash, or the process
//! simply being killed, in between leaves an op sitting there for good --
//! nothing else ever moves an op *out* of `InFlight`, so without recovery
//! it would wait forever for a drain that already happened.
//! [`recover_inflight_ops`], called once by
//! `crate::mailsync::task::run_account_with` before its very first drain,
//! is that recovery: every `InFlight` op for the account goes back to
//! `Pending`. A `Send` op gets one extra check, since the crash could have
//! happened *after* SMTP already accepted the message -- see
//! [`already_sent`].
//!
//! # A draft's server copy, and a reply's `References` chain
//!
//! Both used to be approximated here -- a stale server copy remembered only
//! in this process's memory, and a reply's `In-Reply-To`/`References`
//! synthesised from a decrypted [`Message`] row rather than read from the
//! parent's genuine bytes -- and both are now exact: [`Draft::server_copy`]
//! persists the first, on the draft itself, so a restart still finds the
//! stale copy to delete; [`VaultLookups::parent_raw`] reads the second
//! straight out of the pack store `crate::mailsync::wiring` opens, the same
//! bytes the parent was originally ingested from.

use std::sync::Arc;
use std::time::Duration;

use everyday_core::id::{AccountId, BlobId, DraftId, MailMessageId, MailboxId, OpId, ThreadId};
use everyday_core::mail::{Draft, DraftState, MailboxRole, Op, OpKind, OpState, OpTarget};
use everyday_mail::outbox::{
    ExecContext, Executed, Located, Lookups, Sender, execute, is_retryable,
};
use everyday_mail::session::{MailError, MailSession};

use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};

/// How many due ops one [`drain_outbox`] call fetches and runs. Bounded so
/// one call cannot hold the vault's writer for an unbounded backlog; the
/// module docs' calling convention is what lets the account task simply
/// call again when [`DrainReport::pending`] says there is more.
const DRAIN_BATCH: u32 = 25;

/// This outbox's own schedule, as a [`crate::retry::RetryPolicy`] -- wraps
/// [`everyday_core::mail::backoff_for_attempt`] exactly, rather than
/// re-deriving its table here: per phase 9.3 of
/// `docs/plans/architecture-refactor.md`, "the table lives in core... don't
/// move it". Unlimited attempts, the same as the function it wraps: a
/// retryable op keeps its `not_before` growing, capped at the table's last
/// entry, until it succeeds, is cancelled, or fails for a reason that was
/// never retryable to begin with.
fn outbox_table() -> crate::retry::RetryPolicy {
    crate::retry::RetryPolicy::from_fn(
        |attempts: u32| {
            Duration::try_from(everyday_core::mail::backoff_for_attempt(attempts))
                .expect("RETRY_BACKOFF's entries are all positive durations")
        },
        None,
    )
}

/// What one [`drain_outbox`] pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrainReport {
    /// Ops this pass ran, whatever the outcome.
    pub attempted: u32,
    /// Ops that reached [`OpState::Done`].
    pub done: u32,
    /// Ops that failed with a retryable error and went back to
    /// [`OpState::Pending`] with a later `not_before`.
    pub retried: u32,
    /// Ops that failed permanently: [`OpState::Failed`]`{ permanent: true }`,
    /// with the optimistic local change reversed.
    pub failed: u32,
    /// `true` when this pass's `due_ops` page came back at [`DRAIN_BATCH`] --
    /// a hint, not a promise, that calling again immediately would find
    /// more due work rather than an empty page.
    pub pending: bool,
}

/// Fetch `account`'s due ops and run each one against `session` and
/// `sender`, in order. See the module docs for exactly when the sync
/// agent's per-account task should call this.
pub async fn drain_outbox<S, T>(
    svc: &Arc<Service>,
    account: AccountId,
    session: &mut S,
    sender: &T,
) -> CommandResult<DrainReport>
where
    S: MailSession,
    T: Sender,
{
    let vault = svc.require()?;
    let now = svc.now();
    let ops = blocking({
        let vault = vault.clone();
        move || Ok(vault.due_ops(account, now, DRAIN_BATCH)?)
    })
    .await?;

    let mut report = DrainReport { pending: ops.len() as u32 == DRAIN_BATCH, ..Default::default() };
    let lookups = VaultLookups {
        svc: svc.clone(),
        account,
        gmail: session.capabilities().gmail,
        packs: svc.packs(),
    };

    for mut op in ops {
        report.attempted += 1;

        op.transition_to(OpState::InFlight)?;
        persist_op(&vault, &op).await?;

        let mut ctx = ExecContext { session, sender, lookups: &lookups };
        match execute(&op, &mut ctx).await {
            Ok(executed) => {
                // The op's own state is durable *before* `on_success`'s
                // follow-up writes run, not after: a `Send` or `AppendDraft`
                // that reached the server has done the one thing this op
                // promises, and a failure in `on_success` (the draft's own
                // `save_draft`, say, racing a lock the person's compose
                // window holds) must not leave the op stuck `InFlight` --
                // see the module docs' recovery path for what that would
                // otherwise cost on the very next restart.
                op.transition_to(OpState::Done)?;
                persist_op(&vault, &op).await?;
                report.done += 1;
                if let Err(e) = on_success(svc, &vault, &op, executed).await {
                    tracing::warn!(
                        error = %e,
                        op = %op.id,
                        "a mail op completed but its own follow-up write failed"
                    );
                }
            }
            Err(MailError::Auth(reason)) => {
                // The credential this op just tried is no longer good --
                // not a reason to give up on the op itself. A permanently
                // failed `Send` reverses the draft back to `Editing` (see
                // `on_permanent_failure`), which is wrong here: the person
                // already asked for this to be sent, and the moment they
                // sign back in it should simply go, not need asking again.
                // So the op stays alive, `Pending` with the ordinary
                // backoff, while the account itself is moved to
                // `NeedsSignIn` -- the same state a credential failure at
                // connect time already produces, just reached from deeper
                // in an already-open session. See `crate::mailsync::sender`
                // for the one retry-with-a-fresh-token this crate allows
                // itself before ever surfacing `Auth` at all.
                //
                // `outbox_table`'s delay is read *before* `attempts` is
                // bumped -- it wraps `backoff_for_attempt`, documented as
                // 0-indexed ("the first retry, after attempt 0 failed,
                // waits `RETRY_BACKOFF[0]`"), so reading it after
                // incrementing would make the very first retry wait the
                // *second* schedule entry (a minute) instead of the first
                // (thirty seconds), and every later retry one step further
                // out than the schedule promises.
                let backoff = outbox_table().delay_for(op.attempts);
                op.attempts += 1;
                op.last_error = Some(reason.clone());
                op.not_before = svc.now() + backoff;
                op.transition_to(OpState::Pending)?;
                persist_op(&vault, &op).await?;
                mark_account_needs_sign_in(svc, &vault, account, &reason).await;
                report.retried += 1;
            }
            Err(err) if is_retryable(&err) => {
                // See the `Auth` arm above for why the backoff is read
                // before `attempts` is bumped.
                let backoff = outbox_table().delay_for(op.attempts);
                op.attempts += 1;
                op.last_error = Some(err.to_string());
                op.not_before = svc.now() + backoff;
                op.transition_to(OpState::Pending)?;
                persist_op(&vault, &op).await?;
                report.retried += 1;
            }
            Err(err) => {
                op.attempts += 1;
                let message = err.to_string();
                op.last_error = Some(message.clone());
                op.transition_to(OpState::Failed { permanent: true, message })?;
                persist_op(&vault, &op).await?;
                if let Err(e) = on_permanent_failure(svc, &vault, &op).await {
                    // A permanent failure's own reversal is best-effort on
                    // the same reasoning `on_success`'s follow-up write
                    // already is, a few lines above: the op itself is
                    // already durable, and a local storage hiccup while
                    // reverting it must never propagate up through this
                    // `?` and take the whole account task down with it --
                    // see this function's own module docs, and
                    // `on_permanent_failure`'s, for exactly the crash this
                    // used to be.
                    tracing::warn!(
                        error = %e,
                        op = %op.id,
                        "a mail op failed permanently but reverting its local effect failed too"
                    );
                }
                report.failed += 1;
            }
        }
    }

    Ok(report)
}

/// Ops of `account`'s left [`OpState::InFlight`] by a crash mid-drain --
/// called once, before the account task's very first
/// [`drain_outbox`], so nothing sits waiting forever for a drain that
/// already happened and will not come round again on its own; nothing else
/// ever moves an op out of `InFlight`.
///
/// Every one goes back to [`OpState::Pending`] with `attempts` bumped, as
/// if it had failed once -- which, in effect, it did: whatever ran it never
/// got to say how it went. A [`OpKind::Send`] gets one extra check first,
/// through [`already_sent`]: the crash could have happened *after* SMTP
/// already accepted the message and before this process recorded that, in
/// which case simply re-queuing it would send it twice.
pub async fn recover_inflight_ops<S: MailSession>(
    svc: &Arc<Service>,
    account: AccountId,
    session: &mut S,
) -> CommandResult<()> {
    let vault = svc.require()?;
    let stranded = blocking({
        let vault = vault.clone();
        move || Ok(vault.in_flight_ops(account)?)
    })
    .await?;

    for mut op in stranded {
        if let OpTarget::Draft(id) = op.target
            && matches!(op.kind, OpKind::Send)
            && already_sent(&vault, session, id).await?
        {
            op.transition_to(OpState::Done)?;
            persist_op(&vault, &op).await?;
            if let Err(e) = mark_draft_sent(svc, &vault, id).await {
                tracing::warn!(
                    error = %e,
                    op = %op.id,
                    "a recovered send was found already on the server, but marking its draft sent failed"
                );
            }
            continue;
        }

        op.attempts += 1;
        op.last_error = Some("recovered after an interrupted drain".into());
        op.transition_to(OpState::Pending)?;
        persist_op(&vault, &op).await?;
    }

    reconcile_stranded_drafts(svc, &vault, account).await
}

/// The other half of recovering from a crash mid-drain, alongside
/// [`recover_inflight_ops`]'s own `InFlight` sweep just above: an op is
/// persisted [`OpState::Done`] (or `Failed { permanent: true }`) *before*
/// its own follow-up write runs -- see this module's docs on why, and
/// [`on_success`] and [`on_permanent_failure`] for those follow-up writes
/// themselves -- so a crash in that gap leaves a draft still
/// [`DraftState::Queued`] naming an op that has already, genuinely,
/// finished. Nothing else ever moves a draft out of `Queued` again:
/// [`everyday_core::Vault::queue_draft_send`] refuses a draft that is not
/// `Editing`, and [`everyday_core::Vault::undo_send`] refuses one whose op
/// is not `Pending`, so without this a restarted account task leaves the
/// compose window showing "sending…" forever, with no send, no cancel and
/// no retry able to touch it.
///
/// Called once per account, right alongside [`recover_inflight_ops`]'s own
/// recovery (same call site, same crash), over every draft the account
/// has -- there is no cheaper way to find "drafts named by a since-finished
/// op" than by walking from the draft side, since an [`Op`] does not itself
/// know whether the draft that named it has already moved on.
async fn reconcile_stranded_drafts(
    svc: &Arc<Service>,
    vault: &Arc<everyday_core::Vault>,
    account: AccountId,
) -> CommandResult<()> {
    let vault_for_list = vault.clone();
    let drafts = blocking(move || Ok(vault_for_list.drafts(account)?)).await?;

    for draft in drafts {
        let DraftState::Queued { op: op_id } = draft.state else { continue };
        let vault_for_op = vault.clone();
        let op = match blocking(move || Ok(vault_for_op.op(op_id)?)).await {
            Ok(op) => op,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    draft = %draft.id,
                    op = %op_id,
                    "could not read a queued draft's own op while reconciling a crash"
                );
                continue;
            }
        };
        let outcome = match op.state {
            OpState::Done => mark_draft_sent(svc, vault, draft.id).await,
            OpState::Cancelled | OpState::Failed { permanent: true, .. } => {
                revert_queued_send(svc, vault, draft.id, op_id).await
            }
            // `Pending` and `InFlight` -- genuinely still on its way, the
            // latter already handled by the `InFlight` sweep above this
            // call -- and `Failed { permanent: false }`, which goes back to
            // `Pending` on its own next retry: the draft is right to still
            // say `Queued` in every one of these.
            OpState::Pending | OpState::InFlight | OpState::Failed { permanent: false, .. } => {
                continue;
            }
        };
        if let Err(e) = outcome {
            tracing::warn!(
                error = %e,
                draft = %draft.id,
                op = %op_id,
                "could not reconcile a draft stranded `Queued` by a crash"
            );
        }
    }
    Ok(())
}

/// Whether a recovered `Send` op's draft has already reached the server --
/// asked of the server itself, via [`MailSession::search_message_id`],
/// because local state is exactly what a crash mid-drain cannot be trusted
/// to answer this from. Tried against every mailbox
/// [`everyday_mail::outbox::search_roles`] names for this account, in
/// order, stopping at the first hit -- see that function's own docs for why
/// Sent alone is not enough off Gmail: the only thing that ever puts a sent
/// copy there on a plain IMAP account is this crate's own Sent-append,
/// which runs *after* the send, so a crash inside that gap leaves Sent
/// empty even though the message went out. `false` -- "not found, or could
/// not check" -- is the conservative answer either way: it sends again,
/// which duplicates a message rather than silently dropping one, and a
/// duplicate is the smaller mistake. A draft with no `message_id` yet was
/// never actually built by [`everyday_mail::outbox::execute`] before the
/// crash, so there is nothing a search could find; `false` without asking.
async fn already_sent<S: MailSession>(
    vault: &Arc<everyday_core::Vault>,
    session: &mut S,
    draft_id: DraftId,
) -> CommandResult<bool> {
    let vault_for_draft = vault.clone();
    let draft = blocking(move || Ok(vault_for_draft.draft(draft_id)?)).await?;
    let Some(message_id) = draft.message_id else { return Ok(false) };

    for &role in everyday_mail::outbox::search_roles(session.capabilities().gmail) {
        let Some(mailbox_name) = special_use(vault, draft.account_id, role).await? else {
            continue;
        };
        match session.search_message_id(&mailbox_name, &message_id).await {
            Ok(Some(_)) => return Ok(true),
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    mailbox = %mailbox_name,
                    "could not check whether a recovered send already reached the server; \
                     trying the next mailbox"
                );
                continue;
            }
        }
    }
    Ok(false)
}

async fn persist_op(vault: &Arc<everyday_core::Vault>, op: &Op) -> CommandResult<()> {
    let vault = vault.clone();
    let op = op.clone();
    blocking(move || Ok(vault.update_op(&op)?)).await
}

/// Move `account`'s own record to [`everyday_core::account::AccountStatus::NeedsSignIn`]
/// after an op-level `Auth` failure -- the drain loop's own counterpart to
/// `crate::mailsync::task`'s connect-time handling of the same error, reached
/// from deeper inside an already-open session instead. Errors loading the
/// account are swallowed: the op itself has already been kept alive by the
/// caller regardless, and there is nothing more useful to do with a vault
/// read failing here than there would be anywhere else in this best-effort
/// path.
async fn mark_account_needs_sign_in(
    svc: &Arc<Service>,
    vault: &Arc<everyday_core::Vault>,
    account: AccountId,
    reason: &str,
) {
    svc.token_cache().forget(&account.to_string()).await;
    let vault_for_account = vault.clone();
    let loaded = blocking(move || Ok(vault_for_account.account(account)?)).await;
    if let Ok(acct) = loaded {
        crate::mailsync::credential::mark_needs_sign_in(vault, &acct, reason, svc.now());
    }
}

/// What a successfully executed op does beyond marking itself `Done`:
/// `Send` marks the draft sent and, since the contact index only ever
/// learns "sent to" from a `Sent`-role mailbox the sync engine may not get
/// to for a while, records its recipients right away too. `AppendDraft`
/// remembers where the fresh copy landed.
async fn on_success(
    svc: &Arc<Service>,
    vault: &Arc<everyday_core::Vault>,
    op: &Op,
    executed: Executed,
) -> CommandResult<()> {
    match executed {
        Executed::Ok => Ok(()),
        Executed::Sent { sent_append_error, .. } => {
            let OpTarget::Draft(id) = op.target else {
                return Ok(()); // guarded by `everyday_mail::outbox::send`'s own contract
            };
            if let Some(note) = sent_append_error {
                // The message itself is already gone by the time
                // `everyday_mail::outbox::send` hands this back -- see that
                // function's own docs for why this is a log line, not a
                // failure.
                tracing::warn!(op = %op.id, error = %note, "sent, but the Sent copy was not appended");
            }
            mark_draft_sent(svc, vault, id).await
        }
        Executed::Appended { uid: Some(uid) } => {
            let OpTarget::Draft(id) = op.target else { return Ok(()) };
            let Some(drafts_mailbox) =
                special_use(vault, op.account_id, MailboxRole::Drafts).await?
            else {
                return Ok(()); // nothing to remember without a mailbox name to pair the uid with
            };
            let vault = vault.clone();
            blocking(move || {
                // `Vault::with_draft`, not a bare read-modify-save: only
                // `server_copy` is this write's business, but `AppendDraft`
                // runs on the account task's own schedule, entirely
                // separately from a person still typing, and a
                // read-modify-save racing their own debounced autosave
                // used to be able to win with a draft whose `server_copy`
                // was still the *previous* value -- see `Vault::with_draft`'s
                // own docs.
                vault.with_draft(id, |draft| {
                    draft.server_copy =
                        Some(everyday_core::mail::DraftServerCopy { mailbox: drafts_mailbox, uid });
                    true
                })?;
                Ok(())
            })
            .await
        }
        // No `APPENDUID`: this crate cannot name the fresh copy's uid, so
        // the next `AppendDraft` will not find a `draft_server_copy` either
        // and simply writes another one -- a harmless extra copy on a
        // server old enough to lack UIDPLUS, not a wrong deletion.
        Executed::Appended { uid: None } => Ok(()),
    }
}

/// What a permanently failed op undoes: the batch actions' local effect for
/// a thread-targeted op, and a queued send's draft state (plus, when it was
/// an invite RSVP, the invitation's own `my_response`) for `Send`.
/// `AppendDraft` made no optimistic local change to undo -- `save_draft`
/// writes the person's own text regardless of whether the server copy
/// ever lands -- so it is left alone.
///
/// Tolerant, throughout, of the one record a permanent failure names
/// having already gone missing by the time this runs -- a thread merged
/// into another, a draft deleted from under a `Send` that failed for
/// reasons that had nothing to do with the server (see [`vault_err`]'s own
/// docs on why a merely *local* failure -- a locked vault, a full disk --
/// is retried rather than ever reaching here as `permanent` in the first
/// place). `Error::NotFound` is logged and swallowed rather than
/// `?`-propagated: there is nothing left to revert, which is not a reason
/// to take the rest of the account's outbox down with it.
async fn on_permanent_failure(
    svc: &Arc<Service>,
    vault: &Arc<everyday_core::Vault>,
    op: &Op,
) -> CommandResult<()> {
    match op.target {
        OpTarget::Thread(thread) => {
            let vault = vault.clone();
            let kind = op.kind.clone();
            let op_id = op.id;
            blocking(move || match vault.revert_thread_op(thread, &kind) {
                Ok(()) => Ok(()),
                Err(everyday_core::Error::NotFound { .. }) => {
                    tracing::warn!(
                        op = %op_id,
                        thread = %thread,
                        "a permanently failed op's own thread is already gone; nothing to revert"
                    );
                    Ok(())
                }
                Err(e) => Err(e.into()),
            })
            .await
        }
        OpTarget::Draft(id) if matches!(op.kind, OpKind::Send) => {
            revert_queued_send(svc, vault, id, op.id).await
        }
        // `AppendDraft`, and a `Message`-targeted op -- nothing in phase 3's
        // command surface enqueues the latter; see the module docs on
        // `everyday_mail::outbox::Lookups::message_locations` for the
        // shape a future caller would need.
        _ => Ok(()),
    }
}

/// Put a `Send`-queued draft back to [`DraftState::Editing`], and -- when it
/// was an invite RSVP -- undo the optimistic
/// [`everyday_core::mail::Invite::my_response`] `respond_to_invite` set
/// before the reply ever reached anyone, via
/// [`everyday_core::Vault::revert_invite_response`]. Shared between
/// [`on_permanent_failure`] (the op just failed for good) and
/// [`reconcile_stranded_drafts`] (a crash between that failure and this very
/// write left the draft `Queued` regardless) -- both are "this `Send` is not
/// going to happen after all", reached from different doors.
///
/// The draft's own `in_reply_to` is what names the invitation this RSVP
/// answers -- the same field an ordinary reply threads under, and the field
/// `respond_to_invite_inner` would need to set on its own RSVP draft for
/// this to have anything to revert; that call site lives in
/// `everyday_service::domains::mail`, outside this module.
///
/// Uses [`everyday_core::Vault::with_draft`] rather than a bare
/// read-modify-write: this races the exact same debounced autosave that
/// method's own docs describe, and a permanent failure landing between a
/// person's own keystroke-driven save and this write must not be the write
/// that wins by clobbering the other.
async fn revert_queued_send(
    svc: &Arc<Service>,
    vault: &Arc<everyday_core::Vault>,
    id: DraftId,
    op_id: OpId,
) -> CommandResult<()> {
    let vault_for_draft = vault.clone();
    let thread = blocking(move || {
        let draft = match vault_for_draft.with_draft(id, |d| {
            if matches!(d.state, DraftState::Queued { op: queued } if queued == op_id) {
                d.state = DraftState::Editing;
                true
            } else {
                false
            }
        }) {
            Ok(draft) => draft,
            Err(everyday_core::Error::NotFound { .. }) => {
                tracing::warn!(
                    op = %op_id,
                    draft = %id,
                    "a permanently failed send's own draft is already gone; nothing to revert"
                );
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        };
        // `in_reply_to` alone does not mean this draft answered an
        // invitation -- it is set on every ordinary reply too, since that
        // is also the field the thread view uses to find a message's
        // parent. The one field an RSVP draft sets and an ordinary reply
        // never does is `calendar_part`: `respond_to_invite_inner` (in
        // `everyday_service::domains::mail`) attaches the iTIP `REPLY` part
        // there specifically so `everyday-mail`'s outbox knows to send it as
        // a calendar reply, and nothing else populates that field. Skipping
        // this check would mean any unrelated reply in an invitation's
        // thread that later bounces permanently un-answers the invitation
        // locally, even though the organiser already has -- and keeps -- a
        // real acceptance sent days earlier.
        let Some(parent) = draft.in_reply_to else { return Ok(None) };
        if draft.calendar_part.is_none() {
            return Ok(None);
        }
        vault_for_draft.revert_invite_response(parent)?;
        Ok(vault_for_draft.mail_message(parent).ok().map(|m| m.thread_id))
    })
    .await?;

    // Best-effort: a client that never learns the RSVP was reverted still
    // has the truthful `Invite::my_response` the moment it next reads the
    // thread, which every mail command already re-fetches from the vault
    // rather than trusting a cache -- this event only saves it the wait for
    // that next read to happen on its own.
    if let Some(thread) = thread {
        svc.events().changed(crate::events::Change {
            kind: crate::events::Kind::Thread,
            op: crate::events::Op::Updated,
            id: Some(thread.to_string()),
            ids: Vec::new(),
            origin: None,
        });
    }
    Ok(())
}

/// Mark draft `id` [`everyday_core::mail::DraftState::Sent`] and teach the
/// contact index its recipients -- the write a `Send` op's own success makes
/// true, and also what [`recover_inflight_ops`] applies directly once
/// [`already_sent`] has confirmed a recovered send without this process
/// having run [`everyday_mail::outbox::execute`] for it at all this time.
async fn mark_draft_sent(
    svc: &Arc<Service>,
    vault: &Arc<everyday_core::Vault>,
    id: DraftId,
) -> CommandResult<()> {
    let vault_for_draft = vault.clone();
    let draft = blocking(move || {
        // `Vault::with_draft`, not a bare read-modify-save -- see its own
        // docs for the autosave race a `Send` op's own success (or, from
        // `recover_inflight_ops`, a crash-recovered one) must not lose to.
        Ok(vault_for_draft.with_draft(id, |draft| {
            draft.state = DraftState::Sent;
            true
        })?)
    })
    .await?;
    if let Some(contacts) = svc.mail_contacts() {
        for addr in draft.to.iter().chain(draft.cc.iter()) {
            contacts.record_sent_to(&addr.email, &addr.name);
        }
        contacts.persist_if_dirty(vault);
    }
    Ok(())
}

async fn special_use(
    vault: &Arc<everyday_core::Vault>,
    account: AccountId,
    role: MailboxRole,
) -> CommandResult<Option<String>> {
    let vault = vault.clone();
    blocking(move || {
        Ok(vault.mailboxes(account)?.into_iter().find(|m| m.role == role).map(|m| m.remote_name))
    })
    .await
}

/// Bring every thread whose snooze has passed back to the inbox, and say
/// so. Called from the minute scheduler (`scheduler::tick`), not from a
/// command: nobody asks for this, the clock does.
pub async fn release_due_snoozes(svc: &Arc<Service>) -> CommandResult<usize> {
    let Some(vault) = svc.get() else { return Ok(0) };
    if !vault.is_writable() || !vault.supports_mail() {
        return Ok(0);
    }
    let now = svc.now();
    let due = blocking({
        let vault = vault.clone();
        move || Ok(vault.due_snoozed_threads(now, 200)?)
    })
    .await?;
    if due.is_empty() {
        return Ok(0);
    }
    let ids: Vec<String> = due.iter().map(ToString::to_string).collect();
    blocking({
        let vault = vault.clone();
        let due = due.clone();
        move || {
            for thread in due {
                vault.release_snooze(thread)?;
            }
            Ok(())
        }
    })
    .await?;
    svc.events().changed(crate::events::Change {
        kind: crate::events::Kind::Thread,
        op: crate::events::Op::Updated,
        id: None,
        ids,
        origin: None,
    });
    Ok(due.len())
}

/// Answers [`everyday_mail::outbox::Lookups`] by reading a vault. See the
/// module docs for where a reply's `References` chain and a draft's server
/// copy come from.
struct VaultLookups {
    svc: Arc<Service>,
    account: AccountId,
    /// Captured once, from the live session's own connect-time handshake
    /// (`MailSession::capabilities`), rather than read from the vault on
    /// every call -- see the trait method's own docs for why.
    gmail: bool,
    /// Where [`VaultLookups::parent_raw`] reads a reply's parent's genuine
    /// bytes from. `None` only when the pack store failed to open at all
    /// (`mailsync::wiring::open`'s own tolerance for that) -- `parent_raw`
    /// answers `MailError::Protocol` rather than panicking when it is.
    packs: Option<Arc<dyn everyday_core::packstore::PackStore>>,
}

impl VaultLookups {
    fn vault(&self) -> CommandResult<Arc<everyday_core::Vault>> {
        self.svc.require()
    }

    /// Every `(mailbox, uid)` `id` is filed under, turned into
    /// [`Located`]s -- except, on a Gmail account, the ones that are really
    /// a label membership rather than a place the message physically lives.
    ///
    /// `docs/plans/mail.md`'s Gmail folder rule means `\Inbox` and every
    /// user label reach the vault as [`everyday_core::mail::Mailbox`] rows
    /// of their own (see
    /// `crate::mailsync::discovery`'s own module docs) so that
    /// `message_mailboxes` can name a thread's presence under them the same
    /// way it names presence in a real folder -- but a label was never
    /// `SELECT`able, and the uid paired with it is All Mail's, not a uid in
    /// some mailbox named after the label. Handing either straight to
    /// [`everyday_mail::outbox::execute`] is what let it `SELECT` a label
    /// by name (a permanent failure on `\Inbox`, since no such mailbox
    /// exists to select) or store flags by an All Mail uid against whatever
    /// real mailbox happened to share the label's name. So on Gmail this
    /// filters down to exactly the five real, synced mailboxes
    /// (`crate::mailsync::discovery::GMAIL_SYNCED_ROLES`) before anything
    /// downstream ever sees a [`Located`] -- a label membership never
    /// becomes one. Every mailbox is real on a non-Gmail account, so
    /// nothing is filtered there.
    fn locations_of(&self, id: MailMessageId) -> everyday_mail::session::Result<Vec<Located>> {
        let vault = self.vault().map_err(lookup_err)?;
        let pairs = vault.mail_message_locations(id).map_err(vault_err)?;
        let mut out = Vec::with_capacity(pairs.len());
        for (mailbox, uid) in pairs {
            let mailbox = vault.mailbox(mailbox).map_err(vault_err)?;
            if self.gmail && !is_gmail_selectable(mailbox.role) {
                continue;
            }
            out.push(Located { mailbox: mailbox.remote_name, uid });
        }
        Ok(out)
    }
}

/// Is `role` one of Gmail's five genuinely `SELECT`able mailboxes -- see
/// [`VaultLookups::locations_of`]'s own docs. `\Inbox` ([`MailboxRole::Inbox`])
/// and every user label ([`MailboxRole::Other`]) are excluded on purpose:
/// both are label memberships on Gmail, never folders.
fn is_gmail_selectable(role: MailboxRole) -> bool {
    matches!(
        role,
        MailboxRole::All
            | MailboxRole::Sent
            | MailboxRole::Drafts
            | MailboxRole::Spam
            | MailboxRole::Trash
    )
}

impl Lookups for VaultLookups {
    fn thread_locations(&self, thread: ThreadId) -> everyday_mail::session::Result<Vec<Located>> {
        let vault = self.vault().map_err(lookup_err)?;
        let (_, messages) = vault.thread(thread).map_err(vault_err)?;
        let mut out = Vec::new();
        for message in messages {
            out.extend(self.locations_of(message.id)?);
        }
        Ok(out)
    }

    fn message_locations(
        &self,
        message: MailMessageId,
    ) -> everyday_mail::session::Result<Vec<Located>> {
        self.locations_of(message)
    }

    fn special_use(&self, role: MailboxRole) -> everyday_mail::session::Result<Option<String>> {
        let vault = self.vault().map_err(lookup_err)?;
        Ok(vault
            .mailboxes(self.account)
            .map_err(vault_err)?
            .into_iter()
            .find(|m| m.role == role)
            .map(|m| m.remote_name))
    }

    fn mailbox_name(&self, mailbox: MailboxId) -> everyday_mail::session::Result<String> {
        let vault = self.vault().map_err(lookup_err)?;
        Ok(vault.mailbox(mailbox).map_err(vault_err)?.remote_name)
    }

    fn is_gmail(&self) -> bool {
        self.gmail
    }

    fn draft(&self, id: DraftId) -> everyday_mail::session::Result<Draft> {
        let vault = self.vault().map_err(lookup_err)?;
        vault.draft(id).map_err(vault_err)
    }

    fn set_draft_message_id(
        &self,
        id: DraftId,
        message_id: &str,
    ) -> everyday_mail::session::Result<()> {
        let vault = self.vault().map_err(lookup_err)?;
        // `Vault::with_draft`, not a bare read-modify-save: this is the
        // very write `Vault::with_draft`'s own docs name as the one whose
        // loss duplicates a message on retry -- an autosave interleaving
        // between the old read and the old save could win with a draft
        // whose `message_id` was still `None`, so the next attempt minted
        // a *second* id, skipped `already_delivered` entirely (it only
        // ever checks a *retry*), and sent the message twice.
        vault
            .with_draft(id, |draft| {
                draft.message_id = Some(message_id.to_string());
                true
            })
            .map(|_| ())
            .map_err(vault_err)
    }

    fn draft_server_copy(&self, id: DraftId) -> everyday_mail::session::Result<Option<Located>> {
        let vault = self.vault().map_err(lookup_err)?;
        let draft = vault.draft(id).map_err(vault_err)?;
        Ok(draft.server_copy.map(|c| Located { mailbox: c.mailbox, uid: c.uid }))
    }

    /// Reads the parent's genuine raw bytes out of the pack store --
    /// exactly the bytes it was ingested from, so `mime::parse` ->
    /// `compose::reply_headers` sees its real `References` header, not a
    /// reconstruction of just the fields this crate happens to keep in the
    /// clear.
    fn parent_raw(&self, id: MailMessageId) -> everyday_mail::session::Result<Vec<u8>> {
        let vault = self.vault().map_err(lookup_err)?;
        let m = vault.mail_message(id).map_err(vault_err)?;
        let packs = self
            .packs
            .as_ref()
            .ok_or_else(|| MailError::Protocol("the mail pack store is not open".into()))?;
        packs.read(&m.pack).map_err(|e| MailError::Protocol(e.to_string()))
    }

    fn attachment_bytes(&self, blob: BlobId) -> everyday_mail::session::Result<Vec<u8>> {
        let vault = self.vault().map_err(lookup_err)?;
        vault.blob(blob).map_err(vault_err)
    }

    fn message_id_domain(&self) -> String {
        self.svc
            .get()
            .and_then(|v| v.account(self.account).ok())
            .map(|a| a.address.rsplit('@').next().unwrap_or("localhost").to_string())
            .unwrap_or_else(|| "localhost".to_string())
    }
}

/// Is `code` -- one of [`everyday_service::error::codes`](crate::error::codes)
/// -- a *local* storage failure worth retrying rather than a genuinely wrong
/// request? [`Error::Locked`](everyday_core::Error::Locked) (another writer
/// holds the vault right now), an IO error (a full disk, a transient
/// permission failure) and a backend error (whatever a Postgres connection
/// pool exhausted looks like today) each say nothing about whether *this*
/// op was ever going to succeed -- unlike `NotFound`, `Invalid` or
/// `Decrypt`, which say the exact same thing every time this op is tried
/// again. Read before every [`MailError`] this module mints from a vault
/// read gone wrong, so a locked vault or a full disk backs off and tries
/// again instead of reversing whatever the person just asked for and
/// telling them it failed.
///
/// Not merged with `meeting::pipeline::retry::is_transient` -- see
/// `crate::retry::tests::pipeline_and_outbox_classifiers_disagree_by_design`
/// for the decision, checked directly against both functions.
pub(crate) fn is_transient_local_failure(code: &str) -> bool {
    matches!(code, codes::LOCKED | codes::IO | codes::BACKEND)
}

fn lookup_err(e: CommandError) -> everyday_mail::session::MailError {
    if is_transient_local_failure(&e.code) {
        // `MailError::Network` purely for `is_retryable`'s sake -- nothing
        // here touched a socket, but "is this worth trying again" is
        // exactly what that variant already means to the drain loop, and
        // minting a new variant just to say the same thing would only give
        // `is_retryable` a second case to keep in sync with this one.
        everyday_mail::session::MailError::Network(e.to_string())
    } else {
        everyday_mail::session::MailError::Protocol(e.to_string())
    }
}

fn vault_err(e: everyday_core::Error) -> everyday_mail::session::MailError {
    lookup_err(e.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::Error;

    /// The regression for "every vault/storage error is classified as a
    /// permanent server failure": a locked vault says nothing about
    /// whether the op itself was ever going to succeed, so it must come
    /// back retryable, not permanent.
    #[test]
    fn a_locked_vault_is_retried_not_treated_as_permanent() {
        let err = vault_err(Error::Locked);
        assert!(matches!(err, MailError::Network(_)), "{err:?}");
        assert!(is_retryable(&err));
    }

    /// An IO error (a full disk, a transient permission failure) is the
    /// same shape as a locked vault: local, and worth retrying.
    #[test]
    fn an_io_error_is_retried_not_treated_as_permanent() {
        let io = std::io::Error::other("disk full");
        let err = vault_err(Error::io("/tmp/example", io));
        assert!(matches!(err, MailError::Network(_)), "{err:?}");
        assert!(is_retryable(&err));
    }

    /// A record that genuinely does not exist is the opposite: trying
    /// again would fail the exact same way every time, so this must stay
    /// permanent.
    #[test]
    fn a_missing_record_stays_permanent() {
        let err = vault_err(Error::not_found("draft", "some-id"));
        assert!(matches!(err, MailError::Protocol(_)), "{err:?}");
        assert!(!is_retryable(&err));
    }

    /// Phase 9.3's pinning step: the literal seconds
    /// [`everyday_core::mail::backoff_for_attempt`] answers with, as
    /// `drain_outbox`'s two call sites (`Err(MailError::Auth(..))` and
    /// `Err(err) if is_retryable(&err)`) actually use it -- fixed here,
    /// in this crate, even though the table itself lives in
    /// `everyday-core` and is pinned there too; this is what a caller of
    /// `outbox_table()` must keep seeing once phase 9.3 wraps it.
    #[test]
    fn outbox_backoff_seconds_are_pinned() {
        let expected = [30u64, 60, 300, 900, 3600, 3600, 3600].map(std::time::Duration::from_secs);
        for (attempts, want) in expected.into_iter().enumerate() {
            let got = everyday_core::mail::backoff_for_attempt(attempts as u32);
            assert_eq!(
                std::time::Duration::try_from(got).unwrap(),
                want,
                "attempts already made: {attempts}"
            );
        }
    }

    /// [`outbox_table`] must answer exactly what
    /// [`everyday_core::mail::backoff_for_attempt`] does -- it is a wrapper,
    /// not a second implementation.
    #[test]
    fn outbox_table_matches_backoff_for_attempt_exactly() {
        let policy = outbox_table();
        for attempts in 0..10u32 {
            assert_eq!(
                policy.delay_for(attempts),
                Duration::try_from(everyday_core::mail::backoff_for_attempt(attempts)).unwrap(),
                "attempts already made: {attempts}"
            );
        }
        assert_eq!(policy.max_attempts(), None, "an outbox op never gives up on its own");
    }

    /// Phase 9.3's pinning step: [`is_transient_local_failure`]'s verdict
    /// on a representative set of codes, fixed before the retry mechanism
    /// moves. Also the evidence for why it must stay separate from
    /// `meeting::pipeline::retry::is_transient`: that classifier says
    /// `true` for `NETWORK`/`TIMED_OUT`/`RATE_LIMITED` and `false` for
    /// `LOCKED`/`IO`/`BACKEND` -- the exact opposite of this one. A
    /// vault/storage failure and a provider/network failure are different
    /// domains that happen to share the word "transient"; unioning them
    /// would make a genuinely wrong request (in pipeline's world) retry
    /// forever, or a full disk (in outbox's world) fail permanently.
    #[test]
    fn is_transient_local_failure_verdicts_are_pinned() {
        for code in [codes::LOCKED, codes::IO, codes::BACKEND] {
            assert!(is_transient_local_failure(code), "{code} must be a transient local failure");
        }
        for code in [
            codes::NETWORK,
            codes::TIMED_OUT,
            codes::RATE_LIMITED,
            codes::FORBIDDEN,
            codes::NOT_FOUND,
            codes::INVALID,
            codes::DECRYPT_FAILED,
        ] {
            assert!(
                !is_transient_local_failure(code),
                "{code} must not be a transient local failure"
            );
        }
    }
}
