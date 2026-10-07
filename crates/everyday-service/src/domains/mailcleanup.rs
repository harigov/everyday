//! Quick Cleanup: who is filling the Inbox, and which threads would clear
//! them out of it.
//!
//! One read, `inbox_senders`. The interface draws its answer as a list of
//! senders -- "The Weekly, 41 messages, 38 unread" -- each with an archive
//! and a trash button that hand every thread the row names to the ordinary
//! `archive` and `trash` commands, undo and all. So nothing here writes,
//! and nothing here needs a confirmation of its own: the moves go through
//! the same doors a person's own selection does. The counting itself is
//! [`everyday_core::mail::cleanup::inbox_senders`], tested there; what is
//! here is the window, which accounts, who "you" are, and which threads are
//! still in each Inbox.
//!
//! # Why only what is still in the Inbox
//!
//! A row is an offer to move exactly the threads it counts, so it must count
//! exactly what would move: mail somebody already archived is not cluttering
//! anything, and a row built from it would offer to clean up nothing. It is
//! also what makes the list honest after it has been used -- a sender whose
//! threads were just archived has nothing left in the Inbox, and simply
//! drops off the next read, rather than lingering with a count that no
//! longer describes anything a button could act on. Snoozed threads are left
//! out on the same terms: the person has already said what happens to them,
//! and the Inbox is not showing them.
//!
//! # Gating
//!
//! The `Mail` scope, and nothing finer -- the same as `mail_activity`, which
//! decrypts the same messages to count the same senders for the Overview,
//! and `list_threads`, whose rows these thread ids are.

use std::collections::HashSet;
use std::sync::Arc;

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};
use everyday_core::Vault;
use everyday_core::id::{AccountId, MailboxId, ThreadId};
use everyday_core::mail::cleanup::{self, InboxSender};
use everyday_core::mail::{Mailbox, MailboxRole};
use everyday_core::store::mail::ThreadFilter;
use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;

/// The longest window `inbox_senders` will count in days: a year. Past
/// that, a caller asks for no window at all -- `days` left out -- which
/// counts every message the Inbox still holds, however old.
///
/// The count decrypts every message in the window, which is what the window
/// used to be for. All time is still bounded, by what a mail store holds
/// rather than by a date: message records only, not bodies -- about 100 MB
/// for fifty thousand messages -- and [`MAX_INBOX_THREADS`] on the walk.
/// Somebody clearing out an Inbox that has been filling for five years
/// needs the five years.
const MAX_DAYS: u32 = 365;

/// How many senders a caller gets when it does not say.
const DEFAULT_LIMIT: u32 = 20;

/// The most senders one call lists. Past this the list stops being a list
/// of the few senders worth a bulk action and becomes an address book.
const MAX_LIMIT: u32 = 50;

/// How many of an Inbox's threads are read per page while collecting which
/// threads are still in it -- ids are all that is kept, so a large page
/// costs a decrypt each and nothing more.
const INBOX_PAGE: u32 = 500;

/// The most threads read from any one Inbox, however far back the window
/// reaches. A backstop for an Inbox of tens of thousands of threads inside
/// a year-long window, not a limit anybody is expected to meet: what stops
/// the walk in practice is reaching threads older than the window, see
/// [`inbox_threads`].
const MAX_INBOX_THREADS: usize = 20_000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxSendersArgs {
    /// How far back to count, in days, ending now: 1 to `MAX_DAYS`. Absent
    /// or `null` counts everything still in the Inbox, however old.
    #[serde(default)]
    pub days: Option<u32>,
    /// Whose Inboxes to count. Every account with mail switched on when
    /// absent; an id that names no account counts nothing.
    #[serde(default)]
    pub accounts: Option<Vec<AccountId>>,
    /// How many senders to list: `DEFAULT_LIMIT` when absent, and held to
    /// `1..=MAX_LIMIT` either way.
    #[serde(default)]
    pub limit: Option<u32>,
}

