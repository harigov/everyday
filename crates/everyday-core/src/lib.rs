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
//!   JournalStore       trait: sqlite, postgres, ... (crate::store)
//!     +-- NoteStore      optional (crate::store::notes)
//!     +-- TaskStore      optional (crate::store::tasks)
//!     +-- CalendarStore  optional (crate::store::calendars)
//!     +-- LibraryStore   optional (crate::store::library)
//!     +-- TrackerStore   optional (crate::store::trackers)
//!     +-- PurposeStore   optional (crate::store::purpose)
//!     +-- RoutineStore   optional (crate::store::routines)
//!     +-- AgentStore     optional (crate::store::agent)
//!     |
//!   Cipher             trait: XChaCha20-Poly1305 or none (crate::crypto)
//! ```
//!
//! Every store past the first is optional, and a backend says which it
//! carries through [`Capabilities`]. The interface reads that and hides what
//! cannot work rather than failing at click time. The list is unnumbered on
//! purpose: they were once "the second domain", "the third", and the numbers
//! were wrong within two releases.
//!
//! ## Domains
//!
//! Journals and entries ([`model`]) are the original, and everything since is
//! another kind of thing a day can leave behind:
//!
//! - [`note`] — writing that is not filed under a day.
//! - [`task`] — projects, tasks and blocks of time: the todo app, and the
//!   records the calendar draws.
//! - [`calendar`] — subscribed calendars and the events read out of them.
//! - [`library`] — shelves, the things on them, and the log of what you did
//!   with them.
//! - [`tracker`] — what a day produced in numbers rather than in prose:
//!   habits, doses, symptoms, counts.
//! - [`purpose`] — the roles you play and the goals under them, which is what
//!   lets any of the above answer "what was this *for*".
//! - [`routine`] — the assistant's standing work, and the log of what it did.
//! - [`agent`] — the assistant itself: its configuration, the threads you
//!   have had with it, and what you asked it to remember. The only domain
//!   that exists to act on the others rather than to record anything of its
//!   own.
//!
//! The calendar domain is the smallest of them on purpose. Time you schedule
//! for yourself was already a [`TimeBlock`] — a record in its own right, with
//! a planned/actual split and an ad-hoc subject, precisely so that a calendar
//! would not need a storage layer of its own. What [`calendar`] adds is the
//! part that genuinely was missing: *other people's* calendars, read from
//! iCalendar feeds by [`ics`].
//!
//! The library is the only domain that reaches outward for *content* rather
//! than for a feed: [`websearch`] looks a book or a film up and fills in the
//! metadata. It is built on the same principle as [`ics`] — this crate
//! composes the request and parses the reply, and something above it owns
//! the socket — so the whole of it is testable offline.
//!
//! The tracking domain splits itself between two places for a reason worth
//! knowing: a [`Tracker`](tracker::Tracker) — the decision to record
//! something — is a vault record with its payload sealed, while a
//! [`Reading`](tracker::Reading) is a row of its own with its date, its
//! instant and its value in the clear. Definitions are few and must stay
//! private; readings are many and must stay scannable. A journal names the
//! trackers whose chips it draws and no more: the definitions used to live
//! inside the sealed journal record, which made a tracker belong to one
//! journal — see [`Journal::shown_trackers`](model::Journal::shown_trackers).

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
pub mod note;
pub mod profile;
pub mod purpose;
pub mod quick;
pub mod richtext;
pub mod routine;
pub mod search;
pub mod store;
pub mod task;
pub mod tracker;
pub mod vault;
pub mod websearch;

pub use agent::{
    AgentSettings, Conversation, LLMModelConfig, LLMProviderConfig, Memory, Message, Provider,
    Role as MessageRole, ToolCall,
};
pub use blobstore::FileBlobStore;
pub use calendar::{Calendar, CalendarOrigin, CalendarProvider, Event, EventStatus, SyncReport};
pub use error::{Error, Result};
pub use id::{
    BlobId, BlockId, CalendarId, ConversationId, EntryId, EventId, GoalId, ItemId, JournalId,
    KindId, LogId, MemoryId, MessageId, NoteId, ProjectId, ReadingId, RoleId, RoutineId,
    RoutineRunId, TaskId, TrackerId,
};
pub use library::{
    ExternalRating, FieldDef, FieldType, Item, ItemStatus, Kind, KindCount, LibraryStats, Link,
    LogEntry, LogEvent, Progress, Verbs,
};
pub use model::{Attachment, Entry, EntrySummary, Journal, Location, MediaKind, Weather};
pub use note::{Note, NoteSummary};
pub use profile::Profile;
pub use purpose::{
    Goal, GoalActivity, GoalStatus, Purpose, PurposeMinutes, Role, RoleEventMinutes,
};
pub use quick::{Prompt as QuickPrompt, QuickApp, QuickContext, QuickJob, QuickPolicy};
pub use richtext::RichDoc;
pub use routine::{Due, Outcome, Routine, RoutineRun, Trigger, Weekday};
pub use store::agent::{AgentStore, ConversationQuery};
pub use store::calendars::{CalendarStore, EventQuery};
pub use store::library::{ItemQuery, ItemSort, LibraryStore, LogQuery};
pub use store::notes::{NoteQuery, NoteSort, NoteStore};
pub use store::purpose::{GoalQuery, PurposeStore, PurposeWindow};
pub use store::routines::RunQuery;
pub use store::tasks::{BlockQuery, ParentScope, ProjectScope, TaskQuery, TaskSort, TaskStore};
pub use store::trackers::{ReadingQuery, TrackerDay, TrackerStore};
pub use store::{
    BackendInfo, BackendRegistry, BackendSettings, Capabilities, EntryQuery, JournalStore,
    SettingSpec, SortOrder, StoreContext, StoreFactory, StoreStats,
};
pub use task::{
    BlockKind, BlockSubject, Priority, Project, ProjectStatus, ProjectTaskCount, Task, TaskStats,
    TaskStatus, TimeBlock,
};
pub use tracker::{Aggregate, Reading, Tracker, TrackerKind};
pub use vault::{Vault, VaultConfig, VaultHeader, VaultStatus};
pub use websearch::{Fetcher, SearchRequest, SearchResult, Source, WebSearch};
