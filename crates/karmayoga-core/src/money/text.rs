//! The small rules about text that every way money arrives shares: a sync,
//! a file, a person typing. Here once, so a character added to one list is
//! added to all of them -- the copies these replaced had already drifted.

/// Characters that change how text reads without being seen: bidirectional
/// overrides and isolates, zero-width characters, the Arabic letter mark and
/// the byte-order mark. A transfer's memo is the sender's words, and one of
/// these can make a payee read as somebody else's -- on screen, or in a
/// confirmation that quotes it.
pub fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}'
    )
}

/// An ISO 4217 code, upper-cased, if `text` is three ASCII letters.
pub fn currency_code(text: &str) -> Option<String> {
    let code = text.trim();
    (code.len() == 3 && code.bytes().all(|b| b.is_ascii_alphabetic()))
        .then(|| code.to_ascii_uppercase())
}

/// The most characters of an account number this application ever keeps.
pub const MASK_CHARS: usize = 4;

/// The last [`MASK_CHARS`] characters of what a bank calls an account's mask
/// -- never more of a number, whatever a bank sends.
pub fn mask(text: &str) -> String {
    let chars: Vec<char> = text.trim().chars().collect();
    chars[chars.len().saturating_sub(MASK_CHARS)..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_invisible_list_covers_overrides_isolates_and_marks() {
        for c in ['\u{202E}', '\u{2066}', '\u{200B}', '\u{FEFF}', '\u{061C}'] {
            assert!(is_invisible(c), "{:?}", c);
        }
        assert!(!is_invisible('a') && !is_invisible(' '));
    }

    #[test]
    fn a_currency_is_three_letters_and_a_mask_four_characters() {
        assert_eq!(currency_code(" usd ").as_deref(), Some("USD"));
        assert_eq!(currency_code("US"), None);
        assert_eq!(currency_code("U5D"), None);
        assert_eq!(mask("xxxx-xxxx-1234"), "1234");
        assert_eq!(mask("12"), "12");
    }
}
