//! End-to-end coverage of [`MailIndex`] as a whole: every query operator,
//! address tokenisation reaching all the way through a real search,
//! negation, date ranges, pagination, reopening, opening under the wrong
//! key, and self-healing across a schema change or a busy writer lock.
//! Unit-level coverage of the pieces these lean on lives beside them
//! (`directory.rs`, `cache.rs`, `tokenizer.rs`); this file is about the
//! seams between them.

use std::sync::Arc;

use everyday_core::crypto::{AeadCipher, Cipher, SecretKey};
use everyday_core::{MailDoc, MailQuery, MailSearch, SearchCursor};
use everyday_mailindex::{MailIndex, SealedDirectory};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

const CACHE_BYTES: usize = 16 * 1024 * 1024;

fn cipher() -> Arc<dyn Cipher> {
    Arc::new(AeadCipher::new(&SecretKey::from_bytes([5u8; 32])))
}

fn date_ts(y: i16, m: i8, d: i8) -> Timestamp {
    Date::new(y, m, d).unwrap().to_zoned(TimeZone::UTC).unwrap().timestamp()
}

/// A message with sensible defaults, so each test only names the fields it
/// cares about.
struct Doc {
    key: &'static str,
    thread: &'static str,
    account: &'static str,
    mailboxes: Vec<&'static str>,
    from: &'static str,
    to: &'static str,
    cc: &'static str,
    subject: &'static str,
    body: &'static str,
    labels: Vec<&'static str>,
    date: Timestamp,
    has_attachment: bool,
    unread: bool,
    starred: bool,
}

impl Doc {
    fn new(key: &'static str, date: Timestamp) -> Self {
        Self {
            key,
            thread: key,
            account: "personal",
            mailboxes: vec!["inbox"],
            from: "Alice Smith <alice@example.com>",
            to: "bob@example.org",
            cc: "",
            subject: "hello",
            body: "just a note",
            labels: vec![],
            date,
            has_attachment: false,
            unread: false,
            starred: false,
        }
    }

    fn build(self) -> MailDoc {
        MailDoc {
            message_key: self.key.to_string(),
            thread_key: self.thread.to_string(),
            account: self.account.to_string(),
            mailboxes: self.mailboxes.into_iter().map(str::to_string).collect(),
            from: self.from.to_string(),
            to: self.to.to_string(),
            cc: self.cc.to_string(),
            subject: self.subject.to_string(),
            body_text: self.body.to_string(),
            labels: self.labels.into_iter().map(str::to_string).collect(),
            date: self.date,
            has_attachment: self.has_attachment,
            unread: self.unread,
            starred: self.starred,
        }
    }
}

fn open(dir: &std::path::Path) -> MailIndex {
    MailIndex::open(dir, cipher(), CACHE_BYTES).unwrap()
}

fn index_and_commit(mi: &MailIndex, docs: Vec<MailDoc>) {
    mi.index(&docs).unwrap();
    mi.commit().unwrap();
}

fn keys(page: &everyday_core::SearchPage) -> Vec<String> {
    page.hits.iter().map(|h| h.message_key.clone()).collect()
}

#[test]
fn from_operator_matches_a_name_word_and_the_bare_address() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());
    index_and_commit(
        &mi,
        vec![
            Doc::new("m1", date_ts(2024, 1, 1)).build(),
            Doc::new("m2", date_ts(2024, 1, 2)).build(),
        ],
    );

    let by_name = mi.search(&MailQuery::parse("from:alice"), 10, None).unwrap();
    assert_eq!(keys(&by_name).len(), 2);

    let by_address = mi.search(&MailQuery::parse("from:alice@example.com"), 10, None).unwrap();
    assert_eq!(keys(&by_address).len(), 2);

    let miss = mi.search(&MailQuery::parse("from:carol"), 10, None).unwrap();
    assert!(miss.hits.is_empty());
}

