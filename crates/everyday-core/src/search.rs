//! Full-text search.
//!
//! # Why search lives here and not in the backend
//!
//! A vault's contents are encrypted at rest, so there is nothing for a
//! backend-native index (SQLite FTS5, ripgrep over Markdown) to read: the
//! bytes on disk are ciphertext. The index therefore has to be built from
//! *decrypted* entries, which means it only exists while the vault is
//! unlocked — and if it only exists in memory anyway, it may as well be
//! shared by every backend rather than reimplemented in each one.
//!
//! The index is rebuilt on unlock and maintained incrementally after that.
//! For a decade of daily journalling — call it 5 000 entries — it costs a
//! few megabytes and builds in well under a second.
//!
//! Ranking is [BM25], with a boost for matches in the title, which is what
//! makes searching for a remembered entry title feel immediate.
//!
//! [BM25]: https://en.wikipedia.org/wiki/Okapi_BM25

use crate::id::{EntryId, JournalId};
use crate::model::Entry;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Standard BM25 term-frequency saturation.
const K1: f32 = 1.2;
/// Standard BM25 length normalisation.
const B: f32 = 0.75;
/// How much more a title match is worth than a body match.
const TITLE_BOOST: f32 = 3.0;
/// Characters of context either side of a snippet match.
const SNIPPET_RADIUS: usize = 90;

/// How many tombstones the index tolerates before it compacts.
///
/// Editing a document is a remove followed by an insert, and an open entry
/// is re-inserted on every autosave — once every 700 ms of typing. Without a
/// sweep, an afternoon's writing leaves tens of thousands of dead documents
/// in `docs` and a dead posting in every list the entry ever touched, so the
/// index grows without bound and every query walks the wreckage.
///
/// The floor keeps a small vault from compacting on its third edit; the
/// proportion keeps the sweep amortised, since reaching it again costs as
/// many inserts as there are live documents.
const COMPACT_FLOOR: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Posting {
    doc: u32,
    tf: u32,
    in_title: bool,
}

#[derive(Debug, Clone)]
struct Doc {
    id: EntryId,
    journal_id: JournalId,
    title: String,
    /// Lowercased body text, retained so snippets can be cut from it.
    text: String,
    local_date: Date,
    len: u32,
    /// Tombstone: removed docs are cleared rather than compacted, so that
    /// the `u32` doc ids in every posting list stay valid.
    live: bool,
}

/// A ranked search result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub id: EntryId,
    pub journal_id: JournalId,
    pub title: String,
    pub local_date: Date,
    pub score: f32,
    /// A window of body text around the best match.
    pub snippet: String,
    /// `(start, end)` byte ranges *within `snippet`* that matched, so the UI
    /// can highlight without re-running the tokenizer in JavaScript.
    pub highlights: Vec<(usize, usize)>,
}

/// An in-memory inverted index over the entries of an unlocked vault.
#[derive(Debug, Default)]
pub struct SearchIndex {
    docs: Vec<Doc>,
    by_id: HashMap<EntryId, u32>,
    /// A `BTreeMap` rather than a hash map so that prefix queries — which is
    /// every query, while the user is still typing — are a range scan.
    postings: BTreeMap<String, Vec<Posting>>,
    live_docs: u32,
    total_len: u64,
}

