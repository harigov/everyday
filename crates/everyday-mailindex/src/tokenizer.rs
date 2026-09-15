//! A tokenizer for `from`, `to` and `cc`, so that `from:alice` and
//! `from:alice@example.com` both match the same message.
//!
//! # Why not tantivy's default tokenizer
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
}
