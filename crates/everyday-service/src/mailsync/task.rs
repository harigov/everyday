//! [`run_account`]: one account's whole supervised task.
//!
//! Connect, sync every mailbox, drain the outbox, hold `IDLE` (or poll, on a
//! server without it) until something changes or [`POLL_INTERVAL`] elapses,
//! and do it all again -- for as long as the supervisor keeps this key
//! alive. See `crate::supervisor`'s module doc for the restart-with-backoff
//! and stop-on-lock machinery this task's `Result` and `Ok(Outcome::Done)`
//! answers drive.
//!
//! Generic over [`MailSession`] and over how a connection is made
//! ([`run_account_with`]'s `connect`), so [`tests`](super::tests) can drive
//! the whole state machine -- auth failure, a network error, a clean stop --
//! against a fake session with no socket at all. [`run_account`] is the one
//! production instance, built on [`everyday_mail::imap::ImapSession`].
//!
//! # Where the outbox is drained
//!
//! `crate::outbox`'s own module docs name three moments a drain should
//! happen. [`drain_until_caught_up`], called once at the top of the loop
//! below, is two of them at once: the loop's very first turn follows
//! [`connect`](run_account_with)'s success directly ("after connecting"),
//! and every later turn is reached only by falling out of the `IDLE`/poll
//! `select!` at the bottom of the loop ("after every `IDLE` wake"). The
//! third -- "whenever [`Service::outbox_notify`] fires" -- is its own arm in
//! both of that `select!`'s branches, `continue`ing straight back round to
//! the drain at the top rather than waiting out the rest of the poll or
//! `IDLE` call.
//!
//! A fourth arm, [`sleep_until_due`], answers a moment none of those three
//! cover: an op that is enqueued *not yet due* -- undo-send's five-to-thirty
//! second window, send-at, a backed-off retry -- becomes due while this
//! task is already parked in `IDLE` or the poll sleep, with nobody about to
//! notify it and no reason for `IDLE` itself to fire. Without this, that op
//! would simply wait for `POLL_INTERVAL` (five minutes) or the next
//! unrelated wake to come around. [`next_pending_wake`] reads the earliest
//! `not_before` still `Pending` for this account once per loop turn, and the
//! `select!`s race a sleep to exactly that instant alongside their other
//! arms -- `None`, when nothing is pending, is a branch that simply never
//! wins.
//!
//! # Cancelling `IDLE` without losing the connection
//!
//! [`MailSession::idle`] takes the session's own connection out for as long
//! as the call lasts and only hands it back once the call itself returns --
//! see [`everyday_mail::imap::ImapSession::idle`]'s own docs. Racing the
//! call in a `tokio::select!` the way an earlier version of this loop did
//! is exactly wrong, then: the instant any other arm wins, the idle future
//! is dropped mid-flight, and with it the connection `session_mut` would
//! need for the very next drain -- every op due right after an `IDLE` wake
//! (which is most of them, since a nudge or an outbox notification is what
//! wakes this loop) failed with a non-retryable "mid-IDLE" error and was
//! reverted. [`wait_for_wake`] is this loop's one race of everything that
//! should end an `IDLE` early; rather than racing it *against* the call,
//! the loop below sends on a wake channel `session.idle` is watching and
//! then awaits the call through to its own clean return, exactly the way
//! this same loop already asks `idle` to stop for the supervisor's own
//! `stop` signal. As defence in depth, a session found missing between
//! drains -- which this fix means should no longer actually happen --
//! reads as [`MailError::Network`] (retryable) rather than
//! [`MailError::Protocol`] (permanent); see
//! [`everyday_mail::imap::ImapSession::session_mut`]'s own docs.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use everyday_core::Vault;
use everyday_core::account::{Account, EndpointSecurity};
use everyday_core::id::AccountId;
use everyday_mail::imap::{self, ImapSession, Security};
use everyday_mail::session::{Credential, IdleEvent, MailError, MailSession};
use tokio::sync::watch;

use crate::error::CommandResult;
use crate::mailsync::discovery::LabelMailboxes;
use crate::mailsync::ingest::ThreadIndex;
use crate::mailsync::sender::LazySmtpSender;
use crate::mailsync::status::Phase;
use crate::mailsync::{credential, passes};
use crate::service::Service;
use crate::supervisor::{Outcome, TaskError, TaskResult};

