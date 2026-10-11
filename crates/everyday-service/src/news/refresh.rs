//! One source's turn, and the loop that gives every source its turn.
//!
//! # A turn
//!
//! [`refresh_source`] fetches one source -- a feed with a conditional
//! request, Algolia's answer for a Hacker News list, or, for a newsletter,
//! nothing at all but a call to [`super::newsletters`] -- turns what came
//! back into stories, stores them, and records what it learned in the
//! source's [`FetchState`]: the validators for next time, when to ask again,
//! and why it failed if it did.
//!
//! The refresher never writes a [`NewsSource`] itself, with one exception
//! that has its own doc (`pause`, below). A rename made while a fetch is in flight
//! is therefore never overwritten by it.
//!
//! A story is built only for what is new. What was stored before is left
//! exactly as it is -- the store sees to `read` and `saved` -- except that a
//! Hacker News story's points and comments are brought up to date, and that
//! an article the same address brought from two sources becomes one row
//! that says so ([`Story::also`]).
//!
//! # The loop
//!
//! [`run`] is one supervised task for every source. Each turn it refreshes
//! what is due, at most four at once and one per host, expires what has had
//! its time, and sleeps until the soonest source is due -- at most a minute,
//! so a source added from another window is noticed. It stops the moment the
//! supervisor says so, and treats a locked vault as a reason to sleep rather
//! than to fail.

use super::fetch::{self, FetchError, Fetched};
use super::{Incoming, RefreshReport, extract, hn, newsletters, parse, sanitize};
use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Change, Kind, Notification, Op};
use crate::http;
use crate::service::{Service, blocking};
use crate::supervisor::{Outcome, TaskResult};
use everyday_core::Vault;
use everyday_core::id::{NewsSourceId, StoryId};
use everyday_core::news::schedule::{self, Attempt, Hints};
use everyday_core::news::{
    Also, BodyOrigin, FetchState, HnList, NewsSource, SourceKind, Story, StoryBody,
};
use everyday_core::timestamped::Timestamped;
use futures::StreamExt;
use jiff::Timestamp;
use rand::Rng;
use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// How many sources are fetched at once.
const AT_ONCE: usize = 4;

/// The longest the loop sleeps. A minute, so a source added from another
/// window, or one whose schedule just changed, is noticed promptly.
const LONGEST_SLEEP: Duration = Duration::from_secs(60);

/// The shortest, so a source due a moment from now is not asked for in a
/// hurry.
const SHORTEST_SLEEP: Duration = Duration::from_secs(1);

/// How often the loop expires old stories. Once an hour is far oftener than
/// anything becomes old enough to matter, and costs one pass over a table.
const EXPIRE_EVERY_SECS: i64 = 60 * 60;

/// How many articles a source with "always fetch the full article" is asked
/// for in one turn. The first read of a big feed brings a hundred new
/// stories, and a hundred page loads from one refresh is not a polite thing
/// to do to a site; the rest are left for "Fetch the full article".
const ARTICLES_PER_TURN: usize = 10;

/// Where the loop is when Hacker News's stories come from: one host, however
/// many lists are followed.
const HN_HOST: &str = "hn.algolia.com";

/// The words a title is cut to when an item had none.
const UNTITLED_CHARS: usize = 80;

/// What one source's turn did, and what the caller needs to say about it
/// beyond the report.
struct Turn {
    report: RefreshReport,
    /// The source began failing, stopped, failed for a different reason, or
    /// was paused: something the sidebar draws beside it changed.
    health_changed: bool,
}

impl Turn {
    fn unchanged(source_id: NewsSourceId, error: Option<String>) -> Self {
        let report = RefreshReport { source_id, inserted: 0, not_modified: false, error };
        Self { report, health_changed: false }
    }
}

/// What a fetch brought, as far as scheduling and the report care.
struct Arrived {
    inserted: u32,
    /// The server said `304`.
    not_modified: bool,
    /// New content arrived: something was stored, or this was the first read.
    fresh: bool,
    hints: Hints,
    published: Vec<Timestamp>,
}

impl Arrived {
    fn unchanged(hints: Hints) -> Self {
        Self { inserted: 0, not_modified: true, fresh: false, hints, published: Vec::new() }
    }
}

/// What storing a refresh's items did.
struct Stored {
    inserted: Vec<StoryId>,
    /// New stories with an address and nothing better than the feed's own
    /// text, for "always fetch the full article": a source switched to it
    /// has said the feed's text is not enough, description or not.
    wants_article: Vec<StoryId>,
    published: Vec<Timestamp>,
    ttl_minutes: Option<u32>,
}

