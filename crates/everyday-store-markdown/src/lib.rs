//! Plain-file Markdown storage backend.
//!
//! Where the SQLite backend optimises for speed, this one optimises for the
//! property that your journal outlives this application. Entries are files
//! you can read, grep, diff, sync with git and edit in any editor.
//!
//! ```text
//!   store/
//!     journals/<journal-id>.json
//!     entries/<journal-id>/2024-07-14-a-long-walk-1f3a9c22.md
//!     entries/<journal-id>/2024-07-14-a-long-walk-1f3a9c22.json
//!     media/
//! ```
//!
//! Each entry is two files: the `.md` is the human-readable projection, and
//! the `.json` alongside it is the canonical record (rich text tree,
//! attachment metadata, location, timestamps).
//!
//! # Hand edits are respected
//!
//! The frontmatter records a hash of the Markdown body as the app wrote it.
//! When the entry is read back, a mismatch means a human edited the file, so
//! the Markdown is reparsed (see [`md`]) and wins over the sidecar JSON.
//! Edit an entry in vim and the app picks the change up; leave it alone and
//! the exact rich-text tree round-trips untouched.
//!
//! # Encryption
//!
//! If the vault has a password, both files are sealed and this backend
//! reports `human_readable: false` — encryption and grep-ability are
//! mutually exclusive, and the UI says so rather than implying otherwise.

pub mod md;

use everyday_core::blobstore::FileBlobStore;
use everyday_core::crypto::Cipher;
use everyday_core::error::{Error, Result};
use everyday_core::id::{BlobId, EntryId, JournalId};
use everyday_core::model::{Attachment, Entry, EntrySummary, Journal, Location, Weather};
use everyday_core::richtext::RichDoc;
use everyday_core::store::{
    Capabilities, EntryQuery, JournalStore, StoreContext, StoreFactory, StoreStats, entry_aad,
    journal_aad,
};
use jiff::{Timestamp, civil::Date};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

pub const BACKEND_ID: &str = "markdown";
const JOURNALS_DIR: &str = "journals";
const ENTRIES_DIR: &str = "entries";
const MEDIA_DIR: &str = "media";

pub struct MarkdownFactory;

impl StoreFactory for MarkdownFactory {
    fn id(&self) -> &'static str {
        BACKEND_ID
    }

    fn describe(&self) -> &'static str {
        "Markdown files \u{2014} readable and syncable with any tool"
    }

    fn open(&self, ctx: StoreContext) -> Result<Box<dyn JournalStore>> {
        Ok(Box::new(MarkdownStore::open(ctx)?))
    }
}

/// Cached location and list-view projection of one entry, so listing does
/// not re-read every file on disk.
#[derive(Debug, Clone)]
struct Cached {
    path: PathBuf,
    summary: EntrySummary,
}

pub struct MarkdownStore {
    root: PathBuf,
    cipher: Arc<dyn Cipher>,
    blobs: FileBlobStore,
    index: RwLock<BTreeMap<EntryId, Cached>>,
}

impl MarkdownStore {
    pub fn open(ctx: StoreContext) -> Result<Self> {
        for dir in [JOURNALS_DIR, ENTRIES_DIR] {
            let p = ctx.root.join(dir);
            std::fs::create_dir_all(&p).map_err(|e| Error::io(&p, e))?;
        }
        let store = Self {
            blobs: FileBlobStore::open(ctx.root.join(MEDIA_DIR), ctx.cipher.clone())?,
            root: ctx.root,
            cipher: ctx.cipher,
            index: RwLock::new(BTreeMap::new()),
        };
        store.rebuild_index()?;
        Ok(store)
    }

    fn journals_dir(&self) -> PathBuf {
        self.root.join(JOURNALS_DIR)
    }

    fn entries_dir(&self, journal: JournalId) -> PathBuf {
        self.root.join(ENTRIES_DIR).join(journal.to_string())
    }

