//! Organising what is already there: renaming, moving, tagging, sealing, trashing.

use super::Vault;
use super::helpers::{
    check_batch, files_under, folder_path, found_entry, from_micros, not_there, once_each,
    put_back, restore_place, set_plain_path, stamp_mtime, unique, validate_file_name,
};
use super::prepare::Prepared;
use super::{ENCRYPTED_DIR, HEAD_LEN, MAX_BATCH, PROP_WAS_AT, placing};
use crate::error::{Error, Result};
use crate::files::disk;
use crate::files::{
    FileEntry, FileFilter, FileStatus, FileStorage, FileUpdate, FolderState, PropValue,
    StatusFilter,
};
use crate::id::FileId;
use crate::record::RecordKind;
use jiff::{SignedDuration, Timestamp};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

impl Vault {
    /// Create `name` inside `dir`. Answers its path.
    pub fn create_folder(&self, dir: &str, name: &str) -> Result<String> {
        self.writable()?;
        let dir = disk::validate_rel(dir)?;
        let name = validate_file_name(name)?;
        let path = disk::join(&dir, &name);
        let folder = self.usable_folder()?;
        if folder.resolve(&path)?.exists() {
            return Err(Error::Invalid(format!("{name:?} is already there")));
        }
        folder.ensure_dir(&path)?;
        Ok(path)
    }

    /// Rename a file in place. A sealed file's name is only its record's.
    pub fn rename_file(&self, id: FileId, name: &str) -> Result<FileEntry> {
        self.writable()?;
        let name = validate_file_name(name)?;
        let mut entry = self.file(id)?;
        match entry.status {
            FileStatus::Present => {}
            FileStatus::Missing { .. } => return Err(not_there(&entry)),
            FileStatus::Trashed { .. } => {
                return Err(Error::Invalid(format!("{} is in the trash", entry.name)));
            }
        }
        match entry.storage.clone() {
            FileStorage::Sealed { .. } => entry.name = name,
            FileStorage::Plain { path } => {
                let (dir, old) = disk::split(&path);
                if old == name {
                    return Ok(entry);
                }
                let to = disk::join(dir, &name);
                let _place = placing();
                if self.with_files(|s| s.file_by_path(&to))?.is_some() {
                    return Err(Error::Invalid(format!("{name:?} is already there")));
                }
                let folder = self.usable_folder()?;
                folder.rename(&path, &to)?;
                set_plain_path(&mut entry, to.clone());
                entry.updated_at = Timestamp::now();
                if let Err(e) = self.with_files(|s| s.put_file(&entry)) {
                    let _ = folder.rename(&to, &path);
                    return Err(e);
                }
                self.wrote(RecordKind::File, id);
                return Ok(entry);
            }
        }
        entry.updated_at = Timestamp::now();
        self.with_files(|s| s.put_file(&entry))?;
        self.wrote(RecordKind::File, id);
        Ok(entry)
    }

    /// Move plain files into `dir`, each under a free name there.
    pub fn move_files(&self, ids: &[FileId], dir: &str) -> Result<Vec<FileEntry>> {
        self.writable()?;
        check_batch(ids.len())?;
        let dir = disk::validate_rel(dir)?;
        let entries = self.entries_of(ids)?;
        for entry in &entries {
            match (&entry.storage, &entry.status) {
                (FileStorage::Sealed { .. }, _) => {
                    return Err(Error::Invalid(format!(
                        "{} is sealed in the vault and has no folder to move out of",
                        entry.name
                    )));
                }
                (_, FileStatus::Missing { .. }) => return Err(not_there(entry)),
                (_, FileStatus::Trashed { .. }) => {
                    return Err(Error::Invalid(format!("{} is in the trash", entry.name)));
                }
                _ => {}
            }
        }
        let folder = self.usable_folder()?;
        folder.ensure_dir(&dir)?;
        let _place = placing();
        let now = Timestamp::now();
        let mut moved = Vec::with_capacity(entries.len());
        for mut entry in entries {
            let Some(from) = entry.path().map(str::to_string) else { continue };
            let (from_dir, name) = disk::split(&from);
            if from_dir == dir {
                moved.push(entry);
                continue;
            }
            let to = self.free_path(&folder, &dir, name)?;
            folder.rename(&from, &to)?;
            set_plain_path(&mut entry, to.clone());
            entry.updated_at = now;
            if let Err(e) = self.with_files(|s| s.put_file(&entry)) {
                let _ = folder.rename(&to, &from);
                return Err(e);
            }
            self.wrote(RecordKind::File, entry.id);
            moved.push(entry);
        }
        Ok(moved)
    }

