//! The Feed: what somebody chose to follow, and the stories it brought.
//!
//! "Feed" is what the interface calls the app; `news` is what the code calls
//! the domain, because `feed` already means a subscribed *calendar*
//! throughout this tree (`everyday-service/src/feeds.rs`,
//! `Service::feed_failed`, the `feed:{id}` notification key) and a second
//! meaning would make every search for it ambiguous. See `docs/plans/feed.md`.
//!
//! ```text
//!   NewsSource ────── Story ────── StoryBody
//!   (you chose it)    (it arrived)  (what it said, read when opened)
//!        │
//!        └── FetchState             (what the refresher learned)
//! ```
//!
//! # Who writes what
//!
//! A [`NewsSource`] holds only what a person chose: its address, its name,
//! its folder and its switches. What the refresher learns about it -- the
//! validators a conditional request sends back, the last error, when to ask
//! again -- lives in a [`FetchState`] beside it, in a table of its own, so
//! that a rename made while a fetch is in flight can never be overwritten by
//! the fetch. That is the calendar's rule again: nothing a sync does can
//! touch a record you made.
//!
//! A [`Story`] is the light half of what arrived: enough to draw a row and
//! nothing more. Its [`StoryBody`] is a row of its own, read when the story
//! is opened, for the reason a mail `Body` is apart from its `Message`: a
//! list is drawn far more often than a body is read.
//!
//! # What this crate does not do
//!
//! Open a socket, parse XML, or touch HTML. The core stays free of all three,
//! as it is for calendars: `feed-rs`, the sanitiser and the article
//! extractor live in `everyday-service::news`, the way `calcard` lives in
//! `everyday-service::feeds`. What is here is the records, and the
//! arithmetic that decides things -- [`schedule`] -- which is pure and
//! under test.

pub mod opml;
pub mod schedule;

use crate::id::{BlobId, MailMessageId, NewsSourceId, StoryId};
use crate::timestamped::Timestamped;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// The label [`crate::Vault::news_key`] derives its hashing key under.
///
/// Pinned: changing it changes every story's dedupe key, which would make
/// every feed's whole backlog arrive again as new.
pub const NEWS_KEY_LABEL: &str = "everyday.news.key.v1";

// ── Sources ──────────────────────────────────────────────────────────────

/// Something a person chose to follow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct NewsSource {
    pub id: NewsSourceId,
    pub kind: SourceKind,
    /// What the sidebar calls it. Filled from the feed's own title when it is
    /// added, and the person's from then on: the refresher never writes it.
    pub title: String,
    /// The sidebar folder this sits under. Empty means none. A sealed string
    /// rather than a record of its own: there are tens of sources, they are
    /// decrypted together, and a folder table would be a second thing to
    /// keep in step for nothing.
    #[serde(default)]
    pub folder: String,
    /// The site the feed belongs to, for "open the site". Empty when the feed
    /// did not say.
    #[serde(default)]
    pub site_url: String,
    /// The site's icon, downloaded once into the blob store when the source
    /// was added -- never a URL the webview loads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<BlobId>,
    /// Load a story's pictures when it is opened. On by default for a web
    /// feed and Hacker News: the publisher already sees every refresh, so the
    /// read-receipt argument that keeps mail's pictures off is weaker here.
    /// A mail-backed source ignores it and follows mail's own rules.
    #[serde(default = "yes")]
    pub images: bool,
    /// Fetch the linked page and extract the article for every new story,
    /// rather than only when somebody asks. For feeds that publish a summary
    /// and nothing else.
    #[serde(default)]
    pub full_text: bool,
    /// Mail-backed sources only: archive each new issue on arrival, so it
    /// lives in the Feed and not the inbox. The one thing this app does that
    /// reaches outside the vault, so it is off until somebody turns it on.
    #[serde(default)]
    pub keep_out_of_inbox: bool,
    /// Words that drop a story whose title contains one, case-insensitively.
    #[serde(default)]
    pub mute: Vec<String>,
    /// How long this source's stories are kept. `None` is
    /// [`Retention::default`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<Retention>,
    /// Stop asking. A source that has failed for thirty days in a row pauses
    /// itself; a person can pause one too.
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub sort_order: i64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

