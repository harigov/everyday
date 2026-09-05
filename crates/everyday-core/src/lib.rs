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
//!     |
//!   Cipher             trait: XChaCha20-Poly1305 or none (crate::crypto)
//! ```

pub mod blobstore;
pub mod crypto;
pub mod error;
pub mod id;
pub mod model;
pub mod richtext;
pub mod search;
pub mod store;
pub mod vault;

pub use blobstore::FileBlobStore;
pub use error::{Error, Result};
pub use id::{BlobId, EntryId, JournalId};
pub use model::{Attachment, Entry, EntrySummary, Journal, Location, MediaKind, Weather};
pub use richtext::RichDoc;
pub use store::{
    BackendRegistry, Capabilities, EntryQuery, JournalStore, SortOrder, StoreContext, StoreFactory,
    StoreStats,
};
pub use vault::{Vault, VaultConfig, VaultHeader, VaultStatus};
