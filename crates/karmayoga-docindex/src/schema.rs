//! The tantivy schema the documents are indexed under, and the analyser
//! their words go through.
//!
//! # What a document is, in the index
//!
//! Two kinds of tantivy document, told apart by `kind`:
//!
//! - one **marker** per file: its id, the version it was indexed at, its
//!   title (and authors), and where its first passage is. It is what
//!   [`crate::DocIndex::indexed`] reads back, so a file with no text at all
//!   -- a scanned PDF -- still counts as indexed and is not read again at
//!   every pass; and it is where a search for a title or an author lands.
//! - a **passage** for every few blocks of text: the file's id, the first
//!   block's index and page, the section it is in, and the words.
//!
//! The title is on the marker alone, not repeated on every passage. A title
//! that matched every passage of a book would answer a search for its name
//! with the whole book, one passage at a time, instead of with the book.
//!
//! # Why the passage text is stored
//!
//! Mail's index stores no text -- the vault holds the message -- and this one
//! does, because a hit has to show the words around the match and cutting them
//! out of a book means parsing the book again. The index is sealed whole
//! (`karmayoga_mailindex::SealedDirectory`), so what it holds is no more
//! readable on disk than the cached text it came from.

use tantivy::schema::{
    Field, IndexRecordOption, NumericOptions, STORED, STRING, Schema, TextFieldIndexing,
    TextOptions,
};
use tantivy::tokenizer::{AsciiFoldingFilter, TextAnalyzer};

use karmayoga_mailindex::tokenizer::CjkAwareTokenizer;

/// The name the analyser is registered under.
pub const DOC_TOKENIZER: &str = "karmayoga_docs";

/// `kind` of the one document that stands for a file.
pub const KIND_MARKER: &str = "m";
/// `kind` of a passage.
pub const KIND_PASSAGE: &str = "p";

/// Every field handle, resolved once when the schema is built.
#[derive(Debug, Clone, Copy)]
pub struct Fields {
    pub file: Field,
    pub kind: Field,
    pub version: Field,
    pub title: Field,
    pub block: Field,
    pub page: Field,
    pub section: Field,
    pub text: Field,
}

/// The schema and its handles, built together so they cannot drift apart.
pub fn build() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let words = TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(DOC_TOKENIZER)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        )
        .set_stored();
    let number = NumericOptions::default().set_stored();
    let fields = Fields {
        file: b.add_text_field("file", STRING | STORED),
        kind: b.add_text_field("kind", STRING | STORED),
        version: b.add_text_field("version", TextOptions::default().set_stored()),
        title: b.add_text_field("title", words.clone()),
        block: b.add_u64_field("block", number.clone()),
        page: b.add_u64_field("page", number),
        section: b.add_text_field("section", TextOptions::default().set_stored()),
        text: b.add_text_field("text", words),
    };
    (b.build(), fields)
}

/// The analyser for `title` and `text`: the mail index's, which splits a
/// run of Chinese, Japanese or Korean into pairs of characters rather than
/// dropping it as one over-long word, with accents folded on top, so that
/// "naive" finds "na\u{ef}ve" and "Pyrenees" finds "Pyr\u{e9}n\u{e9}es". The
/// snippets are cut with the same analyser, so what is marked is what matched.
pub fn analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(CjkAwareTokenizer).filter(AsciiFoldingFilter).build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_and_fields_agree_on_field_count() {
        let (schema, _) = build();
        assert_eq!(schema.fields().count(), 8);
    }

    #[test]
    fn the_analyser_lowercases_folds_accents_and_pairs_cjk() {
        use tantivy::tokenizer::TokenStream;
        let mut analyzer = analyzer();
        let mut seen = Vec::new();
        let mut stream = analyzer.token_stream("Na\u{ef}ve Caf\u{e9} \u{4f1a}\u{8b70}\u{9332}");
        while stream.advance() {
            seen.push(stream.token().text.clone());
        }
        assert_eq!(seen, ["naive", "cafe", "\u{4f1a}\u{8b70}", "\u{8b70}\u{9332}"]);
    }
}