fn yes() -> bool {
    true
}

impl NewsSource {
    pub fn new(kind: SourceKind, title: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: NewsSourceId::new(),
            kind,
            title: title.into(),
            folder: String::new(),
            site_url: String::new(),
            icon: None,
            images: true,
            full_text: false,
            keep_out_of_inbox: false,
            mute: Vec::new(),
            retention: None,
            paused: false,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// The retention that applies: this source's own, or the default.
    pub fn retention(&self) -> Retention {
        self.retention.unwrap_or_default()
    }

    /// Does a mute word drop a story with this title?
    pub fn mutes(&self, title: &str) -> bool {
        let title = title.to_lowercase();
        self.mute
            .iter()
            .map(|w| w.trim().to_lowercase())
            .any(|w| !w.is_empty() && title.contains(&w))
    }
}

impl Timestamped for NewsSource {
    fn touch(&mut self) {
        self.updated_at = Timestamp::now();
    }
}

/// Where a source's stories come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SourceKind {
    /// RSS 0.9x, 1.0 and 2.0, Atom, and JSON Feed, at this address. The
    /// address is sealed with the rest of the record: a private feed's
    /// address -- a paid newsletter's, a Patreon's -- is a bearer credential,
    /// exactly as a calendar's secret address is.
    #[serde(rename_all = "camelCase")]
    Web { url: String },
    /// Hacker News, through Algolia's API: one request for a whole list, with
    /// points and comment counts, and no key.
    #[serde(rename_all = "camelCase")]
    HackerNews {
        list: HnList,
        /// Leave out anything with fewer points than this. Zero keeps
        /// everything; [`HnList::Top`] uses it as its whole definition.
        #[serde(default)]
        min_points: u32,
    },
    /// Newsletters already arriving in Mail, from one sender. Fetches
    /// nothing: its stories point at messages the mail engine already
    /// stored, and their read state *is* the message's.
    ///
    /// Matched by sender address alone. A stored message keeps no memory of
    /// the `List-Id` it arrived with (see `mail::categorize`), and one sender
    /// is what a person picks from the list of who writes to them anyway.
    #[serde(rename_all = "camelCase")]
    Mail {
        /// The sender's address, lower-cased.
        sender: String,
    },
}

impl SourceKind {
    /// The word the interface and the assistant use for this kind.
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceKind::Web { .. } => "web",
            SourceKind::HackerNews { .. } => "hackerNews",
            SourceKind::Mail { .. } => "mail",
        }
    }
}

/// Which Hacker News list a source follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum HnList {
    /// The front page, as it stands at each refresh.
    #[default]
    Front,
    /// Show HN.
    Show,
    /// Ask HN.
    Ask,
    /// Every story above `min_points`, newest first: the "best of, as it
    /// happens" list people build out of hnrss.org.
    Top,
}

impl HnList {
    pub fn as_str(self) -> &'static str {
        match self {
            HnList::Front => "front",
            HnList::Show => "show",
            HnList::Ask => "ask",
            HnList::Top => "top",
        }
    }
}

/// How long a source's stories are kept, in days.
///
/// News expires and saving keeps it: a saved story is never expired, and
/// everything else goes once it is older than this. What is left of an
/// expired story is a tombstone -- see [`Story::expired`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Retention {
    pub read_days: u32,
    pub unread_days: u32,
}

impl Default for Retention {
    fn default() -> Self {
        Self { read_days: 30, unread_days: 90 }
    }
}

/// How long a tombstone outlives its story. Long enough that a feed which
/// still lists an old item does not bring it back as new; short enough that
/// a vault does not keep a row for every story it ever saw.
pub const TOMBSTONE_DAYS: u32 = 365;

