//! Looking at an attachment without saving it first.
//!
//! Which parts can be looked at is decided here, once, from what a part says
//! it is and what it is called ([`kind_of`]), and handed to the interface on
//! every `MailAttachment` (`domains::mail`), so the chips and the viewer can
//! never disagree about it.
//!
//! A picture, a film, a recording and a PDF are drawn by the window itself,
//! from the part's own bytes at `everyday://mail/part/…`. Everything else
//! that is text at heart -- a web page, Markdown, a CSV, a forwarded message,
//! source code, an SVG -- is drawn here by [`page`] into the same kind of
//! document a message is (`mailview::page`), and shown in the same sandboxed
//! frame under the same CSP: a stranger's file is trusted no more than a
//! stranger's message. HTML goes through the very sanitiser a message's HTML
//! does, in its offline mode, so nothing in the file can fetch anything;
//! everything else is escaped text, or a picture in a `data:` address.
//!
//! A `text/*` part is not the bytes that were sent: mail-parser decodes every
//! one into UTF-8 at sync, by its `charset` or, lacking one, lossily. So a
//! part stored as text is read as UTF-8 here, whatever a `<meta charset>` in
//! it still claims, and only a part stored as it came -- a `.txt` or `.html`
//! some mailer called `application/octet-stream` -- has its encoding guessed.
//!
//! Office documents (Word, Excel, PowerPoint, OpenDocument) are left out on
//! purpose. Drawing one faithfully takes LibreOffice or a large parser this
//! application does not carry, and a poor imitation is worse than the file
//! itself, which is one click away on the chip.

use base64::Engine;
use encoding_rs::Encoding;
use everyday_core::Vault;
use everyday_core::id::MailMessageId;
use everyday_core::mail::PartRef;
use everyday_mail::sanitize::{Rewrite, sanitize};
use serde::Serialize;

use crate::error::{CommandError, CommandResult, codes};
use crate::mailview::{escape_html, linkify};

/// How an attachment can be looked at, if it can.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PreviewKind {
    /// An `<img>`: PNG, JPEG, GIF, WebP, BMP, AVIF or an icon. Not HEIC or
    /// TIFF, which WebKitGTK cannot draw.
    Image,
    /// A `<video>`. Whether it plays is up to the webview's codecs; the
    /// viewer says so when it does not.
    Video,
    /// An `<audio>`, on the same terms.
    Audio,
    /// Drawn by pdf.js, in the window.
    Pdf,
    /// Drawn here by [`page`], into a sandboxed frame.
    Page,
}

/// Whether, and how, `part` can be looked at.
///
/// A part's declared type decides when it names something showable, its
/// filename's extension otherwise -- plenty of mailers send every file as
/// `application/octet-stream`. An extension that names an office document or
/// an archive vetoes both, so a `.docx` some mailer called `text/plain` is not
/// drawn as a page of binary. So does a picture's, a film's or a PDF's on a
/// part declared as text: sync read its bytes as text (see the module docs),
/// so it is neither what its name says nor anything worth reading.
pub fn kind_of(part: &PartRef) -> Option<PreviewKind> {
    let extension = extension(part.filename.as_deref());
    if extension.as_deref().is_some_and(|e| OPAQUE_EXTENSIONS.contains(&e)) {
        return None;
    }
    let by_extension = extension.as_deref().and_then(kind_by_extension);
    if stored_as_text(part) && by_extension.is_some_and(|k| k != PreviewKind::Page) {
        return None;
    }
    kind_by_type(&essence(&part.mime_type)).or(by_extension)
}

/// The type a film's or a recording's bytes are handed to `<video>` or
/// `<audio>` as -- what the part declares when that is one this file knows,
/// what its name says otherwise. `None` for anything else.
pub fn media_type(part: &PartRef) -> Option<String> {
    let kind = kind_of(part)?;
    if !matches!(kind, PreviewKind::Video | PreviewKind::Audio) {
        return None;
    }
    let declared = essence(&part.mime_type);
    if kind_by_type(&declared) == Some(kind) {
        return Some(declared);
    }
    let extension = extension(part.filename.as_deref())?;
    MEDIA_EXTENSIONS.iter().find(|(e, _)| *e == extension).map(|(_, t)| (*t).to_string())
}

/// Every `text/*` part, which sync stored decoded to UTF-8.
fn stored_as_text(part: &PartRef) -> bool {
    essence(&part.mime_type).starts_with("text/")
}