/// How often the account task re-syncs even when nothing has told it to --
/// the plan's "every N minutes for the other mailboxes", and what stands in
/// for `IDLE` on a server, or an `IdleEvent`, that never fires.
const POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// How long [`drain_until_caught_up`] waits between drain attempts while ops
/// remain due, interruptibly -- "a few seconds is plenty" per
/// `crate::outbox`'s own module docs. `not_before` is what actually paces
/// retries and undo-send; this is only how promptly a due op is noticed.
const OUTBOX_RETRY_INTERVAL: Duration = Duration::from_secs(3);

/// The one production instance of [`run_account_with`]: connects over real
/// IMAP and sends over real SMTP, lazily. What
/// [`crate::mailsync::wiring::ensure_account_task`] registers with the
/// supervisor.
pub async fn run_account(
    svc: Arc<Service>,
    vault: Arc<Vault>,
    account_id: AccountId,
    stop: watch::Receiver<bool>,
) -> TaskResult {
    run_account_with(svc, vault, account_id, stop, connect_over_imap, LazySmtpSender::new).await
}

async fn connect_over_imap(
    account: Account,
    credential: Credential,
) -> everyday_mail::session::Result<ImapSession> {
    let security = match account.imap.security {
        EndpointSecurity::Tls => Security::Tls,
        EndpointSecurity::StartTls => Security::StartTls,
    };
    imap::connect(&account.imap.host, account.imap.port, security, credential).await
}