    fn journal_path(&self, id: JournalId) -> PathBuf {
        self.journals_dir().join(format!("{id}.json"))
    }

    /// Walk the entry tree and cache each entry's path and summary.
    fn rebuild_index(&self) -> Result<()> {
        let mut index = BTreeMap::new();
        let root = self.root.join(ENTRIES_DIR);
        let dirs = match std::fs::read_dir(&root) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                *self.index.write().unwrap() = index;
                return Ok(());
            }
            Err(e) => return Err(Error::io(&root, e)),
        };
        for journal_dir in dirs.flatten() {
            if !journal_dir.path().is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(journal_dir.path()) else { continue };
            for f in files.flatten() {
                let path = f.path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                match self.read_entry_file(&path) {
                    Ok(entry) => {
                        index.insert(entry.id, Cached { path, summary: entry.summarize() });
                    }
                    // One damaged file must not make the whole vault
                    // unopenable; skip it and keep going.
                    Err(e) => tracing_warn(&path, &e),
                }
            }
        }
        *self.index.write().unwrap() = index;
        Ok(())
    }

    /// Deterministic, human-legible file stem: date, title slug, short id.
    fn file_stem(entry: &Entry) -> String {
        let slug = slugify(&entry.display_title());
        let short = entry.id.short();
        if slug.is_empty() {
            format!("{}-{}", entry.local_date, short)
        } else {
            format!("{}-{}-{}", entry.local_date, slug, short)
        }
    }

    fn seal_text(&self, aad: &[u8], text: &str) -> Result<Vec<u8>> {
        self.cipher.seal(aad, text.as_bytes())
    }

    fn open_text(&self, aad: &[u8], bytes: &[u8]) -> Result<String> {
        let plain = self.cipher.open(aad, bytes)?;
        String::from_utf8(plain).map_err(|_| Error::Invalid("file is not valid UTF-8".into()))
    }

    fn write_entry_files(&self, entry: &Entry, path: &Path) -> Result<()> {
        let dir = path.parent().expect("entry path has a parent");
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;

        let markdown = entry.body.to_markdown();
        let front = Frontmatter::from_entry(entry, &markdown);
        let doc = toml::to_string_pretty(&front)
            .map_err(|e| Error::Invalid(format!("could not serialise frontmatter: {e}")))?;
        let text = format!("+++\n{doc}+++\n\n{markdown}\n");

        let aad = entry_aad(entry.id);
        write_atomic(path, &self.seal_text(&aad, &text)?)?;
        write_atomic(&path.with_extension("json"), &self.cipher.seal(&aad, &serde_json::to_vec_pretty(entry)?)?)
    }

    /// Read an entry back, preferring the sidecar JSON but deferring to the
    /// Markdown when a human has edited it.
    fn read_entry_file(&self, path: &Path) -> Result<Entry> {
        let raw = std::fs::read(path).map_err(|e| Error::io(path, e))?;

        // The id is in the frontmatter, but the AAD needs the id to decrypt.
        // Resolve it from the filename, which always ends in the first eight
        // hex digits of the id, by consulting the sidecar-free path: the
        // frontmatter is the authority, so we try each candidate id from the
        // index first and fall back to a scan-free parse for plaintext vaults.
        let id = self.entry_id_for(path, &raw)?;
        let aad = entry_aad(id);

        let text = self.open_text(&aad, &raw)?;
        let (front, markdown) = parse_frontmatter(&text)?;

        let sidecar = path.with_extension("json");
        let from_json = std::fs::read(&sidecar)
            .ok()
            .and_then(|b| self.cipher.open(&aad, &b).ok())
            .and_then(|plain| serde_json::from_slice::<Entry>(&plain).ok());

        let hand_edited = front.body_hash != body_hash(markdown);

        match from_json {
            Some(mut entry) if !hand_edited => {
                // Frontmatter is still authoritative for the fields a person
                // is most likely to tweak by hand.
                front.apply_to(&mut entry);
                Ok(entry)
            }
            other => {
                let mut entry = other.unwrap_or_else(|| Entry {
                    id,
                    journal_id: front.journal,
                    title: String::new(),
                    body: RichDoc::empty(),
                    local_date: front.date,
                    tz: front.tz.clone(),
                    created_at: front.created,
                    updated_at: front.updated,
                    tags: Vec::new(),
                    starred: false,
                    pinned: false,
                    location: None,
                    weather: None,
                    attachments: Vec::new(),
                });
                entry.body = md::parse(markdown);
                front.apply_to(&mut entry);
                Ok(entry)
            }
        }
    }

    /// Recover the entry id for a file.
    ///
    /// For an unencrypted vault the frontmatter can simply be read. For an
    /// encrypted one that is circular — the id is the associated data needed
    /// to decrypt — so the id is taken from the sidecar file name recorded in
    /// the index, or from the eight-hex-digit suffix in the file stem matched
    /// against the ids already known.
    fn entry_id_for(&self, path: &Path, raw: &[u8]) -> Result<EntryId> {
        if let Some(cached) = self
            .index
            .read()
            .unwrap()
            .iter()
            .find(|(_, c)| c.path == path)
            .map(|(id, _)| *id)
        {
            return Ok(cached);
        }
        // Unencrypted: the id is right there in the frontmatter.
        if let Ok(text) = std::str::from_utf8(raw)
            && let Ok((front, _)) = parse_frontmatter(text)
        {
            return Ok(front.id);
        }
        // Encrypted: the id lives in the `.id` companion written next to the
        // entry precisely so that this is not circular.
        let marker = path.with_extension("id");
        let text = std::fs::read_to_string(&marker).map_err(|e| Error::io(&marker, e))?;
        EntryId::parse(text.trim()).map_err(|e| Error::Invalid(e.to_string()))
    }
}

