//! Storage for the library domain: shelves, the things on them, and the log.
//!
//! # Why this is a fourth trait
//!
//! The same reason [`TaskStore`](super::tasks::TaskStore) is a second one and
//! [`CalendarStore`](super::calendars::CalendarStore) a third. A backend that
//! stores journals as a tree of Markdown files has no opinion about a watch
//! list, and folding this into [`JournalStore`](super::JournalStore) would
//! oblige it to grow one. So the library is reached through
//! [`JournalStore::library`](super::JournalStore::library), which returns
//! `None` by default, and the interface reads
//! [`Capabilities::library`](super::Capabilities::library) to know whether to
//! offer the app at all.
//!
//! # Three records, and the cascade between them
//!
//! ```text
//!   Kind ────── Item ────── LogEntry
//! ```
//!
//! Deleting a kind takes its items, and deleting an item takes its log. Both
//! cascades are the implementation's job to honour, because both are
//! *required*: an item whose kind is gone has no fields, no verbs and no
//! shelf to be drawn on, and a log row pointing at nothing is a date with no
//! sentence attached. The conformance suite checks each one.
//!
//! Note what is deliberately *not* cascaded: a cover blob. Covers are
//! content-addressed and shared — the same picture can be the cover of a
//! book and of the film of the book — so they are reclaimed by
//! [`JournalStore::collect_garbage`](super::JournalStore::collect_garbage)
//! like every other blob, on the same grace period, rather than deleted
//! eagerly by whoever happened to drop the last reference.

use crate::error::Result;
use crate::id::{ItemId, KindId, LogId};
use crate::library::{Item, ItemStatus, Kind, LogEntry, LogEvent};
use jiff::civil::Date;
use serde::{Deserialize, Serialize};

/// How a shelf should be ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemSort {
    /// Most recently added first. What a shelf opens on, because the thing
    /// you just heard about is the thing you were just told about.
    #[default]
    AddedDesc,
    AddedAsc,
    UpdatedDesc,
    TitleAsc,
    /// Your rating, best first. Unrated items sort last rather than as zero:
    /// "I have not decided" is not the same as "it was terrible".
    RatingDesc,
    /// Most recently finished first. The "what did I read this year" order.
    FinishedDesc,
    /// Oldest first by publication year, for a shelf you are working through
    /// chronologically.
    YearAsc,
    YearDesc,
    /// Manual, by [`Item::sort_order`].
    Manual,
}

/// Filter + pagination for [`LibraryStore::list_items`].
///
/// All filters are ANDed. An empty query matches every item in the vault,
/// which is what the "Everything" row in the sidebar asks for.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ItemQuery {
    /// Restrict to one shelf. `None` means every kind.
    pub kind_id: Option<KindId>,
    /// Empty means any status.
    pub statuses: Vec<ItemStatus>,
    /// Item must carry *every* listed tag, case-insensitively.
    pub tags: Vec<String>,
    /// Case-insensitive substring of anything in
    /// [`Item::searchable_text`].
    pub text: String,
    /// Only what you starred.
    pub favourite: Option<bool>,
    /// Your rating, on the stored `0..=100` scale. Unrated items never match
    /// a lower bound — see [`ItemSort::RatingDesc`] for the same reasoning.
    pub rating_at_least: Option<u8>,
    /// Finished on or after / on or before. The year-in-review filter.
    pub finished_from: Option<Date>,
    pub finished_to: Option<Date>,
    pub sort: ItemSort,
    pub offset: u32,
    /// `None` means no limit.
    pub limit: Option<u32>,
}

impl ItemQuery {
    pub fn on_shelf(id: KindId) -> Self {
        Self { kind_id: Some(id), ..Default::default() }
    }

