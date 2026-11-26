//! The steps the other parts share: cataloguing a new file and the disk work under it.

use super::Vault;
use super::helpers::{
    from_micros, hash_file, new_entry, read_head, set_plain_path, unique, validate_file_name,
};
use super::{CHUNK, HEAD_LEN, Source, new_token, placing};
use crate::error::{Error, Result};
use crate::files::disk::{self, Folder, Written};
use crate::files::{FileEntry, FileFilter, FileStatus, FileStorage, ImportOptions, StatusFilter};
use crate::id::{BlobId, FileId};
use crate::record::RecordKind;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

impl Vault {
    /// Catalog a new file: sealed into the blob store or placed in the
    /// folder, whichever the options and the settings say. The one path
    /// every way of adding a file shares.
    pub(super) fn add(
        &self,
        source: Source<'_>,
        name: &str,
        opts: &ImportOptions,
    ) -> Result<FileEntry> {
        self.writable()?;
        let name = validate_file_name(opts.name.as_deref().unwrap_or(name))?;
        let sealed = self.is_sealed(opts)?;
        let now = Timestamp::now();
        let today = now.to_zoned(TimeZone::system()).date();
        // A sealed file never touches the folder, not even as an upload: that
        // was staged in a private place of its own (see `UploadArea`).
        let folder = if sealed { None } else { Some(self.usable_folder()?) };
        let staging = disk::UploadStaging::for_vault(self.path());
        let staged = |token: &str| match &folder {
            Some(folder) => folder.upload_path(token),
            None => staging.upload_path(token),
        };
        let head = match &source {
            Source::Path(path) => read_head(path)?,
            Source::Bytes(bytes) => bytes[..bytes.len().min(HEAD_LEN as usize)].to_vec(),
            Source::Upload(token) => read_head(&staged(token)?)?,
        };
        let mime = disk::guess_mime(&name, &head);
        let mut entry = new_entry(FileId::new(), opts.origin, &name, mime, now);

        if sealed {
            let (blob, size, hash) = match &source {
                Source::Path(path) => {
                    let blob = self.read(|u| u.store.put_blob_file(path))?;
                    let (size, hash) = hash_file(path)?;
                    (blob, size, hash)
                }
                Source::Bytes(bytes) => {
                    let blob = self.read(|u| u.store.put_blob(bytes))?;
                    let (size, hash) = disk::hash_reader(*bytes)?;
                    (blob, size, hash)
                }
                Source::Upload(token) => {
                    let path = staged(token)?;
                    let blob = self.read(|u| u.store.put_blob_file(&path))?;
                    let (size, hash) = hash_file(&path)?;
                    (blob, size, hash)
                }
            };
            entry.storage = FileStorage::Sealed { blob };
            entry.size = size;
            entry.hash = hash;
            self.with_files(|s| s.put_file(&entry))?;
            if let Source::Upload(token) = &source {
                let _ = staging.cancel_upload(token);
            }
        } else {
            let folder = folder.ok_or_else(|| Error::Invalid("no files folder".into()))?;
            let dir = match &opts.dir {
                Some(dir) => disk::validate_rel(dir)?,
                None => opts.origin.default_dir(today),
            };
            folder.ensure_dir(&dir)?;
            let _place = placing();
            let path = self.free_path(&folder, &dir, &name)?;
            let written = match &source {
                Source::Path(src) => folder.copy_in(src, &path)?,
                Source::Bytes(bytes) => folder.write_bytes(&path, bytes)?,
                Source::Upload(token) => folder.finish_upload(token, &path)?,
            };
            set_plain_path(&mut entry, written.path.clone());
            entry.size = written.size;
            entry.hash = written.hash;
            entry.modified_at = from_micros(written.modified_us);
            if let Err(e) = self.with_files(|s| s.put_file(&entry)) {
                let _ = folder.remove(&written.path);
                return Err(e);
            }
        }
        self.wrote(RecordKind::File, entry.id);
        Ok(entry)
    }

