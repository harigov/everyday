//! Newsletters from Mail: a sender somebody follows, read in the Feed.
//!
//! Nothing here touches the network. A mail-backed source's turn asks the
//! mail store for what that sender sent after the watermark and stores one
//! story per message, pointing at it. The message stays in Mail; its body is
//! drawn through mail's own route, under mail's own picture rules; its read
//! state *is* the message's seen flag.
//!
//! # Why the store and not the search index
//!
//! `docs/plans/feed.md` pictured `from:<sender> after:<watermark>` through
//! the mail index. A turn reads [`Vault::mail_between`] instead, and filters
//! the sender in Rust, because:
//!
//! - the index is only fed by the bodies pass, after the headers pass has
//!   already stored the message, and a lost or uncommitted index write (the
//!   sync swallows them) would lose an issue for ever; the store cannot
//!   disagree with itself about what mail there is;
//! - `Service::mail_index` is `None` whenever the index did not open
//!   cleanly, which would turn a Feed turn into an error the person cannot
//!   act on;
//! - the index tokenises the `From` header, so `from:news@example.com` also
//!   finds a message that merely names that address somewhere, and every hit
//!   would need checking against the real sender anyway.
//!
//! The price is a decrypt of each message in the window, which the clear
//! `date_us` column keeps to the mail since the watermark -- the same price
//! the Overview's sender counts pay.
//!
//! # Moving mail
//!
//! Marking an issue read or archiving it goes through
//! [`Vault::apply_thread_ops`], the one door a person's own click on a thread
//! uses, so the local change and the outbox op that tells the server are
//! one write. The Feed is acting for the person, so the op's origin is
//! [`Origin::Person`]: "keep these out of my inbox" is their standing
//! instruction, and calling it a routine's or the assistant's would put a
//! run or a conversation in the thread's history that never existed.

use crate::domains::insights;
use crate::domains::mailcleanup::{inbox_of, inbox_threads};
use crate::error::CommandResult;
use crate::events::{self, Change, Kind};
use crate::service::{Service, blocking};
use everyday_core::Vault;
use everyday_core::id::{AccountId, MailMessageId, MailboxId, StoryId, ThreadId};
use everyday_core::mail::{Category, MailboxRole, Message, OpKind, Origin, cleanup};
use everyday_core::news::{FetchState, NewsSource, SUMMARY_CHARS, SourceKind, Story, StoryBody};
use everyday_core::store::mail::ThreadFilter;
use jiff::{SignedDuration, Timestamp};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

/// How far back a source's first turn looks, in days: enough to fill the
/// list with the last few issues of a monthly, without dragging in a year of
/// somebody's archive on the strength of one tick.
const FIRST_TURN_DAYS: i64 = 30;

/// Someone who sends newsletters to one of this vault's inboxes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewsletterSender {
    /// Lower-cased.
    pub email: String,
    /// The display name their most recent message carried.
    pub name: String,
    /// Messages in the window.
    pub messages: u32,
    /// Already followed in the Feed.
    pub followed: bool,
}