#[test]
fn to_and_cc_operators() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());
    let mut d1 = Doc::new("m1", date_ts(2024, 1, 1));
    d1.to = "dana@example.com";
    let mut d2 = Doc::new("m2", date_ts(2024, 1, 2));
    d2.cc = "erin@example.com";
    index_and_commit(&mi, vec![d1.build(), d2.build()]);

    assert_eq!(keys(&mi.search(&MailQuery::parse("to:dana"), 10, None).unwrap()), ["m1"]);
    assert_eq!(keys(&mi.search(&MailQuery::parse("cc:erin"), 10, None).unwrap()), ["m2"]);
}

#[test]
fn subject_word_and_phrase() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());
    let mut d1 = Doc::new("m1", date_ts(2024, 1, 1));
    d1.subject = "quarterly report attached";
    let mut d2 = Doc::new("m2", date_ts(2024, 1, 2));
    d2.subject = "report on the quarterly numbers";
    index_and_commit(&mi, vec![d1.build(), d2.build()]);

    let word = mi.search(&MailQuery::parse("subject:report"), 10, None).unwrap();
    assert_eq!(keys(&word).len(), 2);

    let phrase = mi.search(&MailQuery::parse(r#"subject:"quarterly report""#), 10, None).unwrap();
    assert_eq!(keys(&phrase), ["m1"], "only the adjacent phrase should match");
}

/// Regression for a CJK subject being indexed with zero terms: tantivy's
/// built-in `"default"` analyser tokenises a whole run of CJK characters as
/// *one* token, since nothing in the script marks a word boundary, and then
/// discards it outright once it reaches roughly 14 characters
/// (`RemoveLongFilter::limit(40)` bytes). `schema.rs` now indexes `subject`
/// and `body_text` with `CjkAwareTokenizer` instead, which bigrams a CJK
/// run rather than treating it as one word -- this proves a substring of a
/// long Japanese subject is actually findable end to end, through the real
/// index and the real query translation, not merely at the tokenizer's own
/// unit level.
#[test]
fn a_cjk_subject_is_findable_by_a_substring() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());
    let mut d1 = Doc::new("m1", date_ts(2024, 1, 1));
    d1.subject = "会議の議事録と来週の予定について";
    let mut d2 = Doc::new("m2", date_ts(2024, 1, 2));
    d2.subject = "hello world";
    index_and_commit(&mi, vec![d1.build(), d2.build()]);

    // A three-character substring of the subject, not the whole thing --
    // exactly the shape "search for a word from a sentence" takes in a
    // script with no spaces between words.
    let hits = mi.search(&MailQuery::parse("subject:議事録"), 10, None).unwrap();
    assert_eq!(keys(&hits), ["m1"]);

    // Free text (no `subject:` operator) reaches the same field too.
    let free = mi.search(&MailQuery::parse("議事録"), 10, None).unwrap();
    assert_eq!(keys(&free), ["m1"]);

    let miss = mi.search(&MailQuery::parse("subject:予定について"), 10, None).unwrap();
    assert_eq!(keys(&miss), ["m1"], "a different substring of the same subject must also match");

    let other = mi.search(&MailQuery::parse("subject:hello"), 10, None).unwrap();
    assert_eq!(
        keys(&other),
        ["m2"],
        "the CJK tokenizer must not swallow ordinary English subjects"
    );
}

#[test]
fn has_attachment() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());
    let mut with_att = Doc::new("m1", date_ts(2024, 1, 1));
    with_att.has_attachment = true;
    index_and_commit(&mi, vec![with_att.build(), Doc::new("m2", date_ts(2024, 1, 2)).build()]);

    let hits = mi.search(&MailQuery::parse("has:attachment"), 10, None).unwrap();
    assert_eq!(keys(&hits), ["m1"]);
}

