//! Everything that touches the files folder.
//!
//! Every path a caller hands in is *relative* to the folder, `/`-separated,
//! and goes through [`validate_rel`] before it is joined: no absolute paths,
//! no `..`, no empty segments, nothing under [`INTERNAL_DIR`] unless this
//! module put it there, and -- checked again against the disk in
//! [`Folder::resolve`] -- no symlink that leads out of the folder. A path
//! from the catalog is no more trusted than one from a request: the catalog
//! records what a sync tool wrote.
//!
//! # Writes are whole or absent
//!
//! A file arrives in `INTERNAL_DIR/tmp`, is synced to disk, and is linked
//! into place -- on the same filesystem, so a reader (Syncthing, a photo
//! viewer, the reconciler) sees either nothing or the whole file, never half
//! of a 4 GB film. Nothing here ever overwrites an existing file; a clash is
//! an error, and callers choose a free name with [`Folder::unique_path`]
//! first. A hard link is used because, unlike a rename, it refuses an
//! existing destination atomically; on a filesystem without hard links
//! (FAT, exFAT, some network shares) the fallback is to check the
//! destination is free and rename, which is atomic but leaves a window
//! between the check and the rename.
//!
//! # The trash is inside the folder
//!
//! `INTERNAL_DIR/trash/<file id>/<name>`: a rename, never a copy, so
//! trashing a film is instant and the bytes never leave the folder -- which
//! may be an encrypted volume the person chose for exactly that reason.
//!
//! # The marker
//!
//! `INTERNAL_DIR/folder.json`, written by [`Folder::set_up`]. Its presence is
//! what tells a folder that is there from an empty mount point where a
//! locked encrypted volume would be. See [`Folder::state`].

use super::{FolderState, INTERNAL_DIR, MAX_NAME_LEN};
use crate::error::{Error, Result};
use crate::id::FileId;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The marker's name inside [`INTERNAL_DIR`].
pub const MARKER: &str = "folder.json";

/// Syncthing's ignore file, at the top of a shared folder.
const STIGNORE: &str = ".stignore";

/// The comment above the line [`Folder::ensure_ignored`] writes.
const IGNORE_NOTE: &str = "// Karma Yoga keeps its trash, uploads in progress and decrypted copies \
                           here; they stay on this computer.";

/// Files being written, before they are linked into place.
const TMP: &str = "tmp";
const TRASH: &str = "trash";
/// Uploads in progress, one staging file each.
const UPLOADS: &str = "uploads";
/// Decrypted copies of sealed files, opened in another program.
const OPEN: &str = "open";

/// How much is read, hashed and written at a time. A whole film is never in
/// memory.
const CHUNK: usize = 1 << 20;

/// How many ` (2)`, ` (3)` names [`Folder::unique_path`] tries before it
/// gives up: a folder with thousands of copies of one name has bigger
/// problems.
const MAX_UNIQUE_TRIES: u32 = 10_000;

/// One file as the disk describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskFile {
    /// Relative to the folder, `/`-separated.
    pub path: String,
    pub size: u64,
    /// Microseconds since the epoch.
    pub modified_us: i64,
}

/// One thing directly inside a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirItem {
    pub name: String,
    /// Relative to the folder.
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_us: i64,
    /// For a folder: what is directly inside it, skipped names left out.
    pub items: u32,
}

/// What a write produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// Where it ended up, relative to the folder.
    pub path: String,
    pub size: u64,
    /// BLAKE3 of the contents, lowercase hex.
    pub hash: String,
    pub modified_us: i64,
}

/// A file in `INTERNAL_DIR/tmp` that is removed when this goes out of
/// scope, however that happens: an error half way through a copy, a clash
/// when it is linked into place, or success -- by then the file is linked
/// (or renamed) to its destination, and removing the temporary name is what
/// finishes the job. So no path out of a write leaves anything behind.
struct Staged(PathBuf);

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// The files folder.
#[derive(Debug, Clone)]
pub struct Folder {
    root: PathBuf,
}

impl Folder {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether the folder can be used. `expect_files` is whether the catalog
    /// has plain files in it: a folder that exists without its marker is
    /// unavailable when it does (a mount point with nothing mounted), and
    /// merely not yet marked when it does not.
    ///
    /// Never creates anything.
    pub fn state(&self, expect_files: bool) -> FolderState {
        let path = self.root.display().to_string();
        match self.problem(expect_files) {
            None => FolderState::Available { path },
            Some(reason) => FolderState::Unavailable { path, reason },
        }
    }

