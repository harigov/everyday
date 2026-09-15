//! Translating [`MailQuery`] into the boolean, term, phrase and range
//! queries tantivy understands.
//!
//! # The shape of the translation
//!
//! [`MailQuery::any_of`] is an OR of groups, each group an AND of clauses —
//! [`build`] mirrors that exactly: an outer [`BooleanQuery`] of `Should`
//! clauses, one per group, each itself a [`BooleanQuery`] of `Must` (or
//! `MustNot`, for a negated clause) sub-queries. A negated group that is
//! *only* `MustNot` clauses (`-spam` on its own, say) is anchored against
//! [`AllQuery`] — tantivy's boolean query has no way to score "everything
//! except this" without a positive clause to start from, the same reason
//! every other boolean search engine needs the same trick.
//!
//! # Free text
//!
//! A clause with no field operator searches `subject`, `body_text`, `from`,
//! `to` and `cc` at once, `Should`-joined — the same "search everything"
//! behaviour Gmail's own bare-word search gives. A field operator
//! (`from:`, `subject:`, …) narrows to that one field.
//!
//! # Dates
//!
//! [`DateBound::Relative`] is resolved against `now`, passed in by the
//! caller rather than read from the clock here — see that variant's docs
//! in `everyday_core::mailsearch` for why resolving it at parse time would
//! be wrong, and [`crate::index::MailIndex::search`] for where `now` comes
//! from.

use std::ops::Bound;

use everyday_core::{
    Clause, DateBound, MailQuery, QueryGroup, QueryOp as Op, RelUnit, SearchCursor, TextMatch,
};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};
use tantivy::Index;
use tantivy::query::{
    AllQuery, BooleanQuery, EmptyQuery, Occur, PhraseQuery, Query, RangeQuery, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption, Term};

use crate::schema::Fields;
use crate::tokenizer::address_tokens;

/// Translate `query`, resolving any relative date against `now`.
pub fn build(index: &Index, fields: &Fields, query: &MailQuery, now: Timestamp) -> Box<dyn Query> {
    let base = build_any_of(index, fields, &query.any_of, now);
    if query.accounts.is_empty() {
        base
    } else {
        let accounts = accounts_filter(fields, &query.accounts);
        Box::new(BooleanQuery::new(vec![(Occur::Must, base), (Occur::Must, accounts)]))
    }
}

/// The keyset boundary for a page after `cursor`: everything strictly
/// "later" than it in the total order [`crate::index::MailIndex`] enforces
/// (date descending, `message_key` descending as the deterministic
/// tiebreak) — see that module's docs on why the tiebreak is `message_key`
/// and not the ranking score.
pub fn cursor_filter(fields: &Fields, cursor: &SearchCursor) -> Box<dyn Query> {
    let cursor_date = cursor.date.as_microsecond();
    let older_date: Box<dyn Query> = Box::new(RangeQuery::new(
        Bound::Unbounded,
        Bound::Excluded(Term::from_field_i64(fields.date, cursor_date)),
    ));
    let same_date_and_key: Box<dyn Query> = Box::new(BooleanQuery::new(vec![
        (
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_i64(fields.date, cursor_date),
                IndexRecordOption::Basic,
            )) as Box<dyn Query>,
        ),
        (
            Occur::Must,
            Box::new(RangeQuery::new(
                Bound::Unbounded,
                Bound::Excluded(Term::from_field_text(fields.message_key, &cursor.message_key)),
            )) as Box<dyn Query>,
        ),
    ]));
    Box::new(BooleanQuery::new(vec![
        (Occur::Should, older_date),
        (Occur::Should, same_date_and_key),
    ]))
}

fn accounts_filter(fields: &Fields, accounts: &[String]) -> Box<dyn Query> {
    if accounts.len() == 1 {
        return raw_term(fields.account, &accounts[0]);
    }
    let subs = accounts.iter().map(|a| (Occur::Should, raw_term(fields.account, a))).collect();
    Box::new(BooleanQuery::new(subs))
}

