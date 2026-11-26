//! Adding files: imports, uploads in pieces, and what the assistant or an app keeps.

use super::Vault;
use super::helpers::validate_file_name;
use super::{
    MAX_UPLOADS, PROP_TAKEN_BY_FILE, PendingUpload, Source, UPLOAD_LIFETIME_SECS, UploadArea,
    new_token, uploads,
};
use crate::error::{Error, Result};
use crate::files::disk;
use crate::files::{FileEntry, FileKind, ImportOptions, MediaInfo, PropValue, UploadTicket};
use crate::id::FileId;
use crate::record::RecordKind;
use jiff::Timestamp;
use std::path::Path;
use std::time::Instant;

impl Vault {
    /// Copy a file from anywhere on this machine into the catalog: into the
    /// folder (`opts.dir`, or the origin's own) under a free name, or sealed
    /// into the blob store. Metadata and a thumbnail come later, from the
    /// service, through [`Vault::save_file_derived`].
    pub fn import_file(&self, src: &Path, opts: &ImportOptions) -> Result<FileEntry> {
        let meta = std::fs::metadata(src).map_err(|e| Error::io(src, e))?;
        if !meta.is_file() {
            return Err(Error::Invalid(format!("{} is not a file", src.display())));
        }
        let name = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| Error::Invalid(format!("{} has no name", src.display())))?;
        self.add(Source::Path(src), &name, opts)
    }

    /// The same, from bytes already in memory: a mail attachment saved to
    /// Files, a file the assistant writes, a picture from a chat.
    pub fn import_bytes(
        &self,
        bytes: &[u8],
        name: &str,
        opts: &ImportOptions,
    ) -> Result<FileEntry> {
        self.add(Source::Bytes(bytes), name, opts)
    }

    /// Start an upload of `size` bytes called `name`. The ticket names it in
    /// [`Vault::upload_chunk`], [`Vault::finish_upload`] and
    /// [`Vault::cancel_upload`].
    ///
    /// Whether the file will be sealed is settled here, from the options and
    /// the settings as they are now, and decides where the bytes are staged:
    /// a plain file's in the folder's internal folder, a sealed one's in a
    /// private place outside the files folder ([`disk::UploadStaging`]),
    /// because until it is finished a sealed upload is the plaintext of a file
    /// the person chose to keep encrypted, and the folder may be synced.
    /// A sealed upload therefore needs no files folder at all.
    pub fn begin_upload(
        &self,
        name: &str,
        size: u64,
        opts: &ImportOptions,
    ) -> Result<UploadTicket> {
        self.writable()?;
        let name = validate_file_name(opts.name.as_deref().unwrap_or(name))?;
        if let Some(dir) = &opts.dir {
            disk::validate_rel(dir)?;
        }
        let sealed = self.is_sealed(opts)?;
        let area = self.upload_area(sealed)?;
        let upload = new_token();
        let received = area.begin(&upload)?;
        let mut pending = uploads();
        pending.retain(|_, p| p.begun.elapsed().as_secs() < UPLOAD_LIFETIME_SECS);
        if pending.len() >= MAX_UPLOADS {
            drop(pending);
            let _ = area.cancel(&upload);
            return Err(Error::Invalid("too many uploads are in progress".into()));
        }
        pending.insert(
            upload.clone(),
            PendingUpload {
                name,
                size,
                opts: ImportOptions { encrypted: Some(sealed), ..opts.clone() },
                begun: Instant::now(),
            },
        );
        Ok(UploadTicket { upload, received })
    }

    /// Append a chunk at `offset`. Answers the bytes received so far.
    pub fn upload_chunk(&self, upload: &str, offset: u64, bytes: &[u8]) -> Result<u64> {
        self.writable()?;
        let (size, sealed) = uploads()
            .get(upload)
            .map(|p| (p.size, p.sealed()))
            .ok_or_else(|| Error::not_found("upload", upload))?;
        if offset.checked_add(bytes.len() as u64).is_none_or(|end| end > size) {
            return Err(Error::Invalid(format!(
                "that chunk runs past the {size} bytes this upload was begun with"
            )));
        }
        self.upload_area(sealed)?.append(upload, offset, bytes)
    }

    /// Place (or seal) a complete upload and catalog it. Refuses one whose
    /// received size is not the size it was begun with.
    pub fn finish_upload(&self, upload: &str) -> Result<FileEntry> {
        self.writable()?;
        let pending =
            uploads().get(upload).cloned().ok_or_else(|| Error::not_found("upload", upload))?;
        // Begun again, an upload answers what it has: the one way to learn
        // the staging file's length without reaching into it.
        let received = self.upload_area(pending.sealed())?.begin(upload)?;
        if received != pending.size {
            return Err(Error::Invalid(format!(
                "received {received} of the {} bytes this upload was begun with",
                pending.size
            )));
        }
        let entry = self.add(Source::Upload(upload), &pending.name, &pending.opts)?;
        uploads().remove(upload);
        Ok(entry)
    }

    pub fn cancel_upload(&self, upload: &str) -> Result<()> {
        self.writable()?;
        uploads().remove(upload);
        // Whatever the ticket was, the staging file under that name goes,
        // from wherever it was kept: the ticket may be one this run never
        // heard of. A folder that is not there has nothing staged.
        disk::UploadStaging::for_vault(self.path()).cancel_upload(upload)?;
        match self.files_folder()? {
            Some(folder) if folder.root().is_dir() => folder.cancel_upload(upload),
            _ => Ok(()),
        }
    }

    /// Will a file added with `opts` be sealed in the vault rather than
    /// written to the folder? What the options say, and the settings for the
    /// file's origin when they say nothing.
    pub(super) fn is_sealed(&self, opts: &ImportOptions) -> Result<bool> {
        Ok(match opts.encrypted {
            Some(sealed) => sealed,
            None => self.files_settings()?.encrypted.contains(&opts.origin),
        })
    }

    /// The place an upload's bytes are staged: private and outside the files
    /// folder for one that will be sealed, the folder's own for one that will
    /// not -- which is then the folder that is usable, or an error saying why
    /// not.
    fn upload_area(&self, sealed: bool) -> Result<UploadArea> {
        if sealed {
            Ok(UploadArea::Private(disk::UploadStaging::for_vault(self.path())))
        } else {
            Ok(UploadArea::Folder(self.usable_folder()?))
        }
    }

    /// Store what the service read out of a file and the thumbnail it drew.
    /// `media` replaces the record's own wholesale; the kind and MIME type
    /// are refined when given. A thumbnail of `None` leaves the old one.
    ///
    /// A time somebody corrected (see [`FileUpdate::taken_at`]) outlives
    /// reading the file again: the file's own is kept beside it, so
    /// clearing the correction still goes back to what the file says.
    pub fn save_file_derived(
        &self,
        id: FileId,
        media: MediaInfo,
        thumbnail: Option<&[u8]>,
        mime: Option<&str>,
    ) -> Result<FileEntry> {
        self.writable()?;
        let mut entry = self.file(id)?;
        let mut media = media;
        if entry.properties.contains_key(PROP_TAKEN_BY_FILE) {
            let by_file = media.taken_at.map(|t| t.to_string()).unwrap_or_default();
            entry.properties.insert(PROP_TAKEN_BY_FILE.into(), PropValue::Text(by_file));
            media.taken_at = entry.media.taken_at;
            media.taken_local = None;
        }
        entry.media = media;
        if let Some(mime) = mime.map(str::trim).filter(|m| !m.is_empty()) {
            entry.mime = mime.to_string();
            entry.kind = FileKind::classify(mime, &entry.name);
        }
        if let Some(bytes) = thumbnail {
            entry.thumbnail = Some(self.read(|u| u.store.put_blob(bytes))?);
        }
        entry.updated_at = Timestamp::now();
        self.with_files(|s| s.put_file(&entry))?;
        self.wrote(RecordKind::File, id);
        Ok(entry)
    }
}
