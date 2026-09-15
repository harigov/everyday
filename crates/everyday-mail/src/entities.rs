//! A small, deliberately partial HTML character-reference decoder, shared by
//! [`crate::sanitize`]'s CSS scrubbing and [`crate::text`]'s hidden-content
//! detection.
//!
//! # Why this exists
//!
//! [`lol_html::html_content::Element::get_attribute`] -- what both those
//! passes read a `style=""` value through -- hands back the attribute's
//! *source* text, character references and all:
//! `style="background:url&#40;https://evil.example/t.gif&#41;"` arrives at
//! `sanitize::scrub_css` still carrying `&#40;` and `&#41;`, which do not
//! spell `url(` to a scanner that only ever looks for the literal
//! characters. ammonia, downstream, *does* decode character references when
//! it re-parses and re-serialises the attribute -- an HTML parser has to, to
//! know where the value ends -- so a scrubber that skips this step sees
//! safe-looking text and lets through exactly the `url()` it exists to
//! block. The same gap hits `text::is_invisible_style`: `display&#58;none`
//! does not match a check that only ever compares against the literal
//! string `"display"`/`"none"`, so a message can hide instructions from the
//! *person* (a real browser decodes the attribute before laying it out) while
//! staying invisible to this crate's own hidden-content detector, which is
//! backwards -- see `text.rs`'s own docs on why that detector exists at all.
//! Decoding character references before either pass runs, once, closes both
//! gaps at the source instead of chasing each downstream symptom
//! separately.
//!
//! # Why hand-rolled, and why partial
//!
//! A full HTML5 named character reference table is some two thousand
//! entries, built for round-tripping arbitrary prose -- far more than a
//! CSS/style-attribute scrubber needs, and (per `sanitize.rs`'s own
//! reasoning for `scrub_css` not being a CSS parser) more surface for
//! something to go subtly wrong on hostile input than the grammar this
//! actually has to understand. What has to be closed is exactly two things:
//! a *numeric* reference (decimal or hex) spelling an arbitrary letter or
//! symbol, which is how `&#x75;rl(` spells `u` and then `url(`; and the
//! small set of *named* references for the punctuation a CSS value's own
//! grammar or `scrub_css`'s keyword scan cares about -- `(`, `)`, `:`, `;`,
//! `/`, `@`, `#`, `=`, `?` -- which is how `&lpar;` spells `(` without a
//! digit in sight. Everything else -- `&nbsp;`, `&copy;`, the thousand
//! named references for a symbol no CSS keyword or URL scheme has ever been
//! spelled with -- is left exactly as written, on the same "small,
//! deliberately non-exhaustive" trade-off `sanitize.rs`'s tracking-pixel
//! heuristic and `text.rs`'s own invisible-content checks already make.
//!
//! An unrecognised or malformed reference -- an unknown name, a numeric
//! escape with no digits, a reference with no terminating `;` -- is left
//! untouched rather than treated as an error: this runs ahead of a scrubber
//! whose whole job is to be conservative about what it does not recognise,
//! so leaving a stray `&` as a literal `&` is the safe default, the same
//! one every browser's own lenient HTML parser makes for text that merely
//! looks like it might have been an entity.

