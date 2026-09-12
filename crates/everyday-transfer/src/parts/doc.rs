//! What the journal and the notebook have in common: a rich document, its
//! pictures, and the front matter that carries what Markdown has no line for.
//!
//! # The sidecar, and why the Markdown still wins
//!
//! A journal entry is a ProseMirror document. Markdown can say most of what
//! one contains and not all of it — an embedded video, a table, a highlight —
//! so an export that was only Markdown would round-trip a diary slightly
//! smaller than it went in, for ever, silently.
//!
//! So each document is written twice: as the `.md` everything else reads, and
//! as the exact tree, in `.everyday/<id>.json` beside it. On the way back in
//! the sidecar is preferred — *unless the Markdown has been edited since*,
//! which is what the digest in the sidecar is for. Editing an exported entry
//! in a text editor is one of the two reasons anybody exports one; a sidecar
//! that silently overrode that edit would make the feature a trap.
//!
//! A folder with no `.everyday` at all — somebody's own notes, a folder
//! dragged out of another program, an archive rezipped by a tool that dropped
//! the dot-directory — imports from the Markdown alone. That path is not a
//! fallback that happens to work: it is the one that has to work, because it
//! is the only one another program can produce.

use crate::text::{Fields, FrontMatter};
use everyday_core::id::BlobId;
use everyday_core::model::{Attachment, MediaKind};
use everyday_core::purpose::Purpose;
use everyday_core::richtext::RichDoc;
use everyday_core::store::JournalStore;
use everyday_core::{GoalId, Result, RoleId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The folder the exact bodies live in, inside a part's own folder.
pub const SIDECAR: &str = ".everyday/";

/// What Markdown could not carry, kept beside it.
#[derive(Serialize, Deserialize)]
pub struct Sidecar {
    /// The digest of the Markdown body this was written from. When the `.md`
    /// no longer hashes to this, somebody has edited it and their edit is the
    /// document.
    pub digest: String,
    pub body: RichDoc,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

pub fn digest(markdown: &str) -> String {
    BlobId::of(markdown.as_bytes()).to_hex()
}

// ---- out ----------------------------------------------------------------

/// A document on its way into the archive.
pub struct Written {
    pub markdown: String,
    pub sidecar: String,
}

/// What a part hands over to be written.
///
/// A struct rather than five more arguments: an entry and a note differ in
/// exactly these four things and in nothing else, and a call site with eight
/// positional arguments is one where two of them get swapped.
pub struct Doc<'a> {
    /// The `# Heading` at the top, which is also the record's title.
    pub heading: &'a str,
    pub body: &'a RichDoc,
    pub attachments: &'a [Attachment],
    /// How many folders down the `.md` sits inside its part, so links to
    /// `media/` are relative and the file previews wherever it is opened. An
    /// entry lives in its journal's folder and is one down; a note is none.
    pub depth: usize,
}

/// Render a body and its attachments.
pub fn write(
    store: &dyn JournalStore,
    files: &mut crate::Files<'_>,
    opts: &crate::Options,
    front: FrontMatter,
    doc: Doc<'_>,
) -> Result<Written> {
    let Doc { heading, body, attachments, depth } = doc;
    // Two sources of pictures, and they overlap: the media nodes inside the
    // document, and the attachment list beside it. Exporting the union once
    // is what stops a photograph that is both being written twice.
    let mut links: BTreeMap<String, String> = BTreeMap::new();
    let up = "../".repeat(depth);
    let mut wanted: Vec<(BlobId, String)> =
        attachments.iter().map(|a| (a.blob, a.filename.clone())).collect();
    for blob in body.blob_refs() {
        if !wanted.iter().any(|(held, _)| *held == blob) {
            wanted.push((blob, blob.to_hex()[..8].to_string()));
        }
    }
    for (blob, filename) in &wanted {
        if let Some(path) = files.media(store, *blob, filename, opts)? {
            links.insert(blob.to_hex(), format!("{up}{path}"));
        }
    }

    let mut markdown = String::new();
    if !heading.trim().is_empty() {
        markdown.push_str(&format!("# {}\n\n", heading.trim()));
    }
    let rendered = relink(&body.to_markdown(), &links);
    markdown.push_str(&rendered);
    markdown.push('\n');

    // Attachments the document never embedded — a PDF dropped on an entry —
    // would otherwise be in the archive and mentioned nowhere.
    let loose: Vec<&Attachment> = attachments
        .iter()
        .filter(|a| !body.blob_refs().contains(&a.blob) && links.contains_key(&a.blob.to_hex()))
        .collect();
    if !loose.is_empty() {
        markdown.push_str("\n## Attached\n\n");
        for a in loose {
            let path = &links[&a.blob.to_hex()];
            let label = if a.caption.is_empty() { &a.filename } else { &a.caption };
            let bang = if matches!(a.kind, MediaKind::Image) { "!" } else { "" };
            markdown.push_str(&format!("{bang}[{label}]({path})\n"));
        }
    }

    let mut front = front;
    if !links.is_empty() {
        let mut paths: Vec<String> =
            attachments.iter().filter_map(|a| links.get(&a.blob.to_hex()).cloned()).collect();
        paths.dedup();
        front.list("attachments", &paths);
    }

    let sidecar = serde_json::to_string(&Sidecar {
        digest: digest(&markdown),
        body: body.clone(),
        attachments: attachments.to_vec(),
    })
    .unwrap_or_default();
    Ok(Written { markdown: format!("{}{markdown}", front.render()), sidecar })
}

/// Point every `media/<blob>` link at the file that was actually written.
///
/// [`RichDoc::to_markdown`] writes the blob's address as the path, because in
/// the vault the address *is* the file. In an archive the file has a name a
/// person can read, and a link to a 64-character hex string that is not there
/// would be worse than no link.
fn relink(markdown: &str, links: &BTreeMap<String, String>) -> String {
    let mut out = markdown.to_string();
    for (blob, path) in links {
        out = out.replace(&format!("](media/{blob})"), &format!("]({path})"));
    }
    out
}

// ---- back in ------------------------------------------------------------

/// A document read out of the archive.
pub struct Read {
    pub fields: Fields,
    pub title: String,
    pub body: RichDoc,
    pub attachments: Vec<Attachment>,
}

/// Read one `.md`, using its sidecar when the Markdown has not been touched.
///
/// `name` is the file's path within the part, which is also how the title is
/// recovered for a file that has no front matter and no heading — a note
/// called `Shopping.md` should come in called "Shopping" rather than
/// "Untitled".
pub fn read(
    store: &dyn JournalStore,
    src: &crate::Part<'_>,
    name: &str,
    source: &str,
) -> Result<Read> {
    let (fields, body_text) = crate::text::split_front_matter(source);
    let title = pick_title(&fields, body_text, name);
    let body_text = strip_heading(body_text, &title);

    let sidecar = fields
        .get("id")
        .and_then(|id| src.get(&format!("{SIDECAR}{id}.json")))
        .and_then(|bytes| serde_json::from_slice::<Sidecar>(bytes).ok())
        .filter(|s| s.digest == digest(&strip_front_matter(source)));

    if let Some(sidecar) = sidecar {
        return Ok(Read { fields, title, body: sidecar.body, attachments: sidecar.attachments });
    }

    // No sidecar, or one the Markdown has outgrown. The pictures are found by
    // following the links in the text, which is the only way another program
    // could have expressed them.
    let (body, attachments) = from_markdown(store, src, name, body_text)?;
    Ok(Read { fields, title, body, attachments })
}

fn strip_front_matter(source: &str) -> String {
    let (_, body) = crate::text::split_front_matter(source);
    body.to_string()
}

fn pick_title(fields: &Fields, body: &str, name: &str) -> String {
    if let Some(title) = fields.get("title").filter(|t| !t.trim().is_empty()) {
        return title.to_string();
    }
    if let Some(heading) =
        body.lines().find(|l| !l.trim().is_empty()).and_then(|l| l.strip_prefix("# "))
    {
        return heading.trim().to_string();
    }
    let stem = name.rsplit('/').next().unwrap_or(name);
    stem.trim_end_matches(".md").replace(['-', '_'], " ").trim().to_string()
}

/// Drop the `# Title` the exporter wrote, so a round trip does not leave the
/// title in the document *and* in the field beside it.
fn strip_heading<'a>(body: &'a str, title: &str) -> &'a str {
    let trimmed = body.trim_start();
    let Some(rest) = trimmed.strip_prefix("# ") else { return body };
    let (line, after) = rest.split_once('\n').unwrap_or((rest, ""));
    if line.trim() == title.trim() { after.trim_start_matches('\n') } else { body }
}

