//! A small Markdown parser producing ProseMirror documents.
//!
//! This is the inverse of [`everyday_core::RichDoc::to_markdown`], and it
//! exists so the Markdown backend is a real two-way format rather than a
//! one-way export. If you edit an entry in vim, the app reads your edit back
//! as structured rich text instead of a wall of literal asterisks.
//!
//! It covers the subset the editor emits — headings, paragraphs, block
//! quotes, fenced code, bullet / ordered / task lists, thematic breaks, and
//! inline emphasis, code, strikethrough, links and media. Anything it does
//! not recognise survives as paragraph text, so no content is ever lost.

use everyday_core::RichDoc;
use everyday_core::richtext::MEDIA_NODE;
use serde_json::{Value, json};

/// Blockquotes and lists nest by recursion, so a file consisting of 10 000
/// `>` characters would otherwise be a stack overflow. Past this depth the
/// remaining markup is kept as literal paragraph text.
const MAX_BLOCK_DEPTH: usize = 24;

/// Parse Markdown into a ProseMirror document.
pub fn parse(markdown: &str) -> RichDoc {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let next = parse_block(&lines, i, &mut blocks, 0);
        // Every block parser must consume at least one line. If one ever
        // fails to, force progress rather than looping forever: an infinite
        // loop here appends to `blocks` on every pass, so it does not merely
        // hang, it exhausts memory.
        i = if next > i { next } else { i + 1 };
    }
    if blocks.is_empty() {
        return RichDoc::empty();
    }
    RichDoc(json!({ "type": "doc", "content": blocks }))
}

