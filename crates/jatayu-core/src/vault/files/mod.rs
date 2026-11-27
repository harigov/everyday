//! The files catalog, through the vault: every caller -- the service's
//! commands, the assistant's tools, the background reconciler, an app
//! keeping a picture -- goes through here, never to the store or the folder
//! directly.
//!
//! What this layer adds over [`FilesStore`] and [`Folder`]:
//!
//! - **Validation.** Names, paths, tags, people, properties and the
//!   settings, each against the limits in [`crate::files`].
//! - **The disk and the catalog moving together.** A rename renames the
//!   file, then the record; a failure of the second puts the first back. A
//!   new file is written before its record exists, so a record never names
//!   bytes that are not there.
//! - **The folder's state.** Nothing that writes runs while the folder is
//!   unavailable (see [`FolderState`]); reads of plain files answer an error
//!   that says so, and sealed files work regardless.
//! - **References.** [`Vault::set_file_refs`] is called by every save of a
//!   record that can hold files (entries, notes, assistant messages);
//!   [`Vault::rebuild_file_refs`] recomputes the whole table from those
//!   records, as a backstop.
//!
//! The service's `files` module does what needs more than the standard
//! library -- reading EXIF, drawing thumbnails, watching the folder -- and
//! hands its results back through [`Vault::save_file_derived`] and
//! [`Vault::apply_scan`].
//!
//! # Locks
//!
//! [`Vault::read`] and [`Vault::write`] take the vault's lock, which is not
//! re-entrant: nothing in a closure handed to either may call another
//! method of the vault. Every method here therefore works in short steps --
//! read the catalog, touch the disk, write the catalog -- and holds the
//! vault's lock only inside a step, never across the disk. The one lock held
//! across a step is `placing`, which orders the ones that choose a name in
//! the folder, so two imports of `IMG_0001.jpg` arriving together do not
//! both pick the same free name.
//!
//! # Layout
//!
//! - [`settings`] -- the folder and the settings.
//! - [`read`] -- reading the catalog and the bytes.
//! - [`add`] -- imports and uploads.
//! - [`organise`] -- renaming, moving, tagging, sealing, trashing.
//! - [`albums`] -- albums.
//! - [`references`] -- which records use which files.
//! - [`reconcile`] -- what the background reconciler calls.
//! - [`internals`] -- the shared cataloguing and disk steps.
//! - [`legacy`] -- pictures kept the old way, and carrying them over.
//! - [`helpers`] -- small shared functions.
//! - [`prepare`] -- one update checked once, applied to each file.

use super::Vault;
use super::session::Domain;
use crate::error::Result;
use crate::files::disk::{self, Folder};
use crate::files::{ImportOptions, MAX_NAME_LEN};
use crate::store::files::FilesStore;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

mod add;
mod albums;
mod helpers;
mod internals;
mod legacy;
mod organise;
mod prepare;
mod read;
mod reconcile;
mod references;
mod settings;

/// The most files one call may change, trash or move. Far more than a
/// screen can select; few enough that one exclusive section stays short.
pub(super) const MAX_BATCH: usize = 50_000;
/// How much of a file is read to tell what it is: enough for every format
/// [`disk::guess_mime`] knows to show its magic bytes.
pub(super) const HEAD_LEN: u64 = 8192;
/// The size of one step when a sealed file is read out piece by piece, so a
/// film of several gigabytes is never held in memory whole.
pub(super) const CHUNK: u64 = 4 * 1024 * 1024;
/// Uploads begun and neither finished nor cancelled, at most: a client that
/// begins and walks away cannot fill the table.
pub(super) const MAX_UPLOADS: usize = 256;
/// How long an upload may sit untouched before it is forgotten.
pub(super) const UPLOAD_LIFETIME_SECS: u64 = 24 * 3600;
/// The longest a place's name may be, in characters.
pub(super) const MAX_PLACE_LEN: usize = MAX_NAME_LEN;
/// The longest a property's list may be.
pub(super) const MAX_PROPERTY_LIST: usize = 1000;
/// The longest an album's name may be, in characters.
pub(super) const MAX_ALBUM_NAME: usize = 200;
/// Where a sealed file is written out to when it remembers no place of its
/// own.
pub(super) const ENCRYPTED_DIR: &str = "Encrypted";
/// The property a file carries after being sealed: the path it had, so
/// writing it out again puts it back.
pub(super) const PROP_WAS_AT: &str = "files.was_at";
/// The property a file carries once somebody has corrected when it was
/// taken: what the file itself said, so the correction can be undone.
pub(super) const PROP_TAKEN_BY_FILE: &str = "files.taken_by_file";
/// Documents nested deeper than this are treated as corrupt, as in
/// [`crate::richtext`].
pub(super) const MAX_DEPTH: usize = 64;

