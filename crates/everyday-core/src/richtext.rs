//! Rich text documents.
//!
//! The body of an entry is a [ProseMirror] document: a JSON tree of nodes and
//! marks. We keep it as opaque JSON rather than modelling every node type in
//! Rust, because the editor's schema will grow (tables, callouts, embeds) and
//! we do not want a Rust release to gate an editor feature.
//!
//! What the core *does* care about is two derived views, both computed here:
//!
//! * [`RichDoc::plain_text`] — what the search index and previews consume.
//! * [`RichDoc::blob_refs`] — which attachments the document references, so
//!   the vault can garbage-collect orphaned media.
//!
//! [ProseMirror]: https://prosemirror.net/docs/ref/#model.Document_Structure

use crate::id::BlobId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Documents nested deeper than this are treated as corrupt. Real documents
/// are a handful of levels deep; the limit exists so that a hand-crafted or
/// damaged file cannot blow the stack during traversal.
const MAX_DEPTH: usize = 64;

/// The node type used for embedded images, video, audio and files. Its
/// `attrs.blob` holds the [`BlobId`] of the payload.
pub const MEDIA_NODE: &str = "media";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RichDoc(pub Value);

impl Default for RichDoc {
    fn default() -> Self {
        Self::empty()
    }
}

impl RichDoc {
    /// An empty document containing a single empty paragraph, which is what
    /// ProseMirror requires for a `doc` with a `block+` content expression.
    pub fn empty() -> Self {
        RichDoc(json!({
            "type": "doc",
            "content": [{ "type": "paragraph" }]
        }))
    }

    pub fn is_empty(&self) -> bool {
        self.plain_text().trim().is_empty() && self.blob_refs().is_empty()
    }

    /// Build a document from plain text, one paragraph per line. Used by
    /// importers and by the Markdown backend when a file has no sidecar JSON.
    pub fn from_plain_text(text: &str) -> Self {
        let content: Vec<Value> = text
            .lines()
            .map(|line| {
                if line.trim().is_empty() {
                    json!({ "type": "paragraph" })
                } else {
                    json!({
                        "type": "paragraph",
                        "content": [{ "type": "text", "text": line }]
                    })
                }
            })
            .collect();
        if content.is_empty() {
            return Self::empty();
        }
        RichDoc(json!({ "type": "doc", "content": content }))
    }

    /// Build a document from Markdown.
    ///
    /// The inverse of [`RichDoc::to_markdown`], and it exists because
    /// something now *writes* entries rather than only reading them: the
    /// assistant is handed an entry as Markdown and hands one back, and a
    /// round trip through [`RichDoc::from_plain_text`] would turn every
    /// heading it read into a literal `## Heading` on the way home.
    ///
    /// Deliberately not a CommonMark implementation. It covers what
    /// `to_markdown` emits -- headings, the three list flavours, block
    /// quotes, fenced code, rules, and the inline marks the editor has --
    /// because closing that loop is the whole job. Anything it does not
    /// recognise stays as the text it was, which is the same thing the
    /// editor does when you paste it.
    ///
    /// Two known limits, both of which degrade to something readable rather
    /// than to something wrong: nested lists are flattened to one level, and
    /// a `media/...` link is left as an ordinary link rather than being
    /// resolved back to an attachment node. The second is why the assistant
    /// must not be the thing that rewrites an entry holding photographs --
    /// see the tool that calls this.
    pub fn from_markdown(text: &str) -> Self {
        let mut blocks: Vec<Value> = Vec::new();
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i];
            let trimmed = line.trim_start();

