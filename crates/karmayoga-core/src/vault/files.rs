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

use super::Vault;
use super::session::{Domain, pick_domain};
use crate::error::{Error, Result};
use crate::files::disk::{self, Folder, Written};
use crate::files::{
    Album, AlbumView, AppliedScan, Facet, FileEntry, FileFilter, FileKind, FileOrigin, FilePage,
    FileQuery, FileStatus, FileStorage, FileUpdate, FileUse, FilesFacets, FilesSettings,
    FilesUsage, FolderInfo, FolderListing, FolderState, INTERNAL_DIR, ImportOptions, IndexRow,
    LegacyReport, LegacySummary, MAX_CAPTION_LEN, MAX_NAME_LEN, MAX_PEOPLE, MAX_PROPERTIES,
    MAX_PROPERTY_TEXT_LEN, MAX_TAG_LEN, MAX_TAGS, MediaInfo, PropValue, RefKind, RefOwner,
    ScanChanges, ScanReport, StatusFilter, StorageKind, UploadTicket, reconcile,
    valid_property_key,
};
use crate::id::{
    AlbumId, BlobId, ConversationId, EntryId, FileId, HealthDocumentId, ItemId, MealId, NoteId,
};
use crate::model::{Attachment, MediaKind};
use crate::note::Note;
use crate::record::RecordKind;
use crate::richtext::{MEDIA_NODE, RichDoc};
use crate::store::JournalStore;
use crate::store::agent::ConversationQuery;
use crate::store::files::FilesStore;
use crate::store::library::ItemQuery;
use jiff::civil;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

/// The most files one call may change, trash or move. Far more than a
/// screen can select; few enough that one exclusive section stays short.
const MAX_BATCH: usize = 50_000;
/// How much of a file is read to tell what it is: enough for every format
/// [`disk::guess_mime`] knows to show its magic bytes.
const HEAD_LEN: u64 = 8192;
/// The size of one step when a sealed file is read out piece by piece, so a
/// film of several gigabytes is never held in memory whole.
const CHUNK: u64 = 4 * 1024 * 1024;
/// Uploads begun and neither finished nor cancelled, at most: a client that
/// begins and walks away cannot fill the table.
const MAX_UPLOADS: usize = 256;
/// How long an upload may sit untouched before it is forgotten.
const UPLOAD_LIFETIME_SECS: u64 = 24 * 3600;
/// The longest a place's name may be, in characters.
const MAX_PLACE_LEN: usize = MAX_NAME_LEN;
/// The longest a property's list may be.
const MAX_PROPERTY_LIST: usize = 1000;
/// The longest an album's name may be, in characters.
const MAX_ALBUM_NAME: usize = 200;
/// Where a sealed file is written out to when it remembers no place of its
/// own.
const ENCRYPTED_DIR: &str = "Encrypted";
/// The property a file carries after being sealed: the path it had, so
/// writing it out again puts it back.
const PROP_WAS_AT: &str = "files.was_at";
/// The property a file carries once somebody has corrected when it was
/// taken: what the file itself said, so the correction can be undone.
const PROP_TAKEN_BY_FILE: &str = "files.taken_by_file";
/// Documents nested deeper than this are treated as corrupt, as in
/// [`crate::richtext`].
const MAX_DEPTH: usize = 64;

/// The ordering lock for placing a file in the folder: choosing a free name,
/// writing the file there, and recording it.
static PLACING: Mutex<()> = Mutex::new(());

fn placing() -> MutexGuard<'static, ()> {
    PLACING.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What an upload was begun with, kept until it is finished or cancelled.
#[derive(Debug, Clone)]
struct PendingUpload {
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
enum UploadArea {
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
fn uploads() -> MutexGuard<'static, HashMap<String, PendingUpload>> {
    static UPLOADS: OnceLock<Mutex<HashMap<String, PendingUpload>>> = OnceLock::new();
    UPLOADS.get_or_init(Default::default).lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A ticket nobody can guess: sixteen random bytes as hex.
fn new_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Where a new file's bytes come from.
enum Source<'a> {
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

    // ---- settings and the folder -----------------------------------------

    pub fn files_settings(&self) -> Result<FilesSettings> {
        self.with_files(|s| s.files_settings())
    }

    /// Validate and store. Trash days at most 3650. A changed `folder` must
    /// be an absolute path; it is not created here (see
    /// [`Vault::set_up_files_folder`]), and the catalog's relative paths are
    /// read against it from then on -- which is what lets somebody move the
    /// folder with a file manager and point Karma Yoga at the new place.
    ///
    /// Files found in the folder are plain by definition, so
    /// [`FileOrigin::Found`] is left out of `encrypted` rather than kept as a
    /// choice that could never take effect.
    pub fn save_files_settings(&self, settings: &FilesSettings) -> Result<FilesSettings> {
        self.writable()?;
        let mut settings = settings.clone();
        if settings.trash_days > 3650 {
            return Err(Error::Invalid("trash days must be at most 3650".into()));
        }
        if let Some(folder) = &mut settings.folder {
            let trimmed = folder.trim().to_string();
            if trimmed.is_empty() {
                return Err(Error::Invalid("the files folder must not be empty".into()));
            }
            if trimmed.len() > 4096 || trimmed.contains('\0') {
                return Err(Error::Invalid("that is not a folder path".into()));
            }
            if !Path::new(&trimmed).is_absolute() {
                return Err(Error::Invalid(format!(
                    "the files folder must be an absolute path, not {trimmed:?}"
                )));
            }
            *folder = trimmed;
        }
        settings.encrypted.retain(|o| *o != FileOrigin::Found);
        settings.encrypted.sort();
        settings.encrypted.dedup();
        self.with_files(|s| s.put_files_settings(&settings))?;
        self.files_settings()
    }

    /// The files folder, when it has been set up.
    pub fn files_folder(&self) -> Result<Option<Folder>> {
        Ok(self.files_settings()?.folder.map(Folder::new))
    }

    /// Whether the folder can be used now.
    pub fn folder_state(&self) -> Result<FolderState> {
        match self.files_folder()? {
            Some(folder) => Ok(folder.state(self.expects_files()?)),
            None => Ok(FolderState::NotSetUp {
                suggested: disk::default_folder()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            }),
        }
    }

    /// Set the folder up at `path` (absolute), or at
    /// [`crate::files::disk::default_folder`] when `None`: create it, write
    /// the marker, store it in the settings. A folder that already has files
    /// in it is fine -- the next scan catalogues them.
    pub fn set_up_files_folder(&self, path: Option<&Path>) -> Result<FolderState> {
        self.writable()?;
        let path = match path {
            Some(path) => path.to_path_buf(),
            None => disk::default_folder().ok_or_else(|| {
                Error::Invalid("this machine has no home folder to put the files folder in".into())
            })?,
        };
        if !path.is_absolute() {
            return Err(Error::Invalid(format!(
                "the files folder must be an absolute path, not {}",
                path.display()
            )));
        }
        let text = path
            .to_str()
            .ok_or_else(|| Error::Invalid("the files folder's path is not valid text".into()))?;
        Folder::new(&path).set_up()?;
        let mut settings = self.files_settings()?;
        settings.folder = Some(text.to_string());
        self.save_files_settings(&settings)?;
        self.folder_state()
    }

    /// The folder, set up at the default place first if it never was, and
    /// refused when unavailable. What every write of a plain file starts
    /// with.
    pub fn usable_folder(&self) -> Result<Folder> {
        self.writable()?;
        let folder = match self.files_folder()? {
            Some(folder) => folder,
            None => {
                self.set_up_files_folder(None)?;
                self.files_folder()?
                    .ok_or_else(|| Error::Invalid("the files folder could not be set up".into()))?
            }
        };
        match folder.state(self.expects_files()?) {
            FolderState::Available { .. } => {
                // A folder somebody made and pointed Settings at, or one
                // that was emptied by hand, may lack its marker or its
                // internal folders. Setting up again is safe: it only
                // creates what is missing.
                if !folder.root().join(INTERNAL_DIR).join(disk::MARKER).is_file() {
                    folder.set_up()?;
                }
                Ok(folder)
            }
            FolderState::Unavailable { path, reason } => Err(unavailable(&path, &reason)),
            FolderState::NotSetUp { .. } => {
                Err(Error::Invalid("the files folder has not been set up".into()))
            }
        }
    }

    /// The folder for *reading* a plain file: it must be there, but nothing
    /// is created or changed. A plain file is being read, so the catalog
    /// certainly expects files in the folder, and the count a general state
    /// check would make is skipped -- a film is read in many ranges.
    fn available_folder(&self) -> Result<Folder> {
        let Some(folder) = self.files_folder()? else {
            return Err(Error::Invalid("the files folder has not been set up".into()));
        };
        match folder.state(true) {
            FolderState::Available { .. } => Ok(folder),
            FolderState::Unavailable { path, reason } => Err(unavailable(&path, &reason)),
            FolderState::NotSetUp { .. } => {
                Err(Error::Invalid("the files folder has not been set up".into()))
            }
        }
    }