/// Turn Markdown into a document, storing any pictures it links to.
///
/// Media lines are lifted out first and the prose between them is parsed by
/// the core's own reader, so what comes back is a document with real media
/// nodes in the places the pictures were -- not a paragraph containing the
/// text `![](media/photo.jpg)`.
fn from_markdown(
    store: &dyn JournalStore,
    src: &crate::Part<'_>,
    name: &str,
    markdown: &str,
) -> Result<(RichDoc, Vec<Attachment>)> {
    let folder = name.rsplit_once('/').map_or("", |(dir, _)| dir);
    let mut content: Vec<Value> = Vec::new();
    let mut attachments: Vec<Attachment> = Vec::new();
    let mut prose = String::new();

    let flush = |prose: &mut String, content: &mut Vec<Value>| {
        if !prose.trim().is_empty()
            && let Some(nodes) =
                RichDoc::from_markdown(prose).0.get("content").and_then(Value::as_array)
        {
            content.extend(nodes.iter().cloned());
        }
        prose.clear();
    };

    for line in markdown.lines() {
        match media_link(line) {
            Some((caption, path)) => {
                match attach(store, src, folder, path, caption) {
                    Some(attachment) => {
                        flush(&mut prose, &mut content);
                        content.push(json!({
                            "type": everyday_core::richtext::MEDIA_NODE,
                            "attrs": {
                                "blob": attachment.blob.to_hex(),
                                "kind": kind_name(attachment.kind),
                                "mime": attachment.mime,
                                "filename": attachment.filename,
                                "caption": attachment.caption,
                            }
                        }));
                        attachments.push(attachment);
                    }
                    // A link to a file the archive does not hold is left as
                    // the text it was. Turning it into a broken media node
                    // would put an empty frame in somebody's diary.
                    None => prose.push_str(&format!("{line}\n")),
                }
            }
            None => prose.push_str(&format!("{line}\n")),
        }
    }
    flush(&mut prose, &mut content);

    if content.is_empty() {
        return Ok((RichDoc::empty(), attachments));
    }
    Ok((RichDoc(json!({ "type": "doc", "content": content })), attachments))
}

