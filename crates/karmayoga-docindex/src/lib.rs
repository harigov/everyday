//! Document search: [tantivy] over the passages of every indexed document in
//! the files folder, written through `karmayoga-mailindex`'s
//! [`SealedDirectory`] so that nothing of it is readable on disk without the
//! vault's key.
//!
//! A document is indexed as passages -- a few blocks of
//! [`karmayoga_core::documents::DocText`] at a time, never across a section's
//! start -- each carrying the file's id, the first block's index and page,
//! and the section it is in, so a hit is a place the reader can open at and
//! the assistant can read from. The shape of what is stored, and why a title
//! is on one document per file and not on every passage, is in [`schema`].
//!
//! # One writer, one reader that reloads itself
//!
//! One [`IndexWriter`] is held for the life of the [`DocIndex`], behind a
//! lock, and [`DocIndex::commit`] reloads the reader itself right after the
//! commit, as mail's index does: a search made after a commit returns sees
//! it, with no race against a background reload. The writer is built for one
//! indexing thread; the documents arrive one at a time from the indexing task
//! and a passage is a few hundred words, so there is nothing for more threads
//! to do.
//!
//! A second process that has the same vault open (a read-only window) cannot
//! take the writer lock. It still opens the index and searches it -- without a
//! writer, reloading its reader before every search to see what the first
//! process has committed -- and every write answers that the index is in use
//! elsewhere.
//!
//! # An index that cannot be read is made again
//!
//! The index is derived from the files and can always be built afresh, so an
//! index that will not open -- a different schema from this build's, a key that
//! does not decrypt it, a truncated file -- is wiped and started empty, the
//! way mail's is, and the next pass over the files fills it. It is wiped only
//! under the writer lock: an index another process is writing to is not
//! unreadable, and is left alone.

pub mod schema;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use karmayoga_core::crypto::Cipher;
use karmayoga_core::error::{Error, Result};
use karmayoga_core::id::FileId;
use karmayoga_mailindex::SealedDirectory;
use tantivy::collector::{Count, DocSetCollector, TopDocs};
use tantivy::query::{BooleanQuery, ConstScoreQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{IndexRecordOption, TantivyDocument, Term, Value};
use tantivy::snippet::SnippetGenerator;
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy};

use crate::schema::{DOC_TOKENIZER, Fields, KIND_MARKER, KIND_PASSAGE};

/// The heap tantivy's one indexing thread is given. A document goes in at a
/// time -- a long book is a megabyte or two of passages -- so this is
/// generous, and well above the 15 MB tantivy needs to start at all.
const WRITER_HEAP_BYTES: usize = 32 * 1024 * 1024;

/// How many characters a hit's snippet is cut to, about.
const SNIPPET_CHARS: usize = 240;

/// How much of a passage stands in for its snippet when none of the words
/// of the query are in it (the match was in the title).
const FALLBACK_SNIPPET_CHARS: usize = 200;

/// How much a word in the title counts against one in the text.
const TITLE_BOOST: f32 = 2.0;

/// One passage to index.
#[derive(Debug, Clone, PartialEq)]
pub struct Passage {
    /// The first block it covers.
    pub block: u32,
    pub page: u32,
    /// The innermost section it is in, as the document names it.
    pub section: String,
    pub text: String,
}

/// One passage found.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexHit {
    pub file: FileId,
    pub block: u32,
    pub page: u32,
    pub section: String,
    /// The words around the match, on one line.
    pub snippet: String,
    pub score: f32,
}

/// The index, open.
pub struct DocIndex {
    fields: Fields,
    index: Index,
    /// `None` when another process holds the writer lock: see the module
    /// docs.
    writer: Option<Mutex<IndexWriter>>,
    reader: IndexReader,
}

fn wrap_tantivy(e: tantivy::TantivyError) -> Error {
    Error::Backend(Box::new(e))
}

/// What a write answers when this process has no writer.
fn in_use_elsewhere() -> Error {
    Error::Invalid(
        "the document index is open for writing in another Karma Yoga process; \
         close that one to change it"
            .into(),
    )
}

