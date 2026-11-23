//! Newsletters from Mail: a sender somebody follows, read in the Feed, and
//! every other newsletter in Mail, each as a source of its own.
//!
//! Nothing here touches the network. A mail-backed source's turn asks the
//! mail store for what it claims after the watermark and stores one story per
//! message, pointing at it. The message stays in Mail; its body is drawn
//! through mail's own route, under mail's own picture rules; its read state
//! *is* the message's seen flag.
//!
//! # One source for each newsletter
//!
//! A newsletter is a publication, not an address: The Download and The
//! Algorithm come from one sender, and what tells them apart on a message is
//! the display name on its `From`. So the Newsletters source's turn does not
//! keep what it reads. Each message Mail files under Newsletters goes to the
//! [`Mail`](SourceKind::Mail) source that claims it -- one with this sender
//! and this display name, else one with this sender and no name -- and when
//! none does, a source is made for it, marked `found` and named after the
//! display name, unless somebody has already said that is not a newsletter
//! (`FeedSettings::not_newsletters`). A found source fetches nothing: its
//! issues arrive through the Newsletters turn, and the same turn moves the
//! stories an earlier version stored under Newsletters to the source each
//! message belongs to.
//!
//! A source a person followed is looked at first, and keeps its own turn. It
//! takes every message from its sender -- and, when it has a name, only that
//! display name -- whatever Mail calls it, and the Newsletters turn leaves
//! those to it.
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
use crate::domains::mailcleanup::{inbox_messages, inbox_of};
use crate::error::CommandResult;
use crate::events::{self, Change, Kind};
use crate::service::{Service, blocking};
use jiff::{SignedDuration, Timestamp};
use karmayoga_core::Vault;
use karmayoga_core::id::{AccountId, MailMessageId, MailboxId, NewsSourceId, StoryId, ThreadId};
use karmayoga_core::mail::{Category, MailboxRole, Message, OpKind, Origin, cleanup};
use karmayoga_core::news::{FetchState, NewsSource, SUMMARY_CHARS, SourceKind, Story, StoryBody};
use karmayoga_core::store::mail::ThreadFilter;
use karmayoga_core::store::news::StoryQuery;
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

/// How far back a source's first turn looks, in days: enough to fill the
/// list with the last few issues of a monthly, without dragging in a year of
/// somebody's archive on the strength of one tick.
const FIRST_TURN_DAYS: i64 = 30;

/// Held for a turn over what Mail files under Newsletters. Such a turn makes
/// a source for each newsletter nobody has yet, and two at once -- the
/// background pass and a refresh asked for on a found newsletter, which is a
/// turn of the Newsletters source -- would each find none and each make one.
static FILED_TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The Newsletters sources whose stories from before have been handed on
/// this run ([`rehome`]).
static REHOMED: std::sync::Mutex<BTreeSet<NewsSourceId>> = std::sync::Mutex::new(BTreeSet::new());

/// How far behind its watermark a turn that reads what Mail files under
/// Newsletters looks, in days. A message is filed there when it is sorted,
/// which can be a while after it arrived -- the decision model's pass, or a
/// person moving it by hand -- and by then a newer one may have carried the
/// watermark past its date. A story already stored for a message, expired or
/// not, keeps it from being stored again.
const NEWSLETTERS_LOOKBACK_DAYS: i64 = 3;

/// How many stories are looked at, and moved, at once when the stories
/// stored under Newsletters are handed to the sources they belong to: a page
/// of sealed stories and as many messages to read, then one write.
const REHOME_BATCH: u32 = 200;

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