/// Parse one block starting at `i`, push it to `out`, return the next index.
fn parse_block(lines: &[&str], mut i: usize, out: &mut Vec<Value>, depth: usize) -> usize {
    let line = lines[i];
    let trimmed = line.trim();

    if trimmed.is_empty() {
        return i + 1;
    }
    if depth >= MAX_BLOCK_DEPTH {
        out.push(json!({"type": "paragraph", "content": inline(trimmed)}));
        return i + 1;
    }

    // Thematic break
    if matches!(trimmed, "---" | "***" | "___" | "- - -") {
        out.push(json!({"type": "horizontalRule"}));
        return i + 1;
    }

    // ATX heading
    if let Some(rest) = trimmed.strip_prefix('#') {
        let level = 1 + rest.chars().take_while(|c| *c == '#').count();
        let text = trimmed[level..].trim_start();
        if level <= 6 && (text.is_empty() || trimmed.as_bytes()[level] == b' ') {
            out.push(json!({
                "type": "heading",
                "attrs": {"level": level},
                "content": inline(text),
            }));
            return i + 1;
        }
    }

    // Fenced code block
    if let Some(lang) = trimmed.strip_prefix("```") {
        let lang = lang.trim().to_string();
        let mut body = String::new();
        i += 1;
        while i < lines.len() && !lines[i].trim_start().starts_with("```") {
            body.push_str(lines[i]);
            body.push('\n');
            i += 1;
        }
        if i < lines.len() {
            i += 1; // closing fence
        }
        let content = if body.is_empty() {
            Vec::new()
        } else {
            vec![json!({"type": "text", "text": body.trim_end_matches('\n')})]
        };
        out.push(json!({
            "type": "codeBlock",
            "attrs": {"language": lang},
            "content": content,
        }));
        return i;
    }

    // Block quote: gather the run of `>` lines and parse them recursively.
    if trimmed.starts_with('>') {
        let mut inner_lines = Vec::new();
        while i < lines.len() && lines[i].trim_start().starts_with('>') {
            let l = lines[i].trim_start();
            inner_lines.push(l[1..].strip_prefix(' ').unwrap_or(&l[1..]).to_string());
            i += 1;
        }
        let refs: Vec<&str> = inner_lines.iter().map(String::as_str).collect();
        let mut inner = Vec::new();
        let mut k = 0;
        while k < refs.len() {
            k = parse_block(&refs, k, &mut inner, depth + 1);
        }
        out.push(json!({"type": "blockquote", "content": inner}));
        return i;
    }

    // Standalone media: an image or link on its own line pointing into the
    // vault's media directory is an embed, not a paragraph.
    if let Some(node) = media_node(trimmed) {
        out.push(node);
        return i + 1;
    }

    // Lists
    if let Some(kind) = list_marker(trimmed) {
        return parse_list(lines, i, kind, out, depth);
    }

    // Paragraph: consume until a blank line or the start of another block.
    //
    // The first line is always consumed, even if `starts_new_block` claims
    // it opens a block. It can say that about a line no block branch above
    // accepted -- `#hashtag` looks like a heading but is not one, because a
    // real ATX heading needs a space after the hashes. Breaking immediately
    // there would return `i` unchanged and spin the caller's loop forever.
    let mut text = String::new();
    let first = i;
    while i < lines.len() {
        let l = lines[i];
        if l.trim().is_empty() || (i > first && starts_new_block(l)) {
            break;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(l.trim_end());
        i += 1;
    }
    out.push(json!({"type": "paragraph", "content": inline(&text)}));
    i
}

fn starts_new_block(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with('#')
        || t.starts_with("```")
        || t.starts_with('>')
        || matches!(t, "---" | "***" | "___")
        || list_marker(t).is_some()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ListKind {
    Bullet,
    Ordered,
    Task,
}

/// Recognise a list marker, returning its kind. Task items are checked
/// first, since `- [x] ` also matches the bullet pattern.
fn list_marker(t: &str) -> Option<ListKind> {
    if t.starts_with("- [ ] ") || t.starts_with("- [x] ") || t.starts_with("- [X] ") {
        return Some(ListKind::Task);
    }
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") {
        return Some(ListKind::Bullet);
    }
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    if !digits.is_empty() {
        let rest = &t[digits.len()..];
        if rest.starts_with(". ") || rest.starts_with(") ") {
            return Some(ListKind::Ordered);
        }
    }
    None
}

/// Strip a list marker off a line.
///
/// Total by construction: at the nesting limit a line is folded into a list
/// whose kind may not match its own marker, so indexing must not assume the
/// line is as long as that marker implies.
fn strip_marker(t: &str, kind: ListKind) -> (&str, Option<bool>) {
    match kind {
        ListKind::Task => {
            let checked = t.as_bytes().get(3).is_some_and(|b| *b != b' ');
            (t.get(6..).unwrap_or(""), Some(checked))
        }
        ListKind::Bullet => (t.get(2..).unwrap_or(""), None),
        ListKind::Ordered => {
            let n = t.chars().take_while(char::is_ascii_digit).count();
            (t.get(n + 2..).unwrap_or(""), None)
        }
    }
}

fn parse_list(
    lines: &[&str],
    mut i: usize,
    kind: ListKind,
    out: &mut Vec<Value>,
    depth: usize,
) -> usize {
    let base_indent = indent_of(lines[i]);
    let mut items = Vec::new();
    let mut start: Option<u64> = None;

    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            // A blank line ends the list unless the next line continues it.
            let next = lines.get(i + 1).copied().unwrap_or("");
            if next.trim().is_empty() || indent_of(next) < base_indent {
                break;
            }
            if list_marker(next.trim_start()).is_none() {
                break;
            }
            i += 1;
            continue;
        }
        if indent_of(line) < base_indent {
            break;
        }
        let t = line.trim_start();
        let Some(k) = list_marker(t) else { break };
        if indent_of(line) == base_indent && k != kind {
            break; // a different list kind starts a new list
        }
        // Nested list: attach it to the item we just produced -- unless we
        // are at the nesting limit, in which case the line is folded into
        // the current list as an ordinary item. Falling through rather than
        // breaking is what guarantees the loop still advances.
        if indent_of(line) > base_indent && depth < MAX_BLOCK_DEPTH {
            let mut nested = Vec::new();
            let next = parse_list(lines, i, k, &mut nested, depth + 1);
            if let Some(Value::Object(last)) = items.last_mut()
                && let Some(Value::Array(content)) = last.get_mut("content")
            {
                content.extend(nested);
            }
            i = next;
            continue;
        }

        if kind == ListKind::Ordered && start.is_none() {
            start = t.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok();
        }
        let (text, checked) = strip_marker(t, k);
        let para = json!({"type": "paragraph", "content": inline(text.trim())});
        items.push(match checked {
            Some(c) => json!({"type": "taskItem", "attrs": {"checked": c}, "content": [para]}),
            None => json!({"type": "listItem", "content": [para]}),
        });
        i += 1;
    }

    out.push(match kind {
        ListKind::Bullet => json!({"type": "bulletList", "content": items}),
        ListKind::Task => json!({"type": "taskList", "content": items}),
        ListKind::Ordered => json!({
            "type": "orderedList",
            "attrs": {"start": start.unwrap_or(1)},
            "content": items
        }),
    });
    i
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// `![caption](media/<blob>)` or `[label](media/<blob>)` on its own line.
fn media_node(line: &str) -> Option<Value> {
    let (is_image, rest) = match line.strip_prefix("![") {
        Some(r) => (true, r),
        None => (false, line.strip_prefix('[')?),
    };
    let close = rest.find("](")?;
    let label = &rest[..close];
    let url = rest[close + 2..].strip_suffix(')')?;
    let blob = url.strip_prefix("media/")?;
    if blob.contains(')') || blob.len() != 64 {
        return None;
    }
    Some(json!({
        "type": MEDIA_NODE,
        "attrs": {
            "blob": blob,
            "kind": if is_image { "image" } else { "file" },
            "caption": label,
        }
    }))
}

