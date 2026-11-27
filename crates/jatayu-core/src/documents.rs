//! Documents: a book, an article, a manual or a page of notes in the files
//! folder, read into text that the reader can show and the assistant can find
//! its way around.
//!
//! A document is a catalogued file ([`crate::files::FileEntry`]) whose format
//! is one of [`DocFormat`]: a PDF, an EPUB, Markdown, plain text or a web
//! page. Nothing about it is stored as a record of its own. What is kept is
//! derived and can always be made again from the file: its [`DocText`],
//! parsed once and kept sealed in the cache directory, and its passages in
//! the search index -- both `jatayu-service`'s (`documents::cache`,
//! `jatayu-docindex`).
//!
//! # One address for every place in a document
//!
//! A document is a list of [`DocBlock`]s -- a paragraph, a heading, a list
//! item, a picture -- in reading order, and a block's index in that list is
//! the address of a place in it. The reader marks every block it draws with
//! that index, the assistant reads by it, a search hit points at it, and the
//! screen context says which one is at the top of the window. Because one
//! parse produces both what the reader shows and what the assistant reads,
//! "the paragraph I am looking at" and "the paragraph the model was handed"
//! are the same paragraph by construction.
//!
//! Every block also carries a page. A PDF's pages are its own. A book or a
//! note has none, so it is given made-up ones of about
//! [`SYNTHETIC_PAGE_CHARS`] each, never splitting a block: "page 47 of 312"
//! then means the same thing to the person, the reader's progress and a
//! library item's [`crate::library::Progress`].
//!
//! # Structure, as well as words
//!
//! The [`DocSection`]s are the document's own table of contents -- a PDF's
//! outline, an EPUB's navigation document -- or, without one, its headings.
//! They are what lets "what happens in chapter 3" be answered by reading
//! chapter 3 rather than the whole book, and what a search hit names so the
//! model knows where in the book a passage sits. [`DocText::search`] is a
//! plain BM25 over the blocks of one document, so "who is Mortimer" is a
//! search and a read of the pages it lands on, not a read of the book.
//!
//! # Somebody else's words
//!
//! Everything in here came out of a file, and a file may have come from
//! anywhere: a book's title, its chapter names and its paragraphs are all
//! somebody else's writing. The assistant's tools hand them over under
//! `untrusted_text`, and the screen context names a place in a document by
//! page and block, never by a heading's words.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::files::FileEntry;
use crate::id::FileId;
use crate::text::one_line;

/// What a [`DocText`] was parsed under. Raised whenever a parser changes
/// what it produces, so every cached copy is parsed again rather than read in
/// a shape the reader no longer expects.
pub const DOC_TEXT_VERSION: u32 = 1;

/// About a printed paperback page: three hundred words. What a document
/// without pages of its own is cut into -- see the module docs.
pub const SYNTHETIC_PAGE_CHARS: usize = 1_800;

/// BM25's usual term-frequency saturation and length normalisation, which
/// suit a block of a paragraph or so as well as they suit anything.
const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;

/// How much more a heading holding the words counts for than a paragraph
/// holding them just as often: "chapter 3" should find the chapter.
const HEADING_BOOST: f64 = 1.25;

/// About how long a search hit's snippet is, in characters.
const SNIPPET_CHARS: usize = 200;

/// The most terms of a query that count. A model that pastes a paragraph in
/// has not made a better search of it.
const MAX_QUERY_TERMS: usize = 24;

/// The documents this reads.
///
/// Office files are left out on purpose, as they are from the previews:
/// reading one properly needs LibreOffice or a large third-party library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum DocFormat {
    Pdf,
    Epub,
    Markdown,
    Text,
    Html,
}

impl DocFormat {
    pub const ALL: [DocFormat; 5] =
        [DocFormat::Pdf, DocFormat::Epub, DocFormat::Markdown, DocFormat::Text, DocFormat::Html];

    /// The format a file's name says, by its extension, any case.
    pub fn of_name(name: &str) -> Option<DocFormat> {
        let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
        Some(match ext.as_str() {
            "pdf" => DocFormat::Pdf,
            "epub" => DocFormat::Epub,
            "md" | "markdown" => DocFormat::Markdown,
            "txt" | "text" => DocFormat::Text,
            "html" | "htm" | "xhtml" => DocFormat::Html,
            _ => return None,
        })
    }

    /// The format of a catalogued file: its name first, then its type.
    pub fn of_file(file: &FileEntry) -> Option<DocFormat> {
        DocFormat::of_name(&file.name).or(match file.mime.as_str() {
            "application/pdf" => Some(DocFormat::Pdf),
            "application/epub+zip" => Some(DocFormat::Epub),
            "text/markdown" => Some(DocFormat::Markdown),
            "text/plain" => Some(DocFormat::Text),
            "text/html" | "application/xhtml+xml" => Some(DocFormat::Html),
            _ => None,
        })
    }

    /// What a person calls one: "a PDF", "an EPUB book".
    pub fn phrase(self) -> &'static str {
        match self {
            DocFormat::Pdf => "a PDF",
            DocFormat::Epub => "an EPUB book",
            DocFormat::Markdown => "a Markdown document",
            DocFormat::Text => "a text file",
            DocFormat::Html => "a web page",
        }
    }

    /// Whether its pages are its own (a PDF's) rather than made up.
    pub fn has_own_pages(self) -> bool {
        self == DocFormat::Pdf
    }
}

/// What a block is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum DocBlockKind {
    Heading,
    #[default]
    Paragraph,
    Quote,
    ListItem,
    /// Preformatted text: code, a poem's lines, a table drawn in text.
    Code,
    /// A picture. Its [`DocBlock::src`] says where it is inside the file and
    /// its text is its description, if it has one.
    Image,
    /// A break between scenes or sections.
    Rule,
}

/// One block of a document, in reading order. See the module docs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocBlock {
    pub kind: DocBlockKind,
    /// A heading's level, 1 for the largest; 0 for anything else.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub level: u8,
    /// The page it is on, from 1. A PDF's own page, or a made-up one.
    pub page: u32,
    /// The innermost [`DocSection`] it is in, by index into
    /// [`DocText::sections`]; `None` before the first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<u32>,
    /// Its words, as plain text: what the assistant reads and search finds.
    pub text: String,
    /// The same words as the reader draws them: HTML the parser wrote
    /// itself, from a fixed set of inline elements (`em`, `strong`, `i`,
    /// `b`, `sup`, `sub`, `code`, `br`, `a` within the document) with every
    /// attribute but a link's target left out, and every character of the
    /// file's own text escaped. Safe to put in the page as it is, which is
    /// the point of writing it here rather than passing a file's HTML on.
    /// Empty for a PDF, whose pages pdf.js draws.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub html: String,
    /// A picture's path inside the file, from the file's root (an EPUB's
    /// `OEBPS/images/map.jpg`). Empty for anything else.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub src: String,
}

fn is_zero_u8(n: &u8) -> bool {
    *n == 0
}

/// One entry of a document's table of contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DocSection {
    /// As the document names it. Somebody else's words: see the module docs.
    pub title: String,
    /// 1 for a top-level entry -- a part, or a chapter of a book without
    /// parts -- and deeper from there.
    pub level: u8,
    /// The page it starts on.
    pub page: u32,
    /// The block it starts at.
    pub block: u32,
    /// The section it is inside, by index; `None` at the top level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u32>,
}

/// One file of an EPUB's reading order (a spine item), or the whole of a
/// Markdown, text or HTML document: what the reader loads at a time.
/// A PDF has none; pdf.js draws its pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DocChapter {
    pub first_block: u32,
    /// How many blocks it has.
    pub blocks: u32,
    /// Its path inside the file, for an EPUB: what an internal link names.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub href: String,
}

/// What a document says about itself: an EPUB's package metadata, a PDF's
/// information dictionary. Somebody else's words, every one.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DocMeta {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub authors: Vec<String>,
    /// BCP 47, as the file gives it.
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub publisher: String,
    /// As the file writes it: a year, a date, a timestamp.
    #[serde(default)]
    pub published: String,
    /// Identifiers with their scheme, lowercase: `isbn:9780141439518`,
    /// `doi:10.1038/nature12373`. A DOI found on a PDF's first page counts.
    #[serde(default)]
    pub identifiers: Vec<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub subjects: Vec<String>,
    /// An EPUB's cover picture, as a path inside the file; empty when it
    /// names none.
    #[serde(default)]
    pub cover: String,
}