/// One mail-backed source's turn: store a story for every message from its
/// sender newer than `state.watermark`, advance the watermark, and -- when
/// the source keeps its issues out of the inbox -- archive each new one's
/// thread through mail's own outbox. Returns how many stories were stored.
///
/// The window starts at the watermark, inclusive, so the newest message
/// already turned into a story is read again each turn; it is skipped for
/// having one. It starts thirty days back when there is no watermark yet.
pub async fn refresh(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    source: &NewsSource,
    state: &mut FetchState,
) -> Result<u32, String> {
    let SourceKind::Mail { sender } = &source.kind else {
        return Err("this source does not follow a sender in Mail".into());
    };
    let sender = sender.trim().to_lowercase();
    let now = svc.now();
    let from = state.watermark.unwrap_or(now - SignedDuration::from_hours(FIRST_TURN_DAYS * 24));

    let found = {
        let vault = vault.clone();
        let source = source.clone();
        blocking(move || Ok(gather(&vault, &source, &sender, from, now)?)).await
    }
    .map_err(|e| format!("could not read this sender's mail: {}", e.message))?;

    let inserted: HashSet<StoryId> = if found.issues.is_empty() {
        HashSet::new()
    } else {
        let batch: Vec<(Story, Option<StoryBody>)> =
            found.issues.iter().map(|issue| (issue.story.clone(), None)).collect();
        let vault = vault.clone();
        let ids = blocking(move || Ok(vault.insert_stories(&batch)?)).await;
        ids.map_err(|e| format!("could not store the new issues: {}", e.message))?
            .into_iter()
            .collect()
    };

    // Only once the stories are stored: a failed insert must leave the next
    // turn looking at the same window.
    state.watermark = match (state.watermark, found.newest) {
        (Some(old), Some(new)) => Some(old.max(new)),
        (old, new) => new.or(old),
    };

    if source.keep_out_of_inbox {
        // A thread of two new issues is archived once, and one that is no
        // longer in the Inbox -- already filed away by hand -- not at all.
        let mut seen = HashSet::new();
        let threads: Vec<ThreadId> = found
            .issues
            .iter()
            .filter(|issue| issue.in_inbox && inserted.contains(&issue.story.id))
            .map(|issue| issue.thread)
            .filter(|thread| seen.insert(*thread))
            .collect();
        thread_op(svc, vault, threads, OpKind::Archive).await.map_err(|e| {
            format!("stored the new issues but could not move them out of the inbox: {}", e.message)
        })?;
    }
    Ok(inserted.len() as u32)
}

/// Mark the messages behind these mail-backed stories read or unread,
/// through mail itself, so the server hears about it. Stories without a
/// message are ignored.
///
/// Mail marks a whole thread, as it does for a person, so an issue that
/// shares a thread with others takes them with it. A message that is already
/// as asked is left alone, which keeps a double click from queueing a second
/// op for the server to run.
pub async fn mark_read(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    stories: &[Story],
    read: bool,
) -> CommandResult<()> {
    let messages: Vec<MailMessageId> = stories.iter().filter_map(|s| s.mail_message).collect();
    if messages.is_empty() {
        return Ok(());
    }
    let to_change = {
        let vault = vault.clone();
        blocking(move || {
            let mut seen = HashSet::new();
            let mut threads = Vec::new();
            for id in messages {
                let message = match vault.mail_message(id) {
                    Ok(message) => message,
                    // Deleted in Mail: its story is not listed any more, and
                    // there is nobody to tell.
                    Err(everyday_core::Error::NotFound { .. }) => continue,
                    Err(e) => return Err(e.into()),
                };
                if message.flags.seen != read && seen.insert(message.thread_id) {
                    threads.push(message.thread_id);
                }
            }
            Ok(threads)
        })
        .await?
    };
    let kind = if read { OpKind::MarkRead } else { OpKind::MarkUnread };
    thread_op(svc, vault, to_change, kind).await
}

/// The senders whose mail sat in the Newsletters category over the last
/// `days` days, most prolific first.
///
/// Counted as Quick Cleanup counts an Inbox -- only threads still in it and
/// awake, nothing from you, no drafts -- by the same
/// [`cleanup::inbox_senders`], so the rules about whose mail counts are not
/// written twice. What this adds is the category, and the `followed` mark.
pub fn senders(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    days: u32,
) -> CommandResult<Vec<NewsletterSender>> {
    // A backend with no accounts has no mail either.
    if !vault.supports_accounts() || !vault.supports_mail() {
        return Ok(Vec::new());
    }
    let now = svc.now();
    let since = now - SignedDuration::from_hours(i64::from(days) * 24);
    let me = insights::me(vault)?;

    // Asking for the category in the walk is what keeps it to the
    // newsletters, and not the whole Inbox.
    let newsletters = ThreadFilter { category: Some(Category::Newsletter), ..Default::default() };
    let mut in_inbox = HashSet::new();
    for account in vault.accounts()?.into_iter().filter(|a| a.services.mail) {
        // An account that has not synced an Inbox yet has nothing in one.
        let Some(inbox) = inbox_of(&vault.mailboxes(account.id)?) else { continue };
        inbox_threads(vault, inbox, since, newsletters, None, &mut in_inbox)?;
    }
    let threads: Vec<ThreadId> = in_inbox.iter().copied().collect();
    let messages: Vec<Message> = vault
        .mail_in_threads(&threads)?
        .into_iter()
        .filter(|m| m.category == Some(Category::Newsletter))
        .collect();
    let found = cleanup::inbox_senders(&messages, &in_inbox, &me, since, usize::MAX);

    let followed: HashSet<String> = vault
        .news_sources()?
        .into_iter()
        .filter_map(|s| match s.kind {
            SourceKind::Mail { sender } => Some(sender.trim().to_lowercase()),
            _ => None,
        })
        .collect();
    Ok(found
        .into_iter()
        .map(|s| NewsletterSender {
            followed: followed.contains(&s.email),
            email: s.email,
            name: s.name,
            messages: s.messages,
        })
        .collect())
}

