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
//!   JournalStore       trait: sqlite, markdown, ... (crate::store)
//!     +-- TaskStore    optional second domain (crate::store::tasks)
//!     |
//!   Cipher             trait: XChaCha20-Poly1305 or none (crate::crypto)
//! ```
//!
//! ## Domains
//!
//! A vault holds two kinds of thing, and will hold a third. Journals and
//! entries ([`model`]) are the original; projects, tasks and blocks of time
//! ([`task`]) are the todo app; a calendar is next, and is why [`TimeBlock`]
//! exists as a record in its own right rather than a field on a task.

pub mod blobstore;
pub mod crypto;
pub mod error;
pub mod id;
pub mod model;
pub mod richtext;
pub mod search;
pub mod store;
pub mod task;
pub mod vault;

pub use blobstore::FileBlobStore;
pub use error::{Error, Result};
pub use id::{BlobId, BlockId, EntryId, JournalId, ProjectId, TaskId};
pub use model::{Attachment, Entry, EntrySummary, Journal, Location, MediaKind, Weather};
pub use richtext::RichDoc;
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