fn kind_by_type(mime: &str) -> Option<PreviewKind> {
    use PreviewKind::*;
    Some(match mime {
        "image/png"
        | "image/jpeg"
        | "image/jpg"
        | "image/pjpeg"
        | "image/gif"
        | "image/webp"
        | "image/bmp"
        | "image/x-bmp"
        | "image/x-ms-bmp"
        | "image/avif"
        | "image/x-icon"
        | "image/vnd.microsoft.icon" => Image,
        "video/mp4" | "video/webm" | "video/ogg" | "video/quicktime" | "video/x-m4v" => Video,
        "audio/mpeg" | "audio/mp3" | "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "audio/aac"
        | "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave" | "audio/ogg"
        | "audio/opus" | "audio/flac" | "audio/x-flac" | "audio/webm" => Audio,
        "application/pdf" | "application/x-pdf" => Pdf,
        "image/svg+xml"
        | "message/rfc822"
        | "application/xhtml+xml"
        | "application/json"
        | "application/xml"
        | "application/yaml"
        | "application/x-yaml"
        | "application/toml"
        | "application/javascript"
        | "application/x-javascript"
        | "application/x-sh"
        | "application/sql" => Page,
        // Rich text is text in name only: a page of control words.
        "text/rtf" | "text/richtext" => return None,
        m if m.starts_with("text/") => Page,
        _ => return None,
    })
}

fn kind_by_extension(extension: &str) -> Option<PreviewKind> {
    use PreviewKind::*;
    if let Some((_, media)) = MEDIA_EXTENSIONS.iter().find(|(e, _)| *e == extension) {
        return Some(if media.starts_with("video/") { Video } else { Audio });
    }
    Some(match extension {
        "png" | "jpg" | "jpeg" | "jpe" | "jfif" | "gif" | "webp" | "bmp" | "avif" | "ico" => Image,
        "pdf" => Pdf,
        e if PAGE_EXTENSIONS.contains(&e) || CODE_EXTENSIONS.contains(&e) => Page,
        _ => return None,
    })
}

/// Films and recordings by extension, with the type their bytes are played as.
const MEDIA_EXTENSIONS: &[(&str, &str)] = &[
    ("mp4", "video/mp4"),
    ("m4v", "video/mp4"),
    ("webm", "video/webm"),
    ("ogv", "video/ogg"),
    ("mov", "video/quicktime"),
    ("mp3", "audio/mpeg"),
    ("m4a", "audio/mp4"),
    ("aac", "audio/aac"),
    ("wav", "audio/wav"),
    ("oga", "audio/ogg"),
    ("ogg", "audio/ogg"),
    ("opus", "audio/ogg"),
    ("flac", "audio/flac"),
];

/// Office documents and archives: never a preview, whatever the part claims.
const OPAQUE_EXTENSIONS: &[&str] = &[
    "doc", "docx", "docm", "dot", "dotx", "xls", "xlsx", "xlsm", "xlt", "xltx", "ppt", "pptx",
    "pptm", "pps", "ppsx", "odt", "ods", "odp", "odg", "rtf", "pages", "numbers", "key", "zip",
    "gz", "tgz", "7z", "rar", "tar", "exe", "msi", "dmg", "apk",
];

/// Text with a shape [`page`] draws as more than plain text.
const PAGE_EXTENSIONS: &[&str] = &[
    "html", "htm", "xhtml", "md", "markdown", "mdown", "mkd", "csv", "tsv", "tab", "svg", "eml",
    "txt", "text",
];

/// Text to be read in a fixed-width face: data, configuration, logs, code.
/// Not `ts`, which is as often a video as TypeScript.
const CODE_EXTENSIONS: &[&str] = &[
    "json",
    "xml",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "log",
    "ics",
    "vcf",
    "diff",
    "patch",
    "sql",
    "js",
    "mjs",
    "cjs",
    "tsx",
    "jsx",
    "py",
    "rb",
    "rs",
    "go",
    "java",
    "kt",
    "kts",
    "scala",
    "c",
    "h",
    "cc",
    "cpp",
    "cxx",
    "hpp",
    "cs",
    "swift",
    "m",
    "mm",
    "php",
    "pl",
    "lua",
    "r",
    "dart",
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "bat",
    "css",
    "scss",
    "sass",
    "less",
    "vue",
    "svelte",
    "tex",
    "properties",
    "gradle",
    "env",
];

/// `type/subtype`, lower-cased, without parameters.
fn essence(mime: &str) -> String {
    mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase()
}