            // A fenced block swallows everything up to its closing fence, so
            // it is tested first: a `# comment` inside one is code, not a
            // heading.
            if let Some(lang) = trimmed.strip_prefix("```") {
                let lang = lang.trim().to_string();
                let mut body = Vec::new();
                i += 1;
                while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                    body.push(lines[i]);
                    i += 1;
                }
                // An unterminated fence runs to the end rather than failing:
                // half a code block is still what the author meant.
                i += usize::from(i < lines.len());
                let mut node = json!({ "type": "codeBlock" });
                if !body.is_empty() {
                    node["content"] = json!([{ "type": "text", "text": body.join("\n") }]);
                }
                if !lang.is_empty() {
                    node["attrs"] = json!({ "language": lang });
                }
                blocks.push(node);
                continue;
            }

            if trimmed.is_empty() {
                i += 1;
                continue;
            }

            // `---` is a rule; `- ` is a bullet. Checked in that order.
            if is_rule(trimmed) {
                blocks.push(json!({ "type": "horizontalRule" }));
                i += 1;
                continue;
            }

            if let Some((level, rest)) = heading(trimmed) {
                blocks.push(json!({
                    "type": "heading",
                    "attrs": { "level": level },
                    "content": inline_nodes(rest),
                }));
                i += 1;
                continue;
            }

            if trimmed.starts_with('>') {
                let mut inner = Vec::new();
                while i < lines.len() && lines[i].trim_start().starts_with('>') {
                    let l = lines[i].trim_start();
                    inner.push(l[1..].strip_prefix(' ').unwrap_or(&l[1..]).to_string());
                    i += 1;
                }
                let quoted = Self::from_markdown(&inner.join("\n"));
                blocks.push(json!({
                    "type": "blockquote",
                    "content": quoted.0.get("content").cloned().unwrap_or_else(|| json!([])),
                }));
                continue;
            }

            if let Some(first) = list_item(trimmed) {
                let (kind, node_type, item_type) = match first.marker {
                    Marker::Task(_) => ("task", "taskList", "taskItem"),
                    Marker::Ordered => ("ordered", "orderedList", "listItem"),
                    Marker::Bullet => ("bullet", "bulletList", "listItem"),
                };
                let start = first.start;
                let mut items = Vec::new();
                // One run of same-flavoured items. A bullet list interrupted
                // by a numbered one becomes two lists, which is what both
                // Markdown and the editor mean by it.
                while i < lines.len() {
                    let Some(item) = list_item(lines[i].trim_start()) else { break };
                    if item.marker.kind() != kind {
                        break;
                    }
                    let mut node = json!({
                        "type": item_type,
                        "content": [{ "type": "paragraph", "content": inline_nodes(&item.text) }],
                    });
                    if let Marker::Task(checked) = item.marker {
                        node["attrs"] = json!({ "checked": checked });
                    }
                    items.push(node);
                    i += 1;
                }
                let mut list = json!({ "type": node_type, "content": items });
                if node_type == "orderedList" && start != 1 {
                    list["attrs"] = json!({ "start": start });
                }
                blocks.push(list);
                continue;
            }

            // A paragraph runs until a blank line or the start of another
            // block, and its lines are joined by hard breaks -- which is what
            // the two trailing spaces `to_markdown` emits mean.
            let mut para = vec![line.trim_end()];
            i += 1;
            while i < lines.len() {
                let next = lines[i].trim_start();
                if next.is_empty() || starts_a_block(next) {
                    break;
                }
                para.push(lines[i].trim_end());
                i += 1;
            }
            let mut content = Vec::new();
            for (n, l) in para.iter().enumerate() {
                if n > 0 {
                    content.push(json!({ "type": "hardBreak" }));
                }
                content.extend(inline_nodes(l));
            }
            blocks.push(json!({ "type": "paragraph", "content": content }));
        }

        if blocks.is_empty() {
            return Self::empty();
        }
        RichDoc(json!({ "type": "doc", "content": blocks }))
    }

    /// Reject anything that is not a plausible ProseMirror `doc` node. The UI
    /// is trusted, but files on disk and imported data are not.
    pub fn validate(&self) -> crate::Result<()> {
        let obj = self
            .0
            .as_object()
            .ok_or_else(|| crate::Error::Invalid("document is not an object".into()))?;
        match obj.get("type").and_then(Value::as_str) {
            Some("doc") => {}
            other => {
                return Err(crate::Error::Invalid(format!(
                    "document root must have type \"doc\", found {other:?}"
                )));
            }
        }
        if depth(&self.0, 0) > MAX_DEPTH {
            return Err(crate::Error::Invalid(format!(
                "document nests deeper than the {MAX_DEPTH} level limit"
            )));
        }
        Ok(())
    }

    /// Flatten to text. Block-level nodes are separated by newlines so that
    /// "first line" heuristics and excerpts behave the way a reader expects.
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        collect_text(&self.0, &mut out, 0);
        // Collapse the runs of blank lines that empty paragraphs produce.
        let mut result = String::with_capacity(out.len());
        let mut blank_run = 0usize;
        for line in out.lines() {
            if line.trim().is_empty() {
                blank_run += 1;
                if blank_run > 1 {
                    continue;
                }
            } else {
                blank_run = 0;
            }
            result.push_str(line.trim_end());
            result.push('\n');
        }
        result.trim().to_string()
    }

    /// Every blob referenced by a media node, deduplicated.
    pub fn blob_refs(&self) -> BTreeSet<BlobId> {
        let mut out = BTreeSet::new();
        collect_blobs(&self.0, &mut out, 0);
        out
    }

    /// Render to Markdown. Used by the Markdown storage backend and by
    /// "export entry" in the UI. Unknown node types degrade to their text.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        markdown_node(&self.0, &mut out, 0, 0);
        // Normalise: at most one blank line between blocks, no trailing space.
        let mut result = String::new();
        let mut blank_run = 0usize;
        for line in out.lines() {
            if line.trim().is_empty() {
                blank_run += 1;
                if blank_run > 1 {
                    continue;
                }
            } else {
                blank_run = 0;
            }
            result.push_str(line.trim_end());
            result.push('\n');
        }
        result.trim().to_string()
    }
}