/// What the refresher has learned about one source. Machine-owned: written
/// only by the refresher, never by a person, and never the [`NewsSource`]
/// record itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct FetchState {
    pub source_id: NewsSourceId,
    /// The validators a conditional request sends back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ok: Option<Timestamp>,
    /// Why the last attempt failed, with any address stripped out of it.
    /// Cleared by the next success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// When the current run of failures began. `None` while healthy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failing_since: Option<Timestamp>,
    /// Failures in a row.
    #[serde(default)]
    pub failures: u32,
    /// What [`schedule::next_fetch`] settled on last time, in minutes.
    #[serde(default)]
    pub interval_minutes: u32,
    /// When to ask next.
    pub next_at: Timestamp,
    /// The publication times of the newest stories seen, newest first, at
    /// most [`schedule::RECENT_KEPT`] of them -- what the interval is
    /// stretched towards.
    #[serde(default)]
    pub recent: Vec<Timestamp>,
    /// Mail-backed sources: the newest message date already turned into a
    /// story, so the next turn asks the mail index only for what is after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark: Option<Timestamp>,
}

impl FetchState {
    /// The state of a source nobody has fetched yet: due now.
    pub fn new(source_id: NewsSourceId) -> Self {
        Self {
            source_id,
            etag: None,
            last_modified: None,
            last_attempt: None,
            last_ok: None,
            last_error: None,
            failing_since: None,
            failures: 0,
            interval_minutes: 0,
            next_at: Timestamp::now(),
            recent: Vec::new(),
            watermark: None,
        }
    }
}

/// A source with what the sidebar draws beside it: the refresher's view of
/// its health and how many stories it has waiting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SourceSummary {
    pub source: NewsSource,
    pub unread: u32,
    pub total: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ok: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_at: Option<Timestamp>,
}

/// One source's counts, as the store answers them with a `GROUP BY` that
/// decrypts nothing. Expired stories are counted in neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceCount {
    pub source_id: NewsSourceId,
    pub unread: u32,
    pub total: u32,
}

// ── Stories ──────────────────────────────────────────────────────────────

/// One thing that arrived: the light half, enough to draw a row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Story {
    pub id: StoryId,
    pub source_id: NewsSourceId,
    /// The keyed hash of the story's canonical address (or its guid, when it
    /// has no address) -- see [`crate::Vault::news_key`]. What makes a
    /// refresh idempotent, and what collapses the same article arriving from
    /// two sources into one row. Hex.
    pub key: String,
    pub title: String,
    /// The article itself. Empty for an Ask HN post, which has no link.
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub author: String,
    /// When the publisher says it was published, or when it was first seen
    /// if the publisher did not say.
    pub published_at: Timestamp,
    pub fetched_at: Timestamp,
    /// Plain text, at most [`SUMMARY_CHARS`] characters.
    #[serde(default)]
    pub summary: String,
    /// A lead picture the feed named, if it named one. Never loaded by the
    /// webview directly; see `everyday-service::news`.
    #[serde(default)]
    pub image_url: String,
    /// Hacker News points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<u32>,
    /// Hacker News comment count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comments: Option<u32>,
    /// Where the conversation about it is. Empty when there is none.
    #[serde(default)]
    pub discussion_url: String,
    /// The other sources this same story arrived from after it was first
    /// stored -- "also on Hacker News, 412 points".
    #[serde(default)]
    pub also: Vec<Also>,
    /// Newsletters: the message this story is. Its body is drawn through
    /// mail's own route, and its read state is the message's seen flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mail_message: Option<MailMessageId>,
    /// Read. Filled by the store from its clear column (or, for a mail-backed
    /// story, from the message's flags) on every read; the sealed copy is not
    /// authoritative. See [`crate::store::news::NewsStore::set_read`].
    #[serde(default)]
    pub read: bool,
    /// Saved: kept past expiry. Filled from the clear column, like `read`.
    #[serde(default)]
    pub saved: bool,
    /// A body row exists: the feed carried content, or an article was
    /// extracted.
    #[serde(default)]
    pub has_body: bool,
    /// Expired: a tombstone, kept so the feed cannot bring it back as new.
    /// Never returned by a list.
    #[serde(default)]
    pub expired: bool,
}