fn tracing_warn(path: &Path, err: &Error) {
    tracing::warn!(path = %path.display(), error = %err, "skipping unreadable entry file");
}

/// Frontmatter is a projection of the entry, not a second source of truth —
/// except for the fields a person is likely to edit by hand, which win.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Frontmatter {
    id: EntryId,
    journal: JournalId,
    date: Date,
    tz: String,
    created: Timestamp,
    updated: Timestamp,
    #[serde(default)]
    title: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    starred: bool,
    #[serde(default)]
    pinned: bool,
    /// BLAKE3 of the Markdown body as this app wrote it. A mismatch means a
    /// human has been editing, and their text wins over the sidecar JSON.
    body_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    location: Option<Location>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    weather: Option<Weather>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<Attachment>,
}

impl Frontmatter {
    fn from_entry(e: &Entry, markdown: &str) -> Self {
        Self {
            id: e.id,
            journal: e.journal_id,
            date: e.local_date,
            tz: e.tz.clone(),
            created: e.created_at,
            updated: e.updated_at,
            title: e.title.clone(),
            tags: e.tags.clone(),
            starred: e.starred,
            pinned: e.pinned,
            body_hash: body_hash(markdown),
            location: e.location.clone(),
            weather: e.weather.clone(),
            attachments: e.attachments.clone(),
        }
    }

    fn apply_to(&self, e: &mut Entry) {
        e.id = self.id;
        e.journal_id = self.journal;
        e.local_date = self.date;
        e.tz = self.tz.clone();
        e.created_at = self.created;
        e.updated_at = self.updated;
        e.title = self.title.clone();
        e.tags = self.tags.clone();
        e.starred = self.starred;
        e.pinned = self.pinned;
        e.location = self.location.clone();
        e.weather = self.weather.clone();
        e.attachments = self.attachments.clone();
    }
}

fn body_hash(markdown: &str) -> String {
    blake3_hex(markdown.trim().as_bytes())
}

fn blake3_hex(bytes: &[u8]) -> String {
    BlobId::of(bytes).to_hex()[..16].to_string()
}