#[test]
fn is_unread_read_and_starred() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());
    let mut unread = Doc::new("m1", date_ts(2024, 1, 3));
    unread.unread = true;
    let mut read = Doc::new("m2", date_ts(2024, 1, 2));
    read.unread = false;
    let mut starred = Doc::new("m3", date_ts(2024, 1, 1));
    starred.starred = true;
    index_and_commit(&mi, vec![unread.build(), read.build(), starred.build()]);

    assert_eq!(keys(&mi.search(&MailQuery::parse("is:unread"), 10, None).unwrap()), ["m1"]);
    assert_eq!(keys(&mi.search(&MailQuery::parse("is:starred"), 10, None).unwrap()), ["m3"]);
    let is_read = mi.search(&MailQuery::parse("is:read"), 10, None).unwrap();
    assert_eq!(keys(&is_read).len(), 2, "m2 and m3 are both not unread");
}

#[test]
fn in_mailbox_and_label() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());
    let mut archived = Doc::new("m1", date_ts(2024, 1, 1));
    archived.mailboxes = vec!["archive"];
    archived.labels = vec!["travel"];
    let inbox = Doc::new("m2", date_ts(2024, 1, 2));
    index_and_commit(&mi, vec![archived.build(), inbox.build()]);

    assert_eq!(keys(&mi.search(&MailQuery::parse("in:archive"), 10, None).unwrap()), ["m1"]);
    assert_eq!(keys(&mi.search(&MailQuery::parse("in:Inbox"), 10, None).unwrap()), ["m2"]);
    assert_eq!(keys(&mi.search(&MailQuery::parse("label:travel"), 10, None).unwrap()), ["m1"]);
}

#[test]
fn before_after_older_than_and_newer_than() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());
    index_and_commit(
        &mi,
        vec![
            Doc::new("jan", date_ts(2024, 1, 1)).build(),
            Doc::new("feb", date_ts(2024, 2, 1)).build(),
            Doc::new("mar", date_ts(2024, 3, 1)).build(),
        ],
    );

    assert_eq!(
        keys(&mi.search(&MailQuery::parse("before:2024/02/01"), 10, None).unwrap()),
        ["jan"]
    );
    assert_eq!(
        keys(&mi.search(&MailQuery::parse("after:2024/02/01"), 10, None).unwrap()),
        ["mar"],
        "after is exclusive of the boundary date"
    );

    let now_based = MailQuery::parse("older_than:365d");
    let old = mi.search(&now_based, 10, None).unwrap();
    assert_eq!(
        keys(&old).len(),
        3,
        "everything here is older than a year, whenever this test runs"
    );

    let nothing_that_new = MailQuery::parse("newer_than:1d");
    let none = mi.search(&nothing_that_new, 10, None).unwrap();
    assert!(none.hits.is_empty());
}

#[test]
fn negation_excludes_a_word() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());
    let mut spam = Doc::new("m1", date_ts(2024, 1, 1));
    spam.subject = "you have won a prize";
    let ham = Doc::new("m2", date_ts(2024, 1, 2));
    index_and_commit(&mi, vec![spam.build(), ham.build()]);

    let hits = mi.search(&MailQuery::parse("-prize"), 10, None).unwrap();
    assert_eq!(keys(&hits), ["m2"]);
}

#[test]
fn negation_of_an_operator() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());
    let mut unread = Doc::new("m1", date_ts(2024, 1, 2));
    unread.unread = true;
    let read = Doc::new("m2", date_ts(2024, 1, 1));
    index_and_commit(&mi, vec![unread.build(), read.build()]);

    let hits = mi.search(&MailQuery::parse("-is:unread"), 10, None).unwrap();
    assert_eq!(keys(&hits), ["m2"]);
}

#[test]
fn or_groups_combine_alternatives() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());
    let mut d1 = Doc::new("m1", date_ts(2024, 1, 3));
    d1.subject = "invoice due";
    let mut d2 = Doc::new("m2", date_ts(2024, 1, 2));
    d2.subject = "receipt enclosed";
    let mut d3 = Doc::new("m3", date_ts(2024, 1, 1));
    d3.subject = "just saying hello";
    index_and_commit(&mi, vec![d1.build(), d2.build(), d3.build()]);

    let hits = mi.search(&MailQuery::parse("invoice OR receipt"), 10, None).unwrap();
    assert_eq!(keys(&hits), ["m1", "m2"], "newest first, and m3 excluded");
}