/// `![caption](path)` or `[caption](path)` alone on a line, and nothing else.
///
/// Only a whole line counts. An image in the middle of a sentence is an
/// inline image, and lifting it out would reorder somebody's prose.
fn media_link(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    let inner = line.strip_prefix("![").or_else(|| line.strip_prefix('['))?;
    let (caption, rest) = inner.split_once("](")?;
    let path = rest.strip_suffix(')')?;
    (!path.contains(')') && !caption.contains(']')).then_some((caption, path))
}

/// Store the file a link points at, as an attachment.
fn attach(
    store: &dyn JournalStore,
    src: &crate::Part<'_>,
    folder: &str,
    path: &str,
    caption: &str,
) -> Option<Attachment> {
    // Links are relative to the file that holds them, which for an entry is a
    // folder down. `..` is resolved here rather than by asking the archive
    // for a path with `..` in it, which it refuses on principle.
    let mut segments: Vec<&str> = folder.split('/').filter(|s| !s.is_empty()).collect();
    for segment in path.split('/') {
        match segment {
            ".." => {
                segments.pop()?;
            }
            "." | "" => {}
            other => segments.push(other),
        }
    }
    let resolved = segments.join("/");
    let bytes = src.get(&resolved)?;
    let mime = everyday_core::media::sniff_mime(bytes);
    let blob = store.put_blob(bytes).ok()?;

    // The exporter prefixed the name with eight digits of the blob's address
    // to keep two `IMG_0042.jpg` apart. Taking it off again is what makes a
    // round trip give back the name the photograph had.
    let file = resolved.rsplit('/').next().unwrap_or(&resolved);
    let filename = match file.split_once('-') {
        Some((prefix, rest))
            if prefix.len() == 8 && prefix.chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            rest
        }
        _ => file,
    };
    Some(Attachment {
        blob,
        kind: MediaKind::from_mime(mime),
        mime: mime.to_string(),
        filename: filename.to_string(),
        byte_len: bytes.len() as u64,
        width: None,
        height: None,
        duration_ms: None,
        caption: caption.to_string(),
    })
}