    /// Does this item pass the filters? Backends that cannot express a
    /// filter natively fall back to this, so behaviour stays identical
    /// across backends.
    pub fn matches(&self, item: &Item) -> bool {
        if let Some(kind) = self.kind_id
            && item.kind_id != kind
        {
            return false;
        }
        if !self.statuses.is_empty() && !self.statuses.contains(&item.status) {
            return false;
        }
        if let Some(favourite) = self.favourite
            && item.favourite != favourite
        {
            return false;
        }
        // An unrated item is not a zero-rated one, so it fails any lower
        // bound rather than sorting beneath the bad ones.
        if let Some(floor) = self.rating_at_least
            && item.rating.is_none_or(|r| r < floor)
        {
            return false;
        }
        if let Some(from) = self.finished_from
            && item.finished_on.is_none_or(|d| d < from)
        {
            return false;
        }
        if let Some(to) = self.finished_to
            && item.finished_on.is_none_or(|d| d > to)
        {
            return false;
        }
        // Tags are compared case-insensitively: "Sci-Fi" and "sci-fi" are
        // the same tag to a person, so they should be to the filter too.
        if !self
            .tags
            .iter()
            .all(|want| item.tags.iter().any(|have| have.eq_ignore_ascii_case(want)))
        {
            return false;
        }
        let needle = self.text.trim().to_lowercase();
        needle.is_empty() || item.searchable_text().to_lowercase().contains(&needle)
    }

    /// Filter, sort and paginate a materialised list. Shared by backends
    /// that cannot push the ordering down into their storage layer.
    pub fn apply(&self, mut rows: Vec<Item>) -> Vec<Item> {
        rows.retain(|i| self.matches(i));
        sort_items(&mut rows, self.sort);
        let start = (self.offset as usize).min(rows.len());
        rows.drain(..start);
        if let Some(limit) = self.limit {
            rows.truncate(limit as usize);
        }
        rows
    }
}

/// Order `rows` in place. Favourites float to the top, matching what a
/// pinned journal entry does and what a star anywhere else implies.
pub fn sort_items(rows: &mut [Item], sort: ItemSort) {
    rows.sort_by(|a, b| {
        b.favourite
            .cmp(&a.favourite)
            .then_with(|| match sort {
                ItemSort::AddedDesc => b.created_at.cmp(&a.created_at),
                ItemSort::AddedAsc => a.created_at.cmp(&b.created_at),
                ItemSort::UpdatedDesc => b.updated_at.cmp(&a.updated_at),
                ItemSort::TitleAsc => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
                // `None` last in every "best first" ordering. `Option`'s own
                // ordering puts it first, which would open a shelf sorted by
                // rating on everything you have not rated.
                ItemSort::RatingDesc => b.rating.unwrap_or(0).cmp(&a.rating.unwrap_or(0)),
                ItemSort::FinishedDesc => b
                    .finished_on
                    .is_some()
                    .cmp(&a.finished_on.is_some())
                    .then(b.finished_on.cmp(&a.finished_on)),
                ItemSort::YearAsc => {
                    a.year.is_none().cmp(&b.year.is_none()).then(a.year.cmp(&b.year))
                }
                ItemSort::YearDesc => {
                    a.year.is_none().cmp(&b.year.is_none()).then(b.year.cmp(&a.year))
                }
                ItemSort::Manual => a.sort_order.cmp(&b.sort_order),
            })
            // Every ordering above has ties, and a list that reshuffles under the
            // cursor because two things were added in the same millisecond is a
            // list nobody trusts. Titles break them, and the id breaks those.
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
            .then(a.id.cmp(&b.id))
    });
}

/// Filter for [`LibraryStore::list_logs`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LogQuery {
    /// One item's history. `None` means the whole vault, which is what the
    /// "what did I get through this year" view asks for.
    pub item_id: Option<ItemId>,
    /// Inclusive date bounds.
    pub from: Option<Date>,
    pub to: Option<Date>,
    /// Empty means any event.
    pub events: Vec<LogEvent>,
    pub limit: Option<u32>,
}

impl LogQuery {
    pub fn for_item(id: ItemId) -> Self {
        Self { item_id: Some(id), ..Default::default() }
    }

