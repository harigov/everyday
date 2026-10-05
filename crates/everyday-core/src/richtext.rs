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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// quotes, fenced code, rules, pipe tables, and the inline marks the
    /// editor has --
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
            let ticks = trimmed.len() - trimmed.trim_start_matches('`').len();
            if ticks >= 3 {
                let lang = trimmed[ticks..].trim().to_string();
                let mut body = Vec::new();
                i += 1;
                // Closed by a run at least as long as the one that opened
                // it, so code with a fence of its own in it is written --
                // and read back -- inside a longer one.
                while i < lines.len() && !closes_fence(lines[i], ticks) {
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

            // A pipe table is a header row with the `|---|` rule under it.
            // The rule is what makes it one: a `|` in prose is just a `|`.
            // A list item or a heading with a pipe in it is still a list item
            // or a heading, which is also how `markdown.ts` reads it.
            if !starts_a_block(trimmed)
                && let Some(width) = table_header(trimmed, lines.get(i + 1).copied())
            {
                let aligns = column_aligns(lines[i + 1]);
                let mut rows = vec![table_row(trimmed, width, "tableHeader", &aligns)];
                i += 2;
                while i < lines.len()
                    && lines[i].contains('|')
                    && !lines[i].trim().is_empty()
                    && !starts_a_block(lines[i].trim_start())
                {
                    rows.push(table_row(lines[i], width, "tableCell", &aligns));
                    i += 1;
                }
                blocks.push(json!({ "type": "table", "content": rows }));
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
                    let mut content = inline_nodes(&item.text);
                    i += 1;
                    // A line indented under an item, and not an item itself,
                    // is the item's text carrying on after a line break.
                    while i < lines.len()
                        && lines[i].starts_with([' ', '\t'])
                        && !lines[i].trim().is_empty()
                        && !starts_a_block(lines[i].trim_start())
                    {
                        content.push(json!({ "type": "hardBreak" }));
                        content.extend(inline_nodes(lines[i].trim()));
                        i += 1;
                    }
                    let mut node = json!({
                        "type": item_type,
                        "content": [{ "type": "paragraph", "content": content }],
                    });
                    if let Marker::Task(checked) = item.marker {
                        node["attrs"] = json!({ "checked": checked });
                    }
                    items.push(node);
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
                if next.is_empty()
                    || starts_a_block(next)
                    || table_header(next, lines.get(i + 1).copied()).is_some()
                {
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

    /// Render to Markdown. Used by the Markdown storage backend, by "export
    /// entry" in the UI and wherever the assistant reads a document. Unknown
    /// node types degrade to their text.
    pub fn to_markdown(&self) -> String {
        md_blocks(children(&self.0), 1, false)
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

// ---- Markdown out ----------------------------------------------------------
//
// Mirrored line for line by `ui/src/lib/markdown-copy.ts`, which writes the
// same Markdown onto the clipboard when text is copied out of the editor. The
// cases in `tests/fixtures/markdown.json` are checked against both, so an
// exported note and a copied one cannot drift apart.

/// A line break inside a paragraph: two spaces and a newline, which is what
/// Markdown reads as a break rather than as a space.
const HARD_BREAK: &str = "  \n";

fn children(node: &Value) -> &[Value] {
    node.get("content").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn node_type(node: &Value) -> &str {
    node.get("type").and_then(Value::as_str).unwrap_or("")
}

fn attr<'a>(node: &'a Value, key: &str) -> Option<&'a Value> {
    node.get("attrs").and_then(|a| a.get(key))
}

fn is_list(ty: &str) -> bool {
    matches!(ty, "bulletList" | "orderedList" | "taskList")
}

/// Blocks one after another, with a blank line between them.
///
/// Inside a list item a list follows the line above it directly, which is
/// what keeps a nested list tight rather than spreading every item apart.
fn md_blocks(nodes: &[Value], d: usize, in_item: bool) -> String {
    let mut out = String::new();
    if d > MAX_DEPTH {
        return out;
    }
    for node in nodes {
        let md = md_block(node, d);
        if md.trim().is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str(if in_item && is_list(node_type(node)) { "\n" } else { "\n\n" });
        }
        out.push_str(&md);
    }
    out
}

fn md_block(node: &Value, d: usize) -> String {
    if d > MAX_DEPTH {
        return String::new();
    }
    let ty = node_type(node);
    match ty {
        "paragraph" => md_inline(children(node), d + 1).trim_end().to_string(),
        "heading" => {
            let level = attr(node, "level").and_then(Value::as_u64).unwrap_or(1).clamp(1, 6);
            // A heading is one line in Markdown; a break inside it would end it.
            let text = md_inline(children(node), d + 1).replace(HARD_BREAK, " ");
            format!("{} {}", "#".repeat(level as usize), text.trim_end())
        }
        "blockquote" => md_blocks(children(node), d + 1, false)
            .split('\n')
            .map(|line| if line.is_empty() { ">".to_string() } else { format!("> {line}") })
            .collect::<Vec<_>>()
            .join("\n"),
        "codeBlock" => {
            let body: String = children(node)
                .iter()
                .filter_map(|c| c.get("text").and_then(Value::as_str))
                .collect();
            // One backtick more than the longest run inside, so a fence in
            // the code does not end the block early.
            let fence = "`".repeat(3.max(longest_backtick_run(&body) + 1));
            let language = attr(node, "language").and_then(Value::as_str).unwrap_or_default();
            if body.is_empty() {
                format!("{fence}{language}\n{fence}")
            } else {
                format!("{fence}{language}\n{body}\n{fence}")
            }
        }
        "bulletList" | "orderedList" | "taskList" => md_list(node, d),
        "table" => md_table(node, d),
        "horizontalRule" => "---".to_string(),
        MEDIA_NODE => {
            let text = |key| attr(node, key).and_then(Value::as_str).unwrap_or_default();
            let (caption, blob) = (text("caption"), text("blob"));
            // Images use image syntax; other media degrade to a link so that
            // the Markdown stays readable in any other editor.
            if text("kind") == "image" {
                format!("![{caption}](media/{blob})")
            } else {
                let filename = text("filename");
                let label = [caption, filename, blob].into_iter().find(|s| !s.is_empty());
                format!("[{}](media/{blob})", label.unwrap_or_default())
            }
        }
        "text" | "hardBreak" => md_inline(std::slice::from_ref(node), d),
        // Unknown block: its text, so nothing is silently lost.
        _ => md_blocks(children(node), d + 1, false),
    }
}

fn md_list(list: &Value, d: usize) -> String {
    let ordered = node_type(list) == "orderedList";
    let start = attr(list, "start").and_then(Value::as_u64).unwrap_or(1);
    children(list)
        .iter()
        .enumerate()
        .map(|(n, item)| {
            let marker = if ordered { format!("{}. ", start + n as u64) } else { "- ".into() };
            let check = match attr(item, "checked").and_then(Value::as_bool) {
                Some(true) => "[x] ",
                Some(false) => "[ ] ",
                None => "",
            };
            // What follows the first line lines up under the item's text,
            // which is where Markdown looks for what belongs to it. Under
            // the marker, not the box: as far as Markdown knows, the box is
            // part of the text.
            let pad = " ".repeat(marker.len());
            let body = md_blocks(children(item), d + 2, true);
            let mut lines = body.split('\n');
            let mut out = match lines.next().unwrap_or_default() {
                "" => format!("{marker}{check}").trim_end().to_string(),
                first => format!("{marker}{check}{first}"),
            };
            for line in lines {
                out.push('\n');
                if !line.is_empty() {
                    out.push_str(&pad);
                    out.push_str(line);
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A pipe table. Markdown has no table without a header row, so the first
/// row is the header whether or not the editor drew it as one.
fn md_table(table: &Value, d: usize) -> String {
    let (rows, aligns) = table_grid(table, d);
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if width == 0 {
        return String::new();
    }
    let line = |row: &Vec<String>| {
        let mut out = String::from("|");
        for c in 0..width {
            out.push(' ');
            out.push_str(row.get(c).map(String::as_str).unwrap_or(""));
            out.push_str(" |");
        }
        out
    };
    let rule: String = (0..width)
        .map(|c| match aligns.get(c).copied().flatten() {
            Some("left") => " :--- |",
            Some("center") => " :---: |",
            Some("right") => " ---: |",
            _ => " --- |",
        })
        .collect();
    let mut lines = vec![line(&rows[0]), format!("|{rule}")];
    lines.extend(rows[1..].iter().map(line));
    lines.join("\n")
}

fn longest_backtick_run(text: &str) -> usize {
    text.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

/// Marks written as a pair of delimiters round their text, in the order they
/// are opened when several start together: a link outermost, so its label
/// can carry marks of its own, and bold before italic, so `***` opens as
/// `**` then `*` -- the reading the parser below expects.
const DELIMITED: [(&str, &str, &str); 6] = [
    ("link", "[", ""),
    ("bold", "**", "**"),
    ("italic", "*", "*"),
    ("strike", "~~", "~~"),
    ("highlight", "==", "=="),
    ("underline", "<u>", "</u>"),
];

fn delimiters(mark: &Value) -> Option<(usize, &'static str, &'static str)> {
    // `strong` and `em` are what other ProseMirror schemas call them, and an
    // imported document may still say so; the editor's own never does.
    let ty = match node_type(mark) {
        "strong" => "bold",
        "em" => "italic",
        ty => ty,
    };
    DELIMITED.iter().position(|(t, ..)| *t == ty).map(|rank| {
        let (_, open, close) = DELIMITED[rank];
        (rank, open, close)
    })
}

fn same_mark(a: &Value, b: &Value) -> bool {
    node_type(a) == node_type(b) && a.get("attrs") == b.get("attrs")
}

/// A paragraph's text with its marks written as delimiters.
///
/// A mark is opened where it starts and closed where it ends, rather than
/// round each text node: a run of bold with an italic word in the middle is
/// three nodes, and wrapping each in its own marks wrote `**a *****b***** c**`.
fn md_inline(nodes: &[Value], d: usize) -> String {
    /// The marks open at the end of `out`, outermost first, and where each began.
    struct Open<'a> {
        mark: &'a Value,
        at: usize,
    }

    // `**word **` is not bold: a delimiter after a space cannot close. The
    // space goes outside instead, where it reads the same.
    fn close_to(out: &mut String, open: &mut Vec<Open<'_>>, depth: usize) {
        if open.len() <= depth {
            return;
        }
        let trailing = out.split_off(out.trim_end().len());
        while open.len() > depth {
            let Some(Open { mark, at }) = open.pop() else { break };
            if node_type(mark) == "link" {
                close_link(out, mark, at);
            } else if let Some((_, _, close)) = delimiters(mark) {
                out.push_str(close);
            }
        }
        out.push_str(&trailing);
    }

    let mut out = String::new();
    if d > MAX_DEPTH {
        return out;
    }
    let mut open: Vec<Open<'_>> = Vec::new();
    for node in nodes {
        match node_type(node) {
            // Every mark is closed before a break and opened again after it,
            // so each line reads on its own -- the way the parser below, and
            // most others, take a paragraph.
            "hardBreak" => {
                close_to(&mut out, &mut open, 0);
                out.push_str(HARD_BREAK);
            }
            "text" => {
                let text = node.get("text").and_then(Value::as_str).unwrap_or_default();
                // Whitespace alone neither opens nor closes anything.
                if text.trim().is_empty() {
                    out.push_str(text);
                    continue;
                }
                let all: &[Value] =
                    node.get("marks").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
                let mut marks: Vec<&Value> =
                    all.iter().filter(|m| delimiters(m).is_some()).collect();
                marks.sort_by_key(|m| delimiters(m).map(|(rank, ..)| rank));
                let mut keep = 0;
                while keep < open.len() && marks.iter().any(|m| same_mark(m, open[keep].mark)) {
                    keep += 1;
                }
                close_to(&mut out, &mut open, keep);

                let code = all.iter().any(|m| node_type(m) == "code");
                let lead = if code { "" } else { &text[..text.len() - text.trim_start().len()] };
                out.push_str(lead);
                for mark in marks {
                    if open.iter().any(|o| same_mark(o.mark, mark)) {
                        continue;
                    }
                    open.push(Open { mark, at: out.len() });
                    out.push_str(delimiters(mark).map(|(_, open, _)| open).unwrap_or_default());
                }
                if code {
                    out.push_str(&code_span(text));
                } else {
                    out.push_str(&text[lead.len()..]);
                }
            }
            _ => {
                close_to(&mut out, &mut open, 0);
                out.push_str(&md_inline(children(node), d + 1));
            }
        }
    }
    close_to(&mut out, &mut open, 0);
    // A break at the very end of a paragraph breaks nothing.
    while out.ends_with(HARD_BREAK) {
        out.truncate(out.len() - HARD_BREAK.len());
    }
    out
}

/// `[label](href)`, or `<href>` for an address that is its own label.
///
/// The address is written with its spaces and parentheses escaped, because
/// either would end it early.
fn close_link(out: &mut String, mark: &Value, at: usize) {
    let href = attr(mark, "href").and_then(Value::as_str).unwrap_or_default();
    let label = &out[at + 1..];
    if href.is_empty() {
        out.remove(at);
    } else if label == href && is_autolink(href) {
        out.replace_range(at..at + 1, "<");
        out.push('>');
    } else {
        out.push_str("](");
        out.push_str(&href.replace(' ', "%20").replace('(', "%28").replace(')', "%29"));
        out.push(')');
    }
}

/// Can `href` be written as `<href>` and read back as the same link?
fn is_autolink(href: &str) -> bool {
    ["http://", "https://", "mailto:"].iter().any(|scheme| href.starts_with(scheme))
        && !href.chars().any(|c| c.is_whitespace() || c == '<' || c == '>')
}

/// Fenced with one backtick more than the longest run inside, so none ends
/// it, and padded with a space where Markdown would otherwise take one off
/// or run a backtick at the edge into the fence.
fn code_span(text: &str) -> String {
    let ticks = "`".repeat(longest_backtick_run(text) + 1);
    let spaced = text.starts_with(' ') && text.ends_with(' ') && !text.trim().is_empty();
    let pad = if text.starts_with('`') || text.ends_with('`') || spaced { " " } else { "" };
    format!("{ticks}{pad}{text}{pad}{ticks}")
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

/// The widest a merged cell is taken to be. Real tables are a handful of
/// columns; the cap is so a damaged `colspan` cannot ask for a billion.
const MAX_SPAN: u64 = 1000;

/// A table's cells laid out on the grid they occupy.
///
/// Markdown has no merged cells, so a cell spanning several keeps its text
/// in the first slot it covers and leaves the rest empty. Writing the cells
/// out one after another instead would put every cell after a merged one in
/// the wrong column, and the round trip would save it there.
///
/// Each column's alignment comes back beside it: the first cell in the
/// column that has one decides, since Markdown aligns columns, not cells.
fn table_grid(table: &Value, d: usize) -> (Vec<Vec<String>>, Vec<Option<&str>>) {
    let rows = children(table);
    let mut grid: Vec<Vec<Option<String>>> = vec![Vec::new(); rows.len()];
    let mut aligns: Vec<Option<&str>> = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        let mut c = 0;
        for cell in children(row) {
            // Past any slot a cell above has already reached down into.
            while grid[r].get(c).is_some_and(Option::is_some) {
                c += 1;
            }
            let span = |key: &str| {
                cell.get("attrs")
                    .and_then(|a| a.get(key))
                    .and_then(Value::as_u64)
                    .unwrap_or(1)
                    .clamp(1, MAX_SPAN) as usize
            };
            let (across, down) = (span("colspan"), span("rowspan"));
            if aligns.len() <= c {
                aligns.resize(c + 1, None);
            }
            if aligns[c].is_none() {
                aligns[c] = attr(cell, "align")
                    .and_then(Value::as_str)
                    .filter(|a| matches!(*a, "left" | "center" | "right"));
            }
            let mut text = Some(table_cell(cell, d + 2));
            for slots in grid.iter_mut().skip(r).take(down) {
                if slots.len() < c + across {
                    slots.resize(c + across, None);
                }
                for slot in &mut slots[c..c + across] {
                    *slot = Some(text.take().unwrap_or_default());
                }
            }
            c += across;
        }
    }
    let grid = grid.into_iter().map(|row| row.into_iter().map(Option::unwrap_or_default).collect());
    (grid.collect(), aligns)
}

/// A cell's contents on one line, which is all a Markdown table has room
/// for. Each block in it is written as Markdown -- so a photograph in a cell
/// keeps its `![caption](media/...)` rather than vanishing -- and the lines
/// are run together with a space, with every `|` escaped so it does not end
/// the cell early.
fn table_cell(cell: &Value, d: usize) -> String {
    md_blocks(children(cell), d + 1, false)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

/// The cells of one table row, split on the pipes that are not escaped.
fn table_cells(line: &str) -> Vec<String> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let line =
        if line.ends_with('|') && !line.ends_with("\\|") { &line[..line.len() - 1] } else { line };
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cell.push('|');
                chars.next();
            }
            '|' => cells.push(std::mem::take(&mut cell).trim().to_string()),
            _ => cell.push(c),
        }
    }
    cells.push(cell.trim().to_string());
    cells
}

/// How many columns the table starting at `line` has, if it is one: the line
/// has a pipe, and the line under it is a rule with a `---` per column.
fn table_header(line: &str, next: Option<&str>) -> Option<usize> {
    let rule = next?.trim();
    if !line.contains('|') || !rule.contains('-') {
        return None;
    }
    let width = table_cells(line).len();
    let specs = table_cells(rule);
    let is_rule = specs.iter().all(|spec| {
        let dashes = spec.trim_start_matches(':').trim_end_matches(':');
        !dashes.is_empty() && dashes.chars().all(|c| c == '-')
    });
    (is_rule && specs.len() == width).then_some(width)
}

/// Each column's alignment, from the colons on the rule under the header:
/// `:---` left, `---:` right, `:---:` centre, and none for a bare `---`.
fn column_aligns(rule: &str) -> Vec<Option<&'static str>> {
    table_cells(rule)
        .iter()
        .map(|spec| match (spec.starts_with(':'), spec.ends_with(':')) {
            (true, true) => Some("center"),
            (true, false) => Some("left"),
            (false, true) => Some("right"),
            (false, false) => None,
        })
        .collect()
}

/// One row of a table, padded or cut to the header's width -- the editor's
/// tables are rectangular, and Markdown's are only by convention.
fn table_row(line: &str, width: usize, cell_type: &str, aligns: &[Option<&str>]) -> Value {
    let mut cells = table_cells(line);
    cells.resize(width, String::new());
    let content: Vec<Value> = cells
        .iter()
        .enumerate()
        .map(|(c, text)| {
            let mut paragraph = json!({ "type": "paragraph" });
            if !text.is_empty() {
                paragraph["content"] = json!(inline_nodes(text));
            }
            let mut cell = json!({ "type": cell_type, "content": [paragraph] });
            if let Some(align) = aligns.get(c).copied().flatten() {
                cell["attrs"] = json!({ "align": align });
            }
            cell
        })
        .collect();
    json!({ "type": "tableRow", "content": content })
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
            out.extend(with_mark(
                inline_nodes(&label),
                json!({ "type": "link", "attrs": { "href": href } }),
            ));
            i = next;
            continue;
        }

        // A code span: a run of backticks, closed by a run of the same
        // length, so a span can hold a shorter run of its own. Literal all
        // the way down -- nothing inside is read for marks.
        if chars[i] == '`' {
            let ticks = run_length(&chars, i, '`');
            let Some(end) = code_span_end(&chars, i + ticks, ticks) else {
                plain.extend(&chars[i..i + ticks]);
                i += ticks;
                continue;
            };
            let mut inner: String = chars[i + ticks..end].iter().collect();
            // One space either side is padding, there to keep a backtick at
            // the edge clear of the fence; it is not part of the code.
            if inner.len() > 2
                && inner.starts_with(' ')
                && inner.ends_with(' ')
                && !inner.trim().is_empty()
            {
                inner = inner[1..inner.len() - 1].to_string();
            }
            if !inner.is_empty() {
                flush(&mut plain, &mut out);
                out.push(json!({ "type": "text", "text": inner, "marks": [{ "type": "code" }] }));
            }
            i = end + ticks;
            continue;
        }

        // `<https://...>`, an address that is its own label, and `<u>`, the
        // one tag Markdown is written with here because it has no underline.
        if chars[i] == '<' {
            if let Some((href, next)) = autolink_at(&chars, i) {
                flush(&mut plain, &mut out);
                out.push(json!({
                    "type": "text",
                    "text": href,
                    "marks": [{ "type": "link", "attrs": { "href": href } }],
                }));
                i = next;
                continue;
            }
            let (open, close) = (['<', 'u', '>'], ['<', '/', 'u', '>']);
            if chars[i..].starts_with(&open)
                && let Some(end) = find_closing(&chars, i + open.len(), &close)
                && end > i + open.len()
            {
                flush(&mut plain, &mut out);
                let inner: String = chars[i + open.len()..end].iter().collect();
                out.extend(with_mark(inline_nodes(&inner), json!({ "type": "underline" })));
                i = end + close.len();
                continue;
            }
        }

        let mut matched = false;
        for (delim, mark) in
            [("**", "bold"), ("~~", "strike"), ("==", "highlight"), ("*", "italic")]
        {
            let d: Vec<char> = delim.chars().collect();
            if !chars[i..].starts_with(&d[..]) {
                continue;
            }
            let end = if mark == "italic" {
                italic_close(&chars, i + 1)
            } else {
                closing_delimiter(&chars, i + d.len(), &d)
            };
            let Some(end) = end else { continue };
            let inner: String = chars[i + d.len()..end].iter().collect();
            if inner.is_empty() {
                continue;
            }
            flush(&mut plain, &mut out);
            // Everything but a code span may nest, so its contents are
            // parsed again.
            out.extend(with_mark(inline_nodes(&inner), json!({ "type": mark })));
            i = end + d.len();
            matched = true;
            break;
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

/// `nodes` with `mark` added to each: the contents of a delimited run.
fn with_mark(nodes: Vec<Value>, mark: Value) -> impl Iterator<Item = Value> {
    nodes.into_iter().map(move |mut node| {
        let mut marks = node
            .get_mut("marks")
            .and_then(Value::as_array_mut)
            .map(std::mem::take)
            .unwrap_or_default();
        marks.push(mark.clone());
        node["marks"] = Value::Array(marks);
        node
    })
}

/// How many `c` in a row start at `at`.
fn run_length(chars: &[char], at: usize, c: char) -> usize {
    chars[at..].iter().take_while(|ch| **ch == c).count()
}

/// Where the code span whose opening run of `ticks` backticks ends just
/// before `from` closes: the next run of exactly that many.
fn code_span_end(chars: &[char], from: usize, ticks: usize) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == '`' {
            let run = run_length(chars, i, '`');
            if run == ticks {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

/// Where a single-`*` emphasis closes: the next lone `*`, stepping over the
/// `**` pairs of any bold nested inside it -- `*a **b** c*` is italic round
/// a bold word, not an italic `a ` and a stray asterisk. A run of three or
/// more closes the bold and the italic together, the italic's last.
fn italic_close(chars: &[char], from: usize) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 2,
            '*' => match run_length(chars, i, '*') {
                1 => return Some(i),
                2 => i += 2,
                run => return Some(i + run - 1),
            },
            _ => i += 1,
        }
    }
    None
}

/// `<https://...>` starting at `i`: the address, and where it ends.
fn autolink_at(chars: &[char], i: usize) -> Option<(String, usize)> {
    let len = chars[i + 1..].iter().position(|c| *c == '>' || *c == '<' || c.is_whitespace())?;
    if chars[i + 1 + len] != '>' {
        return None;
    }
    let href: String = chars[i + 1..i + 1 + len].iter().collect();
    is_autolink(&href).then_some((href, i + len + 2))
}

/// Does `line` close a fence opened with `ticks` backticks?
fn closes_fence(line: &str, ticks: usize) -> bool {
    let line = line.trim();
    line.len() >= ticks && line.chars().all(|c| c == '`')
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
    fn a_pipe_table_becomes_a_table_with_its_first_row_as_the_header() {
        let doc = RichDoc::from_markdown(
            "Miles this week:\n| Day | Miles |\n|:---|---:|\n| Mon | **3** |\n| Tue |\n",
        );
        let blocks = doc.0["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "paragraph", "the table interrupts the paragraph");
        let rows = blocks[1]["content"].as_array().unwrap();
        assert_eq!(blocks[1]["type"], "table");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["content"][0]["type"], "tableHeader");
        assert_eq!(rows[1]["content"][0]["type"], "tableCell");
        assert_eq!(rows[1]["content"][1]["content"][0]["content"][0]["marks"][0]["type"], "bold");
        // A short row is padded, because the editor's tables are rectangular.
        assert_eq!(rows[2]["content"].as_array().unwrap().len(), 2);
        assert_eq!(rows[2]["content"][1]["content"][0], json!({ "type": "paragraph" }));
    }

    #[test]
    fn a_pipe_without_a_rule_under_it_is_prose() {
        // The rule has to have a column for every header cell, or it is a
        // rule under a line that happens to have a pipe in it.
        for text in ["either this | or that", "a | b\n---", "a | b\n| --- | --- | --- |"] {
            let doc = RichDoc::from_markdown(text);
            assert!(
                doc.0["content"].as_array().unwrap().iter().all(|b| b["type"] != "table"),
                "{text:?} is not a table"
            );
        }
    }

    #[test]
    fn a_table_survives_a_round_trip_through_markdown() {
        let cell = |ty: &str, text: &str| {
            json!({ "type": ty, "content": [
                { "type": "paragraph", "content": [{ "type": "text", "text": text }] }
            ]})
        };
        let d = doc(json!([{ "type": "table", "content": [
            { "type": "tableRow", "content": [cell("tableHeader", "Day"), cell("tableHeader", "Note")] },
            { "type": "tableRow", "content": [cell("tableCell", "Mon"), cell("tableCell", "this | that")] },
        ]}]));
        let md = d.to_markdown();
        assert_eq!(md, "| Day | Note |\n| --- | --- |\n| Mon | this \\| that |");
        assert_eq!(RichDoc::from_markdown(&md), d);
        assert_eq!(d.plain_text(), "Day\nNote\n\nMon\nthis | that");
    }

    #[test]
    fn a_list_item_or_heading_with_a_pipe_in_it_is_not_a_table_row() {
        let blocks = RichDoc::from_markdown("- a | b\n| --- | --- |").0["content"].clone();
        assert_eq!(blocks[0]["type"], "bulletList", "a list item does not head a table");

        let doc = RichDoc::from_markdown("| x | y |\n|---|---|\n| 1 | 2 |\n- note | aside");
        let blocks = doc.0["content"].as_array().unwrap();
        assert_eq!(blocks[0]["content"].as_array().unwrap().len(), 2, "header and one row");
        assert_eq!(blocks[1]["type"], "bulletList", "and the list under it is kept");
    }

    #[test]
    fn a_merged_cell_keeps_every_column_after_it_in_its_own_column() {
        let cell = |text: &str, attrs: Value| {
            json!({ "type": "tableCell", "attrs": attrs, "content": [
                { "type": "paragraph", "content": [{ "type": "text", "text": text }] }
            ]})
        };
        let one = json!({});
        let d = doc(json!([{ "type": "table", "content": [
            { "type": "tableRow", "content": [cell("a", one.clone()), cell("b", one.clone()), cell("c", one.clone())] },
            { "type": "tableRow", "content": [cell("wide", json!({ "colspan": 2 })), cell("c1", one.clone())] },
            { "type": "tableRow", "content": [cell("tall", json!({ "rowspan": 2 })), cell("b2", one.clone()), cell("c2", one.clone())] },
            { "type": "tableRow", "content": [cell("b3", one.clone()), cell("c3", one.clone())] },
        ]}]));
        assert_eq!(
            d.to_markdown(),
            "| a | b | c |\n| --- | --- | --- |\n| wide |  | c1 |\n| tall | b2 | c2 |\n|  | b3 | c3 |"
        );
    }

    #[test]
    fn a_photograph_in_a_cell_is_written_out_rather_than_lost() {
        let d = doc(json!([{ "type": "table", "content": [
            { "type": "tableRow", "content": [{ "type": "tableHeader", "content": [
                { "type": "paragraph", "content": [{ "type": "text", "text": "Before" }] },
                { "type": "media", "attrs": { "blob": "abc", "kind": "image", "caption": "the fence" } },
            ]}]},
        ]}]));
        assert_eq!(d.to_markdown(), "| Before ![the fence](media/abc) |\n| --- |");
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

    /// The cases `ui/scripts/markdown-copy.test.mjs` checks the clipboard
    /// against: an exported note and a copied one are the same Markdown.
    #[test]
    fn markdown_is_written_as_the_cases_shared_with_the_clipboard() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/markdown.json")).unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let want = case["markdown"].as_str().unwrap();
            assert_eq!(doc(case["doc"].clone()).to_markdown(), want, "{name}");
            if case["readsBack"] != false {
                let back = RichDoc::from_markdown(want);
                back.validate().unwrap();
                assert_eq!(back.to_markdown(), want, "{name}: should read back as itself");
            }
        }
    }

    /// Each run of text in the first block, with the marks on it.
    fn runs(d: &RichDoc) -> Vec<(String, Vec<String>)> {
        children(&d.0["content"][0])
            .iter()
            .map(|n| {
                let mut marks: Vec<String> = n["marks"]
                    .as_array()
                    .map(|ms| ms.iter().map(|m| m["type"].as_str().unwrap().to_string()).collect())
                    .unwrap_or_default();
                marks.sort();
                (n["text"].as_str().unwrap_or("\n").to_string(), marks)
            })
            .collect()
    }

    fn run(text: &str, marks: &[&str]) -> (String, Vec<String>) {
        (text.to_string(), marks.iter().map(|m| m.to_string()).collect())
    }

    #[test]
    fn italic_round_bold_reads_as_italic_round_bold() {
        // The italic used to close on the first asterisk of the bold.
        let d = RichDoc::from_markdown("*a **b** c*");
        assert_eq!(
            runs(&d),
            [run("a ", &["italic"]), run("b", &["bold", "italic"]), run(" c", &["italic"])]
        );
        assert_eq!(runs(&RichDoc::from_markdown("*a**b***")).len(), 2);
    }

    #[test]
    fn a_code_span_can_hold_a_backtick() {
        let d = RichDoc::from_markdown("``a`b`` and `` `x ``");
        assert_eq!(runs(&d), [run("a`b", &["code"]), run(" and ", &[]), run("`x", &["code"])]);
        // An unmatched run is the text it was.
        assert_eq!(RichDoc::from_markdown("a `` b").plain_text(), "a `` b");
    }

    #[test]
    fn a_fence_is_closed_only_by_one_at_least_as_long() {
        let d = RichDoc::from_markdown("````\n```\ninside\n```\n````\nafter");
        let blocks = d.0["content"].as_array().unwrap();
        assert_eq!(blocks[0]["content"][0]["text"], "```\ninside\n```");
        assert_eq!(blocks[1]["type"], "paragraph");
    }

    #[test]
    fn an_address_in_angle_brackets_is_a_link() {
        let d = RichDoc::from_markdown("see <https://x.test/a_b> or <mailto:me@x.test>");
        let nodes = children(&d.0["content"][0]);
        assert_eq!(nodes[1]["text"], "https://x.test/a_b");
        assert_eq!(nodes[1]["marks"][0]["attrs"]["href"], "https://x.test/a_b");
        assert_eq!(nodes[3]["marks"][0]["attrs"]["href"], "mailto:me@x.test");
        // Only an address: a comparison is not a link, and nor is a script.
        for prose in ["a <b> c", "if a < b > c", "<javascript:alert(1)>"] {
            assert_eq!(RichDoc::from_markdown(prose).plain_text(), prose);
        }
    }

    #[test]
    fn underline_is_read_back_from_the_tag_it_is_written_as() {
        let d = RichDoc::from_markdown("an <u>**underlined**</u> word");
        assert_eq!(
            runs(&d),
            [run("an ", &[]), run("underlined", &["bold", "underline"]), run(" word", &[])]
        );
    }

    #[test]
    fn a_line_indented_under_a_list_item_carries_it_on() {
        let d = RichDoc::from_markdown("- first\n  second\n- third");
        let items = d.0["content"][0]["content"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        let first = children(&items[0]["content"][0]);
        assert_eq!(first[1]["type"], "hardBreak");
        assert_eq!(first[2]["text"], "second");
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
