//! Up Next, and the Feed's memory of what was read, heard and watched.
//!
//! ```text
//!   ContentRef ── a story, a file, a library item: anything to read or hear
//!       │
//!       ├── QueueEntry    it is in Up Next, at this place
//!       ├── Consumption   it was read / heard / watched, this far, then
//!       └── Rank          how well it fits the person's criteria
//! ```
//!
//! # One record per thing, not per sitting
//!
//! A [`Consumption`] is kept per [`ContentRef`]: coming back to an episode
//! moves its position on and widens what was heard, rather than adding a
//! row. "What did I listen to last week" is a question about when each thing
//! was last touched and how much of it there was, which one row answers.
//!
//! # Why a consumption outlives its story
//!
//! A story expires; what somebody read must not. So a consumption carries
//! its own title, source and address, and points at the words kept in the
//! files folder ([`Consumption::text_file`]) -- an article saved as Markdown,
//! an episode's transcript -- which the document index finds and the reader
//! opens, long after the story is a tombstone.

use crate::id::{ConsumptionId, FileId, ItemId, QueueEntryId, StoryId};
use crate::timestamped::Timestamped;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Something to read, hear or watch.
///
/// `{"type": "story", "id": "…"}` on the wire. [`ContentRef::key`] is how the
/// store keys it, in the clear: an id and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "id", rename_all = "camelCase")]
pub enum ContentRef {
    /// A story in the Feed: an article, a newsletter issue, an episode.
    Story(StoryId),
    /// A file in the files folder: a recording, a film, the daily podcast,
    /// a document.
    File(FileId),
    /// A library item: an article or a book on a shelf, read through its
    /// first file or its link.
    Item(ItemId),
}

impl ContentRef {
    /// The store's key: `story:<id>`, `file:<id>`, `item:<id>`.
    pub fn key(&self) -> String {
        match self {
            ContentRef::Story(id) => format!("story:{id}"),
            ContentRef::File(id) => format!("file:{id}"),
            ContentRef::Item(id) => format!("item:{id}"),
        }
    }

    /// Read a key back.
    pub fn parse_key(key: &str) -> Option<Self> {
        let (kind, id) = key.split_once(':')?;
        match kind {
            "story" => StoryId::parse(id).ok().map(ContentRef::Story),
            "file" => FileId::parse(id).ok().map(ContentRef::File),
            "item" => ItemId::parse(id).ok().map(ContentRef::Item),
            _ => None,
        }
    }
}

/// How something is taken in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ConsumeMode {
    /// Read on the screen.
    #[default]
    Read,
    /// Heard: an episode played, or an article read aloud.
    Listen,
    /// A video watched.
    Watch,
}

/// Who put something in Up Next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum AddedBy {
    /// The person, from a button.
    #[default]
    Person,
    /// A show set to put every new episode in Up Next.
    Show,
    /// Up Next topped itself up from the recommendations
    /// ([`super::FeedSettings::autofill`]).
    Recommended,
    /// The daily podcast, when it was made.
    Daily,
    /// The assistant, asked to.
    Assistant,
}

/// One thing in Up Next.
///
/// Carries enough of what it is -- a title, where it is from, how long it
/// is -- to be drawn without opening the story, the file or the item behind
/// it, which may be gone by the time it is drawn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    pub id: QueueEntryId,
    pub what: ContentRef,
    pub title: String,
    /// The show, the feed, the shelf -- where it is from, in a few words.
    #[serde(default)]
    pub source: String,
    /// What playing it does: a podcast is heard, a video watched, an article
    /// read aloud or read on the screen.
    #[serde(default)]
    pub mode: ConsumeMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub added_by: AddedBy,
    pub added_at: Timestamp,
    /// Ascending. Kept in the clear for the `ORDER BY`; gaps are fine.
    pub sort_order: i64,
}

impl QueueEntry {
    pub fn new(what: ContentRef, title: impl Into<String>, sort_order: i64) -> Self {
        Self {
            id: QueueEntryId::new(),
            what,
            title: title.into(),
            source: String::new(),
            mode: ConsumeMode::default(),
            duration_ms: None,
            added_by: AddedBy::default(),
            added_at: Timestamp::now(),
            sort_order,
        }
    }
}

/// The gap left between two entries' [`QueueEntry::sort_order`], so a move
/// is one write and not a renumbering.
pub const QUEUE_GAP: i64 = 1024;

/// A stretch of a recording, in milliseconds from its start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl Span {
    pub fn len_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

