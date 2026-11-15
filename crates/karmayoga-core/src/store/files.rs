//! Storage for the files catalog: one record per file, albums, which records
//! refer to which files, and the settings.
//!
//! # Why this is its own trait
//!
//! Money's reason: a backend that keeps journals as Markdown files has no
//! catalog to keep, so the domain is reached through
//! [`JournalStore::files`](super::JournalStore::files), `None` by default,
//! and the interface reads [`Capabilities::files`](super::Capabilities::files)
//! to know whether to offer the app at all.
//!
//! # What stays readable, and why that is safe
//!
//! A file's record is sealed whole under its id. Beside it, clear columns
//! for the indexes:
//!
//! - **For every file:** its id, `storage` (`plain` or `sealed`), `kind`,
//!   `size`, `origin`, `status`, `rating`, `favourite`, `hidden`, `stack`,
//!   its added and updated times, and the blob ids of a sealed file's
//!   contents and of its thumbnail (keyed addresses, which say nothing about
//!   the bytes).
//! - **For a plain file only:** `modified_us` and `when_us`
//!   ([`FileEntry::when`]), timestamps as every table keeps them; and
//!   *tokens* -- keyed hashes under a subkey of the vault key -- of its path,
//!   its folder and its contents, so "the file at this path", "the files in
//!   this folder" and "the files with these contents" are index reads. Never
//!   the path, the folder, the name or the content hash themselves: the
//!   folder may be on an encrypted volume that is locked while somebody
//!   holds a copy of the database, and then a readable path would say what
//!   the volume was locked to keep. A sealed file's are `NULL`.
//! - `rating` and `favourite` are a person's own marks, readable as the
//!   library's are, so a "favourites" view is an index read.
//!
//! Captions, tags, people, places, properties and everything the file said
//! about itself (bar the time) stay sealed. What no token answers -- every
//! file *under* a folder, a listing by name -- the store answers after
//! opening the records.
//!
//! # The rules an implementation must keep
//!
//! Every one of these is in the conformance suite (`conformance/files.rs`):
//!
//! - **A record is bound to its id**, so a payload copied into another row
//!   will not open there.
//! - **A plain file's path is unique** among records not in the trash
//!   ([`FilesStore::put_file`] answers [`Error::Conflict`] for a second
//!   one). A trashed file's path is its place in the trash, unique anyway.
//! - **[`FilesStore::query_files`] lists in the order the filter asks**,
//!   ties by id in the same direction. A sealed file's time is when it was
//!   added, for sorting and for `from`/`to` alike. By name, plain files sort
//!   without regard to case and sealed files are a group of their own in the
//!   order they were added -- after the plain ones ascending, before them
//!   descending. [`FilesStore::count_files`] counts what the same filter
//!   would list, ignoring `limit` and `offset`.
//! - **Deleting a file deletes its references and album memberships**,
//!   never the records that referred to it.
//! - **Deleting an album never deletes a file.**
//! - **[`FilesStore::set_refs`] replaces an owner's whole set**; an empty set
//!   forgets the owner.
//!
//! [`Error::Conflict`]: crate::error::Error::Conflict

use crate::error::Result;
use crate::files::{Album, FileEntry, FileFilter, FilesSettings, FilesUsage, IndexRow, RefOwner};
use crate::id::{AlbumId, BlobId, FileId};
use std::collections::{BTreeMap, BTreeSet};

// `FileSort` is named in the docs above.
#[allow(unused_imports)]
use crate::files::FileSort;

pub trait FilesStore: Send + Sync {
    // ---- files ----------------------------------------------------------