/// What opening one attempt at `dir` came to.
struct Parts {
    index: Index,
    writer: Option<Mutex<IndexWriter>>,
    reader: IndexReader,
}

/// One attempt to open `dir` as it stands. `Ok(None)` when tantivy refuses
/// to hand back an index: no lock is taken that early, so that can only mean
/// the directory's contents disagree with this build (see the module docs).
/// A writer that cannot be had is not that; it leaves a read-only index.
fn open_once(
    dir: &Path,
    cipher: Arc<dyn Cipher>,
    cache_bytes: usize,
    layout: &tantivy::schema::Schema,
) -> Result<Option<Parts>> {
    let directory = SealedDirectory::open(dir, cipher, cache_bytes)?;
    let Ok(index) = Index::open_or_create(directory, layout.clone()) else { return Ok(None) };
    index.tokenizers().register(DOC_TOKENIZER, schema::analyzer());
    let writer = match index.writer_with_num_threads(1, WRITER_HEAP_BYTES) {
        Ok(writer) => Some(Mutex::new(writer)),
        Err(e) => {
            tracing::debug!(error = %e, "documents: the index is open elsewhere; reading only");
            None
        }
    };
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .map_err(wrap_tantivy)?;
    Ok(Some(Parts { index, writer, reader }))
}

/// Remove everything [`SealedDirectory::open`] put at `dir` and leave it
/// empty. `false`, having touched nothing, when another handle holds the
/// writer lock: the lock is taken through the sealed directory exactly as a
/// writer takes it, and the lock file is the one entry left in place, since
/// deleting a file somebody is locking is how a second writer comes to lock a
/// different file of the same name and believe it is alone.
fn clear_dir(dir: &Path, cipher: Arc<dyn Cipher>, cache_bytes: usize) -> Result<bool> {
    use tantivy::Directory;
    use tantivy::directory::INDEX_WRITER_LOCK;
    use tantivy::directory::error::LockError;

    let directory = SealedDirectory::open(dir, cipher, cache_bytes)?;
    let _writer = match directory.acquire_lock(&INDEX_WRITER_LOCK) {
        Ok(lock) => lock,
        Err(LockError::LockBusy) => return Ok(false),
        Err(LockError::IoError(e)) => {
            return Err(Error::io(dir, std::io::Error::new(e.kind(), e.to_string())));
        }
    };
    let entries = std::fs::read_dir(dir).map_err(|e| Error::io(dir, e))?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::io(dir, e))?;
        if entry.file_name() == INDEX_WRITER_LOCK.filepath.as_os_str() {
            continue;
        }
        let path = entry.path();
        let removed = if entry.file_type().map_err(|e| Error::io(&path, e))?.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match removed {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::io(&path, e)),
        }
    }
    Ok(true)
}

impl DocIndex {
    /// Open the index at `dir`, sealed under `cipher`, keeping up to
    /// `cache_bytes` of decrypted segments in memory; a fresh one when there
    /// is none, or when the one there cannot be read (it is rebuilt by the
    /// next pass).
    pub fn open(
        dir: impl Into<PathBuf>,
        cipher: Arc<dyn Cipher>,
        cache_bytes: usize,
    ) -> Result<Self> {
        let dir = dir.into();
        let (layout, fields) = schema::build();
        let parts = match open_once(&dir, cipher.clone(), cache_bytes, &layout)? {
            Some(parts) => parts,
            None => {
                // The one case worth wiping for; see the module docs.
                if !clear_dir(&dir, cipher.clone(), cache_bytes)? {
                    return Err(in_use_elsewhere());
                }
                open_once(&dir, cipher, cache_bytes, &layout)?.ok_or_else(|| {
                    Error::Invalid("a fresh document index could not be opened".into())
                })?
            }
        };
        Ok(Self { fields, index: parts.index, writer: parts.writer, reader: parts.reader })
    }

    /// Whether this handle can write: `false` when another process has the
    /// index open for writing, and this one only reads.
    pub fn can_write(&self) -> bool {
        self.writer.is_some()
    }

