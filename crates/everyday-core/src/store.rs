//! The storage abstraction.
//!
//! A [`JournalStore`] is the only thing the application knows about
//! persistence. SQLite, Postgres, an in-memory store for tests — and later a
//! sync server — all sit behind this one trait, so the rest of the app never
//! learns which is in use.
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
//! uniform without dictating *layout*: a local backend seals a `BLOB` column
//! and a chunk of a file on disk, a server backend seals the same payload
//! before it ever reaches the wire, and both are equally correct. It also
//! means a remote database never sees plaintext — the sealing happens on
//! this side of the connection.

use crate::crypto::Cipher;
use crate::error::{Error, Result};
use crate::id::{BlobId, EntryId, JournalId};
use crate::model::{Entry, EntrySummary, Journal};
use crate::store::agent::AgentStore;
use crate::store::calendars::CalendarStore;
use crate::store::library::LibraryStore;
use crate::store::notes::NoteStore;
use crate::store::purpose::PurposeStore;
use crate::store::tasks::TaskStore;
use crate::store::trackers::TrackerStore;
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
    /// Backend implements [`library::LibraryStore`], so shelves, the things
    /// on them and the log of what you did with them can be held.
    ///
    /// Independent of the other two in the type and in practice: nothing in
    /// the library app reads a task or an event, so it is offered on any
    /// backend that carries this alone.
    #[serde(default)]
    pub library: bool,
    /// Backend implements [`trackers::TrackerStore`], so the habits, doses,
    /// symptoms and counts recorded beside an entry have somewhere to live.
    /// False hides tracking entirely — including the settings that define
    /// it, since offering to configure what cannot be recorded is worse than
    /// not offering it.
    #[serde(default)]
    pub trackers: bool,
    /// Backend implements [`purpose::PurposeStore`], so the roles you play
    /// and the goals under them have somewhere to live — and so the reports
    /// that answer "where did my week go, by role" can be asked at all.
    ///
    /// False hides the Overview app entirely, including the pickers that set
    /// a purpose on a task or a block: offering to file something under a
    /// goal that cannot be stored is worse than not offering it.
    #[serde(default)]
    pub goals: bool,
    /// Backend implements [`notes::NoteStore`], so writing that is not
    /// filed under a day has somewhere to live.
    ///
    /// False hides the Notes app, and takes the assistant's note tools with
    /// it -- which matters more than it sounds, because a note is where a
    /// routine puts prose it has more than a paragraph of.
    #[serde(default)]
    pub notes: bool,
    /// Backend implements [`agent::AgentStore`], so the assistant has
    /// somewhere to keep its settings, its threads and its memory.
    ///
    /// False hides the assistant entirely rather than offering a panel whose
    /// conversation vanishes when the window closes. Independent of the
    /// others in the type, but of limited use without them: an assistant
    /// whose vault holds journals and nothing else can still read and write
    /// entries, and its task tools will say the backend does not do tasks.
    #[serde(default)]
    pub agent: bool,
}

/// Per-vault configuration a backend needs and the core knows nothing about.
///
/// A file-backed store needs only the directory it is handed. A store that
/// talks to a server needs to be told *which* server, and there is no way for
/// [`everyday_core`](crate) to have an opinion about the shape of that: a
/// connection URL, a project reference, a schema name. So it carries opaque
/// string pairs, each backend documents the keys it reads, and the vault
/// persists whatever it was given.
///
/// # These are secrets
///
/// A Postgres URL usually has a password in it, which makes this the only
/// part of a vault's *configuration* that is as sensitive as its contents.
/// The vault therefore seals it under the data key rather than writing it
/// into the plaintext header beside the salt — see
/// [`VaultHeader::backend_settings`](crate::vault::VaultHeader). On an
/// unencrypted vault that sealing is a no-op, which is one more thing "no
/// password" costs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BackendSettings(std::collections::BTreeMap<String, String>);

impl BackendSettings {
    /// The one key with a meaning across backends: where to connect.
    pub const URL: &'static str = "url";

    pub fn new() -> Self {
        Self::default()
    }