impl SearchIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn build(entries: &[Entry]) -> Self {
        let mut idx = Self::new();
        for e in entries {
            idx.insert(e);
        }
        idx
    }

    pub fn len(&self) -> usize {
        self.live_docs as usize
    }

    pub fn is_empty(&self) -> bool {
        self.live_docs == 0
    }

    /// Add or replace an entry.
    pub fn insert(&mut self, entry: &Entry) {
        self.remove(entry.id);

        let title = entry.display_title();
        let body = entry.searchable_text();

        let mut tf: HashMap<String, (u32, bool)> = HashMap::new();
        let mut len = 0u32;
        for tok in tokenize(&title) {
            let slot = tf.entry(tok.text).or_insert((0, false));
            slot.0 += 1;
            slot.1 = true;
            len += 1;
        }
        for tok in tokenize(&body) {
            let slot = tf.entry(tok.text).or_insert((0, false));
            slot.0 += 1;
            len += 1;
        }

        let doc_id = self.docs.len() as u32;
        for (term, (count, in_title)) in tf {
            self.postings.entry(term).or_default().push(Posting {
                doc: doc_id,
                tf: count,
                in_title,
            });
        }

        self.docs.push(Doc {
            id: entry.id,
            journal_id: entry.journal_id,
            title,
            text: body,
            local_date: entry.local_date,
            len,
            live: true,
        });
        self.by_id.insert(entry.id, doc_id);
        self.live_docs += 1;
        self.total_len += u64::from(len);
        self.compact_if_needed();
    }

    /// Number of tombstoned documents still occupying space.
    fn dead_docs(&self) -> usize {
        self.docs.len() - self.live_docs as usize
    }

    /// Sweep the tombstones out when they start to outnumber the real work.
    fn compact_if_needed(&mut self) {
        let dead = self.dead_docs();
        if dead > COMPACT_FLOOR && dead >= self.live_docs as usize {
            self.compact();
        }
    }

    /// Drop every tombstoned document and the postings that point at it.
    ///
    /// Doc ids are positions in `docs`, so removing a document renumbers
    /// everything after it: the remap is built first and every surviving
    /// posting is rewritten through it. Nothing outside this type holds a
    /// doc id, so the renumbering is invisible.
    ///
    /// This is a walk of the postings rather than a re-tokenisation of the
    /// documents — the terms are already here, and re-deriving them would
    /// cost the same as building the index from scratch.
    fn compact(&mut self) {
        let mut remap: Vec<Option<u32>> = Vec::with_capacity(self.docs.len());
        let mut next = 0u32;
        for doc in &self.docs {
            if doc.live {
                remap.push(Some(next));
                next += 1;
            } else {
                remap.push(None);
            }
        }

        self.docs.retain(|d| d.live);
        for postings in self.postings.values_mut() {
            postings.retain_mut(|p| match remap[p.doc as usize] {
                Some(to) => {
                    p.doc = to;
                    true
                }
                None => false,
            });
        }
        // A term whose every document is gone is a key nothing can match,
        // and it would still be walked by every prefix range scan.
        self.postings.retain(|_, postings| !postings.is_empty());

        for (id, doc_id) in self.by_id.iter_mut() {
            debug_assert!(
                remap[*doc_id as usize].is_some(),
                "{id:?} is live but was remapped away"
            );
            *doc_id = remap[*doc_id as usize].unwrap_or(*doc_id);
        }
    }

    /// Remove an entry.
    ///
    /// Tombstoned rather than cut out, because doc ids are positions in
    /// `docs` and every posting holds one. The tombstones are swept by
    /// [`SearchIndex::compact`] once there are enough of them to be worth
    /// renumbering for.
    pub fn remove(&mut self, id: EntryId) {
        let Some(doc_id) = self.by_id.remove(&id) else { return };
        let doc = &mut self.docs[doc_id as usize];
        if !doc.live {
            return;
        }
        doc.live = false;
        doc.text.clear();
        doc.title.clear();
        self.live_docs -= 1;
        self.total_len -= u64::from(doc.len);
        self.compact_if_needed();
    }

    /// Search. The final term is treated as a prefix so results update on
    /// every keystroke; earlier terms must match whole tokens.
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        self.search_in(query, None, limit)
    }

    /// As [`SearchIndex::search`], restricted to one journal.
    pub fn search_in(
        &self,
        query: &str,
        journal: Option<JournalId>,
        limit: usize,
    ) -> Vec<SearchHit> {
        let terms: Vec<String> = tokenize(query).into_iter().map(|t| t.text).collect();
        if terms.is_empty() || self.live_docs == 0 {
            return Vec::new();
        }

        let avg_len = self.total_len as f32 / self.live_docs as f32;
        let n = self.live_docs as f32;

        // Accumulate BM25 per document, and require every term to hit so
        // that multi-word queries narrow rather than widen.
        let mut scores: HashMap<u32, f32> = HashMap::new();
        let mut hit_count: HashMap<u32, usize> = HashMap::new();

        for (i, term) in terms.iter().enumerate() {
            let is_last = i + 1 == terms.len();
            let matched = self.postings_for(term, is_last);
            if matched.is_empty() {
                return Vec::new(); // an unmatched term means no results
            }

            // Document frequency across all expansions of this (prefix) term.
            let df = {
                let mut docs: Vec<u32> = matched
                    .iter()
                    .flat_map(|p| p.iter())
                    .filter(|p| self.docs[p.doc as usize].live)
                    .map(|p| p.doc)
                    .collect();
                docs.sort_unstable();
                docs.dedup();
                docs.len() as f32
            };
            if df == 0.0 {
                return Vec::new();
            }
            let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();

            let mut seen_this_term: HashMap<u32, ()> = HashMap::new();
            for postings in matched {
                for p in postings {
                    let doc = &self.docs[p.doc as usize];
                    if !doc.live {
                        continue;
                    }
                    if let Some(j) = journal
                        && doc.journal_id != j
                    {
                        continue;
                    }
                    let tf = p.tf as f32;
                    let norm = 1.0 - B + B * (doc.len as f32 / avg_len.max(1.0));
                    let mut s = idf * (tf * (K1 + 1.0)) / (tf + K1 * norm);
                    if p.in_title {
                        s *= TITLE_BOOST;
                    }
                    *scores.entry(p.doc).or_insert(0.0) += s;
                    if seen_this_term.insert(p.doc, ()).is_none() {
                        *hit_count.entry(p.doc).or_insert(0) += 1;
                    }
                }
            }
        }

        let mut ranked: Vec<(u32, f32)> = scores
            .into_iter()
            .filter(|(doc, _)| hit_count.get(doc).copied().unwrap_or(0) == terms.len())
            .collect();

        ranked.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                // Equal relevance: prefer the more recent entry, then fall
                // back to the id so results are deterministic.
                .then_with(|| {
                    self.docs[b.0 as usize].local_date.cmp(&self.docs[a.0 as usize].local_date)
                })
                .then_with(|| self.docs[a.0 as usize].id.cmp(&self.docs[b.0 as usize].id))
        });
        ranked.truncate(limit);

        ranked
            .into_iter()
            .map(|(doc_id, score)| {
                let doc = &self.docs[doc_id as usize];
                let (snippet, highlights) = snippet_for(&doc.text, &terms);
                SearchHit {
                    id: doc.id,
                    journal_id: doc.journal_id,
                    title: doc.title.clone(),
                    local_date: doc.local_date,
                    score,
                    snippet,
                    highlights,
                }
            })
            .collect()
    }

    /// Posting lists for a term. With `prefix`, every term starting with it.
    fn postings_for(&self, term: &str, prefix: bool) -> Vec<&Vec<Posting>> {
        if !prefix {
            return self.postings.get(term).into_iter().collect();
        }
        self.postings
            .range(term.to_string()..)
            .take_while(|(k, _)| k.starts_with(term))
            .map(|(_, v)| v)
            .collect()
    }

    /// Every distinct term, for autocomplete. Sorted.
    pub fn terms_with_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        let prefix = prefix.to_lowercase();
        self.postings
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .filter(|(_, v)| v.iter().any(|p| self.docs[p.doc as usize].live))
            .map(|(k, _)| k.clone())
            .take(limit)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    text: String,
    start: usize,
    end: usize,
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF   // hiragana + katakana
        | 0x3400..=0x4DBF // CJK ext A
        | 0x4E00..=0x9FFF // CJK unified
        | 0xF900..=0xFAFF // compatibility ideographs
        | 0xAC00..=0xD7AF // hangul syllables
        | 0x20000..=0x2FA1F)
}