    /// Rename a folder, and every catalogued path under it. Answers the new
    /// path.
    pub fn rename_folder(&self, path: &str, name: &str) -> Result<String> {
        self.writable()?;
        let path = folder_path(path)?;
        let name = validate_file_name(name)?;
        let to = disk::join(disk::split(&path).0, &name);
        if to == path {
            return Ok(path);
        }
        self.relocate_folder(&path, &to)?;
        Ok(to)
    }

    /// Move a folder into another, and every catalogued path under it.
    /// Refuses moving a folder into itself. Answers the new path.
    pub fn move_folder(&self, path: &str, into: &str) -> Result<String> {
        self.writable()?;
        let path = folder_path(path)?;
        let into = disk::validate_rel(into)?;
        if into == path || into.starts_with(&format!("{path}/")) {
            return Err(Error::Invalid("a folder cannot be moved into itself".into()));
        }
        let to = disk::join(&into, disk::split(&path).1);
        if to == path {
            return Ok(path);
        }
        self.relocate_folder(&path, &to)?;
        Ok(to)
    }

    /// Change what the person added to these files. Tags and people are
    /// trimmed, deduplicated case-insensitively (the first spelling wins)
    /// and checked against the limits; properties against
    /// [`crate::files::valid_property_key`].
    ///
    /// Every id must exist, and nothing is written unless every file takes
    /// the change.
    pub fn update_files(&self, ids: &[FileId], update: &FileUpdate) -> Result<Vec<FileEntry>> {
        self.writable()?;
        check_batch(ids.len())?;
        let prepared = Prepared::new(update)?;
        let mut entries = self.entries_of(ids)?;
        let now = Timestamp::now();
        for entry in &mut entries {
            prepared.apply(entry)?;
            entry.updated_at = now;
        }
        self.with_files(|s| s.put_files(&entries))?;
        for entry in &entries {
            self.wrote(RecordKind::File, entry.id);
        }
        Ok(entries)
    }

    /// Put `others` under `top` in a stack. A file already on top of a stack
    /// brings its members along.
    ///
    /// A `top` that is itself under another file stacks `others` under that
    /// one: a stack is one deep.
    pub fn stack_files(&self, top: FileId, others: &[FileId]) -> Result<()> {
        self.writable()?;
        check_batch(others.len())?;
        let top_entry = self.file(top)?;
        // A stack is one deep: stack under the top of the stack `top` is in.
        let top = match top_entry.stack {
            Some(real) if self.file(real).is_ok() => real,
            _ => top,
        };
        let others: Vec<FileId> = unique(others).into_iter().filter(|o| *o != top).collect();
        if others.is_empty() {
            return Ok(());
        }
        let mut changed = self.entries_of(&others)?;
        changed.extend(self.stack_members(&others)?);
        let mut changed = once_each(changed);
        let now = Timestamp::now();
        for entry in &mut changed {
            entry.stack = Some(top);
            entry.updated_at = now;
        }
        self.with_files(|s| s.put_files(&changed))?;
        for entry in &changed {
            self.wrote(RecordKind::File, entry.id);
        }
        Ok(())
    }

    /// Take these files out of their stacks; unstacking a top frees its
    /// whole stack.
    pub fn unstack_files(&self, ids: &[FileId]) -> Result<()> {
        self.writable()?;
        check_batch(ids.len())?;
        let ids = unique(ids);
        let mut changed: Vec<FileEntry> = once_each(
            self.entries_of(&ids)?
                .into_iter()
                .chain(self.stack_members(&ids)?)
                .filter(|f| f.stack.is_some())
                .collect(),
        );
        let now = Timestamp::now();
        for entry in &mut changed {
            entry.stack = None;
            entry.updated_at = now;
        }
        if changed.is_empty() {
            return Ok(());
        }
        self.with_files(|s| s.put_files(&changed))?;
        for entry in &changed {
            self.wrote(RecordKind::File, entry.id);
        }
        Ok(())
    }

