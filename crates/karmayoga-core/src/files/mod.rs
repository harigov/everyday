//! Files: the person's own files, kept as ordinary files in a folder they can
//! see, with a catalog of them in the vault.
//!
//! See `docs/plans/files.md` for the reasoning. In one paragraph: every app
//! that keeps a file -- a picture in a journal entry, a PDF handed to the
//! assistant, a video somebody drops into the Files app -- writes it into
//! **the files folder** (`~/Karma Yoga` unless the person chose another), as
//! a plain file that Syncthing, a photo viewer or a file manager can use
//! where it lies. The vault keeps a [`FileEntry`] per file: where it is, what
//! it is, what the file itself says (EXIF, a film's length), and what the
//! person added (a caption, tags, a rating, the people in it, albums). A few
//! files are kept **sealed** in the vault's blob store instead, by choice or
//! by an app's rule; they have no path and nothing outside Karma Yoga can
//! read them.
//!
//! # The folder is the truth for plain files
//!
//! Something else may change the folder at any moment: Syncthing bringing a
//! file from a phone, a file manager renaming one. The catalog follows the
//! folder ([`reconcile`]), never the other way round, and a file that is
//! gone is *missing*, never deleted, until the person says otherwise.
//!
//! # A folder that is not there is unavailable, never empty
//!
//! The folder may live on an encrypted volume or a mount that is locked or
//! not mounted. [`disk::Folder::state`] tells the difference with a marker
//! file in [`INTERNAL_DIR`]: no marker where the catalog expects files means
//! the folder is unavailable, and nothing -- the reconciler, the trash
//! sweep, a backup -- treats that as every file having been deleted.
//!
//! # Where things are
//!
//! - this module: the records and the value types every layer shares;
//! - [`disk`]: everything that touches the folder -- paths, copying in,
//!   the trash, walking it;
//! - [`reconcile`]: what changed on disk since the catalog last looked,
//!   worked out without touching either;
//! - [`crate::store::files`]: the catalog's storage;
//! - `crate::vault::files`: the facade every caller goes through;
//! - `karmayoga-service`'s `files`: reading metadata out of a file, drawing
//!   thumbnails, and the background task that keeps the catalog in step.

pub mod disk;
pub mod reconcile;

use crate::id::{AlbumId, BlobId, FileId};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The folder inside the files folder Karma Yoga keeps its own things in:
/// the marker, the trash, uploads in progress, decrypted copies opened in
/// another program. Never catalogued, never listed, never backed up.
pub const INTERNAL_DIR: &str = ".karmayoga";

/// The files folder's name when nobody chose one: `~/Karma Yoga`.
pub const DEFAULT_FOLDER_NAME: &str = "Karma Yoga";

/// How long a trashed file is kept before the sweep deletes it for good,
/// unless the settings say otherwise.
pub const DEFAULT_TRASH_DAYS: u32 = 30;

/// Limits on what a person can attach to a file, so one record cannot grow
/// without bound.
pub const MAX_TAGS: usize = 100;
pub const MAX_TAG_LEN: usize = 100;
pub const MAX_PEOPLE: usize = 100;
pub const MAX_CAPTION_LEN: usize = 20_000;
pub const MAX_PROPERTIES: usize = 200;
pub const MAX_PROPERTY_KEY_LEN: usize = 100;
pub const MAX_PROPERTY_TEXT_LEN: usize = 20_000;
pub const MAX_NAME_LEN: usize = 255;

/// Where a file's bytes are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum FileStorage {
    /// An ordinary file at `path`: relative to the files folder,
    /// `/`-separated, never absolute and never containing `..`. A trashed
    /// file's path is its place in the trash, under [`INTERNAL_DIR`].
    Plain { path: String },
    /// Sealed in the vault's blob store. No path; nothing but Karma Yoga can
    /// read it.
    Sealed { blob: BlobId },
}

/// Plain or sealed, without the details: what a filter asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum StorageKind {
    Plain,
    Sealed,
}

impl FileStorage {
    pub fn kind(&self) -> StorageKind {
        match self {
            FileStorage::Plain { .. } => StorageKind::Plain,
            FileStorage::Sealed { .. } => StorageKind::Sealed,
        }
    }
}