/// A document, read. See the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocText {
    /// [`DOC_TEXT_VERSION`] when it was parsed.
    pub version: u32,
    pub format: DocFormat,
    /// How many pages: a PDF's own, else the made-up ones.
    pub pages: u32,
    #[serde(default)]
    pub meta: DocMeta,
    #[serde(default)]
    pub sections: Vec<DocSection>,
    #[serde(default)]
    pub chapters: Vec<DocChapter>,
    #[serde(default)]
    pub blocks: Vec<DocBlock>,
    /// What the parser could not do, in words for the person and the model:
    /// "Pages 3–40 have no text in them; they may be scanned pictures."
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// A stretch of a document as the assistant reads it: see
/// [`DocText::render`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    /// The blocks' text, a `[Page N]` line wherever a page starts and a
    /// heading's text after `#`s for its level.
    pub text: String,
    /// The blocks it covers.
    pub blocks: Range<usize>,
    /// The first and last page it covers.
    pub pages: (u32, u32),
    /// Whether the stretch asked for went on past what was rendered.
    pub truncated: bool,
    /// The block to carry on from when it was truncated.
    pub next_block: Option<u32>,
}

/// One place a search found in a document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DocHit {
    pub block: u32,
    pub page: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<u32>,
    /// The words around the match, cut from the block, on one line.
    pub snippet: String,
    pub score: f32,
}

