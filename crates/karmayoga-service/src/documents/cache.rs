//! Every parsed document, sealed, in the cache directory. See the module
//! docs of [`super`].
//!
//! # What is kept, and where
//!
//! A [`DocText`] costs seconds to make for a long PDF and is asked for again
//! every time the reader opens the book, the assistant reads a page of it, or
//! the indexer comes round, so it is parsed once and kept. Never inside the
//! files folder, which a sync tool carries to every device and which may be
//! on a volume anyone can read, and never in the clear: each entry is sealed
//! under a subkey of the vault's own key, so a copy of the cache directory
//! says nothing without the vault.
//!
//! The directory is `karmayoga-docs-<hash of the vault's path>` in the user's
//! cache directory -- [`scratch_base`](karmayoga_core::files::disk::scratch_base),
//! which is the vault's own directory while a test runs -- with `text/` for
//! these entries and `index/` for the search index beside them. It is named
//! by the vault's *path*, as the open copies of sealed files are, so it can be
//! found again without a key.
//!
//! # Names that say nothing
//!
//! An entry is named by a keyed BLAKE3 of what identifies the file's contents
//! and the version of the parsers: the file's content hash when the catalog
//! has one, else its id, size and modified time. The key is another subkey, so
//! a list of the directory's names says nothing about which documents the
//! vault holds, nor can it be matched against a list of known files' hashes.
//! Because it follows the contents, not the name, a file that is moved or
//! renamed is not parsed again, and one whose contents change is.
//!
//! # A cache can always be made again
//!
//! A sealed entry that will not open -- the key changed, the file was cut off
//! by a full disk -- or will not read as a [`DocText`] is a miss, and is
//! removed, so the document is parsed again rather than failing for good.
//! Entries are written to a temporary file and renamed into place, readable
//! by their owner alone from the moment they exist.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use karmayoga_core::Vault;
use karmayoga_core::crypto::{AeadCipher, Cipher, SecretKey};
use karmayoga_core::documents::{DOC_TEXT_VERSION, DocText};
use karmayoga_core::error::Result;
use karmayoga_core::files::FileEntry;
use karmayoga_core::fsutil;

/// The subkey everything here is sealed under.
const KEY_LABEL: &str = "karmayoga.documents.cache.v1";
/// Sub-labels, so the names and the index are not sealed or named under the
/// key a text entry is.
const NAMES_LABEL: &str = "karmayoga.documents.cache.names.v1";
const INDEX_LABEL: &str = "karmayoga.documents.cache.index.v1";

/// What an entry's AAD starts with, so one sealed as a document's text can
/// be opened as nothing else, and not under another name.
const AAD_PREFIX: &str = "karmayoga.documents.text.v1:";

/// How long an entry or a leftover temporary file is spared by a sweep: a
/// document read a moment ago, under a hash the catalog has not caught up
/// with, is not one that no document maps to.
const SPARED: Duration = Duration::from_secs(60 * 60);

/// The cache for one vault.
pub struct DocCache {
    root: PathBuf,
    text: PathBuf,
    cipher: AeadCipher,
    names: SecretKey,
    index_key: SecretKey,
}

impl DocCache {
    /// Where the vault at `vault_path` keeps its documents' cache and index.
    pub fn root_for(vault_path: &Path) -> PathBuf {
        karmayoga_core::files::disk::scratch_base(vault_path)
            .join(format!("karmayoga-docs-{}", fsutil::stable_path_name(vault_path)))
    }

    /// Open this vault's cache, making its directories (owner-only) if they
    /// are not there. Fails when the vault is locked.
    pub fn open(vault: &Vault) -> Result<Self> {
        let key = vault.derive_subkey(KEY_LABEL)?;
        Self::at(Self::root_for(vault.path()), &key)
    }