/// Split text into lowercase tokens.
///
/// Latin-style scripts tokenize on word boundaries. CJK has no spaces, so
/// each character becomes a token *and* adjacent pairs become bigrams —
/// without the bigrams, searching for a two-character word would match every
/// entry containing either character.
fn tokenize(s: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_start = 0usize;
    let mut prev_cjk: Option<(char, usize, usize)> = None;

    let flush = |cur: &mut String, start: usize, end: usize, out: &mut Vec<Token>| {
        if !cur.is_empty() {
            out.push(Token { text: std::mem::take(cur), start, end });
        }
    };

    for (i, c) in s.char_indices() {
        let end = i + c.len_utf8();
        if is_cjk(c) {
            flush(&mut cur, cur_start, i, &mut out);
            out.push(Token { text: c.to_lowercase().collect(), start: i, end });
            if let Some((p, ps, _)) = prev_cjk {
                let mut bigram = String::new();
                bigram.extend(p.to_lowercase());
                bigram.extend(c.to_lowercase());
                out.push(Token { text: bigram, start: ps, end });
            }
            prev_cjk = Some((c, i, end));
        } else if c.is_alphanumeric() || c == '_' {
            prev_cjk = None;
            if cur.is_empty() {
                cur_start = i;
            }
            cur.extend(c.to_lowercase());
        } else {
            prev_cjk = None;
            flush(&mut cur, cur_start, i, &mut out);
        }
    }
    flush(&mut cur, cur_start, s.len(), &mut out);
    out
}