    /// Insert or replace.
    fn put_file(&self, file: &FileEntry) -> Result<()>;
    /// Several at once, in one transaction: the reconciler's and a bulk
    /// edit's.
    fn put_files(&self, files: &[FileEntry]) -> Result<()>;
    /// [`Error::NotFound`](crate::error::Error::NotFound) when absent.
    fn get_file(&self, id: FileId) -> Result<FileEntry>;
    /// Several by id, in no particular order; ids not found are left out.
    fn get_files(&self, ids: &[FileId]) -> Result<Vec<FileEntry>>;
    /// The plain file at `path`, trashed ones included.
    fn file_by_path(&self, path: &str) -> Result<Option<FileEntry>>;
    /// Plain files with these contents.
    fn files_by_hash(&self, hash: &str) -> Result<Vec<FileEntry>>;
    /// Delete the record, its references and its album memberships.
    fn delete_file(&self, id: FileId) -> Result<()>;
    fn query_files(&self, filter: &FileFilter) -> Result<Vec<FileEntry>>;
    fn count_files(&self, filter: &FileFilter) -> Result<u64>;
    /// How many files not in the trash are under each of `tops` in a stack.
    /// A file that tops no stack is absent from the answer.
    fn stack_counts(&self, tops: &[FileId]) -> Result<BTreeMap<FileId, u64>>;
    /// Every plain file not in the trash, for the reconciler. Clear columns
    /// only: nothing is opened.
    fn file_index(&self) -> Result<Vec<IndexRow>>;
    fn files_usage(&self) -> Result<FilesUsage>;
    /// Every blob a record keeps alive: sealed contents and thumbnails.
    /// What [`JournalStore::collect_garbage`](super::JournalStore::collect_garbage)
    /// must not collect.
    fn file_blob_refs(&self) -> Result<BTreeSet<BlobId>>;

    // ---- references -----------------------------------------------------

    /// Replace everything `owner` refers to.
    fn set_refs(&self, owner: &RefOwner, files: &[FileId]) -> Result<()>;
    /// Replace the whole table: [`crate::Vault::rebuild_file_refs`]'s.
    fn replace_all_refs(&self, refs: &[(RefOwner, FileId)]) -> Result<()>;
    /// Who refers to `file`, by owner.
    fn refs_to(&self, file: FileId) -> Result<Vec<RefOwner>>;
    /// Every file anything refers to.
    fn referenced_files(&self) -> Result<BTreeSet<FileId>>;

    // ---- albums ---------------------------------------------------------

    fn list_albums(&self) -> Result<Vec<Album>>;
    fn get_album(&self, id: AlbumId) -> Result<Album>;
    fn put_album(&self, album: &Album) -> Result<()>;
    /// The album and its memberships; never a file.
    fn delete_album(&self, id: AlbumId) -> Result<()>;
    /// In the order they were added.
    fn album_files(&self, id: AlbumId) -> Result<Vec<FileId>>;
    /// Appended in the order given; a file already in the album stays where
    /// it is.
    fn add_to_album(&self, id: AlbumId, files: &[FileId]) -> Result<()>;
    fn remove_from_album(&self, id: AlbumId, files: &[FileId]) -> Result<()>;
    fn albums_of(&self, file: FileId) -> Result<Vec<AlbumId>>;
    /// How many files each album holds, trashed ones left out.
    fn album_counts(&self) -> Result<BTreeMap<AlbumId, u64>>;

    // ---- settings -------------------------------------------------------

    /// [`FilesSettings::default`] when never saved.
    fn files_settings(&self) -> Result<FilesSettings>;
    fn put_files_settings(&self, settings: &FilesSettings) -> Result<()>;
}

/// Associated data bound into a record's ciphertext. See
/// [`entry_aad`](super::entry_aad) for why records are bound to their id.
/// In the `everyday.<word>.v1` family every record kind's label is in,
/// whatever the application is called -- see [`RecordKind::aad_word`].
///
/// [`RecordKind::aad_word`]: crate::record::RecordKind::aad_word
pub fn file_aad(id: FileId) -> Vec<u8> {
    format!("everyday.file.v1:{id}").into_bytes()
}

pub fn album_aad(id: AlbumId) -> Vec<u8> {
    format!("everyday.album.v1:{id}").into_bytes()
}

/// The settings singleton.
pub fn files_settings_aad() -> Vec<u8> {
    b"karmayoga.files-settings.v1".to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_label_is_distinct() {
        let id = uuid::Uuid::now_v7();
        let labels = [file_aad(FileId(id)), album_aad(AlbumId(id)), files_settings_aad()];
        for (i, a) in labels.iter().enumerate() {
            for b in &labels[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
