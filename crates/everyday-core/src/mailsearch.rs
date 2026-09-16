//! Mail search's shape, held apart from what answers it.
//!
//! # Why this is not [`crate::search`]
//!
//! [`crate::search::SearchIndex`] is an in-memory inverted index, rebuilt
//! whole on unlock (see that module's docs) because a decade of journalling
//! is a few thousand documents and rebuilding costs well under a second.
//! `docs/plans/mail.md` is explicit that mail does not get to make that
//! trade: a hundred thousand messages does not fit comfortably in memory
//! three times over (the vault's copy, the index's copy, and the query-time
//! scratch space), and rebuilding it on every unlock would blow the unlock
//! budget the same plan sets. So mail's index lives on disk, sealed, and
//! survives a lock/unlock cycle instead of being thrown away by one.
//!
//! Putting it on disk means putting it in a crate that can depend on a
//! search engine, and this crate is not that crate — `everyday-core` stays
//! synchronous and dependency-light on principle (see the crate docs). The
//! answer is the inversion already used for storage: a trait here, an
//! implementation in `everyday-mailindex`, handed to whatever assembles the
//! vault at runtime. `search_mail`, the interface's search box, and this
//! trait's `search` all resolve through the same implementation, which is
//! what stops the assistant's tools and the person's search field from ever
//! disagreeing about what a query matches.
//!
//! # What lives here
//!
//! - [`MailDoc`], the shape a message hands to the index. Deliberately built
//!   from plain [`String`] keys rather than typed ids: `Account` and
//!   `Message` records are landing in `everyday-mail` in parallel with this
//!   file, and threading this module through their typed ids before they
//!   exist would mean guessing their shape. `message_key` and `thread_key`
//!   are documented as becoming `MessageId` and `ThreadId` once that domain
//!   is in the tree; nothing about the trait's *shape* changes when it does,
//!   only the type the caller passes.
//! - [`MailSearch`], the trait `everyday-mailindex::MailIndex` implements.
//!   Synchronous, like every trait in this crate: core has no async runtime,
//!   and indexing/searching are CPU-bound work a blocking pool is the right
//!   place for, not a reason to pull tokio into a crate that does not
//!   otherwise need it.
//! - [`MailQuery`] and [`MailQuery::parse`], the query syntax Gmail users
//!   already know, parsed here rather than in the index crate so that it can
//!   be tested — thoroughly, in this module — without spinning up tantivy,
//!   and so that a future second implementation of [`MailSearch`] inherits
//!   the same syntax for free.

use crate::error::Result;
use jiff::Timestamp;
use jiff::civil::Date;
use serde::{Deserialize, Serialize};

/// A message's identity as far as search is concerned.
///
/// A plain string today because the typed `MessageId` this will become is
/// being added to `everyday-core::id` by a different change landing at the
/// same time as this one; threading a real typed id through here first would
/// mean depending on a type that does not exist yet. When it does, this
/// alias becomes `crate::id::MessageId` and every caller updates by
/// following the compiler, not by re-reading this module.
pub type MessageKey = String;

/// A thread's identity, for the same reason [`MessageKey`] is a string: it
/// becomes `crate::id::ThreadId` once the mail domain's records exist.
pub type ThreadKey = String;

/// What one message hands the index.
///
/// Everything here is either already in the clear on the wire between the
/// mail domain and the index (dates, flags) or is text the index tokenises
/// and never returns whole — see [`MailSearch::search`], which hands back
/// keys and a score, never a body. That asymmetry is what lets the index
/// hold indexed *terms* rather than a second copy of the message: the vault
/// remains the one place a body is stored in a form a person can read back.
#[derive(Debug, Clone, PartialEq)]
pub struct MailDoc {
    /// See [`MessageKey`].
    pub message_key: MessageKey,
    /// See [`ThreadKey`].
    pub thread_key: ThreadKey,
    /// Which account this message belongs to, for the account-scoping
    /// `MailSearch` callers need — see the trait's `search` docs. A plain
    /// string for the same reason the keys above are: it becomes
    /// `AccountId` once that record exists.
    pub account: String,
    /// Every mailbox (folder, or Gmail label acting as one) this message is
    /// filed under. Gmail messages are commonly filed under several at
    /// once, which is why this is a list rather than one field.
    pub mailboxes: Vec<String>,
    /// The `From` header, as displayable text (`"Alice Smith
    /// <alice@example.com>"` or just the bare address) — never parsed into
    /// structured parts here, because the index only ever needs to tokenise
    /// it, not address a reply with it.
    pub from: String,
    /// The `To` header. Multiple recipients are one string, exactly as the
    /// header carries them; the index's address tokenizer (see
    /// `everyday-mailindex`) is what makes each recipient separately
    /// findable out of that one string.
    pub to: String,
    /// The `Cc` header, same shape as `to`.
    pub cc: String,
    pub subject: String,
    /// The message's plain-text body — already stripped of quoted replies,
    /// signatures and HTML by the sync pass that built this `MailDoc` (see
    /// `Body` in `docs/plans/mail.md`'s data model). The index tokenises
    /// this and keeps none of it; see the module docs' "why not
    /// `crate::search`" for why holding it back is the whole point.
    pub body_text: String,
    /// User-applied and provider labels (Gmail's, or ones the person made),
    /// distinct from `mailboxes` in that a label is not a place a message
    /// lives, it is a tag on one.
    pub labels: Vec<String>,
    /// When the message was sent, as the mailbox records it. The one field
    /// besides the keys that the index is allowed to keep in the clear on
    /// disk — see `everyday-mailindex`'s schema docs for why: a list cannot
    /// be sorted or paged without it.
    pub date: Timestamp,
    pub has_attachment: bool,
    pub unread: bool,
    pub starred: bool,
}