#[test]
fn account_restriction_filters_results() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());
    let mut work = Doc::new("m1", date_ts(2024, 1, 1));
    work.account = "work";
    let mut personal = Doc::new("m2", date_ts(2024, 1, 2));
    personal.account = "personal";
    index_and_commit(&mi, vec![work.build(), personal.build()]);

    let query = MailQuery::parse("hello").with_accounts(vec!["work".to_string()]);
    let hits = mi.search(&query, 10, None).unwrap();
    assert_eq!(keys(&hits), ["m1"]);
}

#[test]
fn free_text_reaches_subject_body_and_addresses() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());
    let mut in_subject = Doc::new("m1", date_ts(2024, 1, 3));
    in_subject.subject = "windsurfing lessons";
    let mut in_body = Doc::new("m2", date_ts(2024, 1, 2));
    in_body.body = "let's go windsurfing this weekend";
    let mut in_sender = Doc::new("m3", date_ts(2024, 1, 1));
    in_sender.from = "windsurfing-club@example.com";
    index_and_commit(&mi, vec![in_subject.build(), in_body.build(), in_sender.build()]);

    let hits = mi.search(&MailQuery::parse("windsurfing"), 10, None).unwrap();
    assert_eq!(keys(&hits).len(), 3);
}

#[test]
fn results_are_newest_first() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());
    index_and_commit(
        &mi,
        vec![
            Doc::new("oldest", date_ts(2024, 1, 1)).build(),
            Doc::new("newest", date_ts(2024, 3, 1)).build(),
            Doc::new("middle", date_ts(2024, 2, 1)).build(),
        ],
    );
    let hits = mi.search(&MailQuery::parse(""), 10, None).unwrap();
    assert_eq!(keys(&hits), ["newest", "middle", "oldest"]);
}

#[test]
fn re_indexing_a_message_key_replaces_it_rather_than_duplicating() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());
    let mut v1 = Doc::new("m1", date_ts(2024, 1, 1));
    v1.unread = true;
    index_and_commit(&mi, vec![v1.build()]);
    assert_eq!(keys(&mi.search(&MailQuery::parse("is:unread"), 10, None).unwrap()), ["m1"]);

    let mut v2 = Doc::new("m1", date_ts(2024, 1, 1));
    v2.unread = false;
    index_and_commit(&mi, vec![v2.build()]);

    assert!(mi.search(&MailQuery::parse("is:unread"), 10, None).unwrap().hits.is_empty());
    let all = mi.search(&MailQuery::parse(""), 10, None).unwrap();
    assert_eq!(keys(&all), ["m1"], "must not have become two documents");
}

#[test]
fn delete_removes_a_message_from_search() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());
    index_and_commit(&mi, vec![Doc::new("m1", date_ts(2024, 1, 1)).build()]);
    assert_eq!(mi.search(&MailQuery::parse(""), 10, None).unwrap().hits.len(), 1);

    mi.delete(&["m1".to_string()]).unwrap();
    mi.commit().unwrap();
    assert!(mi.search(&MailQuery::parse(""), 10, None).unwrap().hits.is_empty());
}

#[test]
fn pagination_through_a_thousand_docs_has_no_duplicates_or_gaps() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());

    let base = date_ts(2020, 1, 1);
    let docs: Vec<MailDoc> = (0..1000)
        .map(|i| {
            let key = format!("m{i:04}");
            let date = base.checked_add(jiff::SignedDuration::from_secs(i as i64)).unwrap();
            Doc::new(Box::leak(key.into_boxed_str()), date).build()
        })
        .collect();
    index_and_commit(&mi, docs);

    let mut seen = std::collections::HashSet::new();
    let mut cursor: Option<SearchCursor> = None;
    let page_size = 37; // deliberately not a divisor of 1000
    loop {
        let page = mi.search(&MailQuery::parse(""), page_size, cursor.clone()).unwrap();
        assert!(page.hits.len() <= page_size);
        for hit in &page.hits {
            assert!(seen.insert(hit.message_key.clone()), "duplicate hit: {}", hit.message_key);
        }
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen.len(), 1000, "every document must have been seen exactly once");
}