fn parse_frontmatter(text: &str) -> Result<(Frontmatter, &str)> {
    let rest = text
        .strip_prefix("+++\n")
        .ok_or_else(|| Error::Invalid("entry file has no frontmatter".into()))?;
    let end = rest
        .find("\n+++")
        .ok_or_else(|| Error::Invalid("entry frontmatter is not terminated".into()))?;
    let front: Frontmatter = toml::from_str(&rest[..end + 1])
        .map_err(|e| Error::Invalid(format!("malformed entry frontmatter: {e}")))?;
    let body = rest[end + 4..].trim_start_matches('\n');
    Ok((front, body))
}

/// Lowercase ASCII slug, at most 40 characters, for file names.
fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && out.len() < 40 {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= 40 {
            break;
        }
    }
    out.trim_matches('-').to_string()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("writing");
    std::fs::write(&tmp, bytes).map_err(|e| Error::io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))?;
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(path, e)),
    }
}

impl JournalStore for MarkdownStore {
    fn backend(&self) -> &'static str {
        BACKEND_ID
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            blobs: true,
            // Files are written atomically, but a multi-file update (entry +
            // sidecar) is not one transaction.
            transactional: false,
            human_readable: !self.cipher.is_encrypting(),
            max_blob_bytes: None,
            // Neither of the other two domains. A kanban board is not a
            // thing anyone wants as a tree of files, and a cache of somebody
            // else's meetings is even less so -- it would be a directory
            // this backend rewrote wholesale every hour, in a format nobody
            // would ever want to grep.
            tasks: false,
            calendars: false,
        }
    }

    // ---- journals -------------------------------------------------------

    fn list_journals(&self) -> Result<Vec<Journal>> {
        let dir = self.journals_dir();
        let mut out = Vec::new();
        let files = match std::fs::read_dir(&dir) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(Error::io(&dir, e)),
        };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            let Ok(id) = JournalId::parse(stem) else { continue };
            match self.get_journal(id) {
                Ok(j) => out.push(j),
                Err(e) => tracing_warn(&path, &e),
            }
        }
        out.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then_with(|| a.name.cmp(&b.name)));
        Ok(out)
    }

    fn get_journal(&self, id: JournalId) -> Result<Journal> {
        let path = self.journal_path(id);
        let raw = match std::fs::read(&path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::not_found("journal", id));
            }
            Err(e) => return Err(Error::io(&path, e)),
        };
        let plain = self.cipher.open(&journal_aad(id), &raw)?;
        Ok(serde_json::from_slice(&plain)?)
    }

    fn put_journal(&self, j: &Journal) -> Result<()> {
        let sealed = self.cipher.seal(&journal_aad(j.id), &serde_json::to_vec_pretty(j)?)?;
        std::fs::create_dir_all(self.journals_dir())
            .map_err(|e| Error::io(self.journals_dir(), e))?;
        write_atomic(&self.journal_path(j.id), &sealed)?;
        std::fs::create_dir_all(self.entries_dir(j.id))
            .map_err(|e| Error::io(self.entries_dir(j.id), e))
    }

    fn delete_journal(&self, id: JournalId) -> Result<()> {
        remove_if_present(&self.journal_path(id))?;
        let dir = self.entries_dir(id);
        if dir.is_dir() {
            std::fs::remove_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        }
        self.index.write().unwrap().retain(|_, c| !c.path.starts_with(&dir));
        Ok(())
    }

    // ---- entries --------------------------------------------------------

    fn list_entries(&self, query: &EntryQuery) -> Result<Vec<EntrySummary>> {
        let index = self.index.read().unwrap();
        Ok(query.apply(index.values().map(|c| c.summary.clone()).collect()))
    }

    fn get_entry(&self, id: EntryId) -> Result<Entry> {
        let path = self
            .index
            .read()
            .unwrap()
            .get(&id)
            .map(|c| c.path.clone())
            .ok_or_else(|| Error::not_found("entry", id))?;
        self.read_entry_file(&path)
    }

    fn put_entry(&self, entry: &Entry) -> Result<()> {
        let dir = self.entries_dir(entry.journal_id);
        let path = dir.join(format!("{}.md", Self::file_stem(entry)));

        // Register the id before writing, so that a subsequent read can find
        // the entry's associated data even in an encrypted vault.
        let previous = self
            .index
            .write()
            .unwrap()
            .insert(entry.id, Cached { path: path.clone(), summary: entry.summarize() })
            .map(|c| c.path);

        std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        // The `.id` marker only earns its keep on an encrypted vault, where
        // the id is needed as associated data *before* the file can be
        // decrypted. On a plaintext vault the frontmatter already says so,
        // and an extra file per entry is just litter in a directory whose
        // whole point is being pleasant to browse.
        if self.cipher.is_encrypting() {
            write_atomic(&path.with_extension("id"), entry.id.to_string().as_bytes())?;
        }
        self.write_entry_files(entry, &path)?;

        // The file name encodes the date and title, so an edit that changes
        // either moves the file; clean up where it used to live.
        if let Some(old) = previous
            && old != path
        {
            for ext in ["md", "json", "id"] {
                remove_if_present(&old.with_extension(ext))?;
            }
        }
        Ok(())
    }

    fn delete_entry(&self, id: EntryId) -> Result<()> {
        let Some(cached) = self.index.write().unwrap().remove(&id) else { return Ok(()) };
        for ext in ["md", "json", "id"] {
            remove_if_present(&cached.path.with_extension(ext))?;
        }
        Ok(())
    }

    fn all_entries(&self) -> Result<Vec<Entry>> {
        let paths: Vec<PathBuf> =
            self.index.read().unwrap().values().map(|c| c.path.clone()).collect();
        let mut out = Vec::with_capacity(paths.len());
        for p in paths {
            match self.read_entry_file(&p) {
                Ok(e) => out.push(e),
                Err(e) => tracing_warn(&p, &e),
            }
        }
        Ok(out)
    }

    // ---- blobs ----------------------------------------------------------

    fn put_blob(&self, bytes: &[u8]) -> Result<BlobId> {
        self.blobs.put(bytes)
    }

    fn get_blob(&self, id: BlobId) -> Result<Vec<u8>> {
        self.blobs.get(id)
    }

    fn blob_len(&self, id: BlobId) -> Result<u64> {
        self.blobs.len_of(id)
    }

    fn get_blob_range(&self, id: BlobId, offset: u64, len: u64) -> Result<Vec<u8>> {
        self.blobs.get_range(id, offset, len)
    }

    fn has_blob(&self, id: BlobId) -> Result<bool> {
        Ok(self.blobs.has(id))
    }

    fn delete_blob(&self, id: BlobId) -> Result<()> {
        self.blobs.delete(id)
    }

    fn list_blobs(&self) -> Result<Vec<BlobId>> {
        self.blobs.list()
    }

    // ---- housekeeping ---------------------------------------------------

    fn stats(&self) -> Result<StoreStats> {
        let (blobs, blob_bytes) = self.blobs.stats()?;
        Ok(StoreStats {
            journals: self.list_journals()?.len() as u64,
            entries: self.index.read().unwrap().len() as u64,
            blobs,
            blob_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::crypto::{AeadCipher, NullCipher, SecretKey};
    use everyday_core::store::conformance;

    fn ctx(root: &Path, encrypted: bool) -> StoreContext {
        let cipher: Arc<dyn Cipher> = if encrypted {
            Arc::new(AeadCipher::new(&SecretKey::from_bytes([4u8; 32])))
        } else {
            Arc::new(NullCipher)
        };
        StoreContext { root: root.to_path_buf(), cipher }
    }

    fn entry_in(j: &Journal, title: &str, body: &str) -> Entry {
        let mut e = Entry::new(j.id, "UTC");
        e.title = title.into();
        e.body = RichDoc::from_plain_text(body);
        e
    }

    #[test]
    fn passes_the_shared_conformance_suite_unencrypted() {
        let dir = tempfile::tempdir().unwrap();
        let store = MarkdownStore::open(ctx(dir.path(), false)).unwrap();
        conformance::run_all(&store);
    }

    #[test]
    fn passes_the_shared_conformance_suite_when_encrypted() {
        let dir = tempfile::tempdir().unwrap();
        let store = MarkdownStore::open(ctx(dir.path(), true)).unwrap();
        conformance::run_all(&store);
    }

    #[test]
    fn an_unencrypted_vault_is_readable_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let store = MarkdownStore::open(ctx(dir.path(), false)).unwrap();
        assert!(store.capabilities().human_readable);

        let j = Journal::new("Daily");
        store.put_journal(&j).unwrap();
        let e = entry_in(&j, "A long walk", "We went far.");
        store.put_entry(&e).unwrap();

        let path = store.index.read().unwrap()[&e.id].path.clone();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("+++\n"), "frontmatter should lead the file");
        assert!(text.contains("A long walk"));
        assert!(text.contains("We went far."));
        assert!(
            path.file_name().unwrap().to_str().unwrap().contains("a-long-walk"),
            "file name should be legible: {path:?}"
        );
    }

    #[test]
    fn an_encrypted_vault_is_not_readable_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let store = MarkdownStore::open(ctx(dir.path(), true)).unwrap();
        assert!(!store.capabilities().human_readable, "encryption precludes readability");

        let j = Journal::new("Daily");
        store.put_journal(&j).unwrap();
        let e = entry_in(&j, "A long walk", "We went somewhere private.");
        store.put_entry(&e).unwrap();

        let path = store.index.read().unwrap()[&e.id].path.clone();
        let raw = std::fs::read(&path).unwrap();
        assert!(!raw.windows(26).any(|w| w == b"We went somewhere private."));
    }

    #[test]
    fn entries_survive_reopening_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::new("Daily");
        let e = entry_in(&j, "Persisted", "still here after a restart");
        {
            let store = MarkdownStore::open(ctx(dir.path(), false)).unwrap();
            store.put_journal(&j).unwrap();
            store.put_entry(&e).unwrap();
        }
        let store = MarkdownStore::open(ctx(dir.path(), false)).unwrap();
        assert_eq!(store.list_entries(&EntryQuery::default()).unwrap().len(), 1);
        assert_eq!(store.get_entry(e.id).unwrap().body.plain_text(), "still here after a restart");
    }

    #[test]
    fn encrypted_entries_survive_reopening_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::new("Daily");
        let e = entry_in(&j, "Persisted", "sealed but still here");
        {
            let store = MarkdownStore::open(ctx(dir.path(), true)).unwrap();
            store.put_journal(&j).unwrap();
            store.put_entry(&e).unwrap();
        }
        let store = MarkdownStore::open(ctx(dir.path(), true)).unwrap();
        assert_eq!(store.get_entry(e.id).unwrap().body.plain_text(), "sealed but still here");
    }

    #[test]
    fn a_hand_edited_markdown_body_wins_over_the_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let store = MarkdownStore::open(ctx(dir.path(), false)).unwrap();
        let j = Journal::new("Daily");
        store.put_journal(&j).unwrap();
        let e = entry_in(&j, "Edited", "the original text");
        store.put_entry(&e).unwrap();

        // Simulate someone opening the file in an editor.
        let path = store.index.read().unwrap()[&e.id].path.clone();
        let text = std::fs::read_to_string(&path).unwrap();
        let edited = text.replace("the original text", "text I typed in vim, with **bold**");
        std::fs::write(&path, edited).unwrap();

        let got = store.get_entry(e.id).unwrap();
        assert!(got.body.plain_text().contains("text I typed in vim"));
        // And it came back as structure, not literal asterisks.
        assert_eq!(got.body.to_markdown(), "text I typed in vim, with **bold**");
    }

    #[test]
    fn an_untouched_file_round_trips_the_exact_rich_text_tree() {
        let dir = tempfile::tempdir().unwrap();
        let store = MarkdownStore::open(ctx(dir.path(), false)).unwrap();
        let j = Journal::new("Daily");
        store.put_journal(&j).unwrap();

        // A document whose Markdown projection is lossy: nested marks and a
        // media node with a caption. The sidecar must preserve it exactly.
        let mut e = Entry::new(j.id, "UTC");
        let blob = store.put_blob(b"pixels").unwrap();
        e.body = RichDoc(serde_json::json!({
            "type": "doc",
            "content": [
                {"type": "paragraph", "content": [
                    {"type": "text", "text": "mixed", "marks": [
                        {"type": "bold"}, {"type": "italic"}]}]},
                {"type": "media", "attrs": {
                    "blob": blob.to_hex(), "kind": "image",
                    "mime": "image/png", "filename": "p.png", "caption": "a caption"}},
            ]
        }));
        store.put_entry(&e).unwrap();

        assert_eq!(store.get_entry(e.id).unwrap().body, e.body);
    }

    #[test]
    fn renaming_an_entry_moves_its_files_and_leaves_no_orphan() {
        let dir = tempfile::tempdir().unwrap();
        let store = MarkdownStore::open(ctx(dir.path(), false)).unwrap();
        let j = Journal::new("Daily");
        store.put_journal(&j).unwrap();

        let mut e = entry_in(&j, "First title", "body");
        store.put_entry(&e).unwrap();
        let first = store.index.read().unwrap()[&e.id].path.clone();

        e.title = "Second title".into();
        e.local_date = jiff::civil::date(2030, 1, 1);
        store.put_entry(&e).unwrap();
        let second = store.index.read().unwrap()[&e.id].path.clone();

        assert_ne!(first, second);
        assert!(!first.exists(), "the old .md must be removed");
        assert!(!first.with_extension("json").exists(), "the old sidecar must be removed");
        assert!(second.exists());
        assert_eq!(store.list_entries(&EntryQuery::default()).unwrap().len(), 1);

        // And the move survives a reopen — no duplicate entry appears.
        let store = MarkdownStore::open(ctx(dir.path(), false)).unwrap();
        assert_eq!(store.list_entries(&EntryQuery::default()).unwrap().len(), 1);
    }

    #[test]
    fn a_corrupt_file_is_skipped_rather_than_failing_the_whole_vault() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::new("Daily");
        let good = entry_in(&j, "Good", "readable");
        {
            let store = MarkdownStore::open(ctx(dir.path(), false)).unwrap();
            store.put_journal(&j).unwrap();
            store.put_entry(&good).unwrap();
            store.put_entry(&entry_in(&j, "Bad", "about to be mangled")).unwrap();
        }
        // Mangle one file beyond recognition.
        let dir_path = dir.path().join(ENTRIES_DIR).join(j.id.to_string());
        let victim = std::fs::read_dir(&dir_path)
            .unwrap()
            .flatten()
            .map(|f| f.path())
            .find(|p| {
                p.extension().and_then(|e| e.to_str()) == Some("md")
                    && p.to_string_lossy().contains("bad")
            })
            .expect("the 'Bad' entry file");
        std::fs::write(&victim, b"this is not an Every Day entry").unwrap();

        let store = MarkdownStore::open(ctx(dir.path(), false)).unwrap();
        let entries = store.list_entries(&EntryQuery::default()).unwrap();
        assert_eq!(entries.len(), 1, "the readable entry must still load");
        assert_eq!(entries[0].title, "Good");
    }

    #[test]
    fn entries_sharing_a_date_and_title_get_distinct_files() {
        // Regression: file stems used the *leading* digits of the id, which
        // are a shared timestamp in UUIDv7, so same-day same-title entries
        // overwrote each other.
        let dir = tempfile::tempdir().unwrap();
        let store = MarkdownStore::open(ctx(dir.path(), false)).unwrap();
        let j = Journal::new("Daily");
        store.put_journal(&j).unwrap();

        for _ in 0..8 {
            let mut e = entry_in(&j, "Same title", "different body");
            e.local_date = jiff::civil::date(2025, 5, 5);
            store.put_entry(&e).unwrap();
        }
        assert_eq!(store.list_entries(&EntryQuery::default()).unwrap().len(), 8);

        let store = MarkdownStore::open(ctx(dir.path(), false)).unwrap();
        assert_eq!(
            store.list_entries(&EntryQuery::default()).unwrap().len(),
            8,
            "entries were lost to colliding file names"
        );
    }

    #[test]
    fn a_plaintext_vault_holds_only_the_two_readable_files_per_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = MarkdownStore::open(ctx(dir.path(), false)).unwrap();
        let j = Journal::new("Daily");
        store.put_journal(&j).unwrap();
        store.put_entry(&entry_in(&j, "Tidy", "no litter please")).unwrap();

        let exts: Vec<String> = std::fs::read_dir(dir.path().join(ENTRIES_DIR).join(j.id.to_string()))
            .unwrap()
            .flatten()
            .filter_map(|f| f.path().extension().map(|e| e.to_string_lossy().into_owned()))
            .collect();
        assert_eq!(exts.len(), 2, "expected just the .md and .json, got {exts:?}");
        assert!(exts.contains(&"md".to_string()));
        assert!(exts.contains(&"json".to_string()));
    }

    #[test]
    fn an_encrypted_vault_keeps_the_id_marker_it_needs_to_decrypt() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::new("Daily");
        let e = entry_in(&j, "Sealed", "needs its id to open");
        {
            let store = MarkdownStore::open(ctx(dir.path(), true)).unwrap();
            store.put_journal(&j).unwrap();
            store.put_entry(&e).unwrap();
        }
        // Reopening from cold has no in-memory index, so the marker is the
        // only way back to the associated data.
        let store = MarkdownStore::open(ctx(dir.path(), true)).unwrap();
        assert_eq!(store.get_entry(e.id).unwrap().title, "Sealed");
    }

    #[test]
    fn file_names_are_slugified_safely() {
        assert_eq!(slugify("A Long Walk"), "a-long-walk");
        assert_eq!(slugify("../../etc/passwd"), "etc-passwd");
        assert_eq!(slugify("  ???  "), "");
        assert_eq!(slugify("\u{65e5}\u{8a18}"), "", "non-ascii is dropped from file names");
        assert!(slugify(&"x".repeat(200)).len() <= 40);
    }

    #[test]
    fn a_title_of_only_punctuation_still_produces_a_valid_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let store = MarkdownStore::open(ctx(dir.path(), false)).unwrap();
        let j = Journal::new("Daily");
        store.put_journal(&j).unwrap();

        let mut e = Entry::new(j.id, "UTC");
        e.title = "\u{2026}???".into();
        store.put_entry(&e).unwrap();
        assert_eq!(store.get_entry(e.id).unwrap().id, e.id);
    }

    #[test]
    fn frontmatter_parsing_rejects_malformed_input() {
        assert!(parse_frontmatter("no frontmatter here").is_err());
        assert!(parse_frontmatter("+++\nid = 3\n").is_err(), "unterminated");
        assert!(parse_frontmatter("+++\nnot toml at all !!\n+++\n").is_err());
    }

    #[test]
    fn media_lives_in_a_shared_directory() {
        let dir = tempfile::tempdir().unwrap();
        let store = MarkdownStore::open(ctx(dir.path(), false)).unwrap();
        store.put_blob(b"pixels").unwrap();
        assert!(dir.path().join(MEDIA_DIR).is_dir());
    }
}