    /// The cache under `root`, sealed under `key`.
    pub fn at(root: PathBuf, key: &SecretKey) -> Result<Self> {
        let cipher = AeadCipher::new(key);
        let names = cipher.derive_subkey(NAMES_LABEL);
        let index_key = cipher.derive_subkey(INDEX_LABEL);
        let text = root.join("text");
        fsutil::create_private_dir(&root)?;
        fsutil::create_private_dir(&text)?;
        Ok(Self { root, text, cipher, names, index_key })
    }

    /// Where the search index lives.
    pub fn index_dir(&self) -> PathBuf {
        self.root.join("index")
    }

    /// The cipher the search index is sealed with.
    pub fn index_cipher(&self) -> Arc<dyn Cipher> {
        Arc::new(AeadCipher::new(&self.index_key))
    }

    /// What identifies `file`'s contents at this version of the parsers: its
    /// content hash when the catalog has one, else its id, size and modified
    /// time (which a file that has just arrived has, and which changes when
    /// its contents do).
    pub fn identity(file: &FileEntry) -> String {
        if file.hash.is_empty() {
            format!("{}:{}:{}:{DOC_TEXT_VERSION}", file.id, file.size, file.modified_at)
        } else {
            Self::identity_of_hash(&file.hash)
        }
    }

    /// [`DocCache::identity`] of a file whose contents hash to `hash`
    /// (BLAKE3, lowercase hex): what a file the catalog has not caught up
    /// with is named by once its bytes have been read.
    pub fn identity_of_hash(hash: &str) -> String {
        format!("{hash}:{DOC_TEXT_VERSION}")
    }

    /// The entry's name for `identity`: a keyed hash, so the directory's
    /// listing says nothing. This is also the version the search index
    /// records a file at.
    pub fn name(&self, identity: &str) -> String {
        blake3::keyed_hash(self.names.expose(), identity.as_bytes()).to_hex().to_string()
    }

    fn path_of(&self, name: &str) -> PathBuf {
        self.text.join(name)
    }

    fn aad(name: &str) -> Vec<u8> {
        format!("{AAD_PREFIX}{name}").into_bytes()
    }

    /// The document kept under `name`: `None` when there is none, or when
    /// there was one that could not be used, which is removed.
    pub fn get(&self, name: &str) -> Option<DocText> {
        let path = self.path_of(name);
        let sealed = match std::fs::read(&path) {
            Ok(sealed) => sealed,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                tracing::debug!(error = %e, "documents: could not read a cached text");
                return None;
            }
        };
        let opened = self
            .cipher
            .open(&Self::aad(name), &sealed)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<DocText>(&bytes).ok())
            .filter(|text| text.version == DOC_TEXT_VERSION);
        if opened.is_none() {
            let _ = std::fs::remove_file(&path);
        }
        opened
    }

    /// Keep `text` under `name`, replacing what was there.
    pub fn put(&self, name: &str, text: &DocText) -> Result<()> {
        let bytes = serde_json::to_vec(text)?;
        let sealed = self.cipher.seal(&Self::aad(name), &bytes)?;
        // Made again if somebody cleared the cache directory while this ran.
        fsutil::create_private_dir(&self.text)?;
        fsutil::write_atomic_private(&self.path_of(name), &sealed, &fsutil::unique_tag())
    }

    /// How many documents are kept.
    pub fn len(&self) -> usize {
        self.entries().count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The files of `text/` that are entries, with their names.
    fn entries(&self) -> impl Iterator<Item = (String, PathBuf)> {
        std::fs::read_dir(&self.text).into_iter().flatten().flatten().filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            (!name.starts_with('.') && e.file_type().ok()?.is_file()).then(|| (name, e.path()))
        })
    }

    /// Remove every entry no name in `keep` stands for, and any temporary
    /// file a write left behind, sparing what is under an hour old. Answers
    /// how many entries went.
    pub fn sweep(&self, keep: &HashSet<String>) -> usize {
        let old = |path: &Path| {
            std::fs::metadata(path)
                .and_then(|m| m.modified())
                .map(|at| SystemTime::now().duration_since(at).is_ok_and(|age| age >= SPARED))
                .unwrap_or(false)
        };
        let mut removed = 0;
        for (name, path) in self.entries() {
            if !keep.contains(&name) && old(&path) && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        // `write_atomic`'s scratch files are dot-files in the same folder.
        for entry in std::fs::read_dir(&self.text).into_iter().flatten().flatten() {
            let path = entry.path();
            let hidden = entry.file_name().to_string_lossy().starts_with('.');
            if hidden && old(&path) {
                let _ = std::fs::remove_file(&path);
            }
        }
        removed
    }
}