    fn writer(&self) -> Result<MutexGuard<'_, IndexWriter>> {
        let writer = self.writer.as_ref().ok_or_else(in_use_elsewhere)?;
        Ok(writer.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Replace whatever is indexed for `file` with `passages`, recorded as
    /// version `version` (the cache's key for the file's contents) under the
    /// title `title`. Seen by searches after the next [`DocIndex::commit`].
    pub fn put(
        &self,
        file: FileId,
        version: &str,
        title: &str,
        passages: &[Passage],
    ) -> Result<()> {
        let writer = self.writer()?;
        let f = &self.fields;
        let id = file.to_string();
        // Deletes what was added before this call and nothing after it, so
        // the documents added below are the file's whole entry.
        writer.delete_term(Term::from_field_text(f.file, &id));

        // The marker: where the first passage is, so that a hit on the title
        // opens the document at its start.
        let (block, page) = passages.first().map_or((0, 1), |p| (p.block, p.page));
        let mut marker = TantivyDocument::default();
        marker.add_text(f.file, &id);
        marker.add_text(f.kind, KIND_MARKER);
        marker.add_text(f.version, version);
        marker.add_text(f.title, title);
        marker.add_u64(f.block, u64::from(block));
        marker.add_u64(f.page, u64::from(page));
        writer.add_document(marker).map_err(wrap_tantivy)?;

        for passage in passages.iter().filter(|p| !p.text.trim().is_empty()) {
            let mut doc = TantivyDocument::default();
            doc.add_text(f.file, &id);
            doc.add_text(f.kind, KIND_PASSAGE);
            doc.add_u64(f.block, u64::from(passage.block));
            doc.add_u64(f.page, u64::from(passage.page));
            doc.add_text(f.section, &passage.section);
            doc.add_text(f.text, &passage.text);
            writer.add_document(doc).map_err(wrap_tantivy)?;
        }
        Ok(())
    }

    /// Take `file` out of the index.
    pub fn remove(&self, file: FileId) -> Result<()> {
        let writer = self.writer()?;
        writer.delete_term(Term::from_field_text(self.fields.file, &file.to_string()));
        Ok(())
    }

    /// Make every `put` and `remove` since the last one visible, and durable.
    pub fn commit(&self) -> Result<()> {
        let mut writer = self.writer()?;
        writer.commit().map_err(wrap_tantivy)?;
        // Reloaded here, not left to tantivy's own timer: see the module docs.
        self.reader.reload().map_err(wrap_tantivy)?;
        Ok(())
    }

    /// Every indexed file, with the version it was indexed at.
    pub fn indexed(&self) -> Result<HashMap<FileId, String>> {
        self.refresh()?;
        let searcher = self.reader.searcher();
        let markers = TermQuery::new(
            Term::from_field_text(self.fields.kind, KIND_MARKER),
            IndexRecordOption::Basic,
        );
        let found = searcher.search(&markers, &DocSetCollector).map_err(wrap_tantivy)?;
        let mut out = HashMap::with_capacity(found.len());
        for address in found {
            let doc: TantivyDocument = searcher.doc(address).map_err(wrap_tantivy)?;
            let (Some(file), Some(version)) =
                (text_of(&doc, self.fields.file), text_of(&doc, self.fields.version))
            else {
                continue;
            };
            if let Ok(file) = FileId::parse(&file) {
                out.insert(file, version);
            }
        }
        Ok(out)
    }

    /// The version `file` was indexed at, if it is indexed: [`DocIndex::indexed`]
    /// for one file, without reading the rest.
    pub fn version(&self, file: FileId) -> Result<Option<String>> {
        self.refresh()?;
        let searcher = self.reader.searcher();
        let f = &self.fields;
        let marker = BooleanQuery::new(vec![
            (
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(f.file, &file.to_string()),
                    IndexRecordOption::Basic,
                )) as Box<dyn Query>,
            ),
            (
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(f.kind, KIND_MARKER),
                    IndexRecordOption::Basic,
                )) as Box<dyn Query>,
            ),
        ]);
        let top = searcher
            .search(&marker, &TopDocs::with_limit(1).order_by_score())
            .map_err(wrap_tantivy)?;
        let Some((_, address)) = top.first() else { return Ok(None) };
        let doc: TantivyDocument = searcher.doc(*address).map_err(wrap_tantivy)?;
        Ok(text_of(&doc, f.version))
    }

    /// Search: every word of `query` counting, phrases in quotes kept
    /// together, best first. Only `files` when that is not empty. Answers a
    /// page of hits and how many passages match in all.
    pub fn search(
        &self,
        query: &str,
        files: &[FileId],
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<IndexHit>, usize)> {
        let query = query.trim();
        if query.is_empty() || limit == 0 {
            return Ok((Vec::new(), 0));
        }
        self.refresh()?;
        let Some(parsed) = self.parse(query) else { return Ok((Vec::new(), 0)) };
        let f = &self.fields;
        let full: Box<dyn Query> = if files.is_empty() {
            parsed
        } else {
            // The files are a filter, not part of what is being asked, so
            // they are scored at nothing and do not move a passage up or down.
            let wanted = BooleanQuery::new(
                files
                    .iter()
                    .map(|file| {
                        (
                            Occur::Should,
                            Box::new(TermQuery::new(
                                Term::from_field_text(f.file, &file.to_string()),
                                IndexRecordOption::Basic,
                            )) as Box<dyn Query>,
                        )
                    })
                    .collect(),
            );
            let wanted: Box<dyn Query> = Box::new(ConstScoreQuery::new(Box::new(wanted), 0.0));
            Box::new(BooleanQuery::new(vec![(Occur::Must, parsed), (Occur::Must, wanted)]))
        };

        let searcher = self.reader.searcher();
        let collector = (TopDocs::with_limit(limit).and_offset(offset).order_by_score(), Count);
        let (top, total) = searcher.search(full.as_ref(), &collector).map_err(wrap_tantivy)?;

        let mut snippets = SnippetGenerator::create(&searcher, full.as_ref(), f.text).ok();
        if let Some(generator) = snippets.as_mut() {
            generator.set_max_num_chars(SNIPPET_CHARS);
        }
        let mut hits = Vec::with_capacity(top.len());
        for (score, address) in top {
            let doc: TantivyDocument = searcher.doc(address).map_err(wrap_tantivy)?;
            let Some(file) = text_of(&doc, f.file).and_then(|id| FileId::parse(&id).ok()) else {
                continue;
            };
            let snippet = if text_of(&doc, f.kind).as_deref() == Some(KIND_MARKER) {
                text_of(&doc, f.title).unwrap_or_default()
            } else {
                let text = text_of(&doc, f.text).unwrap_or_default();
                match snippets.as_ref().map(|generator| generator.snippet(&text)) {
                    Some(found) if !found.is_empty() => found.fragment().to_string(),
                    _ => leading(&text, FALLBACK_SNIPPET_CHARS),
                }
            };
            hits.push(IndexHit {
                file,
                block: number_of(&doc, f.block),
                page: number_of(&doc, f.page),
                section: text_of(&doc, f.section).unwrap_or_default(),
                snippet: one_line(&snippet),
                score,
            });
        }
        Ok((hits, total))
    }

    /// Empty the index.
    pub fn clear(&self) -> Result<()> {
        {
            let mut writer = self.writer()?;
            writer.delete_all_documents().map_err(wrap_tantivy)?;
            writer.commit().map_err(wrap_tantivy)?;
        }
        self.reader.reload().map_err(wrap_tantivy)
    }

    /// Pick up what another process has committed, when this one has no
    /// writer of its own to say what has changed.
    fn refresh(&self) -> Result<()> {
        if self.writer.is_none() {
            self.reader.reload().map_err(wrap_tantivy)?;
        }
        Ok(())
    }

    /// The query as asked, or -- when it does not parse (an unbalanced
    /// bracket, a field that is not there) -- its words alone. `None` when
    /// it has none.
    fn parse(&self, query: &str) -> Option<Box<dyn Query>> {
        let f = &self.fields;
        let mut parser = QueryParser::for_index(&self.index, vec![f.text, f.title]);
        parser.set_field_boost(f.title, TITLE_BOOST);
        match parser.parse_query(query) {
            Ok(parsed) => Some(parsed),
            Err(_) => {
                let words = plain_words(query);
                if words.is_empty() { None } else { parser.parse_query(&words).ok() }
            }
        }
    }
}