/// What sort of thing a file is, for filtering and for choosing a viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    Image,
    Video,
    Audio,
    /// Something to read: a PDF, a text or Markdown file, an office
    /// document, a spreadsheet, an e-book.
    Document,
    Other,
}

impl FileKind {
    /// Every kind, in the order a filter lists them.
    pub const ALL: [FileKind; 5] =
        [FileKind::Image, FileKind::Video, FileKind::Audio, FileKind::Document, FileKind::Other];

    /// The word stored in the catalog's clear `kind` column.
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::Image => "image",
            FileKind::Video => "video",
            FileKind::Audio => "audio",
            FileKind::Document => "document",
            FileKind::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        FileKind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Classify by MIME type first, the name's extension when the type is
    /// vague.
    pub fn classify(mime: &str, name: &str) -> Self {
        match mime.split('/').next().unwrap_or("") {
            "image" => return FileKind::Image,
            "video" => return FileKind::Video,
            "audio" => return FileKind::Audio,
            "text" => return FileKind::Document,
            _ => {}
        }
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        match ext.as_str() {
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "avif" | "bmp" | "tif"
            | "tiff" | "svg" | "dng" | "cr2" | "cr3" | "nef" | "arw" | "orf" | "rw2" | "raf"
            | "pef" | "srw" => FileKind::Image,
            "mp4" | "m4v" | "mov" | "mkv" | "webm" | "avi" | "3gp" | "mts" | "m2ts" => {
                FileKind::Video
            }
            "mp3" | "m4a" | "aac" | "flac" | "wav" | "ogg" | "oga" | "opus" | "aiff" | "aif" => {
                FileKind::Audio
            }
            "pdf" | "txt" | "md" | "markdown" | "csv" | "tsv" | "json" | "html" | "htm" | "rtf"
            | "doc" | "docx" | "odt" | "xls" | "xlsx" | "ods" | "ppt" | "pptx" | "odp" | "epub"
            | "pages" | "numbers" | "key" | "log" | "xml" | "yaml" | "yml" | "toml" => {
                FileKind::Document
            }
            _ if mime == "application/pdf" => FileKind::Document,
            _ => FileKind::Other,
        }
    }
}

/// Which app put a file in the catalog.
///
/// Decides the folder a new file goes in when the caller names none
/// ([`FileOrigin::default_dir`]), whether it is sealed by default
/// ([`FilesSettings::encrypted`]), and lets the Files app say where a file
/// came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum FileOrigin {
    /// Added in the Files app: picked, dropped or uploaded.
    Files,
    /// Found in the folder by the reconciler: put there by another program
    /// or brought by a sync tool from another device.
    Found,
    /// A picture or a video in a journal entry.
    Journal,
    /// A picture or a file in a note.
    Notes,
    /// Handed to the assistant in the composer, or saved by it.
    Assistant,
    /// Sent to the assistant in a chat (Matrix).
    Chat,
    /// Saved from a mail attachment.
    Mail,
    /// Written by a routine or a background job.
    Jobs,
    /// A clinical document or a meal's photograph kept by Health. Always
    /// sealed when Health adds it (Hari's call: what a clinician gave
    /// somebody stays in the vault), and out of the assistant's file tools,
    /// which have Health's own gates to answer to instead.
    Health,
}

impl FileOrigin {
    pub const ALL: [FileOrigin; 9] = [
        FileOrigin::Files,
        FileOrigin::Found,
        FileOrigin::Journal,
        FileOrigin::Notes,
        FileOrigin::Assistant,
        FileOrigin::Chat,
        FileOrigin::Mail,
        FileOrigin::Jobs,
        FileOrigin::Health,
    ];

    /// The word stored in the catalog's clear `origin` column.
    pub fn as_str(self) -> &'static str {
        match self {
            FileOrigin::Files => "files",
            FileOrigin::Found => "found",
            FileOrigin::Journal => "journal",
            FileOrigin::Notes => "notes",
            FileOrigin::Assistant => "assistant",
            FileOrigin::Chat => "chat",
            FileOrigin::Mail => "mail",
            FileOrigin::Jobs => "jobs",
            FileOrigin::Health => "health",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        FileOrigin::ALL.into_iter().find(|o| o.as_str() == s)
    }

