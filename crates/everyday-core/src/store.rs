//! The storage abstraction.
//!
//! A [`JournalStore`] is the only thing the application knows about
//! persistence. SQLite, a tree of Markdown files, an in-memory store for
//! tests — and later a sync server — all sit behind this one trait, so the
//! rest of the app never learns which is in use.
//!
//! # Why the trait is synchronous
//!
//! Every backend we ship talks to local disk, where the work is measured in
//! microseconds and the kernel already buffers. Making the trait `async`
//! would force every implementation to either invent an executor or wrap
//! blocking calls anyway, for no latency win. The application layer keeps
//! the UI responsive by dispatching store calls onto a blocking thread pool
//! (`tauri::async_runtime::spawn_blocking`). A future network backend can
//! drive its own runtime internally behind the same synchronous surface.
//!
//! # Encryption is the store's job, not the caller's
//!
//! Each store is handed a [`Cipher`] at open time and is responsible for
//! sealing record payloads and blob bytes with it. This keeps encryption
//! uniform without dictating *layout*: the SQLite backend seals a BLOB
//! column, the Markdown backend seals a file body, and both are equally
//! correct.

use crate::crypto::Cipher;
use crate::error::{Error, Result};
use crate::id::{BlobId, EntryId, JournalId};
use crate::model::{Entry, EntrySummary, Journal};
use crate::store::calendars::CalendarStore;
use crate::store::tasks::TaskStore;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// What a backend can and cannot do. The UI reads this to hide features a
/// backend does not support rather than surfacing errors at click time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// Backend can store attachment payloads.
    pub blobs: bool,
    /// `put_entry` / `delete_entry` are atomic and durable.
    pub transactional: bool,
    /// Files are meaningful to other tools (grep, git, another editor).
    /// Never true at the same time as encryption being enabled.
    pub human_readable: bool,
    /// Largest attachment the backend will accept, if it has a limit.
    pub max_blob_bytes: Option<u64>,
    /// Backend implements [`tasks::TaskStore`], so the interface can offer
    /// the todo app. False means it holds journals and nothing else.
    #[serde(default)]
    pub tasks: bool,
    /// Backend implements [`calendars::CalendarStore`], so subscribed
    /// calendars can be held. Independent of `tasks` in the type, though in
    /// practice a backend that carries one carries both: the calendar app
    /// draws time blocks, which live in the task domain, so the interface
    /// requires the pair before it offers the app.
    #[serde(default)]
    pub calendars: bool,
}

/// Everything a backend needs to open a vault directory.
#[derive(Clone)]
pub struct StoreContext {
    /// Directory the backend owns. It may create anything it likes inside.
    pub root: PathBuf,
    /// Cipher for record payloads and blobs. Cheap to clone (`Arc`).
    pub cipher: Arc<dyn Cipher>,
}

impl std::fmt::Debug for StoreContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreContext")
            .field("root", &self.root)
            .field("cipher", &self.cipher.id())
            .finish()
    }
}

/// How an entry list should be ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortOrder {
    /// Newest journal date first. The Day One default.
    #[default]
    DateDesc,
    DateAsc,
    /// Most recently edited first.
    UpdatedDesc,
    CreatedDesc,
    TitleAsc,
}

/// Filter + pagination for [`JournalStore::list_entries`].
///
/// All filters are ANDed. An empty query matches every entry in the vault.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EntryQuery {
    /// Restrict to one journal. `None` means "all journals".
    pub journal_id: Option<JournalId>,
    /// Inclusive lower bound on `local_date`.
    pub from: Option<Date>,
    /// Inclusive upper bound on `local_date`.
    pub to: Option<Date>,
    /// Entry must carry *every* listed tag.
    pub tags: Vec<String>,
    /// Restrict to starred entries.
    pub starred: Option<bool>,
    pub sort: SortOrder,
    pub offset: u32,
    /// `None` means no limit.
    pub limit: Option<u32>,
}

impl EntryQuery {
    pub fn in_journal(id: JournalId) -> Self {
        Self { journal_id: Some(id), ..Default::default() }
    }

