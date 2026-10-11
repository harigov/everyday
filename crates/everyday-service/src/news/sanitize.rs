//! A story's HTML, made safe once, when it arrives: every remote picture
//! rewritten to `everyday://news/img/{story}/{token}`, everything with
//! behaviour removed, by `everyday-mail`'s own sanitiser -- see
//! `everyday_mail::sanitize::Rewrite::news`.
//!
//! Nothing here is new. Mail's sanitiser already rewrites remote images,
//! strips what runs, and reports tracking pixels, and a feed's item is the
//! same kind of thing as a message's HTML part: a stranger's markup on its
//! way into a sandboxed frame. This module only says what is different about
//! a story -- its pictures belong to `news/img`, its relative addresses mean
//! something against the page it came from -- and keeps the plain-text copy
//! beside it that the assistant and a search read.

use everyday_core::id::StoryId;
use everyday_core::news::{BodyOrigin, NewsImage, SUMMARY_CHARS, StoryBody};
use everyday_mail::sanitize::{Rewrite, sanitize};
use everyday_mail::text::html_to_text;

/// Sanitise `html` for `story`, resolving relative addresses against
/// `base_url` first, and keep a plain-text copy beside it.
///
/// The text is read from the markup as it arrived, not from the sanitised
/// copy: `html_to_text` already drops what a sighted reader could not see,
/// and reading the original keeps the words and the order the publisher gave
/// them.
pub fn body(story: StoryId, html: &str, base_url: &str, origin: BodyOrigin) -> StoryBody {
    let rewrite = Rewrite::news(story.to_string(), Some(base_url.to_string()));
    let clean = sanitize(html, &rewrite);
    StoryBody {
        story_id: story,
        html: clean.html,
        text: html_to_text(html),
        origin,
        images: clean
            .remote_images
            .into_iter()
            .map(|image| NewsImage {
                token: image.token,
                url: image.original_url,
                cached_blob: None,
            })
            .collect(),
    }
}

/// A plain-text summary of HTML (or text), at most
/// [`everyday_core::news::SUMMARY_CHARS`] characters, cut on a word.
///
/// What a row shows under the title, so it is read the way a person would:
/// the link list `html_to_text` puts at the foot of a page, the brackets it
/// draws round a link, and the `#`, `*` and `>` it puts in front of
/// headings, bullets and quotes are all taken out first.
pub fn summary(html: &str) -> String {
    let text = readable(&html_to_text(html));
    cut_on_word(&text, SUMMARY_CHARS)
}

/// `html_to_text`'s output without its decoration, whitespace collapsed.
fn readable(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        if is_footnote(line) {
            continue;
        }
        lines.push(unbracket_links(strip_prefix_marks(line.trim())));
    }
    // `[]` is a picture with no description, drawn as its description.
    lines.join(" ").replace("[]", "").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `[12]: https://example.com/`, a line of the link list at the foot.
fn is_footnote(line: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix('[') else { return false };
    let Some((number, _)) = rest.split_once("]: ") else { return false };
    !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())
}

/// A heading's `## `, a bullet's `* `, a quote's `> `.
fn strip_prefix_marks(mut line: &str) -> &str {
    loop {
        let before = line;
        line = line.trim_start();
        if let Some(rest) = line.strip_prefix("> ").or_else(|| line.strip_prefix("* ")) {
            line = rest;
        } else if line.starts_with('#') {
            let rest = line.trim_start_matches('#');
            if rest.starts_with(' ') {
                line = rest;
            }
        }
        if line == before {
            return line;
        }
    }
}