impl DocText {
    /// An empty document of `format`, at the current version: what a parser
    /// starts from.
    pub fn new(format: DocFormat) -> Self {
        Self {
            version: DOC_TEXT_VERSION,
            format,
            pages: 0,
            meta: DocMeta::default(),
            sections: Vec::new(),
            chapters: Vec::new(),
            blocks: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Give every block a made-up page, about [`SYNTHETIC_PAGE_CHARS`] of
    /// text each, never splitting a block; a heading of level 1 or 2 starts
    /// a page of its own unless the page is still empty. Sets
    /// [`DocText::pages`]. For a document without pages of its own; a parser
    /// calls it once every block is in place.
    ///
    /// A block that would run the page over starts the next one rather than
    /// being cut, so a page is a little short more often than it is long; only
    /// a single block longer than a page makes one longer. That is what lets
    /// "page 47" name blocks the reader and the assistant both agree on.
    pub fn assign_synthetic_pages(&mut self) {
        let mut page = 1u32;
        let mut used = 0usize;
        for block in &mut self.blocks {
            let length = block.text.chars().count();
            let opens_a_part = block.kind == DocBlockKind::Heading && block.level <= 2;
            if used > 0 && (opens_a_part || used + length > SYNTHETIC_PAGE_CHARS) {
                page += 1;
                used = 0;
            }
            block.page = page;
            used += length;
        }
        self.pages = if self.blocks.is_empty() { 0 } else { page };
    }

    /// Make the table of contents from the headings, for a document that has
    /// none of its own: one [`DocSection`] per [`DocBlockKind::Heading`]
    /// block, at its level. Leaves sections that are already there alone.
    ///
    /// A heading with no words in it names nothing and is passed over. The
    /// sections still need [`DocText::link_sections`] to be settled.
    pub fn sections_from_headings(&mut self) {
        if !self.sections.is_empty() {
            return;
        }
        self.sections = self
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| block.kind == DocBlockKind::Heading)
            .filter_map(|(at, block)| {
                let title = one_line(&block.text);
                if title.is_empty() {
                    return None;
                }
                Some(DocSection {
                    title,
                    level: block.level.max(1),
                    page: block.page,
                    block: at as u32,
                    parent: None,
                })
            })
            .collect();
    }

    /// Settle the sections against the blocks once both are in place: sort
    /// them by block, set each one's page from its block and its `parent`
    /// from the levels, and set every block's [`DocBlock::section`] to the
    /// innermost section it falls in. Every parser calls it last.
    ///
    /// A section that names a block the document does not have is dropped: a
    /// contents page that points past the end points at nothing, and keeping
    /// it would hand every reader of [`DocText::section_blocks`] a range
    /// outside the document. Two entries for the same place with the same
    /// title -- an EPUB's navigation lists a chapter once under its part and
    /// once on its own -- are one section, the first.
    pub fn link_sections(&mut self) {
        let count = self.blocks.len();
        let mut sections = std::mem::take(&mut self.sections);
        sections.retain(|section| (section.block as usize) < count);
        // Stable, so entries for one block keep the order the document gave
        // them in: a part before its first chapter.
        sections.sort_by_key(|section| section.block);

        let mut kept: Vec<DocSection> = Vec::with_capacity(sections.len());
        for section in sections {
            let repeated = kept
                .iter()
                .rev()
                .take_while(|earlier| earlier.block == section.block)
                .any(|earlier| earlier.title == section.title);
            if !repeated {
                kept.push(section);
            }
        }

        // The sections still open at this point, outermost first: a new one
        // closes every open section as deep as itself or deeper, and is
        // inside whatever is left.
        let mut open: Vec<usize> = Vec::new();
        for at in 0..kept.len() {
            let level = kept[at].level.max(1);
            while open.last().is_some_and(|&top| kept[top].level >= level) {
                open.pop();
            }
            kept[at].level = level;
            kept[at].parent = open.last().map(|&parent| parent as u32);
            kept[at].page = self.blocks[kept[at].block as usize].page;
            open.push(at);
        }
        self.sections = kept;

        // The last section to have started by a block is the innermost one it
        // is in: the next of the same or a shallower level would have closed
        // the one before it.
        let mut started = 0usize;
        let mut current = None;
        for (at, block) in self.blocks.iter_mut().enumerate() {
            while started < self.sections.len() && self.sections[started].block as usize <= at {
                current = Some(started as u32);
                started += 1;
            }
            block.section = current;
        }
    }

    /// The blocks on pages `from` to `to`, inclusive. Empty when no block is
    /// on them.
    ///
    /// Blocks run in page order, so this is a pair of binary searches rather
    /// than a pass over a long book.
    pub fn blocks_on_pages(&self, from: u32, to: u32) -> Range<usize> {
        let start = self.blocks.partition_point(|block| block.page < from);
        let end = self.blocks.partition_point(|block| block.page <= to).max(start);
        start..end
    }

    /// The blocks of section `section` and every section under it: from where
    /// it starts to where the next section of the same or a shallower level
    /// does, or to the end of the document. Empty for a section that is not
    /// there.
    pub fn section_blocks(&self, section: usize) -> Range<usize> {
        let Some(this) = self.sections.get(section) else { return 0..0 };
        let count = self.blocks.len();
        let start = (this.block as usize).min(count);
        // A section that starts at the same block -- a PDF's outline names
        // pages, not places, so a page can begin three of them -- does not end
        // this one: it ends at the first one after it that starts later.
        let end = self.sections[section + 1..]
            .iter()
            .find(|later| later.level <= this.level && later.block > this.block)
            .map_or(count, |later| (later.block as usize).min(count))
            .max(start);
        start..end
    }

    /// The sections a block is in, outermost first.
    pub fn section_trail(&self, block: usize) -> Vec<&DocSection> {
        let mut trail = Vec::new();
        let mut at = self.blocks.get(block).and_then(|block| block.section);
        // A parser's bug must not become a loop: a trail is never longer than
        // the sections there are.
        while let Some(index) = at
            && trail.len() <= self.sections.len()
        {
            let Some(section) = self.sections.get(index as usize) else { break };
            trail.push(section);
            at = section.parent;
        }
        trail.reverse();
        trail
    }

    /// The chapter a block is in, by index into [`DocText::chapters`].
    pub fn chapter_of(&self, block: usize) -> Option<usize> {
        self.chapters.iter().position(|chapter| {
            let first = chapter.first_block as usize;
            (first..first + chapter.blocks as usize).contains(&block)
        })
    }

    /// `blocks` as the assistant reads them, cut short at about `max_chars`
    /// on a block boundary (never before the first block, however long).
    ///
    /// A `[Page N]` line stands wherever a page starts -- and at the top, so
    /// a stretch read from the middle of a page still says which page it is --
    /// then each block as plain text with a little Markdown to say what kind
    /// it is, a blank line between them. A first block longer than `max_chars`
    /// is cut at a character, with an ellipsis, rather than being refused:
    /// the tail of it cannot be reached by carrying on from
    /// [`Rendered::next_block`], which starts at the block after.
    pub fn render(&self, blocks: Range<usize>, max_chars: usize) -> Rendered {
        let end = blocks.end.min(self.blocks.len());
        let start = blocks.start.min(end);
        let mut text = String::new();
        let mut used = 0usize;
        let mut covered = start;
        let mut page = None;
        let mut truncated = false;
        let mut next_block = None;

        for (at, block) in self.blocks.iter().enumerate().take(end).skip(start) {
            let body = render_block(block);
            if body.is_empty() {
                // Nothing to say, but the stretch still covers it.
                covered = at + 1;
                continue;
            }
            let mut piece = String::new();
            if page != Some(block.page) {
                piece.push_str(&format!("[Page {}]\n\n", block.page));
            }
            piece.push_str(&body);
            let gap = if text.is_empty() { 0 } else { 2 };
            let size = piece.chars().count();

            if !text.is_empty() && used + gap + size > max_chars {
                truncated = true;
                next_block = Some(at as u32);
                break;
            }
            if text.is_empty() && size > max_chars {
                // The first block always goes in; this one is too long for the
                // whole allowance, so as much of it as fits does.
                let room = max_chars.saturating_sub(1);
                piece = piece.chars().take(room).collect::<String>() + "\u{2026}";
                truncated = true;
                if at + 1 < end {
                    next_block = Some(at as u32 + 1);
                }
                text.push_str(&piece);
                covered = at + 1;
                break;
            }
            if gap > 0 {
                text.push_str("\n\n");
            }
            text.push_str(&piece);
            used += gap + size;
            page = Some(block.page);
            covered = at + 1;
        }

        let pages = if covered > start {
            (self.blocks[start].page, self.blocks[covered - 1].page)
        } else {
            (0, 0)
        };
        Rendered { text, blocks: start..covered, pages, truncated, next_block }
    }

    /// Search this document's blocks: BM25 over their words, any case and
    /// accents aside, every word of `query` counting and a block with all of
    /// them ranked first. At most `limit` hits, best first.
    ///
    /// Words in double quotes are a phrase and match only side by side, in
    /// order; a heading's score is raised a little, since a heading that
    /// holds the words is what somebody looking for a chapter wants. A block
    /// is a paragraph or so, which is the right size of thing for BM25 to
    /// rank, and each hit's snippet is cut around its first match.
    ///
    /// Everything is worked out per call from the blocks, with nothing kept:
    /// one pass over even a very long book's words costs milliseconds next to
    /// the model call that asked.
    pub fn search(&self, query: &str, limit: usize) -> Vec<DocHit> {
        let query = Query::parse(query);
        if limit == 0 || query.terms.is_empty() {
            return Vec::new();
        }

        let mut counts = vec![0u32; query.vocab.len()];
        let mut stream: Vec<u32> = Vec::new();
        let mut per_term = vec![0u32; query.terms.len()];
        let mut found: Vec<Candidate> = Vec::new();
        let (mut blocks_with_words, mut words) = (0usize, 0usize);

        for (at, block) in self.blocks.iter().enumerate() {
            stream.clear();
            counts.fill(0);
            each_word(&block.text, |word, _, _| {
                let id = query.vocab.get(word).copied().unwrap_or(NO_WORD);
                if let Some(count) = counts.get_mut(id as usize) {
                    *count += 1;
                }
                stream.push(id);
            });
            if stream.is_empty() {
                continue;
            }
            blocks_with_words += 1;
            words += stream.len();

            for (slot, term) in per_term.iter_mut().zip(&query.terms) {
                *slot = match term {
                    Term::Word(id) => counts[*id as usize],
                    Term::Phrase(ids) if ids.iter().all(|id| counts[*id as usize] > 0) => {
                        occurrences(&stream, ids)
                    }
                    Term::Phrase(_) => 0,
                };
            }
            if per_term.iter().any(|&n| n > 0) {
                found.push(Candidate { block: at, words: stream.len(), hits: per_term.clone() });
            }
        }
        if found.is_empty() {
            return Vec::new();
        }

        // Robertson-Sparck Jones weights, floored at zero by the +1 inside the
        // logarithm so a word in most blocks still counts for a little: every
        // word of the query is meant to count.
        let total = blocks_with_words as f64;
        let average = (words as f64 / total).max(1.0);
        let idf: Vec<f64> = (0..query.terms.len())
            .map(|term| {
                let df = found.iter().filter(|c| c.hits[term] > 0).count() as f64;
                ((total - df + 0.5) / (df + 0.5) + 1.0).ln()
            })
            .collect();

        let mut ranked: Vec<(bool, f64, usize)> = found
            .iter()
            .map(|candidate| {
                let length = BM25_K1 * (1.0 - BM25_B + BM25_B * candidate.words as f64 / average);
                let mut score: f64 = candidate
                    .hits
                    .iter()
                    .zip(&idf)
                    .map(|(&tf, weight)| {
                        let tf = f64::from(tf);
                        weight * tf * (BM25_K1 + 1.0) / (tf + length)
                    })
                    .sum();
                if self.blocks[candidate.block].kind == DocBlockKind::Heading {
                    score *= HEADING_BOOST;
                }
                (candidate.hits.iter().all(|&n| n > 0), score, candidate.block)
            })
            .collect();
        ranked.sort_by(|a, b| {
            b.0.cmp(&a.0).then_with(|| b.1.total_cmp(&a.1)).then_with(|| a.2.cmp(&b.2))
        });
        ranked.truncate(limit);

        ranked
            .into_iter()
            .map(|(_, score, at)| {
                let block = &self.blocks[at];
                DocHit {
                    block: at as u32,
                    page: block.page,
                    section: block.section,
                    snippet: snippet(&block.text, &query),
                    score: score as f32,
                }
            })
            .collect()
    }

    /// The blocks on `page` that hold `quote` -- a passage somebody selected
    /// in the reader, matched with spacing, case and hyphenation aside. What
    /// turns a PDF selection, which the reader knows only as words, into
    /// blocks the service can read itself. `None` when it is not there.
    ///
    /// Both sides are lowercased, their runs of white space made one space,
    /// a word broken across lines by a hyphen joined and soft hyphens
    /// dropped, and the quote looked for in the page's blocks as if they were
    /// one text, so a selection that runs across a paragraph break still
    /// names both paragraphs. A text layer is untidier than that -- ligatures,
    /// curly quotes, a hyphen with a space after it -- so when that finds
    /// nothing the two are compared again by their letters and digits alone.
    ///
    /// The first place wins. A word or two that is on the page more than once
    /// cannot be told apart, and gets the first.
    pub fn find_quote(&self, page: u32, quote: &str) -> Option<Range<usize>> {
        let on_page = self.blocks_on_pages(page, page);
        if on_page.is_empty() {
            return None;
        }
        let forms: [fn(&str) -> String; 2] = [plain_form, letters_form];
        for (form, glue) in forms.into_iter().zip([" ", ""]) {
            let wanted = form(quote);
            if wanted.is_empty() {
                return None;
            }
            // The page as one text, remembering where each block begins.
            let mut text = String::new();
            let mut starts: Vec<(usize, usize)> = Vec::new();
            for at in on_page.clone() {
                let words = form(&self.blocks[at].text);
                if words.is_empty() {
                    continue;
                }
                if !text.is_empty() {
                    text.push_str(glue);
                }
                starts.push((text.len(), at));
                text.push_str(&words);
            }
            if let Some(found) = text.find(&wanted) {
                let block_at = |offset: usize| {
                    let index = starts.partition_point(|&(start, _)| start <= offset);
                    starts[index.saturating_sub(1)].1
                };
                let first = block_at(found);
                let last = block_at(found + wanted.len() - 1);
                return Some(first..last + 1);
            }
        }
        None
    }

    /// How many words it has.
    pub fn word_count(&self) -> usize {
        self.blocks.iter().map(|b| b.text.split_whitespace().count()).sum()
    }
}

// ---- reading a block, searching and matching --------------------------------

/// One block as the assistant reads it: its words, with the little Markdown
/// that says what kind of block it is. Empty for a block with nothing to say.
fn render_block(block: &DocBlock) -> String {
    let text = block.text.trim();
    match block.kind {
        DocBlockKind::Rule => "* * *".to_string(),
        DocBlockKind::Image if text.is_empty() => "[Picture]".to_string(),
        DocBlockKind::Image => format!("[Picture: {}]", one_line(text)),
        _ if text.is_empty() => String::new(),
        DocBlockKind::Heading => {
            format!("{} {}", "#".repeat(usize::from(block.level.clamp(1, 6))), one_line(text))
        }
        DocBlockKind::Paragraph => text.to_string(),
        DocBlockKind::Quote => text
            .lines()
            .map(|line| if line.trim().is_empty() { ">".to_string() } else { format!("> {line}") })
            .collect::<Vec<_>>()
            .join("\n"),
        // A list item's second line goes under its first, not back at the
        // margin where it would read as a paragraph.
        DocBlockKind::ListItem => format!("- {}", text.replace('\n', "\n  ")),
        DocBlockKind::Code => {
            // A fence longer than any run of backticks inside, so the code
            // cannot end its own fence.
            let longest = block.text.split(|c: char| c != '`').map(str::len).max().unwrap_or(0);
            let fence = "`".repeat((longest + 1).max(3));
            format!("{fence}\n{}\n{fence}", block.text.trim_matches('\n').trim_end())
        }
    }
}

/// What a word that is not in the query is, in a block's stream of words.
const NO_WORD: u32 = u32::MAX;

/// One thing a search looks for.
#[derive(Debug, Clone, PartialEq)]
enum Term {
    Word(u32),
    /// Words that must come side by side, in this order.
    Phrase(Vec<u32>),
}

/// A search query, read into its terms.
struct Query {
    /// Every word any term names, numbered; a block's words are looked up
    /// here and anything not in it is [`NO_WORD`].
    vocab: HashMap<String, u32>,
    terms: Vec<Term>,
}

impl Query {
    /// Words on their own are terms; what is between double quotes is a
    /// phrase, or a term if it is only one word. A quote left open runs to the
    /// end, as a person typing one means it.
    fn parse(text: &str) -> Query {
        let mut query = Query { vocab: HashMap::new(), terms: Vec::new() };
        for (at, part) in text.split(['"', '\u{201c}', '\u{201d}']).enumerate() {
            let mut words: Vec<u32> = Vec::new();
            each_word(part, |word, _, _| {
                let next = query.vocab.len() as u32;
                words.push(*query.vocab.entry(word.to_string()).or_insert(next));
            });
            let quoted = at % 2 == 1;
            let terms = if quoted && words.len() > 1 {
                vec![Term::Phrase(words)]
            } else {
                words.into_iter().map(Term::Word).collect()
            };
            for term in terms {
                if !query.terms.contains(&term) {
                    query.terms.push(term);
                }
            }
        }
        // "Who is Mortimer" is a question about Mortimer: the words that only
        // make it a question are left out when anything else is left, so a
        // paragraph saying "who is this Barrymore" does not outrank one about
        // the man asked after. Inside quotes they stay -- a phrase is meant
        // whole -- and a query of nothing else keeps them all.
        let said = |term: &Term| match term {
            Term::Word(id) => {
                query.vocab.iter().find(|(_, v)| *v == id).is_none_or(|(w, _)| !is_filler(w))
            }
            Term::Phrase(_) => true,
        };
        if query.terms.iter().any(said) {
            let kept: Vec<Term> = query.terms.iter().filter(|t| said(t)).cloned().collect();
            query.terms = kept;
        }
        query.terms.truncate(MAX_QUERY_TERMS);
        query
    }
}

/// English words that carry a question's shape rather than what it is about.
/// Lower case, folded as [`each_word`] folds a block's words.
fn is_filler(word: &str) -> bool {
    matches!(
        word,
        "a" | "an"
            | "the"
            | "is"
            | "are"
            | "was"
            | "were"
            | "be"
            | "who"
            | "whom"
            | "what"
            | "which"
            | "where"
            | "when"
            | "why"
            | "how"
            | "does"
            | "do"
            | "did"
            | "of"
            | "in"
            | "on"
            | "at"
            | "to"
            | "for"
            | "and"
            | "or"
            | "this"
            | "that"
            | "it"
            | "about"
            | "tell"
            | "me"
    )
}

/// A block that has at least one of the terms.
struct Candidate {
    block: usize,
    /// How many words it has.
    words: usize,
    /// For each of the query's terms, how many times it is in the block.
    hits: Vec<u32>,
}

/// How many times `phrase` is in `stream`, side by side.
fn occurrences(stream: &[u32], phrase: &[u32]) -> u32 {
    stream.windows(phrase.len()).filter(|window| *window == phrase).count() as u32
}

/// Whether `c` is part of how a word is written rather than of the word: a
/// soft hyphen, a combining accent, a zero-width character. Neither a letter
/// nor a break between words.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{ad}' | '\u{300}'..='\u{36f}' | '\u{200b}'..='\u{200d}' | '\u{2060}' | '\u{feff}'
    )
}

