//! Characters that change what text says to a model without being seen by a
//! person: zero-width characters, bidirectional overrides and isolates, the
//! soft hyphen, the Arabic letter mark, the byte-order mark, and the Unicode
//! tag block (U+E0000..U+E007F), which some models read as plain ASCII --
//! "ASCII smuggling". A stranger's mail, an invitation, a feed item or a
//! bank memo can carry instructions in them that nobody reviewing the text
//! on screen would see.
//!
//! One list, here, so every place that hands a stranger's words to the
//! assistant strips the same set. The zero-width joiner and non-joiner are
//! kept: several scripts and every compound emoji need them.

use std::borrow::Cow;

/// Whether `c` is a character that is never seen but can still be read.
pub fn is_hidden(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
            | '\u{E0000}'..='\u{E007F}'
    )
}

/// `text` without its [hidden](is_hidden) characters, borrowed when there
/// were none.
pub fn strip_hidden(text: &str) -> Cow<'_, str> {
    if text.chars().any(is_hidden) {
        Cow::Owned(text.chars().filter(|&c| !is_hidden(c)).collect())
    } else {
        Cow::Borrowed(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_characters_and_overrides_are_stripped() {
        let smuggled: String = "hi"
            .chars()
            .chain("ignore".chars().map(|c| char::from_u32(0xE0000 + c as u32).unwrap()))
            .collect();
        assert_eq!(strip_hidden(&smuggled), "hi");
        assert_eq!(strip_hidden("a\u{202E}b\u{200B}c\u{FEFF}"), "abc");
    }

    #[test]
    fn joiners_and_plain_text_are_kept() {
        let family = "👩\u{200D}👧";
        assert!(matches!(strip_hidden(family), Cow::Borrowed(_)));
        assert!(matches!(strip_hidden("plain"), Cow::Borrowed(_)));
        assert_eq!(strip_hidden("می\u{200C}خواهم"), "می\u{200C}خواهم");
    }
}
