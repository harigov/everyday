//! The Feed's moving parts: fetching, parsing, cleaning and storing what
//! somebody follows. See `docs/plans/feed.md`.
//!
//! "Feed" is the app; `news` is the domain, because `feed` already means a
//! subscribed calendar in this crate (`crate::feeds`).
//!
//! ```text
//!   parse, hn, canonical,     pure: bytes in, `Incoming` out. No socket,
//!   discover, opml            no vault -- everything here is under test
//!                             with fixtures
//!   fetch                     the network: conditional GET for a feed,
//!                             discovery, a site's icon
//!   sanitize                  a story's HTML -> a `StoryBody`, through
//!                             `karmayoga-mail`'s sanitiser
//!   extract                   "fetch the full article": the linked page,
//!                             through a Readability port
//!   refresh                   one source's turn, and the supervised loop
//!                             that gives every source its turn
//!   newsletters               the mail-backed source: no network at all
//!   view                      what `karmayoga://news/...` serves
//!   work                      the lists the background tasks of transcripts and
//!                             audio work through, and the loop that does it
//! ```
//!
//! Two HTTP clients, by who chose the address. The address a person typed
//! goes through [`crate::http::client`], as a calendar subscription does --
//! it honours the system proxy and may point at their own network. Anything
//! a feed *names* -- a picture, an article page, a site's icon -- goes
//! through [`crate::http::public_client`], whose resolver and redirects
//! refuse private and loopback addresses.

pub mod article;
pub mod audio;
pub mod automation;
pub mod canonical;
pub mod cleanup;
pub mod consumed;
pub mod directory;
pub mod discover;
pub mod downloads;
pub mod extract;
pub mod fetch;
pub mod hn;
pub mod keep;
pub mod listen;
pub mod newsletters;
pub mod opml;
pub mod parse;
pub mod queue;
pub mod rank;
pub mod refresh;
pub mod sanitize;
pub mod summary;
pub mod transcribe;
pub mod view;
pub mod work;

use crate::events::{Change, Kind, Op};
use crate::service::Service;
use jiff::Timestamp;
use karmayoga_core::Vault;
use karmayoga_core::id::NewsSourceId;
use karmayoga_core::news::{BodyOrigin, Enclosure};
use serde::Serialize;
use std::sync::Arc;

/// The supervisor key of the one refresher task.
pub const TASK_KEY: &str = "news";

/// What podcasts, Up Next and Recommended keep in memory while the vault is
/// unlocked: the downloads under way and the ranker's pacing. One per
/// [`Service`] (`Service::listening`). See `docs/plans/podcasts.md`.
#[derive(Default)]
pub struct Listening {
    pub downloads: downloads::Downloads,
    pub ranker: rank::Ranker,
    pub transcripts: transcribe::Transcripts,
    pub audio: audio::AudioJobs,
}

impl Listening {
    /// The vault locked: forget everything.
    pub fn clear(&self) {
        self.downloads.clear();
        self.ranker.clear();
        self.transcripts.clear();
        self.audio.clear();
    }
}

/// Raise one change: `ids` as the `id` when there is one, as `ids` when more,
/// and nothing when there are none. What every background task of the Feed
/// says what it did with.
pub(crate) fn announce(svc: &Service, kind: Kind, op: Op, ids: Vec<String>) {
    let (id, ids) = match ids.len() {
        0 => return,
        1 => (ids.into_iter().next(), Vec::new()),
        _ => (None, ids),
    };
    svc.events().changed(Change { kind, op, id, ids, origin: None });
}

/// [`announce`] for the one record.
pub(crate) fn announce_one(svc: &Service, kind: Kind, op: Op, id: impl ToString) {
    announce(svc, kind, op, vec![id.to_string()]);
}

/// One piece of background work on the Feed's behalf -- a transcript, an
/// article, an audio version -- as the interface shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItem {
    pub kind: WorkKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub story_id: Option<karmayoga_core::id::StoryId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_id: Option<karmayoga_core::id::FileId>,
    pub title: String,
    pub state: WorkState,
    /// From 0 to 1, when it can be told.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkKind {
    Transcript,
    Article,
    Audio,
    /// A summary of an article ([`summary`]).
    Summary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkState {
    Waiting,
    Working,
    Failed,
}