/// A ranked search result.
///
/// Deliberately thin: everything a caller needs to draw a result row or
/// open a thread, and nothing that would make this look like a second copy
/// of the message. A caller that wants more asks the vault's own stores for
/// the message this hit names.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub message_key: MessageKey,
    pub thread_key: ThreadKey,
    pub score: f32,
    pub date: Timestamp,
}

/// Where the next page of a search starts.
///
/// Keyset, not offset — the same choice `docs/plans/mail.md`'s phase 0
/// makes for every other large list in the vault, for the reason spelled
/// out there: an offset a hundred thousand deep means walking and discarding
/// a hundred thousand candidates on every page. `date` and `message_key`
/// are enough to resume exactly where a page left off; see
/// `everyday-mailindex::MailIndex::search` for why those two fields, and
/// not the ranking score, are what the cursor carries.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchCursor {
    pub date: Timestamp,
    pub message_key: MessageKey,
}

/// One page of [`MailSearch::search`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SearchPage {
    pub hits: Vec<Hit>,
    /// `Some` if there may be more; hand it back as the next call's cursor.
    /// `None` means this page ended the results, not that it was empty —
    /// callers should not infer "no more" from `hits.is_empty()` on a page
    /// that never had a cursor to begin with, but in practice an empty page
    /// and `next: None` arrive together.
    pub next: Option<SearchCursor>,
}

/// The index of a mailbox's messages, kept apart from the vault's other
/// storage — see the module docs for why.
///
/// Every method is synchronous. Indexing and searching are CPU- and
/// disk-bound, not I/O-bound in the async sense, so the caller (the sync
/// engine, or a tool in `agent/tools/mail.rs`) is expected to run these on a
/// blocking pool rather than this trait growing a runtime dependency to do
/// it itself.
pub trait MailSearch: Send + Sync {
    /// Index or re-index every document in `docs`.
    ///
    /// Indexing an already-indexed `message_key` again (a flag flipped, a
    /// label added) replaces it — this is an upsert, not an append-only
    /// log — but the replacement is not visible to [`MailSearch::search`]
    /// until [`MailSearch::commit`] is called. Batching several documents
    /// into one call and committing once is how a sync pass stays inside
    /// the speed budget; see `everyday-mailindex` for the batch size that
    /// balances that against how long a crash could lose.
    fn index(&self, docs: &[MailDoc]) -> Result<()>;

    /// Remove every message named by `message_ids` from the index.
    ///
    /// Like `index`, not visible until the next commit. Removing a key that
    /// was never indexed, or was already removed, is not an error — the
    /// caller does not have to know which.
    fn delete(&self, message_ids: &[MessageKey]) -> Result<()>;

    /// Remove every document belonging to `account` from the index — what
    /// deleting the account itself calls, so a search run after the account
    /// is gone never turns up a hit for it. A whole-account term delete
    /// rather than `delete` handed every message key that account ever had:
    /// the vault's own rows are gone by the time this runs (see
    /// `everyday_core::store::accounts::AccountStore::delete_account`'s own
    /// cascade), so there is nowhere left to read those keys back from, and
    /// none is needed — every document already carries its own account,
    /// stored as one un-tokenised term precisely so it can be matched, and
    /// deleted, whole. Like `delete`, not visible until the next `commit`.
    fn delete_account(&self, account: &str) -> Result<()>;

    /// Make every `index` and `delete` call since the last commit visible to
    /// new searches.
    fn commit(&self) -> Result<()>;

    /// Search, newest first.
    ///
    /// `limit` bounds the page; `cursor` resumes a previous
    /// [`SearchPage::next`]. `None` starts from the newest message. An
    /// implementation restricts results to [`MailQuery::accounts`] when it
    /// is non-empty — see that field's docs — which is what lets the
    /// assistant and MCP each see only the accounts their caller may read
    /// without the caller having to filter the results itself.
    fn search(
        &self,
        query: &MailQuery,
        limit: usize,
        cursor: Option<SearchCursor>,
    ) -> Result<SearchPage>;