/// The bug this test is named for: a page ordered by `(date, score)` but
/// filtered by a cursor on `(date, message_key)` agree only when every date
/// in the result set is unique. `pagination_through_a_thousand_docs...`
/// above never exercises the disagreement, because every one of its
/// thousand dates is -- deliberately -- unique too. Here, only three dates
/// cover fifty documents, so most page boundaries land mid-tie, and each
/// document's relevance for the `"urgent"` query is deliberately varied
/// (more repeats of the word, a higher BM25 score) so that if score had
/// leaked back into the page's order, this would see it as a document
/// skipped or repeated across the boundary.
#[test]
fn paging_across_tied_dates_visits_every_document_exactly_once_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());

    let dates = [date_ts(2024, 1, 1), date_ts(2024, 1, 2), date_ts(2024, 1, 3)];
    let docs: Vec<MailDoc> = (0..50)
        .map(|i| {
            let key = format!("m{i:04}");
            let mut doc = Doc::new(Box::leak(key.into_boxed_str()), dates[i % 3]);
            doc.body = Box::leak(format!("urgent {}", "urgent ".repeat(i % 5)).into_boxed_str());
            doc.build()
        })
        .collect();
    index_and_commit(&mi, docs);

    // One query tantivy scores identically for every hit (`""`, an
    // `AllQuery`) and one where relevance genuinely differs per document
    // (`"urgent"`) -- the fix must hold for both, since the earlier bug
    // was in the collector's own sort key, not in how a particular query
    // happens to score.
    for query in ["", "urgent"] {
        let mut seen_in_order: Vec<(Timestamp, String)> = Vec::new();
        let mut cursor: Option<SearchCursor> = None;
        let page_size = 7; // deliberately not a divisor of 50
        loop {
            let page = mi.search(&MailQuery::parse(query), page_size, cursor.clone()).unwrap();
            assert!(page.hits.len() <= page_size, "query {query:?}");
            for hit in &page.hits {
                seen_in_order.push((hit.date, hit.message_key.clone()));
            }
            match page.next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        assert_eq!(
            seen_in_order.len(),
            50,
            "query {query:?}: every document must be visited exactly once, not skipped or repeated"
        );
        let seen_keys: std::collections::HashSet<_> =
            seen_in_order.iter().map(|(_, k)| k.clone()).collect();
        assert_eq!(seen_keys.len(), 50, "query {query:?}: a document was repeated across pages");

        // The order this crate actually promises: `(date, message_key)`,
        // both descending -- the same key `query::cursor_filter` filters
        // on, so this only holds once the collector orders by it too.
        let mut expected = seen_in_order.clone();
        expected.sort_by(|a, b| b.cmp(a));
        assert_eq!(
            seen_in_order, expected,
            "query {query:?}: page order does not match (date desc, message_key desc)"
        );
    }
}

#[test]
fn reopening_after_commit_finds_what_was_indexed() {
    let tmp = tempfile::tempdir().unwrap();
    {
        let mi = open(tmp.path());
        index_and_commit(&mi, vec![Doc::new("m1", date_ts(2024, 1, 1)).build()]);
    }
    let reopened = open(tmp.path());
    assert!(!reopened.rebuild_needed());
    let hits = reopened.search(&MailQuery::parse("hello"), 10, None).unwrap();
    assert_eq!(keys(&hits), ["m1"]);
}

