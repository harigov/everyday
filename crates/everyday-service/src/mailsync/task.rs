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
            tokio::select! {
                () = tokio::time::sleep(POLL_INTERVAL) => continue,
                _ = stop.changed() => return Ok(Outcome::Done),
                _ = nudged.changed() => continue,
                () = outbox_notify.notified() => continue,
            }
        }

        // A throwaway stop channel: `MailSession::idle` wants its own
        // `Receiver<()>`, and the supervisor's is a `Receiver<bool>`. Never
        // sent to -- the `select!` below is what actually races the
        // supervisor's stop signal (and a `sync_account` nudge, and an
        // outbox notification) against the idle call, dropping the latter
        // (and, with it, the connection) if any of them wins, which is an
        // entirely ordinary way for an IMAP session to end.
        let (_never_tx, never_rx) = watch::channel(());
        tokio::select! {
            _ = stop.changed() => return Ok(Outcome::Done),
            _ = nudged.changed() => continue,
            () = outbox_notify.notified() => continue,
            outcome = tokio::time::timeout(POLL_INTERVAL, session.idle(never_rx)) => {
                match outcome {
                    Ok(Ok(IdleEvent::Activity | IdleEvent::Stopped)) => continue,
                    Ok(Err(e)) => {
                        statuses.set_error(account_id, e.to_string());
                        return Err(e.to_string().into());
                    }
                    // The poll cadence elapsed with nothing reported: sweep
                    // every mailbox anyway.
                    Err(_elapsed) => continue,
                }
            }
        }
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