    /// True when the index cannot be searched as it stands: something is on
    /// disk at the index's path, but it could not be opened under the
    /// vault's current key — corrupt, or sealed under a key that has since
    /// changed.
    ///
    /// A brand new mailbox, with nothing on disk yet, is not this: an
    /// implementation is expected to create an empty, healthy index in that
    /// case (see `everyday-mailindex::MailIndex::open`), the same as a
    /// vault's other stores start empty rather than "needing a rebuild".
    /// What this flags is the case an empty start cannot cover — data that
    /// existed and is no longer readable — so a caller sees it after an
    /// unlock and schedules a rebuild from the vault's own `messages` and
    /// `bodies` tables rather than treating it as a fatal error: the index
    /// is a derived structure, never the only copy of anything, so losing
    /// it is an inconvenience, not data loss.
    fn rebuild_needed(&self) -> bool;

    /// Wipe this index down to nothing on disk and reopen it fresh and
    /// empty, ready for a caller to [`MailSearch::index`] a full rebuild
    /// into and [`MailSearch::commit`] it.
    ///
    /// This is the only way back from [`MailSearch::rebuild_needed`]
    /// returning `true`. `index` and `commit` cannot heal a dead index by
    /// writing into it — there is nothing on disk for them to write into in
    /// the first place, which is exactly what made `rebuild_needed` answer
    /// `true` — so a caller driving a rebuild (`rebuild_mail_index` in
    /// `everyday_service::domains::mailsync`) must call this first whenever
    /// that is the case, before indexing a single document.
    ///
    /// Also safe to call on a perfectly healthy index — it still ends empty
    /// and freshly opened either way — but a caller rebuilding only *some*
    /// accounts should not reach for this unconditionally: every account's
    /// documents are gone once this returns, not only the ones about to be
    /// re-indexed, so calling it only when [`MailSearch::rebuild_needed`]
    /// is actually `true` is what keeps a one-account rebuild from erasing
    /// every other account's search results along with it.
    fn rebuild_empty(&self) -> Result<()>;
}

// ---------------------------------------------------------------------
// Query syntax
// ---------------------------------------------------------------------

/// A parsed search query, in the syntax Gmail users already type.
///
/// Structurally: an OR of groups, each group an AND of clauses —
/// `MailQuery::parse` documents exactly how a query string maps onto this,
/// and the parser's tests are the executable version of that grammar.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MailQuery {
    /// Groups, OR'd together. A query with no `OR` in it is one group. An
    /// empty list (every group turned out to hold no clauses — an empty or
    /// all-whitespace query, say) means "match everything", which is what a
    /// bare `in:Inbox` list view wants: filters with nothing to search for.
    pub any_of: Vec<QueryGroup>,
    /// Which accounts to search, or every account the caller may read if
    /// empty.
    ///
    /// Never set by [`MailQuery::parse`] — there is no `account:` keyword in
    /// the syntax a person types, because which accounts a search may touch
    /// is a permission, not a preference. A caller (a tool in
    /// `agent/tools/mail.rs`, the interface's search field once accounts
    /// exist) sets this after parsing, from whatever the caller is allowed
    /// to see. See [`MailQuery::with_accounts`].
    pub accounts: Vec<String>,
}

/// One AND-ed group of a [`MailQuery`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QueryGroup {
    pub clauses: Vec<Clause>,
}

/// One clause: what to match, and whether the match is inverted.
#[derive(Debug, Clone, PartialEq)]
pub struct Clause {
    pub op: Op,
    /// `true` for a clause written with a leading `-`, Gmail's negation.
    pub negate: bool,
}

/// A word or a phrase, and which it was.
///
/// The distinction matters once this reaches `everyday-mailindex`: a single
/// word becomes a term match, a phrase becomes an exact, in-order match —
/// see that crate's query translation. `MailQuery::parse` decides `phrase`
/// purely from whether the source was quoted; it never guesses from
/// whitespace, so `from:alice smith` is a word (`alice`) followed by a
/// second, unrelated free-text word (`smith`), while `from:"alice smith"` is
/// one phrase.
#[derive(Debug, Clone, PartialEq)]
pub struct TextMatch {
    pub text: String,
    pub phrase: bool,
}

/// What a [`Clause`] matches.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Free text: everything that was not one of the keyword operators
    /// below. Matched against subject, body and every address field — see
    /// `everyday-mailindex`'s query translation for exactly which.
    Text(TextMatch),
    From(TextMatch),
    To(TextMatch),
    Cc(TextMatch),
    Subject(TextMatch),
    HasAttachment,
    IsUnread,
    IsRead,
    IsStarred,
    /// `in:<mailbox>` — a folder or Gmail label the message is filed under.
    In(String),
    Label(String),
    Before(DateBound),
    After(DateBound),
    OlderThan(DateBound),
    NewerThan(DateBound),
}

