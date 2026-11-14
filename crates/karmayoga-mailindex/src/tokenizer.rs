//! Two tokenizers this crate registers itself, rather than using tantivy's
//! built-in `"default"` analyser as-is: [`AddressTokenizer`] for `from`,
//! `to` and `cc`, so that `from:alice` and `from:alice@example.com` both
//! match the same message, and [`CjkAwareTokenizer`] for `subject` and
//! `body_text`, so a Chinese, Japanese or Korean sentence is not indexed as
//! one enormous, immediately-discarded token. See each type's own docs.
//!
//! # Why not tantivy's default tokenizer, for addresses
//!
//! tantivy's built-in `"default"` tokenizer already splits on `@` and `.`,
//! which — on its own — very nearly gets there for free: `"Alice Smith
//! <alice@example.com>"` tokenises to `alice`, `smith`, `alice`, `example`,
//! `com`, so a query for the bare word `alice` already matches. What it does
//! not give is a single, exact term for the address itself: a query for the
//! whole address has to be built as a phrase match over three adjacent
//! tokens (`alice`, `example`, `com`), which works but is one adjacency
//! assumption away from a false negative, and gives no way to match "this
//! exact address" as a unit distinct from "these three words happened to be
//! next to each other".
//!
//! [`AddressTokenizer`] keeps every one of the default tokenizer's word
//! tokens — `from:alice` still matches by that route — and additionally
//! emits one *extra* token, at the same token position as the address's
//! last word, spanning the whole address exactly as it appeared
//! (lower-cased): `alice@example.com` as one term. `query.rs`'s translation
//! of `from:`/`to:`/`cc:` clauses looks for that whole-address token first
//! and only falls back to a phrase match over the word tokens when the
//! clause's value was not itself address-shaped (a bare name, say). This is
//! the tokenizer/query split the same way a synonym filter works in search
//! engines generally: an extra token riding alongside the ordinary ones,
//! invisible to anything that only ever looked for the ordinary ones.
//!
//! # What counts as "address-shaped"
//!
//! A run of words glued together with no gap except `.` or `@`, containing
//! at least one `@`. `john.doe@example.co.uk` is one run (glued by `.`,
//! `@`, `.`, `.`) and becomes one whole-address token; `example.com` on its
//! own, with no `@` anywhere in the run, is deliberately left alone — it is
//! two ordinary word tokens, `example` and `com`, the same as the default
//! tokenizer would give a filename or a version number. Gating the
//! whole-token behaviour on seeing an `@` is what keeps this specific to
//! addresses rather than becoming a general "any dotted text is one token"
//! rule that `subject` and `body_text` (which do not use this tokenizer)
//! would have every reason to avoid.

use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

/// The name this tokenizer is registered under. See `schema.rs` and
/// `index.rs`.
pub const ADDRESS_TOKENIZER: &str = "everyday_address";

/// One alphanumeric run in the source text, with its byte offsets.
struct Word {
    start: usize,
    end: usize,
}

/// Split `text` into words the way tantivy's own `SimpleTokenizer` does —
/// maximal runs of alphanumeric characters — and separately compute, for
/// each maximal run of words glued only by `.` or `@`, whether an `@`
/// appeared in it.
fn words(text: &str) -> Vec<Word> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if c.is_alphanumeric() {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            out.push(Word { start: s, end: i });
        }
    }
    if let Some(s) = start {
        out.push(Word { start: s, end: text.len() });
    }
    out
}

/// Tokenise `text` exactly as [`AddressTokenizer`] does, as a plain
/// function so that both the tantivy-facing tokenizer and `query.rs`'s
/// translation of a `from:`/`to:`/`cc:` clause use the same rule. Positions
/// are assigned as tantivy expects: incrementing per word, with the
/// whole-address token (when one is emitted) sharing its run's last word's
/// position, `position_length` set to how many word positions it spans.
pub fn address_tokens(text: &str) -> Vec<Token> {
    let words = words(text);
    let mut out = Vec::with_capacity(words.len());
    let mut position = 0usize;
    let mut i = 0usize;
    while i < words.len() {
        let mut j = i;
        let mut saw_at = false;
        while j + 1 < words.len() {
            match &text[words[j].end..words[j + 1].start] {
                "@" => {
                    saw_at = true;
                    j += 1;
                }
                "." => j += 1,
                _ => break,
            }
        }

        for word in &words[i..=j] {
            out.push(Token {
                offset_from: word.start,
                offset_to: word.end,
                position,
                text: text[word.start..word.end].to_lowercase(),
                position_length: 1,
            });
            position += 1;
        }

        if saw_at && j > i {
            out.push(Token {
                offset_from: words[i].start,
                offset_to: words[j].end,
                position: position - 1,
                text: text[words[i].start..words[j].end].to_lowercase(),
                position_length: j - i + 1,
            });
        }

        i = j + 1;
    }
    out
}