/// Cut a window of `text` around the earliest match of any query term, and
/// report the highlight ranges relative to that window.
fn snippet_for(text: &str, terms: &[String]) -> (String, Vec<(usize, usize)>) {
    let tokens = tokenize(text);
    let matches: Vec<&Token> =
        tokens.iter().filter(|t| terms.iter().any(|q| t.text.starts_with(q.as_str()))).collect();

    let Some(first) = matches.first() else {
        let head = clamp_to_char_boundary(text, SNIPPET_RADIUS * 2);
        return (text[..head].to_string(), Vec::new());
    };

    // Widen to char boundaries around the first match.
    let start = floor_char_boundary(text, first.start.saturating_sub(SNIPPET_RADIUS));
    let end = ceil_char_boundary(text, (first.end + SNIPPET_RADIUS).min(text.len()));

    let mut snippet = String::new();
    if start > 0 {
        snippet.push('\u{2026}');
    }
    let prefix_len = snippet.len();
    snippet.push_str(text[start..end].trim_end());

    let highlights = matches
        .iter()
        .filter(|t| t.start >= start && t.end <= end)
        .map(|t| (t.start - start + prefix_len, t.end - start + prefix_len))
        .filter(|(_, e)| *e <= snippet.len())
        .collect();

    if end < text.len() {
        snippet.push('\u{2026}');
    }
    (snippet, highlights)
}

fn clamp_to_char_boundary(s: &str, n: usize) -> usize {
    if n >= s.len() { s.len() } else { floor_char_boundary(s, n) }
}

fn floor_char_boundary(s: &str, mut n: usize) -> usize {
    if n >= s.len() {
        return s.len();
    }
    while n > 0 && !s.is_char_boundary(n) {
        n -= 1;
    }
    n
}