/// Whether a hyphen that `rest` follows ends a line in the middle of a word:
/// a line break, then more of it.
fn hyphen_breaks_a_word(rest: &std::iter::Peekable<std::str::CharIndices<'_>>) -> bool {
    let mut ahead = rest.clone();
    let mut broken = false;
    while let Some(&(_, c)) = ahead.peek() {
        if !c.is_whitespace() {
            break;
        }
        broken |= c == '\n' || c == '\r';
        ahead.next();
    }
    broken && ahead.peek().is_some_and(|&(_, c)| c.is_alphanumeric())
}

/// Add `c` to `out` lowercased, and as a plain letter if it is an accented
/// Latin one -- or one of the ligatures a PDF's text layer is full of.
///
/// A small table, not Unicode's decomposition, which the standard library does
/// not carry: it covers the Latin letters books are written in, and a word in
/// any other script is compared as it is.
fn fold_into(c: char, out: &mut String) {
    for c in c.to_lowercase() {
        match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'ǎ' => out.push('a'),
            'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => out.push('c'),
            'ď' | 'đ' | 'ð' => out.push('d'),
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => out.push('e'),
            'ĝ' | 'ğ' | 'ġ' | 'ģ' => out.push('g'),
            'ĥ' | 'ħ' => out.push('h'),
            'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => out.push('i'),
            'ĵ' => out.push('j'),
            'ķ' => out.push('k'),
            'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => out.push('l'),
            'ñ' | 'ń' | 'ņ' | 'ň' | 'ŉ' => out.push('n'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => out.push('o'),
            'ŕ' | 'ŗ' | 'ř' => out.push('r'),
            'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => out.push('s'),
            'ţ' | 'ť' | 'ŧ' | 'ț' => out.push('t'),
            'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => out.push('u'),
            'ŵ' => out.push('w'),
            'ý' | 'ÿ' | 'ŷ' => out.push('y'),
            'ź' | 'ż' | 'ž' => out.push('z'),
            'ß' => out.push_str("ss"),
            'æ' => out.push_str("ae"),
            'œ' => out.push_str("oe"),
            'þ' => out.push_str("th"),
            '\u{fb00}' => out.push_str("ff"),
            '\u{fb01}' => out.push_str("fi"),
            '\u{fb02}' => out.push_str("fl"),
            '\u{fb03}' => out.push_str("ffi"),
            '\u{fb04}' => out.push_str("ffl"),
            '\u{fb05}' | '\u{fb06}' => out.push_str("st"),
            // What lowercasing leaves of a dotted capital I.
            '\u{300}'..='\u{36f}' => {}
            other => out.push(other),
        }
    }
}

/// Call `found` with each word of `text` -- lowercased and unaccented, with
/// where it starts and ends in `text` -- in order.
///
/// A word is a run of letters and digits. A hyphen at the end of a line that
/// carries on in the next one joins the two halves, as the typesetter meant
/// it, and soft hyphens and zero-width characters are not there at all. The
/// one routine behind searching, so a query and a block are broken into words
/// the same way.
fn each_word(text: &str, mut found: impl FnMut(&str, usize, usize)) {
    let mut word = String::new();
    let (mut start, mut end) = (0, 0);
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if is_invisible(c) {
            continue;
        }
        if c.is_alphanumeric() {
            if word.is_empty() {
                start = at;
            }
            fold_into(c, &mut word);
            end = at + c.len_utf8();
            continue;
        }
        if (c == '-' || c == '\u{2010}') && !word.is_empty() && hyphen_breaks_a_word(&chars) {
            while chars.next_if(|&(_, next)| next.is_whitespace()).is_some() {}
            continue;
        }
        if !word.is_empty() {
            found(&word, start, end);
            word.clear();
        }
    }
    if !word.is_empty() {
        found(&word, start, end);
    }
}