fn depth(v: &Value, d: usize) -> usize {
    if d > MAX_DEPTH {
        return d;
    }
    match v.get("content").and_then(Value::as_array) {
        Some(children) => children.iter().map(|c| depth(c, d + 1)).max().unwrap_or(d),
        None => d,
    }
}

fn is_block(ty: &str) -> bool {
    matches!(
        ty,
        "paragraph"
            | "heading"
            | "blockquote"
            | "codeBlock"
            | "listItem"
            | "taskItem"
            | "horizontalRule"
            | "tableRow"
            | MEDIA_NODE
    )
}

fn collect_text(node: &Value, out: &mut String, d: usize) {
    if d > MAX_DEPTH {
        return;
    }
    let ty = node.get("type").and_then(Value::as_str).unwrap_or("");

    if ty == "text" {
        if let Some(t) = node.get("text").and_then(Value::as_str) {
            out.push_str(t);
        }
        return;
    }
    if ty == "hardBreak" {
        out.push('\n');
        return;
    }
    // A media node contributes its caption so that "photo of the harbour"
    // is findable even though the pixels are not.
    if ty == MEDIA_NODE {
        if let Some(c) = node.get("attrs").and_then(|a| a.get("caption")).and_then(Value::as_str) {
            out.push_str(c);
        }
        out.push('\n');
        return;
    }

    if let Some(children) = node.get("content").and_then(Value::as_array) {
        for child in children {
            collect_text(child, out, d + 1);
        }
    }
    if is_block(ty) {
        out.push('\n');
    }
}

fn collect_blobs(node: &Value, out: &mut BTreeSet<BlobId>, d: usize) {
    if d > MAX_DEPTH {
        return;
    }
    if node.get("type").and_then(Value::as_str) == Some(MEDIA_NODE)
        && let Some(s) = node.get("attrs").and_then(|a| a.get("blob")).and_then(Value::as_str)
        && let Ok(id) = BlobId::parse(s)
    {
        out.insert(id);
    }
    if let Some(children) = node.get("content").and_then(Value::as_array) {
        for child in children {
            collect_blobs(child, out, d + 1);
        }
    }
}