    /// Why the folder cannot be used, in words a person can act on; `None`
    /// when it can.
    fn problem(&self, expect_files: bool) -> Option<String> {
        const HELP: &str = "If it is on a drive or an encrypted volume, connect or unlock it.";
        match fs::metadata(&self.root) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Some(format!(
                    "There is a file where the folder should be, not a folder. {HELP}"
                ));
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Some(format!("The folder is not there. {HELP}"));
            }
            Err(e) if e.kind() == ErrorKind::PermissionDenied => {
                return Some(format!("Karma Yoga is not allowed to open the folder. {HELP}"));
            }
            Err(e) => return Some(format!("The folder cannot be opened ({e}). {HELP}")),
        }
        if let Err(e) = fs::read_dir(&self.root) {
            return Some(format!("The folder cannot be read ({e}). {HELP}"));
        }
        match fs::metadata(self.root.join(INTERNAL_DIR).join(MARKER)) {
            Ok(_) => None,
            Err(e) if e.kind() == ErrorKind::NotFound => expect_files.then(|| {
                format!(
                    "The folder is there, but none of Karma Yoga's own files are in it, as \
                     happens with a drive that is not mounted or a volume that is locked. {HELP}"
                )
            }),
            Err(e) => Some(format!("The folder cannot be read ({e}). {HELP}")),
        }
    }

    /// Create the folder if need be, and write the marker and the internal
    /// folders, and make sure Syncthing leaves [`INTERNAL_DIR`] alone
    /// ([`Folder::ensure_ignored`]).
    pub fn set_up(&self) -> Result<()> {
        fs::create_dir_all(&self.root).map_err(|e| Error::io(&self.root, e))?;
        let internal = self.root.join(INTERNAL_DIR);
        for dir in [TMP, TRASH, UPLOADS, OPEN] {
            let dir = internal.join(dir);
            fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        }
        let marker = serde_json::json!({
            "karmayoga": 1,
            "createdAt": jiff::Timestamp::now().to_string(),
        });
        write_if_absent(&internal.join(MARKER), &marker.to_string())?;
        self.ensure_ignored()
    }

    /// Make sure the folder's `.stignore` names [`INTERNAL_DIR`]: written
    /// whole when there is none, and the one line added to the end of one
    /// that lacks it, everything already in it left as it was.
    ///
    /// Not optional, even in somebody else's file. `INTERNAL_DIR` holds
    /// plaintext that must never leave this computer -- a sealed file opened
    /// in another program, an upload on its way to being sealed -- and a
    /// folder already shared with Syncthing would otherwise copy it to every
    /// device the person shares the folder with. Run on every set-up and on
    /// every pass of the reconciler, so a `.stignore` written or replaced
    /// later is put right too.
    pub fn ensure_ignored(&self) -> Result<()> {
        let path = self.root.join(STIGNORE);
        match fs::read_to_string(&path) {
            Ok(text) => {
                if text.lines().any(|line| line.trim() == INTERNAL_DIR) {
                    return Ok(());
                }
                let mut file = fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .map_err(|e| Error::io(&path, e))?;
                let gap = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
                file.write_all(format!("{gap}{IGNORE_NOTE}\n{INTERNAL_DIR}\n").as_bytes())
                    .map_err(|e| Error::io(&path, e))
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                write_if_absent(&path, &format!("{IGNORE_NOTE}\n{INTERNAL_DIR}\n"))
            }
            Err(e) => Err(Error::io(&path, e)),
        }
    }

    /// `rel`, checked by [`validate_rel`], joined to the folder -- and
    /// refused if any existing part of it is a symlink that resolves outside
    /// the folder.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf> {
        let rel = validate_rel(rel)?;
        let mut path = self.root.clone();
        if !rel.is_empty() {
            path.extend(rel.split('/'));
        }
        self.guarded(path, &rel)
    }

    /// `path`, which is the folder's root with segments joined on, if none of
    /// it leads outside the folder.
    ///
    /// The deepest part that exists is resolved all the way (every symlink
    /// followed) and must land inside the folder's own resolved path. What
    /// does not exist yet cannot be a link, so it needs no checking.
    fn guarded(&self, path: PathBuf, rel: &str) -> Result<PathBuf> {
        let root = self.root.canonicalize().map_err(|e| Error::io(&self.root, e))?;
        let mut probe: &Path = &path;
        loop {
            match probe.symlink_metadata() {
                Ok(_) => break,
                Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                    match probe.parent() {
                        Some(parent) => probe = parent,
                        None => break,
                    }
                }
                Err(e) => return Err(Error::io(probe, e)),
            }
        }
        // A symlink to nowhere fails here too, and is refused: where it would
        // lead cannot be known.
        let real = probe.canonicalize().map_err(|e| Error::io(probe, e))?;
        if real.starts_with(&root) {
            Ok(path)
        } else {
            Err(Error::Invalid(format!("{rel:?} leads outside the files folder")))
        }
    }

    /// A path under [`INTERNAL_DIR`], checked like any other: the folder's
    /// own internals are no exception to never following a link out.
    fn internal(&self, parts: &[&str]) -> Result<PathBuf> {
        let mut path = self.root.join(INTERNAL_DIR);
        path.extend(parts.iter().copied());
        self.guarded(path, INTERNAL_DIR)
    }

    /// Where a trashed file is: `INTERNAL_DIR/trash/<id>`.
    fn trash_dir(&self, id: FileId) -> Result<PathBuf> {
        let id = id.to_string();
        self.internal(&[TRASH, id.as_str()])
    }

    /// Like [`Folder::resolve`], but also accepts the path a trashed file has
    /// (see [`Folder::trash`]), for what only reads or deletes: a trashed file
    /// is still one the catalog can ask about.
    fn resolve_any(&self, rel: &str) -> Result<PathBuf> {
        match parse_trashed(rel) {
            Some((id, name)) => self.guarded(self.trash_dir(id)?.join(name), rel),
            None => self.resolve(rel),
        }
    }

    /// A destination for a new file: checked, not the folder itself, and
    /// free. Answers its spelling and where it is.
    fn file_dest(&self, dest: &str) -> Result<(String, PathBuf)> {
        let rel = validate_rel(dest)?;
        if rel.is_empty() {
            return Err(Error::Invalid("a file needs a name".into()));
        }
        let path = self.resolve(&rel)?;
        if path.symlink_metadata().is_ok() {
            return Err(clash(&rel));
        }
        Ok((rel, path))
    }

    /// Create `dir` and its parents. `""` is the folder itself.
    pub fn ensure_dir(&self, dir: &str) -> Result<()> {
        let path = self.resolve(dir)?;
        fs::create_dir_all(&path).map_err(|e| Error::io(&path, e))
    }

    /// A path in `dir` for a file called `name` that nothing occupies: the
    /// name itself, else `name (2).ext`, `name (3).ext` and so on.
    pub fn unique_path(&self, dir: &str, name: &str) -> Result<String> {
        let dir = validate_rel(dir)?;
        let name = validate_name(name)?;
        let dir_path = self.resolve(&dir)?;
        let taken = |candidate: &str| dir_path.join(candidate).symlink_metadata().is_ok();
        if !taken(&name) {
            return Ok(join(&dir, &name));
        }
        let (stem, ext) = match name.rfind('.') {
            Some(i) if i > 0 => name.split_at(i),
            _ => (name.as_str(), ""),
        };
        for n in 2..=MAX_UNIQUE_TRIES {
            let suffix = format!(" ({n})");
            // A name already at the limit has to give up some of its own
            // length to make room.
            let room = MAX_NAME_LEN.saturating_sub(suffix.len() + ext.len());
            let candidate = format!("{}{suffix}{ext}", truncate(stem, room));
            if !taken(&candidate) {
                return Ok(join(&dir, &candidate));
            }
        }
        Err(Error::Invalid(format!("no free name like {name:?} in {dir:?}")))
    }

    /// Copy `src` (anywhere on this machine) to `dest`, streaming, hashing as
    /// it goes; whole or absent (see the module docs). `dest` must be free.
    /// The copy keeps the source's modification time.
    pub fn copy_in(&self, src: &Path, dest: &str) -> Result<Written> {
        let (rel, path) = self.file_dest(dest)?;
        let mut input = File::open(src).map_err(|e| Error::io(src, e))?;
        let meta = input.metadata().map_err(|e| Error::io(src, e))?;
        if !meta.is_file() {
            return Err(Error::Invalid(format!("{} is not a file", src.display())));
        }
        let (staged, mut out) = self.stage()?;
        let (size, hash) = copy_hashing(&mut input, src, &mut out, &staged.0)?;
        // Best effort: not every filesystem lets a time be set, and a copy
        // that is whole matters more than one that is dated.
        if let Ok(modified) = meta.modified() {
            let _ = out.set_modified(modified);
        }
        out.sync_all().map_err(|e| Error::io(&staged.0, e))?;
        drop(out);
        place(&staged.0, &rel, &path)?;
        written(rel, &path, size, hash)
    }

    /// Write `bytes` to `dest`, whole or absent. `dest` must be free.
    pub fn write_bytes(&self, dest: &str, bytes: &[u8]) -> Result<Written> {
        let (rel, path) = self.file_dest(dest)?;
        let (staged, mut out) = self.stage()?;
        out.write_all(bytes).map_err(|e| Error::io(&staged.0, e))?;
        out.sync_all().map_err(|e| Error::io(&staged.0, e))?;
        drop(out);
        place(&staged.0, &rel, &path)?;
        written(rel, &path, bytes.len() as u64, blake3::hash(bytes).to_hex().to_string())
    }

    /// A new, empty file in `INTERNAL_DIR/tmp`, open for writing.
    fn stage(&self) -> Result<(Staged, File)> {
        let dir = self.internal(&[TMP])?;
        fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        let path = dir.join(format!("{}.part", uuid::Uuid::now_v7()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| Error::io(&path, e))?;
        Ok((Staged(path), file))
    }

    // ---- uploads ---------------------------------------------------------

    /// Start (or find again) an upload's staging file,
    /// `INTERNAL_DIR/uploads/<upload>`. Answers the bytes already received.
    /// `upload` is an opaque token of `[A-Za-z0-9_-]`, refused otherwise.
    pub fn begin_upload(&self, upload: &str) -> Result<u64> {
        let path = self.upload_file(upload)?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| Error::io(&path, e))?;
        Ok(file.metadata().map_err(|e| Error::io(&path, e))?.len())
    }

    /// Append `bytes` at `offset`, which must be exactly the bytes received
    /// so far (a repeated chunk at an earlier offset is accepted and
    /// ignored, so a retried request is harmless). Answers the new total.
    pub fn append_upload(&self, upload: &str, offset: u64, bytes: &[u8]) -> Result<u64> {
        // The length is read and the chunk written as one step: a retried
        // chunk arriving while the first copy is still being written would
        // otherwise see the same length, and be appended twice -- an upload
        // longer than it was begun with, which finishing then refuses.
        // Appends to every upload take turns; a chunk is a few megabytes.
        static APPENDING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _turn = APPENDING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = self.upload_file(upload)?;
        let mut file = match OpenOptions::new().append(true).open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(Error::not_found("upload", upload));
            }
            Err(e) => return Err(Error::io(&path, e)),
        };
        let len = file.metadata().map_err(|e| Error::io(&path, e))?.len();
        let size = bytes.len() as u64;
        if offset == len {
            file.write_all(bytes).map_err(|e| Error::io(&path, e))?;
            return Ok(len + size);
        }
        if offset.checked_add(size).is_some_and(|end| end <= len) {
            // The client never saw the answer and sent it again.
            return Ok(len);
        }
        Err(Error::Invalid(format!(
            "this chunk starts at byte {offset}, but {len} bytes have arrived: \
             the next chunk must start at byte {len}"
        )))
    }

    /// Move a finished upload to `dest`, hashing it. `dest` must be free.
    ///
    /// A clash leaves the upload where it is, so the caller can choose
    /// another name and try again without sending the bytes twice.
    pub fn finish_upload(&self, upload: &str, dest: &str) -> Result<Written> {
        let staging = self.upload_file(upload)?;
        let (rel, path) = self.file_dest(dest)?;
        let mut file = match OpenOptions::new().read(true).write(true).open(&staging) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(Error::not_found("upload", upload));
            }
            Err(e) => return Err(Error::io(&staging, e)),
        };
        let (size, hash) = hash_reader(&mut file).map_err(|e| path_err(e, &staging))?;
        file.sync_all().map_err(|e| Error::io(&staging, e))?;
        drop(file);
        place(&staging, &rel, &path)?;
        written(rel, &path, size, hash)
    }

    /// The staging file of an upload, for a caller that seals it instead of
    /// placing it.
    pub fn upload_path(&self, upload: &str) -> Result<PathBuf> {
        let path = self.upload_file(upload)?;
        match path.symlink_metadata() {
            Ok(_) => Ok(path),
            Err(e) if e.kind() == ErrorKind::NotFound => Err(Error::not_found("upload", upload)),
            Err(e) => Err(Error::io(&path, e)),
        }
    }

    pub fn cancel_upload(&self, upload: &str) -> Result<()> {
        let path = self.upload_file(upload)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io(&path, e)),
        }
    }

    /// Where an upload's bytes are kept, once its token has been checked.
    fn upload_file(&self, upload: &str) -> Result<PathBuf> {
        let ok = (1..=64).contains(&upload.len())
            && upload.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !ok {
            return Err(Error::Invalid(
                "an upload is named by 1 to 64 letters, digits, - or _".into(),
            ));
        }
        self.internal(&[UPLOADS, upload])
    }

    /// Delete staging files untouched for longer than `older_than`. Answers
    /// how many.
    ///
    /// Goes by modification time, so an upload that is still receiving
    /// chunks is never swept. Files left in `INTERNAL_DIR/tmp` by a write
    /// that a crash cut short are staging files too, and go the same way.
    pub fn sweep_uploads(&self, older_than: Duration) -> Result<u32> {
        let mut swept = 0;
        for dir in [UPLOADS, TMP] {
            let dir = self.internal(&[dir])?;
            swept += sweep_dir(&dir, older_than, |meta| meta.is_file())?;
        }
        Ok(swept)
    }

    // ---- moving and deleting ---------------------------------------------

    /// Rename or move a file or a folder. Creates `to`'s parent; refuses an
    /// existing `to`.
    pub fn rename(&self, from: &str, to: &str) -> Result<()> {
        let from = validate_rel(from)?;
        let to = validate_rel(to)?;
        if from.is_empty() || to.is_empty() {
            return Err(Error::Invalid("the files folder itself cannot be moved".into()));
        }
        if to.starts_with(&format!("{from}/")) {
            return Err(Error::Invalid(format!("{from:?} cannot be moved into itself")));
        }
        let src = self.resolve(&from)?;
        let dst = self.resolve(&to)?;
        if src.symlink_metadata().is_err() {
            return Err(Error::not_found("file", &from));
        }
        // On a filesystem that ignores case, `A.jpg` is already "there" when
        // it is the file being renamed to `a.jpg`; that is the one clash that
        // is not one.
        let same_file_other_case = from != to
            && from.to_lowercase() == to.to_lowercase()
            && matches!((src.canonicalize(), dst.canonicalize()), (Ok(a), Ok(b)) if a == b);
        if dst.symlink_metadata().is_ok() && !same_file_other_case {
            return Err(clash(&to));
        }
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        fs::rename(&src, &dst).map_err(|e| Error::io(&src, e))
    }

    /// Move `rel` into the trash under `id`. Answers its path there
    /// (`INTERNAL_DIR/trash/<id>/<name>`).
    pub fn trash(&self, rel: &str, id: FileId) -> Result<String> {
        let rel = validate_rel(rel)?;
        let (_, name) = split(&rel);
        if name.is_empty() {
            return Err(Error::Invalid("the files folder itself cannot be trashed".into()));
        }
        let from = self.resolve(&rel)?;
        if from.symlink_metadata().is_err() {
            return Err(Error::not_found("file", &rel));
        }
        let dir = self.trash_dir(id)?;
        let dest = dir.join(name);
        fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        if dest.symlink_metadata().is_ok() {
            return Err(Error::Invalid(format!("{id} is already in the trash")));
        }
        if let Err(e) = fs::rename(&from, &dest) {
            // Nothing was moved; do not leave the empty folder behind.
            let _ = fs::remove_dir(&dir);
            return Err(Error::io(&from, e));
        }
        Ok(format!("{INTERNAL_DIR}/{TRASH}/{id}/{name}"))
    }

    /// Move a trashed file back to `to` (which must be free; the caller picks
    /// it with [`Folder::unique_path`]). Removes the emptied trash folder.
    pub fn restore(&self, trashed: &str, to: &str) -> Result<()> {
        let Some((id, name)) = parse_trashed(trashed) else {
            return Err(Error::Invalid(format!("{trashed:?} is not a path in the trash")));
        };
        let trash_dir = self.trash_dir(id)?;
        let from = self.guarded(trash_dir.join(name), trashed)?;
        if from.symlink_metadata().is_err() {
            return Err(Error::not_found("file", trashed));
        }
        let (_, dest) = self.file_dest(to)?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        fs::rename(&from, &dest).map_err(|e| Error::io(&from, e))?;
        // Only if it is empty, which it now is; `remove_dir` never deletes
        // anything else.
        let _ = fs::remove_dir(&trash_dir);
        if let Some(parent) = dest.parent() {
            sync_dir(parent);
        }
        Ok(())
    }

    /// Delete a file for good. A trashed file's emptied trash folder goes
    /// with it. Deleting what is already gone is not an error.
    ///
    /// Only a file is deleted, or a folder that is itself in the trash: a
    /// folder anywhere else is refused, because a path that is wrong by one
    /// segment should not be able to delete a person's photos.
    pub fn remove(&self, rel: &str) -> Result<()> {
        let trashed = parse_trashed(rel);
        let path = self.resolve_any(rel)?;
        match path.symlink_metadata() {
            Ok(meta) if meta.is_dir() => {
                if trashed.is_none() {
                    return Err(Error::Invalid(format!("{rel:?} is a folder, not a file")));
                }
                fs::remove_dir_all(&path).map_err(|e| Error::io(&path, e))?;
            }
            Ok(_) => match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(Error::io(&path, e)),
            },
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {}
            Err(e) => return Err(Error::io(&path, e)),
        }
        if let Some((id, _)) = trashed {
            let _ = fs::remove_dir(self.trash_dir(id)?);
        }
        Ok(())
    }

    /// Remove `dir` and every folder under it that holds no files, deepest
    /// first. Never removes a file, and never the folder itself.
    ///
    /// Names [`is_skipped`] are left alone, folders included: `.stfolder` is
    /// an empty folder Syncthing will not sync without.
    pub fn remove_empty_dirs(&self, dir: &str) -> Result<()> {
        let dir = validate_rel(dir)?;
        let path = self.resolve(&dir)?;
        prune(&path)?;
        if !dir.is_empty() {
            // Fails, as it should, when something is left in it.
            let _ = fs::remove_dir(&path);
        }
        Ok(())
    }

    // ---- reading ---------------------------------------------------------

    /// `None` when nothing is there. A folder is an error: this is about
    /// files.
    pub fn stat(&self, rel: &str) -> Result<Option<DiskFile>> {
        let path = self.resolve_any(rel)?;
        match fs::metadata(&path) {
            Ok(meta) if meta.is_file() => Ok(Some(DiskFile {
                path: rel.to_string(),
                size: meta.len(),
                modified_us: modified_us(&meta),
            })),
            Ok(_) => Err(Error::Invalid(format!("{rel:?} is a folder, not a file"))),
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                Ok(None)
            }
            Err(e) => Err(Error::io(&path, e)),
        }
    }

    /// Up to `len` bytes from `start`. Fewer at the end of the file.
    pub fn read_range(&self, rel: &str, start: u64, len: u64) -> Result<Vec<u8>> {
        let path = self.resolve_any(rel)?;
        let mut file = open_file(&path, rel)?;
        let meta = file.metadata().map_err(|e| Error::io(&path, e))?;
        if !meta.is_file() {
            return Err(Error::Invalid(format!("{rel:?} is a folder, not a file")));
        }
        let want = len.min(meta.len().saturating_sub(start));
        if want == 0 {
            return Ok(Vec::new());
        }
        file.seek(SeekFrom::Start(start)).map_err(|e| Error::io(&path, e))?;
        // Reserve for what was asked, but not more than is sensible up
        // front: a caller asking for the rest of a 4 GB film gets it, one
        // growth at a time.
        let mut buf = Vec::with_capacity(want.min(16 << 20) as usize);
        file.take(want).read_to_end(&mut buf).map_err(|e| Error::io(&path, e))?;
        Ok(buf)
    }

    /// BLAKE3 of a file's contents, streaming.
    pub fn hash(&self, rel: &str) -> Result<String> {
        let path = self.resolve_any(rel)?;
        let file = open_file(&path, rel)?;
        hash_reader(file).map(|(_, hash)| hash).map_err(|e| path_err(e, &path))
    }

    /// Copy a file out to `dest` (anywhere on this machine), refusing to
    /// overwrite.
    pub fn copy_out(&self, rel: &str, dest: &Path) -> Result<()> {
        let src = self.resolve_any(rel)?;
        let mut input = open_file(&src, rel)?;
        let meta = input.metadata().map_err(|e| Error::io(&src, e))?;
        if !meta.is_file() {
            return Err(Error::Invalid(format!("{rel:?} is a folder, not a file")));
        }
        let mut out = match OpenOptions::new().write(true).create_new(true).open(dest) {
            Ok(out) => out,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                return Err(Error::Invalid(format!("{} already exists", dest.display())));
            }
            Err(e) => return Err(Error::io(dest, e)),
        };
        let copied = copy_hashing(&mut input, &src, &mut out, dest).and_then(|_| {
            if let Ok(modified) = meta.modified() {
                let _ = out.set_modified(modified);
            }
            out.sync_all().map_err(|e| Error::io(dest, e))
        });
        if copied.is_err() {
            // The file was created here, so it is ours to take away again:
            // a half-written copy is worse than none.
            drop(out);
            let _ = fs::remove_file(dest);
        }
        copied
    }

    /// Every file under the folder, depth first and by name within a folder,
    /// skipping what [`is_skipped`] names (at any depth) and never following
    /// a symlink. A folder that vanishes during the walk is skipped, not an
    /// error; so is one that cannot be opened (a `lost+found` that belongs to
    /// root, say), since one such folder must not stop every scan for good.
    /// Names that are not valid UTF-8 cannot be a catalogued path, and are
    /// skipped too.
    pub fn walk(&self) -> Result<Vec<DiskFile>> {
        let mut out = Vec::new();
        walk_dir(&self.root, "", true, &mut out)?;
        Ok(out)
    }

    /// What is directly inside `dir`, skipped names left out, by name
    /// (ignoring case). A folder that is not there lists as empty: the
    /// catalog may still know files that were in it.
    pub fn list_dir(&self, dir: &str) -> Result<Vec<DirItem>> {
        let dir = validate_rel(dir)?;
        let path = self.resolve(&dir)?;
        let entries = match entries(&path) {
            Ok(entries) => entries,
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                return Ok(Vec::new());
            }
            Err(e) => return Err(Error::io(&path, e)),
        };
        let mut items = Vec::with_capacity(entries.len());
        for (name, entry) in entries {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_symlink() || !(kind.is_dir() || kind.is_file()) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            items.push(DirItem {
                path: join(&dir, &name),
                is_dir: kind.is_dir(),
                size: if kind.is_dir() { 0 } else { meta.len() },
                modified_us: modified_us(&meta),
                items: if kind.is_dir() { count_items(&entry.path()) } else { 0 },
                name,
            });
        }
        items.sort_by(|a, b| {
            a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name))
        });
        Ok(items)
    }

    /// A private folder for a decrypted copy of a sealed file opened in
    /// another program: `INTERNAL_DIR/open/<id>/`, created empty.
    pub fn open_dir(&self, id: FileId) -> Result<PathBuf> {
        let id = id.to_string();
        let dir = self.internal(&[OPEN, id.as_str()])?;
        match fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(Error::io(&dir, e)),
        }
        fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        Ok(dir)
    }

    /// Delete decrypted copies older than `older_than`. Answers how many.
    ///
    /// A copy's age is that of the newest thing in it, so one somebody is
    /// still saving changes to is not pulled from under them.
    pub fn sweep_open(&self, older_than: Duration) -> Result<u32> {
        let dir = self.internal(&[OPEN])?;
        sweep_dir(&dir, older_than, |meta| !meta.file_type().is_symlink())
    }
}

