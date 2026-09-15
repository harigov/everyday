//! Keys and paths for storage that lives beside the vault rather than inside
//! its own backend: mail's pack store and search index, so far.
//!
//! Neither of those is a `JournalStore` domain -- a pack store is files (or,
//! on Postgres, rows a `PackStore` reaches without going through
//! `JournalStore` at all) and a search index is a tantivy directory -- so
//! whoever assembles them (`everyday-service`, on unlock) needs two things
//! only the vault itself can answer: a key, and where the vault's own root
//! is. Both are here rather than spread across the caller, because both are
//! "ask the vault" questions and the vault is what already holds the DEK and
//! knows its own layout.

use super::Vault;
use crate::crypto::SecretKey;
use crate::error::Result;
use std::path::PathBuf;

impl Vault {
    /// A key derived from this vault's own, distinct for every `label`.
    ///
    /// See [`crate::crypto::Cipher::derive_subkey`] for why this exists
    /// rather than handing out the vault's cipher directly: mail's pack
    /// store and search index are new storage, sealed under a key of their
    /// own so that a framing mistake in either cannot say anything about the
    /// key that seals a journal entry. `Err(Error::Locked)` when the vault
    /// has no key to derive from.
    pub fn derive_subkey(&self, label: &str) -> Result<SecretKey> {
        self.read(|u| Ok(u.cipher.derive_subkey(label)))
    }

    /// Where this vault's backend keeps its own files, for a local backend --
    /// `<vault root>/store`. Exists so a caller assembling storage that
    /// lives *beside* the backend (mail's pack store, on a SQLite vault) can
    /// find the same directory [`crate::blobstore::FileBlobStore`] and the
    /// backend's own tables already share, without this crate exposing the
    /// private constant that names it.
    ///
    /// Meaningful for any vault -- a Postgres vault still has this directory,
    /// holding its header's neighbour files -- but only a local backend
    /// stores anything inside it worth finding.
    pub fn store_root(&self) -> PathBuf {
        self.root.join(super::STORE_DIRNAME)
    }
}
