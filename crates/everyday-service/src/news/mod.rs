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
//!                             `everyday-mail`'s sanitiser
//!   extract                   "fetch the full article": the linked page,
//!                             through a Readability port
//!   refresh                   one source's turn, and the supervised loop
//!                             that gives every source its turn
//!   newsletters               the mail-backed source: no network at all
//!   view                      what `everyday://news/...` serves
//! ```
//!
//! Two HTTP clients, by who chose the address. The address a person typed
//! goes through [`crate::http::client`], as a calendar subscription does --
//! it honours the system proxy and may point at their own network. Anything
//! a feed *names* -- a picture, an article page, a site's icon -- goes
//! through [`crate::http::public_client`], whose resolver and redirects
//! refuse private and loopback addresses.

pub mod canonical;
pub mod discover;
pub mod extract;
pub mod fetch;
pub mod hn;
pub mod newsletters;
pub mod opml;
pub mod parse;
pub mod refresh;
pub mod sanitize;
pub mod view;

use crate::service::Service;
use everyday_core::Vault;
use everyday_core::id::NewsSourceId;
use everyday_core::news::BodyOrigin;
use jiff::Timestamp;
use serde::Serialize;
use std::sync::Arc;

/// The supervisor key of the one refresher task.
pub const TASK_KEY: &str = "news";

/// One item as a parser read it, before it is a [`everyday_core::Story`]:
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
    let svc_for_task = svc.clone();
    let vault_for_task = vault.clone();
    svc.supervisor().ensure(TASK_KEY, move |stop| {
        let svc = svc_for_task.clone();
        let vault = vault_for_task.clone();
        Box::pin(async move { refresh::run(svc, vault, stop).await })
    });
}
