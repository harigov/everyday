//! The Feed: the sources somebody follows, the stories they bring, and the
//! few things that reach outside it -- a story saved to the library, a
//! newsletter marked read through mail.
//!
//! See `crate::news` for the moving parts and `docs/plans/feed.md` for why
//! it is shaped this way.
//!
//! The same division of labour the calendar and the library make:
//!
//!   this file         what to follow, what to do when it is already followed,
//!                     and what a command may write
//!   news/fetch.rs     turning an address into bytes -- the typed one over the
//!                     person's own client, the ones a feed names over the
//!                     public one
//!   news/refresh.rs   one source's turn
//!   jatayu-core     the records, the arithmetic and the store
//!
//! Nothing here fetches in order to *decide* anything the person did not ask
//! for. Following an address looks it up once, to give it a name; everything
//! else that touches the network is a refresh, a full article, or a picture,
//! and each of those is a button.

use crate::command;
use crate::ctx::{Ctx, Scope};
use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Change, Kind, Op};
use crate::mail::unsubscribe::{self, Target, UnsubscribeOutcome};
use crate::news::{FoundFeed, RefreshReport, canonical, extract, fetch, newsletters, refresh};
use crate::service::{Service, blocking};
use jatayu_core::id::AccountId;
use jatayu_core::library::Item;
use jatayu_core::mail::{Category, Message};
use jatayu_core::news::opml::Outline;
use jatayu_core::news::{
    HnList, NewsSource, NewsletterRef, PodcastSettings, SourceKind, SourceSummary, Story,
};
use jatayu_core::packstore::PackStore;
use jatayu_core::store::library::ItemQuery;
use jatayu_core::store::news::{StoryPage, StoryQuery};
use jatayu_core::{ItemId, KindId, NewsSourceId, StoryId, Vault};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use super::Nothing;

/// Where Hacker News lives, for "open the site" and for its icon.
const HN_SITE: &str = "https://news.ycombinator.com";