/// What a fetch handed to the blocking pool to be read and stored.
enum Payload {
    Feed { bytes: Vec<u8>, base: String },
    HackerNews { json: Vec<u8>, list: HnList, min_points: u32 },
}

// ── The loop ─────────────────────────────────────────────────────────────

/// The supervised loop: sleep until the soonest source is due (and at most a
/// minute, so a source added from another window is noticed), refresh what
/// is due, expire what has had its time, raise one `Story` change per pass
/// that stored anything. Returns when `stop` turns `true`.
pub async fn run(
    svc: Arc<Service>,
    vault: Arc<Vault>,
    mut stop: watch::Receiver<bool>,
) -> TaskResult {
    let mut expired_at: Option<Timestamp> = None;
    loop {
        if *stop.borrow() {
            return Ok(Outcome::Done);
        }
        let sleep = match pass(&svc, &vault, &stop, &mut expired_at).await {
            Ok(sleep) => sleep,
            // Locked between one turn and the next: nothing to read and
            // nothing to write. The supervisor stops this task on the lock
            // in any case; until it does, wait.
            Err(e) if e.code == codes::LOCKED => LONGEST_SLEEP,
            Err(e) => return Err(e.into()),
        };
        tokio::select! {
            _ = tokio::time::sleep(sleep) => {}
            changed = stop.changed() => {
                // The sender is gone: nobody is left to ask this to stop, or
                // to want what it does.
                if changed.is_err() {
                    return Ok(Outcome::Done);
                }
            }
        }
    }
}

/// One turn of [`run`]: everything that is due, then the hourly expiry.
/// Returns how long to sleep.
async fn pass(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    stop: &watch::Receiver<bool>,
    expired_at: &mut Option<Timestamp>,
) -> CommandResult<Duration> {
    // A second copy of the application holds the write claim. Reading is
    // fine and storing is not, and fetching to throw the answer away is not
    // a useful thing to do on a timer.
    if !vault.is_writable() {
        return Ok(LONGEST_SLEEP);
    }
    let now = svc.now();

    let due = blocking({
        let vault = vault.clone();
        move || {
            let due = vault.due_news_sources(now)?;
            if due.is_empty() {
                return Ok(Vec::new());
            }
            Ok(sources_to_refresh(&vault, Some(due.as_slice()))?)
        }
    })
    .await?;
    if !due.is_empty() {
        let turns = refresh_many(svc, vault, due, Some(stop), true).await;
        announce(svc, &turns);
    }

    if expired_at.is_none_or(|at| now.as_second() - at.as_second() >= EXPIRE_EVERY_SECS) {
        *expired_at = Some(now);
        expire(svc, vault, now).await;
    }

    let next = blocking({
        let vault = vault.clone();
        move || Ok(vault.next_news_due()?)
    })
    .await?;
    Ok(sleep_for(next, svc.now()))
}

/// How long to sleep for a source due at `next`.
///
/// A source already due, after a pass that has just done everything it could
/// for the due ones, is a paused source or one whose state could not be
/// saved. Neither is helped by asking again in a second.
fn sleep_for(next: Option<Timestamp>, now: Timestamp) -> Duration {
    let Some(next) = next else { return LONGEST_SLEEP };
    let millis = next.as_millisecond() - now.as_millisecond();
    if millis <= 0 {
        return LONGEST_SLEEP;
    }
    Duration::from_millis(millis as u64).clamp(SHORTEST_SLEEP, LONGEST_SLEEP)
}

/// Turn stories past their retention into tombstones, and say so.
async fn expire(svc: &Arc<Service>, vault: &Arc<Vault>, now: Timestamp) {
    let expired = blocking({
        let vault = vault.clone();
        move || Ok(vault.expire_stories(now)?)
    })
    .await;
    match expired {
        Ok(report) if report.expired > 0 => {
            svc.events().changed(Change::new(Kind::Story, Op::Deleted));
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "could not expire old stories"),
    }
}

// ── Doing a lot at once ──────────────────────────────────────────────────

/// Give one source its turn now, whatever its schedule says, and record what
/// happened in its fetch state. A mail-backed source is handed to
/// [`super::newsletters::refresh`].
///
/// A paused source is refreshed too: this is the path a person takes by
/// asking.
pub async fn refresh_source(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    source: NewsSourceId,
) -> RefreshReport {
    refresh_one(svc, vault, source, false).await.report
}