/// A wrong key decrypts to garbage inside the very same
/// `Index::open_or_create` call a real schema mismatch fails at --
/// `SealedDirectory`'s decryption happens transparently underneath it, so
/// there is no way to tell "wrong key" apart from "the schema changed" at
/// that layer. `MailIndex::open` therefore self-heals it exactly the same
/// way (see the crate's module docs, "self-healing on open"): the mailbox
/// comes back usable immediately, empty, rather than staying dead for
/// ever. Safe because the search index is never the only copy of
/// anything -- whatever `rebuild_mail_index` would need to fill it back in
/// still lives in the vault's own storage.
#[test]
fn opening_with_the_wrong_key_self_heals_into_an_empty_index() {
    let tmp = tempfile::tempdir().unwrap();
    {
        let mi = open(tmp.path());
        index_and_commit(&mi, vec![Doc::new("m1", date_ts(2024, 1, 1)).build()]);
    }
    let wrong = MailIndex::open(
        tmp.path(),
        Arc::new(AeadCipher::new(&SecretKey::from_bytes([9u8; 32]))),
        CACHE_BYTES,
    )
    .unwrap();
    assert!(
        !wrong.rebuild_needed(),
        "a wrong key must self-heal into a usable index, not stay dead forever"
    );
    assert!(wrong.healed_on_open(), "opening under the wrong key must be reported as a heal");
    let hits = wrong.search(&MailQuery::parse(""), 10, None).unwrap();
    assert!(
        hits.hits.is_empty(),
        "the old, wrong-keyed data must be gone, not merely inaccessible"
    );
}

/// The real upgrade scenario Bug 1 exists for: a persisted field's
/// tokenizer (or any other change to the schema tantivy writes to
/// `meta.json`) makes `Index::open_or_create` refuse an index that already
/// has real data on disk under the *old* schema. Before self-healing
/// existed, this left `MailSearch::rebuild_needed` permanently `true` for
/// every vault that had ever synced mail, across every restart, with
/// nothing in `everyday-app` ever calling the one thing
/// (`rebuild_mail_index`) that could fix it -- see the crate's module docs,
/// "self-healing on open", for the whole story.
///
/// Constructed by writing a *different* schema straight through
/// `SealedDirectory` with tantivy's own `Index::create`, bypassing
/// `MailIndex` entirely -- standing in for "an earlier version of this
/// crate wrote this directory with the schema it had at the time", the one
/// scenario an in-process test can reach without shipping two versions of
/// this crate.
#[test]
fn reopening_with_a_different_schema_self_heals_into_a_usable_empty_index() {
    let tmp = tempfile::tempdir().unwrap();
    {
        let directory = SealedDirectory::open(tmp.path(), cipher(), CACHE_BYTES).unwrap();
        let mut builder = tantivy::schema::Schema::builder();
        builder.add_text_field("a_field_this_crate_has_never_indexed", tantivy::schema::TEXT);
        let old_schema = builder.build();
        tantivy::Index::create(directory, old_schema, tantivy::IndexSettings::default()).unwrap();
    }

    let mi = open(tmp.path());
    assert!(
        !mi.rebuild_needed(),
        "a schema mismatch must self-heal into a usable index on open, not stay dead forever"
    );
    assert!(mi.healed_on_open(), "opening across a schema change must be reported as a heal");
    let hits = mi.search(&MailQuery::parse(""), 10, None).unwrap();
    assert!(hits.hits.is_empty(), "a self-healed index must start genuinely empty");

    // And genuinely usable afterwards, not merely reporting healthy:
    index_and_commit(&mi, vec![Doc::new("m1", date_ts(2024, 1, 1)).build()]);
    let hits = mi.search(&MailQuery::parse("hello"), 10, None).unwrap();
    assert_eq!(keys(&hits), ["m1"]);
}

/// The other half of the self-healing contract: a writer lock another live
/// handle already holds must never be treated the same as a broken schema.
/// Nothing on disk is wrong here, only unavailable to *this* handle right
/// now -- see the crate's module docs, "self-healing on open", for why only
/// a failure at `Index::open_or_create` itself, and never a failure to
/// acquire the writer afterwards, is worth wiping anything over.
#[test]
fn a_busy_writer_lock_is_not_treated_as_broken() {
    let tmp = tempfile::tempdir().unwrap();
    let holder = open(tmp.path());
    index_and_commit(&holder, vec![Doc::new("m1", date_ts(2024, 1, 1)).build()]);

    // A second handle on the same directory while `holder` still holds
    // tantivy's writer lock -- standing in for a second live process, per
    // `directory.rs`'s own docs on that lock.
    let second = open(tmp.path());
    assert!(
        second.rebuild_needed(),
        "a lock another live handle holds must look dead, not healthy"
    );
    assert!(
        !second.healed_on_open(),
        "a busy lock must never be treated as a reason to wipe anything"
    );
    drop(second);
    drop(holder);

    // Once nothing else holds the lock, a fresh open finds `holder`'s
    // committed data completely untouched -- proof the busy path above
    // never wiped anything.
    let reopened = open(tmp.path());
    assert!(!reopened.rebuild_needed());
    let hits = reopened.search(&MailQuery::parse("hello"), 10, None).unwrap();
    assert_eq!(keys(&hits), ["m1"]);
}