/// One item as a parser read it, before it is a [`karmayoga_core::Story`]:
/// no id, no key, nothing sanitised. What [`parse::parse_feed`] and
/// [`hn::parse`] produce and [`refresh`] consumes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Incoming {
    /// What the dedupe key is a hash of: the canonical address when the item
    /// has one, else its guid, else its title -- see
    /// [`canonical::identity`].
    pub identity: String,
    pub title: String,
    /// The article's address, absolute. Empty when it has none.
    pub url: String,
    pub author: String,
    /// When the publisher says it was published. `None` when it did not say;
    /// the refresher then uses the first time it saw it.
    pub published: Option<Timestamp>,
    /// The summary or description, as HTML or text, unsanitised.
    pub summary: String,
    /// The full content, when the feed carried it, as HTML, unsanitised.
    pub content: Option<String>,
    /// What relative addresses in `summary` and `content` resolve against:
    /// the item's link, or the feed's own address.
    pub base_url: String,
    /// A lead picture the feed named (`media:thumbnail`, an image
    /// enclosure). Absolute. Empty when none.
    pub image_url: String,
    /// Hacker News points and comments.
    pub score: Option<u32>,
    pub comments: Option<u32>,
    /// Where the discussion is. Empty when there is none.
    pub discussion_url: String,
    /// Where `content` came from, if there is any.
    pub origin: BodyOrigin,
    /// The episode this item is, when it carries something to hear or watch:
    /// a playable enclosure, with what the feed said about it. Absolute.
    pub media: Option<Enclosure>,
}

/// A whole feed, as [`parse::parse_feed`] read it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedFeed {
    /// The feed's own title. Empty when it had none.
    pub title: String,
    /// The site it belongs to. Empty when it did not say.
    pub site_url: String,
    pub items: Vec<Incoming>,
    /// RSS `<ttl>`, or `sy:updatePeriod` x `sy:updateFrequency`, in minutes.
    pub ttl_minutes: Option<u32>,
    /// A podcast: the items carry playable enclosures, or the feed's
    /// `itunes:` tags say it is one.
    pub podcast: bool,
    /// The feed's own picture (`itunes:image`, else its logo), absolute.
    /// Empty when it had none.
    pub image_url: String,
    /// `itunes:author`, else the feed's author. Empty when it named none.
    pub author: String,
}

/// A feed found at an address somebody pasted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundFeed {
    /// The feed's own address, absolute.
    pub url: String,
    /// Its title, from the `<link title>` or the feed itself. Empty when
    /// neither said.
    pub title: String,
}

/// What one source's turn did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshReport {
    pub source_id: NewsSourceId,
    /// Stories stored that were not there before.
    pub inserted: u32,
    /// The server said nothing had changed.
    pub not_modified: bool,
    /// Why it failed, with any address stripped. `None` on success.
    pub error: Option<String>,
}

/// Start the refresher if it is not already running. Idempotent -- see
/// [`crate::supervisor::Supervisor::ensure`] -- so it is called from every
/// unlock and costs nothing when the task is already there.
pub fn ensure_task(svc: &Arc<Service>, vault: &Arc<Vault>) {
    if !vault.supports_news() {
        return;
    }
    // Looks the vault up each time it starts rather than keeping one: the
    // supervisor keeps this for as long as the process runs, and a vault held
    // here would keep its write lock through a close, so opening it again would
    // come up read-only, and a restart after switching would be refreshing the
    // wrong vault.
    let svc_for_task = svc.clone();
    svc.supervisor().ensure(TASK_KEY, move |stop| {
        let svc = svc_for_task.clone();
        Box::pin(async move {
            match svc.get() {
                Some(vault) if vault.supports_news() => refresh::run(svc, vault, stop).await,
                _ => Ok(crate::supervisor::Outcome::Done),
            }
        })
    });
}