    /// A path in `dir` for a file called `name` that nothing occupies: free
    /// on the disk ([`Folder::unique_path`]) *and* in the catalog. A file
    /// the catalog holds as missing is not on the disk, but its path is
    /// taken: a second record there would be refused.
    pub(super) fn free_path(&self, folder: &Folder, dir: &str, name: &str) -> Result<String> {
        let (stem, ext) = match name.rfind('.') {
            Some(i) if i > 0 => name.split_at(i),
            _ => (name, ""),
        };
        let mut candidate = name.to_string();
        for n in 2..=1000u32 {
            let path = folder.unique_path(dir, &candidate)?;
            if self.with_files(|s| s.file_by_path(&path))?.is_none() {
                return Ok(path);
            }
            candidate = format!("{stem} ({n}){ext}");
        }
        Err(Error::Invalid(format!("could not find a free name for {name:?}")))
    }

    /// The records for `ids`, in that order, each once. An id the catalog
    /// does not hold is [`Error::NotFound`].
    pub(super) fn entries_of(&self, ids: &[FileId]) -> Result<Vec<FileEntry>> {
        let ids = unique(ids);
        let found = self.with_files(|s| s.get_files(&ids))?;
        let mut by_id: HashMap<FileId, FileEntry> = found.into_iter().map(|f| (f.id, f)).collect();
        ids.iter().map(|id| by_id.remove(id).ok_or_else(|| Error::not_found("file", id))).collect()
    }

    /// Every file that is under one of `tops` in a stack.
    ///
    /// The store cannot filter on the stack, so this opens every record. It
    /// is for the deliberate acts -- stacking, unstacking, deleting for
    /// good -- and never for the timeline.
    pub(super) fn stack_members(&self, tops: &[FileId]) -> Result<Vec<FileEntry>> {
        let filter =
            FileFilter { status: StatusFilter::All, include_hidden: true, ..Default::default() };
        let all = self.with_files(|s| s.query_files(&filter))?;
        Ok(all.into_iter().filter(|f| f.stack.is_some_and(|t| tops.contains(&t))).collect())
    }

    /// Move present files into the trash, the disk first and the catalog
    /// after, undoing the disk if the catalog refuses: either every file is
    /// in the trash or none is.
    pub(super) fn trash_entries(&self, mut entries: Vec<FileEntry>) -> Result<Vec<FileEntry>> {
        if entries.is_empty() {
            return Ok(entries);
        }
        let folder =
            if entries.iter().any(|e| !e.is_sealed()) { Some(self.usable_folder()?) } else { None };
        let now = Timestamp::now();
        // (where it was, where it is now), for putting back.
        let mut moved: Vec<(String, String)> = Vec::new();
        let undo = |folder: Option<&Folder>, moved: &[(String, String)]| {
            if let Some(folder) = folder {
                for (from, trashed) in moved.iter().rev() {
                    let _ = folder.restore(trashed, from);
                }
            }
        };
        for entry in &mut entries {
            if let (FileStorage::Plain { path }, Some(folder)) = (&entry.storage, &folder) {
                let from = path.clone();
                match folder.trash(&from, entry.id) {
                    Ok(trashed) => {
                        moved.push((from.clone(), trashed.clone()));
                        entry.storage = FileStorage::Plain { path: trashed };
                        entry.status = FileStatus::Trashed { at: now, from };
                    }
                    Err(e) => {
                        undo(Some(folder), &moved);
                        return Err(e);
                    }
                }
            } else {
                entry.status = FileStatus::Trashed { at: now, from: String::new() };
            }
            entry.updated_at = now;
        }
        if let Err(e) = self.with_files(|s| s.put_files(&entries)) {
            undo(folder.as_ref(), &moved);
            return Err(e);
        }
        for entry in &entries {
            self.wrote(RecordKind::File, entry.id);
        }
        Ok(entries)
    }