/// Give every source that is not paused its turn now. What "Refresh" does.
///
/// At most four at once and one per host, so a hundred sources imported from
/// one OPML file are not a hundred requests to the same few servers in the
/// same second. Raises one `Story` change if anything was stored, and one
/// `NewsSource` change if any source's health changed.
pub async fn refresh_all(svc: &Arc<Service>, vault: &Arc<Vault>) -> Vec<RefreshReport> {
    let all = blocking({
        let vault = vault.clone();
        move || Ok(sources_to_refresh(&vault, None)?)
    })
    .await;
    let all = match all {
        Ok(all) => all,
        Err(e) => {
            tracing::warn!(error = %e, "could not list the sources to refresh");
            return Vec::new();
        }
    };
    let turns = refresh_many(svc, vault, all, None, false).await;
    announce(svc, &turns);
    turns.into_iter().map(|t| t.report).collect()
}

/// The sources that are not paused -- of these `only`, or all of them -- each
/// with the host it will ask.
fn sources_to_refresh(
    vault: &Vault,
    only: Option<&[NewsSourceId]>,
) -> everyday_core::Result<Vec<(NewsSourceId, String)>> {
    Ok(vault
        .news_sources()?
        .into_iter()
        .filter(|s| !s.paused)
        .filter(|s| only.is_none_or(|ids| ids.contains(&s.id)))
        .map(|s| (s.id, host_of(&s.kind)))
        .collect())
}

/// Who a source's turn talks to, for "one per host at a time". A newsletter
/// talks to nobody but the mail index; they take turns with each other
/// under a name no host can have.
fn host_of(kind: &SourceKind) -> String {
    match kind {
        SourceKind::Web { url } => reqwest::Url::parse(url.trim())
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default(),
        SourceKind::HackerNews { .. } => HN_HOST.to_string(),
        SourceKind::Mail { .. } => "<mail>".to_string(),
    }
}

/// `sources` grouped by host, in the order each host first appears.
fn by_host(sources: Vec<(NewsSourceId, String)>) -> Vec<Vec<NewsSourceId>> {
    let mut groups: Vec<(String, Vec<NewsSourceId>)> = Vec::new();
    for (id, host) in sources {
        match groups.iter_mut().find(|(h, _)| *h == host) {
            Some((_, ids)) => ids.push(id),
            None => groups.push((host, vec![id])),
        }
    }
    groups.into_iter().map(|(_, ids)| ids).collect()
}