/// Remove the entries of `dir` whose newest modification is older than
/// `older_than` and that `wanted` says are ours to remove. Answers how many
/// went. A `dir` that is not there has nothing in it.
fn sweep_dir(
    dir: &Path,
    older_than: Duration,
    wanted: impl Fn(&fs::Metadata) -> bool,
) -> Result<u32> {
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(Error::io(dir, e)),
    };
    let now = SystemTime::now();
    let mut swept = 0;
    for entry in read.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !wanted(&meta) {
            continue;
        }
        let path = entry.path();
        let mut newest = meta.modified().ok();
        if meta.is_dir() {
            for child in fs::read_dir(&path).into_iter().flatten().flatten() {
                let modified = child.metadata().and_then(|m| m.modified()).ok();
                newest = newest.max(modified);
            }
        }
        // A time in the future (a clock that was set back) is not old.
        let old =
            newest.and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age > older_than);
        if !old {
            continue;
        }
        let removed =
            if meta.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
        if removed.is_ok() {
            swept += 1;
        }
    }
    Ok(swept)
}

/// The things directly inside `path` that a walk or a listing does not
/// leave out, by name, with names as text. Names that are not UTF-8 cannot
/// be in a catalogued path, and are left out with the rest.
fn entries(path: &Path) -> std::io::Result<Vec<(String, fs::DirEntry)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = match entry {
            Ok(entry) => entry,
            // Gone between the listing and the reading.
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        if !is_skipped(&name) {
            out.push((name, entry));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// How many things are directly inside a folder, as a listing counts them.
fn count_items(path: &Path) -> u32 {
    let Ok(entries) = entries(path) else { return 0 };
    let count = entries
        .iter()
        .filter(|(_, entry)| entry.file_type().is_ok_and(|kind| !kind.is_symlink()))
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

fn walk_dir(path: &Path, rel: &str, top: bool, out: &mut Vec<DiskFile>) -> Result<()> {
    let entries = match entries(path) {
        Ok(entries) => entries,
        // The folder itself must be readable; one inside it need not be.
        Err(e) if !top && e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) if !top && e.kind() == ErrorKind::PermissionDenied => {
            tracing::warn!(folder = %path.display(), "skipping a folder that cannot be opened");
            return Ok(());
        }
        Err(e) => return Err(Error::io(path, e)),
    };
    for (name, entry) in entries {
        let Ok(kind) = entry.file_type() else { continue };
        let child = join(rel, &name);
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            walk_dir(&entry.path(), &child, false, out)?;
        } else if kind.is_file() {
            // Gone between the listing and the reading.
            let Ok(meta) = entry.metadata() else { continue };
            out.push(DiskFile { path: child, size: meta.len(), modified_us: modified_us(&meta) });
        }
    }
    Ok(())
}

/// Remove, deepest first, the folders under `path` that hold nothing.
fn prune(path: &Path) -> Result<()> {
    let entries = match entries(path) {
        Ok(entries) => entries,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(());
        }
        Err(e) => return Err(Error::io(path, e)),
    };
    for (_, entry) in entries {
        // `file_type` does not follow a link, so a link to a folder is not
        // one here.
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let child = entry.path();
        prune(&child)?;
        // Fails, as it should, when something is left in it.
        let _ = fs::remove_dir(&child);
    }
    Ok(())
}

/// Put a finished staging file at `dest` without overwriting anything. On
/// success `from` is gone; on failure it is left where it was.
///
/// A hard link refuses an existing destination in one atomic step, which a
/// rename does not (it replaces). Where the filesystem has no hard links
/// the fallback checks first and renames, accepting a small window.
fn place(from: &Path, rel: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    match fs::hard_link(from, dest) {
        Ok(()) => {
            // The name in `tmp` was only the means; the bytes are at `dest`.
            let _ = fs::remove_file(from);
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => return Err(clash(rel)),
        Err(_) => {
            if dest.symlink_metadata().is_ok() {
                return Err(clash(rel));
            }
            fs::rename(from, dest).map_err(|e| Error::io(dest, e))?;
        }
    }
    // A rename that is not on disk is one a power cut can undo.
    if let Some(parent) = dest.parent() {
        sync_dir(parent);
    }
    Ok(())
}

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(dir) = File::open(dir) {
        let _ = dir.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

/// What a write answers: the file as the disk now has it, so the time in it
/// is exactly the one a later walk will see.
fn written(rel: String, path: &Path, size: u64, hash: String) -> Result<Written> {
    let meta = fs::metadata(path).map_err(|e| Error::io(path, e))?;
    Ok(Written { path: rel, size, hash, modified_us: modified_us(&meta) })
}

fn clash(rel: &str) -> Error {
    Error::Invalid(format!("{rel:?} already exists"))
}

/// Create `path` with `contents` unless something is already there, in
/// which case it is left exactly as it is.
fn write_if_absent(path: &Path, contents: &str) -> Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file
            .write_all(contents.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|e| Error::io(path, e)),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(Error::io(path, e)),
    }
}

