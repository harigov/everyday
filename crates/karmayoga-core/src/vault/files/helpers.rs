//! Small functions the other parts share: names, paths, batches, entries and index rows.

use super::{HEAD_LEN, MAX_BATCH, MAX_DEPTH};
use crate::error::{Error, Result};
use crate::files::disk::{self, Folder};
use crate::files::{
    Facet, FileEntry, FileKind, FileOrigin, FileStatus, FileStorage, IndexRow, MediaInfo, reconcile,
};
use crate::id::FileId;
use crate::store::files::FilesStore;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::path::Path;

/// A meal as "used in" names it: its own name, else what kind of meal it
/// was, with the day it was eaten (in this machine's zone, as the Files app
/// shows every time).
pub(super) fn meal_title(meal: &crate::health::Meal) -> String {
    let day = meal.at.to_zoned(TimeZone::system()).date().strftime("%-d %B %Y");
    let name = match meal.name.trim() {
        "" => meal.slot.word(),
        name => name,
    };
    format!("{name}, {day}")
}

pub(super) fn unavailable(path: &str, reason: &str) -> Error {
    Error::Invalid(format!("the files folder at {path} is not available: {reason}"))
}

pub(super) fn not_there(entry: &FileEntry) -> Error {
    Error::Invalid(format!(
        "{} is not in the files folder (it is missing); forget it, or put it back",
        entry.name
    ))
}

pub(super) fn check_batch(n: usize) -> Result<()> {
    if n > MAX_BATCH {
        return Err(Error::Invalid(format!("at most {MAX_BATCH} files at a time")));
    }
    Ok(())
}

/// `ids` without repeats, in order.
pub(super) fn unique(ids: &[FileId]) -> Vec<FileId> {
    let mut seen = HashSet::with_capacity(ids.len());
    ids.iter().copied().filter(|id| seen.insert(*id)).collect()
}

/// `files` with each file once, the first copy kept: a file that is a stack's
/// top and also among those asked about is read twice, and written once.
pub(super) fn once_each(files: Vec<FileEntry>) -> Vec<FileEntry> {
    let mut seen = HashSet::with_capacity(files.len());
    files.into_iter().filter(|f| seen.insert(f.id)).collect()
}

/// A name valid on every system the folder may be synced to, and one the
/// folder's own listings keep. A name that [`disk::is_skipped`] -- a hidden
/// file, a download in progress -- would be catalogued and then never seen
/// again by the walk that checks the catalog against the disk, and would be
/// marked missing.
pub(super) fn validate_file_name(name: &str) -> Result<String> {
    let name = disk::validate_name(name)?;
    if disk::is_skipped(&name) {
        return Err(Error::Invalid(format!(
            "{name:?} is a name the folder ignores (hidden files, and the leftovers of sync \
             and download tools), so it could not be kept there"
        )));
    }
    Ok(name)
}

/// A folder's path inside the files folder: valid, and not the folder
/// itself.
pub(super) fn folder_path(path: &str) -> Result<String> {
    let path = disk::validate_rel(path)?;
    if path.is_empty() {
        return Err(Error::Invalid("that is the files folder itself".into()));
    }
    Ok(path)
}

/// Where a trashed file goes back to: the folder and name it came from, or
/// the top of the folder under its own name when the record's `from` is not
/// a place inside it (a catalogue gone wrong could name one outside).
pub(super) fn restore_place(from: &str, name: &str) -> (String, String) {
    match disk::validate_rel(from) {
        Ok(valid) if !valid.is_empty() => {
            let (dir, name) = disk::split(&valid);
            (dir.to_string(), name.to_string())
        }
        _ => (String::new(), name.to_string()),
    }
}

/// Undo restores: each file goes back to its place in the trash.
pub(super) fn put_back(folder: &Folder, moved: &[(FileId, String, String)]) {
    for (id, _trashed, to) in moved.iter().rev() {
        let _ = folder.trash(to, *id);
    }
}

pub(super) fn set_plain_path(entry: &mut FileEntry, path: String) {
    entry.name = disk::split(&path).1.to_string();
    entry.storage = FileStorage::Plain { path };
}

pub(super) fn from_micros(us: i64) -> Timestamp {
    Timestamp::from_microsecond(us).unwrap_or_else(|_| Timestamp::now())
}