#[test]
fn a_brand_new_mailbox_does_not_need_a_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    // An empty directory, never opened before: a fresh mailbox, not a
    // broken one. `MailIndex::open` creates an empty index rather than
    // asking the caller to rebuild something that never existed.
    let mi = open(tmp.path());
    assert!(!mi.rebuild_needed());
}

/// Regression for "the rebuild button returns the same error for ever":
/// before `rebuild_empty` existed, nothing ever deleted or recreated the
/// index directory, so `rebuild_needed() == true` was permanent -- every
/// `index`/`commit` call into a dead `MailIndex` just failed the same way
/// `require_opened` already did. This proves the actual recovery path
/// `everyday_service::domains::mailsync::rebuild_mail_index` still needs
/// for the one case `MailIndex::open` itself does not, and must not,
/// self-heal: a busy writer lock (see `a_busy_writer_lock_is_not_treated_
/// as_broken`) -- `rebuild_empty` wipes and reopens unconditionally,
/// whatever kept `opened` empty in the first place.
#[test]
fn rebuild_empty_recovers_a_dead_index_and_reopens_it_fresh() {
    let tmp = tempfile::tempdir().unwrap();
    let holder = open(tmp.path());
    index_and_commit(&holder, vec![Doc::new("m1", date_ts(2024, 1, 1)).build()]);

    // A second handle, opened while `holder` still holds tantivy's writer
    // lock: the same dead shape `a_busy_writer_lock_is_not_treated_as_
    // broken` proves, standing in for any of the ways a real
    // `MailIndex::open` can leave `opened` empty without `open` itself
    // having a safe way to fix it.
    let dead = open(tmp.path());
    assert!(dead.rebuild_needed());
    drop(holder);

    dead.rebuild_empty().unwrap();
    assert!(!dead.rebuild_needed(), "rebuild_empty must leave a healthy, reopened index behind");

    // Fully usable afterwards, exactly like a fresh mailbox: the old data
    // is gone (rebuild_empty wipes the directory), but indexing and
    // searching into it now work rather than repeating the same "needs to
    // be rebuilt" error forever.
    index_and_commit(&dead, vec![Doc::new("m2", date_ts(2024, 1, 2)).build()]);
    let hits = dead.search(&MailQuery::parse("hello"), 10, None).unwrap();
    assert_eq!(keys(&hits), ["m2"]);
}

/// `rebuild_empty` is also safe to call on a perfectly healthy index -- it
/// still ends up empty and freshly opened either way, which is what lets
/// `rebuild_mail_index` share one code path rather than needing to know in
/// advance whether the index it was handed was actually broken.
#[test]
fn rebuild_empty_on_a_healthy_index_still_leaves_it_usable() {
    let tmp = tempfile::tempdir().unwrap();
    let mi = open(tmp.path());
    index_and_commit(&mi, vec![Doc::new("m1", date_ts(2024, 1, 1)).build()]);

    mi.rebuild_empty().unwrap();
    assert!(!mi.rebuild_needed());
    let hits = mi.search(&MailQuery::parse("hello"), 10, None).unwrap();
    assert!(hits.hits.is_empty(), "rebuild_empty always starts empty, healthy or not");

    index_and_commit(&mi, vec![Doc::new("m2", date_ts(2024, 1, 2)).build()]);
    let hits = mi.search(&MailQuery::parse("hello"), 10, None).unwrap();
    assert_eq!(keys(&hits), ["m2"]);
}