/// The words of `text` around its first match for `query`, on one line, about
/// [`SNIPPET_CHARS`] of them, cut at spaces where there is one near and marked
/// with an ellipsis where it was cut.
fn snippet(text: &str, query: &Query) -> String {
    let mut words: Vec<(u32, usize)> = Vec::new();
    each_word(text, |word, start, _| {
        words.push((query.vocab.get(word).copied().unwrap_or(NO_WORD), start));
    });

    // The earliest of any single word asked for and any phrase asked for.
    let mut first = None::<usize>;
    for term in &query.terms {
        let at = match term {
            Term::Word(id) => words.iter().find(|(word, _)| word == id).map(|&(_, start)| start),
            Term::Phrase(ids) => words
                .windows(ids.len())
                .find(|window| window.iter().map(|&(word, _)| word).eq(ids.iter().copied()))
                .map(|window| window[0].1),
        };
        first = match (first, at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
    let at = first.unwrap_or(0);

    // A third of the room before the match, the rest after.
    let mut from = text[..at]
        .char_indices()
        .rev()
        .take(SNIPPET_CHARS / 3)
        .last()
        .map_or(at, |(index, _)| index);
    let mut to = text[from..]
        .char_indices()
        .nth(SNIPPET_CHARS)
        .map_or(text.len(), |(index, _)| from + index);
    // Not in the middle of a word, when a space is near enough to cut at.
    const NEAR: usize = 24;
    if from > 0
        && !text[..from].ends_with(char::is_whitespace)
        && let Some(space) = text[from..at].find(char::is_whitespace)
        && space < NEAR
    {
        from += space;
    }
    if to < text.len()
        && !text[to..].starts_with(char::is_whitespace)
        && let Some(space) = text[at..to].rfind(char::is_whitespace)
        && to - (at + space) < NEAR
    {
        to = at + space;
    }

    let body = one_line(&text[from..to]);
    let before = if text[..from].trim().is_empty() { "" } else { "\u{2026}" };
    let after = if text[to..].trim().is_empty() { "" } else { "\u{2026}" };
    format!("{before}{body}{after}")
}

/// `text` as a quote is compared with it: lowercase, runs of white space made
/// one space, a word broken across lines by a hyphen joined up, soft hyphens
/// gone, and nothing at either end.
fn plain_form(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    let mut chars = text.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        if c == '\u{ad}' {
            continue;
        }
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if (c == '-' || c == '\u{2010}')
            && !space
            && out.chars().next_back().is_some_and(char::is_alphanumeric)
            && hyphen_breaks_a_word(&chars)
        {
            while chars.next_if(|&(_, next)| next.is_whitespace()).is_some() {}
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// `text` as its letters and digits alone, lowercased and unaccented: what two
/// copies of one passage still share when a text layer has spaced, hyphenated
/// and punctuated it differently.
fn letters_form(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().filter(|&c| !is_invisible(c) && c.is_alphanumeric()) {
        fold_into(c, &mut out);
    }
    out
}

// ---- what the interface reads ---------------------------------------------

/// Where somebody is in a document, kept in the file's properties
/// (`reader.page`, `reader.block`) so it opens where they left it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ReaderPlace {
    pub page: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<u32>,
}

/// The property keys a [`ReaderPlace`] is kept under.
pub const READER_PAGE_KEY: &str = "reader.page";
pub const READER_BLOCK_KEY: &str = "reader.block";

/// What the reader opens on: everything about a document but its blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocOutline {
    pub file: FileId,
    pub format: DocFormat,
    pub pages: u32,
    pub meta: DocMeta,
    pub sections: Vec<DocSection>,
    pub chapters: Vec<DocChapter>,
    /// How many blocks it has.
    pub blocks: u32,
    pub words: u64,
    pub warnings: Vec<String>,
    /// Where they left off, if they have read it here before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<ReaderPlace>,
    /// Whether it is in the search index now.
    pub indexed: bool,
}

/// One chapter's blocks, as the reader draws them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocChapterView {
    pub file: FileId,
    pub chapter: u32,
    pub first_block: u32,
    pub blocks: Vec<DocBlockView>,
}

/// One block as the reader draws it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocBlockView {
    pub index: u32,
    pub kind: DocBlockKind,
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub level: u8,
    pub page: u32,
    /// [`DocBlock::html`]; for a block with none, its text escaped.
    pub html: String,
    /// A picture as a `data:` address, read out of the file; empty for
    /// anything else, and for a picture too large or of a type not drawn.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub src: String,
}

// ---- the service, as the core sees it -------------------------------------

/// What the assistant's document tools need from the service: a document's
/// text, read now or from the cache, and the search index over every
/// indexed document.
///
/// Handed down in [`crate::agent::tools::ToolContext::documents`] the way
/// mail's index is, because the core has no cache directory, no index and no
/// background task of its own to ask. `jatayu_service::documents` is the
/// one implementation.
pub trait Documents: Send + Sync {
    /// The document's text. Read from the cache when it holds this version
    /// of the file, else parsed now -- which can take seconds for a long PDF
    /// -- and cached. Blocking. Refuses a file that is not a document, one
    /// that is missing or in the trash, and one Health keeps.
    fn text(&self, file: &FileEntry) -> Result<Arc<DocText>>;

    /// Search every indexed document. Hits for files that are gone, trashed
    /// or no longer indexed are left out.
    fn search(&self, query: &DocSearch) -> Result<DocSearchPage>;

    /// How the index stands.
    fn index_status(&self) -> DocIndexStatus;
}

/// A search over every indexed document.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocSearch {
    pub text: String,
    /// Only these files, when not empty.
    #[serde(default)]
    pub files: Vec<FileId>,
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

/// One passage the index found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DocSearchHit {
    pub file: FileId,
    /// The first block of the passage.
    pub block: u32,
    pub page: u32,
    /// The innermost section it is in, as the document names it.
    #[serde(default)]
    pub section: String,
    pub snippet: String,
    pub score: f32,
}

/// A page of [`DocSearchHit`]s.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DocSearchPage {
    pub hits: Vec<DocSearchHit>,
    /// How many passages match in all.
    pub total: usize,
    /// Whether indexing is still under way, so more may match later.
    pub indexing: bool,
    /// The offset to ask for the next page from. Not `offset + hits.len()`:
    /// hits for files that are gone or no longer indexed are left out after
    /// the index answers, so a page can use up more of the index's answers
    /// than it returns.
    #[serde(default)]
    pub next_offset: usize,
}

/// How the search index stands, for Settings → Files and the tools.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DocIndexStatus {
    /// [`crate::files::FilesSettings::index_documents`].
    pub enabled: bool,
    /// Documents in the index now.
    pub indexed: u64,
    /// Documents waiting to be read into it.
    pub pending: u64,
    /// Documents that could not be read, with why.
    pub failed: Vec<DocFailure>,
    /// A pass is under way.
    pub running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pass: Option<Timestamp>,
}

