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
//! still in each Inbox -- and of those, which are the kind asked for.
//!
//! # Narrowing it down
//!
//! Three filters, each optional and all of them together: an inbox tab
//! (`category`), one of the person's own sorted categories (`sortedAs`), and
//! words to find in a sender's name or address (`query`). The first two
//! pick *threads*, not senders, because one sender often sends several
//! kinds of mail -- a shop's receipts and its promotions -- and clearing out
//! the promotions must leave the receipts where they are. A sender's row
//! then counts, and its buttons move, only their threads of that kind. The
//! third picks senders, and is applied before `limit` rather than after it,
//! so the one sender somebody types the name of is found however far down
//! the ranking they are.
//!
//! # Older than
//!
//! `olderThanDays` turns the window round: everything still in the Inbox
//! from before that many days ago, so somebody can clear out a sender's
//! back catalogue and keep their last month. It too picks *threads*, and by
//! the thread's newest message anywhere, not by each message's own date:
//! the buttons move whole conversations, and a conversation with a reply
//! from last week is not old mail, whatever date its first message carries
//! -- deleting it would delete the reply too. Such a thread is left out
//! whole, and so is everything in it.
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
use everyday_core::mail::{Category, Mailbox, MailboxRole};
use everyday_core::store::mail::ThreadFilter;
use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;

/// The longest window `inbox_senders` will count in days: a year. Past
/// that, a caller asks for no window at all -- `days` left out -- which
/// counts every message the Inbox still holds, however old.
///
/// The count decrypts the messages of the threads still in the Inbox, not
/// every message in the account. All time is still bounded, by what an
/// Inbox holds rather than by a date: message records only, not bodies --
/// about 100 MB for fifty thousand messages -- and [`MAX_INBOX_THREADS`] on
/// the walk. Somebody clearing out an Inbox that has been filling for five
/// years needs the five years.
const MAX_DAYS: u32 = 365;

/// The furthest back `olderThanDays` may start: ten years. A cutoff past
/// that would leave nothing anybody still has in an Inbox to count.
const MAX_OLDER_THAN_DAYS: u32 = 3_650;

/// How many senders a caller gets when it does not say.
const DEFAULT_LIMIT: u32 = 20;

/// The most senders one call lists: as many as one Inbox's walk can turn
/// up, since every sender needs a thread and the walk reads at most
/// [`MAX_INBOX_THREADS`]. Quick Cleanup asks for all of them, once, and
/// pages and filters them itself. The count costs the same however many
/// rows it answers with, and a list already on hand answers typing at once,
/// where asking again for each word typed would run the whole count again.
const MAX_LIMIT: u32 = MAX_INBOX_THREADS as u32;

/// The longest `query` is taken. A sender's address is at most 254
/// characters; words beyond that could not all be in one.
const MAX_QUERY_CHARS: usize = 254;

/// How many of an Inbox's threads are read per page while collecting which
/// threads are still in it -- ids are all that is kept, so a large page
/// costs a decrypt each and nothing more.
const INBOX_PAGE: u32 = 500;