/// The ordering lock for placing a file in the folder: choosing a free name,
/// writing the file there, and recording it.
pub(super) static PLACING: Mutex<()> = Mutex::new(());

pub(super) fn placing() -> MutexGuard<'static, ()> {
    PLACING.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What an upload was begun with, kept until it is finished or cancelled.
#[derive(Debug, Clone)]
pub(super) struct PendingUpload {
    name: String,
    size: u64,
    /// With `encrypted` settled when the upload began ([`Vault::begin_upload`]),
    /// so what the settings say by the time it finishes cannot move the
    /// bytes from where they were staged.
    opts: ImportOptions,
    begun: Instant,
}

impl PendingUpload {
    /// Will the upload be sealed, so that it is staged outside the folder?
    fn sealed(&self) -> bool {
        self.opts.encrypted == Some(true)
    }
}

/// Where an upload's bytes wait while they arrive and until they are placed
/// or sealed. Which it is follows from what the upload will become, and is
/// decided once, when it begins.
pub(super) enum UploadArea {
    /// A plain upload: in the files folder's internal folder, where the file
    /// is going to be in the clear anyway.
    Folder(Folder),
    /// A sealed one: in a private folder of this vault's in the cache
    /// directory, never in the files folder, which a sync tool may be
    /// reading ([`disk::UploadStaging`]).
    Private(disk::UploadStaging),
}

impl UploadArea {
    fn begin(&self, upload: &str) -> Result<u64> {
        match self {
            UploadArea::Folder(folder) => folder.begin_upload(upload),
            UploadArea::Private(staging) => staging.begin_upload(upload),
        }
    }

    fn append(&self, upload: &str, offset: u64, bytes: &[u8]) -> Result<u64> {
        match self {
            UploadArea::Folder(folder) => folder.append_upload(upload, offset, bytes),
            UploadArea::Private(staging) => staging.append_upload(upload, offset, bytes),
        }
    }

    fn cancel(&self, upload: &str) -> Result<()> {
        match self {
            UploadArea::Folder(folder) => folder.cancel_upload(upload),
            UploadArea::Private(staging) => staging.cancel_upload(upload),
        }
    }
}

/// Uploads in progress, by ticket. In memory only: an upload does not
/// survive a restart, and its staging file is swept by the files task's
/// hourly sweep (the folder's own for a plain upload,
/// [`Vault::sweep_open_copies`] for a sealed one).
pub(super) fn uploads() -> MutexGuard<'static, HashMap<String, PendingUpload>> {
    static UPLOADS: OnceLock<Mutex<HashMap<String, PendingUpload>>> = OnceLock::new();
    UPLOADS.get_or_init(Default::default).lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A ticket nobody can guess: sixteen random bytes as hex.
pub(super) fn new_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    crate::hex::encode(&bytes)
}

/// Where a new file's bytes come from.
pub(super) enum Source<'a> {
    Path(&'a Path),
    Bytes(&'a [u8]),
    /// A finished upload's staging file: in the folder's internal folder if
    /// the file is to be plain, in the private place outside it if sealed
    /// ([`UploadArea`]).
    Upload(&'a str),
}

impl Vault {
    /// Does this vault's backend keep a files catalog?
    pub fn supports_files(&self) -> bool {
        self.with_files(|_| Ok(())).is_ok()
    }

    pub(super) fn with_files<T>(&self, f: impl FnOnce(&dyn FilesStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::Files, |s| s.files().map(f))
    }
}