/// What `add_news_source` is asked to follow.
///
/// Defined here, not in the core, because it is a wire type and nothing else:
/// the core's [`SourceKind`] is what is *stored*, and differs from this in
/// what it lets a caller leave out. A caller says "this address" and the
/// command works out the name, the site and the icon; a stored source has
/// all of them.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum NewSource {
    #[serde(rename_all = "camelCase")]
    Web {
        url: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        folder: Option<String>,
    },
    /// A show from the podcast directory (`crate::news::directory`): a web
    /// feed that is known to be a podcast from the start, which is what lets
    /// its first episodes be downloaded by the show's policies. The address
    /// is a stranger's, so unlike a typed [`NewSource::Web`] it may not be one
    /// on the person's own network.
    #[serde(rename_all = "camelCase")]
    Podcast {
        url: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        folder: Option<String>,
        /// The directory's own id for the show, kept in its settings.
        #[serde(default)]
        directory_id: Option<String>,
        #[serde(default)]
        author: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    HackerNews {
        list: HnList,
        #[serde(default)]
        min_points: Option<u32>,
        #[serde(default)]
        folder: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Mail {
        sender: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        folder: Option<String>,
    },
    /// Every newsletter in Mail at once -- see [`SourceKind::Newsletters`].
    #[serde(rename_all = "camelCase")]
    Newsletters {
        #[serde(default)]
        folder: Option<String>,
    },
}

/// What an OPML import did.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpmlImportReport {
    /// Sources followed that were not before.
    pub added: u32,
    /// Outlines already followed, or not a feed.
    pub skipped: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Discover {
    pub url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddSource {
    pub source: NewSource,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSource {
    pub source: NewsSource,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRef {
    pub id: NewsSourceId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stories {
    pub query: StoryQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoryRef {
    pub id: StoryId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkStories {
    pub ids: Vec<StoryId>,
    pub read: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkAll {
    pub sources: Vec<NewsSourceId>,
    pub up_to: Timestamp,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveStories {
    pub ids: Vec<StoryId>,
    pub saved: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshNews {
    #[serde(default)]
    pub source: Option<NewsSourceId>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOpml {
    pub xml: String,
}

// ---- sources ------------------------------------------------------------

/// Every source with its counts and its health.
///
/// A newsletter the Newsletters source found has no fetch state of its own --
/// it fetches nothing, its issues arrive when that source takes its turn --
/// and would be drawn as never having been fetched. It is given the
/// Newsletters source's instead, which is when it was last read.
async fn list_news_sources(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<SourceSummary>> {
    // What the Feed asks for first when it opens: the background tasks still
    // waiting out their first delay after an unlock start now.
    svc.listening.opened.now();
    svc.on_vault(|vault| {
        let mut summaries = vault.news_source_summaries()?;
        let letters = summaries
            .iter()
            .find(|s| matches!(s.source.kind, SourceKind::Newsletters))
            .map(|s| (s.last_ok, s.last_error.clone(), s.next_at));
        if let Some((last_ok, last_error, next_at)) = letters {
            for summary in summaries.iter_mut().filter(|s| s.source.found && s.next_at.is_none()) {
                summary.last_ok = last_ok;
                summary.last_error = last_error.clone();
                summary.next_at = next_at;
            }
        }
        Ok(summaries)
    })
    .await
}

/// Every feed at, or advertised by, an address somebody pasted.
///
/// A request leaving the machine, so a locked vault does not make it. The
/// error is the sentence `fetch::discover` wrote, unchanged: it already has
/// the address stripped out, and "that page did not answer" is more use to a
/// person than any code.
async fn discover_feeds(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: Discover,
) -> CommandResult<Vec<FoundFeed>> {
    svc.require_unlocked()?;
    let url = web_address(&args.url)?;
    fetch::discover(&url).await.map_err(|message| CommandError::new(codes::NETWORK, message))
}

/// Follow something, and show its first stories before the call returns.
///
/// Written in the order the library's `add_item` is, for its reason: the
/// source goes to disk *before* anything slow. The icon and the first fetch
/// can each take their full timeout, and a source that was lost to a quit
/// during them would be a thing somebody chose and has to choose again.
/// Enriching costs a second write; getting this backwards costs the choice.
///
/// Neither slow step is allowed to fail the add. An icon nobody could find is
/// a source with the generic one, and a first fetch that fails is a source
/// that says why in the sidebar -- the refresher records it on the fetch
/// state, where every later failure goes -- and tries again on its schedule.
async fn add_news_source(
    svc: Arc<Service>,
    ctx: Ctx,
    args: AddSource,
) -> CommandResult<NewsSource> {
    let vault = svc.require_unlocked()?;
    let existing = {
        let vault = vault.clone();
        blocking(move || Ok(vault.news_sources()?)).await?
    };
    let source = match args.source {
        NewSource::Web { url, title, folder } => {
            followed_web(&existing, &url, title, folder).await?
        }
        NewSource::Podcast { url, title, folder, directory_id, author } => {
            podcast_source(&existing, &url, title, folder, directory_id, author)?
        }
        NewSource::HackerNews { list, min_points, folder } => {
            let min_points = min_points.unwrap_or(0);
            let kind = SourceKind::HackerNews { list, min_points };
            refuse_duplicate(&existing, &kind)?;
            let mut source = NewsSource::new(kind, hn_title(list, min_points));
            source.site_url = HN_SITE.to_string();
            source.folder = folder.map(|f| f.trim().to_string()).unwrap_or_default();
            source
        }
        NewSource::Mail { sender, title, folder } => {
            // Stories that point at mail need mail to point at. The whole
            // section is hidden from the interface without it; this is the
            // same rule for a caller that never saw the interface.
            if !vault.supports_mail() {
                return Err(CommandError::new(
                    codes::UNSUPPORTED,
                    "newsletters are read from Mail, and this vault does not keep any",
                ));
            }
            let sender = sender.trim().to_lowercase();
            if !sender.contains('@') {
                return Err(CommandError::new(
                    codes::INVALID,
                    "give the email address the newsletter comes from",
                ));
            }
            let kind = SourceKind::Mail { sender: sender.clone(), name: String::new() };
            // A newsletter the Newsletters source found under this address is
            // not refused as followed already: following it makes it the
            // person's, with a turn of its own that takes everything the
            // sender sends, as any followed sender's does.
            if let Some(found) = existing.iter().find(|s| s.found && s.kind == kind) {
                let mut adopted = found.clone();
                adopted.found = false;
                if let Some(title) = clean(title) {
                    adopted.title = title;
                }
                if let Some(folder) = folder {
                    adopted.folder = folder.trim().to_string();
                }
                adopted
            } else {
                refuse_duplicate(&existing, &kind)?;
                let title = clean(title).unwrap_or(sender);
                let mut source = NewsSource::new(kind, title);
                source.folder = folder.map(|f| f.trim().to_string()).unwrap_or_default();
                source
            }
        }
        NewSource::Newsletters { folder } => {
            if !vault.supports_mail() {
                return Err(CommandError::new(
                    codes::UNSUPPORTED,
                    "newsletters are read from Mail, and this vault does not keep any",
                ));
            }
            let kind = SourceKind::Newsletters;
            refuse_duplicate(&existing, &kind)?;
            let mut source = NewsSource::new(kind, "Newsletters");
            source.folder = folder.map(|f| f.trim().to_string()).unwrap_or_default();
            source
        }
    };

    let id = source.id;
    {
        let (vault, source) = (vault.clone(), source.clone());
        blocking(move || Ok(vault.save_news_source(&source)?)).await?;
    }
    if let SourceKind::Mail { sender, name } = &source.kind {
        let (vault, sender, name) = (vault.clone(), sender.clone(), name.clone());
        blocking(move || {
            file_as_newsletter(&vault, &sender);
            if let Err(e) = take_back(&vault, &sender, &name) {
                tracing::info!(error = %e, "could not take a followed sender off the list of non-newsletters");
            }
            Ok(())
        })
        .await?;
    }

    // A show has no site yet -- the host of its feed is a podcast host's, and
    // its favicon is no icon for the show. The first read of the feed brings
    // the show's own picture (`refresh::learn`).
    if !source.site_url.is_empty()
        && let Some(icon) = fetch::site_icon(&vault, &source.site_url).await
    {
        // Read again rather than saving the copy built above: the icon
        // fetch took seconds, and a rename made in that time is the person's.
        let vault = vault.clone();
        blocking(move || {
            let mut stored = vault.news_source(id)?;
            stored.icon = Some(icon);
            Ok(vault.save_news_source(&stored)?)
        })
        .await?;
    }

    let report = refresh::refresh_source(&svc, &vault, id).await;
    if report.inserted > 0 {
        // The stories are a write of their own, and another window showing
        // the list has not been told about them: the command's declared
        // change names the source.
        svc.events().changed(Change {
            kind: Kind::Story,
            op: Op::Created,
            id: None,
            ids: Vec::new(),
            origin: ctx.caller.origin().map(str::to_string),
        });
    }

    blocking(move || Ok(vault.news_source(id)?)).await
}

/// A podcast from the directory, as a source: a web feed with its show
/// settings begun, named as the directory named it.
///
/// Refused when its address is on the person's own network, as an OPML
/// import refuses one: the address came from a directory, which is a
/// stranger's text, and the refresher would fetch it from inside the network
/// every few minutes on the stranger's say-so. A show of their own on their
/// own network is followed by pasting its address as a feed, which is the one
/// gesture that is theirs -- and is a podcast as soon as it has been read.
fn podcast_source(
    existing: &[NewsSource],
    url: &str,
    title: Option<String>,
    folder: Option<String>,
    directory_id: Option<String>,
    author: Option<String>,
) -> CommandResult<NewsSource> {
    let url = web_address(url)?;
    if names_own_network(&url) {
        return Err(CommandError::new(
            codes::INVALID,
            "a podcast from the directory cannot be on your own network; paste its address as a \
             feed instead",
        ));
    }
    refuse_duplicate(existing, &SourceKind::Web { url: url.clone() })?;
    let title = clean(title).unwrap_or_else(|| host_title(&url));
    let mut source = NewsSource::new(SourceKind::Web { url }, title);
    source.folder = folder.map(|f| f.trim().to_string()).unwrap_or_default();
    source.podcast = Some(PodcastSettings {
        author: clean(author).unwrap_or_default(),
        directory_id: clean(directory_id),
        ..PodcastSettings::default()
    });
    Ok(source)
}

/// Following a sender in the Feed says what their mail is: a newsletter. So
/// Mail is told the same, through the per-sender correction a person makes
/// when they move a thread to the Newsletters tab -- in every account that
/// syncs mail, since nothing says which one the sender writes to.
///
/// Never over a correction the person already made. Somebody who pinned this
/// sender to Priority has said what it is to them, and following it here is
/// no reason to argue. Best effort: the source is followed either way, and a
/// mailbox that could not be told is a tab that is merely out of date.
fn file_as_newsletter(vault: &Vault, sender: &str) {
    let Ok(accounts) = vault.accounts() else { return };
    for account in accounts.iter().filter(|a| a.services.mail) {
        let pinned = vault.category_rules(account.id).ok().and_then(|r| r.for_sender(sender));
        if pinned.is_some() {
            continue;
        }
        if let Err(e) = vault.correct_mail_category(account.id, sender, Category::Newsletter) {
            tracing::info!(error = %e, "could not file a followed sender as a newsletter");
        }
    }
}

/// A web source for an address somebody gave, named and with its site worked
/// out. Looks the address up once, and only to be helpful: a lookup that
/// fails still follows the address as given.
async fn followed_web(
    existing: &[NewsSource],
    url: &str,
    title: Option<String>,
    folder: Option<String>,
) -> CommandResult<NewsSource> {
    let url = web_address(url)?;
    refuse_duplicate(existing, &SourceKind::Web { url: url.clone() })?;

    let found = match fetch::discover(&url).await {
        Ok(found) => found,
        Err(e) => {
            tracing::info!(error = %e, "could not look up a feed before following it");
            Vec::new()
        }
    };
    // Given a feed, that is the feed. Given a page, the first feed it
    // advertises -- a caller that wanted a choice asked `discover_feeds`
    // first, and one that did not wants the obvious one rather than a page
    // that can never parse. The page is then the site.
    let (feed_url, site_url) = match found.first() {
        Some(feed) if !same_address(&feed.url, &url) => (feed.url.clone(), url.clone()),
        _ => (url.clone(), origin_of(&url)),
    };
    refuse_duplicate(existing, &SourceKind::Web { url: feed_url.clone() })?;

    let title = clean(title)
        .or_else(|| clean(found.first().map(|f| f.title.clone())))
        .unwrap_or_else(|| host_title(&feed_url));
    let mut source = NewsSource::new(SourceKind::Web { url: feed_url }, title);
    source.site_url = site_url;
    source.folder = folder.map(|f| f.trim().to_string()).unwrap_or_default();
    Ok(source)
}

/// Save a source as a person edited it.
///
/// Whether it was found is not theirs to say: it is the stored copy's, so a
/// caller cannot make a source one the Newsletters source found, and one that
/// was found stays so until it is deleted.
async fn save_news_source(svc: Arc<Service>, _ctx: Ctx, args: SaveSource) -> CommandResult<()> {
    svc.on_vault(move |vault| {
        let mut source = args.source;
        source.found = match vault.news_source(source.id) {
            Ok(stored) => stored.found,
            Err(jatayu_core::Error::NotFound { .. }) => false,
            Err(e) => return Err(e),
        };
        vault.save_news_source(&source)
    })
    .await
}

/// Unfollow: the source, every story it brought -- saved ones too, because
/// unfollowing is the explicit act -- their bodies and its fetch state.
///
/// Unfollowing a newsletter the Newsletters source found is saying it is not
/// one, so it goes on the list of those -- before it is deleted, so that a
/// failure to write the list leaves the source where it was and not gone and
/// back with the next issue.
async fn delete_news_source(svc: Arc<Service>, _ctx: Ctx, args: SourceRef) -> CommandResult<()> {
    let id = args.id;
    svc.on_vault(move |vault| {
        match vault.news_source(id) {
            Ok(NewsSource { found: true, kind: SourceKind::Mail { sender, name }, .. }) => {
                turn_down(vault, &[NewsletterRef { sender, name }])?;
            }
            Ok(_) | Err(jatayu_core::Error::NotFound { .. }) => {}
            Err(e) => return Err(e),
        }
        vault.delete_news_source(id)
    })
    .await?;
    // Forgotten, so that following it again and watching it fail is news again.
    svc.news_recovered(id);
    Ok(())
}

// ---- leaving many at once -------------------------------------------------

/// How many of a newsletter's newest messages are looked through for one
/// whose header says how to leave its list. Only a few of them have their
/// own bytes read for it ([`unsubscribe::MAX_READS`]); the rest are what
/// their records kept.
const UNSUBSCRIBE_CANDIDATES: usize = 20;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceIds {
    pub ids: Vec<NewsSourceId>,
}

/// Stop getting each of `ids`: a newsletter's sender is asked to take this
/// person off their list (see [`crate::mail::unsubscribe`]) and the source
/// leaves the Feed, turned down so it is not found again; a web feed or a
/// podcast is unfollowed. Needs `Mail` as well as `News` when any of them is
/// a newsletter, since asking sends a request in the person's name.
///
/// An id that is not there any more is passed over, and so is the
/// Newsletters source, which is every newsletter at once and never one to
/// leave. The answer is one outcome for each newsletter in each account its
/// mail reached; a web feed has nobody to ask, and gets none.
///
/// Everything asking needs is read before anything is deleted, since
/// deleting a source takes the stories that say which mail it brought. The
/// senders are asked next, and only then are the sources removed: a sender
/// who refused is still a source gone, as the person asked, and their
/// outcome says what is left for them to do.
///
/// A newsletter is turned down whether it was found or followed by hand,
/// unlike [`delete_news_source`], which turns down only what was found:
/// either way its mail goes on being filed under Newsletters for a while --
/// issues already sent, and a followed sender's correction in Mail -- and
/// with nothing left to claim it the Newsletters source would find it again.
async fn unsubscribe_news_sources(
    svc: Arc<Service>,
    ctx: Ctx,
    args: SourceIds,
) -> CommandResult<Vec<UnsubscribeOutcome>> {
    // The lock screen is not a place requests leave the machine from: see
    // `domains::mailunsub`.
    let vault = svc.require_unlocked()?;
    let sources = {
        let vault = vault.clone();
        let ids = args.ids;
        blocking(move || Ok(leavable(&vault, &ids)?)).await?
    };
    let letters: Vec<NewsSource> =
        sources.iter().filter(|s| matches!(s.kind, SourceKind::Mail { .. })).cloned().collect();
    if !letters.is_empty() {
        ctx.require(Scope::Mail)?;
    }

    let Leaving { targets, mut unoffered, refused } = if letters.is_empty() {
        Leaving::default()
    } else {
        let (asker, vault, packs) = (svc.clone(), vault.clone(), svc.packs());
        blocking(move || ways_out(&asker, &vault, packs.as_deref(), &letters)).await?
    };
    let mut outcomes = unsubscribe::unsubscribe_all(&svc, &vault, targets).await;
    outcomes.append(&mut unoffered);

    let gone: Vec<NewsSourceId> = sources.iter().map(|s| s.id).collect();
    remove_sources(&svc, &vault, gone, refused).await?;
    // Mail says "you asked on ..." from the message each sender was asked
    // from, which this command's own change says nothing about. Raised for
    // every window, the one that asked included: its Mail is not its Feed.
    if outcomes.iter().any(|o| o.done) {
        svc.events().changed(Change::new(Kind::Thread, Op::Updated));
    }
    Ok(outcomes)
}

/// The sources among `ids` that can be left, in the sidebar's order: each
/// once, passing over one that is not there any more and the Newsletters
/// source.
fn leavable(vault: &Vault, ids: &[NewsSourceId]) -> jatayu_core::Result<Vec<NewsSource>> {
    let mut sources = vault.news_sources()?;
    sources.retain(|s| ids.contains(&s.id) && !matches!(s.kind, SourceKind::Newsletters));
    Ok(sources)
}

/// What leaving some newsletters needs, read before any of them is deleted.
#[derive(Default)]
struct Leaving {
    /// The message whose header says how to leave each newsletter's list, in
    /// each account its mail reached.
    targets: Vec<Target>,
    /// An outcome for each account where a newsletter's mail named no way
    /// out, or for a newsletter with no mail left at all.
    unoffered: Vec<UnsubscribeOutcome>,
    /// What turning them down puts on the list of non-newsletters
    /// ([`refusals`]).
    refused: Vec<NewsletterRef>,
}

/// [`Leaving`] for each newsletter in `letters`.
///
/// Each target is named as the Feed names the newsletter, so the outcomes
/// read as the sidebar did. Two newsletters whose mail names the very same
/// way out in one account are one list: it is asked once, under both names,
/// rather than sent the same email twice.
///
/// A newsletter with no mail left at all -- no story pointing at any, and
/// nothing in the last ninety days -- is answered for under the first account
/// with mail switched on, so the person hears that nothing could be asked
/// rather than nothing at all. With no such account there is nobody to answer
/// for, and it is only removed.
fn ways_out(
    svc: &Arc<Service>,
    vault: &Vault,
    packs: Option<&dyn PackStore>,
    letters: &[NewsSource],
) -> CommandResult<Leaving> {
    let mail = newsletters::source_messages(svc, vault, letters, UNSUBSCRIBE_CANDIDATES)?;
    let first_account: Option<AccountId> = if vault.supports_accounts() {
        vault.accounts()?.into_iter().find(|a| a.services.mail).map(|a| a.id)
    } else {
        None
    };
    let mut targets: Vec<Target> = Vec::new();
    let mut unoffered: Vec<UnsubscribeOutcome> = Vec::new();
    let mut refused: Vec<NewsletterRef> = Vec::new();
    for source in letters {
        let SourceKind::Mail { sender, .. } = &source.kind else { continue };
        let messages = mail.get(&source.id).map_or(&[][..], Vec::as_slice);
        refused.extend(refusals(source, messages));
        let label = source.title.trim();
        if messages.is_empty() {
            if let Some(account) = first_account {
                unoffered.push(unsubscribe::nothing_offered(account, sender, label));
            }
            continue;
        }
        // Newest first within each account, as `first_target` wants them,
        // and the accounts in the order their newest issue arrived.
        let mut accounts: Vec<AccountId> = Vec::new();
        for message in messages {
            if !accounts.contains(&message.account_id) {
                accounts.push(message.account_id);
            }
        }
        for account in accounts {
            let theirs: Vec<Message> =
                messages.iter().filter(|m| m.account_id == account).cloned().collect();
            let Some(mut target) =
                unsubscribe::first_target(packs, &theirs, unsubscribe::MAX_READS)
            else {
                unoffered.push(unsubscribe::nothing_offered(account, sender, label));
                continue;
            };
            let same = targets
                .iter_mut()
                .find(|t| t.account == target.account && t.header.links == target.header.links);
            match same {
                Some(same) => same.name = format!("{}, {label}", same.name),
                None => {
                    target.name = label.to_string();
                    targets.push(target);
                }
            }
        }
    }
    Ok(Leaving { targets, unoffered, refused })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotNewsletters {
    pub ids: Vec<NewsSourceId>,
    /// Where their mail goes in Mail instead of Newsletters; left in
    /// Newsletters when absent. Needs the `Mail` scope when given.
    #[serde(default)]
    pub category: Option<Category>,
}

/// "These are not newsletters": every newsletter among `ids` is turned down
/// (in one save of the Feed's settings) and removed from the Feed, and, with
/// `category`, its sender's mail is moved there in Mail -- unless another
/// newsletter that stays in the Feed comes from the same address.
///
/// Anything among `ids` that is not a newsletter of one sender's -- a web
/// feed, a podcast, the Newsletters source -- is left alone: none of them is
/// what this is saying no to. A newsletter is turned down whether it was
/// found or followed by hand, since the person says it is not one either way.
///
/// The `Mail` scope is asked for whenever a category is given, before
/// anything is written, so a connection that may not touch Mail cannot get
/// halfway.
async fn mark_not_newsletters(
    svc: Arc<Service>,
    ctx: Ctx,
    args: NotNewsletters,
) -> CommandResult<NotNewslettersReport> {
    let NotNewsletters { ids, category } = args;
    if category.is_some() {
        ctx.require(Scope::Mail)?;
    }
    let vault = svc.require()?;
    let NotOnes { letters, kept, refused } = {
        let (reader, vault) = (svc.clone(), vault.clone());
        blocking(move || not_newsletters(&reader, &vault, &ids)).await?
    };
    let mut report = NotNewslettersReport { removed: letters.len() as u32, ..Default::default() };
    if letters.is_empty() {
        return Ok(report);
    }
    let gone: Vec<NewsSourceId> = letters.iter().map(|s| s.id).collect();
    remove_sources(&svc, &vault, gone, refused).await?;

    let Some(category) = category else { return Ok(report) };
    let (moved, shared) = blocking(move || Ok(refile(&vault, &letters, &kept, category)?)).await?;
    (report.moved, report.shared) = (moved, shared);
    // Mail's lists are drawn from what moved, which this command's own change
    // says nothing about. Raised for every window, the one that asked
    // included: its Mail is not its Feed.
    if moved > 0 {
        svc.events().changed(Change::new(Kind::Thread, Op::Updated));
    }
    Ok(report)
}

/// What [`mark_not_newsletters`] did, so the interface says what happened
/// rather than what was asked: mail stays put when another newsletter still
/// comes from its address, or when nothing in Mail was theirs.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotNewslettersReport {
    /// How many newsletters left the Feed.
    pub removed: u32,
    /// How many of their messages moved to the category asked for in Mail.
    pub moved: u32,
    /// How many of them share an address with a newsletter that stays, so
    /// their mail was left where it was.
    pub shared: u32,
}

/// The newsletters somebody said are not, read before any is deleted.
struct NotOnes {
    /// The newsletters among the ids given.
    letters: Vec<NewsSource>,
    /// Every other newsletter, as it stands.
    kept: Vec<NewsSource>,
    /// What turning `letters` down puts on the list of non-newsletters.
    refused: Vec<NewsletterRef>,
}

/// [`NotOnes`] for `ids`.
fn not_newsletters(
    svc: &Arc<Service>,
    vault: &Vault,
    ids: &[NewsSourceId],
) -> CommandResult<NotOnes> {
    let (letters, kept): (Vec<NewsSource>, Vec<NewsSource>) = vault
        .news_sources()?
        .into_iter()
        .filter(|s| matches!(s.kind, SourceKind::Mail { .. }))
        .partition(|s| ids.contains(&s.id));
    // Only a sender followed as a whole needs its mail read, for the names it
    // sent under; every other newsletter is one name already.
    let wholes: Vec<NewsSource> = letters.iter().filter(|s| followed_whole(s)).cloned().collect();
    let seen = if wholes.is_empty() {
        HashMap::new()
    } else {
        newsletters::source_messages(svc, vault, &wholes, UNSUBSCRIBE_CANDIDATES)?
    };
    let refused: Vec<NewsletterRef> = letters
        .iter()
        .flat_map(|s| refusals(s, seen.get(&s.id).map_or(&[][..], Vec::as_slice)))
        .collect();
    Ok(NotOnes { letters, kept, refused })
}

/// File the mail of every sender among `letters` that no newsletter in
/// `kept` still claims under `category`, in every account with mail
/// switched on -- nothing says which of them a sender writes to, as
/// [`file_as_newsletter`] found -- with one sweep of each account for all of
/// them. Returns how many messages moved, and how many of `letters` were
/// left alone because a newsletter in `kept` still reads their address.
///
/// Mail files by address and the Feed by address and display name, so one
/// address can be several newsletters: MIT Technology Review sends The
/// Download and The Algorithm from one. Moving that address's mail because
/// one of them is not a newsletter would move the other's too, while it is
/// still in the Feed, so an address any remaining newsletter reads is left
/// where it is. A remaining one that has been turned down claims nothing.
///
/// Over a correction already made, too: following a sender filed it as a
/// newsletter, and the person now says otherwise.
fn refile(
    vault: &Vault,
    letters: &[NewsSource],
    kept: &[NewsSource],
    category: Category,
) -> jatayu_core::Result<(u32, u32)> {
    if !vault.supports_accounts() || !vault.supports_mail() {
        return Ok((0, 0));
    }
    let refused = vault.feed_settings()?.not_newsletters;
    let turned_down = |sender: &str, name: &str| {
        refused.iter().any(|n| n.sender == sender && n.name.trim().to_lowercase() == name)
    };
    let claimed: HashSet<String> = kept
        .iter()
        .filter_map(|s| match &s.kind {
            SourceKind::Mail { sender, name } => {
                let (sender, name) = (sender.trim().to_lowercase(), name.trim().to_lowercase());
                (!turned_down(&sender, &name)).then_some(sender)
            }
            _ => None,
        })
        .collect();
    let shared =
        letters.iter().filter_map(newsletter_sender).filter(|s| claimed.contains(s)).count() as u32;
    let senders: BTreeSet<String> = letters
        .iter()
        .filter_map(newsletter_sender)
        .filter(|sender| !claimed.contains(sender))
        .collect();
    if senders.is_empty() {
        return Ok((0, shared));
    }
    let corrections: Vec<(String, Category)> =
        senders.into_iter().map(|sender| (sender, category)).collect();
    let mut moved: u32 = 0;
    for account in vault.accounts()?.into_iter().filter(|a| a.services.mail) {
        moved = moved.saturating_add(vault.correct_mail_categories(account.id, &corrections)?);
    }
    Ok((moved, shared))
}

/// Take `gone` out of the Feed as [`delete_news_source`] does, having put
/// `refused` on the list of non-newsletters first, in one save: a failure to
/// write the list leaves every source where it was rather than gone and back
/// with the next issue.
async fn remove_sources(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    gone: Vec<NewsSourceId>,
    refused: Vec<NewsletterRef>,
) -> CommandResult<()> {
    let listed = !refused.is_empty();
    {
        let (vault, gone) = (vault.clone(), gone.clone());
        blocking(move || {
            turn_down(&vault, &refused)?;
            for id in gone {
                vault.delete_news_source(id)?;
            }
            Ok(())
        })
        .await?;
    }
    for id in gone {
        svc.news_recovered(id);
    }
    // Settings → Feed lists what has been turned down, and it is drawn from
    // the settings record, which neither command's own change names.
    if listed {
        svc.events().changed(Change::new(Kind::Settings, Op::Updated));
    }
    Ok(())
}

/// The sender a newsletter of one sender's reads, lower-cased and trimmed;
/// `None` for any other source.
fn newsletter_sender(source: &NewsSource) -> Option<String> {
    match &source.kind {
        SourceKind::Mail { sender, .. } => Some(sender.trim().to_lowercase()),
        _ => None,
    }
}

/// A sender followed by hand as a whole, rather than one newsletter of
/// theirs: it takes every display name they send under.
fn followed_whole(source: &NewsSource) -> bool {
    !source.found && matches!(&source.kind, SourceKind::Mail { name, .. } if name.trim().is_empty())
}

/// What turning `source` down puts on the list of non-newsletters: its
/// sender and name -- and, for a sender followed as a whole, each display
/// name `messages` came under as well. A name turned down is matched exactly,
/// so with the source gone the Newsletters source would otherwise find each
/// of those again, as a newsletter of its own, with the next issue.
fn refusals(source: &NewsSource, messages: &[Message]) -> Vec<NewsletterRef> {
    let SourceKind::Mail { sender, name } = &source.kind else { return Vec::new() };
    let mut refs = vec![NewsletterRef { sender: sender.clone(), name: name.clone() }];
    if followed_whole(source) {
        for message in messages {
            let shown = message.from.name.trim();
            let listed = refs.iter().any(|r| r.name.to_lowercase() == shown.to_lowercase());
            if !shown.is_empty() && !listed {
                refs.push(NewsletterRef { sender: sender.clone(), name: shown.to_string() });
            }
        }
    }
    refs
}

/// Put newsletters on the list of those somebody said are not newsletters,
/// so the Newsletters source does not find them again -- in one save of the
/// Feed's settings however many there are, since the Settings page saves the
/// same record and a save for each would race it. Once: a sender and name
/// already listed, whatever its case, are not listed twice, and nothing is
/// saved when every one of them already was.
fn turn_down(vault: &Vault, refs: &[NewsletterRef]) -> jatayu_core::Result<()> {
    if refs.is_empty() {
        return Ok(());
    }
    let mut settings = vault.feed_settings()?;
    let before = settings.not_newsletters.len();
    for newsletter in refs {
        let sender = newsletter.sender.trim().to_lowercase();
        let name = newsletter.name.trim().to_string();
        let listed = settings
            .not_newsletters
            .iter()
            .any(|n| n.sender == sender && n.name.to_lowercase() == name.to_lowercase());
        if !listed {
            settings.not_newsletters.push(NewsletterRef { sender, name });
        }
    }
    if settings.not_newsletters.len() != before {
        vault.save_feed_settings(&settings)?;
    }
    Ok(())
}

/// Take what a followed source covers off the list of non-newsletters. A
/// source that follows the sender as a whole covers every name they send
/// under; one that follows a name covers that name.
fn take_back(vault: &Vault, sender: &str, name: &str) -> jatayu_core::Result<()> {
    let mut settings = vault.feed_settings()?;
    let (sender, name) = (sender.trim().to_lowercase(), name.trim().to_lowercase());
    let before = settings.not_newsletters.len();
    settings
        .not_newsletters
        .retain(|n| !(n.sender == sender && (name.is_empty() || n.name.to_lowercase() == name)));
    if settings.not_newsletters.len() != before {
        vault.save_feed_settings(&settings)?;
    }
    Ok(())
}

// ---- stories ------------------------------------------------------------

async fn list_stories(svc: Arc<Service>, _ctx: Ctx, args: Stories) -> CommandResult<StoryPage> {
    svc.on_vault(move |vault| vault.stories(&args.query)).await
}

async fn get_story(svc: Arc<Service>, _ctx: Ctx, args: StoryRef) -> CommandResult<Story> {
    svc.on_vault(move |vault| vault.story(args.id)).await
}

/// Mark stories read or unread.
///
/// The vault is written first, for every story, and mail second for the ones
/// that are mail. A newsletter's own `read` column means nothing -- its read
/// state is its message's seen flag, and only mail can change that, so that
/// the server hears of it -- but writing the column costs nothing, and
/// leaving the stories beside it unmarked because mail was unreachable would
/// make one failure look like two. If mail refuses, the command says so and
/// the newsletter stays as unread as the message is.
async fn mark_stories_read(svc: Arc<Service>, _ctx: Ctx, args: MarkStories) -> CommandResult<()> {
    let vault = svc.require()?;
    let MarkStories { ids, read } = args;
    let mail_backed = {
        let vault = vault.clone();
        blocking(move || {
            vault.set_stories_read(&ids, read)?;
            let stories = vault.stories_by_id(&ids)?;
            Ok(stories.into_iter().filter(|s| s.mail_message.is_some()).collect::<Vec<_>>())
        })
        .await?
    };
    // Recommended offers only what is unread.
    crate::news::rank::forget_window(&svc);
    if mail_backed.is_empty() {
        return Ok(());
    }
    newsletters::mark_read(&svc, &vault, &mail_backed, read).await
}

/// Everything unread in these sources (every source when none are named),
/// published at or before `up_to`, read. Answers with the ids it changed, so
/// that an undo marks exactly those unread again and not whatever else was
/// already read.
///
/// A newsletter is left alone: see `NewsStore::mark_all_read`. An interface
/// that wants those marked too names their ids to `mark_stories_read`.
async fn mark_all_stories_read(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: MarkAll,
) -> CommandResult<Vec<StoryId>> {
    let marked =
        svc.on_vault(move |vault| vault.mark_all_stories_read(&args.sources, args.up_to)).await?;
    crate::news::rank::forget_window(&svc);
    Ok(marked)
}

async fn save_stories(svc: Arc<Service>, _ctx: Ctx, args: SaveStories) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.set_stories_saved(&args.ids, args.saved)).await
}

/// Ask now, rather than when the schedule says: one source, or every source
/// that is not paused. A newsletter the Newsletters source found is asked by
/// asking that source, which is where its issues come from.
///
/// A source that cannot be reached is a report with an error in it, not a
/// failed command -- the same rule `sync_due_calendars` keeps, for the same
/// reason: one feed being down must not hide what the others brought.
async fn refresh_news(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: RefreshNews,
) -> CommandResult<Vec<RefreshReport>> {
    let vault = svc.require_unlocked()?;
    let reports = match args.source {
        Some(id) => {
            // Said plainly here, rather than as a report about nothing.
            {
                let vault = vault.clone();
                blocking(move || Ok(vault.news_source(id)?)).await?;
            }
            vec![refresh::refresh_source(&svc, &vault, id).await]
        }
        None => refresh::refresh_all(&svc, &vault).await,
    };
    // A refresh somebody asked for that works ends the outage as much as a
    // background one does, so the next failure is news again.
    for report in reports.iter().filter(|r| r.error.is_none()) {
        svc.news_recovered(report.source_id);
    }
    Ok(reports)
}

/// Fetch the linked page, extract the article and keep it as the story's
/// body, so it can be read offline from then on.
async fn fetch_full_article(svc: Arc<Service>, _ctx: Ctx, args: StoryRef) -> CommandResult<Story> {
    let vault = svc.require_unlocked()?;
    extract::full_article(&vault, args.id).await?;
    blocking(move || Ok(vault.story(args.id)?)).await
}

// ---- the library --------------------------------------------------------

/// Put a story on the Library's Articles shelf, to read later or keep.
///
/// The long-term home for what is worth keeping: news expires and a shelf
/// does not. Nothing is looked up -- the story already has its title, author
/// and address -- so this is one write and needs no network.
///
/// The same story added twice is one item. The key that adds it is under
/// `L`, and a keyboard shortcut that makes a duplicate when pressed twice is
/// a shortcut nobody trusts.
async fn add_story_to_library(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: StoryRef,
) -> CommandResult<ItemId> {
    let vault = svc.require()?;
    blocking(move || {
        let story = vault.story(args.id)?;
        let shelf = articles_shelf(&vault)?;
        let url = story.url.trim();
        if !url.is_empty() {
            let on_shelf = vault.items(&ItemQuery::on_shelf(shelf))?;
            let same =
                on_shelf.iter().find(|i| i.facts.get("url").is_some_and(|u| same_address(u, url)));
            if let Some(item) = same {
                return Ok(item.id);
            }
        }
        // A source deleted while the story was open takes the story with it,
        // so this only ever comes back empty on a read that raced one.
        let publication = vault.news_source(story.source_id).map(|s| s.title).unwrap_or_default();
        let item = library_item(shelf, &story, &publication);
        vault.save_item(&item)?;
        Ok(item.id)
    })
    .await
}

/// The shelf articles go on: the one whose short name is `article`, seeding
/// the built-in shelves first if the library has none at all.
///
/// Not made if it is missing from a library that has other shelves.
/// Somebody who deleted Articles said something, and an app that puts it
/// back is arguing -- the reason `Vault::seed_library` does nothing to a
/// library with any shelf in it.
fn articles_shelf(vault: &Vault) -> CommandResult<KindId> {
    let mut kinds = vault.kinds()?;
    if kinds.is_empty() {
        vault.seed_library()?;
        kinds = vault.kinds()?;
    }
    kinds.into_iter().find(|k| k.slug == "article").map(|k| k.id).ok_or_else(|| {
        CommandError::new(
            codes::NOT_FOUND,
            "there is no Articles shelf in the library; make one called Articles first",
        )
    })
}

/// A wishlist item for `story` on `shelf`: the title, the author as the
/// creator, the story's own summary, and the Articles shelf's three facts
/// (`url`, `author` and `publication`, the last being the source it came
/// from). A fact with nothing to say is left out rather than written empty.
fn library_item(shelf: KindId, story: &Story, publication: &str) -> Item {
    let title = story.title.trim();
    let title = if title.is_empty() { story.url.trim() } else { title };
    // `Item::new` starts on the wishlist, which is where something you have
    // not read yet belongs.
    let mut item = Item::new(shelf, title);
    item.creator = story.author.trim().to_string();
    item.summary = story.summary.clone();
    let (url, author, publication) = (story.url.trim(), story.author.trim(), publication.trim());
    for (key, value) in [("url", url), ("author", author), ("publication", publication)] {
        if !value.is_empty() {
            item.facts.insert(key.to_string(), value.to_string());
        }
    }
    item
}

// ---- OPML ---------------------------------------------------------------

/// Follow every feed in somebody else's reader's list.
///
/// Fetches nothing. A source with no fetch state is due at once, so the
/// refresher's next wake-up asks every new one in its own time -- at most
/// four requests at a time and one per host, which is exactly what a hundred
/// sources arriving in one call need and what an import that fetched them
/// here would have to rebuild.
///
/// An outline that is not a feed we can follow, or that is already followed,
/// is counted and skipped rather than failing the import: the person's
/// other ninety-nine feeds are not the reason to refuse the hundredth. So is
/// one whose address is on a private network -- `http://192.168.0.1/`,
/// `http://router/`, a `.local` or `.ts.net` name -- because a list from
/// somebody else's reader is a stranger's text, and the refresher would
/// fetch such an address from inside the person's network every few minutes
/// on the stranger's say-so (see [`crate::news::fetch`]). A self-hosted feed
/// of their own is added by typing its address, which is the one gesture that
/// is theirs.
async fn import_opml(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: ImportOpml,
) -> CommandResult<OpmlImportReport> {
    let vault = svc.require()?;
    let outlines = crate::news::opml::read(&args.xml)
        .map_err(|message| CommandError::new(codes::INVALID, message))?;
    blocking(move || {
        let mut followed: Vec<String> = vault
            .news_sources()?
            .into_iter()
            .filter_map(|s| match s.kind {
                SourceKind::Web { url } => Some(url),
                _ => None,
            })
            .collect();
        let (mut added, mut skipped) = (0u32, 0u32);
        for outline in outlines {
            let url = outline.xml_url.trim().to_string();
            if !is_http(&url)
                || names_own_network(&url)
                || followed.iter().any(|f| same_address(f, &url))
            {
                skipped += 1;
                continue;
            }
            let title = match outline.title.trim() {
                "" => host_title(&url),
                title => title.to_string(),
            };
            let mut source = NewsSource::new(SourceKind::Web { url: url.clone() }, title);
            source.folder = outline.folder.trim().to_string();
            let site = outline.html_url.trim();
            if is_http(site) {
                source.site_url = site.to_string();
            }
            vault.save_news_source(&source)?;
            followed.push(url);
            added += 1;
        }
        Ok(OpmlImportReport { added, skipped })
    })
    .await
}

/// Every web source as OPML, for any other reader. Hacker News lists and
/// newsletters are not feeds anybody else's reader can fetch, so they are
/// not in it.
async fn export_opml(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<String> {
    svc.on_vault(|vault| {
        let outlines: Vec<Outline> = vault
            .news_sources()?
            .into_iter()
            .filter_map(|s| match s.kind {
                SourceKind::Web { url } => Some(Outline {
                    title: s.title,
                    xml_url: url,
                    html_url: s.site_url,
                    folder: s.folder,
                }),
                _ => None,
            })
            .collect();
        Ok(jatayu_core::news::opml::write("Jatayu", &outlines))
    })
    .await
}

// ---- small things -------------------------------------------------------

/// Are these two the same address? By their canonical form when both have one
/// -- so `Example.com/feed/` and `example.com/feed`, or a link with and
/// without `utm_source`, are one feed -- and by the trimmed text when either
/// does not.
pub(crate) fn same_address(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    match (canonical::canonicalise(a), canonical::canonicalise(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

/// The addresses of the feeds somebody follows, a podcast's among them: what
/// a directory's hits are compared with to say which are already followed.
pub(crate) fn followed_feeds(sources: &[NewsSource]) -> Vec<String> {
    sources
        .iter()
        .filter_map(|s| match &s.kind {
            SourceKind::Web { url } => Some(url.clone()),
            _ => None,
        })
        .collect()
}

/// Whether `url` is one of the `followed` feeds, by [`same_address`].
pub(crate) fn is_followed(followed: &[String], url: &str) -> bool {
    followed.iter().any(|f| same_address(f, url))
}

/// Trimmed, and `None` when nothing is left.
fn clean(text: Option<String>) -> Option<String> {
    text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

/// Does this address name a machine on the person's own network -- by what
/// the address says, as [`jatayu_core::chat::is_private_network_host`]
/// reads it, so a decimal spelling of `127.0.0.1` is caught too?
fn names_own_network(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(jatayu_core::chat::is_private_network_host))
        .unwrap_or(false)
}

fn is_http(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

/// What somebody typed, as an address a request can be made to: a bare
/// `example.com` gets `https://`, and the `feed://` that some sites hand out
/// is `https://` too. Anything that is then not http(s) is refused here, in
/// words, rather than as a fetch failure.
fn web_address(input: &str) -> CommandResult<String> {
    let typed = input.trim();
    if typed.is_empty() {
        return Err(CommandError::new(codes::INVALID, "paste the address of a site or its feed"));
    }
    let address = if typed.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("feed://")) {
        format!("https://{}", &typed[7..])
    } else if typed.contains("://") {
        typed.to_string()
    } else {
        format!("https://{typed}")
    };
    if !is_http(&address) {
        return Err(CommandError::new(
            codes::INVALID,
            "a feed's address must start with https:// or http://",
        ));
    }
    Ok(address)
}

/// The site an address belongs to: its scheme, host and port.
fn origin_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .filter(|u| u.has_host())
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_default()
}

/// A host to call a source when nothing else named it: `example.com` for
/// `https://www.example.com/feed`.
fn host_title(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.strip_prefix("www.").unwrap_or(h).to_string()))
        .unwrap_or_else(|| url.to_string())
}

/// What a Hacker News source is called. The list names it, and a points
/// floor is part of what it is: "Hacker News, 100+ points" is a different
/// reading list from the front page.
fn hn_title(list: HnList, min_points: u32) -> String {
    let name = match list {
        HnList::Front | HnList::Top => "Hacker News",
        HnList::Show => "Show HN",
        HnList::Ask => "Ask HN",
    };
    if min_points > 0 { format!("{name}, {min_points}+ points") } else { name.to_string() }
}

/// Refuse to follow what is already followed: the same address, the same
/// Hacker News list with the same points floor, or the same sender.
fn refuse_duplicate(existing: &[NewsSource], kind: &SourceKind) -> CommandResult<()> {
    let followed = existing.iter().any(|s| match (&s.kind, kind) {
        (SourceKind::Web { url: a }, SourceKind::Web { url: b }) => same_address(a, b),
        (
            SourceKind::HackerNews { list: l1, min_points: p1 },
            SourceKind::HackerNews { list: l2, min_points: p2 },
        ) => l1 == l2 && p1 == p2,
        (
            SourceKind::Mail { sender: a, name: a_name },
            SourceKind::Mail { sender: b, name: b_name },
        ) => {
            a.eq_ignore_ascii_case(b)
                && a_name.trim().to_lowercase() == b_name.trim().to_lowercase()
        }
        (SourceKind::Newsletters, SourceKind::Newsletters) => true,
        _ => false,
    });
    if followed {
        let what = match kind {
            SourceKind::Web { .. } => "that feed",
            SourceKind::HackerNews { .. } => "that Hacker News list",
            SourceKind::Mail { .. } => "that sender's newsletter",
            SourceKind::Newsletters => "your newsletters",
        };
        return Err(CommandError::new(codes::INVALID, format!("you already follow {what}")));
    }
    Ok(())
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_news_sources", scope: News, effect: Read,
        args: Nothing, returns: "SourceSummary[]", signature: &[],
        run: list_news_sources,
    },
    command! {
        name: "discover_feeds", scope: News, effect: Read,
        args: Discover, returns: "FoundFeed[]",
        signature: &[("url", "string", true)],
        run: discover_feeds,
    },
    command! {
        name: "add_news_source", scope: News, effect: Write,
        change: NewsSource / Created,
        args: AddSource, returns: "NewsSource",
        signature: &[("source", "NewSource", true)],
        run: add_news_source,
    },
    command! {
        name: "save_news_source", scope: News, effect: Write,
        change: NewsSource / Updated,
        id: |a: &SaveSource| Some(a.source.id.to_string()),
        args: SaveSource, returns: "void",
        signature: &[("source", "NewsSource", true)],
        run: save_news_source,
    },
    command! {
        name: "delete_news_source", scope: News, effect: Destructive,
        change: NewsSource / Deleted,
        id: |a: &SourceRef| Some(a.id.to_string()),
        args: SourceRef, returns: "void",
        signature: &[("id", "NewsSourceId", true)],
        run: delete_news_source,
    },
    command! {
        name: "unsubscribe_news_sources", scope: News, effect: Destructive,
        change: NewsSource / Deleted,
        ids: |a: &SourceIds| a.ids.iter().map(|id| id.to_string()).collect(),
        args: SourceIds, returns: "UnsubscribeOutcome[]",
        signature: &[("ids", "NewsSourceId[]", true)],
        run: unsubscribe_news_sources,
    },
    command! {
        name: "mark_not_newsletters", scope: News, effect: Destructive,
        change: NewsSource / Deleted,
        ids: |a: &NotNewsletters| a.ids.iter().map(|id| id.to_string()).collect(),
        args: NotNewsletters, returns: "NotNewslettersReport",
        signature: &[("ids", "NewsSourceId[]", true), ("category", "MailCategory | null", false)],
        run: mark_not_newsletters,
    },
    command! {
        name: "list_stories", scope: News, effect: Read,
        args: Stories, returns: "StoryPage",
        signature: &[("query", "StoryQuery", true)],
        run: list_stories,
    },
    command! {
        name: "get_story", scope: News, effect: Read,
        args: StoryRef, returns: "Story",
        signature: &[("id", "StoryId", true)],
        run: get_story,
    },
    command! {
        name: "mark_stories_read", scope: News, effect: Write,
        change: Story / Updated,
        ids: |a: &MarkStories| a.ids.iter().map(|i| i.to_string()).collect(),
        args: MarkStories, returns: "void",
        signature: &[("ids", "StoryId[]", true), ("read", "boolean", true)],
        run: mark_stories_read,
    },
    command! {
        name: "mark_all_stories_read", scope: News, effect: Write,
        change: Story / Updated,
        args: MarkAll, returns: "StoryId[]",
        signature: &[("sources", "NewsSourceId[]", true), ("upTo", "string", true)],
        run: mark_all_stories_read,
    },
    command! {
        name: "save_stories", scope: News, effect: Write,
        change: Story / Updated,
        ids: |a: &SaveStories| a.ids.iter().map(|i| i.to_string()).collect(),
        args: SaveStories, returns: "void",
        signature: &[("ids", "StoryId[]", true), ("saved", "boolean", true)],
        run: save_stories,
    },
    command! {
        name: "refresh_news", scope: News, effect: Write,
        change: Story / Created,
        args: RefreshNews, returns: "RefreshReport[]",
        signature: &[("source", "NewsSourceId | null", false)],
        run: refresh_news,
    },
    command! {
        name: "fetch_full_article", scope: News, effect: Write,
        change: Story / Updated,
        id: |a: &StoryRef| Some(a.id.to_string()),
        args: StoryRef, returns: "Story",
        signature: &[("id", "StoryId", true)],
        run: fetch_full_article,
    },
    command! {
        name: "add_story_to_library", scope: News, effect: Write,
        change: Item / Created,
        args: StoryRef, returns: "ItemId",
        signature: &[("id", "StoryId", true)],
        run: add_story_to_library,
    },
    command! {
        name: "import_opml", scope: News, effect: Write,
        change: NewsSource / Created,
        args: ImportOpml, returns: "OpmlImportReport",
        signature: &[("xml", "string", true)],
        run: import_opml,
    },
    command! {
        name: "export_opml", scope: News, effect: Read,
        args: Nothing, returns: "string", signature: &[],
        run: export_opml,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_host_gets_a_scheme_and_a_feed_scheme_is_https() {
        assert_eq!(web_address(" example.com/feed ").unwrap(), "https://example.com/feed");
        assert_eq!(web_address("feed://example.com/rss").unwrap(), "https://example.com/rss");
        assert_eq!(web_address("FEED://example.com/rss").unwrap(), "https://example.com/rss");
        assert_eq!(web_address("http://example.com/x").unwrap(), "http://example.com/x");
        assert_eq!(web_address("HTTPS://Example.com").unwrap(), "HTTPS://Example.com");
    }

    #[test]
    fn what_is_not_http_is_refused_in_words() {
        for typed in ["", "   ", "file:///etc/passwd", "ftp://example.com", "javascript://x"] {
            let e = web_address(typed).unwrap_err();
            assert_eq!(e.code, codes::INVALID, "{typed:?}");
        }
    }

    #[test]
    fn a_site_is_the_origin_of_its_address() {
        assert_eq!(origin_of("https://example.com/blog/feed.xml"), "https://example.com");
        assert_eq!(origin_of("http://localhost:8080/feed"), "http://localhost:8080");
        assert_eq!(origin_of("not an address"), "");
    }

    #[test]
    fn an_unnamed_source_is_called_by_its_host() {
        assert_eq!(host_title("https://www.example.com/feed"), "example.com");
        assert_eq!(host_title("https://blog.example.com/feed"), "blog.example.com");
        assert_eq!(host_title("nonsense"), "nonsense");
    }

    #[test]
    fn a_hacker_news_source_is_named_for_its_list_and_its_floor() {
        assert_eq!(hn_title(HnList::Front, 0), "Hacker News");
        assert_eq!(hn_title(HnList::Show, 0), "Show HN");
        assert_eq!(hn_title(HnList::Ask, 0), "Ask HN");
        assert_eq!(hn_title(HnList::Top, 100), "Hacker News, 100+ points");
        assert_eq!(hn_title(HnList::Show, 25), "Show HN, 25+ points");
    }

    #[test]
    fn blank_names_and_folders_are_not_names() {
        assert_eq!(clean(Some("  ".into())), None);
        assert_eq!(clean(None), None);
        assert_eq!(clean(Some(" Blog ".into())), Some("Blog".into()));
    }

    fn web(url: &str) -> NewsSource {
        NewsSource::new(SourceKind::Web { url: url.into() }, "x")
    }

    #[test]
    fn an_address_on_a_private_network_is_told_from_one_on_the_internet() {
        for own in [
            "http://192.168.1.2/feed",
            "http://127.0.0.1:8080/",
            "http://2130706433/",
            "http://169.254.169.254/latest/meta-data/",
            "http://router/rss",
            "http://box.local/rss",
            "https://bridge.tail1234.ts.net/rss",
            "http://[fd7a:115c:a1e0::1]/rss",
        ] {
            assert!(names_own_network(own), "{own}");
        }
        for stranger in ["https://example.com/feed", "http://8.8.8.8/", "not an address"] {
            assert!(!names_own_network(stranger), "{stranger}");
        }
    }

    #[test]
    fn the_same_thing_followed_twice_is_refused() {
        let existing = vec![
            web("https://example.com/feed"),
            NewsSource::new(SourceKind::HackerNews { list: HnList::Show, min_points: 50 }, "Show"),
            NewsSource::new(
                SourceKind::Mail { sender: "letters@example.com".into(), name: String::new() },
                "Letters",
            ),
        ];
        let again = [
            SourceKind::Web { url: "https://example.com/feed".into() },
            SourceKind::HackerNews { list: HnList::Show, min_points: 50 },
            SourceKind::Mail { sender: "Letters@Example.com".into(), name: String::new() },
        ];
        for kind in again {
            let e = refuse_duplicate(&existing, &kind).unwrap_err();
            assert_eq!(e.code, codes::INVALID, "{kind:?}");
            assert!(e.message.contains("already follow"), "{}", e.message);
        }
    }

    #[test]
    fn a_different_list_floor_or_sender_is_a_different_source() {
        let existing = vec![
            web("https://example.com/feed"),
            NewsSource::new(SourceKind::HackerNews { list: HnList::Show, min_points: 50 }, "Show"),
            NewsSource::new(
                SourceKind::Mail { sender: "letters@example.com".into(), name: String::new() },
                "Letters",
            ),
        ];
        let different = [
            SourceKind::Web { url: "https://example.com/other".into() },
            SourceKind::HackerNews { list: HnList::Show, min_points: 100 },
            SourceKind::HackerNews { list: HnList::Ask, min_points: 50 },
            SourceKind::Mail { sender: "news@example.com".into(), name: String::new() },
        ];
        for kind in different {
            assert!(refuse_duplicate(&existing, &kind).is_ok(), "{kind:?}");
        }
    }

    #[test]
    fn a_podcast_from_the_directory_is_a_web_feed_that_is_a_show_from_the_start() {
        let source = podcast_source(
            &[],
            "feeds.example.com/show.xml",
            Some("  The Show ".into()),
            Some(" Listening ".into()),
            Some("173001861".into()),
            Some(" Dan Carlin ".into()),
        )
        .unwrap();
        assert_eq!(
            source.kind,
            SourceKind::Web { url: "https://feeds.example.com/show.xml".into() }
        );
        assert_eq!(source.title, "The Show");
        assert_eq!(source.folder, "Listening");
        assert_eq!(source.site_url, "");
        let show = source.podcast.expect("a show");
        assert_eq!(show.author, "Dan Carlin");
        assert_eq!(show.directory_id.as_deref(), Some("173001861"));
        // Everything else is the Feed's say.
        assert_eq!((show.download, show.keep, show.queue_new), (None, None, None));

        let bare =
            podcast_source(&[], "https://feeds.example.com/show.xml", None, None, None, None)
                .unwrap();
        assert_eq!(bare.title, "feeds.example.com");
        assert_eq!(bare.folder, "");
        assert_eq!(bare.podcast.unwrap().directory_id, None);
    }

    #[test]
    fn a_show_is_refused_as_a_web_feed_is_when_it_is_already_followed() {
        let mut followed = web("https://feeds.example.com/show.xml");
        followed.podcast = Some(PodcastSettings::default());
        // Followed as a show, offered as a show, and followed as a plain feed
        // then offered as a show: all the same feed.
        for existing in [followed, web("https://Feeds.example.com/show.xml/")] {
            let e = podcast_source(
                &[existing],
                "https://feeds.example.com/show.xml",
                None,
                None,
                None,
                None,
            )
            .unwrap_err();
            assert_eq!(e.code, codes::INVALID);
            assert!(e.message.contains("already follow"), "{}", e.message);
        }
        let other = web("https://feeds.example.com/other.xml");
        assert!(
            podcast_source(&[other], "https://feeds.example.com/show.xml", None, None, None, None)
                .is_ok()
        );
    }

    #[test]
    fn a_show_whose_feed_is_on_the_persons_own_network_is_not_taken_from_a_directory() {
        for own in ["http://192.168.1.2/feed", "http://nas.local/rss", "http://127.0.0.1:8080/x"] {
            let e = podcast_source(&[], own, None, None, None, None).unwrap_err();
            assert_eq!(e.code, codes::INVALID, "{own}");
        }
        let e = podcast_source(&[], "ftp://example.com/x", None, None, None, None).unwrap_err();
        assert_eq!(e.code, codes::INVALID);
    }

    #[test]
    fn a_podcast_is_asked_for_the_way_the_interface_writes_it() {
        let wire = serde_json::json!({
            "type": "podcast",
            "url": "https://feeds.example.com/show.xml",
            "title": "The Show",
            "directoryId": "42",
            "author": "A. Host",
        });
        match serde_json::from_value::<NewSource>(wire).unwrap() {
            NewSource::Podcast { url, title, folder, directory_id, author } => {
                assert_eq!(url, "https://feeds.example.com/show.xml");
                assert_eq!(title.as_deref(), Some("The Show"));
                assert_eq!(
                    (folder, directory_id.as_deref(), author.as_deref()),
                    (None, Some("42"), Some("A. Host"))
                );
            }
            other => panic!("not a podcast: {other:?}"),
        }
        let minimal = serde_json::json!({"type": "podcast", "url": "https://x.example/f"});
        assert!(matches!(
            serde_json::from_value::<NewSource>(minimal).unwrap(),
            NewSource::Podcast { title: None, folder: None, directory_id: None, author: None, .. }
        ));
    }

    fn story() -> Story {
        let mut story = Story::new(NewsSourceId::new(), "key", "  A post  ", Timestamp::now());
        story.url = "https://blog.example/post".into();
        story.author = " A. Writer ".into();
        story.summary = "What it says.".into();
        story
    }

    #[test]
    fn a_library_item_carries_the_title_author_summary_and_the_three_facts() {
        let shelf = KindId::new();
        let item = library_item(shelf, &story(), "The Blog");
        assert_eq!(item.kind_id, shelf);
        assert_eq!(item.title, "A post");
        assert_eq!(item.creator, "A. Writer");
        assert_eq!(item.summary, "What it says.");
        assert_eq!(item.status, jatayu_core::library::ItemStatus::Wishlist);
        assert_eq!(item.facts.get("url").map(String::as_str), Some("https://blog.example/post"));
        assert_eq!(item.facts.get("author").map(String::as_str), Some("A. Writer"));
        assert_eq!(item.facts.get("publication").map(String::as_str), Some("The Blog"));
    }

    #[test]
    fn a_fact_with_nothing_to_say_is_left_out_and_a_blank_title_takes_the_address() {
        let mut bare = story();
        bare.title = "   ".into();
        bare.author.clear();
        let item = library_item(KindId::new(), &bare, "");
        assert_eq!(item.title, "https://blog.example/post");
        assert!(item.creator.is_empty());
        assert_eq!(item.facts.keys().map(String::as_str).collect::<Vec<_>>(), ["url"]);
    }
}