/// Wrap `text` in the Markdown delimiters implied by its ProseMirror marks.
fn apply_marks(node: &Value, text: &str) -> String {
    let Some(marks) = node.get("marks").and_then(Value::as_array) else {
        return text.to_string();
    };
    let mut s = text.to_string();
    let mut link: Option<String> = None;
    for mark in marks {
        match mark.get("type").and_then(Value::as_str).unwrap_or("") {
            "bold" | "strong" => s = format!("**{s}**"),
            "italic" | "em" => s = format!("*{s}*"),
            "code" => s = format!("`{s}`"),
            "strike" => s = format!("~~{s}~~"),
            "highlight" => s = format!("=={s}=="),
            "underline" => s = format!("<u>{s}</u>"),
            "link" => {
                link = mark
                    .get("attrs")
                    .and_then(|a| a.get("href"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            _ => {}
        }
    }
    if let Some(href) = link {
        s = format!("[{s}]({href})");
    }
    s
}

fn children(node: &Value) -> &[Value] {
    node.get("content").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn inline_text(node: &Value, out: &mut String, d: usize) {
    if d > MAX_DEPTH {
        return;
    }
    let ty = node.get("type").and_then(Value::as_str).unwrap_or("");
    match ty {
        "text" => {
            let t = node.get("text").and_then(Value::as_str).unwrap_or("");
            out.push_str(&apply_marks(node, t));
        }
        "hardBreak" => out.push_str("  \n"),
        _ => {
            for c in children(node) {
                inline_text(c, out, d + 1);
            }
        }
    }
}

fn markdown_node(node: &Value, out: &mut String, d: usize, list_depth: usize) {
    if d > MAX_DEPTH {
        return;
    }
    let ty = node.get("type").and_then(Value::as_str).unwrap_or("");
    let attrs = node.get("attrs");

    match ty {
        "doc" => {
            for c in children(node) {
                markdown_node(c, out, d + 1, list_depth);
            }
        }
        "paragraph" => {
            let mut line = String::new();
            for c in children(node) {
                inline_text(c, &mut line, d + 1);
            }
            out.push_str(&line);
            out.push_str("\n\n");
        }
        "heading" => {
            let level =
                attrs.and_then(|a| a.get("level")).and_then(Value::as_u64).unwrap_or(1).clamp(1, 6);
            out.push_str(&"#".repeat(level as usize));
            out.push(' ');
            for c in children(node) {
                inline_text(c, out, d + 1);
            }
            out.push_str("\n\n");
        }
        "blockquote" => {
            let mut inner = String::new();
            for c in children(node) {
                markdown_node(c, &mut inner, d + 1, list_depth);
            }
            for line in inner.trim_end().lines() {
                out.push_str("> ");
                out.push_str(line);
                out.push('\n');
            }
            out.push('\n');
        }
        "codeBlock" => {
            let lang =
                attrs.and_then(|a| a.get("language")).and_then(Value::as_str).unwrap_or_default();
            out.push_str("```");
            out.push_str(lang);
            out.push('\n');
            let mut body = String::new();
            for c in children(node) {
                if let Some(t) = c.get("text").and_then(Value::as_str) {
                    body.push_str(t);
                }
            }
            out.push_str(&body);
            if !body.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```\n\n");
        }
        "bulletList" | "orderedList" | "taskList" => {
            let ordered = ty == "orderedList";
            let start =
                attrs.and_then(|a| a.get("start")).and_then(Value::as_u64).unwrap_or(1) as usize;
            for (i, item) in children(node).iter().enumerate() {
                let indent = "  ".repeat(list_depth);
                let checked =
                    item.get("attrs").and_then(|a| a.get("checked")).and_then(Value::as_bool);
                let bullet = match (ordered, checked) {
                    (_, Some(true)) => "- [x] ".to_string(),
                    (_, Some(false)) => "- [ ] ".to_string(),
                    (true, None) => format!("{}. ", start + i),
                    (false, None) => "- ".to_string(),
                };
                let mut inner = String::new();
                for c in children(item) {
                    markdown_node(c, &mut inner, d + 2, list_depth + 1);
                }
                let inner = inner.trim_end();
                for (n, line) in inner.lines().enumerate() {
                    out.push_str(&indent);
                    if n == 0 {
                        out.push_str(&bullet);
                    } else if !line.trim().is_empty() {
                        out.push_str(&" ".repeat(bullet.chars().count()));
                    }
                    out.push_str(line);
                    out.push('\n');
                }
            }
            out.push('\n');
        }
        "horizontalRule" => out.push_str("---\n\n"),
        MEDIA_NODE => {
            let a = attrs.cloned().unwrap_or(Value::Null);
            let caption = a.get("caption").and_then(Value::as_str).unwrap_or("");
            let blob = a.get("blob").and_then(Value::as_str).unwrap_or("");
            let kind = a.get("kind").and_then(Value::as_str).unwrap_or("file");
            let name = a.get("filename").and_then(Value::as_str).unwrap_or(blob);
            // Images use image syntax; other media degrade to a link so that
            // the Markdown stays readable in any other editor.
            if kind == "image" {
                out.push_str(&format!("![{caption}](media/{blob})\n\n"));
            } else {
                let label = if caption.is_empty() { name } else { caption };
                out.push_str(&format!("[{label}](media/{blob})\n\n"));
            }
        }
        "text" | "hardBreak" => {
            let mut line = String::new();
            inline_text(node, &mut line, d);
            out.push_str(&line);
        }
        _ => {
            // Unknown block: emit its text so nothing is silently lost.
            for c in children(node) {
                markdown_node(c, out, d + 1, list_depth);
            }
        }
    }
}

/// `---`, `***` or `___`: three or more of one character and nothing else.
fn is_rule(line: &str) -> bool {
    let line = line.trim();
    ["-", "*", "_"].iter().any(|c| line.len() >= 3 && line.chars().all(|ch| ch.to_string() == **c))
}

/// `## Heading` into its level and its text. Requires the space, so a `#tag`
/// at the start of a line stays a tag.
fn heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.len() - line.trim_start_matches('#').len();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    let text = rest.strip_prefix(' ')?;
    Some((hashes, text.trim()))
}

#[derive(Clone, Copy, PartialEq)]
enum Marker {
    Bullet,
    Ordered,
    Task(bool),
}

impl Marker {
    fn kind(self) -> &'static str {
        match self {
            Marker::Bullet => "bullet",
            Marker::Ordered => "ordered",
            Marker::Task(_) => "task",
        }
    }
}

struct ListItem {
    marker: Marker,
    /// Where an ordered list starts counting, so `3.` does not silently
    /// renumber to 1.
    start: u64,
    text: String,
}

fn list_item(line: &str) -> Option<ListItem> {
    // A task item is a bullet with a box, so it is tested first.
    for (prefix, checked) in [("- [x] ", true), ("- [X] ", true), ("- [ ] ", false)] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return Some(ListItem {
                marker: Marker::Task(checked),
                start: 1,
                text: rest.trim().to_string(),
            });
        }
    }
    for prefix in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return Some(ListItem {
                marker: Marker::Bullet,
                start: 1,
                text: rest.trim().to_string(),
            });
        }
    }
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits > 0 && digits <= 9 {
        let rest = &line[digits..];
        if let Some(text) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some(ListItem {
                marker: Marker::Ordered,
                start: line[..digits].parse().unwrap_or(1),
                text: text.trim().to_string(),
            });
        }
    }
    None
}