/// Decodes the small set of HTML character references described in the
/// module docs, leaving everything else -- including a bare `&` and any
/// reference outside that set -- exactly as written.
pub(crate) fn decode_entities(input: &str) -> String {
    if !input.contains('&') {
        // The overwhelmingly common case: most style attributes carry no
        // character reference at all, answered without touching the
        // scanning loop below.
        return input.to_string();
    }

    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while !rest.is_empty() {
        if !rest.starts_with('&') {
            let ch = rest.chars().next().expect("rest is non-empty");
            out.push(ch);
            rest = &rest[ch.len_utf8()..];
            continue;
        }
        match decode_one(rest) {
            Some((ch, consumed)) => {
                out.push(ch);
                rest = &rest[consumed..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out
}

/// Tries to decode one reference starting at `s[0] == '&'`. Returns the
/// decoded character and how many bytes of `s` it consumed (including the
/// leading `&` and, when present, the trailing `;`), or `None` if `s` does
/// not start with something this decoder recognises.
fn decode_one(s: &str) -> Option<(char, usize)> {
    debug_assert!(s.starts_with('&'));
    let rest = &s[1..];

    if let Some(after_hash) = rest.strip_prefix('#') {
        let (radix, digits_source, prefix_len) =
            match after_hash.strip_prefix('x').or_else(|| after_hash.strip_prefix('X')) {
                Some(hex) => (16, hex, 3),   // '&', '#', 'x'
                None => (10, after_hash, 2), // '&', '#'
            };
        let is_digit = |b: u8| if radix == 16 { b.is_ascii_hexdigit() } else { b.is_ascii_digit() };
        let digit_count = digits_source.bytes().take_while(|&b| is_digit(b)).count();
        if digit_count == 0 {
            return None;
        }
        let digits = &digits_source[..digit_count];
        let value = u32::from_str_radix(digits, radix).ok()?;
        let ch = char::from_u32(value)?;
        let mut consumed = prefix_len + digit_count;
        // A trailing `;` is consumed when present, but not required: a
        // numeric reference is well-formed either way in HTML5's own
        // tokeniser, and refusing to decode the ones missing it would just
        // leave a door `scrub_css` needs shut open a crack.
        if digits_source.as_bytes().get(digit_count) == Some(&b';') {
            consumed += 1;
        }
        return Some((ch, consumed));
    }

    let name_len = rest.bytes().take_while(|b| b.is_ascii_alphanumeric()).count();
    if name_len == 0 || rest.as_bytes().get(name_len) != Some(&b';') {
        return None;
    }
    let ch = named_entity(&rest[..name_len])?;
    Some((ch, 1 + name_len + 1))
}

/// The punctuation a CSS value's own grammar or `scrub_css`'s keyword scan
/// cares about -- see the module docs for why the table stops there rather
/// than trying to be every named reference HTML5 defines.
fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "lpar" => '(',
        "rpar" => ')',
        "colon" => ':',
        "semi" => ';',
        "sol" => '/',
        "commat" => '@',
        "quest" => '?',
        "equals" => '=',
        "num" => '#',
        "lowbar" => '_',
        "ast" | "midast" => '*',
        "plus" => '+',
        "period" => '.',
        "comma" => ',',
        "excl" => '!',
        "grave" => '`',
        "nbsp" => '\u{a0}',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_with_no_ampersand_is_returned_unchanged() {
        assert_eq!(
            decode_entities("background:url(https://a.example/x.png)"),
            "background:url(https://a.example/x.png)"
        );
    }

    #[test]
    fn a_decimal_reference_decodes() {
        assert_eq!(decode_entities("display&#58;none"), "display:none");
    }

    #[test]
    fn a_hex_reference_decodes_case_insensitively() {
        assert_eq!(decode_entities("&#x75;rl("), "url(");
        assert_eq!(decode_entities("&#X75;rl("), "url(");
        assert_eq!(decode_entities("&#x75rl("), "url(", "a reference missing ';' still decodes");
    }

    #[test]
    fn named_punctuation_references_decode() {
        assert_eq!(decode_entities("url&lpar;a&rpar;"), "url(a)");
        assert_eq!(decode_entities("visibility&colon;hidden"), "visibility:hidden");
    }

    #[test]
    fn an_unknown_named_reference_is_left_untouched() {
        assert_eq!(decode_entities("caf&eacute; url(a)"), "caf&eacute; url(a)");
    }

    #[test]
    fn a_bare_ampersand_is_left_untouched() {
        assert_eq!(decode_entities("Q&A"), "Q&A");
        assert_eq!(decode_entities("a&b&c"), "a&b&c");
    }

    #[test]
    fn a_reference_with_no_digits_is_left_untouched() {
        assert_eq!(decode_entities("&#;x"), "&#;x");
        assert_eq!(decode_entities("&#x;x"), "&#x;x");
    }

    #[test]
    fn a_code_point_outside_the_valid_range_is_left_untouched() {
        // 0x110000 is past the last valid Unicode scalar value.
        assert_eq!(decode_entities("&#x110000;"), "&#x110000;");
    }

    #[test]
    fn entity_encoded_css_exfiltration_round_trips_to_literal_parentheses() {
        let decoded = decode_entities("background:url&#40;https://evil.example/t.gif&#41;");
        assert_eq!(decoded, "background:url(https://evil.example/t.gif)");
    }
}