/// One mail-backed source's turn: store a story for every message it claims
/// newer than `state.watermark`, advance the watermark, and -- when the
/// source keeps its issues out of the inbox -- archive each new one's thread
/// through mail's own outbox. Returns how many stories were stored.
///
/// What a source claims is [`Wanted`]. A sender somebody followed takes that
/// sender's messages. The Newsletters source takes whatever Mail files under
/// Newsletters and gives each message to the source it belongs to -- making
/// one when there is none -- after handing the stories it holds from before
/// to theirs ([`rehome`]). A found newsletter with no Newsletters source to
/// ask for it takes its own, from among the same messages.
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
    if !source.kind.is_mail() {
        return Err("this source does not follow a sender, or newsletters, in Mail".into());
    }
    let _turn = if reads_filed(source) { Some(FILED_TURN.lock().await) } else { None };
    let now = svc.now();
    if matches!(source.kind, SourceKind::Newsletters) {
        rehome(svc, vault, source.id).await?;
    }
    let from = match state.watermark {
        Some(mark) if reads_filed(source) => {
            mark - SignedDuration::from_hours(NEWSLETTERS_LOOKBACK_DAYS * 24)
        }
        Some(mark) => mark,
        None => now - SignedDuration::from_hours(FIRST_TURN_DAYS * 24),
    };

    let found = {
        let vault = vault.clone();
        let source = source.clone();
        blocking(move || Ok(gather(&vault, &source, from, now)?)).await
    }
    .map_err(|e| format!("could not read the mail this source follows: {}", e.message))?;
    if !found.created.is_empty() {
        svc.events().changed(Change::new(Kind::NewsSource, events::Op::Created));
    }

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

    // Each newsletter hears of its own new issues, once, whichever turn
    // stored them: what its automation does with them is its own.
    let mut arrived: Vec<(NewsSourceId, Vec<StoryId>)> = Vec::new();
    for issue in found.issues.iter().filter(|i| inserted.contains(&i.story.id)) {
        match arrived.iter_mut().find(|(dest, _)| *dest == issue.dest) {
            Some((_, ids)) => ids.push(issue.story.id),
            None => arrived.push((issue.dest, vec![issue.story.id])),
        }
    }
    for (dest, ids) in &arrived {
        if let Some(dest) = found.sources.get(dest) {
            super::automation::arrived(svc, dest, ids);
        }
    }

    // A thread of two new issues is archived once, and one that is no longer
    // in the Inbox -- already filed away by hand -- not at all.
    let mut seen = HashSet::new();
    let threads: Vec<ThreadId> = found
        .issues
        .iter()
        .filter(|issue| issue.keep_out && issue.in_inbox && inserted.contains(&issue.story.id))
        .map(|issue| issue.thread)
        .filter(|thread| seen.insert(*thread))
        .collect();
    thread_op(svc, vault, threads, OpKind::Archive).await.map_err(|e| {
        format!("stored the new issues but could not move them out of the inbox: {}", e.message)
    })?;
    Ok(inserted.len() as u32)
}

