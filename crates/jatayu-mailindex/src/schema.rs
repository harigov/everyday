//! The tantivy schema mail is indexed under, and the one place that
//! decides what gets stored.
//!
//! # Stored fields are only the keys and the date
//!
//! `docs/plans/mail.md` is explicit: the vault holds the body, so the index
//! must not. Every text field here — `from`, `to`, `cc`, `subject`,
//! `body_text` — is indexed (tokenised into postings) but never `STORED`,
//! meaning tantivy keeps the *terms* it derived from the text and discards
//! the text itself once indexing that document is done. A compromised copy
//! of the sealed index directory, decrypted, still holds no sentence anyone
//! wrote — only which words appeared in which document, which is the
//! minimum a search index can do its job with. `message_key`, `thread_key`
//! and `date` are the exception, stored because [`crate::index::MailIndex`]
//! needs them back verbatim for every hit, and none of the three is
//! content.
//!
//! # Why `message_key` is also a fast field
//!
//! Deleting or re-indexing a message means finding its existing document by
//! that key — `IndexWriter::delete_term` walks the term dictionary, which
//! needs the field indexed, not fast. The fast half is for
//! [`crate::index`]'s keyset cursor: `RangeQuery` over the indexed term
//! dictionary is what the cursor's boundary check itself uses, but reading
//! *this* document's value back out (to hand to the caller as `next`, or to
//! compare inside `search`'s own bookkeeping) is cheaper through the
//! columnar fast-field store than through the row-oriented document store
//! `STORED` would otherwise require. `thread_key` never needs to be
//! compared, only returned, so it stays `STORED` alone.

use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions,
};

use crate::tokenizer::{ADDRESS_TOKENIZER, CJK_AWARE_TOKENIZER};

/// Every field handle [`crate::index::MailIndex`] needs, resolved once at
/// schema build time rather than looked up by name on every document.
#[derive(Debug, Clone, Copy)]
pub struct Fields {
    pub message_key: Field,
    pub thread_key: Field,
    pub account: Field,
    pub mailboxes: Field,
    pub labels: Field,
    pub from: Field,
    pub to: Field,
    pub cc: Field,
    pub subject: Field,
    pub body_text: Field,
    pub date: Field,
    pub has_attachment: Field,
    pub unread: Field,
    pub starred: Field,
}

/// Build the schema and the resolved [`Fields`] together, so the two can
/// never drift apart.
pub fn build() -> (Schema, Fields) {
    let mut b = Schema::builder();

    let address_indexing = TextFieldIndexing::default()
        .set_tokenizer(ADDRESS_TOKENIZER)
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let address_options = TextOptions::default().set_indexing_options(address_indexing);

    // `subject` and `body_text` go through `CjkAwareTokenizer` rather than
    // tantivy's own `TEXT` (which uses the built-in `"default"` analyser)
    // -- see that tokenizer's own docs for why the default analyser drops a
    // CJK sentence's terms entirely. `query.rs`'s translation of a
    // `subject:` or free-text clause looks the analyser up by this same
    // name, which is what keeps indexing and querying in agreement.
    let cjk_indexing = TextFieldIndexing::default()
        .set_tokenizer(CJK_AWARE_TOKENIZER)
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let cjk_options = TextOptions::default().set_indexing_options(cjk_indexing);

    let fields = Fields {
        message_key: b.add_text_field("message_key", STRING | STORED | FAST),
        thread_key: b.add_text_field("thread_key", STRING | STORED),
        account: b.add_text_field("account", STRING),
        mailboxes: b.add_text_field("mailboxes", STRING),
        labels: b.add_text_field("labels", STRING),
        from: b.add_text_field("from", address_options.clone()),
        to: b.add_text_field("to", address_options.clone()),
        cc: b.add_text_field("cc", address_options),
        subject: b.add_text_field("subject", cjk_options.clone()),
        body_text: b.add_text_field("body_text", cjk_options),
        date: b.add_i64_field("date", INDEXED | FAST | STORED),
        has_attachment: b.add_bool_field("has_attachment", INDEXED | FAST),
        unread: b.add_bool_field("unread", INDEXED | FAST),
        starred: b.add_bool_field("starred", INDEXED | FAST),
    };

    (b.build(), fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_and_fields_agree_on_field_count() {
        let (schema, _fields) = build();
        assert_eq!(schema.fields().count(), 14);
    }
}