    /// The folder a new file from this app goes in when the caller names
    /// none, relative to the files folder. `""` is the top.
    ///
    /// By month where an app adds files steadily (a journal's pictures, what
    /// the assistant was given), so no one folder grows to thousands.
    pub fn default_dir(self, today: jiff::civil::Date) -> String {
        let (y, m) = (today.year(), today.month());
        match self {
            FileOrigin::Files | FileOrigin::Found => String::new(),
            FileOrigin::Journal => format!("Journal/{y:04}/{m:02}"),
            FileOrigin::Notes => "Notes".to_string(),
            FileOrigin::Assistant | FileOrigin::Chat => format!("Assistant/{y:04}-{m:02}"),
            FileOrigin::Mail => "Mail".to_string(),
            FileOrigin::Jobs => "Assistant/Jobs".to_string(),
            // Only where one is written out of the vault: Health seals what
            // it keeps.
            FileOrigin::Health => "Health".to_string(),
        }
    }
}

/// Whether a file is where the catalog says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum FileStatus {
    Present,
    /// The folder was there and this file was not, since `since`. Kept, with
    /// everything the person added, until it turns up again (at the same
    /// path, or anywhere with the same contents) or the person forgets it.
    Missing {
        since: Timestamp,
    },
    /// In the trash since `at`. `from` is the path it had, to put it back
    /// to; empty for a sealed file.
    Trashed {
        at: Timestamp,
        from: String,
    },
}

impl FileStatus {
    /// The word stored in the catalog's clear `status` column.
    pub fn as_str(&self) -> &'static str {
        match self {
            FileStatus::Present => "present",
            FileStatus::Missing { .. } => "missing",
            FileStatus::Trashed { .. } => "trashed",
        }
    }

    pub fn is_trashed(&self) -> bool {
        matches!(self, FileStatus::Trashed { .. })
    }
}

/// A place on Earth, as a camera wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct GeoPoint {
    pub lat: f64,
    pub lon: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<f64>,
}

/// What a file says about itself, read out of it when it arrives and again
/// whenever its contents change. Never written back into the file.
///
/// Every field is optional: most files say little, and a document says
/// nothing at all.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// When the picture or the film was taken, as an instant: from the
    /// file's own offset when it wrote one, else read in this machine's
    /// time zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken_at: Option<Timestamp>,
    /// The wall-clock time the camera wrote, as it wrote it
    /// (`2024-03-01T14:22:05`), for showing the time the person saw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken_local: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_make: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lens: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub f_number: Option<f32>,
    /// As a photographer writes it: `1/250`, `2.5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iso: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focal_length_mm: Option<f32>,
    /// EXIF orientation, 1 to 8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orientation: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gps: Option<GeoPoint>,
    /// A film's video codec, or an audio file's: `avc1`, `hvc1`, `mp4a`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    /// A PDF's page count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,
}

/// A value somebody -- or an app -- attached to a file under a key of its
/// own. See [`FileEntry::properties`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum PropValue {
    Text(String),
    Number(f64),
    Bool(bool),
    /// `YYYY-MM-DD`, or an RFC 3339 instant.
    Date(String),
    List(Vec<String>),
}

/// One file in the catalog.
///
/// Sealed as a whole under its id. Beside it the store keeps a few clear
/// columns for the indexes -- see [`crate::store::files`] for exactly which
/// and why each is safe to leave readable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub id: FileId,
    pub storage: FileStorage,
    /// The file's name, with its extension. For a plain file, always the
    /// last segment of its path.
    pub name: String,
    pub kind: FileKind,
    pub mime: String,
    /// Bytes.
    pub size: u64,
    /// BLAKE3 of the contents, lowercase hex; empty until it has been read.
    /// What a file that moved while nobody watched is recognised by.
    #[serde(default)]
    pub hash: String,
    /// The file's own modification time (a sealed file's: when it was
    /// sealed).
    pub modified_at: Timestamp,
    /// When it entered the catalog.
    pub added_at: Timestamp,
    /// When this record last changed.
    pub updated_at: Timestamp,
    pub origin: FileOrigin,
    pub status: FileStatus,
    /// What the file says about itself.
    #[serde(default)]
    pub media: MediaInfo,
    /// A small JPEG of it, sealed in the blob store; `None` until drawn, and
    /// for files nothing can be drawn of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<BlobId>,

    // ---- what the person added ------------------------------------------
    #[serde(default)]
    pub caption: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// 1 to 5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
    #[serde(default)]
    pub favourite: bool,
    /// Left out of the timeline and every listing unless asked for.
    #[serde(default)]
    pub hidden: bool,
    /// The people in it, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<String>,
    /// A place's name: typed in, or looked up from the coordinates when the
    /// person allowed that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// The top of the stack this file belongs to -- a RAW file under its
    /// JPEG, a Live Photo's film under its picture, an edit under its
    /// original. `None` for a file that is not stacked and for a stack's top
    /// itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<FileId>,
    /// Anything else, under namespaced keys: `user.<name>` for what the
    /// person defines, `<app>.<name>` for what an app keeps, `index.<name>`
    /// for what indexing will add. Keys are lowercase ASCII words joined by
    /// dots, at least two parts ([`valid_property_key`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, PropValue>,
}