/// A document the index could not read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DocFailure {
    pub file: FileId,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(kind: DocBlockKind, level: u8, text: &str) -> DocBlock {
        DocBlock { kind, level, text: text.to_string(), ..Default::default() }
    }

    fn heading(level: u8, text: &str) -> DocBlock {
        block(DocBlockKind::Heading, level, text)
    }

    fn para(text: &str) -> DocBlock {
        block(DocBlockKind::Paragraph, 0, text)
    }

    fn doc(blocks: Vec<DocBlock>) -> DocText {
        let mut doc = DocText::new(DocFormat::Epub);
        doc.blocks = blocks;
        doc
    }

    /// `doc`, with its blocks on the pages given, in order.
    fn on_pages(mut doc: DocText, pages: &[u32]) -> DocText {
        assert_eq!(doc.blocks.len(), pages.len());
        for (block, page) in doc.blocks.iter_mut().zip(pages) {
            block.page = *page;
        }
        doc.pages = pages.iter().copied().max().unwrap_or(0);
        doc
    }

    fn section(title: &str, level: u8, block: u32) -> DocSection {
        DocSection { title: title.to_string(), level, page: 0, block, parent: None }
    }

    /// A book with two parts of two chapters and one:
    ///
    /// ```text
    /// 0 preface          5 y
    /// 1 # Part One       6 # Part Two
    /// 2 ## Chapter 1     7 z
    /// 3 x
    /// 4 ## Chapter 2
    /// ```
    fn book() -> DocText {
        let mut book = doc(vec![
            para("preface"),
            heading(1, "Part One"),
            heading(2, "Chapter 1"),
            para("x"),
            heading(2, "Chapter 2"),
            para("y"),
            heading(1, "Part Two"),
            para("z"),
        ]);
        book.assign_synthetic_pages();
        book.sections_from_headings();
        book.link_sections();
        book
    }

    fn titles(sections: &[&DocSection]) -> Vec<String> {
        sections.iter().map(|section| section.title.clone()).collect()
    }

    // ---- pages --------------------------------------------------------------

    #[test]
    fn a_short_document_is_one_page_and_an_empty_one_has_none() {
        let mut short = doc(vec![para("One."), para("Two.")]);
        short.assign_synthetic_pages();
        assert_eq!(short.pages, 1);
        assert!(short.blocks.iter().all(|block| block.page == 1));

        let mut empty = doc(Vec::new());
        empty.assign_synthetic_pages();
        assert_eq!(empty.pages, 0);
    }

    #[test]
    fn a_page_fills_to_about_a_page_and_never_splits_a_block() {
        let chunk = "x".repeat(700);
        let mut text = doc(vec![para(&chunk); 5]);
        text.assign_synthetic_pages();
        // 700 + 700 fits; a third would make 2,100, so it starts the next.
        let pages: Vec<u32> = text.blocks.iter().map(|block| block.page).collect();
        assert_eq!(pages, [1, 1, 2, 2, 3]);
        assert_eq!(text.pages, 3);
    }

    #[test]
    fn a_block_longer_than_a_page_has_one_to_itself() {
        let mut text = doc(vec![para("short"), para(&"y".repeat(5_000)), para("after")]);
        text.assign_synthetic_pages();
        let pages: Vec<u32> = text.blocks.iter().map(|block| block.page).collect();
        assert_eq!(pages, [1, 2, 3]);
    }

    #[test]
    fn a_big_heading_starts_a_page_unless_the_page_is_empty() {
        let mut text = doc(vec![
            heading(1, "Title"),
            para("a"),
            heading(3, "minor"),
            para("b"),
            heading(2, "Chapter"),
            para("c"),
            heading(1, "Next"),
        ]);
        text.assign_synthetic_pages();
        let pages: Vec<u32> = text.blocks.iter().map(|block| block.page).collect();
        // The first heading is at the top of a page already; the third, of
        // level 3, is not big enough to start one.
        assert_eq!(pages, [1, 1, 1, 1, 2, 2, 3]);
        assert_eq!(text.pages, 3);
    }

    #[test]
    fn pictures_do_not_make_a_page_non_empty() {
        let mut text = doc(vec![block(DocBlockKind::Image, 0, ""), heading(1, "Title"), para("a")]);
        text.assign_synthetic_pages();
        assert_eq!(text.pages, 1, "a heading after a picture is still at the top of page 1");
    }

    // ---- sections -----------------------------------------------------------

    #[test]
    fn sections_come_from_the_headings_when_there_are_none() {
        let mut text = doc(vec![
            heading(1, "Part One"),
            para("a"),
            heading(2, "  Chapter\n  1 "),
            heading(2, "   "),
            heading(0, "Odd"),
        ]);
        text.assign_synthetic_pages();
        text.sections_from_headings();
        let found: Vec<(&str, u8, u32)> = text
            .sections
            .iter()
            .map(|section| (section.title.as_str(), section.level, section.block))
            .collect();
        // A heading with no words names nothing; one with level 0 is level 1;
        // white space inside a title is one space.
        assert_eq!(found, [("Part One", 1, 0), ("Chapter 1", 2, 2), ("Odd", 1, 4)]);
        assert_eq!(text.sections[0].page, 1);
        assert_eq!(text.sections[1].page, text.blocks[2].page);
    }

    #[test]
    fn a_contents_the_document_brought_is_left_alone() {
        let mut text = doc(vec![heading(1, "From the headings"), para("a")]);
        text.sections = vec![section("From the contents", 1, 1)];
        text.sections_from_headings();
        assert_eq!(text.sections.len(), 1);
        assert_eq!(text.sections[0].title, "From the contents");
    }

    #[test]
    fn linking_gives_each_section_its_page_parent_and_blocks() {
        let book = book();
        assert_eq!(book.sections.len(), 4);
        let parents: Vec<Option<u32>> = book.sections.iter().map(|s| s.parent).collect();
        assert_eq!(parents, [None, Some(0), Some(0), None]);
        // Each part and chapter heading starts a made-up page of its own.
        let pages: Vec<u32> = book.sections.iter().map(|s| s.page).collect();
        assert_eq!(pages, [2, 3, 4, 5]);
        for section in &book.sections {
            assert_eq!(section.page, book.blocks[section.block as usize].page);
        }
        let of: Vec<Option<u32>> = book.blocks.iter().map(|b| b.section).collect();
        assert_eq!(
            of,
            [None, Some(0), Some(1), Some(1), Some(2), Some(2), Some(3), Some(3)],
            "nothing is in a section before the first one starts"
        );
    }

    #[test]
    fn linking_sorts_drops_what_points_nowhere_and_merges_repeats() {
        let mut text = doc(vec![para("a"), para("b"), para("c")]);
        text.assign_synthetic_pages();
        text.sections = vec![
            section("Two", 1, 2),
            section("One", 1, 0),
            // The same place and title again, deeper: the navigation listing a
            // chapter a second time.
            section("One", 2, 0),
            section("Gone", 1, 9),
            section("Two", 1, 2),
            section("Sub", 2, 2),
        ];
        text.link_sections();

        let found: Vec<(&str, u8, u32, Option<u32>)> =
            text.sections.iter().map(|s| (s.title.as_str(), s.level, s.block, s.parent)).collect();
        assert_eq!(
            found,
            [("One", 1, 0, None), ("Two", 1, 2, None), ("Sub", 2, 2, Some(1))],
            "sorted by block, the dead one dropped, the repeats one each"
        );
        let of: Vec<Option<u32>> = text.blocks.iter().map(|b| b.section).collect();
        assert_eq!(of, [Some(0), Some(0), Some(2)], "the deeper one starting at a block wins it");
    }

    #[test]
    fn a_section_that_skips_a_level_is_inside_the_nearest_shallower_one() {
        let mut text =
            doc(vec![heading(1, "A"), heading(3, "B"), heading(2, "C"), heading(3, "D")]);
        text.assign_synthetic_pages();
        text.sections_from_headings();
        text.link_sections();
        let parents: Vec<Option<u32>> = text.sections.iter().map(|s| s.parent).collect();
        assert_eq!(parents, [None, Some(0), Some(0), Some(2)]);
    }

    #[test]
    fn linking_again_changes_nothing_and_without_sections_clears_the_blocks() {
        let mut once = book();
        let twice = {
            let mut again = once.clone();
            again.link_sections();
            again
        };
        assert_eq!(once, twice);

        once.sections.clear();
        once.link_sections();
        assert!(once.blocks.iter().all(|block| block.section.is_none()));
    }

    #[test]
    fn a_range_of_pages_is_the_blocks_on_them() {
        let text = on_pages(
            doc(vec![para("a"), para("b"), para("c"), para("d"), para("e"), para("f"), para("g")]),
            &[1, 1, 2, 2, 2, 4, 4],
        );
        assert_eq!(text.blocks_on_pages(2, 2), 2..5);
        assert_eq!(text.blocks_on_pages(1, 4), 0..7);
        assert_eq!(text.blocks_on_pages(0, 1), 0..2);
        assert_eq!(text.blocks_on_pages(4, 9), 5..7);
        assert!(text.blocks_on_pages(3, 3).is_empty(), "no block is on page 3");
        assert!(text.blocks_on_pages(5, 9).is_empty());
        assert!(text.blocks_on_pages(4, 2).is_empty(), "a range that runs backwards is empty");
    }

    #[test]
    fn a_section_runs_to_the_next_of_its_level_or_a_shallower_one() {
        let book = book();
        assert_eq!(book.section_blocks(0), 1..6, "a part holds its chapters");
        assert_eq!(book.section_blocks(1), 2..4);
        assert_eq!(book.section_blocks(2), 4..6, "the last chapter ends where the next part does");
        assert_eq!(book.section_blocks(3), 6..8, "and the last section ends with the document");
        assert_eq!(book.section_blocks(9), 0..0, "a section that is not there has no blocks");
    }

    #[test]
    fn a_blocks_trail_runs_from_the_outermost_section_in() {
        let book = book();
        assert_eq!(titles(&book.section_trail(3)), ["Part One", "Chapter 1"]);
        assert_eq!(titles(&book.section_trail(6)), ["Part Two"]);
        assert!(book.section_trail(0).is_empty(), "the preface is in no section");
        assert!(book.section_trail(99).is_empty());
    }

    #[test]
    fn a_trail_cannot_loop_whatever_the_parser_wrote() {
        let mut text = doc(vec![para("a")]);
        text.sections = vec![DocSection { parent: Some(0), ..section("Self", 1, 0) }];
        text.blocks[0].section = Some(0);
        assert_eq!(text.section_trail(0).len(), 2, "cut off, not endless");
    }

    #[test]
    fn a_block_belongs_to_the_chapter_that_holds_it() {
        let mut text = doc(vec![para("a"); 8]);
        text.chapters = vec![
            DocChapter { first_block: 0, blocks: 3, href: String::new() },
            DocChapter { first_block: 3, blocks: 0, href: String::new() },
            DocChapter { first_block: 3, blocks: 5, href: String::new() },
        ];
        assert_eq!(text.chapter_of(0), Some(0));
        assert_eq!(text.chapter_of(2), Some(0));
        assert_eq!(text.chapter_of(3), Some(2), "an empty chapter holds nothing");
        assert_eq!(text.chapter_of(7), Some(2));
        assert_eq!(text.chapter_of(8), None);
        assert_eq!(doc(vec![para("a")]).chapter_of(0), None, "no chapters, no answer");
    }

    // ---- render -------------------------------------------------------------

    fn mixed() -> DocText {
        on_pages(
            doc(vec![
                heading(1, "Moby"),
                para("Call me Ishmael."),
                block(DocBlockKind::ListItem, 0, "one"),
                block(DocBlockKind::ListItem, 0, "two\nlines"),
                block(DocBlockKind::Quote, 0, "To be\nor not"),
                block(DocBlockKind::Code, 0, "let x = 1;"),
                block(DocBlockKind::Image, 0, "A  whale"),
                block(DocBlockKind::Rule, 0, ""),
                para("Last."),
            ]),
            &[1, 1, 1, 1, 1, 1, 2, 2, 2],
        )
    }

    #[test]
    fn rendering_marks_pages_and_says_what_each_block_is() {
        let text = mixed();
        let rendered = text.render(0..9, 100_000);
        let expected = [
            "[Page 1]",
            "# Moby",
            "Call me Ishmael.",
            "- one",
            "- two\n  lines",
            "> To be\n> or not",
            "```\nlet x = 1;\n```",
            "[Page 2]",
            "[Picture: A whale]",
            "* * *",
            "Last.",
        ]
        .join("\n\n");
        assert_eq!(rendered.text, expected);
        assert_eq!(rendered.blocks, 0..9);
        assert_eq!(rendered.pages, (1, 2));
        assert!(!rendered.truncated);
        assert_eq!(rendered.next_block, None);
    }

    #[test]
    fn a_stretch_from_the_middle_of_a_page_still_says_which_page() {
        let rendered = mixed().render(2..4, 100_000);
        assert_eq!(rendered.text, "[Page 1]\n\n- one\n\n- two\n  lines");
        assert_eq!(rendered.blocks, 2..4);
        assert_eq!(rendered.pages, (1, 1));
    }

    #[test]
    fn rendering_stops_on_a_block_boundary_and_says_where_to_carry_on() {
        let text = mixed();
        // "[Page 1]\n\n# Moby" is 16 characters; the next block makes 34.
        let cut = text.render(0..9, 33);
        assert_eq!(cut.text, "[Page 1]\n\n# Moby");
        assert_eq!(cut.blocks, 0..1);
        assert!(cut.truncated);
        assert_eq!(cut.next_block, Some(1));

        let two = text.render(0..9, 34);
        assert_eq!(two.text, "[Page 1]\n\n# Moby\n\nCall me Ishmael.");
        assert_eq!(two.blocks, 0..2);
        assert_eq!(two.next_block, Some(2));

        // And carrying on from there reads the rest, with its page marker.
        let rest = text.render(7..9, 34);
        assert_eq!(rest.text, "[Page 2]\n\n* * *\n\nLast.");
        assert!(!rest.truncated);
    }

    #[test]
    fn the_first_block_is_always_in_and_cut_if_it_is_too_long() {
        let text = on_pages(doc(vec![para(&"a".repeat(100)), para("b")]), &[1, 1]);
        let cut = text.render(0..2, 20);
        // 19 characters, then the ellipsis.
        assert_eq!(cut.text, format!("[Page 1]\n\n{}\u{2026}", "a".repeat(9)));
        assert_eq!(cut.text.chars().count(), 20);
        assert!(cut.truncated);
        assert_eq!(cut.blocks, 0..1);
        assert_eq!(cut.next_block, Some(1), "carrying on starts at the block after");

        let alone = text.render(0..1, 20);
        assert!(alone.truncated);
        assert_eq!(alone.next_block, None, "nothing after it to carry on to");
    }

    #[test]
    fn an_empty_or_out_of_range_stretch_renders_nothing() {
        let text = mixed();
        for range in [5..5, 50..60, Range { start: 7, end: 3 }] {
            let rendered = text.render(range, 1_000);
            assert_eq!(rendered.text, "");
            assert_eq!(rendered.pages, (0, 0));
            assert!(!rendered.truncated && rendered.next_block.is_none());
            assert!(rendered.blocks.is_empty());
        }
        let clamped = text.render(7..100, 1_000);
        assert_eq!(clamped.blocks, 7..9, "a range past the end is cut to the end");
    }

    #[test]
    fn blocks_with_nothing_in_them_are_covered_but_not_written() {
        let text = on_pages(doc(vec![para(""), para("real"), heading(2, "  ")]), &[1, 1, 1]);
        let rendered = text.render(0..3, 1_000);
        assert_eq!(rendered.text, "[Page 1]\n\nreal");
        assert_eq!(rendered.blocks, 0..3);
    }

    #[test]
    fn code_cannot_end_its_own_fence_and_headings_stop_at_six() {
        let text = on_pages(
            doc(vec![
                block(DocBlockKind::Code, 0, "a ``` b"),
                heading(9, "Deep"),
                heading(3, "Mid"),
            ]),
            &[1, 1, 1],
        );
        let rendered = text.render(0..3, 1_000);
        assert_eq!(rendered.text, "[Page 1]\n\n````\na ``` b\n````\n\n###### Deep\n\n### Mid");
    }

    // ---- search -------------------------------------------------------------

    fn ranked(text: &DocText, query: &str) -> Vec<u32> {
        text.search(query, 50).iter().map(|hit| hit.block).collect()
    }

    fn library() -> DocText {
        doc(vec![
            heading(1, "Mortimer Manor"),
            para("Lord Mortimer lived at the manor with his three dogs."),
            para("The weather was fine and nobody spoke of the war."),
            para("Mortimer rode out at dawn. Mortimer returned at dusk."),
            para("\u{c9}lodie arrived from Z\u{fc}rich with a \u{fb01}ne caf\u{e9} au lait."),
            para("The dogs barked at the gardener."),
        ])
    }

    #[test]
    fn a_search_finds_the_blocks_with_the_word_best_first() {
        let hits = library().search("mortimer", 10);
        let blocks: Vec<u32> = hits.iter().map(|hit| hit.block).collect();
        // The heading says nothing else, and is raised a little besides; the
        // paragraph with the name twice beats the one with it once.
        assert_eq!(blocks, [0, 3, 1]);
        assert!(hits.windows(2).all(|pair| pair[0].score >= pair[1].score));
    }

    #[test]
    fn case_and_accents_do_not_matter_either_way() {
        let text = library();
        assert_eq!(ranked(&text, "MORTIMER"), [0, 3, 1]);
        assert_eq!(ranked(&text, "cafe"), [4], "caf\u{e9} is found by cafe");
        assert_eq!(ranked(&text, "CAF\u{c9}"), [4], "and cafe by CAF\u{c9}");
        assert_eq!(ranked(&text, "zurich"), [4]);
        assert_eq!(ranked(&text, "elodie"), [4]);
        assert_eq!(ranked(&text, "fine"), [2, 4], "a ligature is its letters");
    }

    #[test]
    fn a_block_with_every_word_comes_before_one_with_a_better_score_and_fewer() {
        let mut blocks = vec![para("rare rare rare")];
        blocks.push(para(&format!("rare common {}", "word ".repeat(40))));
        blocks.extend((0..4).map(|_| para("common filler text")));
        let text = doc(blocks);
        let found = ranked(&text, "rare common");
        assert_eq!(found[0], 1, "the long block with both words leads: {found:?}");
        assert_eq!(found.len(), 6);
        // Searching for the one word has the short block with it three times
        // ahead, as the plain score says.
        assert_eq!(ranked(&text, "rare")[0], 0);
    }

    #[test]
    fn a_question_is_searched_for_what_it_asks_about() {
        let text = doc(vec![
            para("By the way, who is this Barrymore, anyhow?"),
            para("Dr. Mortimer looked at us gravely."),
            para("It is Mortimer who knows the moor."),
        ]);
        // "who" and "is" are left out, so the paragraph that only shares them
        // is not found, and the two about Mortimer are.
        let found = ranked(&text, "who is Mortimer");
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(!found.contains(&0));
        // A query of nothing but such words keeps them, and a phrase keeps
        // them whole.
        let mut only_fillers = ranked(&text, "who is");
        only_fillers.sort_unstable();
        assert_eq!(only_fillers, [0, 2]);
        assert_eq!(ranked(&text, "\"is mortimer who\""), [2]);
    }

    #[test]
    fn every_word_of_the_query_counts_towards_the_score() {
        let text = library();
        let found = ranked(&text, "who is mortimer");
        // None of "who" or "is" is in the document; the name still finds it.
        assert_eq!(found, [0, 3, 1]);
        // "dogs" as well as the name: only the paragraph with both leads, the
        // others follow.
        let both = ranked(&text, "mortimer dogs");
        assert_eq!(both[0], 1);
        assert_eq!(both.len(), 4);
    }

    #[test]
    fn a_quoted_phrase_matches_only_side_by_side() {
        let text = doc(vec![
            para("the old man and the sea"),
            para("the sea and the old man"),
            para("an old sea man"),
        ]);
        assert_eq!(ranked(&text, "\"old man\""), [0, 1]);
        let loose = ranked(&text, "old man");
        assert_eq!(loose.len(), 3, "unquoted, the words count wherever they are");
        // A phrase and a word: the blocks with both first, the one with the
        // word alone after.
        let mixed = ranked(&text, "\"old man\" sea");
        assert_eq!(mixed.len(), 3);
        assert_eq!(*mixed.last().unwrap(), 2);
        // The curly quotes a word processor would have made are quotes too.
        assert_eq!(ranked(&text, "\u{201c}old man\u{201d}"), [0, 1]);
    }

    #[test]
    fn a_heading_with_the_words_beats_a_paragraph_that_has_them_just_as_often() {
        let text = doc(vec![para("Dragons"), heading(2, "Dragons"), para("Other words here")]);
        assert_eq!(ranked(&text, "dragons"), [1, 0]);
    }

    #[test]
    fn a_word_broken_across_a_line_and_a_soft_hyphen_are_one_word() {
        let text = doc(vec![
            para("inter-\nnational affairs"),
            para("co\u{ad}operation between states"),
            para("a well-known case"),
        ]);
        assert_eq!(ranked(&text, "international"), [0]);
        assert_eq!(ranked(&text, "cooperation"), [1]);
        // A hyphen inside a line is a break between words, so either half
        // finds it.
        assert_eq!(ranked(&text, "known"), [2]);
    }

    #[test]
    fn nothing_found_and_nothing_asked_are_empty() {
        let text = library();
        assert!(text.search("zeppelin", 10).is_empty());
        assert!(text.search("", 10).is_empty());
        assert!(text.search("  !? ", 10).is_empty());
        assert!(text.search("mortimer", 0).is_empty());
        assert_eq!(text.search("mortimer", 1).len(), 1);
        assert!(doc(Vec::new()).search("mortimer", 10).is_empty());
    }

    #[test]
    fn a_hit_says_where_it_is() {
        let mut text = on_pages(
            doc(vec![heading(1, "Part"), para("a needle here"), para("nothing")]),
            &[1, 2, 2],
        );
        text.sections_from_headings();
        text.link_sections();
        let hits = text.search("needle", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].block, hits[0].page, hits[0].section), (1, 2, Some(0)));
    }

    #[test]
    fn words_are_found_with_where_they_start_and_end() {
        let mut seen: Vec<(String, usize, usize)> = Vec::new();
        each_word("Mortimer's  na\u{ef}ve", |word, start, end| {
            seen.push((word.to_string(), start, end));
        });
        assert_eq!(
            seen,
            [
                ("mortimer".to_string(), 0, 8),
                ("s".to_string(), 9, 10),
                ("naive".to_string(), 12, 18),
            ]
        );
    }

    #[test]
    fn a_short_block_is_its_own_snippet_on_one_line() {
        let text = doc(vec![
            para("The quick brown fox jumps over the lazy dog"),
            para("two\nlines\n\n  and   gaps with a needle"),
        ]);
        assert_eq!(text.search("fox", 1)[0].snippet, "The quick brown fox jumps over the lazy dog");
        assert_eq!(text.search("needle", 1)[0].snippet, "two lines and gaps with a needle");
    }

    #[test]
    fn a_long_blocks_snippet_is_cut_around_the_match() {
        let long = format!("{}NEEDLE {}", "lorem ipsum ".repeat(60), "dolor sit ".repeat(60));
        let text = doc(vec![para(&long)]);
        let snippet = &text.search("needle", 1)[0].snippet;
        assert!(snippet.contains("NEEDLE"), "{snippet}");
        assert!(snippet.starts_with('\u{2026}') && snippet.ends_with('\u{2026}'), "{snippet}");
        assert!(snippet.chars().count() <= SNIPPET_CHARS + 2, "{}", snippet.chars().count());
        assert!(!snippet.contains('\n'));

        // At the very start there is nothing before it to ellipsise.
        let early = format!("needle {}", "dolor sit ".repeat(60));
        let snippet = &doc(vec![para(&early)]).search("needle", 1)[0].snippet;
        assert!(snippet.starts_with("needle") && snippet.ends_with('\u{2026}'), "{snippet}");
    }

    #[test]
    fn a_snippet_is_cut_on_characters_not_bytes() {
        let text = format!("{} needle {}", "\u{e9}".repeat(300), "\u{fc}".repeat(300));
        let hit = &doc(vec![para(&text)]).search("needle", 1)[0];
        assert!(hit.snippet.contains("needle"));
        assert!(hit.snippet.starts_with('\u{2026}') && hit.snippet.ends_with('\u{2026}'));
        assert!(hit.snippet.chars().count() <= SNIPPET_CHARS + 2);
    }

    #[test]
    fn a_snippet_is_around_the_first_place_of_a_phrase() {
        let text = doc(vec![para("old and man, and then the old man arrived")]);
        let hit = &text.search("\"old man\"", 1)[0];
        assert!(hit.snippet.contains("old man arrived"), "{}", hit.snippet);
    }

    // ---- find_quote ---------------------------------------------------------

    fn page_of_quotes() -> DocText {
        on_pages(
            doc(vec![
                para("First paragraph of page one."),
                para("Second paragraph, with a well-\nknown name."),
                para("Page two begins here."),
            ]),
            &[1, 1, 2],
        )
    }

    #[test]
    fn a_quote_is_found_with_case_and_spacing_aside() {
        let text = page_of_quotes();
        assert_eq!(text.find_quote(1, "second paragraph"), Some(1..2));
        assert_eq!(text.find_quote(1, "  SECOND   paragraph,\nwith a "), Some(1..2));
        assert_eq!(text.find_quote(2, "begins here"), Some(2..3));
    }

    #[test]
    fn a_quote_across_paragraphs_names_both() {
        let text = page_of_quotes();
        assert_eq!(text.find_quote(1, "paragraph of page one. Second paragraph"), Some(0..2));
    }

    #[test]
    fn hyphenation_and_soft_hyphens_are_not_in_the_way() {
        let text = page_of_quotes();
        // The text has the word broken across a line; the selection may or may
        // not.
        assert_eq!(text.find_quote(1, "well-\nknown name"), Some(1..2));
        assert_eq!(text.find_quote(1, "well-known name"), Some(1..2));
        assert_eq!(text.find_quote(1, "para\u{ad}graph, with"), Some(1..2));
    }

    #[test]
    fn a_text_layers_ligatures_and_curly_quotes_still_match() {
        let text = on_pages(
            doc(vec![
                para("The \u{fb01}nal \u{fb02}ight."),
                para("She said \u{201c}hello\u{201d} twice."),
            ]),
            &[1, 1],
        );
        assert_eq!(text.find_quote(1, "the final flight"), Some(0..1));
        assert_eq!(text.find_quote(1, "She said \"hello\" twice."), Some(1..2));
    }

    #[test]
    fn a_quote_that_is_not_on_that_page_is_not_found() {
        let text = page_of_quotes();
        assert_eq!(text.find_quote(2, "second paragraph"), None, "that is on page 1");
        assert_eq!(text.find_quote(3, "second paragraph"), None, "no blocks on page 3");
        assert_eq!(text.find_quote(1, "a passage nobody wrote"), None);
        assert_eq!(text.find_quote(1, ""), None);
        assert_eq!(text.find_quote(1, "  \n "), None);
        assert_eq!(text.find_quote(1, "..."), None, "nothing to look for but punctuation");
    }

    #[test]
    fn a_quote_found_twice_is_the_first() {
        let text = on_pages(doc(vec![para("a cat sat"), para("the cat ran")]), &[1, 1]);
        assert_eq!(text.find_quote(1, "cat"), Some(0..1));
    }
}