/// How long a story's plain summary may be, in characters.
pub const SUMMARY_CHARS: usize = 400;

impl Story {
    pub fn new(
        source_id: NewsSourceId,
        key: impl Into<String>,
        title: impl Into<String>,
        published_at: Timestamp,
    ) -> Self {
        Self {
            id: StoryId::new(),
            source_id,
            key: key.into(),
            title: title.into(),
            url: String::new(),
            author: String::new(),
            published_at,
            fetched_at: Timestamp::now(),
            summary: String::new(),
            image_url: String::new(),
            score: None,
            comments: None,
            discussion_url: String::new(),
            also: Vec::new(),
            mail_message: None,
            read: false,
            saved: false,
            has_body: false,
            expired: false,
        }
    }
}

/// Another source a stored story also arrived from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Also {
    pub source_id: NewsSourceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comments: Option<u32>,
    #[serde(default)]
    pub discussion_url: String,
}

/// What a story said: sanitised once, when it arrived or was extracted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct StoryBody {
    pub story_id: StoryId,
    /// Sanitised HTML. Every picture is already rewritten to
    /// `everyday://news/img/{story}/{token}`; nothing with behaviour is left.
    pub html: String,
    /// The same as plain text, for the assistant and for search.
    #[serde(default)]
    pub text: String,
    pub origin: BodyOrigin,
    /// The pictures `html` refers to, by token.
    #[serde(default)]
    pub images: Vec<NewsImage>,
}

/// Where a body came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum BodyOrigin {
    /// The feed carried it.
    #[default]
    Feed,
    /// Extracted from the linked page.
    Extracted,
    /// An Ask HN post's own text.
    HackerNews,
}

/// One picture a body refers to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct NewsImage {
    /// `blake3(url)`, hex -- what the rewritten HTML names.
    pub token: String,
    /// The address the publisher named.
    pub url: String,
    /// Set once fetched: the blob holding its bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_blob: Option<BlobId>,
}

/// One source's retention, as handed to
/// [`crate::store::news::NewsStore::expire`]. The store cannot read a
/// source's retention itself -- it is sealed -- so the caller decrypts the
/// sources and passes this down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpiryRule {
    pub source_id: NewsSourceId,
    pub retention: Retention,
}

/// What an expiry pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpireReport {
    /// Stories turned into tombstones.
    pub expired: u32,
    /// Tombstones removed for good.
    pub removed: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mute_words_match_titles_case_insensitively_and_ignore_blanks() {
        let mut s = NewsSource::new(SourceKind::Web { url: "https://x.example/feed".into() }, "X");
        s.mute = vec!["Crypto".into(), "  ".into()];
        assert!(s.mutes("Why crypto failed"));
        assert!(!s.mutes("Why Rust succeeded"));
    }

    #[test]
    fn a_source_kind_is_tagged_by_type() {
        let json =
            serde_json::to_value(SourceKind::HackerNews { list: HnList::Top, min_points: 100 })
                .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "type": "hackerNews", "list": "top", "minPoints": 100 })
        );
    }

    #[test]
    fn images_default_on_when_absent() {
        let json = serde_json::json!({
            "id": NewsSourceId::new(),
            "kind": { "type": "web", "url": "https://x.example/feed" },
            "title": "X",
            "createdAt": "2026-10-11T00:00:00Z",
            "updatedAt": "2026-10-11T00:00:00Z",
        });
        let s: NewsSource = serde_json::from_value(json).unwrap();
        assert!(s.images);
        assert_eq!(s.retention(), Retention { read_days: 30, unread_days: 90 });
    }
}