    /// Move files to the trash (a plain file into the folder's own trash).
    ///
    /// A file already in the trash is left as it is. A file that is missing
    /// from the folder has nothing to move, and is refused: forget it
    /// instead.
    pub fn trash_files(&self, ids: &[FileId]) -> Result<Vec<FileEntry>> {
        self.writable()?;
        check_batch(ids.len())?;
        let entries = self.entries_of(ids)?;
        let mut out = Vec::with_capacity(entries.len());
        let mut to_trash = Vec::new();
        for entry in entries {
            match entry.status {
                FileStatus::Present => to_trash.push(entry),
                FileStatus::Missing { .. } => return Err(not_there(&entry)),
                FileStatus::Trashed { .. } => out.push(entry),
            }
        }
        out.extend(self.trash_entries(to_trash)?);
        Ok(out)
    }

    /// Trash every catalogued file under a folder, catalogue and trash
    /// anything there the catalog had not yet seen, then remove the emptied
    /// folders. Answers how many files went to the trash.
    ///
    /// Files the catalog holds as missing stay as they are: they are not in
    /// the folder to be trashed, and what the person added to them is kept
    /// until they say to forget them.
    pub fn trash_folder(&self, path: &str) -> Result<u64> {
        self.writable()?;
        let path = folder_path(path)?;
        let folder = self.usable_folder()?;
        let prefix = format!("{path}/");
        let filter = FileFilter {
            under: Some(path.clone()),
            status: StatusFilter::Present,
            include_hidden: true,
            ..Default::default()
        };
        let mut entries: Vec<FileEntry> = self
            .with_files(|s| s.query_files(&filter))?
            .into_iter()
            .filter(|f| f.path().is_some_and(|p| p.starts_with(&prefix)))
            .collect();
        // What is on disk and not in the catalog yet: the reconciler has
        // not been round, or the person is looking at a folder a sync tool
        // filled a moment ago. Trashing it uncatalogued would lose its
        // place for a restore.
        let known: HashSet<String> = self.file_index()?.into_iter().map(|row| row.path).collect();
        let mut on_disk = Vec::new();
        files_under(&folder, &path, &mut on_disk, 0)?;
        let now = Timestamp::now();
        for item in on_disk.into_iter().filter(|i| !known.contains(&i.path)) {
            let head = folder.read_range(&item.path, 0, HEAD_LEN)?;
            let hash = folder.hash(&item.path)?;
            let name = disk::split(&item.path).1;
            let mime = disk::guess_mime(name, &head);
            entries.push(found_entry(&item.path, item.size, item.modified_us, hash, mime, now));
        }
        // A file that went since it was listed has nothing to move.
        let mut present = Vec::with_capacity(entries.len());
        for entry in entries {
            if let Some(p) = entry.path()
                && folder.stat(p)?.is_some()
            {
                present.push(entry);
            }
        }
        let trashed = self.trash_entries(present)?.len() as u64;
        folder.remove_empty_dirs(&path)?;
        Ok(trashed)
    }

