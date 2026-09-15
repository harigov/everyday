//! [`MailIndex`], the [`MailSearch`] tantivy provides.
//!
//! # Ordering and the keyset cursor
//!
//! Newest first: the primary sort key is `date`, descending, matching
//! `docs/plans/mail.md`'s "mail search sorts newest first, with relevance
//! breaking ties among free-text matches". "Breaking ties" names what score
//! is allowed to decide: [`query::build`] is what score comes from, and a
//! document either matches a query or it does not, so score has already
//! done its work by the time a hit reaches this module's own collector.
//! What free-text relevance never gets to decide is a *page's* order:
//! [`MailIndex::search`]'s collector orders strictly by `(date, message_key)`,
//! both descending, matching exactly what [`SearchPage::next`]'s cursor
//! carries and [`query::cursor_filter`] filters on. Score and the page
//! order are two different questions -- "does this match, and how well"
//! against "where does it sit relative to its neighbours" -- and keeping
//! them answered by two different things is what keeps paging correct: an
//! earlier version of this module ordered by `(date, score)` while the
//! cursor filtered on `(date, message_key)`, which agree only when every
//! date in the result set is unique. They are not -- a mailbox with a few
//! hundred messages routinely has several sharing the same microsecond,
//! particularly from bulk imports and providers whose own clock resolution
//! is coarser than that -- so a page boundary landing mid-tie skipped or
//! repeated whichever documents fell on the wrong side of it, deterministically,
//! every time that page was requested. Ordering and filtering on the same
//! key removes the disagreement rather than trying to make the two
//! occasionally-differing orders agree by chance.
//!
//! `message_key` -- compared as raw bytes, which is how the term dictionary
//! already orders `STRING` terms, and how [`query::cursor_filter`]'s own
//! `RangeQuery` compares it -- is not a synthetic tiebreak invented for
//! this: it is what makes the order total. Two messages can share a `date`
//! down to the microsecond; no two ever share a `message_key`. This is the
//! trade `docs/plans/mail.md` describes as "keyset-style cursors on (date,
//! message_key)".
//!
//! # The writer
//!
//! One [`IndexWriter`], held for the life of the [`MailIndex`], with a
//! bounded heap ([`WRITER_HEAP_BYTES`]) and a single indexing thread — mail
//! sync commits in batches already (see `docs/plans/mail.md`'s "how a
//! mailbox arrives"), so there is no call for tantivy's own multithreaded
//! indexing to add scheduling overhead on top of batches an outer loop is
//! already sizing.
//!
//! [`IndexReader`] is built with [`ReloadPolicy::Manual`], and
//! [`MailIndex::commit`] calls [`IndexReader::reload`] itself, synchronously,
//! right after [`IndexWriter::commit`] returns. `docs/plans/mail.md` asks for
//! "a `ReaderReloadPolicy` that picks up commits" — tantivy's own
//! `OnCommitWithDelay` does that too, but through `directory.rs`'s `watch`
//! callback firing on a *separate* thread with no promised timing, which
//! would make "call `commit`, then `search`" race its own reload on every
//! caller in this crate, the one process that ever writes and reads the same
//! `MailIndex` in the same breath. Reloading inline instead makes
//! [`MailSearch::commit`]'s own docs — "make every write visible to new
//! searches" — literally true the instant it returns, at the cost of paying
//! the reload (cheap: swapping in newly-opened segment readers, not
//! re-reading everything) on the writer's thread rather than a background
//! one. A second, read-only process opening the same directory would still
//! want `OnCommitWithDelay`, which is exactly what `watch` remains for.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use everyday_core::crypto::Cipher;
use everyday_core::error::{Error, Result};
use everyday_core::{Hit, MailDoc, MailQuery, MailSearch, MessageKey, SearchCursor, SearchPage};
use jiff::Timestamp;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query};
use tantivy::schema::{TantivyDocument, Term, Value};
use tantivy::{DocId, Index, IndexReader, IndexWriter, ReloadPolicy, Score, SegmentReader};

use crate::directory::SealedDirectory;
use crate::query;
use crate::schema::{self, Fields};
use crate::tokenizer::{ADDRESS_TOKENIZER, AddressTokenizer};

/// The indexing heap tantivy is given, per the one thread that uses it. Well
/// above tantivy's own minimum (15 MB) and generous enough that a sync
/// pass's batch — a few hundred messages, per `docs/plans/mail.md`'s "how a
/// mailbox arrives" — commits without tantivy forcing an early flush
/// mid-batch, while still being a bounded, known quantity rather than
/// "however much the batch happens to need".
pub const WRITER_HEAP_BYTES: usize = 64 * 1024 * 1024;