impl FileEntry {
    /// A plain file's path, `None` for a sealed one.
    pub fn path(&self) -> Option<&str> {
        match &self.storage {
            FileStorage::Plain { path } => Some(path),
            FileStorage::Sealed { .. } => None,
        }
    }

    /// The folder a plain file is in (`""` for the top), `None` for a
    /// sealed one.
    pub fn dir(&self) -> Option<&str> {
        self.path().map(|p| p.rsplit_once('/').map(|(d, _)| d).unwrap_or(""))
    }

    pub fn is_sealed(&self) -> bool {
        matches!(self.storage, FileStorage::Sealed { .. })
    }

    /// The time a timeline files it under: when it was taken, else when it
    /// last changed.
    pub fn when(&self) -> Timestamp {
        self.media.taken_at.unwrap_or(self.modified_at)
    }

    /// Every blob this record keeps alive: a sealed file's contents and its
    /// thumbnail.
    pub fn blob_refs(&self) -> impl Iterator<Item = BlobId> + '_ {
        let sealed = match &self.storage {
            FileStorage::Sealed { blob } => Some(*blob),
            FileStorage::Plain { .. } => None,
        };
        sealed.into_iter().chain(self.thumbnail)
    }
}

/// Whether `key` is a property key a file may carry: two or more parts of
/// `[a-z0-9_-]`, joined by dots, at most [`MAX_PROPERTY_KEY_LEN`] long.
pub fn valid_property_key(key: &str) -> bool {
    key.len() <= MAX_PROPERTY_KEY_LEN
        && key.split('.').count() >= 2
        && key.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        })
}

/// A named collection of files. A file may be in many; deleting an album
/// never deletes a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Album {
    pub id: AlbumId,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The file drawn for it; the first file when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<FileId>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// An album as the interface draws it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumView {
    pub album: Album,
    pub count: u64,
    /// The cover to draw: [`Album::cover`] if it is still in the album, else
    /// the first file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<FileEntry>,
}

/// The settings singleton.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct FilesSettings {
    /// The files folder, absolute. `None` until it has been set up; the
    /// first file anything keeps sets up [`disk::default_folder`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// The apps whose new files are sealed in the vault rather than written
    /// to the folder. Empty by default: most files are plain.
    #[serde(default)]
    pub encrypted: Vec<FileOrigin>,
    /// Days a trashed file is kept; 0 keeps them until the trash is
    /// emptied.
    #[serde(default = "default_trash_days")]
    pub trash_days: u32,
    /// Whether the assistant may find and read files it was not handed.
    /// What somebody hands it in the composer it always sees.
    #[serde(default = "yes")]
    pub assistant_access: bool,
    /// Whether a photo's coordinates may be sent to OpenStreetMap
    /// (Nominatim) to name the place it was taken. Off unless switched on:
    /// it puts where somebody was on the network.
    #[serde(default)]
    pub name_places: bool,
}

fn default_trash_days() -> u32 {
    DEFAULT_TRASH_DAYS
}

fn yes() -> bool {
    true
}

impl Default for FilesSettings {
    fn default() -> Self {
        Self {
            folder: None,
            encrypted: Vec::new(),
            trash_days: DEFAULT_TRASH_DAYS,
            assistant_access: true,
            name_places: false,
        }
    }
}