/// `[the post][3]` -> `the post`: the brackets round a link's words and the
/// footnote number after them.
fn unbracket_links(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut opens: Vec<usize> = Vec::new();
    let mut rest = line;
    while let Some(c) = rest.chars().next() {
        if c == '[' {
            opens.push(out.len());
            out.push(c);
            rest = &rest[1..];
        } else if let Some(after) = footnote_marker(rest) {
            // The `[` this closes goes with the marker.
            if let Some(at) = opens.pop() {
                out.remove(at);
            }
            rest = after;
        } else {
            if c == ']' {
                opens.pop();
            }
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// What follows `s` once a `][12]` at its start is taken off.
fn footnote_marker(s: &str) -> Option<&str> {
    let rest = s.strip_prefix("][")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    rest[digits..].strip_prefix(']')
}

/// What a cut should not end on: a comma or a dash hanging off the last word
/// kept.
const TRAILING: &[char] = &[',', ';', ':', '-', '\u{2013}', '\u{2014}'];

/// `text` cut to at most `max` characters, on the last word that fits, with
/// an ellipsis when anything was cut. Text that fits is returned as it is.
pub(super) fn cut_on_word(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    // Room for the ellipsis.
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max.saturating_sub(1)).collect();
    // Already on a word's end when the next character is a space. If not,
    // back to the last space -- unless that would throw away more than half
    // of it: one very long word is better cut inside than dropped.
    let whole = chars.next().is_none_or(char::is_whitespace);
    let cut = match head.rfind(' ') {
        Some(at) if !whole && head[..at].chars().count() * 2 >= head.chars().count() => &head[..at],
        _ => head.as_str(),
    };
    let cut = cut.trim_end_matches(|c: char| c.is_whitespace() || TRAILING.contains(&c));
    format!("{cut}\u{2026}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const POST: &str = "https://blog.example.com/2026/10/post.html";

    #[test]
    fn a_body_is_sanitised_under_the_news_route() {
        let story = StoryId::new();
        let html = r#"<p>Hello</p><script>alert(1)</script>
            <img src="/img/a.png" width="400" height="300">
            <img src="https://track.example/p.gif" width="1" height="1">"#;
        let body = body(story, html, POST, BodyOrigin::Feed);

        assert_eq!(body.story_id, story);
        assert_eq!(body.origin, BodyOrigin::Feed);
        assert!(body.html.contains("Hello"));
        assert!(!body.html.contains("script"), "{}", body.html);
        assert!(!body.html.contains("track.example"), "{}", body.html);
        assert!(body.html.contains(&format!("everyday://news/img/{story}/")), "{}", body.html);

        // The relative picture was resolved against the page before it was
        // proxied, and recorded by the address the publisher meant.
        assert_eq!(body.images.len(), 1);
        assert_eq!(body.images[0].url, "https://blog.example.com/img/a.png");
        assert_eq!(body.images[0].cached_blob, None);
        assert!(body.html.contains(&body.images[0].token), "{}", body.html);
    }

    #[test]
    fn a_body_keeps_a_plain_text_copy_from_the_original_markup() {
        let body = body(
            StoryId::new(),
            "<h1>Title</h1><p>First.</p><p style=\"display:none\">hidden</p>",
            POST,
            BodyOrigin::Extracted,
        );
        assert!(body.text.contains("Title"), "{:?}", body.text);
        assert!(body.text.contains("First."), "{:?}", body.text);
        assert!(!body.text.contains("hidden"), "{:?}", body.text);
        assert_eq!(body.origin, BodyOrigin::Extracted);
    }

    #[test]
    fn a_body_with_no_base_still_sanitises() {
        let body = body(StoryId::new(), r#"<p>x</p><img src="/a.png">"#, "", BodyOrigin::Feed);
        assert!(body.html.contains('x'));
        assert!(body.images.is_empty());
    }

    #[test]
    fn a_summary_is_plain_words() {
        let s = summary("<p>The  quick\n brown</p><p>fox &amp; friends</p>");
        assert_eq!(s, "The quick brown fox & friends");
    }

    #[test]
    fn a_summary_leaves_out_the_link_list_and_the_brackets() {
        let s = summary(r#"<p>Read <a href="https://example.com/a">the post</a> today.</p>"#);
        assert_eq!(s, "Read the post today.");
        assert!(!s.contains("example.com"), "{s}");
        assert!(!s.contains('['), "{s}");
    }

    #[test]
    fn a_summary_leaves_out_what_marks_a_heading_a_bullet_or_a_quote() {
        let html = "<h2>Heading</h2><ul><li>one</li><li>two</li></ul><blockquote>said</blockquote>";
        assert_eq!(summary(html), "Heading one two said");
    }

    #[test]
    fn a_short_summary_is_not_cut_and_has_no_ellipsis() {
        assert_eq!(summary("<p>Short.</p>"), "Short.");
        assert_eq!(summary(""), "");
    }

    #[test]
    fn a_long_summary_is_cut_on_a_word_with_an_ellipsis() {
        let html = format!("<p>{}</p>", "word ".repeat(200));
        let s = summary(&html);
        assert!(s.chars().count() <= SUMMARY_CHARS, "{}", s.chars().count());
        assert!(s.ends_with("word\u{2026}"), "{s:?}");
    }

    #[test]
    fn a_summary_with_one_enormous_word_is_cut_inside_it() {
        let s = summary(&"x".repeat(1000));
        assert_eq!(s.chars().count(), SUMMARY_CHARS);
        assert!(s.ends_with('\u{2026}'));
    }

    #[test]
    fn cutting_counts_characters_not_bytes() {
        let s = cut_on_word(&"\u{e9}t\u{e9} ".repeat(300), 20);
        assert!(s.chars().count() <= 20, "{s:?}");
        assert!(s.ends_with('\u{2026}'));
    }

    #[test]
    fn a_cut_does_not_leave_a_dangling_comma() {
        let s = cut_on_word("alpha beta, gamma delta", 13);
        assert_eq!(s, "alpha beta\u{2026}");
    }
}