// ---- reading what a sender sent -------------------------------------------

/// A message that has not been a story yet, with what archiving it needs.
struct Issue {
    story: Story,
    thread: ThreadId,
    /// Still filed in its account's Inbox, so archiving its thread has
    /// something to do.
    in_inbox: bool,
}

struct Gathered {
    /// Oldest first.
    issues: Vec<Issue>,
    /// The newest date among this sender's messages in the window, however
    /// each was treated: the next turn starts there.
    newest: Option<Timestamp>,
}

/// Which of an account's mailboxes matter to a newsletter.
struct Folders {
    inbox: Option<MailboxId>,
    trash: HashSet<MailboxId>,
}

/// Every message from `sender` in `[from, to)`, across the accounts with mail
/// switched on, that has no story yet -- as a story waiting to be stored.
///
/// Leaves out a draft, a message marked deleted, anything in a Trash folder
/// (somebody has already thrown it away), and anything one of the source's
/// mute words matches. The window's newest date still counts for all of
/// them, so a muted issue is not read again every turn.
fn gather(
    vault: &Vault,
    source: &NewsSource,
    sender: &str,
    from: Timestamp,
    to: Timestamp,
) -> everyday_core::Result<Gathered> {
    let mut folders: HashMap<AccountId, Folders> = HashMap::new();
    let mut from_sender: Vec<Message> = Vec::new();
    if vault.supports_accounts() && vault.supports_mail() {
        for account in vault.accounts()?.into_iter().filter(|a| a.services.mail) {
            let mailboxes = vault.mailboxes(account.id)?;
            let trash =
                mailboxes.iter().filter(|m| m.role == MailboxRole::Trash).map(|m| m.id).collect();
            folders.insert(account.id, Folders { inbox: inbox_of(&mailboxes), trash });
            from_sender.extend(
                vault
                    .mail_between(account.id, from, to)?
                    .into_iter()
                    .filter(|m| is_from(m, sender) && !m.flags.draft && !m.flags.deleted),
            );
        }
    }
    from_sender.sort_by(|a, b| a.date.cmp(&b.date).then(a.id.cmp(&b.id)));
    let newest = from_sender.last().map(|m| m.date);
    if from_sender.is_empty() {
        return Ok(Gathered { issues: Vec::new(), newest });
    }

    let ids: Vec<MailMessageId> = from_sender.iter().map(|m| m.id).collect();
    let known: HashSet<MailMessageId> =
        vault.stories_for_mail(&ids)?.into_iter().map(|(message, _)| message).collect();

    let mut keys = HashSet::new();
    let mut issues = Vec::new();
    for message in from_sender {
        let title = title_of(&message.subject);
        if known.contains(&message.id) || source.mutes(&title) {
            continue;
        }
        let Some(folders) = folders.get(&message.account_id) else { continue };
        let filed = vault.mail_message_locations(message.id)?;
        if filed.iter().any(|(mailbox, _)| folders.trash.contains(mailbox)) {
            continue;
        }
        let in_inbox = filed.iter().any(|(mailbox, _)| Some(*mailbox) == folders.inbox);

        // Mail's own `Message-ID` is what makes an issue the same issue on
        // every account it was delivered to; the message's id is the stand-in
        // for the rare message that has none.
        let header = message.message_id_header.trim();
        let identity = if header.is_empty() { message.id.to_string() } else { header.to_string() };
        let key = vault.news_key(&format!("mail:{identity}"))?;
        if !keys.insert(key.clone()) {
            continue;
        }

        let mut story = Story::new(source.id, key, title, message.date);
        story.author = author_of(&message);
        story.summary = summary_of(&message.snippet);
        story.mail_message = Some(message.id);
        issues.push(Issue { story, thread: message.thread_id, in_inbox });
    }
    Ok(Gathered { issues, newest })
}

