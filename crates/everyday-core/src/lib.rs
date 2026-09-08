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
//!     +-- LibraryStore  optional fourth domain (crate::store::library)
//!     +-- TrackerStore  optional fifth domain  (crate::store::trackers)
//!     +-- AgentStore    optional sixth domain  (crate::store::agent)
//!     |
//!   Cipher             trait: XChaCha20-Poly1305 or none (crate::crypto)
//! ```
//!
//! ## Domains
//!
//! A vault holds six kinds of thing. Journals and entries ([`model`]) are
//! the original; projects, tasks and blocks of time ([`task`]) are the todo
//! app; subscribed calendars and their events ([`calendar`]) are the third;
//! shelves, the things on them and the log of what you did with them
//! ([`library`]) are the fourth; and what a day produced in numbers rather
//! than in prose -- habits, doses, symptoms, counts ([`tracker`]) -- are the
//! fifth. The sixth is the assistant ([`agent`]) -- its configuration, the
//! threads you have had with it, and what you asked it to remember -- and it
//! is the only domain that exists to act on the other five rather than to
//! record anything of its own.
//!
//! The calendar domain is the smallest of the three on purpose. Time you
//! schedule for yourself was already a [`TimeBlock`] — a record in its own
//! right, with a planned/actual split and an ad-hoc subject, precisely so
//! that a calendar would not need a storage layer of its own. What
//! [`calendar`] adds is the part that genuinely was missing: *other
//! people's* calendars, read from iCalendar feeds by [`ics`].
//!
//! The library is the only domain that reaches outward for *content* rather
//! than for a feed: [`websearch`] looks a book or a film up and fills in the
//! metadata. It is built on the same principle as [`ics`] — this crate
//! composes the request and parses the reply, and something above it owns
//! the socket — so the whole of it is testable offline.
//!
//! The tracking domain splits itself between two places for a reason worth
//! knowing: a [`Tracker`](tracker::Tracker) — the decision to record
//! something — is a setting of a [`Journal`](model::Journal) and is sealed
//! inside it, while a [`Reading`](tracker::Reading) is a row of its own with
//! its date, its instant and its value in the clear. Definitions are few and
//! must stay private; readings are many and must stay scannable.

pub mod agent;
pub mod blobstore;
pub mod calendar;
pub mod crypto;
pub mod error;
pub mod fsutil;
pub mod ics;
pub mod id;
pub mod library;
pub mod lockfile;
pub mod model;
pub mod richtext;
pub mod search;
pub mod store;
pub mod task;
pub mod tracker;
pub mod vault;
pub mod websearch;

pub use agent::{
    AgentSettings, Conversation, Memory, Message, ModelConfig, Provider, Role, ToolCall,
};
pub use blobstore::FileBlobStore;
pub use calendar::{Calendar, CalendarOrigin, CalendarProvider, Event, EventStatus, SyncReport};
pub use error::{Error, Result};
pub use id::{
    BlobId, BlockId, CalendarId, ConversationId, EntryId, EventId, ItemId, JournalId, KindId,
    LogId, MemoryId, MessageId, ProjectId, ReadingId, TaskId, TrackerId,
};
pub use library::{
    ExternalRating, FieldDef, FieldType, Item, ItemStatus, Kind, KindCount, LibraryStats, Link,
    LogEntry, LogEvent, Progress, Verbs,
};
pub use model::{Attachment, Entry, EntrySummary, Journal, Location, MediaKind, Weather};
pub use richtext::RichDoc;
pub use store::agent::{AgentStore, ConversationQuery};
pub use store::calendars::{CalendarStore, EventQuery};
pub use store::library::{ItemQuery, ItemSort, LibraryStore, LogQuery};
pub use store::tasks::{BlockQuery, ParentScope, ProjectScope, TaskQuery, TaskSort, TaskStore};
pub use store::trackers::{ReadingQuery, TrackerDay, TrackerStore};
pub use store::{
    BackendRegistry, Capabilities, EntryQuery, JournalStore, SortOrder, StoreContext, StoreFactory,
    StoreStats,
};
pub use task::{
    BlockKind, BlockSubject, Priority, Project, ProjectStatus, ProjectTaskCount, Task, TaskStats,
    TaskStatus, TimeBlock,
};
pub use tracker::{Aggregate, Reading, Tracker, TrackerKind};
pub use vault::{Vault, VaultConfig, VaultHeader, VaultStatus};
pub use websearch::{Fetcher, SearchRequest, SearchResult, Source, WebSearch};