impl std::fmt::Debug for DocCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocCache").field("root", &self.root).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The labels the cache's keys are derived under. Each is an input to the
    /// cipher, not a name: changing one re-keys the cache, so every parsed
    /// document would read as unreadable and be parsed again, and the index
    /// rebuilt from nothing. A change here has to be on purpose.
    #[test]
    fn the_key_labels_are_pinned() {
        assert_eq!(KEY_LABEL, "karmayoga.documents.cache.v1");
        assert_eq!(NAMES_LABEL, "karmayoga.documents.cache.names.v1");
        assert_eq!(INDEX_LABEL, "karmayoga.documents.cache.index.v1");
    }
    use karmayoga_core::documents::{DocBlock, DocFormat};
    use karmayoga_core::files::{FileKind, FileOrigin, FileStatus, FileStorage, MediaInfo};
    use karmayoga_core::id::FileId;
    use std::collections::BTreeMap;

    fn key(seed: u8) -> SecretKey {
        SecretKey::from_bytes([seed; 32])
    }

    fn entry(hash: &str) -> FileEntry {
        let now = jiff::Timestamp::from_second(1_700_000_000).unwrap();
        FileEntry {
            id: FileId::new(),
            storage: FileStorage::Plain { path: "Books/dune.epub".into() },
            name: "dune.epub".into(),
            kind: FileKind::Document,
            mime: "application/epub+zip".into(),
            size: 1234,
            hash: hash.into(),
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

    fn text(words: &str) -> DocText {
        let mut text = DocText::new(DocFormat::Text);
        text.pages = 1;
        text.blocks.push(DocBlock { page: 1, text: words.into(), ..DocBlock::default() });
        text
    }

    fn cache(dir: &Path) -> DocCache {
        DocCache::at(dir.join("docs"), &key(7)).unwrap()
    }

    #[test]
    fn a_document_survives_a_round_trip_and_is_a_miss_before_it_is_put() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());
        let name = cache.name(&DocCache::identity(&entry("abc")));
        assert!(cache.get(&name).is_none());
        assert!(cache.is_empty());

        let saved = text("Mortimer came down the hill.");
        cache.put(&name, &saved).unwrap();
        assert_eq!(cache.get(&name), Some(saved));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn what_is_on_disk_is_sealed_and_named_by_a_keyed_hash() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());
        let identity = DocCache::identity(&entry("0123abcd"));
        let name = cache.name(&identity);
        cache.put(&name, &text("Mortimer found the xylophone")).unwrap();

        let on_disk = std::fs::read(cache.path_of(&name)).unwrap();
        let shown = String::from_utf8_lossy(&on_disk);
        assert!(!shown.contains("Mortimer") && !shown.contains("xylophone"));
        // The name is a 256-bit hex hash that follows from the key, not
        // from the hash the file has.
        assert_eq!(name.len(), 64);
        assert!(name.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!name.contains("0123abcd"));
        let other = DocCache::at(dir.path().join("elsewhere"), &key(8)).unwrap();
        assert_ne!(other.name(&identity), name);
        // And the other key cannot read it, even given the file.
        std::fs::copy(cache.path_of(&name), other.path_of(&name)).unwrap();
        assert!(other.get(&name).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn an_entry_is_readable_by_its_owner_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());
        cache.put("entry", &text("words")).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&cache.path_of("entry")), 0o600);
        assert_eq!(mode(&cache.text), 0o700);
        assert_eq!(mode(&cache.root), 0o700);
    }

    #[test]
    fn the_name_follows_the_contents_not_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());
        let mut moved = entry("samehash");
        let before = cache.name(&DocCache::identity(&moved));
        moved.storage = FileStorage::Plain { path: "Elsewhere/renamed.epub".into() };
        moved.name = "renamed.epub".into();
        assert_eq!(cache.name(&DocCache::identity(&moved)), before, "a rename is not a new file");
        assert_ne!(cache.name(&DocCache::identity(&entry("anotherhash"))), before);
        // A hash the bytes were read to, as the catalog will have it.
        assert_eq!(DocCache::identity_of_hash("samehash"), DocCache::identity(&moved));

        // Without a hash: the id, the size and the time.
        let mut bare = entry("");
        let first = DocCache::identity(&bare);
        bare.size += 1;
        assert_ne!(DocCache::identity(&bare), first);
        assert!(first.ends_with(&format!(":{DOC_TEXT_VERSION}")));
    }

    #[test]
    fn an_entry_that_will_not_open_or_read_is_a_miss_and_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());

        cache.put("cut", &text("words")).unwrap();
        let sealed = std::fs::read(cache.path_of("cut")).unwrap();
        std::fs::write(cache.path_of("cut"), &sealed[..sealed.len() / 2]).unwrap();
        assert!(cache.get("cut").is_none());
        assert!(!cache.path_of("cut").exists(), "a broken entry is cleared away");

        // Sealed properly, but not a document.
        let junk = cache.cipher.seal(&DocCache::aad("junk"), b"{ not a document").unwrap();
        std::fs::write(cache.path_of("junk"), junk).unwrap();
        assert!(cache.get("junk").is_none());
        assert!(!cache.path_of("junk").exists());

        // Sealed under another name: not this entry's.
        cache.put("a", &text("a")).unwrap();
        std::fs::copy(cache.path_of("a"), cache.path_of("b")).unwrap();
        assert!(cache.get("b").is_none());
        assert!(cache.get("a").is_some());

        // A document of another version of the parsers.
        let mut stale = text("old");
        stale.version = DOC_TEXT_VERSION + 1;
        cache.put("stale", &stale).unwrap();
        assert!(cache.get("stale").is_none());
    }

    #[test]
    fn a_sweep_removes_what_no_document_maps_to_and_spares_the_young() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());
        for name in ["kept", "gone", "young"] {
            cache.put(name, &text(name)).unwrap();
        }
        // Everything is brand new, so nothing is swept yet.
        assert_eq!(cache.sweep(&HashSet::new()), 0);
        assert_eq!(cache.len(), 3);

        // Age two of them past the spared time.
        let long_ago = SystemTime::now() - SPARED - Duration::from_secs(60);
        for name in ["kept", "gone"] {
            std::fs::File::options()
                .write(true)
                .open(cache.path_of(name))
                .unwrap()
                .set_modified(long_ago)
                .unwrap();
        }
        let keep = HashSet::from(["kept".to_string()]);
        assert_eq!(cache.sweep(&keep), 1);
        assert!(cache.get("kept").is_some());
        assert!(cache.get("gone").is_none());
        assert!(cache.get("young").is_some());
    }

    #[test]
    fn the_root_is_named_for_the_vault_and_holds_the_index_beside_the_text() {
        let dir = tempfile::tempdir().unwrap();
        let root = DocCache::root_for(dir.path());
        assert!(root.starts_with(dir.path()), "under a test the vault's own directory");
        let name = root.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("karmayoga-docs-"));
        assert_eq!(name.len(), "karmayoga-docs-".len() + 16);

        let cache = cache(dir.path());
        assert_eq!(cache.index_dir(), cache.root.join("index"));
        assert!(cache.text.is_dir());
        // The index is sealed under a key of its own.
        let sealed = cache.index_cipher().seal(b"aad", b"words").unwrap();
        assert!(cache.cipher.open(b"aad", &sealed).is_err());
    }
}