/// Is this message from `sender`, who is already lower-cased and trimmed?
fn is_from(message: &Message, sender: &str) -> bool {
    message.from.email.trim().to_lowercase() == sender
}

/// What a story is called: the subject, or Mail's own words for having none.
fn title_of(subject: &str) -> String {
    match subject.trim() {
        "" => "(no subject)".to_string(),
        subject => subject.to_string(),
    }
}

/// Who a story is by: the name on the message, else the address it came from.
fn author_of(message: &Message) -> String {
    match message.from.name.trim() {
        "" => message.from.email.trim().to_string(),
        name => name.to_string(),
    }
}

/// A story's summary is the message's snippet, held to the length a story's
/// summary may be. Mail's snippet is shorter already; the cap is so a story
/// never holds more than its record promises, whatever mail does later.
fn summary_of(snippet: &str) -> String {
    snippet.trim().chars().take(SUMMARY_CHARS).collect()
}

// ---- telling mail ---------------------------------------------------------

/// Apply `kind` to `threads` the way a person's click does -- one vault write
/// for the local change and every op -- then wake each account's sync task
/// and tell open windows the threads changed.
///
/// The same three steps as `domains::mail`'s `batch_op`, which is private to
/// that module and so cannot be called from here; what it calls is not.
async fn thread_op(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    threads: Vec<ThreadId>,
    kind: OpKind,
) -> CommandResult<()> {
    if threads.is_empty() {
        return Ok(());
    }
    let origin = Origin::Person;
    svc.check_mail_rate_limit(&origin, "person")?;
    let ops = {
        let vault = vault.clone();
        let threads = threads.clone();
        blocking(move || Ok(vault.apply_thread_ops(&threads, kind, origin)?)).await?
    };
    for account in ops.iter().map(|op| op.account_id).collect::<BTreeSet<_>>() {
        svc.notify_mail_write(account);
    }
    // One or the other, never both -- `Change`'s own contract. Mail's windows
    // reload their thread lists on this; the Feed's command announces the
    // stories itself.
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

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::id::PackId;
    use everyday_core::mail::{Address, CategorySource, MessageFlags};
    use everyday_core::packstore::PackRef;

    fn message(from: Address, subject: &str) -> Message {
        let account = AccountId::new();
        Message {
            id: MailMessageId::new(),
            account_id: account,
            thread_id: ThreadId::new(),
            message_id_header: String::new(),
            date: Timestamp::from_second(1_790_000_000).unwrap(),
            from,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: subject.into(),
            snippet: String::new(),
            flags: MessageFlags::default(),
            labels: Vec::new(),
            has_attachments: false,
            size: 0,
            category: None,
            category_source: CategorySource::Rules,
            pack: PackRef { account: account.to_string(), pack: PackId::new(), offset: 0, len: 0 },
            gmail: None,
            invite: None,
        }
    }

    #[test]
    fn a_sender_matches_without_case_or_surrounding_space() {
        let m = message(Address::new("The Weekly", " News@Example.com "), "Issue 1");
        assert!(is_from(&m, "news@example.com"));
        assert!(!is_from(&m, "other@example.com"));
    }

    #[test]
    fn a_story_is_by_the_name_on_the_message_else_its_address() {
        let named = message(Address::new(" The Weekly ", "news@example.com"), "x");
        assert_eq!(author_of(&named), "The Weekly");
        let bare = message(Address::bare(" news@example.com "), "x");
        assert_eq!(author_of(&bare), "news@example.com");
    }

    #[test]
    fn a_story_with_no_subject_says_so_as_mail_does() {
        assert_eq!(title_of("  "), "(no subject)");
        assert_eq!(title_of(" Issue 12 "), "Issue 12");
    }

    #[test]
    fn a_summary_is_held_to_the_length_a_story_allows() {
        assert_eq!(summary_of("  short  "), "short");
        let long = "é".repeat(SUMMARY_CHARS + 50);
        assert_eq!(summary_of(&long).chars().count(), SUMMARY_CHARS);
    }
}
