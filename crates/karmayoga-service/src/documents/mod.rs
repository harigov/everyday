//! Documents: what the service adds to [`karmayoga_core::documents`] --
//! reading a PDF, an EPUB, Markdown, text or a web page into a
//! [`DocText`], keeping it, and searching every document at once.
//!
//! - [`parse`]: the readers, one per [`DocFormat`], pure Rust, each run
//!   under `catch_unwind` with a size cap: a hostile file costs its own
//!   parse and nothing else.
//! - [`cache`]: every parsed document, sealed, in the cache directory --
//!   never inside the files folder, which a sync tool carries to every
//!   device, and never in the clear, since the folder may be on an encrypted
//!   volume that the cache directory is not. Keyed by the file's contents, so
//!   a moved or renamed file is not parsed again.
//! - [`task`]: the background pass that keeps the search index
//!   (`karmayoga-docindex`) in step with the files folder and
//!   [`FilesSettings`](karmayoga_core::files::FilesSettings)' choice of
//!   folders.
//!
//! [`DocumentsState`] is one vault's cache and index, opened on first use
//! ([`Service::documents`]) and dropped when the vault locks ([`Slot::close`]).
//! [`ToolDocuments`] hands it to the assistant's tools as the core's
//! [`Documents`]. The commands are in `crate::domains::documents`.
//!
//! # Reading a document
//!
//! [`DocumentsState::text`] answers from, in order, a few documents kept in
//! memory (the reader asks for the same one again and again: its outline, then
//! a chapter, then the next), the sealed cache, and then a parse of the file's
//! own bytes. The same file is parsed by one caller at a time, so the outline
//! and the first chapter of a three-hundred-page PDF, asked for together by
//! the reader, are one parse and not two.
//!
//! What is read is refused with words for the person and the model: a file
//! that is not a document, one that is in the trash or has gone missing, and
//! Health's, which only Health's own gates reach.
//!
//! # A failed index does not stop reading
//!
//! The search index is a convenience on top of the documents. When it will
//! not open, [`DocumentsState`] has none, searching answers nothing, and every
//! document can still be read.

pub mod cache;
pub mod parse;
pub mod task;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine;
use jiff::Timestamp;
use karmayoga_core::Vault;
use karmayoga_core::documents::{
    DocBlockKind, DocBlockView, DocChapterView, DocFailure, DocFormat, DocIndexStatus, DocOutline,
    DocSearch, DocSearchHit, DocSearchPage, DocText, Documents, READER_BLOCK_KEY, READER_PAGE_KEY,
    ReaderPlace,
};
use karmayoga_core::error::{Error, Result as CoreResult};
use karmayoga_core::files::{FileEntry, FileOrigin, FileStatus, PropValue};
use karmayoga_core::id::FileId;
use karmayoga_docindex::DocIndex;

use crate::error::{CommandError, CommandResult};
use crate::service::Service;

use self::cache::DocCache;

/// How many parsed documents are kept in memory: the one being read, and the
/// few somebody flips between.
const RECENT_TEXTS: usize = 4;

/// How much of the index's segments are kept decrypted in memory. Less than
/// mail's: a passage is a few hundred words, and a search touches few of them.
const INDEX_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// The most a search answers at once.
const MAX_SEARCH_LIMIT: usize = 100;

/// The most hits looked at, past the first page, to make up a page after the
/// ones that are no longer wanted have been left out. See
/// [`DocumentsState::search`].
const MAX_SEARCH_PULL: usize = 400;

/// The largest picture drawn inline, and the most pictures of one chapter
/// together: a book of photographs would otherwise send the reader the whole
/// of itself as text.
const MAX_PICTURE_BYTES: usize = 4 * 1024 * 1024;
const MAX_CHAPTER_PICTURE_BYTES: usize = 24 * 1024 * 1024;

/// The picture types the reader draws. An SVG is included: it is drawn as an
/// image, which never runs a script.
const PICTURE_TYPES: [&str; 5] =
    ["image/jpeg", "image/png", "image/gif", "image/webp", "image/svg+xml"];

/// The open vault's [`DocumentsState`], if anything has asked for it since
/// the vault unlocked.
#[derive(Default)]
pub struct Slot(Mutex<Option<Arc<DocumentsState>>>);