    /// Does this entry pass the filters? Backends that cannot express a
    /// filter natively fall back to this, so behaviour stays identical
    /// across backends.
    pub fn matches(&self, e: &EntrySummary) -> bool {
        if let Some(j) = self.journal_id
            && e.journal_id != j
        {
            return false;
        }
        if let Some(from) = self.from
            && e.local_date < from
        {
            return false;
        }
        if let Some(to) = self.to
            && e.local_date > to
        {
            return false;
        }
        if let Some(starred) = self.starred
            && e.starred != starred
        {
            return false;
        }
        // Tag comparison is case-insensitive: "Travel" and "travel" are the
        // same tag to a person, so they should be to the filter too.
        self.tags.iter().all(|want| e.tags.iter().any(|have| have.eq_ignore_ascii_case(want)))
    }

    /// Sort + paginate a fully-materialised list. Shared by backends that
    /// cannot push the ordering down into their storage layer.
    pub fn apply(&self, mut rows: Vec<EntrySummary>) -> Vec<EntrySummary> {
        rows.retain(|e| self.matches(e));
        sort_summaries(&mut rows, self.sort);
        let start = (self.offset as usize).min(rows.len());
        rows.drain(..start);
        if let Some(limit) = self.limit {
            rows.truncate(limit as usize);
        }
        rows
    }
}

/// Order `rows` in place. Pinned entries always float to the top, matching
/// what Day One does and what people expect from a "pin" affordance.
pub fn sort_summaries(rows: &mut [EntrySummary], sort: SortOrder) {
    rows.sort_by(|a, b| {
        b.pinned.cmp(&a.pinned).then_with(|| match sort {
            // Ties on the journal date are broken by creation time so that
            // several entries written on one day keep a stable order.
            SortOrder::DateDesc => {
                b.local_date.cmp(&a.local_date).then(b.created_at.cmp(&a.created_at))
            }
            SortOrder::DateAsc => {
                a.local_date.cmp(&b.local_date).then(a.created_at.cmp(&b.created_at))
            }
            SortOrder::UpdatedDesc => b.updated_at.cmp(&a.updated_at),
            SortOrder::CreatedDesc => b.created_at.cmp(&a.created_at),
            SortOrder::TitleAsc => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
        })
    });
}

/// Counts shown in the vault info panel.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreStats {
    pub journals: u64,
    pub entries: u64,
    pub blobs: u64,
    /// Sum of on-disk (i.e. sealed) attachment sizes.
    pub blob_bytes: u64,
}

/// The persistence contract. See the module docs for the design rationale.
pub trait JournalStore: Send + Sync {
    /// Stable identifier, e.g. `"sqlite"`. Written into the vault header so
    /// the right backend is chosen when the vault is reopened.
    fn backend(&self) -> &'static str;

    fn capabilities(&self) -> Capabilities;

    /// Storage for the task domain, if this backend has any.
    ///
    /// Returning `None` -- the default -- is a backend saying "journals are
    /// all I do", which the Markdown backend means literally: a kanban board
    /// is not a thing anyone wants as a tree of files. See
    /// [`tasks`](crate::store::tasks) for why this is an accessor rather
    /// than more methods on this trait.
    fn tasks(&self) -> Option<&dyn TaskStore> {
        None
    }

    /// Storage for the calendar domain, if this backend has any.
    ///
    /// Same shape and same reasoning as [`JournalStore::tasks`]: a backend
    /// says what it does rather than failing when asked to do it. See
    /// [`calendars`](crate::store::calendars) for why a cache of other
    /// people's meetings is its own trait rather than more methods here.
    fn calendars(&self) -> Option<&dyn CalendarStore> {
        None
    }

    // ---- journals -------------------------------------------------------

    fn list_journals(&self) -> Result<Vec<Journal>>;

    fn get_journal(&self, id: JournalId) -> Result<Journal>;

    /// Insert or replace. Implementations must be idempotent.
    fn put_journal(&self, journal: &Journal) -> Result<()>;

    /// Delete the journal *and every entry in it*. Blobs referenced only by
    /// those entries become unreferenced and are reclaimed by
    /// [`JournalStore::collect_garbage`].
    fn delete_journal(&self, id: JournalId) -> Result<()>;