/// Does this source read the messages Mail files under Newsletters, which a
/// message can join after the watermark has passed it?
fn reads_filed(source: &NewsSource) -> bool {
    matches!(source.kind, SourceKind::Newsletters) || source.found
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
                    Err(karmayoga_core::Error::NotFound { .. }) => continue,
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
/// Counted as Quick Cleanup counts an Inbox -- only what is still in it, in
/// threads that are awake, nothing from you, no drafts -- by the same
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
    let mut messages: Vec<Message> = Vec::new();
    for account in vault.accounts()?.into_iter().filter(|a| a.services.mail) {
        // An account that has not synced an Inbox yet has nothing in one.
        let Some(inbox) = inbox_of(&vault.mailboxes(account.id)?) else { continue };
        messages.extend(inbox_messages(vault, inbox, since, newsletters, None)?);
    }
    messages.retain(|m| m.category == Some(Category::Newsletter));
    let found = cleanup::inbox_senders(&messages, &me, since, usize::MAX);

    let followed = followed_senders(vault)?;
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

/// The senders with a source of their own, lower-cased and trimmed -- one
/// somebody followed or one the Newsletters source found -- which is what
/// "From your mail" marks as followed: their mail is already in the Feed.
fn followed_senders(vault: &Vault) -> karmayoga_core::Result<HashSet<String>> {
    Ok(vault
        .news_sources()?
        .into_iter()
        .filter_map(|s| match s.kind {
            SourceKind::Mail { sender, .. } => Some(sender.trim().to_lowercase()),
            _ => None,
        })
        .collect())
}

// ---- reading what a source claims -----------------------------------------

/// A message that has not been a story yet, with what archiving it needs.
struct Issue {
    story: Story,
    /// The source the story is stored under.
    dest: NewsSourceId,
    thread: ThreadId,
    /// Still filed in its account's Inbox, so archiving its thread has
    /// something to do.
    in_inbox: bool,
    /// Its thread leaves the Inbox once it is stored: the source that read it,
    /// or the one it is stored under, says so.
    keep_out: bool,
}

struct Gathered {
    /// Oldest first.
    issues: Vec<Issue>,
    /// Where those issues are stored, as they were when the turn read them.
    sources: HashMap<NewsSourceId, NewsSource>,
    /// Sources made for newsletters nobody had a source for.
    created: Vec<NewsSourceId>,
    /// The newest date among the messages the turn claims in the window,
    /// however each was treated: the next turn starts there.
    newest: Option<Timestamp>,
}

/// Which of an account's mailboxes matter to a newsletter.
struct Folders {
    inbox: Option<MailboxId>,
    /// Trash and Spam: somebody threw it away, or nobody should read it.
    trash: HashSet<MailboxId>,
}

/// A Mail source, and which messages it claims.
struct Claimed {
    /// Lower-cased and trimmed.
    sender: String,
    /// Lower-cased and trimmed; empty claims every message from the sender.
    name: String,
    source: NewsSource,
}

/// Where a message belongs.
enum Home<'a> {
    /// A source that claims it.
    Claimed(&'a NewsSource),
    /// The Newsletters source's own: a message with no sender address to
    /// make a source of.
    Itself,
    /// Nowhere: somebody said it is not a newsletter.
    Nowhere,
}

/// Every Mail source, and the newsletters somebody turned down: who a message
/// belongs to is decided here, so that a turn that stores one and the
/// handing-over of what was stored earlier cannot disagree.
struct Homes {
    sources: Vec<Claimed>,
    /// `(sender, name)`, lower-cased and trimmed.
    refused: Vec<(String, String)>,
    /// The sources made since this was read.
    created: Vec<NewsSourceId>,
}

impl Homes {
    fn of(vault: &Vault) -> karmayoga_core::Result<Self> {
        let sources = vault
            .news_sources()?
            .into_iter()
            .filter_map(|source| {
                let (sender, name) = match &source.kind {
                    SourceKind::Mail { sender, name } => {
                        (sender.trim().to_lowercase(), name.trim().to_lowercase())
                    }
                    _ => return None,
                };
                Some(Claimed { sender, name, source })
            })
            .collect();
        let refused = vault
            .feed_settings()?
            .not_newsletters
            .iter()
            .map(|n| (n.sender.trim().to_lowercase(), n.name.trim().to_lowercase()))
            .collect();
        Ok(Self { sources, refused, created: Vec::new() })
    }

    /// Which source claims a message from `sender` under `name` (both
    /// lower-cased and trimmed), if one does. A source somebody followed
    /// comes before one that was found, since it takes everything it names
    /// whatever Mail calls it; and one named for this display name comes
    /// before one that takes every message from the sender. A found source
    /// with no name is only the home of the messages that carry none: had the
    /// sender's first issue come without one, it would otherwise take every
    /// newsletter they send under a name.
    fn claimed_by(&self, sender: &str, name: &str) -> Option<usize> {
        [false, true].into_iter().find_map(|found| {
            let mut theirs = self
                .sources
                .iter()
                .enumerate()
                .filter(|(_, c)| c.source.found == found && c.sender == sender);
            let exact = theirs.clone().find(|(_, c)| !c.name.is_empty() && c.name == name);
            exact
                .or_else(|| theirs.find(|(_, c)| c.name.is_empty() && (!found || name.is_empty())))
                .map(|(at, _)| at)
        })
    }

    fn lookup(&self, sender: &str, name: &str) -> Option<&Claimed> {
        self.claimed_by(sender, name).map(|at| &self.sources[at])
    }

    /// Whether somebody said a newsletter by this sender and name is not one.
    /// The name must be the same: turning down what a sender once sent with
    /// no display name says nothing of what they send under one, as a found
    /// source with no name is the home of nameless mail only.
    fn turned_down(&self, sender: &str, name: &str) -> bool {
        self.refused.iter().any(|(s, n)| s == sender && n == name)
    }

    /// Where `message` belongs, making a found source for it when none
    /// claims it and nobody has turned it down.
    fn home(&mut self, vault: &Vault, message: &Message) -> karmayoga_core::Result<Home<'_>> {
        let (sender, name) = (sender_of(message), message.from.name.trim().to_string());
        let key = name.to_lowercase();
        if let Some(at) = self.claimed_by(&sender, &key) {
            return Ok(Home::Claimed(&self.sources[at].source));
        }
        if self.turned_down(&sender, &key) {
            return Ok(Home::Nowhere);
        }
        if !sender.contains('@') {
            return Ok(Home::Itself);
        }
        let title = if name.is_empty() { sender.clone() } else { name.clone() };
        let mut source = NewsSource::new(SourceKind::Mail { sender: sender.clone(), name }, title);
        source.found = true;
        vault.save_news_source(&source)?;
        self.created.push(source.id);
        self.sources.push(Claimed { sender, name: key, source });
        Ok(Home::Claimed(&self.sources[self.sources.len() - 1].source))
    }
}