    /// Put trashed files back where they were, under a free name if
    /// something took their place. A file that is not in the trash is left
    /// as it is.
    pub fn restore_files(&self, ids: &[FileId]) -> Result<Vec<FileEntry>> {
        self.writable()?;
        check_batch(ids.len())?;
        let entries = self.entries_of(ids)?;
        let folder = if entries.iter().any(|e| e.status.is_trashed() && !e.is_sealed()) {
            Some(self.usable_folder()?)
        } else {
            None
        };
        let _place = placing();
        let now = Timestamp::now();
        let mut out = Vec::with_capacity(entries.len());
        let mut changed: Vec<FileEntry> = Vec::new();
        // (file, its place in the trash, where it went back to), for putting
        // a failed batch back in the trash.
        let mut moved: Vec<(FileId, String, String)> = Vec::new();
        for mut entry in entries {
            let FileStatus::Trashed { from, .. } = entry.status.clone() else {
                out.push(entry);
                continue;
            };
            if let (FileStorage::Plain { path: trashed }, Some(folder)) =
                (entry.storage.clone(), &folder)
            {
                let (dir, name) = restore_place(&from, &entry.name);
                let placed = folder
                    .ensure_dir(&dir)
                    .and_then(|()| self.free_path(folder, &dir, &name))
                    .and_then(|to| folder.restore(&trashed, &to).map(|()| to));
                match placed {
                    Ok(to) => {
                        moved.push((entry.id, trashed, to.clone()));
                        set_plain_path(&mut entry, to);
                    }
                    Err(e) => {
                        put_back(folder, &moved);
                        return Err(e);
                    }
                }
            }
            entry.status = FileStatus::Present;
            entry.updated_at = now;
            changed.push(entry);
        }
        if let Err(e) = self.with_files(|s| s.put_files(&changed)) {
            if let Some(folder) = &folder {
                put_back(folder, &moved);
            }
            return Err(e);
        }
        for entry in &changed {
            self.wrote(RecordKind::File, entry.id);
        }
        out.extend(changed);
        Ok(out)
    }

    /// Delete trashed files for good: the bytes, the record, its references
    /// and memberships. Refuses a file that is not in the trash.
    ///
    /// A sealed file's contents are a blob that the vault's next sweep
    /// reclaims once nothing refers to it; a plain file's are removed from
    /// the folder now, which needs the folder to be available. Files that
    /// were under a deleted one in a stack are freed.
    pub fn delete_files_forever(&self, ids: &[FileId]) -> Result<u64> {
        self.writable()?;
        check_batch(ids.len())?;
        let ids = unique(ids);
        let entries = self.entries_of(&ids)?;
        if let Some(entry) = entries.iter().find(|e| !e.status.is_trashed()) {
            return Err(Error::Invalid(format!("{} is not in the trash", entry.name)));
        }
        let folder =
            if entries.iter().any(|e| !e.is_sealed()) { Some(self.usable_folder()?) } else { None };
        // Whatever was stacked under a file about to go is on its own now.
        let now = Timestamp::now();
        let mut freed = self.stack_members(&ids)?;
        freed.retain(|f| !ids.contains(&f.id));
        for member in &mut freed {
            member.stack = None;
            member.updated_at = now;
        }
        if !freed.is_empty() {
            self.with_files(|s| s.put_files(&freed))?;
        }
        let mut count = 0;
        for entry in &entries {
            if let (FileStorage::Plain { path }, Some(folder)) = (&entry.storage, &folder) {
                folder.remove(path)?;
            }
            self.with_files(|s| s.delete_file(entry.id))?;
            self.wrote(RecordKind::File, entry.id);
            count += 1;
        }
        Ok(count)
    }

    pub fn empty_files_trash(&self) -> Result<u64> {
        self.writable()?;
        let filter = FileFilter {
            status: StatusFilter::Trashed,
            include_hidden: true,
            ..Default::default()
        };
        let ids: Vec<FileId> =
            self.with_files(|s| s.query_files(&filter))?.into_iter().map(|f| f.id).collect();
        let mut count = 0;
        for chunk in ids.chunks(MAX_BATCH) {
            count += self.delete_files_forever(chunk)?;
        }
        Ok(count)
    }

    /// Delete what has been in the trash longer than the settings keep it.
    /// Does nothing while the folder is unavailable.
    pub fn purge_files_trash(&self, now: Timestamp) -> Result<u64> {
        self.writable()?;
        let days = self.files_settings()?.trash_days;
        if days == 0 || matches!(self.folder_state()?, FolderState::Unavailable { .. }) {
            return Ok(0);
        }
        let Ok(cutoff) = now.checked_sub(SignedDuration::from_hours(24 * i64::from(days))) else {
            return Ok(0);
        };
        let filter = FileFilter {
            status: StatusFilter::Trashed,
            include_hidden: true,
            ..Default::default()
        };
        let expired: Vec<FileId> = self
            .with_files(|s| s.query_files(&filter))?
            .into_iter()
            .filter(|f| matches!(f.status, FileStatus::Trashed { at, .. } if at <= cutoff))
            .map(|f| f.id)
            .collect();
        let mut count = 0;
        for chunk in expired.chunks(MAX_BATCH) {
            count += self.delete_files_forever(chunk)?;
        }
        Ok(count)
    }