    // ---- entries --------------------------------------------------------

    fn list_entries(&self, query: &EntryQuery) -> Result<Vec<EntrySummary>>;

    fn get_entry(&self, id: EntryId) -> Result<Entry>;

    fn put_entry(&self, entry: &Entry) -> Result<()>;

    fn delete_entry(&self, id: EntryId) -> Result<()>;

    /// Stream every entry in the vault. Used to build the search index on
    /// unlock and to drive export. Kept separate from `list_entries` because
    /// it returns full bodies, which list views must never pay for.
    fn all_entries(&self) -> Result<Vec<Entry>>;

    // ---- blobs ----------------------------------------------------------

    /// Store `bytes` and return its content address. Storing the same bytes
    /// twice is a no-op that returns the same id.
    fn put_blob(&self, bytes: &[u8]) -> Result<BlobId>;

    fn get_blob(&self, id: BlobId) -> Result<Vec<u8>>;

    /// Plaintext length of a blob, without decrypting it.
    fn blob_len(&self, id: BlobId) -> Result<u64> {
        Ok(self.get_blob(id)?.len() as u64)
    }

    /// Read a window of a blob. Ranges past the end are clamped.
    ///
    /// This is what makes `<video>` scrubbing work: the media protocol
    /// handler turns an HTTP range request into one of these, and a
    /// chunk-encrypted backend decrypts only the chunks it touches instead
    /// of the whole file. The default implementation is correct but reads
    /// everything, so seekable backends should override it.
    fn get_blob_range(&self, id: BlobId, offset: u64, len: u64) -> Result<Vec<u8>> {
        let all = self.get_blob(id)?;
        let start = (offset as usize).min(all.len());
        let end = start.saturating_add(len as usize).min(all.len());
        Ok(all[start..end].to_vec())
    }

    fn has_blob(&self, id: BlobId) -> Result<bool>;

    fn delete_blob(&self, id: BlobId) -> Result<()>;

    fn list_blobs(&self) -> Result<Vec<BlobId>>;

    // ---- housekeeping ---------------------------------------------------

    fn stats(&self) -> Result<StoreStats>;

    /// Force pending writes to durable storage.
    fn flush(&self) -> Result<()> {
        Ok(())
    }

    /// Delete blobs that no entry references any more, returning how many
    /// were removed. Default implementation works for every backend; SQLite
    /// overrides it with a single SQL pass.
    fn collect_garbage(&self) -> Result<u64> {
        let mut live = std::collections::BTreeSet::new();
        for entry in self.all_entries()? {
            live.extend(entry.body.blob_refs());
            live.extend(entry.attachments.iter().map(|a| a.blob));
        }
        let mut removed = 0;
        for id in self.list_blobs()? {
            if !live.contains(&id) {
                self.delete_blob(id)?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

/// Associated data bound into an entry's ciphertext.
///
/// Binding the id means a sealed payload only ever decrypts in the row it
/// was written to; copying entry A's bytes over entry B yields a decryption
/// failure instead of silently swapped content.
pub fn entry_aad(id: EntryId) -> Vec<u8> {
    format!("everyday.entry.v1:{id}").into_bytes()
}

pub fn journal_aad(id: JournalId) -> Vec<u8> {
    format!("everyday.journal.v1:{id}").into_bytes()
}

/// Blobs are content-addressed, so the address is the natural binding.
pub fn blob_aad(id: BlobId) -> Vec<u8> {
    format!("everyday.blob.v1:{id}").into_bytes()
}

/// A factory that turns a [`StoreContext`] into a live backend.
///
/// This is the extension point: to try a new storage strategy, implement
/// [`JournalStore`] plus one of these and register it.
pub trait StoreFactory: Send + Sync {
    fn id(&self) -> &'static str;
    /// Short human description, shown in the vault-creation UI.
    fn describe(&self) -> &'static str;
    fn open(&self, ctx: StoreContext) -> Result<Box<dyn JournalStore>>;
}

/// Registry of available backends, populated at startup.
#[derive(Default)]
pub struct BackendRegistry {
    factories: Vec<Arc<dyn StoreFactory>>,
}

impl BackendRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, f: impl StoreFactory + 'static) -> &mut Self {
        self.factories.push(Arc::new(f));
        self
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.factories.iter().map(|f| f.id()).collect()
    }

    /// `(id, description)` pairs for the backend picker.
    pub fn describe_all(&self) -> Vec<(&'static str, &'static str)> {
        self.factories.iter().map(|f| (f.id(), f.describe())).collect()
    }

    pub fn open(&self, id: &str, ctx: StoreContext) -> Result<Box<dyn JournalStore>> {
        self.factories
            .iter()
            .find(|f| f.id() == id)
            .ok_or_else(|| Error::UnknownBackend(id.to_string()))?
            .open(ctx)
    }
}

pub mod calendars;
pub mod tasks;

#[cfg(any(test, feature = "testing"))]
pub mod conformance;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Entry, EntrySummary};
    use jiff::civil::date;

