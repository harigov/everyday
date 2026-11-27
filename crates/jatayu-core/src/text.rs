//! Plain-text helpers shared across the domain modules: flattening
//! whitespace to one line, and cutting text to a length in characters.
//!
//! Only the copies that were word for word the same live here. The other
//! truncators in the crate differ (some cut on bytes, some on words, some
//! end in `...` rather than the ellipsis character), and what they produce
//! reaches models and people, so each keeps its own.

/// Every run of white space made one space, and the ends trimmed: `text` on
/// one line.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// At most `max` characters of `words`, flattened onto one line, with an
/// ellipsis (U+2026, no space before it) when anything was cut. Characters,
/// never bytes: a multi-byte character sliced in half is not a shorter
/// string but a corrupt one.
pub fn one_line_ellipsized(words: &str, max: usize) -> String {
    let flat = one_line(words);
    match flat.char_indices().nth(max) {
        Some((end, _)) => format!("{}\u{2026}", &flat[..end]),
        None => flat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_space_runs_become_one_space_and_the_ends_go() {
        assert_eq!(one_line("  a \t b\n\n c  "), "a b c");
        assert_eq!(one_line(""), "");
    }

    #[test]
    fn a_long_line_is_cut_on_a_character_and_kept_on_one_line() {
        assert_eq!(one_line_ellipsized("caf\u{e9}\n  au lait", 6), "caf\u{e9} a\u{2026}");
        assert_eq!(one_line_ellipsized("short", 10), "short");
        assert_eq!(one_line_ellipsized("exact", 5), "exact", "exactly the cap is whole");
    }
}
