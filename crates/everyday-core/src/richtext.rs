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
                let checked = item
                    .get("attrs")
                    .and_then(|a| a.get("checked"))
                    .and_then(Value::as_bool);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(content: Value) -> RichDoc {
        RichDoc(json!({ "type": "doc", "content": content }))
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
        assert_eq!(
            d.to_markdown(),
            "## Notes\n\n> quoted\n\n```rust\nfn main() {}\n```"
        );
    }

    #[test]
    fn from_plain_text_round_trips() {
        let d = RichDoc::from_plain_text("line one\n\nline two");
        assert_eq!(d.plain_text(), "line one\n\nline two");
        d.validate().unwrap();
    }
}