/// Would this line begin a new block? What stops a paragraph swallowing the
/// heading underneath it when the author forgot the blank line.
fn starts_a_block(line: &str) -> bool {
    line.starts_with("```")
        || line.starts_with('>')
        || is_rule(line)
        || heading(line).is_some()
        || list_item(line).is_some()
}

/// One line of Markdown into ProseMirror text nodes with marks.
///
/// Longest delimiters first, so `**bold**` is not read as two italics, and
/// code spans win over everything inside them because that is what a code
/// span is for.
fn inline_nodes(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut plain = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    let flush = |plain: &mut String, out: &mut Vec<Value>| {
        if !plain.is_empty() {
            out.push(json!({ "type": "text", "text": std::mem::take(plain) }));
        }
    };

    while i < chars.len() {
        // A link, which may wrap marked-up text of its own.
        if chars[i] == '['
            && let Some((label, href, next)) = link_at(&chars, i)
        {
            flush(&mut plain, &mut out);
            for mut node in inline_nodes(&label) {
                let marks = node
                    .get_mut("marks")
                    .and_then(Value::as_array_mut)
                    .map(std::mem::take)
                    .unwrap_or_default();
                let mut marks = marks;
                marks.push(json!({ "type": "link", "attrs": { "href": href } }));
                node["marks"] = Value::Array(marks);
                out.push(node);
            }
            i = next;
            continue;
        }

        let mut matched = false;
        for (delim, mark) in
            [("**", "bold"), ("~~", "strike"), ("==", "highlight"), ("`", "code"), ("*", "italic")]
        {
            let d: Vec<char> = delim.chars().collect();
            if chars[i..].starts_with(&d[..])
                && let Some(end) = closing_delimiter(&chars, i + d.len(), &d)
            {
                let inner: String = chars[i + d.len()..end].iter().collect();
                if inner.is_empty() {
                    continue;
                }
                flush(&mut plain, &mut out);
                // Code spans are literal all the way down; everything else
                // may nest, so its contents are parsed again.
                if mark == "code" {
                    out.push(json!({
                        "type": "text",
                        "text": inner,
                        "marks": [{ "type": "code" }],
                    }));
                } else {
                    for mut node in inline_nodes(&inner) {
                        let mut marks = node
                            .get_mut("marks")
                            .and_then(Value::as_array_mut)
                            .map(std::mem::take)
                            .unwrap_or_default();
                        marks.push(json!({ "type": mark }));
                        node["marks"] = Value::Array(marks);
                        out.push(node);
                    }
                }
                i = end + d.len();
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }

        plain.push(chars[i]);
        i += 1;
    }

    flush(&mut plain, &mut out);
    out
}

/// Where a run of emphasis actually closes.
///
/// Not simply the next matching delimiter, because of `***bold italic***`.
/// The three asterisks at the end are one italic closer followed by one bold
/// closer, so a `**` search that stopped at the first two would take the
/// italic's asterisk with it and leave a stray `*` in the text -- which is
/// exactly what it did.
///
/// So a candidate that lands inside a longer run of the same character is
/// pushed to the *end* of that run, leaving the earlier delimiters for the
/// shorter emphasis nested inside. The opening side needs no such care: it
/// consumes its two and the leftover starts the italic, which is the same
/// arrangement read from the other end.
fn closing_delimiter(chars: &[char], from: usize, delim: &[char]) -> Option<usize> {
    let at = find_closing(chars, from, delim)?;
    if delim.len() < 2 {
        return Some(at);
    }
    let c = delim[0];
    let run = chars[at..].iter().take_while(|ch| **ch == c).count();
    Some(at + run.saturating_sub(delim.len()))
}

/// The index of the next unescaped `delim` at or after `from`.
fn find_closing(chars: &[char], from: usize, delim: &[char]) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i..].starts_with(delim) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// `[label](href)` starting at `i`, and where it ends.
fn link_at(chars: &[char], i: usize) -> Option<(String, String, usize)> {
    let close = find_closing(chars, i + 1, &[']'])?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = find_closing(chars, close + 2, &[')'])?;
    let label: String = chars[i + 1..close].iter().collect();
    let href: String = chars[close + 2..end].iter().collect();
    if href.trim().is_empty() {
        return None;
    }
    Some((label, href.trim().to_string(), end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(content: Value) -> RichDoc {
        RichDoc(json!({ "type": "doc", "content": content }))
    }

    // ---- Markdown in ------------------------------------------------------

    /// The property that matters: what the assistant is handed is what it can
    /// hand back. Everything `to_markdown` emits must survive a round trip,
    /// or an agent that reads an entry and edits one line flattens the rest.
    #[test]
    fn markdown_survives_a_round_trip_through_the_document() {
        let source = "# Deck, day one\n\n\
                      Cut the **joists** and *ran out* of `screws`.\n\n\
                      ## What is left\n\n\
                      - Sand the rails\n\n\
                      - [ ] Order more screws\n\
                      - [x] Ring the timber yard\n\n\
                      1. Measure\n\
                      2. Cut\n\n\
                      > It always takes twice as long.\n\n\
                      ```rust\n\
                      let joists = 12;\n\
                      ```\n\n\
                      ---\n\n\
                      See [the plan](https://example.com/plan).";

        let doc = RichDoc::from_markdown(source);
        doc.validate().unwrap();
        assert_eq!(doc.to_markdown(), source, "Markdown should round-trip unchanged");
    }

    #[test]
    fn headings_lists_and_rules_become_real_nodes_rather_than_literal_text() {
        // The bug this replaces: `## Heading` stored as a paragraph whose
        // text begins with two hashes, rendered literally in the editor.
        let doc = RichDoc::from_markdown("## Plans\n\n- one\n- two");
        let blocks = doc.0["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "heading");
        assert_eq!(blocks[0]["attrs"]["level"], 2);
        assert_eq!(blocks[0]["content"][0]["text"], "Plans");
        assert_eq!(blocks[1]["type"], "bulletList");
        assert_eq!(blocks[1]["content"].as_array().unwrap().len(), 2);
        assert!(!doc.plain_text().contains('#'), "no hashes should survive as text");
    }

    #[test]
    fn a_hash_without_a_space_is_a_tag_and_not_a_heading() {
        let doc = RichDoc::from_markdown("#deck went well");
        assert_eq!(doc.0["content"][0]["type"], "paragraph");
        assert_eq!(doc.plain_text().trim(), "#deck went well");
    }

    #[test]
    fn a_fenced_block_is_literal_all_the_way_down() {
        // A heading inside code is code. Testing this because the block
        // scanner has to consume the fence before anything else looks at it.
        let doc = RichDoc::from_markdown("```\n# not a heading\n- not a list\n```");
        let blocks = doc.0["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "codeBlock");
        assert_eq!(blocks[0]["content"][0]["text"], "# not a heading\n- not a list");
    }

    #[test]
    fn an_unterminated_fence_runs_to_the_end_rather_than_being_lost() {
        let doc = RichDoc::from_markdown("```\nhalf a block");
        assert_eq!(doc.0["content"][0]["type"], "codeBlock");
        assert!(doc.plain_text().contains("half a block"));
    }

    #[test]
    fn a_task_list_keeps_which_boxes_were_ticked() {
        let doc = RichDoc::from_markdown("- [x] done\n- [ ] not done");
        let list = &doc.0["content"][0];
        assert_eq!(list["type"], "taskList");
        assert_eq!(list["content"][0]["attrs"]["checked"], true);
        assert_eq!(list["content"][1]["attrs"]["checked"], false);
    }

    #[test]
    fn an_ordered_list_keeps_where_it_started_counting() {
        let doc = RichDoc::from_markdown("3. three\n4. four");
        let list = &doc.0["content"][0];
        assert_eq!(list["type"], "orderedList");
        assert_eq!(list["attrs"]["start"], 3, "renumbering to 1 would change the meaning");
    }

    #[test]
    fn one_flavour_of_list_does_not_swallow_the_next() {
        let doc = RichDoc::from_markdown("- a\n1. b");
        let blocks = doc.0["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "bulletList");
        assert_eq!(blocks[1]["type"], "orderedList");
    }

    #[test]
    fn a_paragraph_stops_at_the_block_below_it_even_without_a_blank_line() {
        // Models forget the blank line constantly.
        let doc = RichDoc::from_markdown("some prose\n## A heading");
        let blocks = doc.0["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "paragraph");
        assert_eq!(blocks[1]["type"], "heading");
    }

    #[test]
    fn inline_marks_nest_and_links_carry_their_target() {
        let doc = RichDoc::from_markdown("a **bold *and italic*** and [a link](https://x.test)");
        let text = &doc.0["content"][0]["content"];

        let italic = text
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["text"] == "and italic")
            .expect("the nested run should exist");
        let marks: Vec<&str> = italic["marks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["type"].as_str().unwrap())
            .collect();
        assert!(marks.contains(&"bold") && marks.contains(&"italic"), "got {marks:?}");

        let link = text
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["text"] == "a link")
            .expect("the link text should exist");
        assert_eq!(link["marks"][0]["attrs"]["href"], "https://x.test");
    }

    #[test]
    fn a_code_span_is_not_reparsed_for_marks_inside_it() {
        let doc = RichDoc::from_markdown("run `a * b` now");
        let code = doc.0["content"][0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["marks"][0]["type"] == "code")
            .expect("a code span");
        assert_eq!(code["text"], "a * b", "the asterisk is code, not emphasis");
    }

    #[test]
    fn an_unclosed_delimiter_stays_the_text_it_was() {
        // Half a bold run is prose, not a parse failure.
        let doc = RichDoc::from_markdown("2 ** 3 is not bold");
        assert_eq!(doc.plain_text().trim(), "2 ** 3 is not bold");
    }

    #[test]
    fn empty_and_blank_markdown_give_a_valid_empty_document() {
        for source in ["", "   \n\n  "] {
            let doc = RichDoc::from_markdown(source);
            doc.validate().unwrap();
            assert!(doc.is_empty(), "{source:?} should give an empty document");
        }
    }

    #[test]
    fn empty_doc_is_valid_and_empty() {
        let d = RichDoc::empty();
        d.validate().unwrap();
        assert!(d.is_empty());
        assert_eq!(d.plain_text(), "");
    }

    #[test]
    fn plain_text_separates_blocks_with_newlines() {
        let d = doc(json!([
            { "type": "heading", "attrs": {"level": 1},
              "content": [{"type": "text", "text": "Tuesday"}] },
            { "type": "paragraph",
              "content": [{"type": "text", "text": "It rained."}] },
        ]));
        assert_eq!(d.plain_text(), "Tuesday\nIt rained.");
    }

    #[test]
    fn plain_text_includes_media_captions() {
        let d = doc(json!([
            { "type": MEDIA_NODE, "attrs": {
                "blob": BlobId::of(b"pic").to_hex(), "kind": "image",
                "caption": "the harbour at dusk" } },
        ]));
        assert_eq!(d.plain_text(), "the harbour at dusk");
    }

    #[test]
    fn blob_refs_finds_nested_media_and_dedups() {
        let a = BlobId::of(b"a");
        let d = doc(json!([
            { "type": MEDIA_NODE, "attrs": {"blob": a.to_hex(), "kind": "image"} },
            { "type": "blockquote", "content": [
                { "type": MEDIA_NODE, "attrs": {"blob": a.to_hex(), "kind": "image"} },
            ]},
        ]));
        assert_eq!(d.blob_refs(), [a].into_iter().collect());
    }

    #[test]
    fn blob_refs_ignores_malformed_ids() {
        let d = doc(json!([
            { "type": MEDIA_NODE, "attrs": {"blob": "not-a-blob", "kind": "image"} },
        ]));
        assert!(d.blob_refs().is_empty());
    }

    #[test]
    fn validate_rejects_non_doc_roots() {
        assert!(RichDoc(json!({"type": "paragraph"})).validate().is_err());
        assert!(RichDoc(json!("hello")).validate().is_err());
    }

    #[test]
    fn validate_rejects_pathologically_deep_documents() {
        let mut node = json!({"type": "paragraph"});
        for _ in 0..MAX_DEPTH + 10 {
            node = json!({"type": "blockquote", "content": [node]});
        }
        let d = doc(json!([node]));
        assert!(d.validate().is_err());
    }

    #[test]
    fn traversal_stops_at_the_depth_limit() {
        let mut node = json!({"type": "paragraph",
                              "content": [{"type": "text", "text": "buried"}]});
        for _ in 0..500 {
            node = json!({"type": "blockquote", "content": [node]});
        }
        let d = doc(json!([node]));
        // The text is past MAX_DEPTH, so traversal gives up rather than
        // recursing 500 frames deep.
        assert!(!d.plain_text().contains("buried"));
        assert!(d.validate().is_err());
    }

    #[test]
    fn parsing_rejects_deeply_nested_json_before_we_ever_see_it() {
        // The real ingress path for hostile documents is deserialization,
        // and serde_json enforces its own recursion limit there. This is
        // what actually protects the process, since `Value` also recurses
        // when it is dropped.
        let hostile = format!("{}{}", "[".repeat(4096), "]".repeat(4096));
        assert!(serde_json::from_str::<RichDoc>(&hostile).is_err());
    }

    #[test]
    fn markdown_renders_marks_and_links() {
        let d = doc(json!([
            { "type": "paragraph", "content": [
                {"type": "text", "text": "very ", },
                {"type": "text", "text": "bold", "marks": [{"type": "bold"}]},
                {"type": "text", "text": " and ", },
                {"type": "text", "text": "a link", "marks": [
                    {"type": "link", "attrs": {"href": "https://example.com"}}]},
            ]},
        ]));
        assert_eq!(d.to_markdown(), "very **bold** and [a link](https://example.com)");
    }

    #[test]
    fn markdown_renders_lists_and_tasks() {
        let d = doc(json!([
            { "type": "bulletList", "content": [
                {"type": "listItem", "content": [
                    {"type": "paragraph", "content": [{"type": "text", "text": "one"}]}]},
                {"type": "listItem", "content": [
                    {"type": "paragraph", "content": [{"type": "text", "text": "two"}]}]},
            ]},
            { "type": "taskList", "content": [
                {"type": "taskItem", "attrs": {"checked": true}, "content": [
                    {"type": "paragraph", "content": [{"type": "text", "text": "done"}]}]},
            ]},
        ]));
        assert_eq!(d.to_markdown(), "- one\n- two\n\n- [x] done");
    }

    #[test]
    fn markdown_renders_headings_quotes_and_code() {
        let d = doc(json!([
            {"type": "heading", "attrs": {"level": 2},
             "content": [{"type": "text", "text": "Notes"}]},
            {"type": "blockquote", "content": [
                {"type": "paragraph", "content": [{"type": "text", "text": "quoted"}]}]},
            {"type": "codeBlock", "attrs": {"language": "rust"},
             "content": [{"type": "text", "text": "fn main() {}"}]},
        ]));
        assert_eq!(d.to_markdown(), "## Notes\n\n> quoted\n\n```rust\nfn main() {}\n```");
    }

    #[test]
    fn from_plain_text_round_trips() {
        let d = RichDoc::from_plain_text("line one\n\nline two");
        assert_eq!(d.plain_text(), "line one\n\nline two");
        d.validate().unwrap();
    }
}