    /// Drop the records of missing files. A file that is not missing is
    /// refused: forgetting a present file's record would only have the next
    /// scan catalogue it again, without anything the person had added.
    pub fn forget_missing_files(&self, ids: &[FileId]) -> Result<u64> {
        self.writable()?;
        check_batch(ids.len())?;
        let entries = self.entries_of(ids)?;
        let present = entries.iter().find(|e| !matches!(e.status, FileStatus::Missing { .. }));
        if let Some(entry) = present {
            return Err(Error::Invalid(format!("{} is not missing", entry.name)));
        }
        let mut count = 0;
        for entry in &entries {
            self.with_files(|s| s.delete_file(entry.id))?;
            self.wrote(RecordKind::File, entry.id);
            count += 1;
        }
        Ok(count)
    }

    /// Seal a plain file into the vault (and delete it from the folder), or
    /// write a sealed one out to the folder (into `Encrypted/` unless it
    /// remembers a path) and drop the blob reference.
    pub fn set_file_encrypted(&self, id: FileId, encrypted: bool) -> Result<FileEntry> {
        self.writable()?;
        let before = self.file(id)?;
        if before.is_sealed() == encrypted {
            return Ok(before);
        }
        match before.status {
            FileStatus::Present => {}
            FileStatus::Missing { .. } => return Err(not_there(&before)),
            FileStatus::Trashed { .. } => {
                return Err(Error::Invalid(format!("{} is in the trash", before.name)));
            }
        }
        let mut entry = before.clone();
        let now = Timestamp::now();
        entry.updated_at = now;
        if let FileStorage::Plain { path } = &before.storage {
            let folder = self.usable_folder()?;
            let abs = folder.resolve(path)?;
            let blob = self.read(|u| u.store.put_blob_file(&abs))?;
            entry.storage = FileStorage::Sealed { blob };
            entry.properties.insert(PROP_WAS_AT.into(), PropValue::Text(path.clone()));
            // The record first, then the plain copy goes: the other way
            // round, a failure between them would lose the file.
            self.with_files(|s| s.put_file(&entry))?;
            if let Err(e) = folder.remove(path) {
                let _ = self.with_files(|s| s.put_file(&before));
                return Err(e);
            }
        } else if let FileStorage::Sealed { blob } = &before.storage {
            let folder = self.usable_folder()?;
            let remembered = match before.properties.get(PROP_WAS_AT) {
                Some(PropValue::Text(p)) => disk::validate_rel(p).ok().filter(|p| !p.is_empty()),
                _ => None,
            };
            let (dir, name) = match &remembered {
                Some(path) => {
                    let (d, n) = disk::split(path);
                    (d.to_string(), n.to_string())
                }
                None => (ENCRYPTED_DIR.to_string(), before.name.clone()),
            };
            let name = validate_file_name(&name).unwrap_or_else(|_| before.name.clone());
            folder.ensure_dir(&dir)?;
            let written = self.blob_to_folder(&folder, *blob, &dir, &name)?;
            // Back to the time the file had: it is what a timeline files it
            // under, and what the reconciler will find on disk.
            let modified_us = stamp_mtime(&folder, &written.path, before.modified_at)
                .unwrap_or(written.modified_us);
            set_plain_path(&mut entry, written.path.clone());
            entry.size = written.size;
            entry.hash = written.hash;
            entry.modified_at = from_micros(modified_us);
            entry.properties.remove(PROP_WAS_AT);
            if let Err(e) = self.with_files(|s| s.put_file(&entry)) {
                let _ = folder.remove(&written.path);
                return Err(e);
            }
        }
        self.wrote(RecordKind::File, id);
        Ok(entry)
    }

    /// Copy a file out to `dest` (anywhere on this machine), decrypting a
    /// sealed one. Refuses to overwrite.
    pub fn export_file(&self, id: FileId, dest: &Path) -> Result<()> {
        let entry = self.file(id)?;
        match &entry.storage {
            FileStorage::Plain { path } => self.available_folder()?.copy_out(path, dest),
            FileStorage::Sealed { blob } => self.blob_to_path(*blob, dest, false),
        }
    }