/// Run each host's sources one after another, and up to [`AT_ONCE`] hosts
/// together. `stop` is looked at before each source, so a pass in progress
/// ends between two sources rather than after the last.
async fn refresh_many(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    sources: Vec<(NewsSourceId, String)>,
    stop: Option<&watch::Receiver<bool>>,
    background: bool,
) -> Vec<Turn> {
    futures::stream::iter(by_host(sources))
        .map(|ids| async move {
            let mut turns = Vec::with_capacity(ids.len());
            for id in ids {
                if stop.is_some_and(|s| *s.borrow()) {
                    break;
                }
                turns.push(refresh_one(svc, vault, id, background).await);
            }
            turns
        })
        .buffer_unordered(AT_ONCE)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// Say what a batch of turns changed: one `Story` change if anything was
/// stored, however many sources it came from, and one `NewsSource` change
/// if any source's health moved.
fn announce(svc: &Arc<Service>, turns: &[Turn]) {
    if turns.iter().any(|t| t.report.inserted > 0) {
        svc.events().changed(Change::new(Kind::Story, Op::Created));
    }
    if turns.iter().any(|t| t.health_changed) {
        svc.events().changed(Change::new(Kind::NewsSource, Op::Updated));
    }
}

// ── One source ───────────────────────────────────────────────────────────

/// [`refresh_source`], with the part the loop needs: whether the source's
/// health changed. `background` is true when nobody asked -- the loop's own
/// turn -- which is when a failure is worth reaching a person who is not
/// looking at the Feed.
async fn refresh_one(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    id: NewsSourceId,
    background: bool,
) -> Turn {
    let loaded = blocking({
        let vault = vault.clone();
        move || {
            let source = vault.news_source(id)?;
            let state = vault.news_fetch_state(id)?.unwrap_or_else(|| FetchState::new(id));
            Ok((source, state))
        }
    })
    .await;
    let (source, mut state) = match loaded {
        Ok(loaded) => loaded,
        Err(e) => return Turn::unchanged(id, Some(e.message)),
    };

    let before = state.last_error.clone();
    let started = svc.now();
    state.last_attempt = Some(started);
    let result = attempt(svc, vault, &source, &mut state, started).await;

    let finished = svc.now();
    let jitter: f64 = rand::rng().random_range(-1.0..=1.0);
    let mut pausing = false;
    let mut failure = None;
    let report = match result {
        Ok(arrived) => {
            state.last_ok = Some(finished);
            state.last_error = None;
            state.failing_since = None;
            state.failures = 0;
            schedule::remember(&mut state.recent, arrived.published.iter().copied());
            let how = if arrived.fresh { Attempt::Fetched } else { Attempt::NotModified };
            let (minutes, next_at) =
                schedule::next_fetch(&state, how, &arrived.hints, finished, jitter);
            state.interval_minutes = minutes;
            state.next_at = next_at;
            svc.news_recovered(id);
            RefreshReport {
                source_id: id,
                inserted: arrived.inserted,
                not_modified: arrived.not_modified,
                error: None,
            }
        }
        Err(error) => {
            // Before the failure is counted: `next_fetch` is told this
            // attempt is not in `failures` yet.
            let (minutes, next_at) =
                schedule::next_fetch(&state, Attempt::Failed, &error.hints, finished, jitter);
            state.interval_minutes = minutes;
            state.next_at = next_at;
            state.failures = state.failures.saturating_add(1);
            state.failing_since.get_or_insert(finished);
            pausing = !source.paused && schedule::should_pause(&state, finished);
            state.last_error = Some(if pausing {
                format!(
                    "{} It has been paused after failing for {} days.",
                    error.message,
                    schedule::PAUSE_AFTER_DAYS
                )
            } else {
                error.message.clone()
            });
            tracing::info!(source = %id, error = %error.message, "a source could not be refreshed");
            let report = RefreshReport {
                source_id: id,
                inserted: 0,
                not_modified: false,
                error: Some(error.message.clone()),
            };
            failure = Some(error);
            report
        }
    };

    let saved = blocking({
        let vault = vault.clone();
        let state = state.clone();
        move || save_state(&vault, &state)
    })
    .await;
    if let Err(e) = saved {
        tracing::warn!(source = %id, error = %e, "could not record what a refresh learned");
    }

    let paused = pausing && pause(vault, id).await;
    if let Some(error) = &failure {
        // Asked once per outage, whether this turn is the first or the
        // hundredth of it.
        let first = svc.news_failed(id);
        if paused {
            notify(svc, &source, background, pause_notice(&source));
        } else if first {
            notify(svc, &source, background, failure_notice(&source, error));
        }
    }

    Turn { report, health_changed: paused || state.last_error != before }
}

/// One source's fetch, and storing what came of it.
async fn attempt(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    source: &NewsSource,
    state: &mut FetchState,
    now: Timestamp,
) -> Result<Arrived, FetchError> {
    match &source.kind {
        SourceKind::Web { url } => match fetch::feed(url, state).await? {
            Fetched::NotModified { hints } => Ok(Arrived::unchanged(hints)),
            Fetched::Body { bytes, etag, last_modified, final_url, mut hints } => {
                let payload = Payload::Feed { bytes, base: final_url };
                let stored = store(vault, source, payload, now).await?;
                // Only now: validators for a body that could not be read or
                // stored would make the next request answer `304` for it.
                state.etag = etag;
                state.last_modified = last_modified;
                hints.ttl_minutes = stored.ttl_minutes;
                Ok(settle(vault, source, state, stored, hints).await)
            }
        },
        SourceKind::HackerNews { list, min_points } => {
            let json = fetch::json(&hn::url(*list, *min_points)).await?;
            let payload = Payload::HackerNews { json, list: *list, min_points: *min_points };
            let stored = store(vault, source, payload, now).await?;
            Ok(settle(vault, source, state, stored, Hints::default()).await)
        }
        SourceKind::Mail { .. } => {
            let inserted =
                newsletters::refresh(svc, vault, source, state).await.map_err(|message| {
                    FetchError { message: http::strip_url(&message), hints: Hints::default() }
                })?;
            Ok(Arrived {
                inserted,
                not_modified: false,
                fresh: inserted > 0,
                hints: Hints::default(),
                published: Vec::new(),
            })
        }
    }
}

/// What a stored batch comes to, after the full articles it wanted. Those
/// are best-effort and slow, so they come last, once the stories are safe.
async fn settle(
    vault: &Arc<Vault>,
    source: &NewsSource,
    state: &FetchState,
    stored: Stored,
    hints: Hints,
) -> Arrived {
    if source.full_text {
        for story in stored.wants_article.iter().take(ARTICLES_PER_TURN) {
            if let Err(e) = extract::full_article(vault, *story).await {
                tracing::info!(%story, error = %e, "could not fetch a full article");
            }
        }
    }
    Arrived {
        inserted: u32::try_from(stored.inserted.len()).unwrap_or(u32::MAX),
        not_modified: false,
        fresh: !stored.inserted.is_empty() || state.last_ok.is_none(),
        hints,
        published: stored.published,
    }
}

/// Read what a fetch brought, and store it, on the blocking pool: parsing
/// and sanitising a few hundred items is real work, and so is writing them.
async fn store(
    vault: &Arc<Vault>,
    source: &NewsSource,
    payload: Payload,
    now: Timestamp,
) -> Result<Stored, FetchError> {
    let vault = vault.clone();
    let source = source.clone();
    blocking(move || {
        let (items, ttl_minutes) =
            decode(payload).map_err(|message| CommandError::new(codes::UNREADABLE, message))?;
        store_items(&vault, &source, items, ttl_minutes, now)
    })
    .await
    .map_err(|e| FetchError { message: http::strip_url(&e.message), hints: Hints::default() })
}

/// Items out of the bytes a fetch brought: a feed, or Algolia's answer.
/// Hacker News is filtered to the list's points here, because the front page
/// is the one list whose request cannot say it.
///
/// Contained for the reason `mailsync` contains reading a message: a parser
/// handed a stranger's bytes that panics must cost this source one failed
/// turn, not the task.
fn decode(payload: Payload) -> Result<(Vec<Incoming>, Option<u32>), String> {
    std::panic::catch_unwind(AssertUnwindSafe(|| match payload {
        Payload::Feed { bytes, base } => {
            parse::parse_feed(&bytes, &base).map(|feed| (feed.items, feed.ttl_minutes))
        }
        Payload::HackerNews { json, list, min_points } => hn::parse(&json).map(|mut items| {
            items.retain(|item| hn::keep(list, min_points, item));
            (items, None)
        }),
    }))
    .unwrap_or_else(|_| Err("what the site sent could not be read".to_string()))
    .map_err(|message| http::strip_url(&message))
}

/// Store what a fetch brought: the stories that are new, and the changes to
/// the ones that are not.
fn store_items(
    vault: &Vault,
    source: &NewsSource,
    items: Vec<Incoming>,
    ttl_minutes: Option<u32>,
    now: Timestamp,
) -> CommandResult<Stored> {
    let hacker_news = matches!(source.kind, SourceKind::HackerNews { .. });
    let mut known: HashSet<String> = vault.story_keys(source.id)?.into_iter().collect();
    let mut batch: Vec<(Story, Option<StoryBody>)> = Vec::new();
    let mut published = Vec::new();

    for item in items {
        published.extend(item.published);
        let title = item_title(&item);
        if title.is_empty() || item.identity.is_empty() || source.mutes(&title) {
            continue;
        }
        let key = vault.news_key(&item.identity)?;

        // Already stored: live, or a tombstone that keeps it from coming
        // back as new. Nothing about it changes but a Hacker News story's
        // points.
        if known.contains(&key) {
            if hacker_news {
                refresh_points(vault, source, &key, &item)?;
            }
            continue;
        }
        // Kept too, so a key a feed lists twice is built once.
        known.insert(key.clone());

        // Another source has this same article: it is that row, which now
        // says it was here too.
        if let Some(other) = vault.story_by_key(&key)? {
            also_on(vault, other, source, &item)?;
            continue;
        }

        // Contained: a sanitiser that panics on one item's markup costs the
        // feed that item, not the rest of it.
        let contained = AssertUnwindSafe(|| build(source, key, title, item, now));
        match std::panic::catch_unwind(contained) {
            Ok(pair) => batch.push(pair),
            Err(_) => tracing::warn!(source = %source.id, "an item could not be read; left out"),
        }
    }

    let inserted = if batch.is_empty() { Vec::new() } else { vault.insert_stories(&batch)? };
    let stored: HashSet<StoryId> = inserted.iter().copied().collect();
    let wants_article = batch
        .iter()
        .filter(|(story, body)| {
            let from_feed = body.as_ref().is_none_or(|b| b.origin == BodyOrigin::Feed);
            stored.contains(&story.id) && from_feed && !story.url.is_empty()
        })
        .map(|(story, _)| story.id)
        .collect();
    Ok(Stored { inserted, wants_article, published, ttl_minutes })
}

/// What a row calls an item: its title with the whitespace tidied. A post
/// that has none -- a toot, a note -- is called by its first words, and
/// failing those by its address.
fn item_title(item: &Incoming) -> String {
    let title = item.title.split_whitespace().collect::<Vec<_>>().join(" ");
    if !title.is_empty() {
        return title;
    }
    let content = item.content.as_deref().filter(|c| !c.trim().is_empty());
    let words = sanitize::summary(if item.summary.trim().is_empty() {
        content.unwrap_or("")
    } else {
        &item.summary
    });
    if words.is_empty() {
        return item.url.trim().to_string();
    }
    sanitize::cut_on_word(&words, UNTITLED_CHARS)
}

/// A new story, and its body when the feed carried one.
///
/// The body is the item's content, or failing that its description: a great
/// many feeds -- RSS 2.0 without `content:encoded`, which is most of
/// WordPress and Hugo, and xkcd's picture -- carry the whole post there, and
/// a reading pane that showed only its first four hundred characters would be
/// throwing the rest away. A description that is only a teaser is still a
/// body; the reading pane offers the full article beside one that looks cut
/// off, and the source's "always fetch the full article" replaces it.
fn build(
    source: &NewsSource,
    key: String,
    title: String,
    item: Incoming,
    now: Timestamp,
) -> (Story, Option<StoryBody>) {
    let mut story = Story::new(source.id, key, title, item.published.unwrap_or(now));
    story.fetched_at = now;
    story.author = item.author.trim().to_string();
    story.image_url = item.image_url;
    story.score = item.score;
    story.comments = item.comments;
    story.discussion_url = item.discussion_url;

    let content = item.content.as_deref().filter(|c| !c.trim().is_empty());
    story.summary = sanitize::summary(if item.summary.trim().is_empty() {
        content.unwrap_or("")
    } else {
        &item.summary
    });
    let base = if item.base_url.trim().is_empty() { &item.url } else { &item.base_url };
    let description = Some(item.summary.as_str()).filter(|s| !s.trim().is_empty());
    let body =
        content.or(description).map(|html| sanitize::body(story.id, html, base, item.origin));
    story.has_body = body.is_some();
    story.url = item.url;
    (story, body)
}

/// Bring a stored Hacker News story's points and comments up to date: the
/// one change a refresh makes to a story that is already stored.
fn refresh_points(
    vault: &Vault,
    source: &NewsSource,
    key: &str,
    item: &Incoming,
) -> CommandResult<()> {
    if item.score.is_none() && item.comments.is_none() {
        return Ok(());
    }
    let Some(mut stored) = vault.story_by_key(key)? else { return Ok(()) };
    if stored.source_id != source.id {
        return Ok(());
    }
    let score = item.score.or(stored.score);
    let comments = item.comments.or(stored.comments);
    if (score, comments) == (stored.score, stored.comments) {
        return Ok(());
    }
    stored.score = score;
    stored.comments = comments;
    vault.update_story(&stored)?;
    Ok(())
}

/// Record on a story another source stored first that `source` had it too.
/// Said once, but kept current: Hacker News's points move.
fn also_on(
    vault: &Vault,
    mut story: Story,
    source: &NewsSource,
    item: &Incoming,
) -> CommandResult<()> {
    let also = Also {
        source_id: source.id,
        score: item.score,
        comments: item.comments,
        discussion_url: item.discussion_url.clone(),
    };
    match story.also.iter_mut().find(|a| a.source_id == source.id) {
        Some(existing) if *existing == also => return Ok(()),
        Some(existing) => *existing = also,
        None => story.also.push(also),
    }
    vault.update_story(&story)?;
    Ok(())
}

/// Record what the refresher learned. Skipped for a source unfollowed while
/// its request was out: the cascade has already taken its fetch row, and
/// writing another would leave the loop asking for a source that is not
/// there.
fn save_state(vault: &Vault, state: &FetchState) -> CommandResult<()> {
    match vault.news_source(state.source_id) {
        Ok(_) => {}
        Err(everyday_core::Error::NotFound { .. }) => return Ok(()),
        Err(e) => return Err(e.into()),
    }
    vault.save_news_fetch_state(state)?;
    Ok(())
}

// ── Pausing, and saying so ───────────────────────────────────────────────

/// Pause a source that has failed for [`schedule::PAUSE_AFTER_DAYS`] days in
/// a row. True if it was paused by this call.
///
/// The one place the refresher writes a [`NewsSource`], and why it is
/// allowed: the alternative is asking a dead feed for ever, once a day, for
/// as long as the app is installed -- which is rude to its server and
/// pointless to its follower. It is a flag a person can turn off again, so
/// nothing they chose is lost, and the source is read afresh first: a rename
/// or a folder move made during the fetch is kept, and only `paused` changes.
async fn pause(vault: &Arc<Vault>, id: NewsSourceId) -> bool {
    let paused = blocking({
        let vault = vault.clone();
        move || {
            let mut source = vault.news_source(id)?;
            if source.paused {
                return Ok(false);
            }
            source.paused = true;
            source.touch();
            vault.save_news_source(&source)?;
            Ok(true)
        }
    })
    .await;
    match paused {
        Ok(paused) => paused,
        Err(e) => {
            tracing::warn!(source = %id, error = %e, "could not pause a failing source");
            false
        }
    }
}

/// The notification for the first failure of an outage. Never the address,
/// which is a credential for a private feed: the reason is a sentence with
/// every address already taken out of it.
fn failure_notice(source: &NewsSource, error: &FetchError) -> Notification {
    Notification::warning(format!("{} could not be refreshed", source.title))
        .body(http::strip_url(&error.message))
        .key(format!("news:{}", source.id))
}

fn pause_notice(source: &NewsSource) -> Notification {
    Notification::warning(format!("{} has been paused", source.title))
        .body(format!(
            "It has not answered for {} days, so it is not being asked any more. Turn it back \
             on in the Feed when it is working again.",
            schedule::PAUSE_AFTER_DAYS
        ))
        .key(format!("news:{}", source.id))
}

/// Raise a notification. One a background pass raises must reach a person
/// who may have the Feed closed; one raised by somebody pressing Refresh
/// is for the window they are looking at.
fn notify(svc: &Arc<Service>, source: &NewsSource, background: bool, notice: Notification) {
    tracing::info!(source = %source.id, "raising a notification for a source");
    svc.events().notify(if background { notice.for_user() } else { notice });
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::news::BodyOrigin;

    fn id() -> NewsSourceId {
        NewsSourceId::new()
    }

    fn web(url: &str) -> NewsSource {
        NewsSource::new(SourceKind::Web { url: url.into() }, "A feed")
    }

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn sources_are_grouped_by_host_in_the_order_they_first_appear() {
        let (a, b, c, d) = (id(), id(), id(), id());
        let groups = by_host(vec![
            (a, "one.example".into()),
            (b, "two.example".into()),
            (c, "one.example".into()),
            (d, "three.example".into()),
        ]);
        assert_eq!(groups, vec![vec![a, c], vec![b], vec![d]]);
        assert!(by_host(Vec::new()).is_empty());
    }

    #[test]
    fn a_sources_host_is_the_one_it_asks() {
        let web = |url: &str| host_of(&SourceKind::Web { url: url.into() });
        assert_eq!(web("https://Blog.Example.com/feed"), "blog.example.com");
        assert_eq!(web("not a url"), "");
        let hn = SourceKind::HackerNews { list: HnList::Front, min_points: 0 };
        assert_eq!(host_of(&hn), HN_HOST);
        assert_eq!(host_of(&SourceKind::Mail { sender: "a@b.example".into() }), "<mail>");
    }

    #[test]
    fn the_loop_sleeps_until_the_next_source_but_never_beyond_a_minute() {
        let now = at("2026-10-11T12:00:00Z");
        assert_eq!(sleep_for(Some(at("2026-10-11T12:00:20Z")), now), Duration::from_secs(20));
        assert_eq!(sleep_for(Some(at("2026-10-11T12:30:00Z")), now), LONGEST_SLEEP);
        assert_eq!(sleep_for(Some(at("2026-10-11T12:00:00.2Z")), now), SHORTEST_SLEEP);
        assert_eq!(sleep_for(None, now), LONGEST_SLEEP);
    }

    #[test]
    fn a_source_already_due_after_a_pass_is_not_asked_again_at_once() {
        let now = at("2026-10-11T12:00:00Z");
        assert_eq!(sleep_for(Some(at("2026-10-11T11:00:00Z")), now), LONGEST_SLEEP);
        assert_eq!(sleep_for(Some(now), now), LONGEST_SLEEP);
    }

    #[test]
    fn an_item_with_no_title_is_called_by_its_first_words_then_its_address() {
        let toot = Incoming {
            summary: "<p>Just landed in Lisbon, and the <a href=\"https://x.example\">pasteis</a> \
                      are every bit as good as everyone said they would be, honestly</p>"
                .into(),
            url: "https://social.example/@me/1".into(),
            ..Incoming::default()
        };
        let title = item_title(&toot);
        assert!(title.starts_with("Just landed in Lisbon, and the pasteis"), "{title}");
        assert!(title.chars().count() <= UNTITLED_CHARS, "{title}");

        let bare = Incoming { url: " https://social.example/@me/2 ".into(), ..Incoming::default() };
        assert_eq!(item_title(&bare), "https://social.example/@me/2");
        assert_eq!(item_title(&Incoming::default()), "");

        let titled = Incoming { title: "  Two \n words ".into(), ..Incoming::default() };
        assert_eq!(item_title(&titled), "Two words");
    }

    #[test]
    fn a_new_story_carries_what_the_item_said() {
        let source = web("https://blog.example.com/feed");
        let item = Incoming {
            identity: "https://blog.example.com/post".into(),
            title: "Post".into(),
            url: "https://blog.example.com/post".into(),
            author: " Ada ".into(),
            published: Some(at("2026-10-01T09:00:00Z")),
            summary: "<p>A short summary.</p>".into(),
            content: Some("<p>The <b>whole</b> post.</p><img src=\"/a.png\">".into()),
            base_url: "https://blog.example.com/post".into(),
            image_url: "https://blog.example.com/lead.jpg".into(),
            score: Some(12),
            comments: Some(3),
            discussion_url: "https://news.ycombinator.com/item?id=1".into(),
            origin: BodyOrigin::Feed,
        };
        let now = at("2026-10-11T12:00:00Z");
        let (story, body) = build(&source, "key".into(), "Post".into(), item, now);

        assert_eq!(story.source_id, source.id);
        assert_eq!(story.key, "key");
        assert_eq!(story.title, "Post");
        assert_eq!(story.url, "https://blog.example.com/post");
        assert_eq!(story.author, "Ada");
        assert_eq!(story.published_at, at("2026-10-01T09:00:00Z"));
        assert_eq!(story.fetched_at, now);
        assert_eq!(story.summary, "A short summary.");
        assert_eq!(story.image_url, "https://blog.example.com/lead.jpg");
        assert_eq!((story.score, story.comments), (Some(12), Some(3)));
        assert_eq!(story.discussion_url, "https://news.ycombinator.com/item?id=1");
        assert!(!story.read && !story.saved && story.also.is_empty());

        let body = body.expect("the item carried content");
        assert!(story.has_body);
        assert_eq!(body.story_id, story.id);
        assert_eq!(body.origin, BodyOrigin::Feed);
        assert!(body.html.contains("whole"), "{}", body.html);
        assert_eq!(body.images.len(), 1);
        assert_eq!(body.images[0].url, "https://blog.example.com/a.png");
    }

    #[test]
    fn an_item_with_only_a_description_takes_it_as_its_body_and_dates_itself_from_now() {
        let source = web("https://blog.example.com/feed");
        let item = Incoming {
            identity: "guid:1".into(),
            title: "Post".into(),
            summary: "Only this.".into(),
            ..Incoming::default()
        };
        let now = at("2026-10-11T12:00:00Z");
        let (story, body) = build(&source, "key".into(), "Post".into(), item, now);
        assert!(body.expect("the description is the body").text.contains("Only this."));
        assert!(story.has_body);
        assert_eq!(story.summary, "Only this.");
        assert_eq!(story.published_at, now);
    }

    #[test]
    fn a_summary_is_made_from_the_content_when_the_item_had_none() {
        let source = web("https://blog.example.com/feed");
        let item = Incoming {
            identity: "guid:1".into(),
            title: "Post".into(),
            content: Some("<p>From the content.</p>".into()),
            ..Incoming::default()
        };
        let now = at("2026-10-11T12:00:00Z");
        let (story, _) = build(&source, "k".into(), "Post".into(), item, now);
        assert_eq!(story.summary, "From the content.");
    }
}