fn build_any_of(
    index: &Index,
    fields: &Fields,
    groups: &[QueryGroup],
    now: Timestamp,
) -> Box<dyn Query> {
    match groups.len() {
        0 => Box::new(AllQuery),
        1 => build_group(index, fields, &groups[0], now),
        _ => {
            let subs = groups
                .iter()
                .map(|g| (Occur::Should, build_group(index, fields, g, now)))
                .collect();
            Box::new(BooleanQuery::new(subs))
        }
    }
}

fn build_group(
    index: &Index,
    fields: &Fields,
    group: &QueryGroup,
    now: Timestamp,
) -> Box<dyn Query> {
    if group.clauses.is_empty() {
        return Box::new(AllQuery);
    }
    let subs: Vec<(Occur, Box<dyn Query>)> = group
        .clauses
        .iter()
        .map(|c: &Clause| {
            let occur = if c.negate { Occur::MustNot } else { Occur::Must };
            (occur, build_clause(index, fields, &c.op, now))
        })
        .collect();
    if subs.iter().all(|(o, _)| *o == Occur::MustNot) {
        // See the module docs: an all-negative group needs a positive
        // anchor or tantivy's boolean query matches nothing at all.
        let mut anchored = vec![(Occur::Must, Box::new(AllQuery) as Box<dyn Query>)];
        anchored.extend(subs);
        Box::new(BooleanQuery::new(anchored))
    } else {
        Box::new(BooleanQuery::new(subs))
    }
}

fn build_clause(index: &Index, fields: &Fields, op: &Op, now: Timestamp) -> Box<dyn Query> {
    match op {
        Op::Text(m) => free_text_query(index, fields, m),
        Op::From(m) => address_field_query(fields.from, m),
        Op::To(m) => address_field_query(fields.to, m),
        Op::Cc(m) => address_field_query(fields.cc, m),
        Op::Subject(m) => text_field_query(index, fields.subject, m),
        Op::HasAttachment => bool_term(fields.has_attachment, true),
        Op::IsUnread => bool_term(fields.unread, true),
        Op::IsRead => bool_term(fields.unread, false),
        Op::IsStarred => bool_term(fields.starred, true),
        Op::In(mailbox) => raw_term(fields.mailboxes, mailbox),
        Op::Label(label) => raw_term(fields.labels, label),
        Op::Before(d) => {
            date_range(fields.date, Bound::Unbounded, Bound::Excluded(resolve_date(*d, now)))
        }
        Op::After(d) => {
            date_range(fields.date, Bound::Excluded(resolve_date(*d, now)), Bound::Unbounded)
        }
        Op::OlderThan(d) => {
            date_range(fields.date, Bound::Unbounded, Bound::Excluded(resolve_date(*d, now)))
        }
        Op::NewerThan(d) => {
            date_range(fields.date, Bound::Excluded(resolve_date(*d, now)), Bound::Unbounded)
        }
    }
}

/// A bare word or phrase, matched against every text-bearing field at once.
fn free_text_query(index: &Index, fields: &Fields, m: &TextMatch) -> Box<dyn Query> {
    let subs: Vec<(Occur, Box<dyn Query>)> = vec![
        (Occur::Should, text_field_query(index, fields.subject, m)),
        (Occur::Should, text_field_query(index, fields.body_text, m)),
        (Occur::Should, address_field_query(fields.from, m)),
        (Occur::Should, address_field_query(fields.to, m)),
        (Occur::Should, address_field_query(fields.cc, m)),
    ];
    Box::new(BooleanQuery::new(subs))
}

/// `subject` and `body_text` are indexed with tantivy's built-in `"default"`
/// tokenizer; this runs the same analyser over the query value so the terms
/// line up with what was indexed.
fn text_field_query(index: &Index, field: Field, m: &TextMatch) -> Box<dyn Query> {
    let terms = tokenize(index, "default", field, &m.text);
    make_query(terms, m.phrase)
}

