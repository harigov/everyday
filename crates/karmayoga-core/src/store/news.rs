//! Storage for the Feed: sources, what the refresher learned about each, the
//! stories they brought, and those stories' bodies.
//!
//! # Why this is its own trait
//!
//! The library's reason: a backend that keeps journals as Markdown files has
//! no opinion about a feed reader, and folding one into
//! [`JournalStore`](super::JournalStore) would oblige it to grow one. So the
//! domain is reached through [`JournalStore::news`](super::JournalStore::news),
//! `None` by default, and the interface reads
//! [`Capabilities::news`](super::Capabilities::news) to know whether to offer
//! the app at all.
//!
//! # The rules an implementation must keep
//!
//! Every one of these is in the conformance suite (`conformance/news.rs`):
//!
//! - **Deleting a source takes everything under it**: its stories (saved ones
//!   too -- unfollowing is the explicit act), their bodies, and its fetch
//!   state.
//! - **Inserting is idempotent by `(source_id, key)`.** A story already
//!   stored under that pair -- tombstone or not -- is left exactly as it is,
//!   and is not in the returned list. That is what makes a refresh safe to
//!   run as often as anybody likes, and what stops a feed that still lists an
//!   old item from bringing it back as new.
//! - **`read`, `saved` and `expired` are clear columns, and the columns win.**
//!   Every read fills those three fields from the columns, never from the
//!   sealed copy, so marking a thousand stories read is one `UPDATE` that
//!   decrypts nothing. [`NewsStore::update_story`] never touches them.
//! - **A mail-backed story's read state is the message's.** For a story with
//!   [`Story::mail_message`] set, `read` is the `SEEN` bit of that
//!   `mail_messages` row's `flags`, read through a join; its own column is
//!   ignored. A story whose message is gone is never listed.
//! - **Expiry leaves a tombstone.** An expired story keeps its row -- id,
//!   source, key, date -- with its sealed content cleared and its body
//!   deleted, and is never listed or counted again. A saved story never
//!   expires. A tombstone older than [`TOMBSTONE_DAYS`] is deleted outright.
//!
//! [`TOMBSTONE_DAYS`]: crate::news::TOMBSTONE_DAYS

use crate::error::Result;
use crate::id::{BlobId, MailMessageId, NewsSourceId, StoryId};
use crate::news::{
    ExpireReport, ExpiryRule, FetchState, NewsSource, SourceCount, Story, StoryBody,
};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Where a page of stories ends: newest first, so the next page is
/// everything strictly older than this `(published_at, id)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct StoryCursor {
    pub published_at: Timestamp,
    pub id: StoryId,
}

/// Filter and page for [`NewsStore::list_stories`]. All filters are ANDed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", default)]
pub struct StoryQuery {
    /// Restrict to these sources. Empty means every source.
    pub sources: Vec<NewsSourceId>,
    /// Only stories not yet read.
    pub unread_only: bool,
    /// Only saved stories.
    pub saved_only: bool,
    /// Only stories published at or after this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<Timestamp>,
    /// Continue after this cursor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<StoryCursor>,
    /// At most this many; `0` means [`StoryQuery::DEFAULT_LIMIT`].
    pub limit: u32,
}

impl StoryQuery {
    pub const DEFAULT_LIMIT: u32 = 50;
    pub const MAX_LIMIT: u32 = 500;

    /// The limit to actually use.
    pub fn limit(&self) -> u32 {
        match self.limit {
            0 => Self::DEFAULT_LIMIT,
            n => n.min(Self::MAX_LIMIT),
        }
    }
}

/// One page of stories, newest first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct StoryPage {
    pub stories: Vec<Story>,
    /// Where the next page starts. `None` when this was the last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<StoryCursor>,
}

/// The persistence contract for the Feed.
pub trait NewsStore: Send + Sync {
    // ---- sources --------------------------------------------------------

    /// Every source. There are tens of these; ordering is the caller's.
    fn list_sources(&self) -> Result<Vec<NewsSource>>;

    fn get_source(&self, id: NewsSourceId) -> Result<NewsSource>;

    /// Insert or replace. Idempotent.
    fn put_source(&self, source: &NewsSource) -> Result<()>;

    /// Delete the source, every story it brought (saved ones too), their
    /// bodies, and its fetch state.
    fn delete_source(&self, id: NewsSourceId) -> Result<()>;

    // ---- what the refresher learned -------------------------------------

    /// `None` for a source nobody has fetched yet.
    fn fetch_state(&self, source: NewsSourceId) -> Result<Option<FetchState>>;

    /// Insert or replace.
    fn put_fetch_state(&self, state: &FetchState) -> Result<()>;

    /// Every source whose `next_at` is at or before `now`, plus every source
    /// with no fetch state at all, soonest first. Paused sources are the
    /// caller's to skip: `paused` is sealed.
    fn due_sources(&self, now: Timestamp) -> Result<Vec<NewsSourceId>>;

    /// When the soonest fetch is due, across every source with state.
    /// `None` when no source has any. What the refresher sleeps until.
    fn next_due(&self) -> Result<Option<Timestamp>>;