/// A blank record for a file that is about to be catalogued: everything the
/// person adds is empty, and the caller fills in where the bytes are.
pub(super) fn new_entry(
    id: FileId,
    origin: FileOrigin,
    name: &str,
    mime: String,
    now: Timestamp,
) -> FileEntry {
    FileEntry {
        id,
        storage: FileStorage::Plain { path: name.to_string() },
        name: name.to_string(),
        kind: FileKind::classify(&mime, name),
        mime,
        size: 0,
        hash: String::new(),
        modified_at: now,
        added_at: now,
        updated_at: now,
        origin,
        status: FileStatus::Present,
        media: MediaInfo::default(),
        thumbnail: None,
        caption: String::new(),
        tags: Vec::new(),
        rating: None,
        favourite: false,
        hidden: false,
        people: Vec::new(),
        place: None,
        stack: None,
        properties: BTreeMap::new(),
    }
}

/// The record for a file the reconciler found in the folder.
pub(super) fn found_entry(
    path: &str,
    size: u64,
    modified_us: i64,
    hash: String,
    mime: String,
    now: Timestamp,
) -> FileEntry {
    let mut entry = new_entry(FileId::new(), FileOrigin::Found, disk::split(path).1, mime, now);
    set_plain_path(&mut entry, path.to_string());
    entry.size = size;
    entry.hash = hash;
    entry.modified_at = from_micros(modified_us);
    entry
}

pub(super) fn read_head(path: &Path) -> Result<Vec<u8>> {
    let mut head = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(HEAD_LEN).read_to_end(&mut head))
        .map_err(|e| Error::io(path, e))?;
    Ok(head)
}

pub(super) fn hash_file(path: &Path) -> Result<(u64, String)> {
    let file = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    disk::hash_reader(file)
}

/// Set a file's modification time and answer what the disk now says it is,
/// in microseconds. `None` when it could not be set, and the caller keeps
/// what the file already had.
pub(super) fn stamp_mtime(folder: &Folder, rel: &str, at: Timestamp) -> Option<i64> {
    let path = folder.resolve(rel).ok()?;
    let file = std::fs::OpenOptions::new().write(true).open(&path).ok()?;
    file.set_modified(std::time::SystemTime::from(at)).ok()?;
    drop(file);
    folder.stat(rel).ok().flatten().map(|d| d.modified_us)
}

/// Every file under `dir`, by walking the folders on the disk. Beyond
/// sixty-four levels is not a folder anybody made.
pub(super) fn files_under(
    folder: &Folder,
    dir: &str,
    out: &mut Vec<disk::DirItem>,
    depth: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    for item in folder.list_dir(dir)? {
        if item.is_dir {
            files_under(folder, &item.path, out, depth + 1)?;
        } else {
            out.push(item);
        }
    }
    Ok(())
}

/// Stack what arrived beside what is already there: by names alone, over
/// the new files and every catalogued file in the same folders. Only a new
/// file is ever put under another -- a stack somebody undid is theirs.
pub(super) fn stack_new_files(fresh: &mut [FileEntry], index: &[IndexRow]) {
    if fresh.is_empty() {
        return;
    }
    let dirs: HashSet<String> = fresh.iter().filter_map(|f| f.dir().map(str::to_string)).collect();
    let mut candidates: Vec<(FileId, String)> =
        fresh.iter().filter_map(|f| f.path().map(|p| (f.id, p.to_string()))).collect();
    candidates.extend(
        index
            .iter()
            .filter(|row| !row.missing && dirs.contains(disk::split(&row.path).0))
            .map(|row| (row.id, row.path.clone())),
    );
    let position: HashMap<FileId, usize> =
        fresh.iter().enumerate().map(|(i, f)| (f.id, i)).collect();
    for (top, members) in reconcile::stacks(&candidates) {
        for member in members {
            if let Some(&i) = position.get(&member) {
                fresh[i].stack = Some(top);
            }
        }
    }
}