/// Whose mail a source reads.
enum Wanted {
    /// A sender somebody followed: every message from `sender`, or when
    /// `name` is not empty, from that display name. Both lower-cased and
    /// trimmed.
    Sender { sender: String, name: String },
    /// What Mail files under Newsletters, but for what a followed source
    /// claims, which is that source's. Of the rest, all of it for the
    /// Newsletters source (`only` is `None`), which gives each message to its
    /// home; or, for a found newsletter that has no Newsletters source to ask
    /// for it, just what is its own.
    Filed { homes: Homes, only: Option<NewsSourceId> },
}

impl Wanted {
    fn of(vault: &Vault, source: &NewsSource) -> karmayoga_core::Result<Self> {
        Ok(match &source.kind {
            SourceKind::Mail { sender, name } if !source.found => Wanted::Sender {
                sender: sender.trim().to_lowercase(),
                name: name.trim().to_lowercase(),
            },
            SourceKind::Mail { .. } => {
                Wanted::Filed { homes: Homes::of(vault)?, only: Some(source.id) }
            }
            _ => Wanted::Filed { homes: Homes::of(vault)?, only: None },
        })
    }

    fn wants(&self, message: &Message) -> bool {
        match self {
            Wanted::Sender { sender, name } => {
                is_from(message, sender)
                    && (name.is_empty() || message.from.name.trim().to_lowercase() == *name)
            }
            Wanted::Filed { homes, only } => {
                if message.category != Some(Category::Newsletter) {
                    return false;
                }
                let name = message.from.name.trim().to_lowercase();
                match homes.lookup(&sender_of(message), &name) {
                    Some(claimed) if !claimed.source.found => false,
                    Some(claimed) => only.is_none_or(|id| id == claimed.source.id),
                    None => only.is_none(),
                }
            }
        }
    }
}

/// Every message the source wants ([`Wanted`]) in `[from, to)`, across the
/// accounts with mail switched on, that has no story yet -- as a story
/// waiting to be stored, under the source it belongs to.
///
/// Leaves out a draft, a message marked deleted, anything in a Trash or Spam
/// folder, anything one of the source's mute words matches -- or its
/// destination's -- and what somebody has turned down as a newsletter, and a
/// found newsletter that has been paused. The window's newest date still
/// counts for all of them, so a muted issue is not read again every turn.
/// Sources are made, for the newsletters nobody has one for, only for a
/// message that is going to be stored.
fn gather(
    vault: &Vault,
    source: &NewsSource,
    from: Timestamp,
    to: Timestamp,
) -> karmayoga_core::Result<Gathered> {
    let mut wanted = Wanted::of(vault, source)?;
    let mut folders: HashMap<AccountId, Folders> = HashMap::new();
    let mut from_sender: Vec<Message> = Vec::new();
    if vault.supports_accounts() && vault.supports_mail() {
        for account in vault.accounts()?.into_iter().filter(|a| a.services.mail) {
            let mailboxes = vault.mailboxes(account.id)?;
            let trash = mailboxes
                .iter()
                .filter(|m| matches!(m.role, MailboxRole::Trash | MailboxRole::Spam))
                .map(|m| m.id)
                .collect();
            folders.insert(account.id, Folders { inbox: inbox_of(&mailboxes), trash });
            from_sender.extend(
                vault
                    .mail_between(account.id, from, to)?
                    .into_iter()
                    .filter(|m| wanted.wants(m) && !m.flags.draft && !m.flags.deleted),
            );
        }
    }
    from_sender.sort_by(|a, b| a.date.cmp(&b.date).then(a.id.cmp(&b.id)));
    let newest = from_sender.last().map(|m| m.date);
    if from_sender.is_empty() {
        return Ok(Gathered {
            issues: Vec::new(),
            sources: HashMap::new(),
            created: Vec::new(),
            newest,
        });
    }

    let ids: Vec<MailMessageId> = from_sender.iter().map(|m| m.id).collect();
    let known: HashSet<MailMessageId> =
        vault.stories_for_mail(&ids)?.into_iter().map(|(message, _)| message).collect();

    let mut keys = HashSet::new();
    let mut issues = Vec::new();
    let mut sources: HashMap<NewsSourceId, NewsSource> = HashMap::new();
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

        let home = match &mut wanted {
            Wanted::Sender { .. } => Home::Itself,
            Wanted::Filed { homes, .. } => homes.home(vault, &message)?,
        };
        let (dest, keep_out) = match home {
            Home::Itself => {
                sources.entry(source.id).or_insert_with(|| source.clone());
                (source.id, source.keep_out_of_inbox)
            }
            Home::Claimed(dest) => {
                if dest.paused || dest.mutes(&title) {
                    continue;
                }
                sources.entry(dest.id).or_insert_with(|| dest.clone());
                (dest.id, source.keep_out_of_inbox || dest.keep_out_of_inbox)
            }
            Home::Nowhere => continue,
        };

        let mut story = Story::new(dest, key, title, message.date);
        story.author = author_of(&message);
        story.summary = summary_of(&message.snippet);
        story.mail_message = Some(message.id);
        issues.push(Issue { story, dest, thread: message.thread_id, in_inbox, keep_out });
    }
    let created = match wanted {
        Wanted::Filed { homes, .. } => homes.created,
        Wanted::Sender { .. } => Vec::new(),
    };
    Ok(Gathered { issues, sources, created, newest })
}