impl Slot {
    /// Drop the state: the vault is locking, and the cache's and the index's
    /// keys go with it.
    pub fn close(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

/// What the indexing task has done and is doing, for [`DocumentsState::status`]
/// and for [`DocumentsState::search`]'s `indexing`.
#[derive(Default)]
pub(crate) struct Progress {
    /// Documents in the index. `None` until it has been counted, which is on
    /// the first look at it, not when the state is opened.
    pub indexed: Option<u64>,
    /// Documents the current pass has yet to read.
    pub pending: u64,
    /// A pass is under way.
    pub running: bool,
    /// Documents that could not be read, by file: the version that failed
    /// and why. A version that failed is not tried again.
    pub failed: BTreeMap<FileId, (String, String)>,
    pub last_pass: Option<Timestamp>,
}

/// One vault's parsed documents and search index.
pub struct DocumentsState {
    /// The vault this was opened for, so a different vault gets its own.
    vault_path: PathBuf,
    cache: DocCache,
    /// `None` when the index would not open: see the module docs.
    index: Option<DocIndex>,
    /// The documents read lately, newest first.
    recent: Mutex<VecDeque<(String, Arc<DocText>)>>,
    flights: Flights,
    progress: Mutex<Progress>,
    /// Raised by a reindex, so a pass that is under way knows what it has
    /// found out about the index has been cleared since.
    epoch: AtomicU64,
}

/// Why a document could not be loaded, by whose doing.
enum LoadError {
    /// It is not something to read at all: see [`readable`].
    Refused(Error),
    /// The file could not be got at -- the folder is away, the vault locked.
    /// Likely to be otherwise next time.
    Unreadable(Error),
    /// The file is what is wrong with it: too large, or not a document inside.
    Unparsable(String),
}

impl LoadError {
    fn into_error(self) -> Error {
        match self {
            LoadError::Refused(e) | LoadError::Unreadable(e) => e,
            LoadError::Unparsable(why) => Error::Invalid(why),
        }
    }
}

/// One parse at a time of each document: see the module docs.
#[derive(Default)]
struct Flights(Mutex<HashMap<String, Arc<Mutex<()>>>>);

impl Flights {
    fn gate(&self, key: &str) -> Arc<Mutex<()>> {
        let mut all = self.0.lock().unwrap_or_else(|e| e.into_inner());
        all.entry(key.to_string()).or_default().clone()
    }

    /// Forget `key`'s gate unless somebody else is waiting at it.
    fn done(&self, key: &str, gate: Arc<Mutex<()>>) {
        let mut all = self.0.lock().unwrap_or_else(|e| e.into_inner());
        // The map's own handle and the one being given back; a third is a
        // caller that came for the same document while it was being read.
        if Arc::strong_count(&gate) <= 2 {
            all.remove(key);
        }
    }
}

/// The format of `file` when it is a document that may be read; why not
/// when it is not. In words for the model, which will say them to the
/// person.
pub fn readable(file: &FileEntry) -> CoreResult<DocFormat> {
    if file.origin == FileOrigin::Health {
        return Err(Error::Invalid(format!(
            "{} is one of Health's files, which documents are not read from: \
             what a clinical document says is Health's to show",
            file.name
        )));
    }
    let Some(format) = DocFormat::of_file(file) else {
        return Err(Error::Invalid(format!(
            "{} is not a document that can be read here: PDF, EPUB, Markdown, plain text and \
             web page files are",
            file.name
        )));
    };
    match file.status {
        FileStatus::Present => Ok(format),
        FileStatus::Trashed { .. } => Err(Error::Invalid(format!(
            "{} is in the trash; it has to be restored before it can be read",
            file.name
        ))),
        FileStatus::Missing { .. } => Err(Error::Invalid(format!(
            "{} has gone missing from the files folder, so it cannot be read now",
            file.name
        ))),
    }
}

impl DocumentsState {
    /// Open `vault`'s cache and index. The cache must open; the index
    /// need not.
    pub(crate) fn open(vault: &Vault) -> CoreResult<Self> {
        let cache = DocCache::open(vault)?;
        let index = match DocIndex::open(cache.index_dir(), cache.index_cipher(), INDEX_CACHE_BYTES)
        {
            Ok(index) => Some(index),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "documents: the search index would not open; documents can still be read"
                );
                None
            }
        };
        Ok(Self {
            vault_path: vault.path().to_path_buf(),
            cache,
            index,
            recent: Mutex::new(VecDeque::new()),
            flights: Flights::default(),
            progress: Mutex::new(Progress::default()),
            epoch: AtomicU64::new(0),
        })
    }

    pub(crate) fn cache(&self) -> &DocCache {
        &self.cache
    }

    pub(crate) fn index(&self) -> Option<&DocIndex> {
        self.index.as_ref()
    }

    pub(crate) fn progress(&self) -> MutexGuard<'_, Progress> {
        self.progress.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Raised each time the index is cleared.
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// What the index records a file at when it is indexed as the catalog has
    /// it: the cache's name for its contents.
    pub(crate) fn version_of(&self, file: &FileEntry) -> String {
        self.cache.name(&DocCache::identity(file))
    }

    /// The document's text: from the cache when it holds this version of
    /// the file, else parsed now and cached. Blocking.
    pub fn text(&self, vault: &Vault, file: &FileEntry) -> CoreResult<Arc<DocText>> {
        self.load_as(vault, file, true).map_err(LoadError::into_error)
    }