/// Whether the files folder can be used right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum FolderState {
    /// Never set up. `suggested` is the folder that would be.
    NotSetUp {
        suggested: String,
    },
    Available {
        path: String,
    },
    /// Set up, and not usable now: not there, not a folder, not readable,
    /// or there without its marker while the catalog expects files in it --
    /// an encrypted volume that is locked, a drive that is not mounted.
    Unavailable {
        path: String,
        reason: String,
    },
}

impl FolderState {
    pub fn is_available(&self) -> bool {
        matches!(self, FolderState::Available { .. })
    }
}

/// Which files by status a listing wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StatusFilter {
    /// Present and missing: everything not in the trash.
    #[default]
    Live,
    Present,
    Missing,
    Trashed,
    All,
}

/// The order of a listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileSort {
    /// [`FileEntry::when`]: taken, else modified.
    #[default]
    When,
    Added,
    Name,
    Size,
}

/// What the store can filter on by itself: clear columns only. See
/// [`FileQuery`] for what the vault adds on top.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileFilter {
    /// Any of these kinds; every kind when empty.
    #[serde(default)]
    pub kinds: Vec<FileKind>,
    /// Plain files directly in this folder (`""` is the top).
    #[serde(default)]
    pub dir: Option<String>,
    /// Plain files anywhere under this folder.
    #[serde(default)]
    pub under: Option<String>,
    #[serde(default)]
    pub album: Option<AlbumId>,
    /// Only these files.
    #[serde(default)]
    pub ids: Option<Vec<FileId>>,
    #[serde(default)]
    pub favourite: bool,
    #[serde(default)]
    pub min_rating: Option<u8>,
    #[serde(default)]
    pub origin: Option<FileOrigin>,
    /// None of these origins: what the assistant's file tools leave out
    /// (Health's, which only the health tools reach).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_origins: Vec<FileOrigin>,
    #[serde(default)]
    pub status: StatusFilter,
    #[serde(default)]
    pub storage: Option<StorageKind>,
    /// [`FileEntry::when`] at or after.
    #[serde(default)]
    pub from: Option<Timestamp>,
    /// [`FileEntry::when`] before.
    #[serde(default)]
    pub to: Option<Timestamp>,
    /// Include files marked hidden.
    #[serde(default)]
    pub include_hidden: bool,
    /// Leave out every file that is under another in a stack.
    #[serde(default)]
    pub stack_tops_only: bool,
    /// Only this stack: its top and every file under it.
    #[serde(default)]
    pub stack: Option<FileId>,
    #[serde(default)]
    pub sort: FileSort,
    /// Newest, largest or Z first. Ties are broken by id in the same
    /// direction.
    #[serde(default)]
    pub descending: bool,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: u32,
}

/// A listing as the interface and the assistant ask for one: the store's
/// filter, plus what only the sealed record can answer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileQuery {
    #[serde(flatten)]
    pub filter: FileFilter,
    /// Words, every one of which must appear (case-insensitively) in the
    /// name, the caption, a tag, a person, the place or the path.
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub person: Option<String>,
    #[serde(default)]
    pub place: Option<String>,
    /// Only files an app added that nothing refers to any more.
    #[serde(default)]
    pub unused: bool,
}

/// One page of a listing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilePage {
    pub files: Vec<FileEntry>,
    /// How many match, over every page.
    pub total: u64,
    /// How many files are under each listed file that is the top of a
    /// stack, by its id. A file that tops no stack is absent.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stack_counts: BTreeMap<FileId, u64>,
}

/// A folder inside the files folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderInfo {
    pub name: String,
    /// Relative to the files folder.
    pub path: String,
    /// What is directly inside it, files and folders, as the disk says.
    pub items: u32,
    pub modified_at: Option<Timestamp>,
}

/// One folder's contents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderListing {
    pub dir: String,
    pub state: FolderState,
    /// From the disk, by name.
    pub folders: Vec<FolderInfo>,
    /// From the catalog: the plain files in this folder, by name, missing
    /// ones included.
    pub files: Vec<FileEntry>,
}

