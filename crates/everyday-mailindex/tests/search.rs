//! End-to-end coverage of [`MailIndex`] as a whole: every query operator,
//! address tokenisation reaching all the way through a real search,
//! negation, date ranges, pagination, reopening, and opening under the
//! wrong key. Unit-level coverage of the pieces these lean on lives beside
//! them (`directory.rs`, `cache.rs`, `tokenizer.rs`); this file is about
//! the seams between them.

use std::sync::Arc;

use everyday_core::crypto::{AeadCipher, Cipher, SecretKey};
use everyday_core::{MailDoc, MailQuery, MailSearch, SearchCursor};
use everyday_mailindex::MailIndex;
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

#[test]
fn opening_with_the_wrong_key_fails_clearly() {
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
    assert!(wrong.rebuild_needed());
    let err = wrong.search(&MailQuery::parse(""), 10, None).unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("rebuilt"));
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