    /// [`DocumentsState::text`], keeping apart why it failed: the indexing
    /// task records a file that cannot be parsed and tries again for one that
    /// could not be reached. `keep` says whether a document read now is kept
    /// in memory for the next caller; the indexer says no, so a pass over a
    /// library does not push out the book that is open in the reader.
    fn load_as(
        &self,
        vault: &Vault,
        file: &FileEntry,
        keep: bool,
    ) -> Result<Arc<DocText>, LoadError> {
        let format = readable(file).map_err(LoadError::Refused)?;
        let (name, mut bytes) = self.name_of(vault, file)?;
        if let Some(text) = self.recent_text(&name) {
            return Ok(text);
        }

        let gate = self.flights.gate(&name);
        let loaded = {
            let _turn = gate.lock().unwrap_or_else(|e| e.into_inner());
            // Another caller may have read it while this one waited.
            match self.recent_text(&name) {
                Some(text) => Ok(text),
                None => self.read_through(vault, file, format, &name, &mut bytes, keep),
            }
        };
        self.flights.done(&name, gate);
        loaded
    }

    /// The cache's name for `file`'s contents, and the bytes if working it
    /// out meant reading them.
    ///
    /// The catalog's hash is trusted while the disk still has the size and
    /// time it recorded. When it does not -- a file edited since the last
    /// pass of the folder's reconciler -- the bytes are read and hashed here,
    /// so a document being edited is never shown as it was.
    fn name_of(
        &self,
        vault: &Vault,
        file: &FileEntry,
    ) -> Result<(String, Option<Vec<u8>>), LoadError> {
        // What the interface versions a file by: its hash, or its modified
        // time while it has none.
        let version =
            if file.hash.is_empty() { file.modified_at.to_string() } else { file.hash.clone() };
        let current = vault.file_version_is_current(file.id, &version).unwrap_or(false);
        if current {
            return Ok((self.cache.name(&DocCache::identity(file)), None));
        }
        let bytes = read_bytes(vault, file)?;
        let hash = blake3::hash(&bytes).to_hex().to_string();
        Ok((self.cache.name(&DocCache::identity_of_hash(&hash)), Some(bytes)))
    }

    /// The cache, then the file: what is left to try when memory had nothing.
    fn read_through(
        &self,
        vault: &Vault,
        file: &FileEntry,
        format: DocFormat,
        name: &str,
        bytes: &mut Option<Vec<u8>>,
        keep: bool,
    ) -> Result<Arc<DocText>, LoadError> {
        let settle = |text: DocText| if keep { self.remember(name, text) } else { Arc::new(text) };
        if let Some(text) = self.cache.get(name) {
            return Ok(settle(text));
        }
        let bytes = match bytes.take() {
            Some(bytes) => bytes,
            None => read_bytes(vault, file)?,
        };
        let text = parse::parse(format, &bytes)
            .map_err(|e| LoadError::Unparsable(format!("{} could not be read: {e}", file.name)))?;
        if let Err(e) = self.cache.put(name, &text) {
            // Read and shown all the same: it is only parsed again next time.
            tracing::warn!(error = %e, "documents: could not keep a parsed document");
        }
        Ok(settle(text))
    }

    fn recent_text(&self, name: &str) -> Option<Arc<DocText>> {
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        let at = recent.iter().position(|(have, _)| have == name)?;
        let entry = recent.remove(at)?;
        let text = entry.1.clone();
        recent.push_front(entry);
        Some(text)
    }

    fn remember(&self, name: &str, text: DocText) -> Arc<DocText> {
        let text = Arc::new(text);
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        recent.push_front((name.to_string(), text.clone()));
        recent.truncate(RECENT_TEXTS);
        text
    }

    /// What the reader opens on.
    pub fn outline(&self, vault: &Vault, file: &FileEntry) -> CoreResult<DocOutline> {
        let text = self.text(vault, file)?;
        let indexed = self
            .index
            .as_ref()
            .and_then(|index| index.version(file.id).ok().flatten())
            .is_some_and(|version| version == self.version_of(file));
        Ok(DocOutline {
            file: file.id,
            format: text.format,
            pages: text.pages,
            meta: text.meta.clone(),
            sections: text.sections.clone(),
            chapters: text.chapters.clone(),
            blocks: u32::try_from(text.blocks.len()).unwrap_or(u32::MAX),
            words: text.word_count() as u64,
            warnings: text.warnings.clone(),
            position: place_of(file),
            indexed,
        })
    }

    /// One chapter's blocks, pictures read out of the file as `data:`
    /// addresses.
    pub fn chapter(
        &self,
        vault: &Vault,
        file: &FileEntry,
        chapter: u32,
    ) -> CoreResult<DocChapterView> {
        let text = self.text(vault, file)?;
        // The file is read again only if the chapter has a picture to fetch
        // out of it, and then once.
        let mut whole: Option<Option<Vec<u8>>> = None;
        let format = text.format;
        view_of(file, &text, chapter, |path| {
            let bytes = whole.get_or_insert_with(|| read_bytes(vault, file).ok());
            parse::read_resource(format, bytes.as_deref()?, path)
        })
    }