    fn summary(day: i8, title: &str) -> EntrySummary {
        let jid = JournalId::new();
        let mut e = Entry::new(jid, "UTC");
        e.title = title.into();
        e.local_date = date(2025, 3, day);
        e.summarize()
    }

    #[test]
    fn empty_query_matches_everything() {
        let q = EntryQuery::default();
        assert!(q.matches(&summary(1, "a")));
    }

    #[test]
    fn date_bounds_are_inclusive() {
        let q = EntryQuery {
            from: Some(date(2025, 3, 10)),
            to: Some(date(2025, 3, 20)),
            ..Default::default()
        };
        assert!(!q.matches(&summary(9, "before")));
        assert!(q.matches(&summary(10, "lower edge")));
        assert!(q.matches(&summary(20, "upper edge")));
        assert!(!q.matches(&summary(21, "after")));
    }

    #[test]
    fn tag_filter_requires_all_tags_and_ignores_case() {
        let mut s = summary(1, "x");
        s.tags = vec!["Travel".into(), "spain".into()];

        let q = |tags: &[&str]| EntryQuery {
            tags: tags.iter().map(|t| t.to_string()).collect(),
            ..Default::default()
        };
        assert!(q(&["travel"]).matches(&s));
        assert!(q(&["travel", "SPAIN"]).matches(&s));
        assert!(!q(&["travel", "portugal"]).matches(&s));
    }

    #[test]
    fn sorting_floats_pinned_entries_to_the_top() {
        let mut rows = vec![summary(3, "c"), summary(1, "a"), summary(2, "b")];
        rows[1].pinned = true; // the oldest entry is pinned
        sort_summaries(&mut rows, SortOrder::DateDesc);
        assert_eq!(rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(), ["a", "c", "b"]);
    }

    #[test]
    fn apply_filters_then_sorts_then_paginates() {
        let rows: Vec<_> = (1..=5).map(|d| summary(d, &format!("e{d}"))).collect();
        let q = EntryQuery {
            sort: SortOrder::DateAsc,
            offset: 1,
            limit: Some(2),
            ..Default::default()
        };
        let out = q.apply(rows);
        assert_eq!(out.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(), ["e2", "e3"]);
    }

    #[test]
    fn pagination_past_the_end_yields_nothing_rather_than_panicking() {
        let rows = vec![summary(1, "a")];
        let q = EntryQuery { offset: 99, ..Default::default() };
        assert!(q.apply(rows).is_empty());
    }

    #[test]
    fn aad_is_distinct_per_record_and_per_kind() {
        let a = EntryId::new();
        let b = EntryId::new();
        assert_ne!(entry_aad(a), entry_aad(b));
        // A journal and an entry that somehow shared a UUID must still not
        // share associated data.
        let same = uuid::Uuid::now_v7();
        assert_ne!(entry_aad(EntryId(same)), journal_aad(JournalId(same)));
    }

    #[test]
    fn registry_reports_unknown_backends_by_name() {
        let reg = BackendRegistry::new();
        let ctx = StoreContext {
            root: PathBuf::from("/tmp/nope"),
            cipher: Arc::new(crate::crypto::NullCipher),
        };
        let Err(err) = reg.open("redis", ctx) else { panic!("expected an error") };
        assert!(matches!(err, Error::UnknownBackend(b) if b == "redis"));
    }
}