    /// Whether the catalog has a plain file that is not in the trash: what
    /// makes a folder without its marker unavailable rather than new.
    fn expects_files(&self) -> Result<bool> {
        self.with_files(|s| {
            let filter = FileFilter {
                storage: Some(StorageKind::Plain),
                status: StatusFilter::Live,
                include_hidden: true,
                ..Default::default()
            };
            Ok(s.count_files(&filter)? > 0)
        })
    }

    // ---- reading ---------------------------------------------------------

    pub fn file(&self, id: FileId) -> Result<FileEntry> {
        self.with_files(|s| s.get_file(id))
    }

    /// A page of files. Store-side filtering where the query allows it; the
    /// sealed fields (`text`, `tag`, `person`, `place`) and `unused` are
    /// applied here, after opening, with paging after that.
    ///
    /// `tag`, `person` and `place` are exact names, compared without regard
    /// to case -- what a facet in the sidebar hands back. `text` is the
    /// loose one: every word must be somewhere in the file.
    pub fn files(&self, query: &FileQuery) -> Result<FilePage> {
        let words: Vec<String> = query
            .text
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let tag = wanted(query.tag.as_deref());
        let person = wanted(query.person.as_deref());
        let place = wanted(query.place.as_deref());
        if words.is_empty() && tag.is_none() && person.is_none() && place.is_none() && !query.unused
        {
            return self.with_files(|s| {
                let files = s.query_files(&query.filter)?;
                Ok(FilePage {
                    stack_counts: stack_counts(s, &files)?,
                    total: s.count_files(&query.filter)?,
                    files,
                })
            });
        }

        // Everything the store cannot narrow is decided after opening, so
        // the store is asked for every candidate and paged here.
        let mut filter = query.filter.clone();
        let (limit, offset) = (filter.limit.take(), filter.offset as usize);
        filter.offset = 0;
        let (rows, referenced) = self.with_files(|s| {
            let rows = s.query_files(&filter)?;
            let referenced = if query.unused { s.referenced_files()? } else { BTreeSet::new() };
            Ok((rows, referenced))
        })?;
        let mut kept: Vec<FileEntry> = rows
            .into_iter()
            .filter(|f| {
                words_match(f, &words)
                    && tag.as_deref().is_none_or(|t| f.tags.iter().any(|x| same(x, t)))
                    && person.as_deref().is_none_or(|p| f.people.iter().any(|x| same(x, p)))
                    && place
                        .as_deref()
                        .is_none_or(|p| f.place.as_deref().is_some_and(|x| same(x, p)))
                    && (!query.unused || is_unused(f, &referenced))
            })
            .collect();
        let total = kept.len() as u64;
        kept.drain(..offset.min(kept.len()));
        if let Some(limit) = limit {
            kept.truncate(limit as usize);
        }
        let stack_counts = self.with_files(|s| stack_counts(s, &kept))?;
        Ok(FilePage { files: kept, total, stack_counts })
    }

    /// One folder: its subfolders from the disk, its files from the catalog.
    ///
    /// Files marked hidden are listed: this is the folder as it is, and a
    /// file that is in it should not be missing from it. When the folder is
    /// not available the listing says so in `state` and is empty.
    pub fn list_folder(&self, dir: &str) -> Result<FolderListing> {
        let dir = disk::validate_rel(dir)?;
        let state = self.folder_state()?;
        if !state.is_available() {
            return Ok(FolderListing { dir, state, folders: Vec::new(), files: Vec::new() });
        }
        let folder = self.available_folder()?;
        let folders = folder
            .list_dir(&dir)?
            .into_iter()
            .filter(|item| item.is_dir)
            .map(|item| FolderInfo {
                name: item.name,
                path: item.path,
                items: item.items,
                modified_at: Timestamp::from_microsecond(item.modified_us).ok(),
            })
            .collect();
        let filter = FileFilter {
            dir: Some(dir.clone()),
            status: StatusFilter::Live,
            include_hidden: true,
            sort: crate::files::FileSort::Name,
            ..Default::default()
        };
        let files = self.with_files(|s| s.query_files(&filter))?;
        Ok(FolderListing { dir, state, folders, files })
    }