/// Is this MIME type, or failing that this name, one of text?
pub(super) fn is_texty(mime: &str, name: &str) -> bool {
    let mime = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if mime.starts_with("text/")
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
        || matches!(
            mime.as_str(),
            "application/json"
                | "application/xml"
                | "application/yaml"
                | "application/x-yaml"
                | "application/toml"
                | "application/javascript"
                | "application/x-sh"
                | "application/x-ndjson"
                | "application/sql"
        )
    {
        return true;
    }
    if !(mime.is_empty() || mime == "application/octet-stream") {
        return false;
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(
        ext.as_str(),
        "txt"
            | "md"
            | "markdown"
            | "csv"
            | "tsv"
            | "json"
            | "log"
            | "xml"
            | "yaml"
            | "yml"
            | "toml"
            | "ini"
            | "cfg"
            | "conf"
            | "html"
            | "htm"
            | "css"
            | "js"
            | "ts"
            | "rs"
            | "py"
            | "sh"
            | "sql"
    )
}

/// A query's text, trimmed; `None` when there is none.
pub(super) fn wanted(text: Option<&str>) -> Option<String> {
    text.map(str::trim).filter(|t| !t.is_empty()).map(str::to_string)
}

/// Equal without regard to case.
pub(super) fn same(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// How many files are under each listed file that tops a stack: what a grid
/// puts on a stack's badge without fetching its members.
pub(super) fn stack_counts(
    store: &dyn FilesStore,
    files: &[FileEntry],
) -> Result<BTreeMap<FileId, u64>> {
    let tops: Vec<FileId> = files.iter().filter(|f| f.stack.is_none()).map(|f| f.id).collect();
    if tops.is_empty() { Ok(BTreeMap::new()) } else { store.stack_counts(&tops) }
}

/// Is every word somewhere in the file: its name, caption, tags, people,
/// place, path, or the camera it was taken with? The camera is there because
/// the Photos view's camera facet narrows by it as text, the way
/// [`camera_of`] names it.
pub(super) fn words_match(f: &FileEntry, words: &[String]) -> bool {
    if words.is_empty() {
        return true;
    }
    let haystack = format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}",
        f.name,
        f.caption,
        f.tags.join("\n"),
        f.people.join("\n"),
        f.place.as_deref().unwrap_or_default(),
        f.path().unwrap_or_default(),
        camera_of(&f.media).unwrap_or_default()
    )
    .to_lowercase();
    words.iter().all(|w| haystack.contains(w))
}

/// A file an app added that nothing refers to any more. A file the person
/// added themselves, or that turned up in the folder, is never "unused":
/// nothing was meant to refer to it.
pub(super) fn is_unused(f: &FileEntry, referenced: &BTreeSet<FileId>) -> bool {
    !matches!(f.origin, FileOrigin::Files | FileOrigin::Found) && !referenced.contains(&f.id)
}

/// A camera as a person says it: `Canon EOS R5`, not `Canon Canon EOS R5`.
pub(super) fn camera_of(media: &MediaInfo) -> Option<String> {
    let make = media.camera_make.as_deref().map(str::trim).unwrap_or_default();
    let model = media.camera_model.as_deref().map(str::trim).unwrap_or_default();
    match (make.is_empty(), model.is_empty()) {
        (true, true) => None,
        (false, true) => Some(make.to_string()),
        (true, false) => Some(model.to_string()),
        (false, false) if model.to_lowercase().starts_with(&make.to_lowercase()) => {
            Some(model.to_string())
        }
        (false, false) => Some(format!("{make} {model}")),
    }
}

/// Names and how many files carry each, without regard to case, in the
/// spelling first seen.
#[derive(Default)]
pub(super) struct Tally {
    by_key: BTreeMap<String, (String, u64)>,
}

impl Tally {
    pub(super) fn add(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.by_key.entry(name.to_lowercase()).or_insert_with(|| (name.to_string(), 0)).1 += 1;
    }

    /// Most used first; names alphabetically within a count.
    pub(super) fn into_facets(self) -> Vec<Facet> {
        let mut facets: Vec<(String, Facet)> = self
            .by_key
            .into_iter()
            .map(|(key, (name, count))| (key, Facet { name, count }))
            .collect();
        facets.sort_by(|a, b| b.1.count.cmp(&a.1.count).then_with(|| a.0.cmp(&b.0)));
        facets.into_iter().map(|(_, f)| f).collect()
    }
}