/// tantivy's search, sealed. See the module docs.
pub struct MailIndex {
    fields: Fields,
    opened: Option<Opened>,
}

struct Opened {
    index: Index,
    writer: Mutex<IndexWriter>,
    reader: IndexReader,
}

fn wrap_tantivy(e: tantivy::TantivyError) -> Error {
    Error::Backend(Box::new(e))
}

fn register_tokenizers(index: &Index) {
    index.tokenizers().register(ADDRESS_TOKENIZER, AddressTokenizer);
}

impl MailIndex {
    /// Open (creating if necessary) the sealed index at `dir`.
    ///
    /// This does not fail merely because the index cannot be opened under
    /// `cipher` — a wrong key or a missing/corrupt directory leaves
    /// [`MailIndex::rebuild_needed`] `true` rather than propagating an
    /// error, because the index is a derived structure a caller is
    /// expected to recreate from the vault's own records, not a reason to
    /// refuse to construct the object that would let it do so. See
    /// [`MailSearch::rebuild_needed`]'s docs.
    pub fn open(
        dir: impl Into<PathBuf>,
        cipher: Arc<dyn Cipher>,
        cache_bytes: usize,
    ) -> Result<Self> {
        let (schema, fields) = schema::build();
        let directory = SealedDirectory::open(dir, cipher, cache_bytes)?;
        let opened = Index::open_or_create(directory, schema).ok().and_then(|index| {
            register_tokenizers(&index);
            let writer = index.writer_with_num_threads(1, WRITER_HEAP_BYTES).ok()?;
            let reader =
                index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into().ok()?;
            Some(Opened { index, writer: Mutex::new(writer), reader })
        });
        Ok(Self { fields, opened })
    }

    fn require_opened(&self) -> Result<&Opened> {
        self.opened
            .as_ref()
            .ok_or_else(|| Error::Invalid("the mail search index needs to be rebuilt".into()))
    }

    fn build_document(&self, doc: &MailDoc) -> TantivyDocument {
        let f = &self.fields;
        let mut d = TantivyDocument::default();
        d.add_text(f.message_key, &doc.message_key);
        d.add_text(f.thread_key, &doc.thread_key);
        d.add_text(f.account, doc.account.to_lowercase());
        for mailbox in &doc.mailboxes {
            d.add_text(f.mailboxes, mailbox.to_lowercase());
        }
        for label in &doc.labels {
            d.add_text(f.labels, label.to_lowercase());
        }
        d.add_text(f.from, &doc.from);
        d.add_text(f.to, &doc.to);
        d.add_text(f.cc, &doc.cc);
        d.add_text(f.subject, &doc.subject);
        d.add_text(f.body_text, &doc.body_text);
        d.add_i64(f.date, doc.date.as_microsecond());
        d.add_bool(f.has_attachment, doc.has_attachment);
        d.add_bool(f.unread, doc.unread);
        d.add_bool(f.starred, doc.starred);
        d
    }
}

/// The sort key [`MailIndex::search`]'s collector orders by: `date`
/// descending, then `message_key` descending as the deterministic tiebreak.
/// See the module docs' "ordering and the keyset cursor" for why that pair,
/// and why `score` -- present on every `RankKey`, needed to fill in
/// [`Hit::score`] once a document has been chosen -- plays no part in
/// comparing two of them. That is exactly why `PartialOrd` is written by
/// hand below rather than derived over all three fields: a derive would
/// have silently put score back into the comparison the first time someone
/// reordered the struct's fields.
#[derive(Debug, Clone)]
struct RankKey {
    date: i64,
    message_key: String,
    score: Score,
}

impl PartialEq for RankKey {
    fn eq(&self, other: &Self) -> bool {
        self.date == other.date && self.message_key == other.message_key
    }
}

impl PartialOrd for RankKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        (self.date, &self.message_key).partial_cmp(&(other.date, &other.message_key))
    }
}

impl MailSearch for MailIndex {
    fn index(&self, docs: &[MailDoc]) -> Result<()> {
        if docs.is_empty() {
            return Ok(());
        }
        let opened = self.require_opened()?;
        let writer = opened.writer.lock().unwrap_or_else(|e| e.into_inner());
        for doc in docs {
            writer.delete_term(Term::from_field_text(self.fields.message_key, &doc.message_key));
            writer.add_document(self.build_document(doc)).map_err(wrap_tantivy)?;
        }
        Ok(())
    }

