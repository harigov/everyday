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
//!   everyday-core     the records, the arithmetic and the store
//!
//! Nothing here fetches in order to *decide* anything the person did not ask
//! for. Following an address looks it up once, to give it a name; everything
//! else that touches the network is a refresh, a full article, or a picture,
//! and each of those is a button.

use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Change, Kind, Op};
use crate::news::{FoundFeed, RefreshReport, canonical, extract, fetch, newsletters, refresh};
use crate::service::{Service, blocking};
use everyday_core::library::Item;
use everyday_core::mail::Category;
use everyday_core::news::opml::Outline;
use everyday_core::news::{HnList, NewsSource, SourceKind, SourceSummary, Story};
use everyday_core::store::library::ItemQuery;
use everyday_core::store::news::{StoryPage, StoryQuery};
use everyday_core::{ItemId, KindId, NewsSourceId, StoryId, Vault};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
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

async fn list_news_sources(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<SourceSummary>> {
    svc.on_vault(|vault| vault.news_source_summaries()).await
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
            let kind = SourceKind::Mail { sender: sender.clone() };
            refuse_duplicate(&existing, &kind)?;
            let title = clean(title).unwrap_or(sender);
            let mut source = NewsSource::new(kind, title);
            source.folder = folder.map(|f| f.trim().to_string()).unwrap_or_default();
            source
        }
    };

    let id = source.id;
    {
        let (vault, source) = (vault.clone(), source.clone());
        blocking(move || Ok(vault.save_news_source(&source)?)).await?;
    }
    if let SourceKind::Mail { sender } = &source.kind {
        let (vault, sender) = (vault.clone(), sender.clone());
        blocking(move || {
            file_as_newsletter(&vault, &sender);
            Ok(())
        })
        .await?;
    }

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

async fn save_news_source(svc: Arc<Service>, _ctx: Ctx, args: SaveSource) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_news_source(&args.source)).await
}

/// Unfollow: the source, every story it brought -- saved ones too, because
/// unfollowing is the explicit act -- their bodies and its fetch state.
async fn delete_news_source(svc: Arc<Service>, _ctx: Ctx, args: SourceRef) -> CommandResult<()> {
    let id = args.id;
    svc.on_vault(move |vault| vault.delete_news_source(id)).await?;
    // Forgotten, so that following it again and watching it fail is news again.
    svc.news_recovered(id);
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
    svc.on_vault(move |vault| vault.mark_all_stories_read(&args.sources, args.up_to)).await
}

async fn save_stories(svc: Arc<Service>, _ctx: Ctx, args: SaveStories) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.set_stories_saved(&args.ids, args.saved)).await
}

/// Ask now, rather than when the schedule says: one source, or every source
/// that is not paused.
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
/// other ninety-nine feeds are not the reason to refuse the hundredth.
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
            if !is_http(&url) || followed.iter().any(|f| same_address(f, &url)) {
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
        Ok(everyday_core::news::opml::write("Every Day", &outlines))
    })
    .await
}

// ---- small things -------------------------------------------------------

/// Are these two the same address? By their canonical form when both have one
/// -- so `Example.com/feed/` and `example.com/feed`, or a link with and
/// without `utm_source`, are one feed -- and by the trimmed text when either
/// does not.
fn same_address(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    match (canonical::canonicalise(a), canonical::canonicalise(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

/// Trimmed, and `None` when nothing is left.
fn clean(text: Option<String>) -> Option<String> {
    text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
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
        (SourceKind::Mail { sender: a }, SourceKind::Mail { sender: b }) => {
            a.eq_ignore_ascii_case(b)
        }
        _ => false,
    });
    if followed {
        let what = match kind {
            SourceKind::Web { .. } => "that feed",
            SourceKind::HackerNews { .. } => "that Hacker News list",
            SourceKind::Mail { .. } => "that sender's newsletter",
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
    fn the_same_thing_followed_twice_is_refused() {
        let existing = vec![
            web("https://example.com/feed"),
            NewsSource::new(SourceKind::HackerNews { list: HnList::Show, min_points: 50 }, "Show"),
            NewsSource::new(SourceKind::Mail { sender: "letters@example.com".into() }, "Letters"),
        ];
        let again = [
            SourceKind::Web { url: "https://example.com/feed".into() },
            SourceKind::HackerNews { list: HnList::Show, min_points: 50 },
            SourceKind::Mail { sender: "Letters@Example.com".into() },
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
            NewsSource::new(SourceKind::Mail { sender: "letters@example.com".into() }, "Letters"),
        ];
        let different = [
            SourceKind::Web { url: "https://example.com/other".into() },
            SourceKind::HackerNews { list: HnList::Show, min_points: 100 },
            SourceKind::HackerNews { list: HnList::Ask, min_points: 50 },
            SourceKind::Mail { sender: "news@example.com".into() },
        ];
        for kind in different {
            assert!(refuse_duplicate(&existing, &kind).is_ok(), "{kind:?}");
        }
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
        assert_eq!(item.status, everyday_core::library::ItemStatus::Wishlist);
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