    /// Search every indexed document.
    ///
    /// The index is asked first and the catalog second: a hit for a file that
    /// is gone, trashed, Health's, or no longer one the settings index is left
    /// out, so what the index has not yet been told ( the next pass is
    /// minutes away) is never shown. The page is made up from further hits
    /// when some were left out, so a page is as full as the matches allow; the
    /// offset counts from the first hit the index has, which can be a hit or
    /// two off where something was left out before it.
    pub fn search(&self, vault: &Vault, query: &DocSearch) -> CoreResult<DocSearchPage> {
        let indexing = self.working();
        let nothing = DocSearchPage { indexing, ..DocSearchPage::default() };
        let Some(index) = &self.index else { return Ok(nothing) };
        let settings = vault.files_settings()?;
        if !settings.index_documents || query.text.trim().is_empty() {
            return Ok(nothing);
        }
        let limit = query.limit.clamp(1, MAX_SEARCH_LIMIT);

        let mut wanted: HashMap<FileId, bool> = HashMap::new();
        let mut hits: Vec<DocSearchHit> = Vec::with_capacity(limit);
        let (mut at, mut matching, mut left_out) = (query.offset, 0, 0usize);
        while hits.len() < limit && at < query.offset + MAX_SEARCH_PULL {
            let (found, total) = index.search(&query.text, &query.files, limit, at)?;
            matching = total;
            if found.is_empty() {
                break;
            }
            at += found.len();
            for hit in found {
                let kept = *wanted.entry(hit.file).or_insert_with(|| {
                    vault.file(hit.file).is_ok_and(|file| settings.indexes(&file))
                });
                if !kept {
                    left_out += 1;
                } else if hits.len() < limit {
                    hits.push(DocSearchHit {
                        file: hit.file,
                        block: hit.block,
                        page: hit.page,
                        section: hit.section,
                        snippet: hit.snippet,
                        score: hit.score,
                    });
                }
            }
            if at >= matching {
                break;
            }
        }
        Ok(DocSearchPage { hits, total: matching.saturating_sub(left_out), indexing })
    }

    /// Whether the indexing task has work under way or waiting.
    fn working(&self) -> bool {
        let progress = self.progress();
        progress.running || progress.pending > 0
    }

    /// How the index stands.
    pub fn status(&self, vault: &Vault) -> DocIndexStatus {
        let enabled = vault.files_settings().is_ok_and(|s| s.index_documents);
        let known = self.progress().indexed;
        let indexed = known.unwrap_or_else(|| {
            let counted = self
                .index
                .as_ref()
                .and_then(|index| index.indexed().ok())
                .map_or(0, |all| all.len() as u64);
            self.progress().indexed = Some(counted);
            counted
        });
        let progress = self.progress();
        DocIndexStatus {
            enabled,
            indexed,
            pending: progress.pending,
            failed: progress
                .failed
                .iter()
                .map(|(file, (_, reason))| DocFailure { file: *file, reason: reason.clone() })
                .collect(),
            running: progress.running,
            last_pass: progress.last_pass,
        }
    }

    /// Empty the index and read every document into it again, at the next
    /// pass (which this brings forward).
    pub fn reindex(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        if let Some(index) = &self.index
            && let Err(e) = index.clear()
        {
            tracing::warn!(error = %e, "documents: could not empty the search index");
        }
        {
            let mut progress = self.progress();
            progress.failed.clear();
            progress.indexed = Some(0);
        }
        task::wake();
    }
}

/// The file's bytes, whole: a plain file from the folder, a sealed one
/// decrypted. Refused past [`parse::MAX_DOCUMENT_BYTES`], before any of it is
/// read.
fn read_bytes(vault: &Vault, file: &FileEntry) -> Result<Vec<u8>, LoadError> {
    let len = vault.file_len(file.id).map_err(LoadError::Unreadable)?;
    if len > parse::MAX_DOCUMENT_BYTES {
        return Err(LoadError::Unparsable(format!(
            "{} is {} MB, which is more than the {} MB a document is read up to",
            file.name,
            len / (1024 * 1024),
            parse::MAX_DOCUMENT_BYTES / (1024 * 1024)
        )));
    }
    vault.read_file_range(file.id, 0, len).map_err(LoadError::Unreadable)
}

/// Where somebody left off, from the file's properties.
fn place_of(file: &FileEntry) -> Option<ReaderPlace> {
    let number = |key: &str| match file.properties.get(key) {
        Some(PropValue::Number(n)) if n.is_finite() && *n >= 0.0 => Some(*n as u32),
        _ => None,
    };
    Some(ReaderPlace { page: number(READER_PAGE_KEY)?, block: number(READER_BLOCK_KEY) })
}