    /// The common case, spelled once.
    pub fn with_url(url: impl Into<String>) -> Self {
        let mut s = Self::new();
        s.set(Self::URL, url);
        s
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str).filter(|v| !v.is_empty())
    }

    pub fn url(&self) -> Option<&str> {
        self.get(Self::URL)
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.0.insert(key.into(), value.into());
        self
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// Every pair, for merging one set of settings into another.
    pub fn into_pairs(self) -> impl Iterator<Item = (String, String)> {
        self.0.into_iter()
    }

    /// The value a backend cannot open without, or a message naming it.
    ///
    /// Backends call this instead of unwrapping, so a vault configured
    /// without its connection URL says which key is missing rather than
    /// failing somewhere inside a driver.
    pub fn require(&self, key: &str, backend: &str) -> Result<&str> {
        self.get(key).ok_or_else(|| {
            Error::Invalid(format!(
                "the {backend} backend needs a {key:?} setting, and none is set"
            ))
        })
    }
}

/// One field a backend needs before it can be opened, for the setup screen.
///
/// The interface renders these rather than knowing that Postgres exists: a
/// backend that later needs two fields, or none, changes here and nowhere in
/// the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingSpec {
    pub key: &'static str,
    pub label: &'static str,
    /// Shown greyed in the empty field. Never a real credential.
    pub placeholder: &'static str,
    /// The backend refuses to open without it.
    pub required: bool,
    /// Masked on entry and never sent back to the interface afterwards.
    pub secret: bool,
}

/// A backend as the vault-creation screen sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendInfo {
    pub id: &'static str,
    /// Short human name, e.g. "Database".
    pub name: &'static str,
    pub description: &'static str,
    /// Empty for a backend that needs nothing but a directory.
    pub settings: Vec<SettingSpec>,
}

/// Everything a backend needs to open a vault directory.
#[derive(Clone)]
pub struct StoreContext {
    /// Directory the backend owns. It may create anything it likes inside.
    ///
    /// Still handed to a backend that keeps its records on a server: a vault
    /// is a local directory whatever is underneath it, and a remote store may
    /// well want somewhere to put a cache.
    pub root: PathBuf,
    /// Cipher for record payloads and blobs. Cheap to clone (`Arc`).
    pub cipher: Arc<dyn Cipher>,
    /// Whatever this backend was configured with. See [`BackendSettings`].
    pub settings: BackendSettings,
}

impl StoreContext {
    /// A context over `root` with no backend settings — what every
    /// directory-backed store wants, and what the tests use.
    pub fn new(root: impl Into<PathBuf>, cipher: Arc<dyn Cipher>) -> Self {
        Self { root: root.into(), cipher, settings: BackendSettings::default() }
    }

    pub fn with_settings(mut self, settings: BackendSettings) -> Self {
        self.settings = settings;
        self
    }
}

impl std::fmt::Debug for StoreContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreContext")
            .field("root", &self.root)
            .field("cipher", &self.cipher.id())
            // Keys only. A connection URL carries a password, and this type
            // is `Debug`-printed into logs.
            .field("settings", &self.settings.keys().collect::<Vec<_>>())
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

/// How old an unreferenced blob must be before
/// [`JournalStore::collect_garbage`] will delete it.
///
/// A day. Long enough to cover a draft left open overnight, short enough
/// that a failed import does not squat on disk indefinitely.
pub const GC_GRACE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// The persistence contract. See the module docs for the design rationale.
pub trait JournalStore: Send + Sync {
    /// Stable identifier, e.g. `"sqlite"`. Written into the vault header so
    /// the right backend is chosen when the vault is reopened.
    fn backend(&self) -> &'static str;

    fn capabilities(&self) -> Capabilities;

    /// Storage for the task domain, if this backend has any.
    ///
    /// Returning `None` -- the default -- is a backend saying "journals are
    /// all I do". Every backend shipped today carries all four domains, but
    /// the accessors stay optional because the alternative is worse: folding
    /// a kanban board into the journal trait would oblige a read-only import
    /// source, or a store built for one app, to grow a todo list it has no
    /// use for. See [`tasks`](crate::store::tasks) for why this is an
    /// accessor rather than more methods on this trait.
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

    /// Storage for the library domain, if this backend has any.
    ///
    /// Same shape and same reasoning as [`JournalStore::tasks`]: a backend
    /// says what it does rather than failing when asked to do it. See
    /// [`library`](crate::store::library) for the three records it holds and
    /// the cascades between them.
    fn library(&self) -> Option<&dyn LibraryStore> {
        None
    }