fn ceil_char_boundary(s: &str, mut n: usize) -> usize {
    if n >= s.len() {
        return s.len();
    }
    while n < s.len() && !s.is_char_boundary(n) {
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::richtext::RichDoc;

    fn entry(jid: JournalId, title: &str, body: &str) -> Entry {
        let mut e = Entry::new(jid, "UTC");
        e.title = title.into();
        e.body = RichDoc::from_plain_text(body);
        e
    }

    fn index_of(pairs: &[(&str, &str)]) -> (SearchIndex, JournalId, Vec<EntryId>) {
        let jid = JournalId::new();
        let entries: Vec<Entry> = pairs.iter().map(|(t, b)| entry(jid, t, b)).collect();
        let ids = entries.iter().map(|e| e.id).collect();
        (SearchIndex::build(&entries), jid, ids)
    }

    #[test]
    fn tokenizer_splits_on_punctuation_and_lowercases() {
        let toks: Vec<String> =
            tokenize("Hello, World! it's 2025").into_iter().map(|t| t.text).collect();
        assert_eq!(toks, ["hello", "world", "it", "s", "2025"]);
    }

    #[test]
    fn tokenizer_emits_cjk_unigrams_and_bigrams() {
        let toks: Vec<String> = tokenize("\u{65e5}\u{8a18}").into_iter().map(|t| t.text).collect();
        assert_eq!(toks, ["\u{65e5}", "\u{8a18}", "\u{65e5}\u{8a18}"]);
    }

    #[test]
    fn finds_an_entry_by_a_body_word() {
        let (idx, _, ids) = index_of(&[("Monday", "the heron stood in the shallows")]);
        let hits = idx.search("heron", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, ids[0]);
    }

    #[test]
    fn matches_the_last_term_as_a_prefix_for_type_ahead() {
        let (idx, _, _) = index_of(&[("Monday", "the heron stood in the shallows")]);
        assert_eq!(idx.search("her", 10).len(), 1);
        assert_eq!(idx.search("hero", 10).len(), 1);
        assert_eq!(idx.search("herox", 10).len(), 0);
    }

    #[test]
    fn multiple_terms_are_anded() {
        let (idx, _, ids) =
            index_of(&[("Monday", "heron in the shallows"), ("Tuesday", "heron on the roof")]);
        let hits = idx.search("heron shallows", 10);
        assert_eq!(hits.len(), 1, "both terms must match the same entry");
        assert_eq!(hits[0].id, ids[0]);
    }

    #[test]
    fn an_unmatched_term_yields_no_results() {
        let (idx, _, _) = index_of(&[("Monday", "heron")]);
        assert!(idx.search("heron zebra", 10).is_empty());
        assert!(idx.search("", 10).is_empty());
        assert!(idx.search("   ", 10).is_empty());
    }

    #[test]
    fn title_matches_outrank_body_matches() {
        let (idx, _, ids) = index_of(&[
            ("A day of nothing", "we talked about herons for a while, herons herons"),
            ("Herons", "a quiet afternoon"),
        ]);
        let hits = idx.search("herons", 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, ids[1], "the entry titled 'Herons' should rank first");
    }

    #[test]
    fn search_can_be_scoped_to_one_journal() {
        let a = JournalId::new();
        let b = JournalId::new();
        let idx = SearchIndex::build(&[entry(a, "one", "heron"), entry(b, "two", "heron")]);
        assert_eq!(idx.search("heron", 10).len(), 2);
        assert_eq!(idx.search_in("heron", Some(a), 10).len(), 1);
    }

    #[test]
    fn removed_entries_disappear_from_results() {
        let (mut idx, _, ids) = index_of(&[("Monday", "heron")]);
        assert_eq!(idx.len(), 1);
        idx.remove(ids[0]);
        assert!(idx.search("heron", 10).is_empty());
        assert_eq!(idx.len(), 0);
        // Removing twice must not underflow the live-document counter.
        idx.remove(ids[0]);
        assert_eq!(idx.len(), 0);
    }

    #[test]
    fn reinserting_an_entry_replaces_the_old_text() {
        let jid = JournalId::new();
        let mut e = entry(jid, "Monday", "heron");
        let mut idx = SearchIndex::build(std::slice::from_ref(&e));

        e.body = RichDoc::from_plain_text("kingfisher");
        idx.insert(&e);

        assert!(idx.search("heron", 10).is_empty(), "stale text must not linger");
        assert_eq!(idx.search("kingfisher", 10).len(), 1);
        assert_eq!(idx.len(), 1, "replacing must not double-count the document");
    }

    #[test]
    fn snippets_highlight_the_match() {
        let (idx, _, _) = index_of(&[(
            "Long",
            &format!("{} heron {}", "padding ".repeat(40), "trailing ".repeat(40)),
        )]);
        let hit = &idx.search("heron", 1)[0];
        assert!(hit.snippet.contains("heron"));
        assert!(hit.snippet.len() < 400, "snippet should be a window, not the whole entry");
        assert_eq!(hit.highlights.len(), 1);
        let (s, e) = hit.highlights[0];
        assert_eq!(&hit.snippet[s..e], "heron", "highlight range must land on the term");
    }

    #[test]
    fn snippet_ranges_are_valid_utf8_boundaries() {
        let body = format!("{} heron", "\u{1f602}".repeat(60));
        let (idx, _, _) = index_of(&[("Emoji", &body)]);
        let hit = &idx.search("heron", 1)[0];
        // Slicing at a bad boundary would panic here.
        for (s, e) in &hit.highlights {
            assert_eq!(&hit.snippet[*s..*e], "heron");
        }
    }

    #[test]
    fn cjk_search_matches_words_not_just_characters() {
        let a = JournalId::new();
        let idx = SearchIndex::build(&[
            entry(a, "\u{65e5}\u{8a18}", "\u{4eca}\u{65e5}\u{306f}\u{6674}\u{308c}"),
            entry(a, "\u{5929}\u{6c17}", "\u{660e}\u{65e5}\u{306f}\u{96e8}"),
        ]);
        // "今日" (today) should match only the first, not everything with 日.
        let hits = idx.search("\u{4eca}\u{65e5}", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "\u{65e5}\u{8a18}");
    }

    #[test]
    fn tags_and_captions_are_searchable() {
        let jid = JournalId::new();
        let mut e = entry(jid, "Trip", "nothing much");
        e.tags = vec!["Patagonia".into()];
        let idx = SearchIndex::build(&[e]);
        assert_eq!(idx.search("patagonia", 10).len(), 1, "tags must be indexed");
    }

    #[test]
    fn term_autocomplete_returns_sorted_prefixes() {
        let (idx, _, _) = index_of(&[("x", "heron herring herbs cat")]);
        assert_eq!(idx.terms_with_prefix("her", 10), ["herbs", "heron", "herring"]);
        assert!(idx.terms_with_prefix("zz", 10).is_empty());
    }

    #[test]
    fn re_editing_one_entry_does_not_grow_the_index_without_bound() {
        // An open entry is re-inserted on every autosave. Left to itself
        // that added a document and a posting per keystroke-burst forever.
        let jid = JournalId::new();
        let mut e = entry(jid, "Monday", "heron");
        let mut idx = SearchIndex::build(std::slice::from_ref(&e));
        for i in 0..(COMPACT_FLOOR * 8) {
            e.body = RichDoc::from_plain_text(&format!("heron {i}"));
            idx.insert(&e);
        }
        assert_eq!(idx.len(), 1);
        assert!(
            idx.docs.len() <= COMPACT_FLOOR * 2,
            "tombstones must be swept, not accumulated: {} documents for one entry",
            idx.docs.len()
        );
        // And the sweep must not have broken what the index is for.
        let hits = idx.search("heron", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, e.id);
        assert!(idx.search("kingfisher", 10).is_empty());
    }

    #[test]
    fn compaction_keeps_every_surviving_entry_findable() {
        // Half the vault deleted, which is what drives the sweep from the
        // other direction, and the half left behind must still be intact.
        // The body terms are zero-padded so that no one of them is a prefix
        // of another -- the last term of a query is matched as a prefix.
        let jid = JournalId::new();
        let count = COMPACT_FLOOR * 4;
        let entries: Vec<Entry> = (0..count)
            .map(|i| entry(jid, &format!("title{i:04}"), &format!("heron body{i:04}")))
            .collect();
        let mut idx = SearchIndex::build(&entries);
        let doomed = |i: usize| i % 2 == 0;
        for (_, e) in entries.iter().enumerate().filter(|(i, _)| doomed(*i)) {
            idx.remove(e.id);
        }
        assert_eq!(idx.len(), count / 2);
        assert!(idx.docs.len() < count, "the sweep should have run");

        for (i, e) in entries.iter().enumerate() {
            let hits = idx.search(&format!("body{i:04}"), 10);
            if doomed(i) {
                assert!(hits.is_empty(), "{} was deleted", e.title);
            } else {
                assert_eq!(hits.len(), 1, "{} should still be findable", e.title);
                assert_eq!(hits[0].id, e.id);
                assert_eq!(hits[0].title, e.title, "titles must survive renumbering");
            }
        }
    }

    #[test]
    fn results_are_capped_by_the_limit() {
        let pairs: Vec<(String, String)> =
            (0..50).map(|i| (format!("e{i}"), "heron".to_string())).collect();
        let refs: Vec<(&str, &str)> = pairs.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let (idx, _, _) = index_of(&refs);
        assert_eq!(idx.search("heron", 5).len(), 5);
    }

    #[test]
    fn ranking_is_deterministic_for_identical_documents() {
        let pairs: Vec<(String, String)> =
            (0..10).map(|i| (format!("t{i}"), "heron".to_string())).collect();
        let refs: Vec<(&str, &str)> = pairs.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let (idx, _, _) = index_of(&refs);
        let first: Vec<EntryId> = idx.search("heron", 10).iter().map(|h| h.id).collect();
        for _ in 0..5 {
            let again: Vec<EntryId> = idx.search("heron", 10).iter().map(|h| h.id).collect();
            assert_eq!(first, again, "equal-scoring results must have a stable order");
        }
    }
}