/// One chapter as the reader draws it. `picture` fetches a picture out of
/// the file by its path inside it.
fn view_of(
    file: &FileEntry,
    text: &DocText,
    chapter: u32,
    mut picture: impl FnMut(&str) -> Option<parse::Resource>,
) -> CoreResult<DocChapterView> {
    if text.format == DocFormat::Pdf {
        return Err(Error::Invalid(format!(
            "{} is a PDF, which has no chapters to load: its pages are drawn as they are, \
             and read by page",
            file.name
        )));
    }
    let Some(found) = text.chapters.get(chapter as usize) else {
        return Err(Error::Invalid(format!(
            "{} has {} chapters, so there is no chapter {chapter} (they count from 0)",
            file.name,
            text.chapters.len()
        )));
    };
    let first = (found.first_block as usize).min(text.blocks.len());
    let end = first.saturating_add(found.blocks as usize).min(text.blocks.len());

    let mut spent = 0usize;
    let mut blocks = Vec::with_capacity(end - first);
    for (offset, block) in text.blocks[first..end].iter().enumerate() {
        let mut src = String::new();
        if block.kind == DocBlockKind::Image
            && !block.src.is_empty()
            && let Some(pic) = picture(&block.src)
            && PICTURE_TYPES.contains(&pic.mime.as_str())
            && pic.bytes.len() <= MAX_PICTURE_BYTES
            && spent + pic.bytes.len() <= MAX_CHAPTER_PICTURE_BYTES
        {
            spent += pic.bytes.len();
            let encoded = base64::engine::general_purpose::STANDARD.encode(&pic.bytes);
            src = format!("data:{};base64,{encoded}", pic.mime);
        }
        blocks.push(DocBlockView {
            index: u32::try_from(first + offset).unwrap_or(u32::MAX),
            kind: block.kind,
            level: block.level,
            page: block.page,
            html: if block.html.is_empty() {
                crate::mailview::escape_html(&block.text)
            } else {
                block.html.clone()
            },
            src,
        });
    }
    Ok(DocChapterView {
        file: file.id,
        chapter,
        first_block: u32::try_from(first).unwrap_or(u32::MAX),
        blocks,
    })
}

impl Service {
    /// The open vault's documents, opened on first use. Refuses when no
    /// vault is unlocked.
    pub fn documents(&self) -> CommandResult<Arc<DocumentsState>> {
        let vault = self.require()?;
        if !vault.is_unlocked() {
            return Err(CommandError::from(Error::Locked));
        }
        let mut slot = self.documents.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(state) = slot.as_ref()
            && state.vault_path == vault.path()
        {
            return Ok(state.clone());
        }
        // Held across the open, so two first callers make one state, not two
        // fighting over the index's writer lock.
        let state = Arc::new(DocumentsState::open(&vault)?);
        *slot = Some(state.clone());
        Ok(state)
    }
}

/// The core's [`Documents`], over one vault's state.
pub struct ToolDocuments {
    pub state: Arc<DocumentsState>,
    pub vault: Arc<Vault>,
}

impl Documents for ToolDocuments {
    fn text(&self, file: &FileEntry) -> CoreResult<Arc<DocText>> {
        self.state.text(&self.vault, file)
    }

    fn search(&self, query: &DocSearch) -> CoreResult<DocSearchPage> {
        self.state.search(&self.vault, query)
    }

