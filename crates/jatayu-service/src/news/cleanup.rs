//! After an expiry pass: the email of each newsletter issue cleaned up from a
//! source that says so goes to Trash ([`NewsSource::trash_mail`]).
//!
//! Cleaning up a story takes it out of the Feed and nothing more -- the
//! message stays in Mail, where it was filed. A newsletter that wants its
//! email gone with its issues says so itself, and the expiry pass has already
//! worked out which messages that is ([`ExpireReport::mail`]): only mail
//! sources, never the Newsletters source, since Mail files some mail there
//! that is not a newsletter at all.
//!
//! # The same door as a click
//!
//! The move goes through [`Vault::apply_thread_ops`] as [`Origin::Person`],
//! as the Feed's other moves of mail do (see [`super::newsletters`]): the
//! change to the thread and the op that tells the server are one write, and
//! the history does not gain a conversation or a routine's run that never
//! happened.
//!
//! # Which threads
//!
//! Trash moves a whole conversation, and a message is not always alone in
//! one -- Gmail threads issues that share a subject. So a thread is left
//! where it is, and picked up again when the next of its issues is cleaned
//! up, if:
//!
//! - any message in it is starred, or a draft: the one rule the setting
//!   promises, and a draft is somebody's work;
//! - an issue in it is still in the Feed, which would otherwise lose its
//!   email while it is being kept;
//! - a message of the person's own is in it (filed in Sent): a reply they
//!   wrote to a newsletter is not the newsletter's to throw away;
//! - it is in Trash already, all of it.
//!
//! A message that has gone from Mail is skipped quietly.
//!
//! [`NewsSource::trash_mail`]: jatayu_core::news::NewsSource::trash_mail

use crate::error::CommandResult;
use crate::events::{self, Change, Kind};
use crate::service::{Service, blocking};
use jatayu_core::Vault;
use jatayu_core::id::{MailMessageId, MailboxId, ThreadId};
use jatayu_core::mail::{MailboxRole, OpKind, Origin};
use jatayu_core::news::ExpireReport;
use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

/// How many conversations one write moves. A newsletter followed for years
/// and cleaned up for the first time can have thousands; a person selecting
/// that many moves them in one go too, but nothing here is waiting on it.
const BATCH: usize = 200;

/// Act on what `report` says was cleaned up. Never fails the pass it follows:
/// a message that could not be moved is logged, and its story is gone either
/// way.
pub async fn after_expiry(svc: &Arc<Service>, vault: &Arc<Vault>, report: &ExpireReport) {
    if report.mail.is_empty() {
        return;
    }
    let found = {
        let vault = vault.clone();
        let messages = report.mail.clone();
        blocking(move || Ok(threads_to_trash(&vault, &messages)?)).await
    };
    let threads = match found {
        Ok(threads) => threads,
        Err(e) => {
            tracing::warn!(error = %e, "cleaned up newsletter issues but could not find their email to bin");
            return;
        }
    };
    for batch in threads.chunks(BATCH) {
        if let Err(e) = trash(svc, vault, batch.to_vec()).await {
            tracing::warn!(
                error = %e,
                threads = batch.len(),
                "cleaned up newsletter issues but could not move their email to Trash"
            );
        }
    }
}

/// The conversations holding `messages` that may be moved to Trash, each
/// once, in the order the messages came.
fn threads_to_trash(
    vault: &Vault,
    messages: &[MailMessageId],
) -> jatayu_core::Result<Vec<ThreadId>> {
    let mut seen = HashSet::new();
    let mut threads = Vec::new();
    for &id in messages {
        let message = match vault.mail_message(id) {
            Ok(message) => message,
            Err(jatayu_core::Error::NotFound { .. }) => continue,
            Err(e) => return Err(e),
        };
        if seen.insert(message.thread_id) && movable(vault, message.thread_id)? {
            threads.push(message.thread_id);
        }
    }
    Ok(threads)
}

/// Is `thread` one whose email the setting may bin? See "Which threads" in
/// the module docs.
fn movable(vault: &Vault, thread: ThreadId) -> jatayu_core::Result<bool> {
    let all = match vault.thread(thread) {
        Ok((_, all)) => all,
        Err(jatayu_core::Error::NotFound { .. }) => return Ok(false),
        Err(e) => return Err(e),
    };
    let draft =
        |m: &jatayu_core::mail::Message| m.flags.draft || m.labels.iter().any(|l| l == "\\Draft");
    if all.iter().any(|m| m.flags.flagged || draft(m)) {
        return Ok(false);
    }
    let ids: Vec<MailMessageId> = all.iter().map(|m| m.id).collect();
    if !vault.stories_for_mail(&ids)?.is_empty() {
        return Ok(false);
    }
    // Where it is filed: a mailbox that has gone since is no place at all.
    let mailboxes: BTreeSet<MailboxId> = vault
        .mail_thread_filings(&[thread])?
        .into_iter()
        .map(|(_, _, mailbox, _)| mailbox)
        .collect();
    let roles: Vec<MailboxRole> =
        mailboxes.into_iter().filter_map(|id| vault.mailbox(id).ok()).map(|m| m.role).collect();
    if roles.contains(&MailboxRole::Sent) {
        return Ok(false);
    }
    Ok(roles.is_empty() || roles.iter().any(|role| *role != MailboxRole::Trash))
}

/// Move `threads` to Trash the way a person's click does -- one vault write
/// for the local change and every op -- then wake each account's sync task
/// and tell open windows the threads changed.
async fn trash(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    threads: Vec<ThreadId>,
) -> CommandResult<()> {
    let origin = Origin::Person;
    svc.check_mail_rate_limit(&origin, "person")?;
    let ops = {
        let vault = vault.clone();
        let threads = threads.clone();
        blocking(move || Ok(vault.apply_thread_ops(&threads, OpKind::Trash, origin)?)).await?
    };
    for account in ops.iter().map(|op| op.account_id).collect::<BTreeSet<_>>() {
        svc.notify_mail_write(account);
    }
    // One or the other, never both -- `Change`'s own contract. The stories
    // were announced by the pass that expired them.
    let (id, ids) = match threads.as_slice() {
        [one] => (Some(one.to_string()), Vec::new()),
        many => (None, many.iter().map(ToString::to_string).collect()),
    };
    svc.events().changed(Change {
        kind: Kind::Thread,
        op: events::Op::Updated,
        id,
        ids,
        origin: None,
    });
    Ok(())
}