/// The extension of `filename`, lower-cased. A name that is all extension
/// (`.env`) counts, a name with no dot does not.
fn extension(filename: Option<&str>) -> Option<String> {
    let name = filename?.trim().rsplit(['/', '\\']).next()?;
    let (_, extension) = name.rsplit_once('.')?;
    (!extension.is_empty()).then(|| extension.to_ascii_lowercase())
}

// ---- the page ----------------------------------------------------------------

/// The most of a text file a page shows. More than anyone reads in a preview,
/// and few enough characters for a frame to lay out at once.
const MOST_TEXT: usize = 2 * 1024 * 1024;

/// The most rows and columns of a CSV a page shows.
const MOST_ROWS: usize = 2_000;
const MOST_COLUMNS: usize = 100;

/// Past this, an HTML page, an SVG or a message is not drawn at all: unlike
/// text, none of them can be cut short and still be what they were.
const MOST_WHOLE: usize = 20 * 1024 * 1024;

/// What a page adds to `mailview`'s base style. Greys are see-through, so they
/// sit right on the light page and the dark one alike: the interface themes
/// the page after it arrives (`applyBodyTheme`), and nothing here knows which.
const STYLE: &str = r#"
.doc { max-width: 46em; margin: 0 auto; }
.note { color: #8b857c; font-size: 13px; margin: 0 0 12px; }
pre.code, .doc code, .doc pre {
  font-family: ui-monospace, 'SF Mono', 'JetBrains Mono', 'DejaVu Sans Mono', Menlo, Consolas,
    monospace;
  font-size: 13px;
}
pre.code { line-height: 1.5; overflow-wrap: anywhere; }
.doc code { background: rgba(128, 128, 128, 0.14); border-radius: 4px; padding: 1px 4px; }
.doc pre {
  background: rgba(128, 128, 128, 0.14);
  border-radius: 6px;
  padding: 10px 12px;
  white-space: pre;
  overflow-x: auto;
}
.doc pre code { background: none; padding: 0; }
.doc blockquote { margin: 0 0 1em; padding-left: 12px; border-left: 3px solid rgba(128, 128, 128, 0.4); }
.doc img { max-width: 100%; }
.doc table, table.sheet { border-collapse: collapse; }
.doc th, .doc td, .sheet th, .sheet td {
  border: 1px solid rgba(128, 128, 128, 0.35);
  padding: 4px 8px;
  text-align: left;
  vertical-align: top;
}
table.sheet { font-size: 13px; }
.sheet td { white-space: pre-wrap; }
.sheet th { background: rgba(128, 128, 128, 0.14); font-weight: 600; }
.picture { display: flex; justify-content: center; }
.picture img { background: #ffffff; }
table.headers { border-collapse: collapse; margin: 0 0 16px; font-size: 14px; }
.headers th { color: #8b857c; font-weight: normal; text-align: left; padding: 0 12px 2px 0; vertical-align: top; }
.headers td { padding: 0 0 2px; }
hr.split { border: none; border-top: 1px solid rgba(128, 128, 128, 0.35); margin: 0 0 16px; }
"#;

/// Part `index` of `message_id`, drawn as a complete document for the
/// sandboxed frame -- for a part [`kind_of`] calls [`PreviewKind::Page`].
pub fn page(vault: &Vault, message_id: MailMessageId, index: usize) -> CommandResult<String> {
    let body = vault.body(message_id)?;
    let part = body
        .parts
        .get(index)
        .ok_or_else(|| CommandError::new(codes::NOT_FOUND, "no such part of this message"))?;
    if kind_of(part) != Some(PreviewKind::Page) {
        return Err(CommandError::new(codes::INVALID, "this attachment cannot be shown as a page"));
    }
    let blob = part.blob.ok_or_else(|| {
        CommandError::new(codes::NOT_FOUND, "that attachment has not been downloaded yet")
    })?;
    // Only as much as will be shown: the first stretch of a text file, all of
    // one that cannot be cut short, and none of one too large to draw at all.
    let total = vault.with_store(|s| s.blob_len(blob))?;
    let wanted = match format_of(part).whole() {
        true if total > MOST_WHOLE as u64 => 0,
        true => total,
        false => total.min(MOST_TEXT as u64),
    };
    let bytes = vault.with_store(|s| s.get_blob_range(blob, 0, wanted))?;
    Ok(draw(part, &bytes, total))
}

/// What a page is drawn as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Html,
    Markdown,
    /// Rows of fields, split on this character.
    Table(char),
    Svg,
    /// A forwarded message, `.eml`.
    Message,
    /// Text in a fixed-width face.
    Code,
    /// Text as a person wrote it, links and all.
    Text,
}

impl Format {
    /// Unlike text, none of these can be cut short and still be what they
    /// were: drawn whole, or not at all.
    fn whole(self) -> bool {
        matches!(self, Format::Html | Format::Svg | Format::Message)
    }
}

/// The extension decides when it says something, since `text/plain` is what
/// many mailers call any text at all; the declared type otherwise.
fn format_of(part: &PartRef) -> Format {
    let by_extension = match extension(part.filename.as_deref()).as_deref() {
        Some("html" | "htm" | "xhtml") => Some(Format::Html),
        Some("md" | "markdown" | "mdown" | "mkd") => Some(Format::Markdown),
        Some("csv") => Some(Format::Table(',')),
        Some("tsv" | "tab") => Some(Format::Table('\t')),
        Some("svg") => Some(Format::Svg),
        Some("eml") => Some(Format::Message),
        Some("txt" | "text") => Some(Format::Text),
        Some(e) if CODE_EXTENSIONS.contains(&e) => Some(Format::Code),
        _ => None,
    };
    by_extension.unwrap_or_else(|| match essence(&part.mime_type).as_str() {
        "text/html" | "application/xhtml+xml" => Format::Html,
        "text/markdown" | "text/x-markdown" => Format::Markdown,
        "text/csv" | "text/comma-separated-values" => Format::Table(','),
        "text/tab-separated-values" => Format::Table('\t'),
        "image/svg+xml" => Format::Svg,
        "message/rfc822" => Format::Message,
        "text/plain" => Format::Text,
        _ => Format::Code,
    })
}

/// `bytes` -- the start of a file `total` bytes long, or all of it -- drawn
/// as `part`'s page.
fn draw(part: &PartRef, bytes: &[u8], total: u64) -> String {
    let format = format_of(part);
    let raw = !stored_as_text(part);
    let inner = if total == 0 {
        note("This file is empty.")
    } else if format.whole() && (total > MOST_WHOLE as u64 || (bytes.len() as u64) < total) {
        note("This file is too large to preview. Save it to open it.")
    } else {
        let shown = &bytes[..bytes.len().min(MOST_TEXT)];
        let cut = total > shown.len() as u64;
        let text = || decode(shown, None, cut, raw);
        match format {
            Format::Html => {
                let declared = if raw { declared_charset(bytes) } else { None };
                sanitize(&decode(bytes, declared, false, raw), &Rewrite::offline()).html
            }
            Format::Markdown => {
                format!("{}<div class=\"doc\">{}</div>", cut_note(cut), markdown(&text()))
            }
            Format::Table(separator) => table(&text(), separator, cut),
            Format::Svg => format!(
                "<div class=\"picture\"><img alt=\"\" src=\"data:image/svg+xml;base64,{}\"></div>",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            ),
            Format::Message => message(bytes),
            Format::Code => {
                format!("{}<pre class=\"code\">{}</pre>", cut_note(cut), escape_html(&text()))
            }
            Format::Text => format!("{}<pre>{}</pre>", cut_note(cut), linkify(&text())),
        }
    };
    crate::mailview::page(&inner, STYLE)
}

fn note(words: &str) -> String {
    format!("<p class=\"note\">{}</p>", escape_html(words))
}

fn cut_note(cut: bool) -> String {
    if cut {
        note("Only the beginning of this file is shown. Save it to read the rest.")
    } else {
        String::new()
    }
}

/// The first [`MOST_TEXT`] bytes of a message part as text, and whether there
/// was more -- for a forwarded message that does not parse.
fn text_of(bytes: &[u8]) -> (String, bool) {
    let cut = bytes.len() > MOST_TEXT;
    (decode(&bytes[..bytes.len().min(MOST_TEXT)], None, cut, true), cut)
}

/// `bytes` as text. `cut` says they were cut short, which can leave half a
/// UTF-8 character at the end.
///
/// Not `raw`: a `text/*` part, which sync already decoded to UTF-8 (see the
/// module docs), so UTF-8 it is. `raw`: bytes as they were sent, read in the
/// encoding a byte-order mark names, or `declared`, or UTF-8, or -- when it is
/// not UTF-8 either -- Windows-1252, which is what unlabelled text that is not
/// UTF-8 usually turns out to be.
fn decode(bytes: &[u8], declared: Option<&'static Encoding>, cut: bool, raw: bool) -> String {
    if !raw {
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        return match std::str::from_utf8(bytes) {
            Ok(text) => text.to_string(),
            Err(e) if cut && e.error_len().is_none() => {
                String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned()
            }
            Err(_) => String::from_utf8_lossy(bytes).into_owned(),
        };
    }
    if let Some((encoding, bom)) = Encoding::for_bom(bytes) {
        return encoding.decode_without_bom_handling(&bytes[bom..]).0.into_owned();
    }
    if let Some(encoding) = declared
        && encoding != encoding_rs::UTF_8
    {
        return encoding.decode_without_bom_handling(bytes).0.into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(e) if cut && e.error_len().is_none() => {
            String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned()
        }
        Err(_) => encoding_rs::WINDOWS_1252.decode_without_bom_handling(bytes).0.into_owned(),
    }
}

/// The `charset=` an HTML file names near its top, as a browser would find it.
/// UTF-16 is not believed without a byte-order mark, as a browser does not.
fn declared_charset(bytes: &[u8]) -> Option<&'static Encoding> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).to_ascii_lowercase();
    let at = head.find("charset=")? + "charset=".len();
    let label: String = head[at..]
        .trim_start_matches(['"', '\''])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
        .collect();
    Encoding::for_label(label.as_bytes())
        .filter(|e| *e != encoding_rs::UTF_16LE && *e != encoding_rs::UTF_16BE)
}

/// Markdown as HTML, through the same sanitiser a message's HTML goes
/// through: raw HTML in Markdown is as much a stranger's as any other.
fn markdown(text: &str) -> String {
    use pulldown_cmark::{Options, Parser, html};
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let mut out = String::new();
    html::push_html(&mut out, Parser::new_ext(text, options));
    sanitize(&out, &Rewrite::offline()).html
}

/// `text` as a table, the first row as its heading.
fn table(text: &str, separator: char, cut: bool) -> String {
    let (rows, more_rows) = rows(text, separator, MOST_ROWS);
    let more_columns = rows.iter().any(|r| r.len() > MOST_COLUMNS);
    let mut out = String::new();
    if cut || more_rows {
        out.push_str(&note(&format!(
            "Only the first {} rows are shown. Save the file to see the rest.",
            rows.len()
        )));
    }
    if more_columns {
        out.push_str(&note(&format!(
            "Only the first {MOST_COLUMNS} columns are shown. Save the file to see the rest."
        )));
    }
    out.push_str("<table class=\"sheet\">");
    for (i, row) in rows.iter().enumerate() {
        let cell = if i == 0 { "th" } else { "td" };
        if i == 0 {
            out.push_str("<thead>");
        } else if i == 1 {
            out.push_str("<tbody>");
        }
        out.push_str("<tr>");
        for field in row.iter().take(MOST_COLUMNS) {
            out.push_str(&format!("<{cell}>{}</{cell}>", escape_html(field)));
        }
        out.push_str("</tr>");
        if i == 0 {
            out.push_str("</thead>");
        }
    }
    if rows.len() > 1 {
        out.push_str("</tbody>");
    }
    out.push_str("</table>");
    out
}

/// RFC 4180, leniently: a quoted field may hold the separator, line breaks and
/// doubled quotes, and a quote anywhere else is kept as written. Stops after
/// `most` rows, answering whether there was more.
fn rows(text: &str, separator: char, most: usize) -> (Vec<Vec<String>>, bool) {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut starting = true;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if quoted {
            match ch {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => field.push(ch),
            }
            continue;
        }
        match ch {
            '"' if starting => {
                quoted = true;
                starting = false;
            }
            c if c == separator => {
                row.push(std::mem::take(&mut field));
                starting = true;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                starting = true;
                if rows.len() == most {
                    return (rows, chars.peek().is_some());
                }
            }
            _ => {
                field.push(ch);
                starting = false;
            }
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    (rows, false)
}

/// A forwarded message: who it was from and to, then its own body, drawn the
/// way a message is but offline, so its own pictures -- remote, or parts of it
/// this cannot reach -- are left out rather than drawn broken. Its attachments
/// are named but cannot be opened from here. Bytes that do not parse as a
/// message are shown as the text they are.
fn message(bytes: &[u8]) -> String {
    let Ok(parsed) = everyday_mail::mime::parse(bytes) else {
        let (text, cut) = text_of(bytes);
        return format!("{}<pre class=\"code\">{}</pre>", cut_note(cut), escape_html(&text));
    };
    let mut out = String::from("<table class=\"headers\">");
    let mut header = |name: &str, value: String| {
        if !value.is_empty() {
            out.push_str(&format!("<tr><th>{name}</th><td>{}</td></tr>", escape_html(&value)));
        }
    };
    header("From", addresses(&parsed.from));
    header("To", addresses(&parsed.to));
    header("Cc", addresses(&parsed.cc));
    header(
        "Date",
        parsed
            .date
            .map(|d| {
                d.to_zoned(jiff::tz::TimeZone::system()).strftime("%a %-d %b %Y, %H:%M").to_string()
            })
            .unwrap_or_default(),
    );
    header("Subject", parsed.subject.clone().unwrap_or_default());
    let names: Vec<&str> = parsed.parts.iter().filter_map(|p| p.filename.as_deref()).collect();
    header("Attached", names.join(", "));
    out.push_str("</table><hr class=\"split\">");

    if let Some(html) = parsed.html.as_deref().filter(|h| !h.trim().is_empty()) {
        out.push_str(&sanitize(html, &Rewrite::offline()).html);
    } else {
        out.push_str(&format!("<pre>{}</pre>", linkify(parsed.text.as_deref().unwrap_or(""))));
    }
    out
}

fn addresses(list: &[everyday_mail::mime::Address]) -> String {
    list.iter()
        .map(|a| match (a.name.as_deref(), a.email.as_deref()) {
            (Some(name), Some(email)) if !name.trim().is_empty() => format!("{name} <{email}>"),
            (_, Some(email)) => email.to_string(),
            (Some(name), None) => name.to_string(),
            (None, None) => String::new(),
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(filename: Option<&str>, mime: &str) -> PartRef {
        PartRef {
            cid: None,
            filename: filename.map(str::to_string),
            mime_type: mime.to_string(),
            size: 0,
            blob: None,
            attached: true,
        }
    }

    fn drawn(filename: &str, mime: &str, bytes: &[u8]) -> String {
        draw(&part(Some(filename), mime), bytes, bytes.len() as u64)
    }

    #[test]
    fn the_declared_type_decides_and_the_name_fills_in_for_a_vague_one() {
        use PreviewKind::*;
        assert_eq!(kind_of(&part(Some("a.jpg"), "image/jpeg")), Some(Image));
        assert_eq!(kind_of(&part(Some("scan"), "application/pdf")), Some(Pdf));
        assert_eq!(kind_of(&part(Some("Report.PDF"), "application/octet-stream")), Some(Pdf));
        assert_eq!(kind_of(&part(Some("clip.mov"), "application/octet-stream")), Some(Video));
        assert_eq!(kind_of(&part(Some("memo.m4a"), "audio/x-m4a")), Some(Audio));
        assert_eq!(kind_of(&part(Some("notes.md"), "application/octet-stream")), Some(Page));
        assert_eq!(kind_of(&part(Some("data.csv"), "text/csv")), Some(Page));
        assert_eq!(kind_of(&part(Some("fwd.eml"), "message/rfc822")), Some(Page));
        assert_eq!(kind_of(&part(None, "message/rfc822")), Some(Page));
        assert_eq!(kind_of(&part(Some("logo.svg"), "image/svg+xml")), Some(Page));
        assert_eq!(kind_of(&part(Some("main.rs"), "application/octet-stream")), Some(Page));
    }

    #[test]
    fn office_files_archives_and_pictures_the_webview_cannot_draw_are_not_previewed() {
        for (name, mime) in [
            ("a.docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
            ("a.xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            ("a.pptx", "application/vnd.openxmlformats-officedocument.presentationml.presentation"),
            ("a.odt", "application/vnd.oasis.opendocument.text"),
            ("a.doc", "text/plain"),
            ("a.zip", "application/zip"),
            ("a.rtf", "text/rtf"),
            ("IMG_1.HEIC", "image/heic"),
            ("scan.tiff", "image/tiff"),
            ("clip.ts", "application/octet-stream"),
            ("noextension", "application/octet-stream"),
            // Read as text at sync, so neither a picture nor a page any more.
            ("scan.pdf", "text/plain"),
            ("photo.jpg", "text/plain"),
            ("clip.mp4", "text/html"),
        ] {
            assert_eq!(kind_of(&part(Some(name), mime)), None, "{name} as {mime}");
        }
    }

    #[test]
    fn a_film_or_recording_is_played_as_the_type_it_declares_or_its_name_says() {
        let typed = |name: &str, mime: &str| media_type(&part(Some(name), mime));
        assert_eq!(typed("a.mov", "video/quicktime"), Some("video/quicktime".to_string()));
        assert_eq!(typed("a.mov", "application/octet-stream"), Some("video/quicktime".into()));
        assert_eq!(typed("memo.m4a", "application/octet-stream"), Some("audio/mp4".into()));
        assert_eq!(typed("memo.m4a", "audio/x-m4a"), Some("audio/x-m4a".into()));
        assert_eq!(typed("a.pdf", "application/pdf"), None);
        assert_eq!(typed("a.jpg", "image/jpeg"), None);
    }

    #[test]
    fn an_extension_is_read_from_the_last_dot_of_the_last_path_segment() {
        assert_eq!(extension(Some("a.tar.gz")), Some("gz".to_string()));
        assert_eq!(extension(Some(".env")), Some("env".to_string()));
        assert_eq!(extension(Some("dir.d/README")), None);
        assert_eq!(extension(Some("trailing.")), None);
        assert_eq!(extension(None), None);
    }

    #[test]
    fn a_page_never_carries_a_script_whatever_the_file_held() {
        let hostile = b"<script>alert(1)</script><img src=x onerror=alert(2)><a href=\"javascript:alert(3)\">x</a>";
        for (name, mime) in [
            ("a.html", "text/html"),
            ("a.md", "text/markdown"),
            ("a.txt", "text/plain"),
            ("a.json", "application/json"),
            ("a.csv", "text/csv"),
        ] {
            let page = drawn(name, mime, hostile);
            let lower = page.to_ascii_lowercase();
            // As markup, that is: a text file shows the same words escaped.
            assert!(!lower.contains("<script"), "{name}: {page}");
            assert!(!lower.contains("<img src=x onerror"), "{name}: {page}");
            assert!(!lower.contains("onerror=alert(2)>"), "{name}: {page}");
            assert!(!lower.contains("href=\"javascript:"), "{name}: {page}");
            assert!(page.contains("Content-Security-Policy"), "{name}");
        }
    }

    #[test]
    fn an_svg_is_a_picture_in_a_data_address_never_markup_in_the_page() {
        let page = drawn("logo.svg", "image/svg+xml", b"<svg onload=\"alert(1)\"></svg>");
        assert!(page.contains("src=\"data:image/svg+xml;base64,"));
        assert!(!page.contains("<svg"));
    }

    #[test]
    fn text_is_escaped_and_its_links_are_live() {
        let page = drawn("notes.txt", "text/plain", b"a < b, see https://example.com/x");
        assert!(page.contains("a &lt; b"));
        assert!(page.contains("<a href=\"https://example.com/x\""));
        let code = drawn("a.json", "application/json", b"{\"a\": \"<b>\"}");
        assert!(code.contains("<pre class=\"code\">{&quot;a&quot;: &quot;&lt;b&gt;&quot;}</pre>"));
    }

    #[test]
    fn markdown_is_drawn_as_a_document() {
        let page = drawn("a.md", "application/octet-stream", b"# Title\n\n- one\n- **two**\n");
        assert!(page.contains("<h1>Title</h1>"), "{page}");
        assert!(page.contains("<strong>two</strong>"), "{page}");
    }

    #[test]
    fn a_csv_is_a_table_with_quoted_fields_kept_whole() {
        let page = drawn(
            "a.csv",
            "text/csv",
            b"name,note\r\n\"Smith, J\",\"said \"\"hi\"\"\nthen left\"\r\n",
        );
        assert!(page.contains("<thead><tr><th>name</th><th>note</th></tr></thead>"), "{page}");
        assert!(
            page.contains("<td>Smith, J</td><td>said &quot;hi&quot;\nthen left</td>"),
            "{page}"
        );
        let tsv = drawn("a.tsv", "text/tab-separated-values", b"a\tb\n1\t2");
        assert!(tsv.contains("<td>1</td><td>2</td>"), "{tsv}");
    }

    #[test]
    fn rows_stop_at_the_limit_and_say_there_was_more() {
        let (cut, more) = rows("a\nb\nc\n", ',', 2);
        assert_eq!(cut, vec![vec!["a".to_string()], vec!["b".to_string()]]);
        assert!(more);
        let (whole, more) = rows("a\nb\n", ',', 2);
        assert_eq!(whole.len(), 2);
        assert!(!more);
    }

    #[test]
    fn a_file_with_pictures_draws_none_it_would_have_to_fetch() {
        let html = br#"<p>Hi</p><img src="https://evil.example/t.gif"><img src="cid:logo@x">
            <img src="everyday://mail/img/m/abc"><img src="data:image/png;base64,iVBORw0KGgo=">"#;
        for (name, mime) in [("a.html", "text/html"), ("a.md", "text/markdown")] {
            let page = drawn(name, mime, html);
            assert!(!page.contains("evil.example"), "{name}: {page}");
            assert!(!page.contains("everyday://"), "{name}: {page}");
            assert!(!page.contains("cid:"), "{name}: {page}");
            assert!(page.contains("<p>Hi</p>"), "{name}: {page}");
        }
        let eml = b"From: a@example.com\r\nSubject: Logo\r\nContent-Type: text/html\r\n\r\n\
            <p>Here</p><img src=\"cid:logo@x\"><img src=\"https://evil.example/p.png\">\r\n";
        let page = drawn("fwd.eml", "message/rfc822", eml);
        assert!(page.contains("<p>Here</p>"), "{page}");
        assert!(!page.contains("<img"), "{page}");
    }

    #[test]
    fn a_text_part_is_read_as_the_utf8_sync_made_it_whatever_it_claims() {
        // Sync decoded this from ISO-8859-1 already; the tag is a leftover.
        let html = "<meta http-equiv=\"Content-Type\" content=\"text/html; charset=iso-8859-1\">\
            <p>café</p>";
        assert!(drawn("a.html", "text/html", html.as_bytes()).contains("café"));
        assert!(drawn("a.txt", "text/plain", "crème".as_bytes()).contains("crème"));
        assert!(drawn("a.csv", "text/csv", "a\nné".as_bytes()).contains("<td>né</td>"));
    }

    #[test]
    fn only_the_start_of_a_long_text_file_is_asked_for() {
        // `page` reads the first stretch of a long file; the page says so.
        let start = vec![b'a'; MOST_TEXT];
        let page = draw(&part(Some("big.log"), "text/plain"), &start, 300 * 1024 * 1024);
        assert!(page.contains("Only the beginning of this file is shown"));
        // And a whole-or-nothing file it could not read whole is not drawn.
        let page = draw(&part(Some("a.html"), "text/html"), b"", MOST_WHOLE as u64 + 1);
        assert!(page.contains("too large to preview"));
    }

    #[test]
    fn a_long_file_is_cut_short_and_says_so() {
        let mut text = b"x".to_vec();
        text.extend("é".repeat(MOST_TEXT / 2 + 10).into_bytes());
        text.extend(b"ENDOFTEXT");
        let page = drawn("a.txt", "text/plain", &text);
        assert!(page.contains("Only the beginning of this file is shown"));
        assert!(page.contains("éé"), "still read as UTF-8");
        assert!(!page.contains("ENDOFTEXT"));
        assert!(!page.contains('\u{FFFD}'), "the half character at the cut must not show");
    }

    #[test]
    fn raw_text_is_read_in_the_encoding_it_names_or_windows_1252() {
        assert_eq!(decode(b"caf\xe9", None, false, true), "café");
        assert_eq!(decode(b"caf\xc3", None, true, true), "caf");
        assert_eq!(decode(b"\xef\xbb\xbfhi", None, false, true), "hi");
        assert_eq!(decode(b"\xff\xfeh\0i\0", None, false, true), "hi");
        let html = b"<meta charset=\"koi8-r\"><p>\xf0\xd2\xc9\xd7\xc5\xd4</p>";
        assert_eq!(declared_charset(html), Some(encoding_rs::KOI8_R));
        assert!(drawn("a.html", "application/octet-stream", html).contains("Привет"));
        assert!(drawn("menu.txt", "application/octet-stream", b"Caf\xe9").contains("Café"));
    }

    #[test]
    fn a_forwarded_message_shows_who_it_was_from_and_its_own_body() {
        let eml = b"From: Ann <ann@example.com>\r\nTo: bob@example.com\r\nSubject: Plans\r\n\
            Content-Type: text/plain\r\n\r\nSee you <then>.\r\n";
        let page = drawn("fwd.eml", "message/rfc822", eml);
        assert!(page.contains("<th>From</th><td>Ann &lt;ann@example.com&gt;</td>"), "{page}");
        assert!(page.contains("<th>Subject</th><td>Plans</td>"), "{page}");
        assert!(page.contains("See you &lt;then&gt;."), "{page}");
    }

    #[test]
    fn an_empty_or_enormous_file_says_so_instead() {
        assert!(drawn("a.txt", "text/plain", b"").contains("This file is empty."));
        let big = vec![b'a'; MOST_WHOLE + 1];
        assert!(drawn("a.html", "text/html", &big).contains("too large to preview"));
    }
}