/// Registered under `"everyday_address"` — see `schema.rs`.
#[derive(Clone, Default)]
pub struct AddressTokenizer;

pub struct AddressTokenStream {
    tokens: Vec<Token>,
    index: usize,
}

impl Tokenizer for AddressTokenizer {
    type TokenStream<'a> = AddressTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> AddressTokenStream {
        AddressTokenStream { tokens: address_tokens(text), index: 0 }
    }
}

impl TokenStream for AddressTokenStream {
    fn advance(&mut self) -> bool {
        if self.index < self.tokens.len() {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn token(&self) -> &Token {
        &self.tokens[self.index - 1]
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.tokens[self.index - 1]
    }
}

// ---------------------------------------------------------------------
// `subject` / `body_text`: CJK-aware tokenisation
// ---------------------------------------------------------------------

/// The name this tokenizer is registered under. See `schema.rs` and
/// `index.rs`. `query.rs`'s translation of a free-text or `subject:` clause
/// looks this analyser up by the same name, which is what keeps indexing
/// and querying in agreement -- see [`cjk_aware_tokens`]'s own docs.
pub const CJK_AWARE_TOKENIZER: &str = "everyday_cjk_aware";

/// A token 40 bytes or longer is dropped rather than kept -- matching
/// tantivy's own `"default"` analyser (`RemoveLongFilter::limit(40)`).
/// [`cjk_aware_tokens`] replaces that analyser on `subject` and
/// `body_text`, not merely supplements it, so it keeps the same limit for
/// the runs it treats the same way, rather than introducing a second,
/// different one for no reason.
const MAX_WORD_BYTES: usize = 40;

/// Is `c` a character from a script where words are conventionally written
/// with no space between them -- Chinese, Japanese or Korean, broadly.
/// Covers CJK Unified Ideographs (plus Extension A and the compatibility
/// block), Hiragana, Katakana and Hangul syllables: what an ordinary
/// subject or body actually contains. Not exhaustive of every Unicode block
/// documentation calls "CJK" (Extension B and beyond sit outside the Basic
/// Multilingual Plane and are vanishingly rare in ordinary mail), but
/// comfortably covers what this tokenizer exists to fix.
fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x3040..=0x309F   // Hiragana
        | 0x30A0..=0x30FF // Katakana
        | 0x3400..=0x4DBF // CJK Unified Ideographs Extension A
        | 0x4E00..=0x9FFF // CJK Unified Ideographs
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
        | 0xAC00..=0xD7A3 // Hangul syllables
    )
}

fn push_word_token(
    out: &mut Vec<Token>,
    position: &mut usize,
    text: &str,
    start: usize,
    end: usize,
) {
    out.push(Token {
        offset_from: start,
        offset_to: end,
        position: *position,
        text: text[start..end].to_lowercase(),
        position_length: 1,
    });
    *position += 1;
}

/// Turn one maximal run of same-kind characters (all CJK, or all not) into
/// tokens, appending them to `out`. See [`cjk_aware_tokens`] for what "one
/// run" means and why the two kinds are handled differently.
fn flush_run(
    out: &mut Vec<Token>,
    position: &mut usize,
    run: &[(usize, usize)],
    cjk: bool,
    text: &str,
) {
    if run.is_empty() {
        return;
    }
    if !cjk {
        // Exactly tantivy's own `"default"` analyser for this run: one
        // token for the whole thing, dropped if it is too long.
        let (start, _) = run[0];
        let (_, end) = run[run.len() - 1];
        if end - start < MAX_WORD_BYTES {
            push_word_token(out, position, text, start, end);
        }
        return;
    }
    if run.len() == 1 {
        // Nothing to pair a lone CJK character with; keep it as its own
        // token rather than dropping it.
        let (start, end) = run[0];
        push_word_token(out, position, text, start, end);
        return;
    }
    // Overlapping two-character bigrams -- the standard substring-search
    // answer for a script with no natural word boundaries, the same shape
    // Lucene's and Elasticsearch's own CJK analysers use.
    for pair in run.windows(2) {
        let (start, _) = pair[0];
        let (_, end) = pair[1];
        push_word_token(out, position, text, start, end);
    }
}