/// What the stories stored under the Newsletters source -- by the turns that
/// read it before each newsletter had a source of its own -- came to.
#[derive(Default)]
struct Rehomed {
    stories: u32,
    created: u32,
}

/// Hand the stories still under the Newsletters source that point at a
/// message to the source each message belongs to ([`Homes`]), making found
/// sources as the messages need them. Once moved, a story is not under it
/// any more, so the next turn has nothing left to hand over but what could not
/// go: a story whose destination already has its key, one from a sender
/// somebody turned down, one whose message left Mail.
async fn rehome(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    newsletters: NewsSourceId,
) -> Result<(), String> {
    // Once a run: after the first pass, what is left under the source is what
    // cannot move -- turned down, its key already at home, no address -- and
    // nothing new is stored there, so every later turn would only read it all
    // again. Something that could move later, once a sender is followed by
    // hand, moves when the app next starts.
    if REHOMED.lock().unwrap().contains(&newsletters) {
        return Ok(());
    }
    let done = {
        let vault = vault.clone();
        blocking(move || Ok(rehome_stories(&vault, newsletters)?)).await
    }
    .map_err(|e| format!("could not give each newsletter its own source: {}", e.message))?;
    REHOMED.lock().unwrap().insert(newsletters);
    if done.created > 0 {
        svc.events().changed(Change::new(Kind::NewsSource, events::Op::Created));
    }
    if done.stories > 0 {
        svc.events().changed(Change::new(Kind::Story, events::Op::Updated));
    }
    Ok(())
}

fn rehome_stories(vault: &Vault, newsletters: NewsSourceId) -> karmayoga_core::Result<Rehomed> {
    let mut homes = Homes::of(vault)?;
    let mut done = Rehomed::default();
    let mut before = None;
    loop {
        let query = StoryQuery {
            sources: vec![newsletters],
            before,
            limit: REHOME_BATCH,
            ..Default::default()
        };
        let page = vault.stories(&query)?;
        let mut moves = Vec::new();
        for story in &page.stories {
            let Some(id) = story.mail_message else { continue };
            let message = match vault.mail_message(id) {
                Ok(message) => message,
                // Deleted in Mail: not listed, so only a race gets here.
                Err(karmayoga_core::Error::NotFound { .. }) => continue,
                Err(e) => return Err(e),
            };
            if let Home::Claimed(dest) = homes.home(vault, &message)? {
                moves.push((story.id, dest.id));
            }
        }
        if !moves.is_empty() {
            done.stories += vault.move_stories(&moves)?;
        }
        match page.next {
            Some(next) => before = Some(next),
            None => break,
        }
    }
    done.created = u32::try_from(homes.created.len()).unwrap_or(u32::MAX);
    Ok(done)
}

/// The sender of a message, lower-cased and trimmed: how a sender is compared.
fn sender_of(message: &Message) -> String {
    message.from.email.trim().to_lowercase()
}

/// Is this message from `sender`, who is already lower-cased and trimmed?
fn is_from(message: &Message, sender: &str) -> bool {
    sender_of(message) == sender
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
    use karmayoga_core::id::PackId;
    use karmayoga_core::mail::{Address, CategorySource, MessageFlags};
    use karmayoga_core::packstore::PackRef;

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
