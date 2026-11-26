//! Small, dependency-free pieces of HTML handling shared across the domain
//! modules: escaping text for HTML and stripping tags.
//!
//! The three character-reference decoders (mail's, the transcript reader's and
//! the search sources') stay where they are: each knows a different set of
//! named entities, and what they decode reaches a model.

/// `s` made safe to put in HTML text or in a double-quoted attribute: the
/// five characters that mean something to a parser become their references.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Strip HTML tags out with no parser at all: everything from a `<` to the
/// next `>` is dropped and the rest kept. This is for a preview or a line of
/// text, not something rendered, so a malformed fragment degrading gracefully
/// matters far more than fidelity -- an unclosed `<` swallows the rest, and
/// a `<` inside an attribute value ends the tag early. Entities are left as
/// they are.
pub fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut inside = false;
    for c in s.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_five_characters_are_escaped_and_ampersand_only_once() {
        assert_eq!(
            escape_html(r#"Tom & "Jerry" <b>'x'</b>"#),
            "Tom &amp; &quot;Jerry&quot; &lt;b&gt;&#39;x&#39;&lt;/b&gt;"
        );
        assert_eq!(escape_html("caf\u{e9}"), "caf\u{e9}");
    }

    #[test]
    fn tags_go_and_words_stay() {
        assert_eq!(strip_tags("a <b>bold</b> &amp; <i>x"), "a bold &amp; ");
        assert_eq!(strip_tags("no markup"), "no markup");
    }
}