fn open_file(path: &Path, rel: &str) -> Result<File> {
    match File::open(path) {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == ErrorKind::NotFound => Err(Error::not_found("file", rel)),
        Err(e) => Err(Error::io(path, e)),
    }
}

/// Give the error from [`hash_reader`], which knows no path, the one it was
/// reading.
fn path_err(e: Error, path: &Path) -> Error {
    match e {
        Error::RawIo(e) => Error::io(path, e),
        e => e,
    }
}

/// The path a trashed file has: `INTERNAL_DIR/trash/<file id>/<name>`, and
/// nothing else. Answers the id and the name.
fn parse_trashed(rel: &str) -> Option<(FileId, &str)> {
    let rest = rel.strip_prefix(INTERNAL_DIR)?.strip_prefix('/')?.strip_prefix(TRASH)?;
    let (id, name) = rest.strip_prefix('/')?.split_once('/')?;
    // Only the one spelling of an id, so that two paths cannot name one
    // trashed file.
    let parsed = FileId::parse(id).ok().filter(|parsed| parsed.to_string() == id)?;
    (check_segment(name).is_ok() && !name.contains('/')).then_some((parsed, name))
}

/// A file's modification time as microseconds since the epoch; 0 where the
/// platform will not say.
fn modified_us(meta: &fs::Metadata) -> i64 {
    let Ok(modified) = meta.modified() else { return 0 };
    match modified.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_micros()).unwrap_or(i64::MAX),
        Err(before) => i64::try_from(before.duration().as_micros()).map_or(i64::MIN, |us| -us),
    }
}

/// `s` cut to at most `max` bytes, at a character boundary.
fn truncate(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// `~/Karma Yoga`: the folder offered when nobody chose one. `None` when
/// this machine has no home folder to put it in.
///
/// In a test binary, a folder under the temporary directory instead -- the
/// same sandbox `karmayoga_vault` keeps a test's settings in. A test that
/// hands the assistant a picture sets up the default folder as the app
/// would, and must not do it in the home folder of whoever runs the suite.
pub fn default_folder() -> Option<PathBuf> {
    if crate::fsutil::under_test() {
        return Some(
            std::env::temp_dir().join("karmayoga-test-home").join(super::DEFAULT_FOLDER_NAME),
        );
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home).join(super::DEFAULT_FOLDER_NAME))
}

/// Check a relative path and put it in its one spelling: `/`-separated, no
/// leading, trailing or doubled `/`, no `.` or `..` segment, no NUL or
/// backslash, no segment longer than [`super::MAX_NAME_LEN`] bytes, and not
/// under [`INTERNAL_DIR`]. `""` (the folder itself) is allowed.
///
/// On Windows a segment also may not contain `<>:"|?*` or end in a dot or a
/// space, which that system would silently change.
pub fn validate_rel(rel: &str) -> Result<String> {
    if rel.is_empty() {
        return Ok(String::new());
    }
    for segment in rel.split('/') {
        check_segment(segment).map_err(|why| Error::Invalid(format!("{rel:?}: {why}")))?;
    }
    if rel.split('/').next().is_some_and(|first| first.eq_ignore_ascii_case(INTERNAL_DIR)) {
        return Err(Error::Invalid(format!("{rel:?} is where Karma Yoga keeps its own things")));
    }
    Ok(rel.to_string())
}

/// What is wrong with one segment of a path, if anything.
fn check_segment(segment: &str) -> std::result::Result<(), &'static str> {
    if segment.is_empty() {
        Err("it has an empty part (a leading, trailing or doubled /)")
    } else if segment == "." || segment == ".." {
        Err("it has a . or .. part")
    } else if segment.contains(['\0', '\\']) {
        Err("it has a NUL or a backslash")
    } else if segment.len() > MAX_NAME_LEN {
        Err("a name in it is too long")
    } else if cfg!(windows) && windows_unsafe(segment) {
        Err("a name in it has a character or an ending Windows does not allow")
    } else {
        Ok(())
    }
}

/// Characters Windows refuses in a name, and the trailing dots and spaces it
/// quietly strips (so the name on disk would not be the one asked for).
fn windows_unsafe(name: &str) -> bool {
    name.contains(['<', '>', ':', '"', '|', '?', '*']) || name.ends_with(['.', ' '])
}

/// Check one file or folder name: not empty, not `.` or `..`, no `/`, `\`
/// or NUL, no control characters, at most [`super::MAX_NAME_LEN`] bytes,
/// not [`INTERNAL_DIR`]. Trims surrounding whitespace.
pub fn validate_name(name: &str) -> Result<String> {
    let name = name.trim();
    let bad = |why: &str| Error::Invalid(format!("{name:?} is not a usable name: {why}"));
    if name.is_empty() {
        Err(bad("it is empty"))
    } else if name == "." || name == ".." {
        Err(bad("it is a . or .."))
    } else if name.contains(['/', '\\', '\0']) {
        Err(bad("it has a / or a backslash in it"))
    } else if name.chars().any(char::is_control) {
        Err(bad("it has a control character in it"))
    } else if name.len() > MAX_NAME_LEN {
        Err(bad("it is too long"))
    } else if name.eq_ignore_ascii_case(INTERNAL_DIR) {
        Err(bad("Karma Yoga keeps its own things under that name"))
    } else if cfg!(windows) && windows_unsafe(name) {
        Err(bad("it has a character or an ending Windows does not allow"))
    } else {
        Ok(name.to_string())
    }
}

/// Join a folder and a name: `("", "a.jpg")` is `a.jpg`.
pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") }
}

/// Split a path into its folder and its name: `a/b/c.jpg` is
/// `("a/b", "c.jpg")`, `c.jpg` is `("", "c.jpg")`.
pub fn split(rel: &str) -> (&str, &str) {
    rel.rsplit_once('/').unwrap_or(("", rel))
}

/// Whether a walk or a listing leaves a name out, at any depth:
/// [`INTERNAL_DIR`]; Syncthing's own (`.stfolder`, `.stversions`,
/// `.stignore`, `~syncthing~*.tmp`, `.syncthing.*.tmp`); other sync tools'
/// partial files (`*.part`, `*.crdownload`, `.~lock.*#`); the operating
/// systems' litter (`.DS_Store`, `Thumbs.db`, `desktop.ini`, `.Trash-*`,
/// `.directory`); and every other name starting with a dot.
pub fn is_skipped(name: &str) -> bool {
    // Everything dotted -- the lot of Syncthing's and the desktops' own
    // names -- is one rule; the rest are the names that cannot be told apart
    // by their first character.
    if name.starts_with('.') || name == INTERNAL_DIR {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    matches!(lower.as_str(), "thumbs.db" | "desktop.ini")
        || (lower.starts_with("~syncthing~") && lower.ends_with(".tmp"))
        || lower.ends_with(".part")
        || lower.ends_with(".crdownload")
}

/// A MIME type for a file, from its first bytes when they say (see
/// [`crate::media::sniff_mime`]), else its extension, else
/// `application/octet-stream`.
///
/// The bytes win except where they cannot be trusted: a two-letter `BM`, an
/// XML prolog and an MP3 frame sync all begin plenty of files that are
/// something else, so for those the extension decides when it has a say. An
/// XML prolog under any other extension is XML, not an image: a GPX track
/// must not be taken for a picture.
pub fn guess_mime(name: &str, head: &[u8]) -> String {
    const OPAQUE: &str = "application/octet-stream";
    let sniffed = crate::media::sniff_mime(head);
    let by_extension = mime_from_extension(name);
    let mime = match (sniffed, by_extension) {
        (OPAQUE, by_extension) => by_extension.unwrap_or(OPAQUE),
        ("image/bmp" | "image/svg+xml" | "audio/mpeg", Some(by_extension)) => by_extension,
        ("image/svg+xml", None) if head.starts_with(b"<?xml") => "application/xml",
        (sniffed, _) => sniffed,
    };
    mime.to_string()
}

/// The type an extension stands for, when there is a well-known one.
fn mime_from_extension(name: &str) -> Option<&'static str> {
    let (_, ext) = name.rsplit_once('.')?;
    Some(match ext.to_ascii_lowercase().as_str() {
        // Pictures.
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        // Cameras' RAW formats.
        "dng" => "image/x-adobe-dng",
        "cr2" => "image/x-canon-cr2",
        "cr3" => "image/x-canon-cr3",
        "nef" => "image/x-nikon-nef",
        "arw" => "image/x-sony-arw",
        "orf" => "image/x-olympus-orf",
        "rw2" => "image/x-panasonic-rw2",
        "raf" => "image/x-fuji-raf",
        "pef" => "image/x-pentax-pef",
        "srw" => "image/x-samsung-srw",
        // Films.
        "mp4" => "video/mp4",
        "m4v" => "video/x-m4v",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "3gp" => "video/3gpp",
        "mts" | "m2ts" => "video/mp2t",
        "mpg" | "mpeg" => "video/mpeg",
        "wmv" => "video/x-ms-wmv",
        "ogv" => "video/ogg",
        // Sound.
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "aiff" | "aif" => "audio/aiff",
        "wma" => "audio/x-ms-wma",
        // Text.
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "xml" => "application/xml",
        "yaml" | "yml" => "text/yaml",
        "toml" => "application/toml",
        "rtf" => "application/rtf",
        // Office and e-books.
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "epub" => "application/epub+zip",
        // Archives.
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        _ => return None,
    })
}