/// `spans` with `add` folded in: sorted, and every two that overlap or touch
/// (within `slack_ms`) made one. Empty spans are dropped.
pub fn merge_spans(spans: &[Span], add: Span, slack_ms: u64) -> Vec<Span> {
    let mut all: Vec<Span> =
        spans.iter().copied().chain(std::iter::once(add)).filter(|s| s.len_ms() > 0).collect();
    all.sort_by_key(|s| (s.start_ms, s.end_ms));
    let mut merged: Vec<Span> = Vec::with_capacity(all.len());
    for span in all {
        match merged.last_mut() {
            Some(last) if span.start_ms <= last.end_ms.saturating_add(slack_ms) => {
                last.end_ms = last.end_ms.max(span.end_ms);
            }
            _ => merged.push(span),
        }
    }
    merged
}

/// How much of `duration_ms` the spans cover, from 0 to 1.
pub fn covered(spans: &[Span], duration_ms: u64) -> f32 {
    if duration_ms == 0 {
        return 0.0;
    }
    let heard: u64 =
        spans.iter().map(|s| s.end_ms.min(duration_ms).saturating_sub(s.start_ms)).sum();
    (heard as f64 / duration_ms as f64).clamp(0.0, 1.0) as f32
}

/// How much of a thing must have been taken in for it to count as finished.
pub const FINISHED_AT: f32 = 0.9;

/// Something somebody read, heard or watched, and how much of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Consumption {
    pub id: ConsumptionId,
    pub what: ContentRef,
    pub title: String,
    /// The show, the feed, the shelf.
    #[serde(default)]
    pub source: String,
    /// The article's or the episode's own address, for opening it again.
    #[serde(default)]
    pub url: String,
    pub mode: ConsumeMode,
    pub first_at: Timestamp,
    pub last_at: Timestamp,
    /// From 0 to 1: how much of it.
    #[serde(default)]
    pub progress: f32,
    /// Read, heard or watched to the end -- or said to be, from a button.
    #[serde(default)]
    pub finished: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
    /// Where a recording stopped, to pick up from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// The stretches of a recording heard, merged ([`merge_spans`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heard: Vec<Span>,
    /// Its words in the files folder: the article kept as Markdown, or the
    /// episode's transcript. What memory is searched through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_file: Option<FileId>,
    /// The recording itself, while it is kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_file: Option<FileId>,
}

impl Consumption {
    pub fn new(what: ContentRef, title: impl Into<String>, mode: ConsumeMode) -> Self {
        let now = Timestamp::now();
        Self {
            id: ConsumptionId::new(),
            what,
            title: title.into(),
            source: String::new(),
            url: String::new(),
            mode,
            first_at: now,
            last_at: now,
            progress: 0.0,
            finished: false,
            finished_at: None,
            position_ms: None,
            duration_ms: None,
            heard: Vec::new(),
            text_file: None,
            media_file: None,
        }
    }
}

impl Timestamped for Consumption {
    fn touch(&mut self) {
        self.last_at = Timestamp::now();
    }
}

/// A filter for [`crate::store::news::NewsStore::consumptions`]. All ANDed;
/// newest `last_at` first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", default)]
pub struct ConsumedQuery {
    /// Last touched at or after this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<Timestamp>,
    /// Last touched before this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<Timestamp>,
    /// Only what was finished.
    pub finished_only: bool,
    /// At most this many; `0` means 50, and no more than 500 are returned.
    pub limit: u32,
}

impl ConsumedQuery {
    pub fn limit(&self) -> u32 {
        match self.limit {
            0 => 50,
            n => n.min(500),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_reads_back_to_what_it_names() {
        let what = ContentRef::Story(StoryId::new());
        assert_eq!(ContentRef::parse_key(&what.key()), Some(what));
        assert_eq!(ContentRef::parse_key("story:nope"), None);
        assert_eq!(ContentRef::parse_key("thread:x"), None);
    }

    #[test]
    fn a_content_ref_is_tagged_by_type() {
        let id = FileId::new();
        let json = serde_json::to_value(ContentRef::File(id)).unwrap();
        assert_eq!(json, serde_json::json!({ "type": "file", "id": id }));
    }

    #[test]
    fn spans_merge_when_they_overlap_or_nearly_touch() {
        let spans = vec![Span { start_ms: 0, end_ms: 1000 }, Span { start_ms: 5000, end_ms: 6000 }];
        let merged = merge_spans(&spans, Span { start_ms: 1500, end_ms: 4800 }, 500);
        assert_eq!(merged, vec![Span { start_ms: 0, end_ms: 6000 }]);
        let apart = merge_spans(&spans, Span { start_ms: 2000, end_ms: 3000 }, 500);
        assert_eq!(apart.len(), 3);
        assert_eq!(merge_spans(&[], Span { start_ms: 7, end_ms: 7 }, 0), vec![]);
    }

    #[test]
    fn coverage_is_a_fraction_of_the_whole() {
        let spans = [Span { start_ms: 0, end_ms: 500 }, Span { start_ms: 900, end_ms: 2000 }];
        assert!((covered(&spans, 1000) - 0.6).abs() < 1e-6);
        assert_eq!(covered(&spans, 0), 0.0);
    }
}