    /// The records that refer to a file, each named for the person. Newest
    /// first. A record that no longer exists is left out: the references
    /// table is kept up by saves, and a record removed some other way leaves
    /// a row behind until the next rebuild.
    ///
    /// A message is named by its conversation, found by looking through the
    /// conversations newest first until every message is placed -- the
    /// store has no way to ask which conversation a message is in.
    pub fn file_uses(&self, id: FileId) -> Result<Vec<FileUse>> {
        self.read(|u| {
            let files = pick_domain(u.store.as_ref(), Domain::Files, |s| s.files())?;
            let mut uses = Vec::new();
            let mut messages: BTreeMap<String, RefOwner> = BTreeMap::new();
            for owner in files.refs_to(id)? {
                match owner.kind {
                    RefKind::Entry => {
                        let Ok(entry_id) = EntryId::parse(&owner.id) else { continue };
                        match u.store.get_entry(entry_id) {
                            Ok(e) => uses.push(FileUse {
                                owner,
                                title: e.display_title(),
                                when: Some(e.created_at),
                                parent: None,
                            }),
                            Err(Error::NotFound { .. }) => {}
                            Err(e) => return Err(e),
                        }
                    }
                    RefKind::Note => {
                        let (Ok(note_id), Some(notes)) =
                            (NoteId::parse(&owner.id), u.store.notes())
                        else {
                            continue;
                        };
                        match notes.get_note(note_id) {
                            Ok(n) => uses.push(FileUse {
                                owner,
                                title: n.display_title(),
                                when: Some(n.created_at),
                                parent: None,
                            }),
                            Err(Error::NotFound { .. }) => {}
                            Err(e) => return Err(e),
                        }
                    }
                    RefKind::Message => {
                        messages.insert(owner.id.clone(), owner);
                    }
                    RefKind::HealthDocument => {
                        let (Ok(document_id), Some(health)) =
                            (HealthDocumentId::parse(&owner.id), u.store.health())
                        else {
                            continue;
                        };
                        match health.get_document(document_id) {
                            Ok(d) => uses.push(FileUse {
                                owner,
                                title: match d.title.trim() {
                                    "" => "Untitled document".to_string(),
                                    t => t.to_string(),
                                },
                                when: Some(d.created_at),
                                parent: None,
                            }),
                            Err(Error::NotFound { .. }) => {}
                            Err(e) => return Err(e),
                        }
                    }
                    RefKind::Meal => {
                        let (Ok(meal_id), Some(health)) =
                            (MealId::parse(&owner.id), u.store.health())
                        else {
                            continue;
                        };
                        match health.get_meal(meal_id) {
                            Ok(m) => uses.push(FileUse {
                                owner,
                                title: meal_title(&m),
                                when: Some(m.at),
                                parent: None,
                            }),
                            Err(Error::NotFound { .. }) => {}
                            Err(e) => return Err(e),
                        }
                    }
                    RefKind::Item => {
                        let (Ok(item_id), Some(library)) =
                            (ItemId::parse(&owner.id), u.store.library())
                        else {
                            continue;
                        };
                        match library.get_item(item_id) {
                            // Its shelf's id rides along so the interface can
                            // open the shelf the item is on.
                            Ok(i) => uses.push(FileUse {
                                owner,
                                title: match i.title.trim() {
                                    "" => "Untitled item".to_string(),
                                    t => t.to_string(),
                                },
                                when: Some(i.created_at),
                                parent: Some(i.kind_id.to_string()),
                            }),
                            Err(Error::NotFound { .. }) => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
            }
            if !messages.is_empty()
                && let Some(agent) = u.store.agent()
            {
                for conversation in agent.list_conversations(&ConversationQuery::default())? {
                    if messages.is_empty() {
                        break;
                    }
                    for m in agent.list_messages(conversation.id)? {
                        if let Some(owner) = messages.remove(&m.id.to_string()) {
                            let title = match conversation.title.trim() {
                                "" => "Untitled conversation".to_string(),
                                t => t.to_string(),
                            };
                            uses.push(FileUse {
                                owner,
                                title,
                                when: Some(m.created_at),
                                parent: Some(conversation.id.to_string()),
                            });
                        }
                    }
                }
            }
            uses.sort_by_key(|u| std::cmp::Reverse(u.when));
            Ok(uses)
        })
    }

    /// The albums a file is in, by name.
    pub fn file_albums(&self, id: FileId) -> Result<Vec<Album>> {
        self.with_files(|s| {
            let mut albums = Vec::new();
            for album in s.albums_of(id)? {
                match s.get_album(album) {
                    Ok(a) => albums.push(a),
                    Err(Error::NotFound { .. }) => {}
                    Err(e) => return Err(e),
                }
            }
            albums.sort_by_key(|a| a.name.to_lowercase());
            Ok(albums)
        })
    }

    pub fn files_usage(&self) -> Result<FilesUsage> {
        self.with_files(|s| s.files_usage())
    }

    /// Tags, people, places, years and cameras over every live file that is
    /// not hidden, most used first (years newest first).
    ///
    /// Names that differ only in case are one facet, spelled as the first
    /// file that carried it spelled it: a file's own tags are deduplicated
    /// that way already, and the sidebar should not list "Travel" and
    /// "travel" apart.
    pub fn files_facets(&self) -> Result<FilesFacets> {
        let filter = FileFilter { status: StatusFilter::Live, ..Default::default() };
        let files = self.with_files(|s| s.query_files(&filter))?;
        let zone = TimeZone::system();
        let mut tags = Tally::default();
        let mut people = Tally::default();
        let mut places = Tally::default();
        let mut cameras = Tally::default();
        let mut years: BTreeMap<i16, u64> = BTreeMap::new();
        for f in &files {
            f.tags.iter().for_each(|t| tags.add(t));
            f.people.iter().for_each(|p| people.add(p));
            if let Some(place) = &f.place {
                places.add(place);
            }
            if let Some(camera) = camera_of(&f.media) {
                cameras.add(&camera);
            }
            *years.entry(f.when().to_zoned(zone.clone()).year()).or_default() += 1;
        }
        Ok(FilesFacets {
            tags: tags.into_facets(),
            people: people.into_facets(),
            places: places.into_facets(),
            years: years
                .into_iter()
                .rev()
                .map(|(year, count)| Facet { name: year.to_string(), count })
                .collect(),
            cameras: cameras.into_facets(),
        })
    }

    /// A file's length in bytes, from the disk for a plain file.
    pub fn file_len(&self, id: FileId) -> Result<u64> {
        let entry = self.file(id)?;
        match &entry.storage {
            FileStorage::Sealed { blob } => self.read(|u| u.store.blob_len(*blob)),
            FileStorage::Plain { path } => {
                let folder = self.available_folder()?;
                match folder.stat(path)? {
                    Some(on_disk) => Ok(on_disk.size),
                    None => Err(not_there(&entry)),
                }
            }
        }
    }

    /// Up to `len` bytes from `start`, plain or sealed alike.
    pub fn read_file_range(&self, id: FileId, start: u64, len: u64) -> Result<Vec<u8>> {
        let entry = self.file(id)?;
        match &entry.storage {
            FileStorage::Sealed { blob } => {
                self.read(|u| u.store.get_blob_range(*blob, start, len))
            }
            FileStorage::Plain { path } => self.available_folder()?.read_range(path, start, len),
        }
    }

    /// A text file's contents, decoded as UTF-8 (lossily), at most
    /// `max_bytes` of it. Refuses what is not text (by MIME type and by a
    /// NUL in the first 8 KiB).
    pub fn read_file_text(&self, id: FileId, max_bytes: usize) -> Result<String> {
        let entry = self.file(id)?;
        if !is_texty(&entry.mime, &entry.name) {
            return Err(Error::Invalid(format!("{} is not a text file", entry.name)));
        }
        let bytes = self.read_file_range(id, 0, max_bytes as u64)?;
        if bytes[..bytes.len().min(HEAD_LEN as usize)].contains(&0) {
            return Err(Error::Invalid(format!("{} is not a text file", entry.name)));
        }
        // A cut in the middle of a character is the cut's doing, not the
        // file's: stop before it rather than ending in a replacement mark.
        let end = match std::str::from_utf8(&bytes) {
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            _ => bytes.len(),
        };
        Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
    }

    /// A plain file's absolute path, checked to be inside the folder. For
    /// reading metadata, opening it in another program, showing it in a
    /// file manager. Refuses a sealed file.
    pub fn plain_path(&self, id: FileId) -> Result<PathBuf> {
        let entry = self.file(id)?;
        match &entry.storage {
            FileStorage::Sealed { .. } => Err(Error::Invalid(format!(
                "{} is sealed in the vault and has no place in the folder",
                entry.name
            ))),
            FileStorage::Plain { path } => self.available_folder()?.resolve(path),
        }
    }

    /// Whether `version` -- what the interface versions a file's address by:
    /// its hash, or its modified time while it has none -- is this file as it
    /// is now. True only when the catalog says that version *and*, for a
    /// plain file, the disk still has the size and the time the catalog
    /// recorded, so nothing has changed the bytes since the catalog last
    /// looked. What lets a webview keep the bytes behind an address for good:
    /// a file edited on disk before the reconciler's next pass must not be
    /// cached under the version it had.
    pub fn file_version_is_current(&self, id: FileId, version: &str) -> Result<bool> {
        let entry = self.file(id)?;
        let named = if entry.hash.is_empty() {
            version == entry.modified_at.to_string()
        } else {
            version == entry.hash
        };
        if !named {
            return Ok(false);
        }
        let Some(path) = entry.path() else { return Ok(true) };
        let Some(folder) = self.files_folder()? else { return Ok(false) };
        Ok(folder.stat(path)?.is_some_and(|disk| {
            disk.size == entry.size && disk.modified_us == entry.modified_at.as_microsecond()
        }))
    }

    /// The thumbnail's JPEG, if one has been drawn.
    pub fn file_thumbnail(&self, id: FileId) -> Result<Option<Vec<u8>>> {
        match self.file(id)?.thumbnail {
            Some(blob) => self.read(|u| u.store.get_blob(blob)).map(Some),
            None => Ok(None),
        }
    }

    // ---- adding ----------------------------------------------------------

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
    fn is_sealed(&self, opts: &ImportOptions) -> Result<bool> {
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

    // ---- organising ------------------------------------------------------

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
    /// not -- a second window or `karmayoga list` beside the real one, whose
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

    // ---- albums ----------------------------------------------------------

    /// Every album, by name, with its count and cover.
    pub fn albums(&self) -> Result<Vec<AlbumView>> {
        self.with_files(|s| {
            let counts = s.album_counts()?;
            let mut views = Vec::new();
            for album in s.list_albums()? {
                let count = counts.get(&album.id).copied().unwrap_or(0);
                let mut cover = None;
                if count > 0 {
                    let members = s.album_files(album.id)?;
                    // The cover somebody chose, if it is still in the album
                    // and not in the trash; else the first file that is.
                    let chosen = album.cover.filter(|c| members.contains(c));
                    for id in chosen.into_iter().chain(members.iter().copied()) {
                        match s.get_file(id) {
                            Ok(f) if !f.status.is_trashed() => {
                                cover = Some(f);
                                break;
                            }
                            Ok(_) | Err(Error::NotFound { .. }) => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
                views.push(AlbumView { album, count, cover });
            }
            views.sort_by_key(|v| v.album.name.to_lowercase());
            Ok(views)
        })
    }

    /// Create or rename. A name is 1 to 200 characters.
    pub fn save_album(&self, album: &Album) -> Result<Album> {
        self.writable()?;
        let mut album = album.clone();
        album.name = album.name.trim().to_string();
        let length = album.name.chars().count();
        if length == 0 || length > MAX_ALBUM_NAME {
            return Err(Error::Invalid(format!(
                "an album's name is 1 to {MAX_ALBUM_NAME} characters"
            )));
        }
        if album.description.chars().count() > MAX_CAPTION_LEN {
            return Err(Error::Invalid(format!(
                "an album's description is at most {MAX_CAPTION_LEN} characters"
            )));
        }
        let now = Timestamp::now();
        let id = album.id;
        let stored = self.with_files(|s| {
            // An album that exists keeps when it was made, whatever the
            // caller says.
            match s.get_album(id) {
                Ok(existing) => album.created_at = existing.created_at,
                Err(Error::NotFound { .. }) => album.created_at = now,
                Err(e) => return Err(e),
            }
            album.updated_at = now;
            s.put_album(&album)?;
            Ok(album.clone())
        })?;
        self.wrote(RecordKind::Album, id);
        Ok(stored)
    }

    pub fn delete_album(&self, id: AlbumId) -> Result<()> {
        self.writable()?;
        self.with_files(|s| s.delete_album(id))?;
        self.wrote(RecordKind::Album, id);
        Ok(())
    }

    pub fn add_to_album(&self, id: AlbumId, files: &[FileId]) -> Result<()> {
        self.writable()?;
        check_batch(files.len())?;
        let files = unique(files);
        self.with_files(|s| {
            s.get_album(id)?;
            let found = s.get_files(&files)?;
            if let Some(missing) = files.iter().find(|f| !found.iter().any(|x| x.id == **f)) {
                return Err(Error::not_found("file", missing));
            }
            s.add_to_album(id, &files)
        })?;
        self.wrote(RecordKind::Album, id);
        Ok(())
    }

    pub fn remove_from_album(&self, id: AlbumId, files: &[FileId]) -> Result<()> {
        self.writable()?;
        check_batch(files.len())?;
        self.with_files(|s| s.remove_from_album(id, files))?;
        self.wrote(RecordKind::Album, id);
        Ok(())
    }

    // ---- references ------------------------------------------------------

    /// Record that `owner` refers to exactly `files`. Quiet when the
    /// backend keeps no catalog, so a save of an entry never fails for it.
    ///
    /// Only files the catalog holds are recorded: a body can still name a
    /// file that was deleted, and a reference to nothing says nothing.
    pub fn set_file_refs(&self, owner: &RefOwner, files: &[FileId]) -> Result<()> {
        self.writable()?;
        let recorded = self.with_files(|s| {
            let held: Vec<FileId> = if files.is_empty() {
                Vec::new()
            } else {
                s.get_files(files)?.into_iter().map(|f| f.id).collect()
            };
            s.set_refs(owner, &held)
        });
        match recorded {
            Err(Error::Unsupported(_)) => Ok(()),
            other => other,
        }
    }

    /// [`Vault::set_file_refs`] for the save of a record, which must never
    /// fail for it: a failure is logged and the next save tries again, and
    /// [`Vault::rebuild_file_refs`] is there for a table that has drifted.
    pub(super) fn track_file_refs(&self, owner: RefOwner, files: impl IntoIterator<Item = FileId>) {
        let files: Vec<FileId> = files.into_iter().collect();
        if let Err(e) = self.set_file_refs(&owner, &files) {
            tracing::warn!(
                error = %e,
                kind = owner.kind.as_str(),
                id = %owner.id,
                "could not record which files a record refers to"
            );
        }
    }

    /// A record is gone, and with it everything it referred to.
    pub(super) fn drop_file_refs(&self, owner: RefOwner) {
        self.track_file_refs(owner, std::iter::empty());
    }

    /// Recompute the whole references table from every entry, note,
    /// assistant message, health document, meal and library item. Answers
    /// how many references there are.
    pub fn rebuild_file_refs(&self) -> Result<u64> {
        self.writable()?;
        // The exclusive lock, not the shared one: a save landing between
        // reading the records and writing the table would have its own
        // references overwritten with the older ones.
        self.write(|u| {
            let files = match pick_domain(u.store.as_ref(), Domain::Files, |s| s.files()) {
                Ok(files) => files,
                Err(Error::Unsupported(_)) => return Ok(0),
                Err(e) => return Err(e),
            };
            let mut refs: Vec<(RefOwner, FileId)> = Vec::new();
            for entry in u.store.all_entries()? {
                for file in entry.body.file_refs() {
                    refs.push((RefOwner::new(RefKind::Entry, entry.id), file));
                }
            }
            if let Some(notes) = u.store.notes() {
                for note in notes.all_notes()? {
                    for file in note.body.file_refs() {
                        refs.push((RefOwner::new(RefKind::Note, note.id), file));
                    }
                }
            }
            if let Some(agent) = u.store.agent() {
                for conversation in agent.list_conversations(&ConversationQuery::default())? {
                    for message in agent.list_messages(conversation.id)? {
                        for file in &message.files {
                            refs.push((RefOwner::new(RefKind::Message, message.id), *file));
                        }
                    }
                }
            }
            if let Some(health) = u.store.health() {
                for document in health.list_documents()? {
                    for page in &document.files {
                        refs.push((RefOwner::new(RefKind::HealthDocument, document.id), page.file));
                    }
                }
                for meal in health.meals(Timestamp::MIN, Timestamp::MAX)? {
                    if let Some(photo) = meal.photo {
                        refs.push((RefOwner::new(RefKind::Meal, meal.id), photo));
                    }
                }
            }
            if let Some(library) = u.store.library() {
                for item in library.list_items(&ItemQuery::default())? {
                    for file in &item.files {
                        refs.push((RefOwner::new(RefKind::Item, item.id), *file));
                    }
                }
            }
            // Only files the catalog holds, as `set_file_refs` keeps.
            let distinct: Vec<FileId> =
                refs.iter().map(|(_, f)| *f).collect::<BTreeSet<_>>().into_iter().collect();
            let mut held: HashSet<FileId> = HashSet::with_capacity(distinct.len());
            for chunk in distinct.chunks(200) {
                held.extend(files.get_files(chunk)?.into_iter().map(|f| f.id));
            }
            refs.retain(|(_, f)| held.contains(f));
            files.replace_all_refs(&refs)?;
            Ok(refs.len() as u64)
        })
    }

    // ---- the reconciler's ------------------------------------------------

    pub fn file_index(&self) -> Result<Vec<IndexRow>> {
        self.with_files(|s| s.file_index())
    }

    /// Write one pass's findings in one go: new files catalogued (origin
    /// [`crate::files::FileOrigin::Found`]), changed ones updated (their
    /// metadata to be read again), moved ones re-pathed, vanished ones
    /// marked missing, returned ones present again. Stacks among the new
    /// files are formed ([`crate::files::reconcile::stacks`]).
    ///
    /// A file arriving next to one the catalog already has -- the RAW file
    /// that a sync tool brings after its JPEG -- is stacked under it too,
    /// found by names alone from the catalog's clear columns. Nothing is
    /// done while the folder is unavailable, whatever the changes say.
    pub fn apply_scan(&self, changes: &ScanChanges) -> Result<AppliedScan> {
        self.writable()?;
        let mut report = ScanReport::default();
        if !self.folder_state()?.is_available() {
            report.unavailable = true;
            return Ok(AppliedScan { report, to_probe: Vec::new() });
        }
        let now = Timestamp::now();
        let wanted: Vec<FileId> = changes
            .changed
            .iter()
            .map(|c| c.id)
            .chain(changes.moved.iter().map(|m| m.id))
            .chain(changes.vanished.iter().copied())
            .chain(changes.returned.iter().copied())
            .collect();
        let mut known: HashMap<FileId, FileEntry> =
            self.with_files(|s| s.get_files(&wanted))?.into_iter().map(|f| (f.id, f)).collect();
        let index = self.file_index()?;
        let taken: HashSet<&str> = index.iter().map(|row| row.path.as_str()).collect();
        let move_targets: HashSet<&str> = changes.moved.iter().map(|m| m.path.as_str()).collect();

        let mut batch: Vec<FileEntry> = Vec::new();
        let mut to_probe: Vec<FileId> = Vec::new();
        for c in &changes.changed {
            let Some(mut f) = known.remove(&c.id).filter(|f| !f.status.is_trashed()) else {
                continue;
            };
            f.hash.clone_from(&c.hash);
            f.size = c.size;
            f.modified_at = from_micros(c.modified_us);
            f.status = FileStatus::Present;
            f.updated_at = now;
            to_probe.push(f.id);
            batch.push(f);
            report.changed += 1;
        }
        for m in &changes.moved {
            let Some(mut f) = known.remove(&m.id).filter(|f| !f.status.is_trashed()) else {
                continue;
            };
            set_plain_path(&mut f, m.path.clone());
            f.size = m.size;
            f.modified_at = from_micros(m.modified_us);
            f.status = FileStatus::Present;
            f.updated_at = now;
            batch.push(f);
            report.moved += 1;
        }
        for id in &changes.vanished {
            let Some(mut f) = known.remove(id).filter(|f| f.status == FileStatus::Present) else {
                continue;
            };
            f.status = FileStatus::Missing { since: now };
            f.updated_at = now;
            batch.push(f);
            report.missing += 1;
        }
        for id in &changes.returned {
            let Some(mut f) =
                known.remove(id).filter(|f| matches!(f.status, FileStatus::Missing { .. }))
            else {
                continue;
            };
            f.status = FileStatus::Present;
            f.updated_at = now;
            batch.push(f);
            report.returned += 1;
        }

        let mut fresh: Vec<FileEntry> = Vec::with_capacity(changes.new.len());
        for n in &changes.new {
            // Somebody got there first: an import, or the pass before.
            if taken.contains(n.path.as_str()) || move_targets.contains(n.path.as_str()) {
                continue;
            }
            let name = disk::split(&n.path).1;
            let mime = if n.mime.is_empty() || n.mime == "application/octet-stream" {
                disk::guess_mime(name, &[])
            } else {
                n.mime.clone()
            };
            fresh.push(found_entry(&n.path, n.size, n.modified_us, n.hash.clone(), mime, now));
        }
        stack_new_files(&mut fresh, &index);
        report.added = fresh.len() as u32;
        to_probe.extend(fresh.iter().map(|f| f.id));
        batch.extend(fresh);

        if !batch.is_empty() {
            self.with_files(|s| s.put_files(&batch))?;
            for f in &batch {
                self.wrote(RecordKind::File, f.id);
            }
        }
        Ok(AppliedScan { report, to_probe })
    }

    // ---- what was kept the old way ---------------------------------------

    /// How many pictures and files are kept as blobs inside entries, notes
    /// and assistant messages.
    pub fn legacy_attachments(&self) -> Result<LegacySummary> {
        let scan = self.read(|u| scan_legacy(u.store.as_ref()))?;
        Ok(LegacySummary {
            entries: scan.entries.len() as u32,
            notes: scan.notes.len() as u32,
            messages: scan.messages,
            files: scan.blobs.len() as u32,
            bytes: scan.blobs.values().map(|b| b.len).sum(),
        })
    }

    /// Bring them into the files folder: each blob written out once (under
    /// its origin's folder, by the date of the record it was found in), a
    /// file record for it, and every record that held the blob rewritten to
    /// refer to the file instead. The blobs are left for the garbage
    /// collector.
    ///
    /// # How a record is rewritten
    ///
    /// - A **media node** that named a blob names the file instead:
    ///   `attrs.blob` becomes `attrs.file`, and the kind, caption and
    ///   filename stay.
    /// - An **attachment** of an entry or a note is metadata about a blob
    ///   the body shows, and nothing in the interface draws it but the list
    ///   row's count and cover. It is dropped from the list once its blob is
    ///   a file -- left, it would keep the blob alive for ever and the bytes
    ///   would be kept twice -- and its caption goes into the media node.
    ///   One that the body never showed (the command line's `attach` adds
    ///   such) gets a media node at the end of the body, so the picture the
    ///   person attached is still on the page and the file still has
    ///   something that refers to it.
    /// - An **assistant message** keeps its attachments, which are the
    ///   copies a model is shown, and gains the file in `files`. A message
    ///   that has files already is taken to have been brought in, so a
    ///   second run does not write its pictures again.
    ///
    /// A blob that cannot be brought in is reported and left where it is,
    /// with every record that holds it. Run again, only that is retried.
    /// Records are saved as they were, with their times untouched.
    pub fn bring_in_attachments(&self) -> Result<LegacyReport> {
        self.writable()?;
        let scan = self.read(|u| scan_legacy(u.store.as_ref()))?;
        let mut report = LegacyReport::default();
        if scan.absent > 0 {
            report.errors.push(format!(
                "{} pictures or files are named by records but are no longer in the vault",
                scan.absent
            ));
        }
        if scan.blobs.is_empty() {
            return Ok(report);
        }

        // 1. The blobs, one file each.
        let settings = self.files_settings()?;
        let folder = if scan.blobs.values().any(|b| !settings.encrypted.contains(&b.origin)) {
            Some(self.usable_folder()?)
        } else {
            None
        };
        let mut brought: BTreeMap<BlobId, Brought> = BTreeMap::new();
        for (blob, held) in &scan.blobs {
            let sealed = settings.encrypted.contains(&held.origin);
            match self.bring_blob(folder.as_ref().filter(|_| !sealed), *blob, held) {
                Ok((size, b)) => {
                    report.files += 1;
                    report.bytes += size;
                    brought.insert(*blob, b);
                }
                Err(e) => report.errors.push(format!("{}: {e}", short_blob(*blob))),
            }
        }
        if brought.is_empty() {
            return Ok(report);
        }

        // 2. The records, each read again, so a change since the scan is
        //    not overwritten, and written as it was found.
        for id in &scan.entries {
            let rewritten = self.write(|u| {
                let mut entry = match u.store.get_entry(*id) {
                    Ok(e) => e,
                    Err(Error::NotFound { .. }) => return Ok(None),
                    Err(e) => return Err(e),
                };
                if !rewrite_record(&mut entry.body, &mut entry.attachments, &brought) {
                    return Ok(None);
                }
                u.store.put_entry(&entry)?;
                u.index.insert(&entry);
                Ok(Some(entry.body.file_refs()))
            });
            match rewritten {
                Ok(Some(refs)) => {
                    report.entries += 1;
                    self.track_file_refs(RefOwner::new(RefKind::Entry, id), refs);
                    self.wrote(RecordKind::Entry, id);
                }
                Ok(None) => {}
                Err(e) => report.errors.push(format!("entry {id}: {e}")),
            }
        }
        for id in &scan.notes {
            let rewritten = self.write(|u| {
                let notes = pick_domain(u.store.as_ref(), Domain::Notes, |s| s.notes())?;
                let mut note: Note = match notes.get_note(*id) {
                    Ok(n) => n,
                    Err(Error::NotFound { .. }) => return Ok(None),
                    Err(e) => return Err(e),
                };
                if !rewrite_record(&mut note.body, &mut note.attachments, &brought) {
                    return Ok(None);
                }
                notes.put_note(&note)?;
                super::meetings::reindex_note(u, note.id);
                Ok(Some(note.body.file_refs()))
            });
            match rewritten {
                Ok(Some(refs)) => {
                    report.notes += 1;
                    self.track_file_refs(RefOwner::new(RefKind::Note, id), refs);
                    self.wrote(RecordKind::Note, id);
                }
                Ok(None) => {}
                Err(e) => report.errors.push(format!("note {id}: {e}")),
            }
        }
        for id in &scan.conversations {
            let rewritten = self.write(|u| {
                let agent = pick_domain(u.store.as_ref(), Domain::Agent, |s| s.agent())?;
                let mut changed = Vec::new();
                for mut message in agent.list_messages(*id)? {
                    if rewrite_message(&mut message, &brought) {
                        agent.put_message(&message)?;
                        changed.push((message.id, message.files));
                    }
                }
                Ok(changed)
            });
            match rewritten {
                Ok(changed) => {
                    for (message, files) in changed {
                        report.messages += 1;
                        self.track_file_refs(RefOwner::new(RefKind::Message, message), files);
                        self.wrote(RecordKind::Message, message);
                    }
                }
                Err(e) => report.errors.push(format!("conversation {id}: {e}")),
            }
        }
        Ok(report)
    }

    /// One legacy blob into the catalog: written into the folder under its
    /// origin's folder (or kept sealed, when `folder` is `None`), dated by
    /// the record it was found in. Answers its size and how records are to
    /// name it.
    fn bring_blob(
        &self,
        folder: Option<&Folder>,
        blob: BlobId,
        held: &LegacyBlob,
    ) -> Result<(u64, Brought)> {
        let mut mime = held.meta.mime.trim().to_string();
        if mime.is_empty() || mime == "application/octet-stream" {
            let head = self.read(|u| u.store.get_blob_range(blob, 0, HEAD_LEN))?;
            mime = disk::guess_mime(&held.meta.filename, &head);
        }
        let name = legacy_name(&held.meta.filename, blob, &mime);
        let now = Timestamp::now();
        let id = FileId::new();
        let (storage, size, hash, modified_at) = match folder {
            None => (FileStorage::Sealed { blob }, held.len, String::new(), held.at),
            Some(folder) => {
                let dir = held.origin.default_dir(held.date);
                folder.ensure_dir(&dir)?;
                let written = self.blob_to_folder(folder, blob, &dir, &name)?;
                // A film or a picture is dated by the record it was put in,
                // not by the day it was copied out: the timeline files it
                // under its file's time until its own metadata is read.
                let modified_us =
                    stamp_mtime(folder, &written.path, held.at).unwrap_or(written.modified_us);
                (
                    FileStorage::Plain { path: written.path },
                    written.size,
                    written.hash,
                    from_micros(modified_us),
                )
            }
        };
        let mut entry = new_entry(id, held.origin, &name, mime.clone(), now);
        entry.storage = storage;
        entry.size = size;
        entry.hash = hash;
        entry.modified_at = modified_at;
        entry.caption.clone_from(&held.meta.caption);
        entry.media = MediaInfo {
            width: held.meta.width,
            height: held.meta.height,
            duration_ms: held.meta.duration_ms,
            ..Default::default()
        };
        if let Some(path) = entry.path().map(str::to_string) {
            set_plain_path(&mut entry, path);
        }
        if let Err(e) = self.with_files(|s| s.put_file(&entry)) {
            if let (Some(folder), Some(path)) = (folder, entry.path()) {
                let _ = folder.remove(path);
            }
            return Err(e);
        }
        self.wrote(RecordKind::File, id);
        let brought = Brought {
            id,
            name: entry.name.clone(),
            mime,
            caption: held.meta.caption.clone(),
            width: held.meta.width,
            height: held.meta.height,
        };
        Ok((size, brought))
    }

    // ---- internals -------------------------------------------------------

    /// Catalog a new file: sealed into the blob store or placed in the
    /// folder, whichever the options and the settings say. The one path
    /// every way of adding a file shares.
    fn add(&self, source: Source<'_>, name: &str, opts: &ImportOptions) -> Result<FileEntry> {
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
    fn free_path(&self, folder: &Folder, dir: &str, name: &str) -> Result<String> {
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
    fn entries_of(&self, ids: &[FileId]) -> Result<Vec<FileEntry>> {
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
    fn stack_members(&self, tops: &[FileId]) -> Result<Vec<FileEntry>> {
        let filter =
            FileFilter { status: StatusFilter::All, include_hidden: true, ..Default::default() };
        let all = self.with_files(|s| s.query_files(&filter))?;
        Ok(all.into_iter().filter(|f| f.stack.is_some_and(|t| tops.contains(&t))).collect())
    }

    /// Move present files into the trash, the disk first and the catalog
    /// after, undoing the disk if the catalog refuses: either every file is
    /// in the trash or none is.
    fn trash_entries(&self, mut entries: Vec<FileEntry>) -> Result<Vec<FileEntry>> {
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
    fn relocate_folder(&self, from: &str, to: &str) -> Result<()> {
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
    fn blob_to_folder(
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
    fn blob_to_path(&self, blob: BlobId, dest: &Path, private: bool) -> Result<()> {
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

// ---- small helpers ---------------------------------------------------------

/// A meal as "used in" names it: its own name, else what kind of meal it
/// was, with the day it was eaten (in this machine's zone, as the Files app
/// shows every time).
fn meal_title(meal: &crate::health::Meal) -> String {
    let day = meal.at.to_zoned(TimeZone::system()).date().strftime("%-d %B %Y");
    let name = match meal.name.trim() {
        "" => meal.slot.word(),
        name => name,
    };
    format!("{name}, {day}")
}

fn unavailable(path: &str, reason: &str) -> Error {
    Error::Invalid(format!("the files folder at {path} is not available: {reason}"))
}

fn not_there(entry: &FileEntry) -> Error {
    Error::Invalid(format!(
        "{} is not in the files folder (it is missing); forget it, or put it back",
        entry.name
    ))
}

fn check_batch(n: usize) -> Result<()> {
    if n > MAX_BATCH {
        return Err(Error::Invalid(format!("at most {MAX_BATCH} files at a time")));
    }
    Ok(())
}

/// `ids` without repeats, in order.
fn unique(ids: &[FileId]) -> Vec<FileId> {
    let mut seen = HashSet::with_capacity(ids.len());
    ids.iter().copied().filter(|id| seen.insert(*id)).collect()
}

/// `files` with each file once, the first copy kept: a file that is a stack's
/// top and also among those asked about is read twice, and written once.
fn once_each(files: Vec<FileEntry>) -> Vec<FileEntry> {
    let mut seen = HashSet::with_capacity(files.len());
    files.into_iter().filter(|f| seen.insert(f.id)).collect()
}

/// A name valid on every system the folder may be synced to, and one the
/// folder's own listings keep. A name that [`disk::is_skipped`] -- a hidden
/// file, a download in progress -- would be catalogued and then never seen
/// again by the walk that checks the catalog against the disk, and would be
/// marked missing.
fn validate_file_name(name: &str) -> Result<String> {
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
fn folder_path(path: &str) -> Result<String> {
    let path = disk::validate_rel(path)?;
    if path.is_empty() {
        return Err(Error::Invalid("that is the files folder itself".into()));
    }
    Ok(path)
}

/// Where a trashed file goes back to: the folder and name it came from, or
/// the top of the folder under its own name when the record's `from` is not
/// a place inside it (a catalogue gone wrong could name one outside).
fn restore_place(from: &str, name: &str) -> (String, String) {
    match disk::validate_rel(from) {
        Ok(valid) if !valid.is_empty() => {
            let (dir, name) = disk::split(&valid);
            (dir.to_string(), name.to_string())
        }
        _ => (String::new(), name.to_string()),
    }
}

/// Undo restores: each file goes back to its place in the trash.
fn put_back(folder: &Folder, moved: &[(FileId, String, String)]) {
    for (id, _trashed, to) in moved.iter().rev() {
        let _ = folder.trash(to, *id);
    }
}

fn set_plain_path(entry: &mut FileEntry, path: String) {
    entry.name = disk::split(&path).1.to_string();
    entry.storage = FileStorage::Plain { path };
}

fn from_micros(us: i64) -> Timestamp {
    Timestamp::from_microsecond(us).unwrap_or_else(|_| Timestamp::now())
}

/// A blank record for a file that is about to be catalogued: everything the
/// person adds is empty, and the caller fills in where the bytes are.
fn new_entry(
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
fn found_entry(
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

fn read_head(path: &Path) -> Result<Vec<u8>> {
    let mut head = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(HEAD_LEN).read_to_end(&mut head))
        .map_err(|e| Error::io(path, e))?;
    Ok(head)
}

fn hash_file(path: &Path) -> Result<(u64, String)> {
    let file = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    disk::hash_reader(file)
}

/// Set a file's modification time and answer what the disk now says it is,
/// in microseconds. `None` when it could not be set, and the caller keeps
/// what the file already had.
fn stamp_mtime(folder: &Folder, rel: &str, at: Timestamp) -> Option<i64> {
    let path = folder.resolve(rel).ok()?;
    let file = std::fs::OpenOptions::new().write(true).open(&path).ok()?;
    file.set_modified(std::time::SystemTime::from(at)).ok()?;
    drop(file);
    folder.stat(rel).ok().flatten().map(|d| d.modified_us)
}

/// Every file under `dir`, by walking the folders on the disk. Beyond
/// sixty-four levels is not a folder anybody made.
fn files_under(
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
fn stack_new_files(fresh: &mut [FileEntry], index: &[IndexRow]) {
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
fn is_texty(mime: &str, name: &str) -> bool {
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
fn wanted(text: Option<&str>) -> Option<String> {
    text.map(str::trim).filter(|t| !t.is_empty()).map(str::to_string)
}

/// Equal without regard to case.
fn same(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// How many files are under each listed file that tops a stack: what a grid
/// puts on a stack's badge without fetching its members.
fn stack_counts(store: &dyn FilesStore, files: &[FileEntry]) -> Result<BTreeMap<FileId, u64>> {
    let tops: Vec<FileId> = files.iter().filter(|f| f.stack.is_none()).map(|f| f.id).collect();
    if tops.is_empty() { Ok(BTreeMap::new()) } else { store.stack_counts(&tops) }
}

/// Is every word somewhere in the file: its name, caption, tags, people,
/// place, path, or the camera it was taken with? The camera is there because
/// the Photos view's camera facet narrows by it as text, the way
/// [`camera_of`] names it.
fn words_match(f: &FileEntry, words: &[String]) -> bool {
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
fn is_unused(f: &FileEntry, referenced: &BTreeSet<FileId>) -> bool {
    !matches!(f.origin, FileOrigin::Files | FileOrigin::Found) && !referenced.contains(&f.id)
}

/// A camera as a person says it: `Canon EOS R5`, not `Canon Canon EOS R5`.
fn camera_of(media: &MediaInfo) -> Option<String> {
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
struct Tally {
    by_key: BTreeMap<String, (String, u64)>,
}

impl Tally {
    fn add(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.by_key.entry(name.to_lowercase()).or_insert_with(|| (name.to_string(), 0)).1 += 1;
    }

    /// Most used first; names alphabetically within a count.
    fn into_facets(self) -> Vec<Facet> {
        let mut facets: Vec<(String, Facet)> = self
            .by_key
            .into_iter()
            .map(|(key, (name, count))| (key, Facet { name, count }))
            .collect();
        facets.sort_by(|a, b| b.1.count.cmp(&a.1.count).then_with(|| a.0.cmp(&b.0)));
        facets.into_iter().map(|(_, f)| f).collect()
    }
}

// ---- changing what the person added -----------------------------------------

/// A [`FileUpdate`] that has been checked on its own, ready to apply to each
/// file: what does not depend on a file is decided once.
struct Prepared<'a> {
    update: &'a FileUpdate,
    tags: Option<Vec<String>>,
    add_tags: Vec<String>,
    remove_tags: Vec<String>,
    people: Option<Vec<String>>,
    add_people: Vec<String>,
    remove_people: Vec<String>,
    place: Option<Option<String>>,
    rating: Option<Option<u8>>,
    /// `Some(None)` is "back to what the file says".
    taken_at: Option<Option<Timestamp>>,
}

impl<'a> Prepared<'a> {
    fn new(update: &'a FileUpdate) -> Result<Self> {
        if let Some(caption) = &update.caption
            && caption.chars().count() > MAX_CAPTION_LEN
        {
            return Err(Error::Invalid(format!(
                "a caption is at most {MAX_CAPTION_LEN} characters"
            )));
        }
        let place = match &update.place {
            None => None,
            Some(p) => {
                let p = p.trim();
                if p.chars().count() > MAX_PLACE_LEN {
                    return Err(Error::Invalid(format!(
                        "a place's name is at most {MAX_PLACE_LEN} characters"
                    )));
                }
                Some((!p.is_empty()).then(|| p.to_string()))
            }
        };
        let rating = match update.rating {
            None => None,
            Some(0) => Some(None),
            Some(r @ 1..=5) => Some(Some(r)),
            Some(r) => {
                return Err(Error::Invalid(format!("a rating is 1 to 5, or 0 to clear; not {r}")));
            }
        };
        for (key, value) in &update.properties {
            if !valid_property_key(key) {
                return Err(Error::Invalid(format!(
                    "{key:?} is not a property key: lowercase words joined by dots, at least two"
                )));
            }
            if let Some(value) = value {
                check_property(key, value)?;
            }
        }
        let taken_at = match update.taken_at.as_deref().map(str::trim) {
            None => None,
            Some("") => Some(None),
            Some(text) => Some(Some(parse_taken(text)?)),
        };
        Ok(Self {
            update,
            tags: update
                .tags
                .as_deref()
                .map(|l| clean_list(l, MAX_TAG_LEN, "a tag"))
                .transpose()?,
            add_tags: clean_list(&update.add_tags, MAX_TAG_LEN, "a tag")?,
            remove_tags: clean_list(&update.remove_tags, MAX_TAG_LEN, "a tag")?,
            people: update
                .people
                .as_deref()
                .map(|l| clean_list(l, MAX_TAG_LEN, "a person's name"))
                .transpose()?,
            add_people: clean_list(&update.add_people, MAX_TAG_LEN, "a person's name")?,
            remove_people: clean_list(&update.remove_people, MAX_TAG_LEN, "a person's name")?,
            place,
            rating,
            taken_at,
        })
    }

    fn apply(&self, f: &mut FileEntry) -> Result<()> {
        let u = self.update;
        if let Some(caption) = &u.caption {
            f.caption.clone_from(caption);
        }
        f.tags = merged(&f.tags, &self.tags, &self.add_tags, &self.remove_tags);
        if f.tags.len() > MAX_TAGS {
            return Err(Error::Invalid(format!("a file has at most {MAX_TAGS} tags")));
        }
        f.people = merged(&f.people, &self.people, &self.add_people, &self.remove_people);
        if f.people.len() > MAX_PEOPLE {
            return Err(Error::Invalid(format!("a file has at most {MAX_PEOPLE} people")));
        }
        if let Some(rating) = self.rating {
            f.rating = rating;
        }
        if let Some(favourite) = u.favourite {
            f.favourite = favourite;
        }
        if let Some(hidden) = u.hidden {
            f.hidden = hidden;
        }
        if let Some(place) = &self.place {
            f.place.clone_from(place);
        }
        for (key, value) in &u.properties {
            match value {
                Some(value) => {
                    f.properties.insert(key.clone(), value.clone());
                }
                None => {
                    f.properties.remove(key);
                }
            }
        }
        if f.properties.len() > MAX_PROPERTIES {
            return Err(Error::Invalid(format!("a file has at most {MAX_PROPERTIES} properties")));
        }
        match self.taken_at {
            Some(Some(corrected)) => {
                // The file's own time is kept the first time it is
                // corrected, so clearing the correction can go back to it.
                f.properties.entry(PROP_TAKEN_BY_FILE.into()).or_insert_with(|| {
                    PropValue::Text(f.media.taken_at.map(|t| t.to_string()).unwrap_or_default())
                });
                f.media.taken_at = Some(corrected);
                f.media.taken_local = None;
            }
            Some(None) => {
                if let Some(PropValue::Text(by_file)) = f.properties.remove(PROP_TAKEN_BY_FILE) {
                    f.media.taken_at = by_file.parse().ok();
                    f.media.taken_local = None;
                }
            }
            None => {}
        }
        Ok(())
    }
}

/// Trimmed, empty ones dropped, each within `max` characters, repeats
/// (without regard to case) dropped -- the first spelling wins.
fn clean_list(items: &[String], max: usize, what: &str) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if item.chars().count() > max {
            return Err(Error::Invalid(format!("{what} is at most {max} characters")));
        }
        if !out.iter().any(|x| same(x, item)) {
            out.push(item.to_string());
        }
    }
    Ok(out)
}

/// The list a change leaves: the replacement (or what the file has), with
/// `add` appended where not already there, and `remove` taken out.
fn merged(
    current: &[String],
    replace: &Option<Vec<String>>,
    add: &[String],
    remove: &[String],
) -> Vec<String> {
    let mut list = replace.clone().unwrap_or_else(|| current.to_vec());
    for item in add {
        if !list.iter().any(|x| same(x, item)) {
            list.push(item.clone());
        }
    }
    list.retain(|x| !remove.iter().any(|r| same(x, r)));
    list
}

fn check_property(key: &str, value: &PropValue) -> Result<()> {
    let too_long = |text: &str| text.chars().count() > MAX_PROPERTY_TEXT_LEN;
    let bad = match value {
        PropValue::Text(t) => too_long(t),
        PropValue::Number(n) => !n.is_finite(),
        PropValue::Bool(_) => false,
        PropValue::Date(d) => {
            if d.parse::<civil::Date>().is_err() && d.parse::<Timestamp>().is_err() {
                return Err(Error::Invalid(format!(
                    "{key}: a date is YYYY-MM-DD or an RFC 3339 time, not {d:?}"
                )));
            }
            false
        }
        PropValue::List(items) => {
            items.len() > MAX_PROPERTY_LIST || items.iter().any(|i| too_long(i))
        }
    };
    if bad {
        return Err(Error::Invalid(format!("{key}: that value is not allowed")));
    }
    Ok(())
}

/// When a picture was taken, as somebody types it: an RFC 3339 time, a local
/// date and time (`2024-03-01T14:22:05`), or a date alone, which is noon of
/// that day so no time zone can move it to another.
fn parse_taken(text: &str) -> Result<Timestamp> {
    let zone = TimeZone::system();
    if let Ok(t) = text.parse::<Timestamp>() {
        return Ok(t);
    }
    if let Ok(dt) = text.parse::<civil::DateTime>()
        && let Ok(z) = dt.to_zoned(zone.clone())
    {
        return Ok(z.timestamp());
    }
    if let Ok(date) = text.parse::<civil::Date>()
        && let Ok(z) = date.at(12, 0, 0, 0).to_zoned(zone)
    {
        return Ok(z.timestamp());
    }
    Err(Error::Invalid(format!(
        "{text:?} is not a time: use YYYY-MM-DD, YYYY-MM-DDTHH:MM:SS or an RFC 3339 time"
    )))
}

// ---- old attachments -------------------------------------------------------

/// What is known about a blob from the record that held it.
#[derive(Debug, Clone, Default)]
struct Meta {
    mime: String,
    filename: String,
    caption: String,
    width: Option<u32>,
    height: Option<u32>,
    duration_ms: Option<u64>,
}

/// A blob an old-style record holds.
struct Held {
    blob: BlobId,
    meta: Meta,
}

/// One distinct blob found by the scan: where it will go, by the first
/// record that held it.
struct LegacyBlob {
    origin: FileOrigin,
    /// The record's time, which the file takes as its own.
    at: Timestamp,
    /// The record's day, in this machine's time zone, for the folder.
    date: civil::Date,
    meta: Meta,
    len: u64,
}

/// Every old-style record, found.
#[derive(Default)]
struct LegacyScan {
    blobs: BTreeMap<BlobId, LegacyBlob>,
    entries: Vec<EntryId>,
    notes: Vec<NoteId>,
    conversations: Vec<ConversationId>,
    /// Messages holding a blob that is there.
    messages: u32,
    /// Blobs records name that the vault no longer holds.
    absent: u32,
    gone: BTreeSet<BlobId>,
}

impl LegacyScan {
    /// Note what a record holds. Answers whether it held anything that is
    /// still in the vault.
    fn note(
        &mut self,
        store: &dyn JournalStore,
        held: Vec<Held>,
        origin: FileOrigin,
        at: Timestamp,
        date: civil::Date,
    ) -> bool {
        let mut any = false;
        for h in held {
            if self.gone.contains(&h.blob) {
                continue;
            }
            if let Some(known) = self.blobs.get_mut(&h.blob) {
                any = true;
                // What the first record did not say, a later one may.
                if known.meta.filename.is_empty() {
                    known.meta.filename = h.meta.filename;
                }
                if known.meta.mime.is_empty() {
                    known.meta.mime = h.meta.mime;
                }
                if known.meta.caption.is_empty() {
                    known.meta.caption = h.meta.caption;
                }
                continue;
            }
            let Ok(len) = store.blob_len(h.blob) else {
                self.gone.insert(h.blob);
                self.absent += 1;
                continue;
            };
            any = true;
            self.blobs.insert(h.blob, LegacyBlob { origin, at, date, meta: h.meta, len });
        }
        any
    }
}

/// Walk every entry, note and assistant message for blobs. Under whatever
/// lock the caller holds: this reads, and writes nothing.
fn scan_legacy(store: &dyn JournalStore) -> Result<LegacyScan> {
    let zone = TimeZone::system();
    let mut scan = LegacyScan::default();
    for entry in store.all_entries()? {
        let held = held_in(&entry.body, &entry.attachments);
        if !held.is_empty()
            && scan.note(store, held, FileOrigin::Journal, entry.created_at, entry.local_date)
        {
            scan.entries.push(entry.id);
        }
    }
    if let Some(notes) = store.notes() {
        for note in notes.all_notes()? {
            let held = held_in(&note.body, &note.attachments);
            let date = note.created_at.to_zoned(zone.clone()).date();
            if !held.is_empty() && scan.note(store, held, FileOrigin::Notes, note.created_at, date)
            {
                scan.notes.push(note.id);
            }
        }
    }
    if let Some(agent) = store.agent() {
        for conversation in agent.list_conversations(&ConversationQuery::default())? {
            let origin = if conversation.run_id.is_some() || conversation.job_id.is_some() {
                FileOrigin::Jobs
            } else if conversation.via.is_some() {
                FileOrigin::Chat
            } else {
                FileOrigin::Assistant
            };
            let mut any = false;
            for message in agent.list_messages(conversation.id)? {
                // A message that already has files has been brought in (or
                // is one written since files existed): its attachments are
                // the copies a model is shown, not pictures waiting to move.
                if message.attachments.is_empty() || !message.files.is_empty() {
                    continue;
                }
                let held: Vec<Held> = message
                    .attachments
                    .iter()
                    .map(|a| Held { blob: a.blob, meta: meta_of(a) })
                    .collect();
                let date = message.created_at.to_zoned(zone.clone()).date();
                if scan.note(store, held, origin, message.created_at, date) {
                    scan.messages += 1;
                    any = true;
                }
            }
            if any {
                scan.conversations.push(conversation.id);
            }
        }
    }
    Ok(scan)
}

fn meta_of(a: &Attachment) -> Meta {
    Meta {
        mime: a.mime.clone(),
        filename: a.filename.clone(),
        caption: a.caption.clone(),
        width: a.width,
        height: a.height,
        duration_ms: a.duration_ms,
    }
}

/// Every blob a body's media nodes and an attachment list name, each once,
/// with what is known of it: the attachment's account first, the node's for
/// a blob the list does not have.
fn held_in(body: &RichDoc, attachments: &[Attachment]) -> Vec<Held> {
    let mut out: Vec<Held> = Vec::new();
    for a in attachments {
        if !out.iter().any(|h| h.blob == a.blob) {
            out.push(Held { blob: a.blob, meta: meta_of(a) });
        }
    }
    visit_media(&body.0, 0, &mut |attrs| {
        let Some(blob) = blob_of(attrs) else { return };
        let text =
            |key: &str| attrs.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
        let number =
            |key: &str| attrs.get(key).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
        match out.iter_mut().find(|h| h.blob == blob) {
            Some(h) => {
                if h.meta.caption.is_empty() {
                    h.meta.caption = text("caption");
                }
            }
            None => out.push(Held {
                blob,
                meta: Meta {
                    mime: text("mime"),
                    filename: text("filename"),
                    caption: text("caption"),
                    width: number("width"),
                    height: number("height"),
                    duration_ms: None,
                },
            }),
        }
    });
    out
}

/// The blob a media node's attributes name, if they name one.
fn blob_of(attrs: &Map<String, Value>) -> Option<BlobId> {
    attrs.get("blob").and_then(Value::as_str).and_then(|s| BlobId::parse(s).ok())
}

fn visit_media(node: &Value, depth: usize, f: &mut impl FnMut(&Map<String, Value>)) {
    if depth > MAX_DEPTH {
        return;
    }
    if node.get("type").and_then(Value::as_str) == Some(MEDIA_NODE)
        && let Some(attrs) = node.get("attrs").and_then(Value::as_object)
    {
        f(attrs);
    }
    if let Some(children) = node.get("content").and_then(Value::as_array) {
        for child in children {
            visit_media(child, depth + 1, f);
        }
    }
}

/// How records are to name a blob that is now a file.
struct Brought {
    id: FileId,
    name: String,
    mime: String,
    caption: String,
    width: Option<u32>,
    height: Option<u32>,
}

/// The name a legacy blob's file gets: the attachment's own filename when it
/// is one the folder can keep, else `picture-<short>.<ext>`.
fn legacy_name(filename: &str, blob: BlobId, mime: &str) -> String {
    let base = filename.rsplit(['/', '\\']).next().unwrap_or_default().trim();
    if let Ok(name) = validate_file_name(base) {
        return name;
    }
    let ext = match mime.split(';').next().unwrap_or_default().trim() {
        "image/jpeg" | "image/jpg" => ".jpg",
        "image/png" => ".png",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/heic" => ".heic",
        "image/avif" => ".avif",
        "video/mp4" => ".mp4",
        "video/quicktime" => ".mov",
        "video/webm" => ".webm",
        "audio/mpeg" => ".mp3",
        "audio/mp4" => ".m4a",
        "audio/wav" => ".wav",
        "audio/ogg" => ".ogg",
        "application/pdf" => ".pdf",
        m if m.starts_with("image/") => ".jpg",
        _ => ".bin",
    };
    format!("picture-{}{ext}", &blob.to_hex()[..8])
}

fn short_blob(blob: BlobId) -> String {
    format!("a stored file ({})", &blob.to_hex()[..8])
}

fn media_word(mime: &str) -> &'static str {
    match MediaKind::from_mime(mime) {
        MediaKind::Image => "image",
        MediaKind::Video => "video",
        MediaKind::Audio => "audio",
        MediaKind::File => "file",
    }
}

/// Rewrite a body's media nodes, and the attachment list that goes with it,
/// for blobs that are now files. Answers whether anything changed. See
/// [`Vault::bring_in_attachments`] for what each part becomes.
fn rewrite_record(
    body: &mut RichDoc,
    attachments: &mut Vec<Attachment>,
    brought: &BTreeMap<BlobId, Brought>,
) -> bool {
    let captions: BTreeMap<BlobId, String> = attachments
        .iter()
        .filter(|a| !a.caption.is_empty())
        .map(|a| (a.blob, a.caption.clone()))
        .collect();
    let mut shown = BTreeSet::new();
    let mut changed = rewrite_nodes(&mut body.0, 0, brought, &captions, &mut shown);
    let mut kept = Vec::with_capacity(attachments.len());
    for attachment in attachments.drain(..) {
        match brought.get(&attachment.blob) {
            Some(b) => {
                // One the body never showed is shown now, at the end.
                if shown.insert(attachment.blob) {
                    append_media_node(&mut body.0, b, &attachment.caption);
                }
                changed = true;
            }
            None => kept.push(attachment),
        }
    }
    *attachments = kept;
    changed
}

fn rewrite_nodes(
    node: &mut Value,
    depth: usize,
    brought: &BTreeMap<BlobId, Brought>,
    captions: &BTreeMap<BlobId, String>,
    shown: &mut BTreeSet<BlobId>,
) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let mut changed = false;
    if node.get("type").and_then(Value::as_str) == Some(MEDIA_NODE)
        && let Some(attrs) = node.get_mut("attrs").and_then(Value::as_object_mut)
        && let Some(blob) = blob_of(attrs)
        && let Some(b) = brought.get(&blob)
    {
        attrs.remove("blob");
        attrs.insert("file".into(), json!(b.id.to_string()));
        let has_caption =
            attrs.get("caption").and_then(Value::as_str).is_some_and(|c| !c.is_empty());
        if !has_caption && let Some(caption) = captions.get(&blob) {
            attrs.insert("caption".into(), json!(caption));
        }
        shown.insert(blob);
        changed = true;
    }
    if let Some(children) = node.get_mut("content").and_then(Value::as_array_mut) {
        for child in children {
            changed |= rewrite_nodes(child, depth + 1, brought, captions, shown);
        }
    }
    changed
}

fn append_media_node(doc: &mut Value, b: &Brought, caption: &str) {
    let mut attrs = Map::new();
    attrs.insert("file".into(), json!(b.id.to_string()));
    attrs.insert("kind".into(), json!(media_word(&b.mime)));
    attrs.insert("mime".into(), json!(b.mime));
    attrs.insert("filename".into(), json!(b.name));
    let caption = if caption.is_empty() { b.caption.as_str() } else { caption };
    attrs.insert("caption".into(), json!(caption));
    if let Some(width) = b.width {
        attrs.insert("width".into(), json!(width));
    }
    if let Some(height) = b.height {
        attrs.insert("height".into(), json!(height));
    }
    if let Some(content) = doc.get_mut("content").and_then(Value::as_array_mut) {
        content.push(json!({ "type": MEDIA_NODE, "attrs": Value::Object(attrs) }));
    }
}

/// A message gains the files its attachments became. Its attachments stay:
/// they are the copies a model is shown.
fn rewrite_message(
    message: &mut crate::agent::Message,
    brought: &BTreeMap<BlobId, Brought>,
) -> bool {
    let mut changed = false;
    for attachment in &message.attachments {
        if let Some(b) = brought.get(&attachment.blob)
            && !message.files.contains(&b.id)
        {
            message.files.push(b.id);
            changed = true;
        }
    }
    changed
}