    // ---- stories --------------------------------------------------------

    /// Store what arrived, each with its body if it carried one. Idempotent
    /// by `(source_id, key)`: anything already stored under that pair is left
    /// alone. Returns the ids actually inserted, in input order. A story's
    /// `read`, `saved` and `expired` go into their columns as given.
    fn insert_stories(&self, stories: &[(Story, Option<StoryBody>)]) -> Result<Vec<StoryId>>;

    /// A live (not expired) story stored under `key` by *any* source, if one
    /// is: how the same article arriving from a second source is folded into
    /// the first one's row.
    fn story_by_key(&self, key: &str) -> Result<Option<Story>>;

    /// The `(source_id, key)` pairs already stored for one source, live or
    /// tombstoned -- so a refresh can tell what is new before it does any
    /// work on it.
    fn keys_for_source(&self, source: NewsSourceId) -> Result<Vec<String>>;

    /// Rewrite a stored story's sealed content: the fields a refresh owns
    /// after insert (`score`, `comments`, `also`, `has_body`). Never touches
    /// `read`, `saved` or `expired`, whatever the record passed in says.
    fn update_story(&self, story: &Story) -> Result<()>;

    fn list_stories(&self, query: &StoryQuery) -> Result<StoryPage>;

    fn get_story(&self, id: StoryId) -> Result<Story>;

    /// Many stories at once, in no particular order; ids that do not exist
    /// are skipped rather than failing the whole call.
    fn get_stories(&self, ids: &[StoryId]) -> Result<Vec<Story>> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            match self.get_story(*id) {
                Ok(s) => out.push(s),
                Err(crate::error::Error::NotFound { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// Every live story pointing at one of these mail messages, as
    /// `(message, story)` pairs. How newsletter ingest avoids a second story
    /// for a message it already turned into one.
    fn stories_for_mail(&self, messages: &[MailMessageId])
    -> Result<Vec<(MailMessageId, StoryId)>>;

    // ---- bodies ---------------------------------------------------------

    fn get_body(&self, story: StoryId) -> Result<Option<StoryBody>>;

    /// Insert or replace, and set the story's `has_body`.
    fn put_body(&self, body: &StoryBody) -> Result<()>;

    // ---- state ----------------------------------------------------------

    /// One `UPDATE` over the clear column. Unknown ids are ignored.
    fn set_read(&self, ids: &[StoryId], read: bool) -> Result<()>;

    /// Mark every live, unread story read in these sources (every source
    /// when empty) published at or before `up_to`. Returns the ids that
    /// changed. Mail-backed stories are left out: their read state is the
    /// message's, and only mail can change it.
    fn mark_all_read(&self, sources: &[NewsSourceId], up_to: Timestamp) -> Result<Vec<StoryId>>;

    fn set_saved(&self, ids: &[StoryId], saved: bool) -> Result<()>;

    /// Unread and total live stories per source, as one `GROUP BY` that
    /// decrypts nothing. Sources with no stories may be absent.
    fn counts(&self) -> Result<Vec<SourceCount>>;

    /// Turn stories older than their source's retention into tombstones, and
    /// delete tombstones older than [`crate::news::TOMBSTONE_DAYS`]. A source
    /// with no rule in `rules` gets [`crate::news::Retention::default`].
    /// Saved stories are never expired.
    fn expire(&self, rules: &[ExpiryRule], now: Timestamp) -> Result<ExpireReport>;

    /// Every blob a source or a body holds -- source icons and cached
    /// pictures -- for [`JournalStore::collect_garbage`](super::JournalStore::collect_garbage).
    fn blob_refs(&self) -> Result<Vec<BlobId>>;
}

/// Associated data bound into a record's ciphertext. See
/// [`entry_aad`](super::entry_aad) for why records are bound to their id.
pub fn source_aad(id: NewsSourceId) -> Vec<u8> {
    format!("everyday.news_source.v1:{id}").into_bytes()
}

pub fn story_aad(id: StoryId) -> Vec<u8> {
    format!("everyday.story.v1:{id}").into_bytes()
}

/// A body is bound to its story's id under a label of its own, so a body row
/// can never be passed off as the story row it belongs to, or the reverse.
pub fn story_body_aad(id: StoryId) -> Vec<u8> {
    format!("everyday.story_body.v1:{id}").into_bytes()
}

/// Fetch state is keyed by its source's id, under a label of its own.
pub fn fetch_state_aad(id: NewsSourceId) -> Vec<u8> {
    format!("everyday.news_fetch.v1:{id}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_limit_is_the_default_and_a_huge_one_is_capped() {
        assert_eq!(StoryQuery::default().limit(), StoryQuery::DEFAULT_LIMIT);
        assert_eq!(
            StoryQuery { limit: 10_000, ..Default::default() }.limit(),
            StoryQuery::MAX_LIMIT
        );
        assert_eq!(StoryQuery { limit: 7, ..Default::default() }.limit(), 7);
    }
}