/// Parse inline markup into ProseMirror text nodes with marks.
fn inline(text: &str) -> Vec<Value> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut buf = String::new();
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0;

    // Flush accumulated literal text as an unmarked text node.
    macro_rules! flush {
        () => {
            if !buf.is_empty() {
                out.push(json!({"type": "text", "text": std::mem::take(&mut buf)}));
            }
        };
    }

    while i < bytes.len() {
        let c = bytes[i];

        // Escape: a backslash makes the next character literal.
        if c == '\\' && i + 1 < bytes.len() {
            buf.push(bytes[i + 1]);
            i += 2;
            continue;
        }

        // Inline code binds tighter than everything else.
        if c == '`'
            && let Some(end) = find_from(&bytes, i + 1, "`")
        {
            flush!();
            out.push(json!({
                "type": "text",
                "text": bytes[i + 1..end].iter().collect::<String>(),
                "marks": [{"type": "code"}],
            }));
            i = end + 1;
            continue;
        }

        // Two-character delimiters. Resolved with `find_map` rather than a
        // loop containing `continue`, which would bind to the inner loop.
        let two = [("**", "bold"), ("~~", "strike"), ("==", "highlight")].into_iter().find_map(
            |(delim, mark)| {
                (starts_with_at(&bytes, i, delim) && opens(&bytes, i, 2))
                    .then(|| find_closer(&bytes, i + 2, delim).map(|end| (end, mark)))
                    .flatten()
            },
        );
        if let Some((end, mark)) = two {
            flush!();
            out.extend(marked(&bytes[i + 2..end].iter().collect::<String>(), mark));
            i = end + 2;
            continue;
        }

        if c == '*'
            && !starts_with_at(&bytes, i, "**")
            && opens(&bytes, i, 1)
            && let Some(end) = find_closer(&bytes, i + 1, "*")
        {
            flush!();
            out.extend(marked(&bytes[i + 1..end].iter().collect::<String>(), "italic"));
            i = end + 1;
            continue;
        }

        // Link: [label](href)
        if c == '['
            && let Some(close) = find_from(&bytes, i + 1, "]")
            && bytes.get(close + 1) == Some(&'(')
            && let Some(paren) = find_from(&bytes, close + 2, ")")
        {
            flush!();
            let label: String = bytes[i + 1..close].iter().collect();
            let href: String = bytes[close + 2..paren].iter().collect();
            out.push(json!({
                "type": "text",
                "text": label,
                "marks": [{"type": "link", "attrs": {"href": href}}],
            }));
            i = paren + 1;
            continue;
        }

        if c == '\n' {
            flush!();
            out.push(json!({"type": "hardBreak"}));
            i += 1;
            continue;
        }

        buf.push(c);
        i += 1;
    }
    flush!();
    out
}