    /// Everything that counts as getting to the end of something, between
    /// two dates. The query behind the year in review.
    pub fn completions(from: Date, to: Date) -> Self {
        Self {
            from: Some(from),
            to: Some(to),
            events: vec![LogEvent::Finished, LogEvent::Revisited],
            ..Default::default()
        }
    }

    pub fn matches(&self, log: &LogEntry) -> bool {
        if let Some(item) = self.item_id
            && log.item_id != item
        {
            return false;
        }
        if let Some(from) = self.from
            && log.date < from
        {
            return false;
        }
        if let Some(to) = self.to
            && log.date > to
        {
            return false;
        }
        self.events.is_empty() || self.events.contains(&log.event)
    }

    /// Filter and order a materialised list: most recent first, which is how
    /// a history reads.
    pub fn apply(&self, mut rows: Vec<LogEntry>) -> Vec<LogEntry> {
        rows.retain(|l| self.matches(l));
        rows.sort_by(|a, b| b.date.cmp(&a.date).then(b.created_at.cmp(&a.created_at)));
        if let Some(limit) = self.limit {
            rows.truncate(limit as usize);
        }
        rows
    }
}

/// The persistence contract for the library domain.
pub trait LibraryStore: Send + Sync {
    // ---- kinds ----------------------------------------------------------

    /// Every shelf, hidden ones included. There are a handful of these, so
    /// filtering is the caller's business.
    fn list_kinds(&self) -> Result<Vec<Kind>>;

    fn get_kind(&self, id: KindId) -> Result<Kind>;

    /// Insert or replace. Implementations must be idempotent.
    fn put_kind(&self, kind: &Kind) -> Result<()>;

    /// Delete the shelf, everything on it, and every log row belonging to
    /// those items.
    fn delete_kind(&self, id: KindId) -> Result<()>;

    // ---- items ----------------------------------------------------------

    fn list_items(&self, query: &ItemQuery) -> Result<Vec<Item>>;

    fn get_item(&self, id: ItemId) -> Result<Item>;

    fn put_item(&self, item: &Item) -> Result<()>;

    /// One write for many items: what a re-ordered shelf is, and what a
    /// bulk status change is. Atomic where the backend can be atomic.
    fn put_items(&self, items: &[Item]) -> Result<()> {
        for item in items {
            self.put_item(item)?;
        }
        Ok(())
    }

    /// Delete the item and its whole log.
    fn delete_item(&self, id: ItemId) -> Result<()>;

    /// How many items are on one shelf, and how many are still ahead of you.
    ///
    /// For the line under a shelf's name in the sidebar. Counted by the
    /// backend over the whole vault rather than derived in the interface,
    /// which only ever holds the page it is showing.
    fn count_items(&self, kind: KindId) -> Result<(u64, u64)>;

    // ---- the log --------------------------------------------------------

    fn list_logs(&self, query: &LogQuery) -> Result<Vec<LogEntry>>;

    fn get_log(&self, id: LogId) -> Result<LogEntry>;

    fn put_log(&self, log: &LogEntry) -> Result<()>;

    fn delete_log(&self, id: LogId) -> Result<()>;
}

/// Associated data bound into a record's ciphertext. See
/// [`entry_aad`](super::entry_aad) for why records are bound to their id.
pub fn kind_aad(id: KindId) -> Vec<u8> {
    format!("everyday.kind.v1:{id}").into_bytes()
}

pub fn item_aad(id: ItemId) -> Vec<u8> {
    format!("everyday.item.v1:{id}").into_bytes()
}