/// How a caller wants a new file kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOptions {
    /// The folder to put it in, relative to the files folder. `None` is the
    /// origin's own ([`FileOrigin::default_dir`]).
    #[serde(default)]
    pub dir: Option<String>,
    /// The name to give it; the source's own when `None`. A clash gets
    /// ` (2)`, ` (3)` before the extension, never an overwrite.
    #[serde(default)]
    pub name: Option<String>,
    pub origin: FileOrigin,
    /// Sealed in the vault rather than written to the folder. `None` follows
    /// [`FilesSettings::encrypted`] for the origin.
    #[serde(default)]
    pub encrypted: Option<bool>,
}

impl ImportOptions {
    pub fn new(origin: FileOrigin) -> Self {
        Self { dir: None, name: None, origin, encrypted: None }
    }
}

/// An upload in progress, as [`crate::Vault::begin_upload`] hands it out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadTicket {
    /// Opaque; names the upload in every later call.
    pub upload: String,
    /// Bytes already received: 0 for a new upload.
    pub received: u64,
}

/// A change to what the person added to one or more files. Every field left
/// `None` (or empty) is left alone.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileUpdate {
    #[serde(default)]
    pub caption: Option<String>,
    /// Replace the tags.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub add_tags: Vec<String>,
    #[serde(default)]
    pub remove_tags: Vec<String>,
    /// 1 to 5; 0 clears it.
    #[serde(default)]
    pub rating: Option<u8>,
    #[serde(default)]
    pub favourite: Option<bool>,
    #[serde(default)]
    pub hidden: Option<bool>,
    /// Replace the people.
    #[serde(default)]
    pub people: Option<Vec<String>>,
    #[serde(default)]
    pub add_people: Vec<String>,
    #[serde(default)]
    pub remove_people: Vec<String>,
    /// `""` clears it.
    #[serde(default)]
    pub place: Option<String>,
    /// Set a property, or remove it with `null`.
    #[serde(default)]
    pub properties: BTreeMap<String, Option<PropValue>>,
    /// Correct when it was taken; `""` goes back to what the file says.
    #[serde(default)]
    pub taken_at: Option<String>,
}

/// What a file can be referred to by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RefKind {
    /// A journal entry's body.
    Entry,
    /// A note's body.
    Note,
    /// A message in a conversation with the assistant.
    Message,
    /// A clinical document in Health: its pages.
    HealthDocument,
    /// A meal in Health: its photograph.
    Meal,
}

impl RefKind {
    /// The word stored in the references table.
    pub fn as_str(self) -> &'static str {
        match self {
            RefKind::Entry => "entry",
            RefKind::Note => "note",
            RefKind::Message => "message",
            RefKind::HealthDocument => "health_document",
            RefKind::Meal => "meal",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [RefKind::Entry, RefKind::Note, RefKind::Message, RefKind::HealthDocument, RefKind::Meal]
            .into_iter()
            .find(|k| k.as_str() == s)
    }
}

/// A record that refers to files.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefOwner {
    pub kind: RefKind,
    /// The record's id, as text.
    pub id: String,
}

impl RefOwner {
    pub fn new(kind: RefKind, id: impl std::fmt::Display) -> Self {
        Self { kind, id: id.to_string() }
    }
}

/// "Used in": one record that refers to a file, named for the person.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileUse {
    pub owner: RefOwner,
    /// The entry's title or first line, the note's title, the conversation's
    /// title.
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Timestamp>,
    /// The conversation a message is in, for opening it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// What the catalog holds, for the storage line and Settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesUsage {
    /// Live files (present and missing).
    pub files: u64,
    pub bytes: u64,
    pub by_kind: Vec<KindUsage>,
    pub sealed_files: u64,
    pub sealed_bytes: u64,
    pub trashed_files: u64,
    pub trashed_bytes: u64,
    pub missing_files: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KindUsage {
    pub kind: FileKind,
    pub files: u64,
    pub bytes: u64,
}

/// A name and how many live files carry it: a tag, a person, a place, a
/// camera.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Facet {
    pub name: String,
    pub count: u64,
}

/// What the Photos view's sidebar lists.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesFacets {
    pub tags: Vec<Facet>,
    pub people: Vec<Facet>,
    pub places: Vec<Facet>,
    /// Years taken, newest first, as text (`"2024"`).
    pub years: Vec<Facet>,
    pub cameras: Vec<Facet>,
}