async fn inbox_senders(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: InboxSendersArgs,
) -> CommandResult<Vec<InboxSender>> {
    if let Some(days) = args.days
        && !(1..=MAX_DAYS).contains(&days)
    {
        return Err(CommandError::new(
            codes::INVALID,
            format!(
                "count between 1 and {MAX_DAYS} days back, or leave the days out for all of it"
            ),
        ));
    }
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;
    let vault = svc.require()?;
    let now = svc.now();
    let since = match args.days {
        Some(days) => now - SignedDuration::from_hours(i64::from(days) * 24),
        None => Timestamp::MIN,
    };
    blocking(move || {
        // A backend with no accounts has no mail either -- see
        // `domains::insights`, which asks the same question first.
        if !vault.supports_accounts() {
            return Ok(Vec::new());
        }
        let me = super::insights::me(&vault)?;
        let accounts: Vec<AccountId> = match args.accounts {
            Some(mut ids) => {
                // The same account named twice would count its mail twice.
                ids.sort();
                ids.dedup();
                ids
            }
            None => {
                vault.accounts()?.into_iter().filter(|a| a.services.mail).map(|a| a.id).collect()
            }
        };

        let mut in_inbox = HashSet::new();
        let mut messages = Vec::new();
        for account in accounts {
            // An account that has not synced an Inbox yet has nothing in
            // one to clean up.
            let Some(inbox) = inbox_of(&vault.mailboxes(account)?) else { continue };
            inbox_threads(&vault, inbox, since, &mut in_inbox)?;
            messages.extend(vault.mail_between(account, since, now)?);
        }
        Ok(cleanup::inbox_senders(&messages, &in_inbox, &me, since, limit))
    })
    .await
}

/// The mailbox that is an account's Inbox: the one synced with
/// [`MailboxRole::Inbox`] -- on Gmail, the `\Inbox` label's row, see
/// `crate::mailsync::discovery` -- or, on a server that never said which of
/// its folders was which, the one named `INBOX`, the name IMAP reserves for
/// it (and spells without regard to case). The leading backslashes are
/// ignored so a Gmail account synced before `\Inbox` was resolved to its
/// role -- its row still `Other`, named `\Inbox`, or `\\Inbox` from before
/// `unescape_imap_quoted` -- is found too, the same gap the interface's own
/// `promoteGmailInbox` closes for the mailbox list. `None` when there is
/// neither.
fn inbox_of(mailboxes: &[Mailbox]) -> Option<MailboxId> {
    let named_inbox =
        |m: &&Mailbox| m.remote_name.trim_start_matches('\\').eq_ignore_ascii_case("INBOX");
    mailboxes
        .iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .or_else(|| mailboxes.iter().find(named_inbox))
        .map(|m| m.id)
}

/// Add to `into` every thread still in `inbox` -- not snoozed -- with
/// activity there since `since`: what a sender's mail has to sit in to be
/// counted, see the module docs.
///
/// Pages newest first and stops at the first page whose last thread's
/// `last_date` is before `since`. That stop is safe because of how the two
/// dates relate: the list is ordered by each thread's newest message *in
/// this mailbox*, and a thread's own `last_date` -- its newest message
/// anywhere -- is never older than that. So once one row's `last_date` is
/// before the window, its newest Inbox message is too, and every row after
/// it in the order is older still: no later thread has an Inbox message in
/// the window. (Such a thread could still hold a newer message filed
/// somewhere else -- in practice, a reply you sent, which is never counted
/// as a sender anyway.) [`MAX_INBOX_THREADS`] stops the walk regardless.
fn inbox_threads(
    vault: &Vault,
    inbox: MailboxId,
    since: Timestamp,
    into: &mut HashSet<ThreadId>,
) -> everyday_core::Result<()> {
    let awake = ThreadFilter { snoozed: Some(false), ..Default::default() };
    let mut cursor: Option<String> = None;
    let mut read = 0usize;
    loop {
        let page = vault.list_threads(inbox, &awake, cursor.as_deref(), INBOX_PAGE)?;
        read += page.threads.len();
        let past_the_window = page.threads.last().is_some_and(|t| t.last_date < since);
        into.extend(page.threads.iter().map(|t| t.id));
        match page.next_cursor {
            Some(next) if !past_the_window && read < MAX_INBOX_THREADS => cursor = Some(next),
            _ => return Ok(()),
        }
    }
}

pub static COMMANDS: &[crate::command::Command] = &[command! {
    name: "inbox_senders", scope: Mail, effect: Read,
    args: InboxSendersArgs, returns: "InboxSender[]",
    signature: &[
        ("days", "number | null", false),
        ("accounts", "AccountId[] | null", false),
        ("limit", "number | null", false),
    ],
    run: inbox_senders,
}];