fn kind_name(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Image => "image",
        MediaKind::Video => "video",
        MediaKind::Audio => "audio",
        MediaKind::File => "file",
    }
}

// ---- purpose ------------------------------------------------------------

/// A purpose, written the way a person can read and an importer can parse:
/// `goal:<id>` or `role:<id>`.
///
/// The name is not written beside it. A goal's title is in the purpose part's
/// own files, and repeating it here would be a second copy to disagree with
/// the first the moment somebody renames a goal.
pub fn purpose_text(purpose: Option<&Purpose>) -> String {
    match purpose {
        Some(Purpose::Goal { id }) => format!("goal:{id}"),
        Some(Purpose::Role { id }) => format!("role:{id}"),
        None => String::new(),
    }
}

pub fn parse_purpose(text: &str) -> Option<Purpose> {
    let (kind, id) = text.trim().split_once(':')?;
    match kind {
        "goal" => Some(Purpose::Goal { id: GoalId::parse(id).ok()? }),
        "role" => Some(Purpose::Role { id: RoleId::parse(id).ok()? }),
        _ => None,
    }
}

/// A timestamp as ISO-8601, which is what both a person and a parser expect.
pub fn stamp(at: jiff::Timestamp) -> String {
    at.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_media_line_is_recognised_only_when_it_is_the_whole_line() {
        assert_eq!(media_link("![a photo](media/x.jpg)"), Some(("a photo", "media/x.jpg")));
        assert_eq!(
            media_link("  [notes.pdf](../media/y.pdf) "),
            Some(("notes.pdf", "../media/y.pdf"))
        );
        assert!(media_link("see ![a photo](media/x.jpg) here").is_none());
        assert!(media_link("plain text").is_none());
    }

    #[test]
    fn the_heading_the_exporter_wrote_is_not_left_in_the_body() {
        assert_eq!(strip_heading("# Monday\n\nIt rained.\n", "Monday"), "It rained.\n");
        // A heading that is not the title belongs to the author.
        assert_eq!(strip_heading("# Other\n\ntext\n", "Monday"), "# Other\n\ntext\n");
    }

    #[test]
    fn a_title_is_found_wherever_it_is() {
        let (fields, body) = crate::text::split_front_matter("---\ntitle: Named\n---\nx\n");
        assert_eq!(pick_title(&fields, body, "a.md"), "Named");
        let (fields, body) = crate::text::split_front_matter("# From the heading\n\nx\n");
        assert_eq!(pick_title(&fields, body, "a.md"), "From the heading");
        let (fields, body) = crate::text::split_front_matter("just text\n");
        assert_eq!(pick_title(&fields, body, "notes/shopping-list.md"), "shopping list");
    }

    #[test]
    fn a_purpose_round_trips() {
        let goal = Purpose::Goal { id: GoalId::new() };
        assert_eq!(parse_purpose(&purpose_text(Some(&goal))), Some(goal));
        let role = Purpose::Role { id: RoleId::new() };
        assert_eq!(parse_purpose(&purpose_text(Some(&role))), Some(role));
        assert!(parse_purpose("").is_none());
        assert!(parse_purpose("goal:not-an-id").is_none());
    }
}