/// Tokenise `text` for `subject`/`body_text`.
///
/// Every maximal run of non-CJK alphanumeric characters becomes one
/// lowercased token, dropped once it reaches [`MAX_WORD_BYTES`] -- exactly
/// what tantivy's own `"default"` analyser already does, and deliberately
/// unchanged here: this tokenizer only has anything new to say about the
/// runs that analyser gets wrong.
///
/// A maximal run of CJK characters (see [`is_cjk`]) has no natural word
/// boundaries for `SimpleTokenizer` to find at all, so the *whole* run
/// became one token under the default analyser -- immediately discarded by
/// `RemoveLongFilter` once it reached roughly fourteen characters, since
/// most CJK characters are three UTF-8 bytes. A subject like "会議の議事録
/// と来週の予定について" was, in full, indexed with no terms at all: not
/// merely hard to search, entirely absent from the index. Here such a run
/// instead becomes overlapping two-character bigrams (`"会議の"` yields
/// `"会議"` and `"議の"`), so a substring query matches on shared bigrams
/// even though nothing in this scripts' writing marks where one word ends
/// and the next begins. A run of exactly one CJK character, with nothing to
/// pair it with, is kept as a single-character token rather than dropped.
///
/// Positions are assigned sequentially across the whole stream, mixing word
/// tokens and bigrams in the order they appear, so a quoted phrase spanning
/// a CJK run and an adjacent word still requires adjacency the same way an
/// all-English phrase already does.
pub fn cjk_aware_tokens(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut position = 0usize;
    let mut run: Vec<(usize, usize)> = Vec::new();
    let mut run_is_cjk = false;

    for (i, c) in text.char_indices() {
        if c.is_alphanumeric() {
            let cjk = is_cjk(c);
            if !run.is_empty() && run_is_cjk != cjk {
                flush_run(&mut out, &mut position, &run, run_is_cjk, text);
                run.clear();
            }
            run_is_cjk = cjk;
            run.push((i, i + c.len_utf8()));
        } else if !run.is_empty() {
            flush_run(&mut out, &mut position, &run, run_is_cjk, text);
            run.clear();
        }
    }
    flush_run(&mut out, &mut position, &run, run_is_cjk, text);

    out
}

/// Registered under `"everyday_cjk_aware"` -- see `schema.rs`.
#[derive(Clone, Default)]
pub struct CjkAwareTokenizer;

pub struct CjkAwareTokenStream {
    tokens: Vec<Token>,
    index: usize,
}

impl Tokenizer for CjkAwareTokenizer {
    type TokenStream<'a> = CjkAwareTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> CjkAwareTokenStream {
        CjkAwareTokenStream { tokens: cjk_aware_tokens(text), index: 0 }
    }
}