/// BLAKE3 of everything `r` yields, lowercase hex, and how many bytes that
/// was.
pub fn hash_reader(mut r: impl Read) -> Result<(u64, String)> {
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        let n = read_some(&mut r, &mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((total, hasher.finalize().to_hex().to_string()))
}

/// Copy `input` to `out` a chunk at a time, hashing what goes through. The
/// paths are only for saying which of the two failed.
fn copy_hashing(
    input: &mut impl Read,
    input_path: &Path,
    out: &mut impl Write,
    out_path: &Path,
) -> Result<(u64, String)> {
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        let n = read_some(input, &mut buf).map_err(|e| Error::io(input_path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| Error::io(out_path, e))?;
        total += n as u64;
    }
    Ok((total, hasher.finalize().to_hex().to_string()))
}

/// One read, retried when a signal interrupts it.
fn read_some(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        match r.read(buf) {
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A folder that has been set up, inside a temporary directory that is
    /// also where "elsewhere on this machine" is.
    fn folder() -> (TempDir, Folder) {
        let tmp = tempfile::tempdir().unwrap();
        let folder = Folder::new(tmp.path().join("Karma Yoga"));
        folder.set_up().unwrap();
        (tmp, folder)
    }

    fn put(folder: &Folder, rel: &str, bytes: &[u8]) {
        let path = folder.root().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn names(files: &[DiskFile]) -> Vec<&str> {
        files.iter().map(|f| f.path.as_str()).collect()
    }

    fn in_tmp(folder: &Folder) -> usize {
        fs::read_dir(folder.root().join(INTERNAL_DIR).join(TMP)).unwrap().count()
    }

    /// Make `path` look as if it was last touched `days` ago.
    fn backdate(path: &Path, days: u64) {
        let file = if path.is_dir() {
            File::open(path).unwrap()
        } else {
            OpenOptions::new().write(true).open(path).unwrap()
        };
        file.set_modified(SystemTime::now() - Duration::from_secs(days * 86_400)).unwrap();
    }

    // ---- paths ------------------------------------------------------------

    #[test]
    fn relative_paths_are_checked_and_kept_as_they_are() {
        for ok in ["", "a.jpg", "a/b/c.jpg", "Photos/2024/IMG 1 (2).jpg", "a/.hidden", "é/ü.txt"]
        {
            assert_eq!(validate_rel(ok).unwrap(), ok);
        }
        let too_long = "x".repeat(MAX_NAME_LEN + 1);
        for bad in [
            "/a",
            "a/",
            "a//b",
            ".",
            "..",
            "a/./b",
            "a/../b",
            "../a",
            "a\\b",
            "a\0b",
            ".karmayoga",
            ".karmayoga/trash/x",
            ".KarmaYoga/tmp",
            too_long.as_str(),
            "a/\0",
        ] {
            assert!(
                matches!(validate_rel(bad), Err(Error::Invalid(_))),
                "{bad:?} should be refused"
            );
        }
        // At the limit is fine, and `.karmayoga` deeper down is just a name.
        assert!(validate_rel(&"x".repeat(MAX_NAME_LEN)).is_ok());
        assert!(validate_rel("a/.karmayoga").is_ok());
    }

    #[test]
    fn names_are_trimmed_and_checked() {
        assert_eq!(validate_name("  a.jpg ").unwrap(), "a.jpg");
        assert_eq!(validate_name(".hidden").unwrap(), ".hidden");
        for bad in
            ["", "   ", ".", "..", "a/b", "a\\b", "a\0b", "tab\there", "line\nbreak", ".karmayoga"]
        {
            assert!(matches!(validate_name(bad), Err(Error::Invalid(_))), "{bad:?}");
        }
        assert!(validate_name(".KARMAYOGA").is_err());
        assert!(validate_name(&"x".repeat(MAX_NAME_LEN + 1)).is_err());
        assert!(validate_name(&"x".repeat(MAX_NAME_LEN)).is_ok());
    }

    #[test]
    fn windows_refuses_what_it_would_quietly_change() {
        for bad in ["a:b", "a<b", "a>b", "a\"b", "a|b", "a?b", "a*b", "name.", "name "] {
            assert!(windows_unsafe(bad), "{bad:?}");
        }
        for ok in ["a.jpg", "a b", ".hidden", "a-b_c (2).txt"] {
            assert!(!windows_unsafe(ok), "{ok:?}");
        }
    }

    #[test]
    fn join_and_split_are_inverses() {
        assert_eq!(join("", "a.jpg"), "a.jpg");
        assert_eq!(join("a/b", "c.jpg"), "a/b/c.jpg");
        assert_eq!(split("a/b/c.jpg"), ("a/b", "c.jpg"));
        assert_eq!(split("c.jpg"), ("", "c.jpg"));
    }

    #[test]
    fn the_folders_own_and_the_systems_litter_are_skipped() {
        for skipped in [
            ".karmayoga",
            ".stfolder",
            ".stversions",
            ".stignore",
            ".syncthing.photo.jpg.tmp",
            "~syncthing~photo.jpg.tmp",
            ".~lock.report.odt#",
            ".DS_Store",
            "Thumbs.db",
            "thumbs.db",
            "desktop.ini",
            ".Trash-1000",
            ".directory",
            ".anything-else",
            "film.mp4.part",
            "FILM.MP4.PART",
            "setup.exe.crdownload",
        ] {
            assert!(is_skipped(skipped), "{skipped:?} should be skipped");
        }
        for kept in ["photo.jpg", "notes.txt", "part", "partial.mp4", "syncthing.tmp", "a.part.jpg"]
        {
            assert!(!is_skipped(kept), "{kept:?} should be kept");
        }
    }

    #[test]
    fn mime_types_come_from_the_bytes_then_the_extension() {
        // The bytes win over a wrong name.
        assert_eq!(guess_mime("holiday.bin", b"\x89PNG\r\n\x1a\n...."), "image/png");
        assert_eq!(guess_mime("scan.txt", &[0xFF, 0xD8, 0xFF, 0xE0]), "image/jpeg");
        assert_eq!(guess_mime("clip", b"____ftypqt  ...."), "video/quicktime");
        // The name when the bytes say nothing.
        for (name, mime) in [
            ("a.jpg", "image/jpeg"),
            ("A.JPEG", "image/jpeg"),
            ("a.heic", "image/heic"),
            ("a.heif", "image/heif"),
            ("a.avif", "image/avif"),
            ("a.dng", "image/x-adobe-dng"),
            ("a.CR3", "image/x-canon-cr3"),
            ("a.nef", "image/x-nikon-nef"),
            ("a.mov", "video/quicktime"),
            ("a.mp4", "video/mp4"),
            ("a.m4v", "video/x-m4v"),
            ("a.mkv", "video/x-matroska"),
            ("a.webm", "video/webm"),
            ("a.avi", "video/x-msvideo"),
            ("a.3gp", "video/3gpp"),
            ("a.mp3", "audio/mpeg"),
            ("a.m4a", "audio/mp4"),
            ("a.flac", "audio/flac"),
            ("a.pdf", "application/pdf"),
            ("a.txt", "text/plain"),
            ("a.md", "text/markdown"),
            ("a.csv", "text/csv"),
            ("a.json", "application/json"),
            ("a.html", "text/html"),
            ("a.xml", "application/xml"),
            ("a.yaml", "text/yaml"),
            ("a.yml", "text/yaml"),
            ("a.docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
            ("a.odt", "application/vnd.oasis.opendocument.text"),
            ("a.epub", "application/epub+zip"),
            ("a.zip", "application/zip"),
        ] {
            assert_eq!(guess_mime(name, b""), mime, "{name}");
        }
        assert_eq!(guess_mime("README", b"hello"), "application/octet-stream");
        assert_eq!(guess_mime("a.unknownext", b"hello"), "application/octet-stream");
        assert_eq!(guess_mime("trailing.", b""), "application/octet-stream");
        assert_eq!(guess_mime(".bashrc", b""), "application/octet-stream");
    }

    #[test]
    fn a_weak_signature_does_not_outvote_the_extension() {
        // A note that happens to begin "BM".
        assert_eq!(guess_mime("bmw.txt", b"BMW service history"), "text/plain");
        // A real bitmap, and one with no useful name.
        assert_eq!(guess_mime("a.bmp", b"BM\x36\x00"), "image/bmp");
        assert_eq!(guess_mime("a", b"BM\x36\x00"), "image/bmp");
        // XML is not a picture unless it says it is one.
        assert_eq!(guess_mime("track.gpx", b"<?xml version=\"1.0\"?>"), "application/xml");
        assert_eq!(guess_mime("data.xml", b"<?xml version=\"1.0\"?>"), "application/xml");
        assert_eq!(guess_mime("logo.svg", b"<?xml version=\"1.0\"?><svg>"), "image/svg+xml");
        assert_eq!(guess_mime("logo", b"<svg xmlns=\"x\">"), "image/svg+xml");
    }

    #[test]
    fn hashing_a_reader_agrees_with_hashing_the_bytes() {
        // Longer than one chunk, so the loop turns more than once.
        let bytes: Vec<u8> = (0..(CHUNK * 2 + 123)).map(|i| (i % 251) as u8).collect();
        let (n, hash) = hash_reader(&bytes[..]).unwrap();
        assert_eq!(n, bytes.len() as u64);
        assert_eq!(hash, blake3::hash(&bytes).to_hex().to_string());
        let (n, hash) = hash_reader(&b""[..]).unwrap();
        assert_eq!(n, 0);
        assert_eq!(hash, blake3::hash(b"").to_hex().to_string());
    }

    // ---- the folder and its state -----------------------------------------

    #[test]
    fn a_folder_that_is_not_there_is_unavailable_and_says_what_to_do() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = Folder::new(tmp.path().join("gone"));
        for expect in [true, false] {
            let FolderState::Unavailable { path, reason } = folder.state(expect) else {
                panic!("a folder that is not there is not available");
            };
            assert_eq!(path, folder.root().display().to_string());
            assert!(reason.contains("not there"), "{reason}");
            assert!(reason.contains("connect or unlock it"), "{reason}");
        }
        // And looking never creates it.
        assert!(!folder.root().exists());
    }

    #[test]
    fn a_file_where_the_folder_should_be_is_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("Karma Yoga");
        fs::write(&path, b"not a folder").unwrap();
        let FolderState::Unavailable { reason, .. } = Folder::new(&path).state(false) else {
            panic!("a file is not a folder");
        };
        assert!(reason.contains("not a folder"), "{reason}");
    }

    #[test]
    fn a_folder_without_its_marker_is_unavailable_only_when_files_are_expected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("mount");
        fs::create_dir(&root).unwrap();
        let folder = Folder::new(&root);
        // A mount point with nothing mounted looks exactly like this.
        let FolderState::Unavailable { reason, .. } = folder.state(true) else {
            panic!("an unmarked folder where files are expected is unavailable");
        };
        assert!(reason.contains("connect or unlock it"), "{reason}");
        // Not yet used: fine, and the caller sets it up.
        assert!(folder.state(false).is_available());
        // The folder is still untouched either way.
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        folder.set_up().unwrap();
        assert!(folder.state(true).is_available());
        assert!(folder.state(false).is_available());
        assert_eq!(folder.state(true), FolderState::Available { path: root.display().to_string() });
    }

    #[test]
    fn set_up_makes_the_marker_the_internal_folders_and_a_stignore() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("deep").join("er").join("Karma Yoga");
        let folder = Folder::new(&root);
        folder.set_up().unwrap();
        for dir in [TMP, TRASH, UPLOADS, OPEN] {
            assert!(root.join(INTERNAL_DIR).join(dir).is_dir(), "{dir}");
        }
        let marker: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join(INTERNAL_DIR).join(MARKER)).unwrap())
                .unwrap();
        assert_eq!(marker["karmayoga"], 1);
        let created = marker["createdAt"].as_str().unwrap();
        assert!(created.parse::<jiff::Timestamp>().is_ok(), "{created}");
        let stignore = fs::read_to_string(root.join(".stignore")).unwrap();
        assert!(stignore.lines().next().unwrap().starts_with("//"), "{stignore}");
        assert!(stignore.lines().any(|l| l == INTERNAL_DIR), "{stignore}");
    }

    #[test]
    fn set_up_twice_changes_nothing_and_keeps_what_a_stignore_already_says() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Karma Yoga");
        fs::create_dir(&root).unwrap();
        // The person already has a .stignore, without a final newline.
        fs::write(root.join(".stignore"), "*.tmp").unwrap();
        let folder = Folder::new(&root);
        folder.set_up().unwrap();
        let marker = fs::read(root.join(INTERNAL_DIR).join(MARKER)).unwrap();
        let after_first = fs::read_to_string(root.join(".stignore")).unwrap();
        folder.set_up().unwrap();
        assert_eq!(fs::read(root.join(INTERNAL_DIR).join(MARKER)).unwrap(), marker);
        // Theirs is kept, line for line, and the internal folder is added
        // once -- the plaintext in it must never be shared.
        let stignore = fs::read_to_string(root.join(".stignore")).unwrap();
        assert_eq!(stignore, after_first, "a second set-up adds nothing");
        assert!(stignore.starts_with("*.tmp\n"), "{stignore}");
        assert_eq!(stignore.lines().filter(|l| *l == INTERNAL_DIR).count(), 1, "{stignore}");
    }

    // ---- resolve ----------------------------------------------------------

    #[test]
    fn resolve_joins_inside_the_folder_and_refuses_what_validation_refuses() {
        let (_tmp, folder) = folder();
        assert_eq!(folder.resolve("").unwrap(), folder.root());
        assert_eq!(folder.resolve("a/b.txt").unwrap(), folder.root().join("a").join("b.txt"));
        assert!(folder.resolve("../x").is_err());
        assert!(folder.resolve("/etc/passwd").is_err());
        assert!(folder.resolve(".karmayoga/folder.json").is_err());
    }

    #[test]
    fn resolve_needs_the_folder_to_be_there() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = Folder::new(tmp.path().join("gone"));
        assert!(folder.resolve("a.txt").is_err());
        // Which also means nothing creates a folder by accident.
        assert!(folder.ensure_dir("a").is_err());
        assert!(folder.write_bytes("a.txt", b"x").is_err());
        assert!(!folder.root().exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_folder_is_refused_however_deep() {
        use std::os::unix::fs::symlink;
        let (tmp, folder) = folder();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), b"secret").unwrap();
        symlink(&outside, folder.root().join("link")).unwrap();
        symlink(outside.join("secret.txt"), folder.root().join("file-link")).unwrap();
        symlink(tmp.path().join("nowhere"), folder.root().join("dangling")).unwrap();

        for rel in ["link", "link/secret.txt", "link/new.txt", "link/a/b/c.txt", "file-link"] {
            assert!(
                matches!(folder.resolve(rel), Err(Error::Invalid(_))),
                "{rel} leads outside the folder"
            );
        }
        assert!(folder.resolve("dangling").is_err());
        // Nothing reached through it either.
        assert!(folder.read_range("link/secret.txt", 0, 10).is_err());
        assert!(folder.hash("file-link").is_err());
        assert!(folder.write_bytes("link/new.txt", b"x").is_err());
        assert!(!outside.join("new.txt").exists());
        assert!(folder.copy_in(&outside.join("secret.txt"), "link/copy.txt").is_err());
        assert!(folder.rename("link/secret.txt", "taken.txt").is_err());
        assert!(folder.ensure_dir("link/sub").is_err());
        assert!(!outside.join("sub").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_that_stays_inside_the_folder_is_fine() {
        use std::os::unix::fs::symlink;
        let (_tmp, folder) = folder();
        put(&folder, "real/a.txt", b"hello");
        symlink(folder.root().join("real"), folder.root().join("alias")).unwrap();
        assert!(folder.resolve("alias/a.txt").is_ok());
        assert_eq!(folder.read_range("alias/a.txt", 0, 10).unwrap(), b"hello");
    }

    #[test]
    fn ensure_dir_creates_parents_and_accepts_the_folder_itself() {
        let (_tmp, folder) = folder();
        folder.ensure_dir("").unwrap();
        folder.ensure_dir("a/b/c").unwrap();
        folder.ensure_dir("a/b/c").unwrap();
        assert!(folder.root().join("a/b/c").is_dir());
        assert!(folder.ensure_dir("..").is_err());
        assert!(folder.ensure_dir(".karmayoga/x").is_err());
    }

    #[test]
    fn unique_path_numbers_before_the_extension() {
        let (_tmp, folder) = folder();
        assert_eq!(folder.unique_path("", "a.jpg").unwrap(), "a.jpg");
        put(&folder, "a.jpg", b"1");
        assert_eq!(folder.unique_path("", "a.jpg").unwrap(), "a (2).jpg");
        put(&folder, "a (2).jpg", b"2");
        assert_eq!(folder.unique_path("", "a.jpg").unwrap(), "a (3).jpg");
        // In a folder, with a name that needs trimming, and with no extension.
        put(&folder, "Photos/README", b"x");
        assert_eq!(folder.unique_path("Photos", " README ").unwrap(), "Photos/README (2)");
        assert_eq!(folder.unique_path("Photos", "new.txt").unwrap(), "Photos/new.txt");
        // A folder that is not there yet is simply free.
        assert_eq!(folder.unique_path("Not/yet", "a.jpg").unwrap(), "Not/yet/a.jpg");
        // Several dots: only the last is the extension. A leading dot is not.
        put(&folder, "a.tar.gz", b"x");
        assert_eq!(folder.unique_path("", "a.tar.gz").unwrap(), "a.tar (2).gz");
        put(&folder, ".profile", b"x");
        assert_eq!(folder.unique_path("", ".profile").unwrap(), ".profile (2)");
        // A folder in the way counts as taken.
        folder.ensure_dir("dir").unwrap();
        assert_eq!(folder.unique_path("", "dir").unwrap(), "dir (2)");
        assert!(folder.unique_path("", "..").is_err());
        assert!(folder.unique_path("../x", "a").is_err());
    }

    #[test]
    fn unique_path_shortens_a_name_that_is_already_as_long_as_allowed() {
        let (_tmp, folder) = folder();
        let long = format!("{}.jpg", "x".repeat(MAX_NAME_LEN - 4));
        assert_eq!(long.len(), MAX_NAME_LEN);
        put(&folder, &long, b"1");
        let next = folder.unique_path("", &long).unwrap();
        assert!(next.len() <= MAX_NAME_LEN, "{}", next.len());
        assert!(next.ends_with(" (2).jpg"), "{next}");
        assert!(validate_rel(&next).is_ok());
    }

    // ---- writing ----------------------------------------------------------

    #[test]
    fn copy_in_hashes_as_it_goes_and_keeps_the_sources_time() {
        let (tmp, folder) = folder();
        let src = tmp.path().join("IMG_0001.jpg");
        // More than one chunk.
        let bytes: Vec<u8> = (0..(CHUNK + 4321)).map(|i| (i % 253) as u8).collect();
        fs::write(&src, &bytes).unwrap();
        let taken = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        OpenOptions::new().write(true).open(&src).unwrap().set_modified(taken).unwrap();

        let written = folder.copy_in(&src, "Photos/2020/IMG_0001.jpg").unwrap();
        assert_eq!(written.path, "Photos/2020/IMG_0001.jpg");
        assert_eq!(written.size, bytes.len() as u64);
        assert_eq!(written.hash, blake3::hash(&bytes).to_hex().to_string());
        assert_eq!(written.modified_us, 1_600_000_000_000_000);
        assert_eq!(fs::read(folder.root().join("Photos/2020/IMG_0001.jpg")).unwrap(), bytes);
        // The source is untouched, and the same time is what a walk reports.
        assert!(src.exists());
        let walked = folder.walk().unwrap();
        assert_eq!(walked.len(), 1);
        assert_eq!(walked[0].modified_us, written.modified_us);
        assert_eq!(in_tmp(&folder), 0, "nothing is left in tmp");
    }

    #[test]
    fn copy_in_never_overwrites_and_leaves_nothing_behind() {
        let (tmp, folder) = folder();
        put(&folder, "a.txt", b"mine");
        let src = tmp.path().join("other.txt");
        fs::write(&src, b"theirs").unwrap();

        let e = folder.copy_in(&src, "a.txt").unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{e}");
        assert_eq!(fs::read(folder.root().join("a.txt")).unwrap(), b"mine");
        assert_eq!(in_tmp(&folder), 0);

        // A missing source, a folder as source, and a bad destination.
        assert!(folder.copy_in(&tmp.path().join("nope"), "b.txt").is_err());
        assert!(folder.copy_in(tmp.path(), "b.txt").is_err());
        assert!(folder.copy_in(&src, "").is_err());
        assert!(folder.copy_in(&src, "../b.txt").is_err());
        assert!(folder.copy_in(&src, ".karmayoga/x").is_err());
        assert!(!folder.root().join("b.txt").exists());
        assert_eq!(in_tmp(&folder), 0);
    }

    #[test]
    fn write_bytes_is_whole_or_absent_and_never_overwrites() {
        let (_tmp, folder) = folder();
        let written = folder.write_bytes("Notes/a.md", b"# hello").unwrap();
        assert_eq!(written.path, "Notes/a.md");
        assert_eq!(written.size, 7);
        assert_eq!(written.hash, blake3::hash(b"# hello").to_hex().to_string());
        assert_eq!(fs::read(folder.root().join("Notes/a.md")).unwrap(), b"# hello");
        let now = jiff::Timestamp::now().as_microsecond();
        assert!((now - written.modified_us).abs() < 60_000_000);

        assert!(matches!(folder.write_bytes("Notes/a.md", b"other"), Err(Error::Invalid(_))));
        assert_eq!(fs::read(folder.root().join("Notes/a.md")).unwrap(), b"# hello");
        // Empty files are files.
        assert_eq!(folder.write_bytes("empty", b"").unwrap().size, 0);
        assert_eq!(in_tmp(&folder), 0);
    }

    // ---- uploads ----------------------------------------------------------

    #[test]
    fn an_upload_is_appended_in_order_and_finished() {
        let (_tmp, folder) = folder();
        assert_eq!(folder.begin_upload("u-1_A").unwrap(), 0);
        assert_eq!(folder.append_upload("u-1_A", 0, b"hello ").unwrap(), 6);
        assert_eq!(folder.append_upload("u-1_A", 6, b"world").unwrap(), 11);
        // Asking again, as a client resuming would, finds what arrived.
        assert_eq!(folder.begin_upload("u-1_A").unwrap(), 11);
        assert_eq!(fs::read(folder.upload_path("u-1_A").unwrap()).unwrap(), b"hello world");

        let written = folder.finish_upload("u-1_A", "Uploads/hi.txt").unwrap();
        assert_eq!(written.path, "Uploads/hi.txt");
        assert_eq!(written.size, 11);
        assert_eq!(written.hash, blake3::hash(b"hello world").to_hex().to_string());
        assert_eq!(fs::read(folder.root().join("Uploads/hi.txt")).unwrap(), b"hello world");
        // Moved, not copied.
        assert!(matches!(folder.upload_path("u-1_A"), Err(Error::NotFound { .. })));
        assert!(matches!(folder.append_upload("u-1_A", 11, b"x"), Err(Error::NotFound { .. })));
    }

    #[test]
    fn a_retried_chunk_is_ignored_and_a_gap_is_refused() {
        let (_tmp, folder) = folder();
        folder.begin_upload("up").unwrap();
        folder.append_upload("up", 0, b"abcdef").unwrap();
        // The same chunk again, a part of it, and an empty one.
        assert_eq!(folder.append_upload("up", 0, b"abcdef").unwrap(), 6);
        assert_eq!(folder.append_upload("up", 2, b"cd").unwrap(), 6);
        assert_eq!(folder.append_upload("up", 6, b"").unwrap(), 6);
        assert_eq!(fs::read(folder.upload_path("up").unwrap()).unwrap(), b"abcdef");

        // A gap, and a chunk that overlaps what arrived but goes past it.
        let gap = folder.append_upload("up", 9, b"xyz").unwrap_err();
        assert!(matches!(gap, Error::Invalid(_)));
        assert!(gap.to_string().contains("byte 6"), "names the expected offset: {gap}");
        assert!(folder.append_upload("up", 4, b"efgh").is_err());
        assert!(folder.append_upload("up", u64::MAX, b"xyz").is_err());
        assert_eq!(fs::read(folder.upload_path("up").unwrap()).unwrap(), b"abcdef");
    }

    #[test]
    fn upload_tokens_are_plain_words() {
        let (_tmp, folder) = folder();
        let longest = "a".repeat(64);
        let too_long = "a".repeat(65);
        assert!(folder.begin_upload(&longest).is_ok());
        for bad in ["", "a b", "../x", "a/b", "a.b", "é", too_long.as_str(), ".."] {
            assert!(matches!(folder.begin_upload(bad), Err(Error::Invalid(_))), "{bad:?}");
            assert!(folder.append_upload(bad, 0, b"x").is_err());
            assert!(folder.finish_upload(bad, "a.txt").is_err());
            assert!(folder.upload_path(bad).is_err());
            assert!(folder.cancel_upload(bad).is_err());
        }
    }

    #[test]
    fn finishing_onto_a_taken_name_keeps_the_upload_for_another_try() {
        let (_tmp, folder) = folder();
        put(&folder, "a.txt", b"mine");
        folder.begin_upload("up").unwrap();
        folder.append_upload("up", 0, b"theirs").unwrap();
        assert!(matches!(folder.finish_upload("up", "a.txt"), Err(Error::Invalid(_))));
        assert_eq!(fs::read(folder.root().join("a.txt")).unwrap(), b"mine");
        assert_eq!(folder.begin_upload("up").unwrap(), 6);
        folder.finish_upload("up", "a (2).txt").unwrap();
        assert_eq!(fs::read(folder.root().join("a (2).txt")).unwrap(), b"theirs");
        assert!(folder.finish_upload("never-begun", "b.txt").is_err());
    }

    #[test]
    fn cancelling_an_upload_removes_it_and_is_harmless_twice() {
        let (_tmp, folder) = folder();
        folder.begin_upload("up").unwrap();
        folder.append_upload("up", 0, b"abc").unwrap();
        folder.cancel_upload("up").unwrap();
        folder.cancel_upload("up").unwrap();
        assert!(folder.upload_path("up").is_err());
        assert_eq!(folder.begin_upload("up").unwrap(), 0);
    }

    #[test]
    fn sweeping_removes_old_staging_files_and_only_those() {
        let (_tmp, folder) = folder();
        folder.begin_upload("old").unwrap();
        folder.append_upload("old", 0, b"abc").unwrap();
        folder.begin_upload("new").unwrap();
        folder.append_upload("new", 0, b"abc").unwrap();
        backdate(&folder.upload_path("old").unwrap(), 3);
        // What a crash would have left in tmp.
        let stale = folder.root().join(INTERNAL_DIR).join(TMP).join("x.part");
        fs::write(&stale, b"half").unwrap();
        backdate(&stale, 3);
        put(&folder, "keep.txt", b"a real file");

        let day = Duration::from_secs(86_400);
        assert_eq!(folder.sweep_uploads(day).unwrap(), 2);
        assert!(folder.upload_path("old").is_err());
        assert!(folder.upload_path("new").is_ok());
        assert!(!stale.exists());
        assert!(folder.root().join("keep.txt").exists());
        // Nothing left that is old enough.
        assert_eq!(folder.sweep_uploads(day).unwrap(), 0);
        assert_eq!(folder.sweep_uploads(Duration::from_secs(10 * 86_400)).unwrap(), 0);
    }

    // ---- moving and deleting ----------------------------------------------

    #[test]
    fn rename_moves_files_and_folders_and_creates_the_parent() {
        let (_tmp, folder) = folder();
        put(&folder, "a.txt", b"a");
        folder.rename("a.txt", "Sub/Dir/b.txt").unwrap();
        assert!(!folder.root().join("a.txt").exists());
        assert_eq!(fs::read(folder.root().join("Sub/Dir/b.txt")).unwrap(), b"a");

        folder.rename("Sub", "Renamed").unwrap();
        assert!(folder.root().join("Renamed/Dir/b.txt").exists());
    }

    #[test]
    fn rename_refuses_a_taken_name_a_missing_source_and_a_loop() {
        let (_tmp, folder) = folder();
        put(&folder, "a.txt", b"a");
        put(&folder, "b.txt", b"b");
        put(&folder, "dir/x.txt", b"x");
        assert!(matches!(folder.rename("a.txt", "b.txt"), Err(Error::Invalid(_))));
        assert_eq!(fs::read(folder.root().join("b.txt")).unwrap(), b"b");
        assert!(matches!(folder.rename("nope.txt", "c.txt"), Err(Error::NotFound { .. })));
        assert!(folder.rename("dir", "dir/inside").is_err());
        assert!(folder.rename("a.txt", "a.txt").is_err());
        assert!(folder.rename("", "x").is_err());
        assert!(folder.rename("a.txt", "").is_err());
        assert!(folder.rename("a.txt", ".karmayoga/a.txt").is_err());
        assert!(folder.rename(".karmayoga", "x").is_err());
        assert!(folder.root().join("a.txt").exists());
    }

    #[test]
    fn trash_and_restore_put_a_file_back_where_it_is_told() {
        let (_tmp, folder) = folder();
        put(&folder, "Photos/a.jpg", b"pixels");
        let id = FileId::new();
        let trashed = folder.trash("Photos/a.jpg", id).unwrap();
        assert_eq!(trashed, format!(".karmayoga/trash/{id}/a.jpg"));
        assert!(!folder.root().join("Photos/a.jpg").exists());
        // A trashed file can still be asked about, but is in nobody's walk.
        assert_eq!(folder.stat(&trashed).unwrap().unwrap().size, 6);
        assert_eq!(folder.read_range(&trashed, 0, 100).unwrap(), b"pixels");
        assert_eq!(folder.hash(&trashed).unwrap(), blake3::hash(b"pixels").to_hex().to_string());
        assert!(folder.walk().unwrap().is_empty());

        folder.restore(&trashed, "Back/a.jpg").unwrap();
        assert_eq!(fs::read(folder.root().join("Back/a.jpg")).unwrap(), b"pixels");
        assert!(
            !folder.root().join(INTERNAL_DIR).join(TRASH).join(id.to_string()).exists(),
            "the emptied trash folder goes"
        );
        assert!(folder.stat(&trashed).unwrap().is_none());
    }

    #[test]
    fn restore_refuses_a_taken_name_and_keeps_the_trashed_file() {
        let (_tmp, folder) = folder();
        put(&folder, "a.txt", b"old");
        let id = FileId::new();
        let trashed = folder.trash("a.txt", id).unwrap();
        put(&folder, "a.txt", b"new");
        assert!(matches!(folder.restore(&trashed, "a.txt"), Err(Error::Invalid(_))));
        assert_eq!(fs::read(folder.root().join("a.txt")).unwrap(), b"new");
        assert_eq!(folder.read_range(&trashed, 0, 10).unwrap(), b"old");
        // So the caller picks a free name, as documented.
        let free = folder.unique_path("", "a.txt").unwrap();
        folder.restore(&trashed, &free).unwrap();
        assert_eq!(fs::read(folder.root().join("a (2).txt")).unwrap(), b"old");
        // Only a trash path is restorable.
        assert!(folder.restore("a.txt", "b.txt").is_err());
        assert!(folder.restore(".karmayoga/trash/not-an-id/x", "b.txt").is_err());
        assert!(folder.restore(".karmayoga/folder.json", "b.txt").is_err());
        assert!(matches!(
            folder.restore(&format!(".karmayoga/trash/{}/x", FileId::new()), "b.txt"),
            Err(Error::NotFound { .. })
        ));
    }

    #[test]
    fn trash_refuses_what_is_not_there_and_what_it_must_not_touch() {
        let (_tmp, folder) = folder();
        assert!(matches!(folder.trash("nope.txt", FileId::new()), Err(Error::NotFound { .. })));
        assert!(folder.trash("", FileId::new()).is_err());
        assert!(folder.trash(".karmayoga/folder.json", FileId::new()).is_err());
        assert!(folder.trash("../x", FileId::new()).is_err());
        assert!(folder.root().join(INTERNAL_DIR).join(MARKER).exists());
        // Nothing is left over in the trash by the failures.
        assert_eq!(fs::read_dir(folder.root().join(INTERNAL_DIR).join(TRASH)).unwrap().count(), 0);
        // The same id twice would clobber the first.
        put(&folder, "a.txt", b"1");
        let id = FileId::new();
        let trashed = folder.trash("a.txt", id).unwrap();
        put(&folder, "a.txt", b"2");
        assert!(matches!(folder.trash("a.txt", id), Err(Error::Invalid(_))));
        assert_eq!(fs::read(folder.root().join("a.txt")).unwrap(), b"2");
        assert_eq!(folder.read_range(&trashed, 0, 10).unwrap(), b"1");
    }

    #[test]
    fn a_folder_can_be_trashed_and_brought_back() {
        let (_tmp, folder) = folder();
        put(&folder, "Album/1.jpg", b"1");
        put(&folder, "Album/2.jpg", b"2");
        let id = FileId::new();
        let trashed = folder.trash("Album", id).unwrap();
        assert!(folder.walk().unwrap().is_empty());
        folder.restore(&trashed, "Album again").unwrap();
        assert_eq!(names(&folder.walk().unwrap()), ["Album again/1.jpg", "Album again/2.jpg"]);
    }

    #[test]
    fn remove_deletes_for_good_and_is_content_with_what_is_gone() {
        let (_tmp, folder) = folder();
        put(&folder, "a.txt", b"a");
        folder.remove("a.txt").unwrap();
        assert!(!folder.root().join("a.txt").exists());
        folder.remove("a.txt").unwrap();
        folder.remove("never/was.txt").unwrap();
        // A parent that is a file means there is nothing at that path either.
        put(&folder, "file", b"x");
        folder.remove("file/under").unwrap();
    }

    #[test]
    fn removing_a_trashed_file_removes_its_trash_folder_too() {
        let (_tmp, folder) = folder();
        put(&folder, "a.txt", b"a");
        let id = FileId::new();
        let trashed = folder.trash("a.txt", id).unwrap();
        let dir = folder.root().join(INTERNAL_DIR).join(TRASH).join(id.to_string());
        assert!(dir.is_dir());
        folder.remove(&trashed).unwrap();
        assert!(!dir.exists());
        folder.remove(&trashed).unwrap();

        // Even when the file was already gone and only the folder is left.
        put(&folder, "b.txt", b"b");
        let id = FileId::new();
        let trashed = folder.trash("b.txt", id).unwrap();
        let dir = folder.root().join(INTERNAL_DIR).join(TRASH).join(id.to_string());
        fs::remove_file(dir.join("b.txt")).unwrap();
        folder.remove(&trashed).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn remove_will_not_delete_a_folder_or_the_foldereds_own_things() {
        let (_tmp, folder) = folder();
        put(&folder, "Photos/a.jpg", b"a");
        assert!(matches!(folder.remove("Photos"), Err(Error::Invalid(_))));
        assert!(folder.root().join("Photos/a.jpg").exists());
        assert!(folder.remove("").is_err());
        assert!(folder.remove(".karmayoga").is_err());
        assert!(folder.remove(".karmayoga/folder.json").is_err());
        assert!(folder.remove(".karmayoga/trash/x/y").is_err());
        assert!(folder.root().join(INTERNAL_DIR).join(MARKER).exists());
        // A trashed folder is the exception.
        let id = FileId::new();
        let trashed = folder.trash("Photos", id).unwrap();
        folder.remove(&trashed).unwrap();
        assert!(folder.walk().unwrap().is_empty());
    }

    #[test]
    fn empty_folders_go_deepest_first_and_nothing_else_does() {
        let (_tmp, folder) = folder();
        folder.ensure_dir("a/b/c").unwrap();
        folder.ensure_dir("a/keep").unwrap();
        folder.ensure_dir("d/e").unwrap();
        folder.ensure_dir(".stfolder").unwrap();
        folder.ensure_dir("hidden-only/.thumbs").unwrap();
        put(&folder, "a/keep/x.txt", b"x");
        put(&folder, "f/g/h.txt", b"h");

        folder.remove_empty_dirs("").unwrap();
        let root = folder.root();
        assert!(!root.join("a/b").exists());
        assert!(!root.join("d").exists());
        assert!(root.join("a/keep/x.txt").exists());
        assert!(root.join("f/g/h.txt").exists());
        // Syncthing's marker is an empty folder on purpose, and so is ours.
        assert!(root.join(".stfolder").is_dir());
        assert!(root.join(INTERNAL_DIR).join(TMP).is_dir());
        assert!(root.join("hidden-only/.thumbs").is_dir());
        // The folder itself stays.
        assert!(root.is_dir());

        // Starting lower removes that folder too, once it is empty.
        folder.ensure_dir("x/y/z").unwrap();
        folder.remove_empty_dirs("x/y").unwrap();
        assert!(!root.join("x/y").exists());
        assert!(root.join("x").is_dir());
        folder.remove_empty_dirs("not/there").unwrap();
        assert!(folder.remove_empty_dirs("..").is_err());
        assert!(folder.remove_empty_dirs(".karmayoga").is_err());
    }

    // ---- reading ----------------------------------------------------------

    #[test]
    fn stat_reports_a_file_and_is_content_with_nothing() {
        let (_tmp, folder) = folder();
        put(&folder, "a/b.txt", b"hello");
        let stat = folder.stat("a/b.txt").unwrap().unwrap();
        assert_eq!(stat.path, "a/b.txt");
        assert_eq!(stat.size, 5);
        assert!(stat.modified_us > 1_500_000_000_000_000);
        assert!(folder.stat("a/nope.txt").unwrap().is_none());
        assert!(folder.stat("a/b.txt/under").unwrap().is_none());
        assert!(matches!(folder.stat("a"), Err(Error::Invalid(_))));
        assert!(folder.stat("../a").is_err());
        assert!(folder.stat(".karmayoga/folder.json").is_err());
    }

    #[test]
    fn read_range_clamps_to_the_end_of_the_file() {
        let (_tmp, folder) = folder();
        put(&folder, "a.txt", b"0123456789");
        assert_eq!(folder.read_range("a.txt", 0, 4).unwrap(), b"0123");
        assert_eq!(folder.read_range("a.txt", 4, 3).unwrap(), b"456");
        assert_eq!(folder.read_range("a.txt", 8, 100).unwrap(), b"89");
        assert!(folder.read_range("a.txt", 10, 5).unwrap().is_empty());
        assert!(folder.read_range("a.txt", 99, 5).unwrap().is_empty());
        assert!(folder.read_range("a.txt", 0, 0).unwrap().is_empty());
        assert_eq!(folder.read_range("a.txt", 0, u64::MAX).unwrap(), b"0123456789");
        assert!(matches!(folder.read_range("nope.txt", 0, 1), Err(Error::NotFound { .. })));
        folder.ensure_dir("d").unwrap();
        assert!(folder.read_range("d", 0, 1).is_err());
    }

    #[test]
    fn hash_streams_the_file() {
        let (_tmp, folder) = folder();
        let bytes: Vec<u8> = (0..(CHUNK + 17)).map(|i| (i % 249) as u8).collect();
        put(&folder, "big.bin", &bytes);
        assert_eq!(folder.hash("big.bin").unwrap(), blake3::hash(&bytes).to_hex().to_string());
        assert!(matches!(folder.hash("nope"), Err(Error::NotFound { .. })));
    }

    #[test]
    fn copy_out_copies_and_will_not_overwrite() {
        let (tmp, folder) = folder();
        put(&folder, "a.txt", b"hello");
        let dest = tmp.path().join("out.txt");
        folder.copy_out("a.txt", &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"hello");
        assert_eq!(
            fs::metadata(&dest).unwrap().modified().unwrap(),
            fs::metadata(folder.root().join("a.txt")).unwrap().modified().unwrap()
        );
        let e = folder.copy_out("a.txt", &dest).unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{e}");
        assert_eq!(fs::read(&dest).unwrap(), b"hello");
        assert!(matches!(
            folder.copy_out("nope.txt", &tmp.path().join("x.txt")),
            Err(Error::NotFound { .. })
        ));
        assert!(!tmp.path().join("x.txt").exists());
        assert!(folder.copy_out("../a.txt", &tmp.path().join("y.txt")).is_err());
    }

    #[test]
    fn walk_lists_files_depth_first_by_name_and_skips_what_it_should() {
        let (_tmp, folder) = folder();
        put(&folder, "b.txt", b"b");
        put(&folder, "a/z.txt", b"z");
        put(&folder, "a/y/deep.txt", b"d");
        put(&folder, "c.txt", b"c");
        put(&folder, ".hidden", b"x");
        put(&folder, ".stfolder/marker", b"x");
        put(&folder, ".stversions/old.txt", b"x");
        put(&folder, "a/.DS_Store", b"x");
        put(&folder, "Thumbs.db", b"x");
        put(&folder, "film.mp4.part", b"x");
        put(&folder, "dl.crdownload", b"x");
        put(&folder, "~syncthing~b.txt.tmp", b"x");
        put(&folder, ".syncthing.b.txt.tmp", b"x");
        folder.write_bytes("Trashed/x.txt", b"in the trash soon").unwrap();
        folder.trash("Trashed/x.txt", FileId::new()).unwrap();

        let walked = folder.walk().unwrap();
        assert_eq!(names(&walked), ["a/y/deep.txt", "a/z.txt", "b.txt", "c.txt"]);
        assert_eq!(walked[0].size, 1);
        assert!(walked.iter().all(|f| f.modified_us > 0));
    }

    #[test]
    fn walk_needs_the_folder_to_be_there() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(Folder::new(tmp.path().join("gone")).walk().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn walk_and_listings_never_follow_or_show_a_symlink() {
        use std::os::unix::fs::symlink;
        let (tmp, folder) = folder();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), b"secret").unwrap();
        put(&folder, "real/a.txt", b"a");
        symlink(&outside, folder.root().join("link-to-dir")).unwrap();
        symlink(outside.join("secret.txt"), folder.root().join("link-to-file")).unwrap();
        symlink(folder.root().join("real"), folder.root().join("alias")).unwrap();
        symlink(folder.root(), folder.root().join("real/loop")).unwrap();

        assert_eq!(names(&folder.walk().unwrap()), ["real/a.txt"]);
        let listed = folder.list_dir("").unwrap();
        assert_eq!(listed.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["real"]);
        assert_eq!(listed[0].items, 1, "a link inside is not counted");
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_cannot_be_opened_does_not_stop_the_walk() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, folder) = folder();
        put(&folder, "ok.txt", b"ok");
        put(&folder, "locked/secret.txt", b"s");
        let locked = folder.root().join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        // Root can read anything, which would make this test say nothing.
        let readable = fs::read_dir(&locked).is_ok();
        let walked = folder.walk();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let walked = walked.unwrap();
        if readable {
            assert_eq!(names(&walked), ["locked/secret.txt", "ok.txt"]);
        } else {
            assert_eq!(names(&walked), ["ok.txt"]);
        }
    }

    #[test]
    fn list_dir_lists_one_level_with_counts() {
        let (_tmp, folder) = folder();
        put(&folder, "b.txt", b"bb");
        put(&folder, "Apple.txt", b"a");
        put(&folder, "docs/one.txt", b"1");
        put(&folder, "docs/two.txt", b"2");
        put(&folder, "docs/.hidden", b"x");
        put(&folder, "docs/sub/three.txt", b"3");
        put(&folder, "docs/dl.part", b"x");
        folder.ensure_dir("empty").unwrap();
        put(&folder, ".stignore-not", b"x");

        let top = folder.list_dir("").unwrap();
        // By name, ignoring case, folders among the files; litter left out.
        assert_eq!(
            top.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
            ["Apple.txt", "b.txt", "docs", "empty"]
        );
        let docs = top.iter().find(|i| i.name == "docs").unwrap();
        assert!(docs.is_dir);
        assert_eq!(docs.path, "docs");
        assert_eq!(docs.items, 3, "one.txt, two.txt and sub; skipped names are not counted");
        assert_eq!(docs.size, 0);
        assert!(docs.modified_us > 0);
        assert_eq!(top.iter().find(|i| i.name == "empty").unwrap().items, 0);
        let b = top.iter().find(|i| i.name == "b.txt").unwrap();
        assert!(!b.is_dir);
        assert_eq!((b.size, b.items), (2, 0));

        let inner = folder.list_dir("docs").unwrap();
        assert_eq!(
            inner.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(),
            ["docs/one.txt", "docs/sub", "docs/two.txt"]
        );
        assert_eq!(inner[1].items, 1);
        // Not there: empty, not an error. Not allowed: an error.
        assert!(folder.list_dir("nope").unwrap().is_empty());
        assert!(folder.list_dir("b.txt").unwrap().is_empty());
        assert!(folder.list_dir("../x").is_err());
        assert!(folder.list_dir(".karmayoga").is_err());
    }

    // ---- decrypted copies -------------------------------------------------

    #[test]
    fn open_dir_is_a_private_empty_folder_per_file() {
        let (_tmp, folder) = folder();
        let id = FileId::new();
        let dir = folder.open_dir(id).unwrap();
        assert_eq!(dir, folder.root().join(INTERNAL_DIR).join(OPEN).join(id.to_string()));
        assert!(dir.is_dir());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::write(dir.join("photo.jpg"), b"decrypted").unwrap();
        // Opening again starts clean: no stale copy of an older version.
        let again = folder.open_dir(id).unwrap();
        assert_eq!(again, dir);
        assert_eq!(fs::read_dir(&again).unwrap().count(), 0);
        // Not part of the catalog's view of the folder.
        fs::write(again.join("photo.jpg"), b"decrypted").unwrap();
        assert!(folder.walk().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn old_decrypted_copies_are_swept_and_newer_ones_are_not() {
        let (_tmp, folder) = folder();
        let (old, new) = (FileId::new(), FileId::new());
        let old_dir = folder.open_dir(old).unwrap();
        fs::write(old_dir.join("a.jpg"), b"a").unwrap();
        let new_dir = folder.open_dir(new).unwrap();
        fs::write(new_dir.join("b.jpg"), b"b").unwrap();
        backdate(&old_dir.join("a.jpg"), 3);
        backdate(&old_dir, 3);

        let day = Duration::from_secs(86_400);
        assert_eq!(folder.sweep_open(day).unwrap(), 1);
        assert!(!old_dir.exists());
        assert!(new_dir.join("b.jpg").exists());
        assert_eq!(folder.sweep_open(day).unwrap(), 0);
        // A copy touched recently inside an old folder is still in use.
        let kept = folder.open_dir(old).unwrap();
        fs::write(kept.join("c.jpg"), b"c").unwrap();
        backdate(&kept, 3);
        assert_eq!(folder.sweep_open(day).unwrap(), 0);
        assert!(kept.join("c.jpg").exists());
    }
}