    /// Rename or move a folder on the disk, and every catalogued path under
    /// it in one transaction; the disk goes back if the catalog refuses.
    pub(super) fn relocate_folder(&self, from: &str, to: &str) -> Result<()> {
        let folder = self.usable_folder()?;
        let filter = FileFilter {
            under: Some(from.to_string()),
            status: StatusFilter::Live,
            include_hidden: true,
            ..Default::default()
        };
        let rows = self.with_files(|s| s.query_files(&filter))?;
        let prefix = format!("{from}/");
        let _place = placing();
        folder.rename(from, to)?;
        let now = Timestamp::now();
        let mut moved = Vec::with_capacity(rows.len());
        for mut f in rows {
            let Some(rest) = f.path().and_then(|p| p.strip_prefix(&prefix)).map(str::to_string)
            else {
                continue;
            };
            set_plain_path(&mut f, format!("{to}/{rest}"));
            f.updated_at = now;
            moved.push(f);
        }
        if !moved.is_empty()
            && let Err(e) = self.with_files(|s| s.put_files(&moved))
        {
            let _ = folder.rename(to, from);
            return Err(e);
        }
        for f in &moved {
            self.wrote(RecordKind::File, f.id);
        }
        Ok(())
    }

    /// A sealed file's contents into the folder at a free name in `dir`,
    /// through the same staging an upload uses, so a film is never held in
    /// memory and the file appears whole or not at all.
    pub(super) fn blob_to_folder(
        &self,
        folder: &Folder,
        blob: BlobId,
        dir: &str,
        name: &str,
    ) -> Result<Written> {
        let token = new_token();
        let staged = self.blob_to_staging(folder, blob, &token);
        let placed = staged.and_then(|()| {
            let _place = placing();
            let path = self.free_path(folder, dir, name)?;
            folder.finish_upload(&token, &path)
        });
        if placed.is_err() {
            let _ = folder.cancel_upload(&token);
        }
        placed
    }

    fn blob_to_staging(&self, folder: &Folder, blob: BlobId, token: &str) -> Result<()> {
        let len = self.read(|u| u.store.blob_len(blob))?;
        folder.begin_upload(token)?;
        let mut offset = 0;
        while offset < len {
            let step = CHUNK.min(len - offset);
            let chunk = self.read(|u| u.store.get_blob_range(blob, offset, step))?;
            if chunk.is_empty() {
                return Err(Error::Invalid("a stored file is shorter than it says".into()));
            }
            offset = folder.append_upload(token, offset, &chunk)?;
        }
        Ok(())
    }

    /// A sealed file's contents to a new file at `dest`, piece by piece. An
    /// existing file is never overwritten, and a failed copy leaves nothing.
    ///
    /// `private` makes the file readable by its owner alone, from the moment
    /// it exists (never made wide and narrowed after): for a copy that is
    /// only there for another program to open. An export is somebody's own
    /// file in a place they chose, and gets the mode any file of theirs would.
    pub(super) fn blob_to_path(&self, blob: BlobId, dest: &Path, private: bool) -> Result<()> {
        let len = self.read(|u| u.store.blob_len(blob))?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            if private {
                options.mode(0o600);
            }
        }
        #[cfg(not(unix))]
        let _ = private;
        let mut out = match options.open(dest) {
            Ok(out) => out,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(Error::Invalid(format!("{} already exists", dest.display())));
            }
            Err(e) => return Err(Error::io(dest, e)),
        };
        let copied = (|| -> Result<()> {
            let mut offset = 0;
            while offset < len {
                let chunk =
                    self.read(|u| u.store.get_blob_range(blob, offset, CHUNK.min(len - offset)))?;
                if chunk.is_empty() {
                    return Err(Error::Invalid("a stored file is shorter than it says".into()));
                }
                out.write_all(&chunk).map_err(|e| Error::io(dest, e))?;
                offset += chunk.len() as u64;
            }
            out.flush().map_err(|e| Error::io(dest, e))
        })();
        if copied.is_err() {
            drop(out);
            let _ = std::fs::remove_file(dest);
        }
        copied
    }
}