    fn index_status(&self) -> DocIndexStatus {
        self.state.status(&self.vault)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use karmayoga_core::VaultConfig;
    use karmayoga_core::documents::{DocBlock, DocChapter};
    use karmayoga_core::files::{FileUpdate, ImportOptions};

    fn open(home: &std::path::Path) -> (Arc<Service>, Arc<Vault>) {
        let config = VaultConfig { password: None, ..Default::default() };
        let vault = karmayoga_vault::create(home, config).unwrap();
        let svc = Arc::new(Service::new());
        let vault = svc.set(vault);
        (svc, vault)
    }

    /// A vault with a files folder, and the state for it.
    fn with_folder() -> (tempfile::TempDir, tempfile::TempDir, Arc<Service>, Arc<Vault>) {
        let (home, folder) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (svc, vault) = open(home.path());
        vault.set_up_files_folder(Some(folder.path())).unwrap();
        (home, folder, svc, vault)
    }

    fn plain(vault: &Vault, name: &str, bytes: &[u8]) -> FileEntry {
        vault.import_bytes(bytes, name, &ImportOptions::new(FileOrigin::Files)).unwrap()
    }

    fn sealed(vault: &Vault, name: &str, bytes: &[u8], origin: FileOrigin) -> FileEntry {
        let options = ImportOptions { encrypted: Some(true), ..ImportOptions::new(origin) };
        vault.import_bytes(bytes, name, &options).unwrap()
    }

    fn block(kind: DocBlockKind, page: u32, text: &str) -> DocBlock {
        DocBlock { kind, page, text: text.into(), ..DocBlock::default() }
    }

    #[test]
    fn the_state_is_opened_once_for_the_open_vault_and_dropped_at_lock() {
        let (_home, _folder, svc, _vault) = with_folder();
        let first = svc.documents().unwrap();
        assert!(Arc::ptr_eq(&first, &svc.documents().unwrap()));
        assert!(first.index().is_some(), "the index opens beside the cache");

        svc.documents.close();
        let again = svc.documents().unwrap();
        assert!(!Arc::ptr_eq(&first, &again), "a closed state is opened afresh");
    }

    #[test]
    fn there_is_no_state_without_a_vault_and_none_for_a_locked_one() {
        let svc = Service::new();
        assert_eq!(svc.documents().err().map(|e| e.code), Some("no_vault".to_string()));

        let home = tempfile::tempdir().unwrap();
        let (svc, vault) = open(home.path());
        assert!(svc.documents().is_ok());
        vault.lock();
        assert_eq!(svc.documents().err().map(|e| e.code), Some("locked".to_string()));
    }

    #[test]
    fn what_is_not_a_document_or_not_there_or_not_for_this_is_refused_in_words() {
        let (_home, _folder, svc, vault) = with_folder();
        let state = svc.documents().unwrap();

        let picture = plain(&vault, "holiday.png", b"\x89PNG\r\n\x1a\n");
        let refused = state.text(&vault, &picture).unwrap_err().to_string();
        assert!(refused.contains("holiday.png") && refused.contains("not a document"), "{refused}");

        let health = sealed(&vault, "labs.txt", b"results", FileOrigin::Health);
        let refused = state.text(&vault, &health).unwrap_err().to_string();
        assert!(refused.contains("Health"), "{refused}");

        let note = plain(&vault, "notes.txt", b"words");
        vault.trash_files(&[note.id]).unwrap();
        let trashed = vault.file(note.id).unwrap();
        let refused = state.text(&vault, &trashed).unwrap_err().to_string();
        assert!(refused.contains("in the trash"), "{refused}");
    }

    #[test]
    fn a_document_is_parsed_once_and_then_kept_sealed_and_in_memory() {
        let (_home, _folder, svc, vault) = with_folder();
        let state = svc.documents().unwrap();
        let file = plain(&vault, "notes.txt", b"Mortimer came down the hill.\n\nThe inn was shut.");

        assert!(state.cache().is_empty());
        let first = state.text(&vault, &file).unwrap();
        assert!(first.blocks.iter().any(|b| b.text.contains("Mortimer")), "{first:?}");
        assert_eq!(state.cache().len(), 1);
        assert!(Arc::ptr_eq(&first, &state.text(&vault, &file).unwrap()), "kept in memory");

        // Gone from memory, it is read back from the cache and not parsed again.
        state.recent.lock().unwrap().clear();
        let from_cache = state.text(&vault, &file).unwrap();
        assert_eq!(*from_cache, *first);
        assert_eq!(state.cache().len(), 1);

        // A sealed document reads the same way.
        let secret = sealed(&vault, "secret.txt", b"Kept in the vault.", FileOrigin::Files);
        let text = state.text(&vault, &secret).unwrap();
        assert!(text.blocks.iter().any(|b| b.text.contains("vault")));
        assert_eq!(state.cache().len(), 2);
    }

    #[test]
    fn only_a_few_documents_are_kept_in_memory() {
        let (_home, _folder, svc, vault) = with_folder();
        let state = svc.documents().unwrap();
        for n in 0..RECENT_TEXTS + 2 {
            let file = plain(&vault, &format!("{n}.txt"), format!("document {n}").as_bytes());
            state.text(&vault, &file).unwrap();
        }
        assert_eq!(state.recent.lock().unwrap().len(), RECENT_TEXTS);
    }

    #[test]
    fn a_file_changed_on_disk_is_read_as_it_is_now_not_as_the_catalog_has_it() {
        let (_home, folder, svc, vault) = with_folder();
        let state = svc.documents().unwrap();
        let file = plain(&vault, "draft.txt", b"the first words");
        let before = state.text(&vault, &file).unwrap();
        assert!(before.blocks.iter().any(|b| b.text.contains("first")));

        // Edited behind the catalog's back, with a different size.
        std::fs::write(folder.path().join("draft.txt"), b"entirely different words").unwrap();
        let after = state.text(&vault, &file).unwrap();
        assert!(after.blocks.iter().any(|b| b.text.contains("different")), "{after:?}");
        assert!(!after.blocks.iter().any(|b| b.text.contains("first")));
    }

    #[test]
    fn a_gate_is_forgotten_when_nobody_waits_at_it() {
        let flights = Flights::default();
        let gate = flights.gate("a");
        assert!(Arc::ptr_eq(&gate, &flights.gate("a")), "one gate for one document");
        assert!(!Arc::ptr_eq(&gate, &flights.gate("b")));

        // Somebody else holds it: left in place.
        let waiting = flights.gate("a");
        flights.done("a", gate);
        assert_eq!(flights.0.lock().unwrap().len(), 2);
        flights.done("a", waiting);
        flights.done("b", flights.gate("b"));
        assert!(flights.0.lock().unwrap().is_empty());
    }

    #[test]
    fn the_outline_says_where_somebody_left_off_and_whether_it_is_searchable() {
        let (_home, _folder, svc, vault) = with_folder();
        let state = svc.documents().unwrap();
        let file = plain(&vault, "notes.txt", b"one two three");

        let outline = state.outline(&vault, &file).unwrap();
        assert_eq!((outline.file, outline.format), (file.id, DocFormat::Text));
        assert_eq!(outline.position, None);
        assert!(!outline.indexed);
        assert!(outline.words >= 3 && outline.blocks >= 1);

        let mut properties = BTreeMap::new();
        properties.insert(READER_PAGE_KEY.to_string(), Some(PropValue::Number(3.0)));
        properties.insert(READER_BLOCK_KEY.to_string(), Some(PropValue::Number(12.0)));
        let update = FileUpdate { properties, ..FileUpdate::default() };
        vault.update_files(&[file.id], &update).unwrap();
        let file = vault.file(file.id).unwrap();
        let outline = state.outline(&vault, &file).unwrap();
        assert_eq!(outline.position, Some(ReaderPlace { page: 3, block: Some(12) }));

        state.index().unwrap().put(file.id, &state.version_of(&file), "notes", &[]).unwrap();
        state.index().unwrap().commit().unwrap();
        assert!(state.outline(&vault, &file).unwrap().indexed);
        // Indexed at some other version of the file is not indexed as it is.
        state.index().unwrap().put(file.id, "an older version", "notes", &[]).unwrap();
        state.index().unwrap().commit().unwrap();
        assert!(!state.outline(&vault, &file).unwrap().indexed);
    }

    #[test]
    fn a_place_is_read_from_numbers_only() {
        let (_home, _folder, _svc, vault) = with_folder();
        let mut file = plain(&vault, "x.txt", b"x");
        assert_eq!(place_of(&file), None);
        file.properties.insert(READER_PAGE_KEY.into(), PropValue::Text("3".into()));
        assert_eq!(place_of(&file), None);
        file.properties.insert(READER_PAGE_KEY.into(), PropValue::Number(7.0));
        assert_eq!(place_of(&file), Some(ReaderPlace { page: 7, block: None }));
        file.properties.insert(READER_BLOCK_KEY.into(), PropValue::Number(-1.0));
        assert_eq!(place_of(&file), Some(ReaderPlace { page: 7, block: None }));
    }

    fn book() -> DocText {
        let mut text = DocText::new(DocFormat::Epub);
        text.pages = 2;
        text.blocks = vec![
            block(DocBlockKind::Heading, 1, "One"),
            block(DocBlockKind::Paragraph, 1, "a < b & c"),
            DocBlock { html: "<em>fine</em>".into(), ..block(DocBlockKind::Paragraph, 1, "fine") },
            DocBlock { src: "OEBPS/map.png".into(), ..block(DocBlockKind::Image, 1, "a map") },
            block(DocBlockKind::Heading, 2, "Two"),
        ];
        text.chapters = vec![
            DocChapter { first_block: 0, blocks: 4, href: "one.xhtml".into() },
            DocChapter { first_block: 4, blocks: 1, href: "two.xhtml".into() },
        ];
        text
    }

    #[test]
    fn a_chapter_is_its_blocks_drawn_with_text_escaped_and_pictures_inlined() {
        let (_home, _folder, _svc, vault) = with_folder();
        let file = plain(&vault, "book.epub", b"not read here");
        let text = book();
        let view = view_of(&file, &text, 0, |path| {
            assert_eq!(path, "OEBPS/map.png");
            Some(parse::Resource { mime: "image/png".into(), bytes: vec![1, 2, 3] })
        })
        .unwrap();
        assert_eq!((view.file, view.chapter, view.first_block), (file.id, 0, 0));
        assert_eq!(view.blocks.len(), 4);
        assert_eq!(view.blocks[1].html, "a &lt; b &amp; c", "no html: the text, escaped");
        assert_eq!(view.blocks[2].html, "<em>fine</em>", "the parser's html as it is");
        assert_eq!(view.blocks[3].src, "data:image/png;base64,AQID");
        assert_eq!(view.blocks[3].index, 3);

        let second = view_of(&file, &text, 1, |_| None).unwrap();
        assert_eq!((second.first_block, second.blocks[0].index, second.blocks[0].level), (4, 4, 0));
    }

    #[test]
    fn a_picture_that_is_too_large_or_not_a_picture_type_is_not_inlined() {
        let (_home, _folder, _svc, vault) = with_folder();
        let file = plain(&vault, "book.epub", b"x");
        let text = book();
        let picture = |mime: &str, len: usize| {
            let (mime, len) = (mime.to_string(), len);
            move |_: &str| Some(parse::Resource { mime: mime.clone(), bytes: vec![0; len] })
        };
        let src = |view: DocChapterView| view.blocks[3].src.clone();

        assert!(!src(view_of(&file, &text, 0, picture("image/png", 100)).unwrap()).is_empty());
        for kept in PICTURE_TYPES {
            let view = view_of(&file, &text, 0, picture(kept, 10)).unwrap();
            assert!(!src(view).is_empty(), "{kept}");
        }
        let too_big = view_of(&file, &text, 0, picture("image/png", MAX_PICTURE_BYTES + 1));
        assert!(src(too_big.unwrap()).is_empty());
        assert!(src(view_of(&file, &text, 0, picture("text/html", 10)).unwrap()).is_empty());
        assert!(src(view_of(&file, &text, 0, picture("application/pdf", 10)).unwrap()).is_empty());
        assert!(src(view_of(&file, &text, 0, |_| None).unwrap()).is_empty());

        // A chapter's pictures together are held to a total.
        let mut many = DocText::new(DocFormat::Epub);
        for _ in 0..8 {
            many.blocks.push(DocBlock { src: "a.png".into(), ..block(DocBlockKind::Image, 1, "") });
        }
        many.chapters = vec![DocChapter { first_block: 0, blocks: 8, href: String::new() }];
        let view = view_of(&file, &many, 0, picture("image/png", MAX_PICTURE_BYTES)).unwrap();
        let drawn = view.blocks.iter().filter(|b| !b.src.is_empty()).count();
        assert_eq!(drawn, MAX_CHAPTER_PICTURE_BYTES / MAX_PICTURE_BYTES);
    }

    #[test]
    fn a_pdf_has_no_chapters_and_a_chapter_that_is_not_there_says_so() {
        let (_home, _folder, _svc, vault) = with_folder();
        let file = plain(&vault, "paper.pdf", b"x");
        let mut text = book();
        text.format = DocFormat::Pdf;
        let refused = view_of(&file, &text, 0, |_| None).unwrap_err().to_string();
        assert!(refused.contains("no chapters") && refused.contains("paper.pdf"), "{refused}");

        let epub = book();
        let refused = view_of(&file, &epub, 2, |_| None).unwrap_err().to_string();
        assert!(refused.contains("2 chapters") && refused.contains("chapter 2"), "{refused}");
    }

    #[test]
    fn a_search_leaves_out_what_the_catalog_no_longer_wants_indexed() {
        let (_home, _folder, svc, vault) = with_folder();
        let state = svc.documents().unwrap();
        let (kept, trashed, health) = (
            plain(&vault, "kept.txt", b"kept"),
            plain(&vault, "trashed.txt", b"trashed"),
            sealed(&vault, "labs.txt", b"labs", FileOrigin::Health),
        );
        let index = state.index().unwrap();
        for (file, word) in [(&kept, "alpha"), (&trashed, "alpha"), (&health, "alpha")] {
            let passage = karmayoga_docindex::Passage {
                block: 0,
                page: 1,
                section: String::new(),
                text: format!("{word} in {}", file.name),
            };
            index.put(file.id, "v", &file.name, &[passage]).unwrap();
        }
        index.commit().unwrap();
        vault.trash_files(&[trashed.id]).unwrap();
        let ghost = FileId::new();
        index
            .put(
                ghost,
                "v",
                "ghost",
                &[karmayoga_docindex::Passage {
                    block: 0,
                    page: 1,
                    section: String::new(),
                    text: "alpha in a file the catalog does not have".into(),
                }],
            )
            .unwrap();
        index.commit().unwrap();

        let query = DocSearch { text: "alpha".into(), limit: 10, ..DocSearch::default() };
        let page = state.search(&vault, &query).unwrap();
        // Only the kept file: the sealed one is not indexed by the settings,
        // the trashed and the vanished are not there to show.
        assert_eq!(page.hits.iter().map(|h| h.file).collect::<Vec<_>>(), vec![kept.id]);
        assert_eq!(page.total, 1);

        // Switched off, nothing is shown, whatever the index still holds.
        let mut settings = vault.files_settings().unwrap();
        settings.index_documents = false;
        vault.save_files_settings(&settings).unwrap();
        assert_eq!(state.search(&vault, &query).unwrap().hits.len(), 0);
        assert!(!state.status(&vault).enabled);
    }

    #[test]
    fn the_status_counts_what_is_indexed_and_a_reindex_empties_it() {
        let (_home, _folder, svc, vault) = with_folder();
        let state = svc.documents().unwrap();
        let file = plain(&vault, "notes.txt", b"words");
        state.index().unwrap().put(file.id, "v", "notes", &[]).unwrap();
        state.index().unwrap().commit().unwrap();

        let status = state.status(&vault);
        assert_eq!((status.enabled, status.indexed, status.pending), (true, 1, 0));
        assert!(!status.running && status.failed.is_empty());

        state.progress().failed.insert(file.id, ("v".into(), "not a document".into()));
        let before = state.epoch();
        state.reindex();
        assert!(state.epoch() > before);
        let status = state.status(&vault);
        assert_eq!(status.indexed, 0);
        assert!(status.failed.is_empty(), "a reindex forgets what failed");
        assert!(state.index().unwrap().indexed().unwrap().is_empty());
    }
}