/// Recursively parse `text` and add `mark` to every text node it produces,
/// so `**bold and *italic*** ` nests correctly.
fn marked(text: &str, mark: &str) -> Vec<Value> {
    inline(text)
        .into_iter()
        .map(|mut node| {
            if node.get("type").and_then(Value::as_str) == Some("text") {
                let marks =
                    node.get("marks").and_then(Value::as_array).cloned().unwrap_or_default();
                let mut marks = marks;
                marks.push(json!({"type": mark}));
                node["marks"] = Value::Array(marks);
            }
            node
        })
        .collect()
}

fn starts_with_at(chars: &[char], i: usize, pat: &str) -> bool {
    pat.chars().enumerate().all(|(k, c)| chars.get(i + k) == Some(&c))
}

fn find_from(chars: &[char], from: usize, pat: &str) -> Option<usize> {
    let n = pat.chars().count();
    (from..chars.len().saturating_sub(n - 1)).find(|&i| starts_with_at(chars, i, pat))
}

/// Can a delimiter of width `n` at `i` *open* emphasis?
///
/// CommonMark's left-flanking rule: a run followed by whitespace is not an
/// opener. Without this, ordinary prose such as "a lone * asterisk" would be
/// silently swallowed as markup instead of surviving as text.
fn opens(chars: &[char], i: usize, n: usize) -> bool {
    chars.get(i + n).is_some_and(|c| !c.is_whitespace())
}

