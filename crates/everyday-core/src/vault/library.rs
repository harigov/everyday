//! Shelves, the things on them, and the log of what you did with them.
//!
//! The fourth domain, on exactly the terms of the second and third:
//! everything goes through [`Vault::with_library`], which fails with
//! `unsupported` on a backend that holds journals only.
//!
//! Note the same division of labour the calendar makes. This layer takes
//! a `SearchResult` -- something already fetched and already parsed --
//! never a query to run: the core has no socket, and
//! `crate::websearch` is built so that stays true. What the core owns is
//! the part worth testing, which is what a result does to an item once it
//! arrives.

use super::Vault;
use super::session::Domain;
use crate::error::{Error, Result};
use crate::id::{ItemId, KindId, LogId};
use crate::library::{Item, ItemStatus, Kind, KindCount, LibraryStats, LogEntry, default_kinds};
use crate::store::library::{ItemQuery, LibraryStore, LogQuery};

impl Vault {
    /// Does this vault's backend store a library?
    pub fn supports_library(&self) -> bool {
        self.with_library(|_| Ok(())).is_ok()
    }

    fn with_library<T>(&self, f: impl FnOnce(&dyn LibraryStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Library, |s| s.library().map(f))
    }

    pub fn kinds(&self) -> Result<Vec<Kind>> {
        self.with_library(|l| {
            let mut kinds = l.list_kinds()?;
            kinds.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)));
            Ok(kinds)
        })
    }

    pub fn kind(&self, id: KindId) -> Result<Kind> {
        self.with_library(|l| l.get_kind(id))
    }

    pub fn save_kind(&self, kind: &Kind) -> Result<()> {
        self.writable()?;
        if kind.name.trim().is_empty() {
            return Err(Error::Invalid("a shelf needs a name".into()));
        }
        if kind.slug.trim().is_empty() {
            return Err(Error::Invalid("a shelf needs a short name to look things up by".into()));
        }
        self.with_library(|l| l.put_kind(kind))
    }

    /// Delete the shelf, everything on it and every log row those items had.
    pub fn delete_kind(&self, id: KindId) -> Result<()> {
        self.writable()?;
        self.with_library(|l| l.delete_kind(id))
    }

    /// Put the built-in shelves in an empty library, and do nothing at all
    /// otherwise. Returns how many were added.
    ///
    /// Called on unlock rather than at creation, which is what makes it work
    /// for the vault somebody already had before this app existed: the
    /// library appears on their next launch with shelves in it rather than
    /// as an empty screen with a "make a category" button.
    ///
    /// The emptiness test is deliberately "no kinds at all", not "no kind
    /// with this slug". Somebody who deletes the Films shelf has said
    /// something, and an application that puts it back every time it starts
    /// is an application that is arguing.
    pub fn seed_library(&self) -> Result<usize> {
        if !self.supports_library() || !self.is_writable() {
            return Ok(0);
        }
        self.with_library(|l| {
            if !l.list_kinds()?.is_empty() {
                return Ok(0);
            }
            let seeds = default_kinds();
            for kind in &seeds {
                l.put_kind(kind)?;
            }
            Ok(seeds.len())
        })
    }

    pub fn items(&self, query: &ItemQuery) -> Result<Vec<Item>> {
        self.with_library(|l| l.list_items(query))
    }

    pub fn item(&self, id: ItemId) -> Result<Item> {
        self.with_library(|l| l.get_item(id))
    }

    pub fn save_item(&self, item: &Item) -> Result<()> {
        self.writable()?;
        if item.title.trim().is_empty() {
            return Err(Error::Invalid("an item needs a title".into()));
        }
        // A rating is stored out of a hundred. Anything above that is a
        // caller that has not read `library::from_stars`, and silently
        // clamping it would hide the bug rather than the number.
        if item.rating.is_some_and(|r| r > 100) {
            return Err(Error::Invalid("a rating runs from 0 to 100".into()));
        }
        // An item on a shelf that does not exist has no fields, no verbs and
        // nowhere to be drawn. Checked here rather than by a foreign key so
        // that every backend enforces it, including the ones that have no
        // such thing.
        self.with_library(|l| {
            l.get_kind(item.kind_id)?;
            l.put_item(item)
        })
    }

    /// One write for many items: a re-ordered shelf, or a bulk status change.
    pub fn save_items(&self, items: &[Item]) -> Result<()> {
        self.writable()?;
        if let Some(bad) = items.iter().find(|i| i.title.trim().is_empty()) {
            return Err(Error::Invalid(format!("item {} has no title", bad.id)));
        }
        self.with_library(|l| l.put_items(items))
    }

    /// Delete the item and its whole log.
    pub fn delete_item(&self, id: ItemId) -> Result<()> {
        self.writable()?;
        self.with_library(|l| l.delete_item(id))
    }

    pub fn logs(&self, query: &LogQuery) -> Result<Vec<LogEntry>> {
        self.with_library(|l| l.list_logs(query))
    }

    pub fn log(&self, id: LogId) -> Result<LogEntry> {
        self.with_library(|l| l.get_log(id))
    }

    pub fn save_log(&self, log: &LogEntry) -> Result<()> {
        self.writable()?;
        if log.rating.is_some_and(|r| r > 100) {
            return Err(Error::Invalid("a rating runs from 0 to 100".into()));
        }
        // The same argument as `save_item`: a log row pointing at nothing is
        // a date with no sentence attached.
        self.with_library(|l| {
            l.get_item(log.item_id)?;
            l.put_log(log)
        })
    }

    pub fn delete_log(&self, id: LogId) -> Result<()> {
        self.writable()?;
        self.with_library(|l| l.delete_log(id))
    }

    /// Counts for the library sidebar, as of the calendar year `year`.
    ///
    /// One pass over the items and one over the year's completions, done in
    /// the core rather than in the interface for the reason `task_stats` is:
    /// a sidebar counting the rows it happens to be showing says "3" for a
    /// shelf of four hundred.
    pub fn library_stats(&self, year: i16) -> Result<LibraryStats> {
        self.with_library(|l| {
            let kinds = l.list_kinds()?;
            let items = l.list_items(&ItemQuery::default())?;

            let mut stats = LibraryStats {
                kinds: kinds.len() as u64,
                items: items.len() as u64,
                ..Default::default()
            };
            let mut rated_total: u64 = 0;
            let mut by_kind: std::collections::BTreeMap<KindId, KindCount> = kinds
                .iter()
                .map(|k| (k.id, KindCount { kind_id: k.id, ..Default::default() }))
                .collect();

            for item in &items {
                match item.status {
                    ItemStatus::Wishlist => stats.wishlist += 1,
                    ItemStatus::Active => stats.active += 1,
                    ItemStatus::Done => stats.done += 1,
                    _ => {}
                }
                if let Some(rating) = item.rating {
                    stats.rated += 1;
                    rated_total += u64::from(rating);
                }
                // An item whose shelf has been deleted under it is counted in
                // the totals and in no shelf, which is honest: it is still in
                // the vault, and the sidebar has nowhere to put it.
                if let Some(count) = by_kind.get_mut(&item.kind_id) {
                    count.items += 1;
                    if item.status.is_open() {
                        count.open += 1;
                    }
                    if item.status == ItemStatus::Active {
                        count.active += 1;
                    }
                }
            }

            stats.mean_rating =
                (stats.rated > 0).then(|| (rated_total / stats.rated).min(100) as u8);

            let (from, to) = (jiff::civil::date(year, 1, 1), jiff::civil::date(year, 12, 31));
            stats.finished_this_year = l.list_logs(&LogQuery::completions(from, to))?.len() as u64;

            // Sidebar order, so the interface can render the counts against
            // the shelves without a join.
            let mut counts: Vec<KindCount> = by_kind.into_values().collect();
            let order: std::collections::BTreeMap<KindId, (i32, String)> =
                kinds.iter().map(|k| (k.id, (k.sort_order, k.name.clone()))).collect();
            counts.sort_by(|a, b| order.get(&a.kind_id).cmp(&order.get(&b.kind_id)));
            stats.by_kind = counts;
            Ok(stats)
        })
    }
}