/// `from`, `to` and `cc` use [`crate::tokenizer::address_tokens`] directly
/// rather than going through the registered analyser, and additionally
/// prefer an exact whole-address match when the value tokenised to exactly
/// one address run covering the whole value — see that module's docs.
fn address_field_query(field: Field, m: &TextMatch) -> Box<dyn Query> {
    let tokens = address_tokens(&m.text);
    if tokens.is_empty() {
        return Box::new(EmptyQuery);
    }
    if let Some(whole) = tokens
        .iter()
        .find(|t| t.position_length > 1 && t.offset_from == 0 && t.offset_to == m.text.len())
    {
        return Box::new(TermQuery::new(
            Term::from_field_text(field, &whole.text),
            IndexRecordOption::WithFreqsAndPositions,
        ));
    }
    let terms: Vec<Term> = tokens
        .into_iter()
        .filter(|t| t.position_length == 1)
        .map(|t| Term::from_field_text(field, &t.text))
        .collect();
    make_query(terms, m.phrase)
}

fn tokenize(index: &Index, analyzer_name: &str, field: Field, text: &str) -> Vec<Term> {
    let Some(mut analyzer) = index.tokenizers().get(analyzer_name) else {
        return Vec::new();
    };
    let mut stream = analyzer.token_stream(text);
    let mut terms = Vec::new();
    while let Some(tok) = stream.next() {
        terms.push(Term::from_field_text(field, &tok.text));
    }
    terms
}

/// `terms.len() == 1` is always a term match. More than one is a phrase
/// (adjacent, in order) when the source was quoted, and an unordered
/// co-occurrence otherwise — `subject:"quarterly report"` requires the
/// words adjacent; `subject:mother-in-law`, never quoted but still
/// tokenising to more than one term because of the hyphen, only requires
/// both words to appear.
fn make_query(terms: Vec<Term>, phrase: bool) -> Box<dyn Query> {
    match terms.len() {
        0 => Box::new(EmptyQuery),
        1 => Box::new(TermQuery::new(
            terms.into_iter().next().expect("len checked above"),
            IndexRecordOption::WithFreqsAndPositions,
        )),
        _ if phrase => Box::new(PhraseQuery::new(terms)),
        _ => {
            let subs = terms
                .into_iter()
                .map(|t| {
                    (
                        Occur::Must,
                        Box::new(TermQuery::new(t, IndexRecordOption::WithFreqsAndPositions))
                            as Box<dyn Query>,
                    )
                })
                .collect();
            Box::new(BooleanQuery::new(subs))
        }
    }
}

fn bool_term(field: Field, value: bool) -> Box<dyn Query> {
    Box::new(TermQuery::new(Term::from_field_bool(field, value), IndexRecordOption::Basic))
}

fn raw_term(field: Field, value: &str) -> Box<dyn Query> {
    Box::new(TermQuery::new(
        Term::from_field_text(field, &value.to_lowercase()),
        IndexRecordOption::Basic,
    ))
}

fn date_range(field: Field, lower: Bound<i64>, upper: Bound<i64>) -> Box<dyn Query> {
    let map = |b: Bound<i64>| match b {
        Bound::Included(v) => Bound::Included(Term::from_field_i64(field, v)),
        Bound::Excluded(v) => Bound::Excluded(Term::from_field_i64(field, v)),
        Bound::Unbounded => Bound::Unbounded,
    };
    Box::new(RangeQuery::new(map(lower), map(upper)))
}

/// Resolve a date bound to microseconds since the epoch, at the start
/// (UTC midnight) of the day in question. `docs/plans/mail.md` gives dates
/// on `before:`/`after:`/`older_than:`/`newer_than:` at day granularity —
/// there is no `HH:MM` in the syntax — so resolving to a day's start is
/// exact for [`DateBound::Absolute`] and a reasonable, documented rounding
/// for [`DateBound::Relative`], which only ever names a *count* of days,
/// months or years, not a time of day to be that precise about.
fn resolve_date(bound: DateBound, now: Timestamp) -> i64 {
    let date = match bound {
        DateBound::Absolute(date) => date,
        DateBound::Relative { amount, unit } => {
            let today = now.to_zoned(TimeZone::UTC).date();
            let span = match unit {
                RelUnit::Days => Span::new().days(amount),
                RelUnit::Months => Span::new().months(amount),
                RelUnit::Years => Span::new().years(amount),
            };
            today.saturating_sub(span)
        }
    };
    date_start_micros(date)
}

fn date_start_micros(date: Date) -> i64 {
    date.to_zoned(TimeZone::UTC)
        .expect("every civil date has a valid UTC midnight")
        .timestamp()
        .as_microsecond()
}