/// The most threads read from any one Inbox, however far back the window
/// reaches. A backstop for an Inbox of tens of thousands of threads inside
/// the window, not a limit anybody is expected to meet: what stops the walk
/// in practice is reaching threads older than the window, see
/// [`inbox_threads`]. "Older than" leaves the newer threads out in the
/// store, before any is read, so there the walk reads only threads that
/// can count, and the cap is still only for an Inbox of twenty thousand old
/// ones.
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
    /// Only threads filed under this inbox tab -- `Thread::category`, what
    /// the tabs themselves list by. Every tab when absent.
    #[serde(default)]
    pub category: Option<Category>,
    /// Only threads a decision model sorted into this one of the person's
    /// own categories, by its id (`SortCategory::id`, Settings → Mail
    /// sorting). An id that names no category counts nothing.
    #[serde(default)]
    pub sorted_as: Option<String>,
    /// Only senders whose address or name holds every word of this -- see
    /// [`InboxSender::matches`]. Blank or absent lists everybody.
    #[serde(default)]
    pub query: Option<String>,
    /// Only threads with nothing newer than this many days, 1 to
    /// `MAX_OLDER_THAN_DAYS` -- see "Older than" in the module docs. With
    /// `days` as well, the window runs from `days` ago to `olderThanDays`
    /// ago, and so must start before it ends.
    #[serde(default)]
    pub older_than_days: Option<u32>,
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
    if let Some(older) = args.older_than_days {
        if !(1..=MAX_OLDER_THAN_DAYS).contains(&older) {
            return Err(CommandError::new(
                codes::INVALID,
                format!("count mail older than 1 to {MAX_OLDER_THAN_DAYS} days"),
            ));
        }
        if args.days.is_some_and(|days| days <= older) {
            return Err(CommandError::new(
                codes::INVALID,
                "nothing is both that recent and that old: the window has to start before it ends",
            ));
        }
    }
    let query = args.query.unwrap_or_default();
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(CommandError::new(
            codes::INVALID,
            format!("look for at most {MAX_QUERY_CHARS} characters of a name or address"),
        ));
    }
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;
    let vault = svc.require()?;
    let now = svc.now();
    let days_ago = |days: u32| now - SignedDuration::from_hours(i64::from(days) * 24);
    let since = args.days.map_or(Timestamp::MIN, days_ago);
    // Which threads count -- see "Narrowing it down" and "Older than" in
    // the module docs.
    let filter = ThreadFilter {
        category: args.category,
        last_before: args.older_than_days.map(days_ago),
        ..Default::default()
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
        for account in accounts {
            // An account that has not synced an Inbox yet has nothing in
            // one to clean up.
            let Some(inbox) = inbox_of(&vault.mailboxes(account)?) else { continue };
            inbox_threads(&vault, inbox, since, filter, args.sorted_as.as_deref(), &mut in_inbox)?;
        }
        // Only what can be counted is decrypted: the messages of the threads
        // just found, wherever each is filed, rather than every message in
        // the window across every folder of every account.
        let threads: Vec<ThreadId> = in_inbox.iter().copied().collect();
        let messages = vault.mail_in_threads(&threads)?;
        let mut senders = cleanup::inbox_senders(&messages, &in_inbox, &me, since, usize::MAX);
        senders.retain(|s| s.matches(&query));
        senders.truncate(limit);
        Ok(senders)
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
///
/// The Feed's newsletter finder asks the same, so the two agree about what
/// an Inbox is.
pub(crate) fn inbox_of(mailboxes: &[Mailbox]) -> Option<MailboxId> {
    let named_inbox =
        |m: &&Mailbox| m.remote_name.trim_start_matches('\\').eq_ignore_ascii_case("INBOX");
    mailboxes
        .iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .or_else(|| mailboxes.iter().find(named_inbox))
        .map(|m| m.id)
}

/// Add to `into` every thread still in `inbox` -- not snoozed, and passing
/// `filter` and `sorted_as` -- with activity there since `since`: what a
/// sender's mail has to sit in to be counted, see the module docs. Also the
/// Feed's walk over the newsletters in an Inbox, with `filter` asking for
/// that category.
///
/// The tab and the age are asked of the store, from clear columns, so a
/// thread that cannot count is never read; `filter.snoozed` is always
/// replaced with "not snoozed". The sorted category is sealed inside each
/// thread, so it is checked here on the page that comes back.
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
pub(crate) fn inbox_threads(
    vault: &Vault,
    inbox: MailboxId,
    since: Timestamp,
    filter: ThreadFilter,
    sorted_as: Option<&str>,
    into: &mut HashSet<ThreadId>,
) -> everyday_core::Result<()> {
    let awake = ThreadFilter { snoozed: Some(false), ..filter };
    let mut cursor: Option<String> = None;
    let mut read = 0usize;
    loop {
        let page = vault.list_threads(inbox, &awake, cursor.as_deref(), INBOX_PAGE)?;
        read += page.threads.len();
        // Judged on the whole page, before the sorted category thins it:
        // the order is by date whatever a thread was sorted into.
        let past_the_window = page.threads.last().is_some_and(|t| t.last_date < since);
        into.extend(
            page.threads
                .iter()
                .filter(|t| {
                    sorted_as.is_none_or(|id| {
                        t.sorted_as.as_ref().is_some_and(|sorted| sorted.category == id)
                    })
                })
                .map(|t| t.id),
        );
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
        ("category", "MailCategory | null", false),
        ("sortedAs", "string | null", false),
        ("query", "string | null", false),
        ("olderThanDays", "number | null", false),
    ],
    run: inbox_senders,
}];

#[cfg(test)]
mod tests {
    use super::*;

    fn mailbox(name: &str, role: MailboxRole) -> Mailbox {
        Mailbox::new(AccountId::new(), name, role)
    }

    #[test]
    fn the_inbox_is_found_by_role_then_by_the_name_inbox() {
        let by_role = mailbox("Whatever", MailboxRole::Inbox);
        let archive = mailbox("Archive", MailboxRole::Archive);
        assert_eq!(inbox_of(&[archive.clone(), by_role.clone()]), Some(by_role.id));

        // A server that said nothing about roles, and Gmail's escaped name.
        let plain = mailbox("inbox", MailboxRole::Other);
        let gmail = mailbox("\\\\Inbox", MailboxRole::Other);
        assert_eq!(inbox_of(&[archive.clone(), plain.clone()]), Some(plain.id));
        assert_eq!(inbox_of(&[archive.clone(), gmail.clone()]), Some(gmail.id));

        // Role wins over a folder that merely shares the name.
        assert_eq!(inbox_of(&[plain, by_role.clone()]), Some(by_role.id));
        assert_eq!(inbox_of(&[archive]), None);
    }
}