pub fn log_aad(id: LogId) -> Vec<u8> {
    format!("everyday.log.v1:{id}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn item(kind: KindId, title: &str) -> Item {
        Item::new(kind, title)
    }

    #[test]
    fn an_empty_query_matches_everything() {
        assert!(ItemQuery::default().matches(&item(KindId::new(), "Dune")));
    }

    #[test]
    fn the_shelf_filter_narrows_to_one_kind() {
        let books = KindId::new();
        let films = KindId::new();
        let q = ItemQuery::on_shelf(books);
        assert!(q.matches(&item(books, "Dune")));
        assert!(!q.matches(&item(films, "Dune")));
    }

    #[test]
    fn statuses_are_a_set_rather_than_a_single_value() {
        // "Everything still ahead of me" is three statuses, not one.
        let kind = KindId::new();
        let q = ItemQuery {
            statuses: vec![ItemStatus::Wishlist, ItemStatus::Active, ItemStatus::Paused],
            ..Default::default()
        };
        for status in ItemStatus::ALL {
            let mut it = item(kind, "x");
            it.status = status;
            assert_eq!(q.matches(&it), status.is_open(), "{status:?}");
        }
    }

    #[test]
    fn an_unrated_item_is_not_a_zero_rated_one() {
        // The bug this guards: "show me everything I gave four stars or
        // more" quietly including everything you never rated.
        let kind = KindId::new();
        let q = ItemQuery { rating_at_least: Some(80), ..Default::default() };
        let unrated = item(kind, "unrated");
        assert!(!q.matches(&unrated));

        let mut good = item(kind, "good");
        good.rating = Some(90);
        assert!(q.matches(&good));

        let mut bad = item(kind, "bad");
        bad.rating = Some(20);
        assert!(!q.matches(&bad));
    }

    #[test]
    fn tag_and_text_filters_ignore_case() {
        let mut it = item(KindId::new(), "Dune");
        it.tags = vec!["Sci-Fi".into()];
        it.creator = "Frank Herbert".into();
        assert!(ItemQuery { tags: vec!["sci-fi".into()], ..Default::default() }.matches(&it));
        assert!(ItemQuery { text: "HERBERT".into(), ..Default::default() }.matches(&it));
        assert!(!ItemQuery { text: "asimov".into(), ..Default::default() }.matches(&it));
        // Every listed tag must be present, not merely one of them.
        let both = ItemQuery { tags: vec!["sci-fi".into(), "reread".into()], ..Default::default() };
        assert!(!both.matches(&it));
    }

    #[test]
    fn the_finished_window_excludes_what_was_never_finished() {
        let kind = KindId::new();
        let mut done = item(kind, "done");
        done.finished_on = Some(date(2026, 4, 2));
        let unfinished = item(kind, "unfinished");
        let q = ItemQuery {
            finished_from: Some(date(2026, 1, 1)),
            finished_to: Some(date(2026, 12, 31)),
            ..Default::default()
        };
        assert!(q.matches(&done));
        assert!(!q.matches(&unfinished), "a year in review must not include the unread");
    }

    #[test]
    fn unrated_items_sort_last_rather_than_first() {
        // `Option`'s own ordering puts `None` first, which would open a
        // shelf sorted by rating on everything you have not rated.
        let kind = KindId::new();
        let mut rows = vec![item(kind, "unrated"), item(kind, "great"), item(kind, "poor")];
        rows[1].rating = Some(95);
        rows[2].rating = Some(30);
        sort_items(&mut rows, ItemSort::RatingDesc);
        assert_eq!(
            rows.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(),
            ["great", "poor", "unrated"]
        );
    }

    #[test]
    fn unfinished_and_undated_items_sort_last_in_their_orderings() {
        let kind = KindId::new();
        let mut rows = vec![item(kind, "never"), item(kind, "recent"), item(kind, "old")];
        rows[1].finished_on = Some(date(2026, 4, 2));
        rows[2].finished_on = Some(date(2024, 1, 9));
        sort_items(&mut rows, ItemSort::FinishedDesc);
        assert_eq!(
            rows.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(),
            ["recent", "old", "never"]
        );

        let mut years = vec![item(kind, "unknown"), item(kind, "new"), item(kind, "ancient")];
        years[1].year = Some(2021);
        years[2].year = Some(1965);
        sort_items(&mut years, ItemSort::YearAsc);
        assert_eq!(
            years.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(),
            ["ancient", "new", "unknown"]
        );
    }

    #[test]
    fn favourites_float_to_the_top_of_every_ordering() {
        let kind = KindId::new();
        let mut rows = vec![item(kind, "a"), item(kind, "b"), item(kind, "c")];
        rows[2].favourite = true;
        sort_items(&mut rows, ItemSort::TitleAsc);
        assert_eq!(rows[0].title, "c");
    }

    #[test]
    fn ties_are_broken_so_a_shelf_does_not_reshuffle_under_the_cursor() {
        // Three items added in the same millisecond, which happens on an
        // import. Without the title tiebreak the order is whatever the sort
        // happened to do that run.
        let kind = KindId::new();
        let at = jiff::Timestamp::now();
        let mut rows: Vec<Item> = ["c", "a", "b"]
            .iter()
            .map(|t| {
                let mut i = item(kind, t);
                i.created_at = at;
                i
            })
            .collect();
        sort_items(&mut rows, ItemSort::AddedDesc);
        let once: Vec<String> = rows.iter().map(|i| i.title.clone()).collect();
        assert_eq!(once, ["a", "b", "c"]);
        for _ in 0..8 {
            sort_items(&mut rows, ItemSort::AddedDesc);
            assert_eq!(rows.iter().map(|i| i.title.clone()).collect::<Vec<_>>(), once);
        }
    }

    #[test]
    fn apply_filters_then_sorts_then_paginates() {
        let kind = KindId::new();
        let rows: Vec<Item> = ["e", "d", "c", "b", "a"].iter().map(|t| item(kind, t)).collect();
        let q =
            ItemQuery { sort: ItemSort::TitleAsc, offset: 1, limit: Some(2), ..Default::default() };
        let out = q.apply(rows);
        assert_eq!(out.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), ["b", "c"]);
    }

    #[test]
    fn pagination_past_the_end_yields_nothing_rather_than_panicking() {
        let rows = vec![item(KindId::new(), "a")];
        assert!(ItemQuery { offset: 99, ..Default::default() }.apply(rows).is_empty());
    }

    #[test]
    fn the_completions_query_is_both_ways_of_getting_to_the_end() {
        let item_id = ItemId::new();
        let q = LogQuery::completions(date(2026, 1, 1), date(2026, 12, 31));
        for event in LogEvent::ALL {
            let log = LogEntry::new(item_id, event, date(2026, 3, 4), "UTC");
            assert_eq!(q.matches(&log), event.is_completion(), "{event:?}");
        }
        // ...and it is a window, not the whole log.
        let old = LogEntry::new(item_id, LogEvent::Finished, date(2025, 12, 31), "UTC");
        assert!(!q.matches(&old));
    }

    #[test]
    fn a_history_reads_newest_first() {
        let id = ItemId::new();
        let rows = vec![
            LogEntry::new(id, LogEvent::Started, date(2026, 3, 3), "UTC"),
            LogEntry::new(id, LogEvent::Finished, date(2026, 4, 2), "UTC"),
            LogEntry::new(id, LogEvent::Progress, date(2026, 3, 20), "UTC"),
        ];
        let out = LogQuery::for_item(id).apply(rows);
        assert_eq!(out[0].date, date(2026, 4, 2));
        assert_eq!(out[2].date, date(2026, 3, 3));
    }

    #[test]
    fn aad_is_distinct_per_record_and_per_kind() {
        let same = uuid::Uuid::now_v7();
        assert_ne!(kind_aad(KindId(same)), item_aad(ItemId(same)));
        assert_ne!(item_aad(ItemId(same)), log_aad(LogId(same)));
        // And distinct from the other three domains, which share the id space.
        assert_ne!(item_aad(ItemId(same)), super::super::entry_aad(crate::EntryId(same)));
        assert_ne!(log_aad(LogId(same)), super::super::tasks::block_aad(crate::BlockId(same)));
        assert_ne!(
            kind_aad(KindId(same)),
            super::super::calendars::calendar_aad(crate::CalendarId(same))
        );
    }
}