/// A date written on one side of `before:`, `after:`, `older_than:` or
/// `newer_than:`.
///
/// Gmail accepts both an absolute date and a duration before "now" on every
/// one of those four keywords — `older_than:2d` and `before:2024/01/15` are
/// both legal — so this type carries either, and it is
/// `everyday-mailindex::MailIndex::search` that resolves a [`Relative`]
/// bound against the moment the search runs, not the parser: resolving it
/// here would make `MailQuery::parse`'s output depend on the clock, and two
/// calls made a day apart would silently mean different things for a query
/// string that looks identical.
///
/// [`Relative`]: DateBound::Relative
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateBound {
    Absolute(Date),
    Relative { amount: i64, unit: RelUnit },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelUnit {
    Days,
    Months,
    Years,
}

impl MailQuery {
    /// Parse a query string in the syntax `docs/plans/mail.md` specifies:
    /// `from:`, `to:`, `cc:`, `subject:`, `has:attachment`, `is:unread`,
    /// `is:read`, `is:starred`, `in:<mailbox>`, `label:`, `before:`,
    /// `after:`, `older_than:`, `newer_than:` (a date as `YYYY/MM/DD` or
    /// `YYYY-MM-DD`, or a relative form like `2d`, `3m`, `1y`), `"quoted
    /// phrases"`, `-negation`, and `OR`. Everything else is free text.
    ///
    /// # Grammar, informally
    ///
    /// The string is split on whitespace into tokens, except that a `"`
    /// opens a span that swallows whitespace until the matching `"` (or the
    /// end of the string, for a query still being typed with an unclosed
    /// quote — this is lenient on purpose, because a search box updates on
    /// every keystroke and a query is very often "unclosed" for the tenth
    /// of a second before the next character lands).
    ///
    /// A bare `OR` token — uppercase, exactly, the same convention Gmail
    /// uses to tell it apart from the ordinary word "or" — starts a new
    /// group; every other token becomes a clause in the current group.
    /// [`MailQuery::any_of`] documents what the groups mean once parsing is
    /// done.
    ///
    /// Each token, after stripping a leading `-` (which sets
    /// [`Clause::negate`] and is otherwise invisible to the rest of this
    /// grammar):
    ///
    /// - If it is `"quoted"`, it is a free-text phrase.
    /// - If it has the shape `keyword:value` and `keyword` (matched without
    ///   regard to case) is one of the operators above, it becomes that
    ///   operator's clause. `value` may itself be quoted
    ///   (`subject:"hello world"`).
    /// - If the keyword is recognised but the value cannot be parsed as
    ///   what that operator needs (`is:` with a value that is none of
    ///   `unread`/`read`/`starred`, an unparsable date on `before:` and
    ///   friends), the token is not an error: it falls back to free text,
    ///   the same as Gmail's own box does with an operator it does not
    ///   recognise. A search box should never refuse to search.
    /// - Otherwise it is a free-text word.
    ///
    /// This function cannot fail: there is no query string it rejects,
    /// because a person typing into a search box should never see a parse
    /// error, only a search that came back empty.
    pub fn parse(input: &str) -> MailQuery {
        let mut groups: Vec<QueryGroup> = Vec::new();
        let mut current = QueryGroup::default();
        for raw in lex(input) {
            if raw == "OR" {
                if !current.clauses.is_empty() {
                    groups.push(std::mem::take(&mut current));
                }
                continue;
            }
            if let Some(clause) = parse_token(&raw) {
                current.clauses.push(clause);
            }
        }
        groups.push(current);
        groups.retain(|g| !g.clauses.is_empty());
        MailQuery { any_of: groups, accounts: Vec::new() }
    }

    /// Restrict the query to `accounts`. See [`MailQuery::accounts`].
    pub fn with_accounts(mut self, accounts: Vec<String>) -> Self {
        self.accounts = accounts;
        self
    }

    /// `true` if the query has no clauses at all — every group was empty,
    /// which after [`MailQuery::parse`]'s cleanup means there were no
    /// groups. A bare filter query like `in:Inbox` is not empty; an empty
    /// string, or one that is only whitespace, is.
    pub fn is_empty(&self) -> bool {
        self.any_of.is_empty()
    }
}

/// Split `input` into tokens on whitespace, treating a `"..."` span —
/// including an unterminated one that runs to the end of the string — as
/// one token regardless of whitespace inside it.
fn lex(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for c in input.chars() {
        if c == '"' {
            in_quotes = !in_quotes;
            current.push(c);
        } else if c.is_whitespace() && !in_quotes {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// `Some(inner)` if `s` opens with `"`, stripping a matching closing quote
/// if one is there and tolerating one that is not (see [`lex`]'s docs on
/// why). `None` if `s` is not quoted at all.
fn strip_quotes(s: &str) -> Option<&str> {
    let inner = s.strip_prefix('"')?;
    Some(inner.strip_suffix('"').unwrap_or(inner))
}

/// A word or phrase used as an operator's value: quoted becomes a phrase,
/// bare becomes a word, both lowercased — matching is case-insensitive
/// throughout, the same as every mail search box's.
fn text_match(value: &str) -> TextMatch {
    match strip_quotes(value) {
        Some(inner) => TextMatch { text: inner.to_lowercase(), phrase: true },
        None => TextMatch { text: value.to_lowercase(), phrase: false },
    }
}

fn parse_token(raw: &str) -> Option<Clause> {
    if raw.is_empty() || raw == "-" {
        // A bare hyphen with nothing after it is not negating anything, and
        // is not a word worth searching for either.
        return None;
    }
    let (negate, rest) = match raw.strip_prefix('-') {
        Some(r) if !r.is_empty() => (true, r),
        _ => (false, raw),
    };
    Some(Clause { op: parse_op(rest), negate })
}

fn parse_op(rest: &str) -> Op {
    if let Some(inner) = strip_quotes(rest) {
        return Op::Text(TextMatch { text: inner.to_lowercase(), phrase: true });
    }
    if let Some(idx) = rest.find(':') {
        let keyword = rest[..idx].to_lowercase();
        let value = &rest[idx + 1..];
        if !value.is_empty()
            && let Some(op) = parse_keyword_op(&keyword, value)
        {
            return op;
        }
    }
    Op::Text(TextMatch { text: rest.to_lowercase(), phrase: false })
}

/// The operator half of [`parse_op`]: `None` means "not a recognised
/// operator, or the value did not fit it", which sends the caller back to
/// the free-text fallback.
fn parse_keyword_op(keyword: &str, value: &str) -> Option<Op> {
    match keyword {
        "from" => Some(Op::From(text_match(value))),
        "to" => Some(Op::To(text_match(value))),
        "cc" => Some(Op::Cc(text_match(value))),
        "subject" => Some(Op::Subject(text_match(value))),
        "in" => Some(Op::In(unquote_lower(value))),
        "label" => Some(Op::Label(unquote_lower(value))),
        "has" if unquote_lower(value) == "attachment" => Some(Op::HasAttachment),
        "is" => match unquote_lower(value).as_str() {
            "unread" => Some(Op::IsUnread),
            "read" => Some(Op::IsRead),
            "starred" => Some(Op::IsStarred),
            _ => None,
        },
        "before" => parse_date_bound(value).map(Op::Before),
        "after" => parse_date_bound(value).map(Op::After),
        "older_than" => parse_date_bound(value).map(Op::OlderThan),
        "newer_than" => parse_date_bound(value).map(Op::NewerThan),
        _ => None,
    }
}

fn unquote_lower(value: &str) -> String {
    strip_quotes(value).unwrap_or(value).to_lowercase()
}

fn parse_date_bound(value: &str) -> Option<DateBound> {
    let value = strip_quotes(value).unwrap_or(value);
    if let Some(date) = parse_absolute_date(value) {
        return Some(DateBound::Absolute(date));
    }
    parse_relative_date(value)
}

/// `YYYY/MM/DD` or `YYYY-MM-DD`.
fn parse_absolute_date(value: &str) -> Option<Date> {
    let normalised = value.replace('/', "-");
    normalised.parse::<Date>().ok()
}

/// A count and a unit with no space between them: `2d`, `3m`, `1y`.
fn parse_relative_date(value: &str) -> Option<DateBound> {
    let unit_char = value.chars().next_back()?;
    let unit = match unit_char {
        'd' | 'D' => RelUnit::Days,
        'm' | 'M' => RelUnit::Months,
        'y' | 'Y' => RelUnit::Years,
        _ => return None,
    };
    let digits = &value[..value.len() - unit_char.len_utf8()];
    if digits.is_empty() {
        return None;
    }
    let amount: i64 = digits.parse().ok()?;
    Some(DateBound::Relative { amount: clamp_relative_amount(amount, unit), unit })
}

/// Clamp `amount` to what jiff's `Span` can hold for `unit` without
/// panicking.
///
/// `Span::days`/`months`/`years` (jiff is pinned at 0.2.35 -- see
/// `Cargo.toml`) panic outside roughly ±7,304,484 days, ±239,976 months or
/// ±19,998 years (see each method's own doc comment), and this crate builds
/// with `panic = "abort"` in release, so any amount outside that range would
/// kill the whole process the moment `everyday-mailindex` resolved it — not
/// a hypothetical typo, since the search tool's schema advertises exactly
/// this syntax to a model, which can and does try `older_than:10000000d`.
/// `MailQuery::parse` is documented never to fail, so the fix belongs here,
/// at parse time, rather than as a `Result` threaded through every caller:
/// a query for "older than 20,000 years" and one for "older than a billion
/// years" mean the same thing in practice -- everything -- so clamping loses
/// nothing a person typing the query actually wanted.
fn clamp_relative_amount(amount: i64, unit: RelUnit) -> i64 {
    let bound: i64 = match unit {
        RelUnit::Days => 7_300_000,
        RelUnit::Months => 239_000,
        RelUnit::Years => 19_990,
    };
    amount.clamp(-bound, bound)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unwraps a `MailQuery` that is expected to be exactly one group, and
    /// returns that group's clauses — the shape almost every test below
    /// wants, since almost none of them are testing `OR` itself.
    fn one_group(q: &MailQuery) -> &[Clause] {
        assert_eq!(q.any_of.len(), 1, "expected exactly one group in {q:?}");
        &q.any_of[0].clauses
    }

    fn text(t: &str) -> TextMatch {
        TextMatch { text: t.to_string(), phrase: false }
    }

    fn phrase(t: &str) -> TextMatch {
        TextMatch { text: t.to_string(), phrase: true }
    }

    #[test]
    fn empty_and_whitespace_queries_match_everything() {
        assert!(MailQuery::parse("").is_empty());
        assert!(MailQuery::parse("   ").is_empty());
        assert!(MailQuery::parse("\t\n").is_empty());
    }

    #[test]
    fn a_bare_word_is_free_text() {
        let q = MailQuery::parse("invoice");
        assert_eq!(one_group(&q), [Clause { op: Op::Text(text("invoice")), negate: false }]);
    }

    #[test]
    fn free_text_is_lowercased() {
        let q = MailQuery::parse("Invoice URGENT");
        assert_eq!(
            one_group(&q),
            [
                Clause { op: Op::Text(text("invoice")), negate: false },
                Clause { op: Op::Text(text("urgent")), negate: false },
            ]
        );
    }

    #[test]
    fn from_operator_with_a_bare_address() {
        let q = MailQuery::parse("from:alice@example.com");
        assert_eq!(
            one_group(&q),
            [Clause { op: Op::From(text("alice@example.com")), negate: false }]
        );
    }

    #[test]
    fn from_operator_is_case_insensitive_to_its_keyword() {
        let a = MailQuery::parse("from:alice");
        let b = MailQuery::parse("FROM:alice");
        let c = MailQuery::parse("From:alice");
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn to_and_cc_operators() {
        let q = MailQuery::parse("to:bob cc:carol");
        assert_eq!(
            one_group(&q),
            [
                Clause { op: Op::To(text("bob")), negate: false },
                Clause { op: Op::Cc(text("carol")), negate: false },
            ]
        );
    }

    #[test]
    fn subject_operator_with_a_quoted_phrase() {
        let q = MailQuery::parse(r#"subject:"quarterly report""#);
        assert_eq!(
            one_group(&q),
            [Clause { op: Op::Subject(phrase("quarterly report")), negate: false }]
        );
    }

    #[test]
    fn subject_operator_with_a_bare_word() {
        let q = MailQuery::parse("subject:invoice");
        assert_eq!(one_group(&q), [Clause { op: Op::Subject(text("invoice")), negate: false }]);
    }

    #[test]
    fn has_attachment() {
        let q = MailQuery::parse("has:attachment");
        assert_eq!(one_group(&q), [Clause { op: Op::HasAttachment, negate: false }]);
    }

    #[test]
    fn has_with_an_unrecognised_value_falls_back_to_text() {
        let q = MailQuery::parse("has:wings");
        assert_eq!(one_group(&q), [Clause { op: Op::Text(text("has:wings")), negate: false }]);
    }

    #[test]
    fn is_unread_read_and_starred() {
        let q = MailQuery::parse("is:unread is:read is:starred");
        assert_eq!(
            one_group(&q),
            [
                Clause { op: Op::IsUnread, negate: false },
                Clause { op: Op::IsRead, negate: false },
                Clause { op: Op::IsStarred, negate: false },
            ]
        );
    }

    #[test]
    fn is_with_an_unrecognised_value_falls_back_to_text() {
        let q = MailQuery::parse("is:important");
        assert_eq!(one_group(&q), [Clause { op: Op::Text(text("is:important")), negate: false }]);
    }

    #[test]
    fn in_mailbox() {
        let q = MailQuery::parse("in:Inbox");
        assert_eq!(one_group(&q), [Clause { op: Op::In("inbox".into()), negate: false }]);
    }

    #[test]
    fn in_mailbox_quoted_with_a_space() {
        let q = MailQuery::parse(r#"in:"Team Updates""#);
        assert_eq!(one_group(&q), [Clause { op: Op::In("team updates".into()), negate: false }]);
    }

    #[test]
    fn label_operator() {
        let q = MailQuery::parse("label:travel");
        assert_eq!(one_group(&q), [Clause { op: Op::Label("travel".into()), negate: false }]);
    }

    #[test]
    fn before_and_after_with_slash_dates() {
        let q = MailQuery::parse("after:2024/01/15 before:2024/02/01");
        assert_eq!(
            one_group(&q),
            [
                Clause {
                    op: Op::After(DateBound::Absolute(Date::new(2024, 1, 15).unwrap())),
                    negate: false
                },
                Clause {
                    op: Op::Before(DateBound::Absolute(Date::new(2024, 2, 1).unwrap())),
                    negate: false
                },
            ]
        );
    }

    #[test]
    fn dash_dates_parse_the_same_as_slash_dates() {
        let slash = MailQuery::parse("before:2024/02/01");
        let dash = MailQuery::parse("before:2024-02-01");
        assert_eq!(slash, dash);
    }

    #[test]
    fn older_than_and_newer_than_with_relative_dates() {
        let q = MailQuery::parse("older_than:2d newer_than:3m");
        assert_eq!(
            one_group(&q),
            [
                Clause {
                    op: Op::OlderThan(DateBound::Relative { amount: 2, unit: RelUnit::Days }),
                    negate: false
                },
                Clause {
                    op: Op::NewerThan(DateBound::Relative { amount: 3, unit: RelUnit::Months }),
                    negate: false
                },
            ]
        );
    }

    #[test]
    fn relative_date_units_are_days_months_and_years() {
        assert_eq!(
            parse_relative_date_for_test("5d"),
            Some(DateBound::Relative { amount: 5, unit: RelUnit::Days })
        );
        assert_eq!(
            parse_relative_date_for_test("1y"),
            Some(DateBound::Relative { amount: 1, unit: RelUnit::Years })
        );
        assert_eq!(
            parse_relative_date_for_test("12M"),
            Some(DateBound::Relative { amount: 12, unit: RelUnit::Months })
        );
    }

    fn parse_relative_date_for_test(s: &str) -> Option<DateBound> {
        super::parse_relative_date(s)
    }

    /// Regression for a process-killing panic in `everyday-mailindex`:
    /// `Span::days`/`months`/`years` (jiff 0.2.35) panic on an amount this
    /// large, and `digits.parse::<i64>()` here never used to range-check
    /// its result before building a `DateBound::Relative` with it.
    /// `MailQuery::parse` is documented never to fail, so this crate must
    /// clamp before the value ever reaches jiff.
    #[test]
    fn a_huge_relative_amount_in_every_unit_is_clamped_not_passed_through() {
        for (suffix, unit) in [("d", RelUnit::Days), ("m", RelUnit::Months), ("y", RelUnit::Years)]
        {
            let parsed = parse_relative_date_for_test(&format!("10000000000{suffix}"));
            let Some(DateBound::Relative { amount, unit: parsed_unit }) = parsed else {
                panic!("expected a relative bound for suffix {suffix:?}, got {parsed:?}");
            };
            assert_eq!(parsed_unit, unit);
            assert!(
                amount.abs() < 10_000_000_000,
                "an out-of-range amount must be clamped down, not passed through as-is"
            );
        }
    }

    #[test]
    fn i64_max_and_min_are_clamped_to_finite_amounts() {
        assert_eq!(clamp_relative_amount(i64::MAX, RelUnit::Days), 7_300_000);
        assert_eq!(clamp_relative_amount(i64::MIN, RelUnit::Days), -7_300_000);
        assert_eq!(clamp_relative_amount(i64::MAX, RelUnit::Months), 239_000);
        assert_eq!(clamp_relative_amount(i64::MAX, RelUnit::Years), 19_990);
    }

    #[test]
    fn an_ordinary_amount_is_left_exactly_as_parsed() {
        assert_eq!(clamp_relative_amount(5, RelUnit::Days), 5);
        assert_eq!(clamp_relative_amount(-3, RelUnit::Months), -3);
    }

    #[test]
    fn an_unparsable_date_falls_back_to_text() {
        let q = MailQuery::parse("before:soon");
        assert_eq!(one_group(&q), [Clause { op: Op::Text(text("before:soon")), negate: false }]);
    }

    #[test]
    fn quoted_phrase_as_free_text() {
        let q = MailQuery::parse(r#""out of office""#);
        assert_eq!(
            one_group(&q),
            [Clause { op: Op::Text(phrase("out of office")), negate: false }]
        );
    }

    #[test]
    fn an_unterminated_quote_is_tolerated() {
        let q = MailQuery::parse(r#""out of"#);
        assert_eq!(one_group(&q), [Clause { op: Op::Text(phrase("out of")), negate: false }]);
    }

    #[test]
    fn negation_of_a_bare_word() {
        let q = MailQuery::parse("-spam");
        assert_eq!(one_group(&q), [Clause { op: Op::Text(text("spam")), negate: true }]);
    }

    #[test]
    fn negation_of_an_operator() {
        let q = MailQuery::parse("-from:newsletter@example.com");
        assert_eq!(
            one_group(&q),
            [Clause { op: Op::From(text("newsletter@example.com")), negate: true }]
        );
    }

    #[test]
    fn negation_of_a_quoted_phrase() {
        let q = MailQuery::parse(r#"-"do not reply""#);
        assert_eq!(one_group(&q), [Clause { op: Op::Text(phrase("do not reply")), negate: true }]);
    }

    #[test]
    fn a_lone_hyphen_is_not_treated_as_negation_of_nothing() {
        // Stripping a leading `-` from an otherwise-empty token would leave
        // an empty rest; that must not become a clause that matches
        // everything with the sign flipped.
        let q = MailQuery::parse("- invoice");
        assert_eq!(one_group(&q), [Clause { op: Op::Text(text("invoice")), negate: false }]);
    }

    #[test]
    fn or_splits_into_groups() {
        let q = MailQuery::parse("from:alice OR from:bob");
        assert_eq!(q.any_of.len(), 2);
        assert_eq!(q.any_of[0].clauses, [Clause { op: Op::From(text("alice")), negate: false }]);
        assert_eq!(q.any_of[1].clauses, [Clause { op: Op::From(text("bob")), negate: false }]);
    }

    #[test]
    fn or_chains_into_more_than_two_groups() {
        let q = MailQuery::parse("from:alice OR from:bob OR from:carol");
        assert_eq!(q.any_of.len(), 3);
    }

    #[test]
    fn or_groups_can_hold_more_than_one_and_ed_clause() {
        let q = MailQuery::parse("invoice is:unread OR receipt is:starred");
        assert_eq!(q.any_of.len(), 2);
        assert_eq!(
            q.any_of[0].clauses,
            [
                Clause { op: Op::Text(text("invoice")), negate: false },
                Clause { op: Op::IsUnread, negate: false },
            ]
        );
        assert_eq!(
            q.any_of[1].clauses,
            [
                Clause { op: Op::Text(text("receipt")), negate: false },
                Clause { op: Op::IsStarred, negate: false },
            ]
        );
    }

    #[test]
    fn lowercase_or_is_an_ordinary_word() {
        let q = MailQuery::parse("cats or dogs");
        assert_eq!(q.any_of.len(), 1, "lowercase 'or' must not split groups");
        assert_eq!(
            one_group(&q),
            [
                Clause { op: Op::Text(text("cats")), negate: false },
                Clause { op: Op::Text(text("or")), negate: false },
                Clause { op: Op::Text(text("dogs")), negate: false },
            ]
        );
    }

    #[test]
    fn a_leading_or_with_nothing_before_it_does_not_produce_an_empty_group() {
        let q = MailQuery::parse("OR invoice");
        assert_eq!(q.any_of.len(), 1);
        assert_eq!(one_group(&q), [Clause { op: Op::Text(text("invoice")), negate: false }]);
    }

    #[test]
    fn a_trailing_or_does_not_produce_an_empty_group() {
        let q = MailQuery::parse("invoice OR");
        assert_eq!(q.any_of.len(), 1);
        assert_eq!(one_group(&q), [Clause { op: Op::Text(text("invoice")), negate: false }]);
    }

    #[test]
    fn a_realistic_gmail_style_query() {
        let q = MailQuery::parse(
            r#"from:alice subject:"quarterly report" has:attachment -is:read after:2024/01/01"#,
        );
        assert_eq!(
            one_group(&q),
            [
                Clause { op: Op::From(text("alice")), negate: false },
                Clause { op: Op::Subject(phrase("quarterly report")), negate: false },
                Clause { op: Op::HasAttachment, negate: false },
                Clause { op: Op::IsRead, negate: true },
                Clause {
                    op: Op::After(DateBound::Absolute(Date::new(2024, 1, 1).unwrap())),
                    negate: false
                },
            ]
        );
    }

    #[test]
    fn with_accounts_sets_the_restriction_and_parse_never_does() {
        let q = MailQuery::parse("from:alice account:work");
        // `account:` is not a keyword this syntax has, so it is free text --
        // scoping to accounts is a permission a caller applies afterwards,
        // never something typed into the box.
        assert!(q.accounts.is_empty());
        assert_eq!(
            one_group(&q),
            [
                Clause { op: Op::From(text("alice")), negate: false },
                Clause { op: Op::Text(text("account:work")), negate: false },
            ]
        );
        let scoped = q.with_accounts(vec!["work".to_string()]);
        assert_eq!(scoped.accounts, ["work"]);
    }
}