/// Find the matching *closing* delimiter: right-flanking, so it may not be
/// preceded by whitespace, and it must leave the emphasis non-empty.
fn find_closer(chars: &[char], from: usize, pat: &str) -> Option<usize> {
    let n = pat.chars().count();
    (from + 1..chars.len().saturating_sub(n - 1)).find(|&i| {
        starts_with_at(chars, i, pat) && chars.get(i - 1).is_some_and(|c| !c.is_whitespace())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(doc: &RichDoc) -> String {
        doc.plain_text()
    }

    #[test]
    fn parses_headings_and_paragraphs() {
        let d = parse("# Title\n\nSome body text.\n");
        let c = d.0["content"].as_array().unwrap();
        assert_eq!(c[0]["type"], "heading");
        assert_eq!(c[0]["attrs"]["level"], 1);
        assert_eq!(c[1]["type"], "paragraph");
        assert_eq!(text_of(&d), "Title\nSome body text.");
    }

    #[test]
    fn a_hash_without_a_space_is_not_a_heading() {
        let d = parse("#hashtag not a heading");
        assert_eq!(d.0["content"][0]["type"], "paragraph");
    }

    #[test]
    fn lines_that_only_look_like_blocks_terminate() {
        // Regression: each of these was accepted by `starts_new_block` but
        // rejected by every block branch, so the parser stopped advancing
        // and allocated until the process was killed.
        for src in [
            "#hashtag not a heading",
            "####### seven hashes is not a heading",
            "#no-space #another",
        ] {
            let d = parse(src);
            let blocks = d.0["content"].as_array().unwrap();
            assert_eq!(blocks.len(), 1, "{src:?} produced {} blocks", blocks.len());
            assert_eq!(blocks[0]["type"], "paragraph", "{src:?} is not a heading");
            assert_eq!(d.plain_text(), src, "{src:?} lost its text");
        }

        // A bare `#` is a legitimate empty heading, so it is not in the list
        // above -- but it must still terminate.
        let d = parse("#");
        assert_eq!(d.0["content"][0]["type"], "heading");
    }

    #[test]
    fn every_block_parser_consumes_at_least_one_line() {
        // A direct property check on the invariant fix 2 enforces.
        for src in ["#hashtag", "#######x", "---", "> q", "- a", "1. a", "```", "text", "#"] {
            let lines: Vec<&str> = src.lines().collect();
            if lines.is_empty() {
                continue;
            }
            let mut out = Vec::new();
            assert!(
                parse_block(&lines, 0, &mut out, 0) > 0,
                "parse_block made no progress on {src:?}"
            );
        }
    }

    #[test]
    fn parses_inline_marks() {
        let d = parse("plain **bold** *italic* `code` ~~struck~~");
        let marks: Vec<String> = d.0["content"][0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|n| n["marks"][0]["type"].as_str().map(str::to_string))
            .collect();
        assert_eq!(marks, ["bold", "italic", "code", "strike"]);
    }

    #[test]
    fn parses_links() {
        let d = parse("see [the docs](https://example.com/a_b) for more");
        let node = &d.0["content"][0]["content"][1];
        assert_eq!(node["text"], "the docs");
        assert_eq!(node["marks"][0]["attrs"]["href"], "https://example.com/a_b");
    }

    #[test]
    fn backslash_escapes_are_honoured() {
        let d = parse(r"literal \*not italic\* here");
        assert_eq!(text_of(&d), "literal *not italic* here");
        assert!(d.0["content"][0]["content"][0]["marks"].is_null());
    }

    #[test]
    fn unmatched_delimiters_stay_literal() {
        let d = parse("a lone * asterisk and ** two");
        assert_eq!(text_of(&d), "a lone * asterisk and ** two");
    }

    #[test]
    fn asterisks_in_ordinary_prose_are_not_markup() {
        for src in
            ["a lone * asterisk and ** two", "5 * 3 = 15", "see the footnote *", "trailing ** "]
        {
            assert_eq!(parse(src).plain_text(), src.trim_end(), "mangled {src:?}");
        }
    }

    #[test]
    fn emphasis_still_works_when_it_hugs_its_text() {
        assert_eq!(parse("*a*").plain_text(), "a");
        assert_eq!(parse("**a**").to_markdown(), "**a**");
        assert_eq!(parse("x *y z* w").to_markdown(), "x *y z* w");
    }

    #[test]
    fn parses_code_blocks_with_a_language() {
        let d = parse("```rust\nfn main() {}\nlet x = 1;\n```");
        let block = &d.0["content"][0];
        assert_eq!(block["type"], "codeBlock");
        assert_eq!(block["attrs"]["language"], "rust");
        assert_eq!(block["content"][0]["text"], "fn main() {}\nlet x = 1;");
    }

    #[test]
    fn markdown_inside_a_code_block_is_not_parsed() {
        let d = parse("```\n# not a heading\n**not bold**\n```");
        assert_eq!(d.0["content"][0]["type"], "codeBlock");
        assert_eq!(d.0["content"][0]["content"][0]["text"], "# not a heading\n**not bold**");
    }

    #[test]
    fn an_unclosed_code_fence_does_not_hang_or_panic() {
        let d = parse("```rust\nfn main() {}");
        assert_eq!(d.0["content"][0]["type"], "codeBlock");
    }

    #[test]
    fn parses_block_quotes_recursively() {
        let d = parse("> # Quoted heading\n> and a line\n\nafter");
        let q = &d.0["content"][0];
        assert_eq!(q["type"], "blockquote");
        assert_eq!(q["content"][0]["type"], "heading");
        assert_eq!(d.0["content"][1]["type"], "paragraph");
    }

    #[test]
    fn parses_bullet_ordered_and_task_lists() {
        let d = parse("- one\n- two\n\n1. first\n2. second\n\n- [x] done\n- [ ] todo");
        let c = d.0["content"].as_array().unwrap();
        assert_eq!(c[0]["type"], "bulletList");
        assert_eq!(c[0]["content"].as_array().unwrap().len(), 2);
        assert_eq!(c[1]["type"], "orderedList");
        assert_eq!(c[1]["attrs"]["start"], 1);
        assert_eq!(c[2]["type"], "taskList");
        assert_eq!(c[2]["content"][0]["attrs"]["checked"], true);
        assert_eq!(c[2]["content"][1]["attrs"]["checked"], false);
    }

    #[test]
    fn an_ordered_list_remembers_where_it_started() {
        let d = parse("5. five\n6. six");
        assert_eq!(d.0["content"][0]["attrs"]["start"], 5);
    }

    #[test]
    fn parses_nested_lists() {
        let d = parse("- outer\n  - inner\n- outer two");
        let list = &d.0["content"][0];
        assert_eq!(list["content"].as_array().unwrap().len(), 2);
        // The nested list is attached to the first item.
        assert_eq!(list["content"][0]["content"][1]["type"], "bulletList");
    }

    #[test]
    fn parses_media_references() {
        let blob = "a".repeat(64);
        let d = parse(&format!("![the ridge](media/{blob})"));
        let node = &d.0["content"][0];
        assert_eq!(node["type"], MEDIA_NODE);
        assert_eq!(node["attrs"]["blob"], blob);
        assert_eq!(node["attrs"]["caption"], "the ridge");
        assert_eq!(node["attrs"]["kind"], "image");
    }

    #[test]
    fn an_ordinary_image_link_is_not_treated_as_vault_media() {
        let d = parse("![a cat](https://example.com/cat.png)");
        assert_eq!(d.0["content"][0]["type"], "paragraph");
    }

    #[test]
    fn parses_thematic_breaks() {
        let d = parse("before\n\n---\n\nafter");
        assert_eq!(d.0["content"][1]["type"], "horizontalRule");
    }

    #[test]
    fn empty_input_yields_an_empty_document() {
        assert_eq!(parse(""), RichDoc::empty());
        assert_eq!(parse("\n\n\n"), RichDoc::empty());
    }

    #[test]
    fn round_trips_through_to_markdown() {
        // The important property: rendering a parsed document back to
        // Markdown reproduces the source.
        for src in [
            "# Heading\n\nA paragraph with **bold** and *italic* and `code`.",
            "- one\n- two",
            "1. first\n2. second",
            "- [x] done\n- [ ] todo",
            "> quoted text",
            "```rust\nfn main() {}\n```",
            "A [link](https://example.com) inline.",
            "---",
        ] {
            let parsed = parse(src);
            assert_eq!(parsed.to_markdown(), src, "round trip failed for {src:?}");
        }
    }

    #[test]
    fn unicode_content_is_preserved() {
        let src = "\u{65e5}\u{8a18}: **\u{6674}\u{308c}** \u{1f602}";
        let d = parse(src);
        assert!(text_of(&d).contains("\u{6674}\u{308c}"));
        assert_eq!(d.to_markdown(), src);
    }

    #[test]
    fn deeply_indented_lists_do_not_recurse_without_bound() {
        // Each line indents one step further, so without a depth limit this
        // is one stack frame per line.
        let src: String = (0..2000).map(|i| format!("{}- item {i}\n", " ".repeat(i))).collect();
        let d = parse(&src);
        assert!(d.plain_text().contains("item 0"));
        assert!(d.plain_text().contains("item 1999"), "content past the limit must survive");
    }

    #[test]
    fn a_short_line_at_the_nesting_limit_does_not_panic() {
        // Mixes marker kinds while descending, so a line is eventually
        // folded into a list whose marker is wider than the line itself.
        let src: String = (0..200)
            .map(|i| {
                let m = if i % 2 == 0 { "- [ ] x" } else { "- y" };
                format!("{}{m}\n", " ".repeat(i))
            })
            .collect();
        let _ = parse(&src);
    }

    #[test]
    fn deeply_pathological_input_terminates() {
        // Guards against the scanner failing to advance on odd delimiters.
        for src in ["*".repeat(500), "`".repeat(500), "[".repeat(500), "> ".repeat(500)] {
            let _ = parse(&src);
        }
    }
}