    fn delete(&self, message_ids: &[MessageKey]) -> Result<()> {
        if message_ids.is_empty() {
            return Ok(());
        }
        let opened = self.require_opened()?;
        let writer = opened.writer.lock().unwrap_or_else(|e| e.into_inner());
        for key in message_ids {
            writer.delete_term(Term::from_field_text(self.fields.message_key, key));
        }
        Ok(())
    }

    fn delete_account(&self, account: &str) -> Result<()> {
        let opened = self.require_opened()?;
        let writer = opened.writer.lock().unwrap_or_else(|e| e.into_inner());
        // `account` is a `STRING` field -- see `schema::build` -- so the
        // whole lowercased value is one term, the same one `build_document`
        // indexed it as, and this single `delete_term` removes every
        // document that account ever had in one call.
        writer.delete_term(Term::from_field_text(self.fields.account, &account.to_lowercase()));
        Ok(())
    }

    fn commit(&self) -> Result<()> {
        let opened = self.require_opened()?;
        let mut writer = opened.writer.lock().unwrap_or_else(|e| e.into_inner());
        writer.commit().map_err(wrap_tantivy)?;
        // See the module docs: reloading inline, rather than trusting the
        // async `OnCommitWithDelay` machinery, is what makes this method's
        // own promise -- visible to new searches -- true by the time it
        // returns.
        opened.reader.reload().map_err(wrap_tantivy)?;
        Ok(())
    }

    fn search(
        &self,
        query: &MailQuery,
        limit: usize,
        cursor: Option<SearchCursor>,
    ) -> Result<SearchPage> {
        let limit = limit.max(1);
        let opened = self.require_opened()?;
        let now = Timestamp::now();
        let base = query::build(&opened.index, &self.fields, query, now);
        let full: Box<dyn Query> = match &cursor {
            Some(c) => Box::new(BooleanQuery::new(vec![
                (Occur::Must, base),
                (Occur::Must, query::cursor_filter(&self.fields, c)),
            ])),
            None => base,
        };

        let searcher = opened.reader.searcher();
        let date_field = self.fields.date;
        let message_key_field = self.fields.message_key;
        let collector =
            TopDocs::with_limit(limit).tweak_score(move |segment_reader: &SegmentReader| {
                let date_col = segment_reader
                    .fast_fields()
                    .i64(schema_field_name(segment_reader.schema(), date_field))
                    .unwrap_or_else(|_| panic!("date field must be a fast field"));
                // `message_key` is `STRING | STORED | FAST` (`schema.rs`'s
                // own docs on why); the fast half is exactly what lets a
                // page's own order be compared against without a second
                // trip through the stored document for every candidate
                // `tweak_score` considers, not only the ones that make the
                // final page.
                let message_key_col = segment_reader
                    .fast_fields()
                    .str(schema_field_name(segment_reader.schema(), message_key_field))
                    .unwrap_or_else(|_| panic!("message_key field must be a fast field"))
                    .unwrap_or_else(|| panic!("message_key field must be a string fast field"));
                move |doc: DocId, score: Score| {
                    let date = date_col.first(doc).unwrap_or(i64::MIN);
                    let mut message_key = String::new();
                    if let Some(ord) = message_key_col.term_ords(doc).next() {
                        let _ = message_key_col.ord_to_str(ord, &mut message_key);
                    }
                    RankKey { date, message_key, score }
                }
            });

        let results = searcher.search(full.as_ref(), &collector).map_err(wrap_tantivy)?;

        let mut hits = Vec::with_capacity(results.len());
        for (rank, addr) in &results {
            let doc: TantivyDocument = searcher.doc(*addr).map_err(wrap_tantivy)?;
            let thread_key = text_value(&doc, self.fields.thread_key)?;
            let date = Timestamp::from_microsecond(rank.date)
                .map_err(|e| Error::Invalid(format!("indexed date out of range: {e}")))?;
            hits.push(Hit {
                message_key: rank.message_key.clone(),
                thread_key,
                score: rank.score,
                date,
            });
        }

        let next = if hits.len() >= limit {
            hits.last().map(|h| SearchCursor { date: h.date, message_key: h.message_key.clone() })
        } else {
            None
        };
        Ok(SearchPage { hits, next })
    }

    fn rebuild_needed(&self) -> bool {
        self.opened.is_none()
    }
}

fn schema_field_name(schema: &tantivy::schema::Schema, field: tantivy::schema::Field) -> &str {
    schema.get_field_name(field)
}

fn text_value(doc: &TantivyDocument, field: tantivy::schema::Field) -> Result<String> {
    doc.get_first(field)
        .and_then(|v| Value::as_str(&v).map(str::to_string))
        .ok_or_else(|| Error::Invalid("indexed document is missing a required stored field".into()))
}