/// One account's whole task, generic over the session type and how one is
/// obtained, and over the sender type and how *it* is built -- see the
/// module docs. `build_sender` is called once, after a live session is in
/// hand, with the same `Account` [`connect`](Self) used -- production hands
/// it [`LazySmtpSender::new`] directly, since that function's own signature
/// already matches; a test hands it a closure building a fake.
pub async fn run_account_with<S, C, Fut, T, B>(
    svc: Arc<Service>,
    vault: Arc<Vault>,
    account_id: AccountId,
    mut stop: watch::Receiver<bool>,
    connect: C,
    build_sender: B,
) -> TaskResult
where
    S: MailSession,
    C: Fn(Account, Credential) -> Fut,
    Fut: Future<Output = everyday_mail::session::Result<S>>,
    T: everyday_mail::outbox::Sender,
    B: FnOnce(Account, Arc<Service>, Arc<Vault>) -> T,
{
    if *stop.borrow() {
        return Ok(Outcome::Done);
    }

    // The account may have been deleted, or had mail switched off, between
    // the supervisor deciding to (re)start this key and this attempt
    // actually running.
    let Ok(account) = vault.account(account_id) else { return Ok(Outcome::Done) };
    if !account.services.mail {
        return Ok(Outcome::Done);
    }

    let Some(packs) = svc.packs() else {
        return Err(unavailable("the mail pack store is not open"));
    };
    let Some(index) = svc.mail_index() else {
        return Err(unavailable("the mail search index is not open"));
    };
    let statuses = svc.mail_statuses().unwrap_or_default();
    statuses.set_phase(account_id, Phase::Connecting, 0, 0);

    let credential = match credential::resolve(&svc, &vault, &account).await {
        credential::Resolved::Ready(c) => c,
        credential::Resolved::NeedsSignIn(reason) => {
            credential::mark_needs_sign_in(&vault, &account, &reason);
            statuses.set_idle(account_id);
            return Ok(Outcome::Done);
        }
        credential::Resolved::Transient(message) => {
            statuses.set_error(account_id, message.clone());
            return Err(message.into());
        }
    };

    let mut session = match connect(account.clone(), credential).await {
        Ok(session) => session,
        Err(MailError::Auth(reason)) => {
            // The credential IMAP just rejected may be a cached access
            // token that is merely stale -- forgotten so the *next* attempt
            // (after the person signs in again, or on this same account's
            // next restart) resolves a genuinely fresh one rather than
            // handing out the same bad token again. See
            // `crate::mailsync::sender`'s module docs for the equivalent
            // reasoning on the SMTP side.
            svc.token_cache().forget(&account_id.to_string()).await;
            credential::mark_needs_sign_in(&vault, &account, &reason);
            statuses.set_idle(account_id);
            return Ok(Outcome::Done);
        }
        Err(MailError::Server(message)) => {
            credential::mark_error(&vault, &account, &message);
            return Err(message.into());
        }
        Err(e) => {
            statuses.set_error(account_id, e.to_string());
            return Err(e.to_string().into());
        }
    };

    let identities: Vec<String> = std::iter::once(account.address.clone())
        .chain(account.identities.iter().map(|i| i.address.clone()))
        .collect();
    let ctx = passes::SyncContext {
        vault: &vault,
        account_id,
        packs,
        index,
        statuses: &statuses,
        attachment_cap_bytes: account.attachment_cap_bytes,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities,
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    // `sync_account`'s way of waking this task early -- see
    // `StatusRegistry::nudge`.
    let mut nudged = statuses.subscribe(account_id);
    // What a write command's `Service::notify_outbox` wakes -- see the
    // module docs' "where the outbox is drained".
    let outbox_notify = svc.outbox_notify(account_id);
    let sender = build_sender(account.clone(), svc.clone(), vault.clone());

    // Before this task's very first drain: any op a previous run of this
    // same account left `InFlight` -- stranded by a crash between claiming
    // it and recording how it went -- goes back to work now, rather than
    // sitting forever waiting for a drain that already happened. See
    // `crate::outbox`'s own module docs for why nothing else ever notices.
    if let Err(e) = crate::outbox::recover_inflight_ops(&svc, account_id, &mut session).await {
        statuses.set_error(account_id, e.to_string());
        return Err(e.to_string().into());
    }

    loop {
        if *stop.borrow() {
            return Ok(Outcome::Done);
        }

        if let Err(e) =
            drain_until_caught_up(&svc, account_id, &mut session, &sender, &mut stop).await
        {
            statuses.set_error(account_id, e.to_string());
            return Err(e.to_string().into());
        }
        if *stop.borrow() {
            return Ok(Outcome::Done);
        }

        let mailboxes = match passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await
        {
            Ok(mailboxes) => mailboxes,
            Err(MailError::Auth(reason)) => {
                svc.token_cache().forget(&account_id.to_string()).await;
                credential::mark_needs_sign_in(&vault, &account, &reason);
                statuses.set_idle(account_id);
                return Ok(Outcome::Done);
            }
            Err(MailError::Server(message)) => {
                credential::mark_error(&vault, &account, &message);
                return Err(message.into());
            }
            Err(e) => {
                statuses.set_error(account_id, e.to_string());
                return Err(e.to_string().into());
            }
        };
        credential::mark_ok(&vault, &account);
        statuses.set_phase(account_id, Phase::Idling, 0, 0);

        // Undo-send and send-at are both a `Pending` op whose `not_before`
        // is the only thing standing between it and a drain; so is a
        // backed-off retry. Waiting out `POLL_INTERVAL` (or `IDLE`, which
        // may not fire again for a while on a quiet mailbox) for one of
        // those would mean a five-second undo window taking up to five
        // minutes to actually send. `next_wake` is `None` whenever nothing
        // is pending, in which case its branch below never fires -- exactly
        // the outcome racing it against `stop`, a nudge and a notify already
        // gives the other branches.
        let next_wake = next_pending_wake(&vault, account_id).await;

        // `IDLE` on the inbox -- All Mail, on Gmail, since that is where its
        // messages physically live -- re-issued (inside `ImapSession::idle`
        // itself) every twenty-five minutes; this task's own `POLL_INTERVAL`
        // is shorter, so the mailboxes `IDLE` says nothing about still get
        // their `changes_since` sweep on a cadence, not only when the inbox
        // happens to change.
        let can_idle = session.capabilities().idle
            && mailboxes.iter().any(|m| {
                matches!(
                    m.row.role,
                    everyday_core::mail::MailboxRole::Inbox | everyday_core::mail::MailboxRole::All
                )
            });
        if !can_idle {
            match wait_for_wake(&mut stop, &mut nudged, &outbox_notify, next_wake).await {
                Wake::Stop => return Ok(Outcome::Done),
                Wake::Nudge | Wake::OutboxNotify | Wake::Due | Wake::Poll => continue,
            }
        }

        // `IDLE`, watching a wake channel this loop turn owns for exactly
        // as long as the call lasts. `session.idle` sends `DONE` and hands
        // the session back the moment `wake_rx` changes -- see
        // `MailSession::idle`'s own docs -- so every reason this task has
        // to stop waiting (the supervisor's `stop`, a nudge, an outbox
        // notification, a due op's deadline, or simply `POLL_INTERVAL`
        // elapsing) ends the `IDLE` the same clean way, never by dropping
        // the call outright the way racing it in a bare `select!` used to.
        // Racing the call *itself* below is only ever won by `idle`
        // finishing on its own account (real activity, or its own internal
        // re-issue reporting an error) -- every other winner sends on
        // `wake_tx` and then awaits `idle_call` through to completion
        // rather than abandoning it.
        let (wake_tx, wake_rx) = watch::channel(());
        let idle_call = session.idle(wake_rx);
        tokio::pin!(idle_call);
        let (outcome, wake) = tokio::select! {
            outcome = &mut idle_call => (outcome, None),
            wake = wait_for_wake(&mut stop, &mut nudged, &outbox_notify, next_wake) => {
                let _ = wake_tx.send(());
                (idle_call.await, Some(wake))
            }
        };
        if matches!(wake, Some(Wake::Stop)) {
            return Ok(Outcome::Done);
        }
        match outcome {
            Ok(IdleEvent::Activity | IdleEvent::Stopped) => continue,
            Err(e) => {
                statuses.set_error(account_id, e.to_string());
                return Err(e.to_string().into());
            }
        }
    }
}

/// Every reason this task's own `select!`s stop waiting, named so
/// [`wait_for_wake`]'s callers can tell them apart without repeating the
/// five-armed race themselves.
enum Wake {
    Stop,
    Nudge,
    OutboxNotify,
    Due,
    Poll,
}

/// Race the supervisor's stop signal, a `sync_account` nudge, an outbox
/// notification, a due-but-not-yet op's deadline, and [`POLL_INTERVAL`]
/// itself, and report whichever fires first. The one place both of this
/// task's `select!`s -- the no-`IDLE` poll sleep and the `IDLE` call's own
/// wake channel -- build their race from, so the five arms are written
/// once.
async fn wait_for_wake(
    stop: &mut watch::Receiver<bool>,
    nudged: &mut watch::Receiver<()>,
    outbox_notify: &tokio::sync::Notify,
    next_wake: Option<Duration>,
) -> Wake {
    tokio::select! {
        _ = stop.changed() => Wake::Stop,
        _ = nudged.changed() => Wake::Nudge,
        () = outbox_notify.notified() => Wake::OutboxNotify,
        () = sleep_until_due(next_wake) => Wake::Due,
        () = tokio::time::sleep(POLL_INTERVAL) => Wake::Poll,
    }
}

/// How long until `account_id`'s earliest still-[`OpState::Pending`] op is
/// due, or `None` when the outbox has nothing pending at all --
/// [`everyday_core::Vault::next_pending_op_at`] read once per loop turn and
/// converted from a wall-clock instant to a duration the same way
/// `crate::token_cache::deadline_from` converts a token's expiry, and
/// clamped at zero for the same reason: a `not_before` that has already
/// passed (the drain just above did not reach it because `DRAIN_BATCH`
/// capped the page) must wake this `select!` at once, not underflow it.
async fn next_pending_wake(vault: &Arc<Vault>, account_id: AccountId) -> Option<Duration> {
    let at = crate::service::blocking({
        let vault = vault.clone();
        move || Ok(vault.next_pending_op_at(account_id)?)
    })
    .await
    .ok()
    .flatten()?;
    let remaining = jiff::Timestamp::now().duration_until(at);
    Some(if remaining.is_negative() { Duration::ZERO } else { remaining.unsigned_abs() })
}

/// The `select!` branch built from [`next_pending_wake`]'s answer: a real
/// sleep when there is a due time to wake for, or a future that never
/// completes when there is none, so omitting this branch's effect entirely
/// needs no `if` around the `select!` itself -- [`std::future::pending`] is
/// exactly as inert as leaving the arm out.
async fn sleep_until_due(remaining: Option<Duration>) {
    match remaining {
        Some(d) => tokio::time::sleep(d).await,
        None => std::future::pending().await,
    }
}

/// Drain `account_id`'s outbox, and keep draining -- on
/// [`OUTBOX_RETRY_INTERVAL`], interruptibly -- for as long as
/// [`crate::outbox::DrainReport::pending`] says there is more due work.
/// Returns once a pass comes back short, or the moment `stop` fires. See the
/// module docs' "where the outbox is drained" for the three moments this is
/// reached from.
async fn drain_until_caught_up<S: MailSession, T: everyday_mail::outbox::Sender>(
    svc: &Arc<Service>,
    account_id: AccountId,
    session: &mut S,
    sender: &T,
    stop: &mut watch::Receiver<bool>,
) -> CommandResult<()> {
    loop {
        let report = crate::outbox::drain_outbox(svc, account_id, session, sender).await?;
        if !report.pending || *stop.borrow() {
            return Ok(());
        }
        tokio::select! {
            _ = stop.changed() => return Ok(()),
            () = tokio::time::sleep(OUTBOX_RETRY_INTERVAL) => {}
        }
    }
}

fn unavailable(message: &str) -> TaskError {
    let error: TaskError = message.to_string().into();
    error
}