impl TokenStream for CjkAwareTokenStream {
    fn advance(&mut self) -> bool {
        if self.index < self.tokens.len() {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn token(&self) -> &Token {
        &self.tokens[self.index - 1]
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.tokens[self.index - 1]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(text: &str) -> Vec<String> {
        address_tokens(text).into_iter().map(|t| t.text).collect()
    }

    #[test]
    fn a_display_name_and_address_yields_the_words_and_the_whole_address() {
        let toks = texts("Alice Smith <alice@example.com>");
        assert_eq!(toks, ["alice", "smith", "alice", "example", "com", "alice@example.com"]);
    }

    #[test]
    fn a_bare_address_yields_its_words_and_the_whole_address() {
        let toks = texts("alice@example.com");
        assert_eq!(toks, ["alice", "example", "com", "alice@example.com"]);
    }

    #[test]
    fn a_dotted_local_part_and_multi_label_domain_glue_into_one_token() {
        let toks = texts("john.doe@example.co.uk");
        assert_eq!(toks, ["john", "doe", "example", "co", "uk", "john.doe@example.co.uk"]);
    }

    #[test]
    fn a_dotted_run_with_no_at_sign_is_not_glued() {
        // No `@` in the run, so this is two ordinary words, not an address.
        let toks = texts("example.com");
        assert_eq!(toks, ["example", "com"]);
    }

    #[test]
    fn a_bare_word_with_no_address_is_just_itself() {
        let toks = texts("alice");
        assert_eq!(toks, ["alice"]);
    }

    #[test]
    fn multiple_addresses_each_get_their_own_whole_token() {
        let toks = texts("alice@example.com, bob@example.org");
        assert_eq!(
            toks,
            [
                "alice",
                "example",
                "com",
                "alice@example.com",
                "bob",
                "example",
                "org",
                "bob@example.org"
            ]
        );
    }

    #[test]
    fn uppercase_input_is_lowercased() {
        let toks = texts("Alice@Example.COM");
        assert_eq!(toks, ["alice", "example", "com", "alice@example.com"]);
    }

    #[test]
    fn the_whole_address_token_shares_the_last_words_position() {
        let toks = address_tokens("alice@example.com");
        let whole = toks.iter().find(|t| t.text == "alice@example.com").unwrap();
        let com = toks.iter().find(|t| t.text == "com").unwrap();
        assert_eq!(whole.position, com.position);
        assert_eq!(whole.position_length, 3);
    }

    #[test]
    fn empty_input_yields_no_tokens() {
        assert!(address_tokens("").is_empty());
    }

    fn cjk_texts(text: &str) -> Vec<String> {
        cjk_aware_tokens(text).into_iter().map(|t| t.text).collect()
    }

    #[test]
    fn a_cjk_run_becomes_overlapping_bigrams() {
        // 会議 (meeting) 議事録 (minutes) -- four characters, three
        // overlapping bigrams.
        assert_eq!(cjk_texts("会議事録"), ["会議", "議事", "事録"]);
    }

    #[test]
    fn a_lone_cjk_character_is_kept_as_a_single_token_rather_than_dropped() {
        assert_eq!(cjk_texts("駅"), ["駅"]);
    }

    #[test]
    fn ordinary_english_words_are_tokenised_exactly_as_before() {
        // No CJK anywhere: this must behave exactly like tantivy's own
        // `"default"` analyser -- one lowercased token per word, split on
        // punctuation and whitespace.
        assert_eq!(cjk_texts("Quarterly Report, attached!"), ["quarterly", "report", "attached"]);
    }

    #[test]
    fn a_run_forty_bytes_or_longer_is_dropped_like_the_default_analyser() {
        let long_word = "a".repeat(40);
        assert!(cjk_texts(&long_word).is_empty());
        let just_under = "a".repeat(39);
        assert_eq!(cjk_texts(&just_under), [just_under]);
    }

    #[test]
    fn cjk_and_latin_glued_together_split_into_separate_runs() {
        assert_eq!(cjk_texts("会議2024"), ["会議", "2024"]);
    }

    #[test]
    fn a_cjk_subject_is_findable_by_a_substring_query() {
        // The exact sentence from the bug report: previously indexed with
        // no subject terms at all, because the whole run was one token over
        // `RemoveLongFilter`'s 40-byte limit.
        let subject = "会議の議事録と来週の予定について";
        let terms = cjk_texts(subject);
        assert!(!terms.is_empty(), "a CJK subject must not be indexed with zero terms");
        // A substring a person might actually search for must tokenise to
        // bigrams that are themselves present in the indexed set --
        // otherwise the query and the index could never agree on anything.
        for term in cjk_texts("議事録") {
            assert!(terms.contains(&term), "{term:?} from a substring query must be a real term");
        }
    }

    #[test]
    fn positions_are_sequential_across_word_and_cjk_runs() {
        let toks = cjk_aware_tokens("hello 会議 world");
        let positions: Vec<usize> = toks.iter().map(|t| t.position).collect();
        assert_eq!(positions, (0..toks.len()).collect::<Vec<_>>());
    }
}
