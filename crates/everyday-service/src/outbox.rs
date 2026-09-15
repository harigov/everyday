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

use everyday_core::id::{AccountId, BlobId, DraftId, MailMessageId, MailboxId, ThreadId};
use everyday_core::mail::{Draft, MailboxRole, Op, OpKind, OpState, OpTarget};
use everyday_mail::outbox::{
    ExecContext, Executed, Located, Lookups, Sender, execute, is_retryable,
};
use everyday_mail::session::{MailError, MailSession};
use jiff::Timestamp;

use crate::error::{CommandError, CommandResult};
use crate::service::{Service, blocking};

/// How many due ops one [`drain_outbox`] call fetches and runs. Bounded so
/// one call cannot hold the vault's writer for an unbounded backlog; the
/// module docs' calling convention is what lets the account task simply
/// call again when [`DrainReport::pending`] says there is more.
const DRAIN_BATCH: u32 = 25;

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
    let now = Timestamp::now();
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
                on_success(svc, &vault, &op, executed).await?;
                op.transition_to(OpState::Done)?;
                persist_op(&vault, &op).await?;
                report.done += 1;
            }
            Err(err) if is_retryable(&err) => {
                op.attempts += 1;
                op.last_error = Some(err.to_string());
                let backoff = everyday_core::mail::backoff_for_attempt(op.attempts);
                op.not_before = Timestamp::now() + backoff;
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
                on_permanent_failure(&vault, &op).await?;
                report.failed += 1;
            }
        }
    }

    Ok(report)
}

async fn persist_op(vault: &Arc<everyday_core::Vault>, op: &Op) -> CommandResult<()> {
    let vault = vault.clone();
    let op = op.clone();
    blocking(move || Ok(vault.update_op(&op)?)).await
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
        Executed::Sent { .. } => {
            let OpTarget::Draft(id) = op.target else {
                return Ok(()); // guarded by `everyday_mail::outbox::send`'s own contract
            };
            let vault_for_draft = vault.clone();
            let draft = blocking(move || {
                let mut draft = vault_for_draft.draft(id)?;
                draft.state = everyday_core::mail::DraftState::Sent;
                draft.updated_at = Timestamp::now();
                vault_for_draft.save_draft(&draft)?;
                Ok(draft)
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
        Executed::Appended { uid: Some(uid) } => {
            let OpTarget::Draft(id) = op.target else { return Ok(()) };
            let Some(drafts_mailbox) =
                special_use(vault, op.account_id, MailboxRole::Drafts).await?
            else {
                return Ok(()); // nothing to remember without a mailbox name to pair the uid with
            };
            let vault = vault.clone();
            blocking(move || {
                // Reloaded fresh rather than reusing whatever the caller
                // holds: `AppendDraft` runs on the account task's own
                // schedule, entirely separately from a person still typing,
                // and only `server_copy` is this write's business -- every
                // other field is whatever the draft's own record already
                // says.
                let mut draft = vault.draft(id)?;
                draft.server_copy =
                    Some(everyday_core::mail::DraftServerCopy { mailbox: drafts_mailbox, uid });
                vault.save_draft(&draft)?;
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
/// a thread-targeted op, and a queued send's draft state for `Send`.
/// `AppendDraft` made no optimistic local change to undo -- `save_draft`
/// writes the person's own text regardless of whether the server copy
/// ever lands -- so it is left alone.
async fn on_permanent_failure(vault: &Arc<everyday_core::Vault>, op: &Op) -> CommandResult<()> {
    match op.target {
        OpTarget::Thread(thread) => {
            let vault = vault.clone();
            let kind = op.kind.clone();
            blocking(move || Ok(vault.revert_thread_op(thread, &kind)?)).await
        }
        OpTarget::Draft(id) if matches!(op.kind, OpKind::Send) => {
            let vault = vault.clone();
            let op_id = op.id;
            blocking(move || {
                let mut draft = vault.draft(id)?;
                if matches!(draft.state, everyday_core::mail::DraftState::Queued { op: queued } if queued == op_id)
                {
                    draft.state = everyday_core::mail::DraftState::Editing;
                    draft.updated_at = Timestamp::now();
                    vault.save_draft(&draft)?;
                }
                Ok(())
            })
            .await
        }
        // `AppendDraft`, and a `Message`-targeted op -- nothing in phase 3's
        // command surface enqueues the latter; see the module docs on
        // `everyday_mail::outbox::Lookups::message_locations` for the
        // shape a future caller would need.
        _ => Ok(()),
    }
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
    let now = Timestamp::now();
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

    fn locations_of(&self, id: MailMessageId) -> everyday_mail::session::Result<Vec<Located>> {
        let vault = self.vault().map_err(lookup_err)?;
        let pairs = vault.mail_message_locations(id).map_err(vault_err)?;
        pairs
            .into_iter()
            .map(|(mailbox, uid)| {
                let name = vault.mailbox(mailbox).map_err(vault_err)?.remote_name;
                Ok(Located { mailbox: name, uid })
            })
            .collect()
    }
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

fn lookup_err(e: CommandError) -> everyday_mail::session::MailError {
    everyday_mail::session::MailError::Protocol(e.to_string())
}

fn vault_err(e: everyday_core::Error) -> everyday_mail::session::MailError {
    everyday_mail::session::MailError::Protocol(e.to_string())
}