    /// A path another program can open: a plain file's own; a sealed file
    /// decrypted into a private folder of this vault's that is *not* in the
    /// files folder ([`disk::OpenCopies`]).
    ///
    /// A plain file is already in the folder, readable by whoever the folder
    /// is shared with, so opening it from there gives nothing away. A sealed
    /// one is encrypted precisely so that it is not, and the folder may be
    /// synced by a tool that ignores `.stignore`; its plaintext is put
    /// in the user's cache directory instead, in a `0700` folder, as a `0600`
    /// file created fresh.
    pub fn open_file_copy(&self, id: FileId) -> Result<PathBuf> {
        let entry = self.file(id)?;
        let FileStorage::Sealed { blob } = &entry.storage else {
            return self.plain_path(id);
        };
        let name = validate_file_name(&entry.name).unwrap_or_else(|_| "file".to_string());
        // `dir` empties what was there, so the file is always made anew.
        let target = disk::OpenCopies::for_vault(self.path()).dir(id)?.join(name);
        self.blob_to_path(*blob, &target, true)?;
        Ok(target)
    }

    /// Delete what has been lying about for longer than `older_than` of the
    /// plaintext this vault keeps outside the files folder, and the old
    /// build's leftovers: decrypted copies of sealed files, and the staging
    /// files of sealed uploads nobody finished (both in private folders in the
    /// cache directory), and the files folder's own `open` folder, which an
    /// older build used for copies and which is emptied completely, since
    /// nothing legitimate is kept there now. Answers how many things went.
    ///
    /// This vault's own only; [`Vault::sweep_left_behind_plaintext`] looks
    /// wider.
    pub fn sweep_open_copies(&self, older_than: std::time::Duration) -> Result<u32> {
        let mut swept = disk::OpenCopies::for_vault(self.path()).sweep(older_than)?;
        swept += disk::UploadStaging::for_vault(self.path()).sweep(older_than)?;
        if let Ok(Some(folder)) = self.files_folder() {
            swept += folder.sweep_open(std::time::Duration::ZERO).unwrap_or(0);
        }
        Ok(swept)
    }

    /// Delete every decrypted copy of a sealed file and every staged sealed
    /// upload this vault has, however new. Answers how many went.
    ///
    /// Called when the vault locks ([`Vault::lock`]), by the process that
    /// holds the write lock: the folders are shared with whichever process
    /// that is, so a read-only handle must not call it. It needs no key and
    /// no open store -- the folders are named by the vault's path -- so it
    /// works on a vault that is already locked, and the copies cannot outlive
    /// the key. Another program that has a copy open keeps reading it on
    /// unix; on Windows such a file cannot be deleted, and waits for a sweep.
    pub fn remove_plaintext_scratch(&self) -> u32 {
        disk::OpenCopies::for_vault(self.path()).remove_all()
            + disk::UploadStaging::for_vault(self.path()).remove_all()
    }

    /// Clear out the plaintext a run that is gone left behind, as the vault is
    /// opened -- before it is unlocked, which a sweep that needs the catalog
    /// cannot wait for. Answers how many things went.
    ///
    /// Where this process holds the vault's write lock, nobody else has it, so
    /// every decrypted copy and staged upload of it is a leftover of a run that
    /// quit or crashed without locking, and all of them go. Wherever it does
    /// not -- a second window or `jatayu list` beside the real one, whose
    /// copies are live -- nothing of this vault's is touched. And either way
    /// the whole cache directory is swept of what is older than `older_than`
    /// ([`disk::sweep_scratch_everywhere`]), which is how the leftovers of a
    /// vault that has since moved or been deleted are found.
    pub fn sweep_left_behind_plaintext(&self, older_than: std::time::Duration) -> Result<u32> {
        let mut swept = 0;
        if self.is_writable() {
            swept += self.remove_plaintext_scratch();
        }
        Ok(swept + disk::sweep_scratch_everywhere(self.path(), older_than)?)
    }
}