/// One plain file as the reconciler compares it against the disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexRow {
    pub id: FileId,
    pub path: String,
    pub size: u64,
    /// Microseconds since the epoch.
    pub modified_us: i64,
    pub hash: String,
    pub missing: bool,
}

/// What one pass of the reconciler did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub added: u32,
    pub changed: u32,
    pub moved: u32,
    pub missing: u32,
    pub returned: u32,
    pub unchanged: u32,
    /// The folder was not available, so nothing was compared.
    #[serde(default)]
    pub unavailable: bool,
    /// So many files vanished at once that the pass stopped short of
    /// marking them missing: the folder is probably not what it was (a
    /// mount half there, the wrong drive). Said in Settings; the next pass
    /// looks again.
    #[serde(default)]
    pub held_back: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// What [`reconcile`] and hashing found, for [`crate::Vault::apply_scan`]
/// to write in one go.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanChanges {
    /// Files on disk the catalog has never seen.
    pub new: Vec<NewFile>,
    /// Catalogued files whose size or time changed, freshly hashed.
    pub changed: Vec<ChangedFile>,
    /// Catalogued files found at another path, recognised by their hash.
    pub moved: Vec<MovedFile>,
    /// Present files that are not on disk any more.
    pub vanished: Vec<FileId>,
    /// Missing files that are back at their own path.
    pub returned: Vec<FileId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFile {
    pub path: String,
    pub size: u64,
    pub modified_us: i64,
    pub hash: String,
    pub mime: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub id: FileId,
    pub size: u64,
    pub modified_us: i64,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovedFile {
    pub id: FileId,
    pub path: String,
    pub size: u64,
    pub modified_us: i64,
}

/// What [`crate::Vault::apply_scan`] wrote: the report, and the files whose
/// contents are new to the catalog and want their metadata read and a
/// thumbnail drawn.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppliedScan {
    pub report: ScanReport,
    pub to_probe: Vec<FileId>,
}

/// Pictures and files kept the old way -- sealed blobs inside journal
/// entries, notes and assistant messages -- that could be brought into the
/// files folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacySummary {
    pub entries: u32,
    pub notes: u32,
    pub messages: u32,
    /// Distinct blobs among them.
    pub files: u32,
    pub bytes: u64,
}

/// What bringing them in did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyReport {
    pub files: u32,
    pub bytes: u64,
    pub entries: u32,
    pub notes: u32,
    pub messages: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_origins_round_trip_through_their_column_words() {
        for k in FileKind::ALL {
            assert_eq!(FileKind::parse(k.as_str()), Some(k));
        }
        for o in FileOrigin::ALL {
            assert_eq!(FileOrigin::parse(o.as_str()), Some(o));
        }
    }

    #[test]
    fn classify_prefers_the_declared_type_then_the_extension() {
        assert_eq!(FileKind::classify("image/jpeg", "x.bin"), FileKind::Image);
        assert_eq!(FileKind::classify("application/octet-stream", "IMG_1.HEIC"), FileKind::Image);
        assert_eq!(FileKind::classify("application/pdf", "report"), FileKind::Document);
        assert_eq!(FileKind::classify("application/octet-stream", "a.zip"), FileKind::Other);
    }

    #[test]
    fn property_keys_are_namespaced_lowercase_words() {
        assert!(valid_property_key("user.mood"));
        assert!(valid_property_key("journal.entry_count"));
        assert!(!valid_property_key("mood"));
        assert!(!valid_property_key("User.mood"));
        assert!(!valid_property_key("user..mood"));
        assert!(!valid_property_key("user.mo od"));
    }

    #[test]
    fn a_plain_files_dir_is_its_paths_parent() {
        let mut f = sample();
        assert_eq!(f.dir(), Some("Photos/2024"));
        f.storage = FileStorage::Plain { path: "top.txt".into() };
        assert_eq!(f.dir(), Some(""));
    }

    fn sample() -> FileEntry {
        let now = Timestamp::now();
        FileEntry {
            id: FileId::new(),
            storage: FileStorage::Plain { path: "Photos/2024/a.jpg".into() },
            name: "a.jpg".into(),
            kind: FileKind::Image,
            mime: "image/jpeg".into(),
            size: 1,
            hash: String::new(),
            modified_at: now,
            added_at: now,
            updated_at: now,
            origin: FileOrigin::Files,
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
}
