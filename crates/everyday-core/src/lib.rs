//! Core of **Every Day**, a private journal.
//!
//! This crate is the whole application minus its pixels: the domain model,
//! the encryption envelope, the pluggable storage abstraction, the search
//! index and the vault lifecycle. It has no UI, no platform and no async
//! runtime dependencies, which is what lets the same logic back a desktop
//! shell today and a mobile one later.
//!
//! ## Layers
//!
//! ```text
//!   Vault              lock / unlock, password changes, search index
//!     |
//!   JournalStore        trait: sqlite, markdown, ... (crate::store)
//!     +-- TaskStore     optional second domain (crate::store::tasks)
//!     +-- CalendarStore optional third domain  (crate::store::calendars)
//!     |
//!   Cipher             trait: XChaCha20-Poly1305 or none (crate::crypto)
//! ```
//!
//! ## Domains
//!
//! A vault holds three kinds of thing. Journals and entries ([`model`]) are
//! the original; projects, tasks and blocks of time ([`task`]) are the todo
//! app; subscribed calendars and their events ([`calendar`]) are the third.
//!
//! The calendar domain is the smallest of the three on purpose. Time you
//! schedule for yourself was already a [`TimeBlock`] — a record in its own
//! right, with a planned/actual split and an ad-hoc subject, precisely so
//! that a calendar would not need a storage layer of its own. What
//! [`calendar`] adds is the part that genuinely was missing: *other
//! people's* calendars, read from iCalendar feeds by [`ics`].

pub mod blobstore;
pub mod calendar;
pub mod crypto;
pub mod error;
pub mod fsutil;
pub mod ics;
pub mod id;
pub mod lockfile;
pub mod model;
pub mod richtext;
pub mod search;
pub mod store;
pub mod task;
pub mod vault;

pub use blobstore::FileBlobStore;
pub use calendar::{Calendar, CalendarOrigin, CalendarProvider, Event, EventStatus, SyncReport};
pub use error::{Error, Result};
pub use id::{BlobId, BlockId, CalendarId, EntryId, EventId, JournalId, ProjectId, TaskId};
pub use model::{Attachment, Entry, EntrySummary, Journal, Location, MediaKind, Weather};
pub use richtext::RichDoc;
pub use store::calendars::{CalendarStore, EventQuery};
pub use store::tasks::{BlockQuery, ParentScope, ProjectScope, TaskQuery, TaskSort, TaskStore};
pub use store::{
    BackendRegistry, Capabilities, EntryQuery, JournalStore, SortOrder, StoreContext, StoreFactory,
    StoreStats,
};
pub use task::{
    BlockKind, BlockSubject, Priority, Project, ProjectStatus, ProjectTaskCount, Task, TaskStats,
    TaskStatus, TimeBlock,
};
pub use vault::{Vault, VaultConfig, VaultHeader, VaultStatus};