    /// Storage for the tracking domain, if this backend has any.
    ///
    /// Same shape and same reasoning as the three above. See
    /// [`trackers`](crate::store::trackers) for why a stream of timestamped
    /// numbers is its own trait — and why the *definitions* are not in it.
    fn trackers(&self) -> Option<&dyn TrackerStore> {
        None
    }

    /// Storage for the purpose domain, if this backend has any.
    ///
    /// Same shape and same reasoning as the four above. See
    /// [`purpose`](crate::store::purpose) for the two records it holds, the
    /// cascade it deliberately refuses, and why the reports are SQL rather
    /// than a fold in Rust.
    fn purpose(&self) -> Option<&dyn PurposeStore> {
        None
    }

    /// Storage for notes, if this backend has any.
    ///
    /// Same shape and same reasoning as the four above. See
    /// [`notes`](crate::store::notes) for what a note is and what it is not.
    fn notes(&self) -> Option<&dyn NoteStore> {
        None
    }

    /// Storage for the assistant, if this backend has any.
    ///
    /// Same shape and same reasoning as the four above. See
    /// [`agent`](crate::store::agent) for why a chat transcript is its own
    /// trait -- and why none of it is left in the clear.
    fn agent(&self) -> Option<&dyn AgentStore> {
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

    /// Write `entry`, whatever is already there. Import, restore, and the
    /// deliberate "keep mine" after a conflict.
    fn put_entry(&self, entry: &Entry) -> Result<()>;

    /// Write `entry` only if the stored copy is still the one the caller
    /// read, failing with [`Error::Conflict`] if it is not.
    ///
    /// `expect` is the `updated_at` the caller last saw, or `None` for "this
    /// entry is new and nothing should be there yet". Note that it cannot be
    /// taken from `entry` itself: by the time a save happens the caller has
    /// already stamped a *fresh* `updated_at` on it, so the version being
    /// replaced has to be carried separately.
    ///
    /// This default is a read, a comparison and a write, which is correct
    /// against another thread -- the vault's own write lock covers that --
    /// but not against another *process* slipping between the read and the
    /// write. Backends that can say it in one statement should override it,
    /// and the SQLite one does.
    fn put_entry_if(&self, entry: &Entry, expect: Option<jiff::Timestamp>) -> Result<()> {
        match (self.get_entry(entry.id), expect) {
            // The happy path: it is there, and it is what we read.
            (Ok(current), Some(want)) if current.updated_at == want => {}
            // It is there and it is not. Someone else wrote it.
            (Ok(_), _) => return Err(Error::Conflict { kind: "entry" }),
            // Not there. Fine if we were creating it, a conflict if we
            // thought we were updating one -- it has been deleted under us,
            // and silently recreating it would undo that deletion.
            (Err(e), expect) if e.code() == "not_found" => {
                if expect.is_some() {
                    return Err(Error::Conflict { kind: "entry" });
                }
            }
            (Err(e), _) => return Err(e),
        }
        self.put_entry(entry)
    }

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

    /// Check the store's own consistency, returning what is wrong with it.
    ///
    /// An empty vector means healthy. This is about *structural* damage --
    /// a torn page, a broken index -- not about whether the key is right;
    /// a store that cannot decrypt is a different failure with a different
    /// message. Cheap enough to run on unlock.
    fn check_integrity(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Write a consistent copy of everything this store holds into `dir`.
    ///
    /// `dir` must be empty or absent. The copy has to be readable by a
    /// normal open, so a backend that keeps records in a database writes a
    /// real database and not a dump: restoring is then a directory move,
    /// which is a thing someone can do while upset, at night, without
    /// instructions.
    ///
    /// The snapshot stays sealed under the same key. It is a copy of the
    /// vault, not an export of its contents -- see the CLI's `export` for
    /// that -- so it is exactly as safe to keep as the vault itself.
    fn snapshot(&self, _dir: &std::path::Path) -> Result<()> {
        Err(Error::Unsupported("backing up this storage backend"))
    }

    /// How long ago `id` was written, or `None` if the backend cannot say.
    ///
    /// Only [`collect_garbage`](JournalStore::collect_garbage) needs this, and
    /// only to tell a genuine orphan from a blob that is about to be
    /// referenced. A backend that cannot answer collects nothing young,
    /// because `None` is treated as "too recent to judge".
    fn blob_age(&self, _id: BlobId) -> Result<Option<std::time::Duration>> {
        Ok(None)
    }

    /// Delete blobs that no entry references any more, returning how many
    /// were removed.
    ///
    /// `grace` is the age a blob must reach before it can be collected, and
    /// it is the whole safety story here. Liveness is decided by walking the
    /// entries, so a blob is "garbage" from the instant it is stored until
    /// the entry embedding it is saved -- and in the editor those are two
    /// separate events with a person typing in between. Worse, they can be
    /// in two *processes*: `everyday gc` on the command line cannot see the
    /// image you just pasted into the open window.
    ///
    /// Anything younger than `grace` is therefore left alone no matter what
    /// the reference walk says. The cost of keeping an orphan another day is
    /// a few kilobytes; the cost of collecting a live one is an attachment
    /// that is gone for good.
    fn collect_garbage(&self, grace: std::time::Duration) -> Result<u64> {
        let mut live = std::collections::BTreeSet::new();
        for entry in self.all_entries()? {
            live.extend(entry.body.blob_refs());
            live.extend(entry.attachments.iter().map(|a| a.blob));
        }
        // A library cover is a blob like any other, and liveness here is
        // decided by walking every record that can hold one. Missing this
        // walk would make the first `everyday gc` after a day's grace delete
        // the cover of every book on the shelf -- silently, since the item
        // itself would survive with a blob id pointing at nothing.
        if let Some(library) = self.library() {
            let all = crate::store::library::ItemQuery::default();
            live.extend(library.list_items(&all)?.iter().filter_map(|i| i.cover));
        }
        // And a note holds a document, so it holds pictures. Same walk, same
        // reason: a photograph dropped into a note is a photograph somebody
        // wants kept, and a sweep that did not know about notes would take it
        // and leave the note pointing at nothing.
        if let Some(notes) = self.notes() {
            for note in notes.all_notes()? {
                live.extend(note.body.blob_refs());
                live.extend(note.attachments.iter().map(|a| a.blob));
            }
        }
        let mut removed = 0;
        for id in self.list_blobs()? {
            if live.contains(&id) {
                continue;
            }
            // Unknown age counts as young. A backend that cannot date its
            // blobs should under-collect, never over-collect.
            match self.blob_age(id)? {
                Some(age) if age >= grace => {}
                _ => continue,
            }
            self.delete_blob(id)?;
            removed += 1;
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
    /// Short human name, shown as the heading of the option in the picker.
    fn name(&self) -> &'static str;
    /// Short human description, shown in the vault-creation UI.
    fn describe(&self) -> &'static str;
    /// What this backend must be told before it can be opened.
    ///
    /// Empty -- the default -- is a backend saying "the directory is all I
    /// need", which is what every file-backed store means. A backend that
    /// talks to a server returns the fields the setup screen should ask for;
    /// see [`SettingSpec`].
    fn settings(&self) -> Vec<SettingSpec> {
        Vec::new()
    }

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

    /// Everything the backend picker needs, in registration order.
    pub fn describe_all(&self) -> Vec<BackendInfo> {
        self.factories
            .iter()
            .map(|f| BackendInfo {
                id: f.id(),
                name: f.name(),
                description: f.describe(),
                settings: f.settings(),
            })
            .collect()
    }

    /// What `id` must be configured with, or `None` if it is not registered.
    pub fn settings_for(&self, id: &str) -> Option<Vec<SettingSpec>> {
        self.factories.iter().find(|f| f.id() == id).map(|f| f.settings())
    }

    pub fn open(&self, id: &str, ctx: StoreContext) -> Result<Box<dyn JournalStore>> {
        self.factories
            .iter()
            .find(|f| f.id() == id)
            .ok_or_else(|| Error::UnknownBackend(id.to_string()))?
            .open(ctx)
    }
}

pub mod agent;
pub mod calendars;
pub mod library;
pub mod notes;
pub mod purpose;
pub mod tasks;
pub mod trackers;

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
        let ctx = StoreContext::new("/tmp/nope", Arc::new(crate::crypto::NullCipher));
        let Err(err) = reg.open("redis", ctx) else { panic!("expected an error") };
        assert!(matches!(err, Error::UnknownBackend(b) if b == "redis"));
    }
}