/// `query` with everything but its words taken out: what a query that would
/// not parse is searched as. The words the query language reads as operators
/// (`AND`, `OR`, `NOT`, `TO`, `IN`, in capitals) are dropped with the
/// brackets and quotes, or "this AND" would fail to parse the second time too.
fn plain_words(query: &str) -> String {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty() && !matches!(*word, "AND" | "OR" | "NOT" | "TO" | "IN"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whitespace of every kind -- the line breaks between blocks included --
/// as single spaces.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first `max` characters of `text`, cut on a character.
fn leading(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn text_of(doc: &TantivyDocument, field: tantivy::schema::Field) -> Option<String> {
    doc.get_first(field).and_then(|v| Value::as_str(&v).map(str::to_string))
}

fn number_of(doc: &TantivyDocument, field: tantivy::schema::Field) -> u32 {
    let n = doc.get_first(field).and_then(|v| Value::as_u64(&v)).unwrap_or(0);
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use karmayoga_core::crypto::{AeadCipher, SecretKey};

    fn cipher(seed: u8) -> Arc<dyn Cipher> {
        Arc::new(AeadCipher::new(&SecretKey::from_bytes([seed; 32])))
    }

    fn open(dir: &Path) -> DocIndex {
        DocIndex::open(dir, cipher(3), 1024 * 1024).unwrap()
    }

    fn passage(block: u32, page: u32, section: &str, text: &str) -> Passage {
        Passage { block, page, section: section.into(), text: text.into() }
    }

    fn book() -> Vec<Passage> {
        vec![
            passage(0, 1, "Arrival", "Mortimer came down the hill at dusk, carrying a lantern."),
            passage(4, 2, "Arrival", "The inn was shut, and nobody answered the bell."),
            passage(9, 3, "The Orchard", "In the orchard Mortimer found the xylophone again."),
        ]
    }

    #[test]
    fn a_put_is_found_after_a_commit_and_not_before() {
        let dir = tempfile::tempdir().unwrap();
        let index = open(dir.path());
        let file = FileId::new();
        index.put(file, "v1", "The Hill", &book()).unwrap();
        assert_eq!(index.search("mortimer", &[], 10, 0).unwrap().1, 0, "not committed yet");

        index.commit().unwrap();
        let (hits, total) = index.search("mortimer", &[], 10, 0).unwrap();
        assert_eq!(total, 2);
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.file == file));
        let orchard =
            hits.iter().find(|h| h.section == "The Orchard").expect("the orchard passage");
        assert_eq!((orchard.block, orchard.page), (9, 3));
        assert!(orchard.snippet.contains("Mortimer"), "{}", orchard.snippet);
        assert!(!orchard.snippet.contains('\n'));
    }

    #[test]
    fn a_hit_on_the_title_is_the_document_at_its_start() {
        let dir = tempfile::tempdir().unwrap();
        let index = open(dir.path());
        let file = FileId::new();
        index.put(file, "v1", "The Hill \u{2014} Edith Wharton", &book()).unwrap();
        index.commit().unwrap();

        let (hits, total) = index.search("wharton", &[], 10, 0).unwrap();
        assert_eq!(total, 1, "one hit for the book, not one per passage");
        assert_eq!((hits[0].block, hits[0].page), (0, 1));
        assert!(hits[0].snippet.contains("Wharton"));
    }

    #[test]
    fn putting_again_replaces_and_removing_takes_it_out() {
        let dir = tempfile::tempdir().unwrap();
        let index = open(dir.path());
        let (a, b) = (FileId::new(), FileId::new());
        index.put(a, "a1", "A", &book()).unwrap();
        index.put(b, "b1", "B", &[passage(0, 1, "", "A second book about a lantern.")]).unwrap();
        index.commit().unwrap();
        let both = HashMap::from([(a, "a1".to_string()), (b, "b1".to_string())]);
        assert_eq!(index.indexed().unwrap(), both);
        assert_eq!(index.version(a).unwrap().as_deref(), Some("a1"));
        assert_eq!(index.search("lantern", &[], 10, 0).unwrap().1, 2);

        // A new version of `a` leaves nothing of the old one.
        index.put(a, "a2", "A", &[passage(0, 1, "", "Only a bell now.")]).unwrap();
        index.commit().unwrap();
        assert_eq!(index.version(a).unwrap().as_deref(), Some("a2"));
        let (hits, _) = index.search("lantern", &[], 10, 0).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file, b);
        assert_eq!(index.search("mortimer", &[], 10, 0).unwrap().1, 0);

        index.remove(b).unwrap();
        index.commit().unwrap();
        assert_eq!(index.indexed().unwrap().keys().copied().collect::<Vec<_>>(), vec![a]);
        assert_eq!(index.version(b).unwrap(), None);
        assert_eq!(index.search("lantern", &[], 10, 0).unwrap().1, 0);
    }

    #[test]
    fn a_file_with_no_words_is_still_indexed() {
        let dir = tempfile::tempdir().unwrap();
        let index = open(dir.path());
        let scan = FileId::new();
        index.put(scan, "v1", "A scan", &[]).unwrap();
        index.commit().unwrap();
        assert_eq!(index.indexed().unwrap().get(&scan).map(String::as_str), Some("v1"));
    }

    #[test]
    fn a_search_can_be_kept_to_some_files_and_paged() {
        let dir = tempfile::tempdir().unwrap();
        let index = open(dir.path());
        let (a, b) = (FileId::new(), FileId::new());
        index.put(a, "a1", "A", &book()).unwrap();
        let elsewhere = passage(0, 1, "", "Mortimer, in another book entirely.");
        index.put(b, "b1", "B", &[elsewhere]).unwrap();
        index.commit().unwrap();

        assert_eq!(index.search("mortimer", &[], 10, 0).unwrap().1, 3);
        let (only_b, total) = index.search("mortimer", &[b], 10, 0).unwrap();
        assert_eq!((only_b.len(), total), (1, 1));
        assert_eq!(only_b[0].file, b);
        let (both, _) = index.search("mortimer", &[a, b], 10, 0).unwrap();
        assert_eq!(both.len(), 3);

        let (first, total) = index.search("mortimer", &[], 2, 0).unwrap();
        let (second, _) = index.search("mortimer", &[], 2, 2).unwrap();
        assert_eq!((first.len(), second.len(), total), (2, 1, 3));
        let seen = |h: &IndexHit| first.iter().any(|f| (f.file, f.block) == (h.file, h.block));
        assert!(!second.iter().any(seen));
        assert_eq!(index.search("mortimer", &[], 0, 0).unwrap(), (Vec::new(), 0));
    }

    #[test]
    fn phrases_stay_together_and_accents_and_case_do_not_matter() {
        let dir = tempfile::tempdir().unwrap();
        let index = open(dir.path());
        let file = FileId::new();
        index
            .put(
                file,
                "v1",
                "T",
                &[
                    passage(0, 1, "", "The caf\u{e9} by the river was na\u{ef}ve about prices."),
                    passage(1, 1, "", "A river by the cafe: the prices were naive."),
                ],
            )
            .unwrap();
        index.commit().unwrap();

        assert_eq!(index.search("NAIVE", &[], 10, 0).unwrap().1, 2);
        assert_eq!(index.search("caf\u{e9}", &[], 10, 0).unwrap().1, 2);
        let (phrase, _) = index.search("\"by the river\"", &[], 10, 0).unwrap();
        assert_eq!(phrase.len(), 1);
        assert_eq!(phrase[0].block, 0);
    }

    #[test]
    fn a_query_that_will_not_parse_is_searched_as_its_words() {
        let dir = tempfile::tempdir().unwrap();
        let index = open(dir.path());
        index.put(FileId::new(), "v1", "T", &book()).unwrap();
        index.commit().unwrap();

        // An open bracket and a field that is not there.
        for query in ["lantern AND (", "nosuchfield:lantern", "lantern )", "\"lantern"] {
            let (hits, _) = index.search(query, &[], 10, 0).unwrap();
            assert_eq!(hits.len(), 1, "{query:?}");
        }
        assert_eq!(index.search("", &[], 10, 0).unwrap(), (Vec::new(), 0));
        assert_eq!(index.search("  !!! ", &[], 10, 0).unwrap().0.len(), 0);
    }

    #[test]
    fn chinese_japanese_and_korean_are_searchable() {
        let dir = tempfile::tempdir().unwrap();
        let index = open(dir.path());
        let file = FileId::new();
        let japanese = passage(0, 1, "", "\u{4f1a}\u{8b70}\u{9332}\u{3092}\u{8aad}\u{3080}");
        index.put(file, "v1", "T", &[japanese]).unwrap();
        index.commit().unwrap();
        assert_eq!(index.search("\u{4f1a}\u{8b70}", &[], 10, 0).unwrap().1, 1);
    }

    #[test]
    fn clearing_empties_it_and_it_can_be_filled_again() {
        let dir = tempfile::tempdir().unwrap();
        let index = open(dir.path());
        let file = FileId::new();
        index.put(file, "v1", "T", &book()).unwrap();
        index.commit().unwrap();
        index.clear().unwrap();
        assert!(index.indexed().unwrap().is_empty());
        assert_eq!(index.search("mortimer", &[], 10, 0).unwrap().1, 0);

        index.put(file, "v2", "T", &book()).unwrap();
        index.commit().unwrap();
        assert_eq!(index.version(file).unwrap().as_deref(), Some("v2"));
        assert_eq!(index.search("mortimer", &[], 10, 0).unwrap().1, 2);
    }

    #[test]
    fn it_is_there_when_opened_again_and_sealed_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let file = FileId::new();
        {
            let index = open(dir.path());
            index.put(file, "v1", "The Hill", &book()).unwrap();
            index.commit().unwrap();
        }
        let index = open(dir.path());
        assert_eq!(index.version(file).unwrap().as_deref(), Some("v1"));
        assert_eq!(index.search("xylophone", &[], 10, 0).unwrap().1, 1);

        // Nothing a passage says is in a file in the clear.
        let mut files = 0;
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            files += 1;
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains("xylophone") && !text.contains("Mortimer"));
        }
        assert!(files > 0);
    }

    #[test]
    fn an_index_that_the_key_does_not_open_starts_again_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        {
            let index = DocIndex::open(dir.path(), cipher(1), 1024 * 1024).unwrap();
            index.put(FileId::new(), "v1", "T", &book()).unwrap();
            index.commit().unwrap();
        }
        // Another key: the old index is unreadable, so it is made afresh.
        let index = DocIndex::open(dir.path(), cipher(2), 1024 * 1024).unwrap();
        assert!(index.indexed().unwrap().is_empty());
        let file = FileId::new();
        index.put(file, "v1", "T", &book()).unwrap();
        index.commit().unwrap();
        assert_eq!(index.search("lantern", &[], 10, 0).unwrap().1, 1);
    }

    #[test]
    fn a_second_handle_reads_and_cannot_write() {
        let dir = tempfile::tempdir().unwrap();
        let first = open(dir.path());
        let file = FileId::new();
        first.put(file, "v1", "T", &book()).unwrap();
        first.commit().unwrap();

        let second = open(dir.path());
        assert_eq!(second.search("lantern", &[], 10, 0).unwrap().1, 1);
        assert!(second.put(FileId::new(), "v1", "T", &book()).is_err());

        // What the first commits afterwards, the second sees on its next search.
        first.put(file, "v2", "T", &[passage(0, 1, "", "A fresh word: zeppelin.")]).unwrap();
        first.commit().unwrap();
        assert_eq!(second.search("zeppelin", &[], 10, 0).unwrap().1, 1);
        assert_eq!(second.version(file).unwrap().as_deref(), Some("v2"));
    }
}
