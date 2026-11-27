//! The readers: one per [`DocFormat`], each turning a file's bytes into a
//! [`DocText`]. Pure Rust, every one; each runs under `catch_unwind` and a
//! size cap ([`parse`]), so a file that trips a library's panic costs its
//! own parse and nothing else.
//!
//! - [`pdf`]: `pdf-extract` for each page's text and `lopdf` for the outline
//!   and the information dictionary. A PDF's blocks carry its own page
//!   numbers and no HTML: pdf.js draws the pages.
//! - [`epub`]: `zip` and `roxmltree` for the container, the package and the
//!   navigation, and the shared walk in [`html`] for each chapter.
//! - [`html`]: the walk that turns a page's tree into blocks, written to
//!   serve an EPUB's chapters and a standalone web page alike.
//! - [`text`]: plain text, and Markdown through `pulldown-cmark`.
//!
//! Every reader writes the `html` of a block itself, from a fixed set of
//! inline elements and with every character of the file's own text escaped,
//! and never copies an attribute out of the file but its own `data-block`
//! on a link. What the reader draws is therefore safe to put in the page as
//! it is, whatever the file said: that is the whole reason these do not
//! pass a file's HTML on.

pub mod epub;
pub mod html;
pub mod pdf;
pub mod text;

use std::panic::{AssertUnwindSafe, catch_unwind};

use jatayu_core::documents::{DocFormat, DocText};

/// The largest file read at all. A document bigger than this is a scan, an
/// archive or a mistake, and reading it would hold all of it in memory.
pub const MAX_DOCUMENT_BYTES: u64 = 256 * 1024 * 1024;

const TOO_LARGE: &str = "This document is too large to be read.";
const UNREADABLE: &str = "This document could not be read.";

/// Why a document could not be read, in words for the person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

/// A file inside a document -- an EPUB's picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// Read `bytes` as `format`.
pub fn parse(format: DocFormat, bytes: &[u8]) -> Result<DocText, ParseError> {
    parse_within(format, bytes, MAX_DOCUMENT_BYTES)
}

/// [`parse`] under a size cap of the caller's choosing, so the cap itself can
/// be tried without a file the size of the real one.
fn parse_within(format: DocFormat, bytes: &[u8], limit: u64) -> Result<DocText, ParseError> {
    if bytes.len() as u64 > limit {
        return Err(ParseError(TOO_LARGE.into()));
    }
    guarded(|| match format {
        DocFormat::Pdf => pdf::parse(bytes),
        DocFormat::Epub => epub::parse(bytes),
        DocFormat::Markdown => text::parse_markdown(bytes),
        DocFormat::Text => text::parse_text(bytes),
        DocFormat::Html => html::parse(bytes),
    })
}

/// The file at `path` inside a document, for a picture the reader draws or a
/// library item's cover: an EPUB's, by its path from the archive's root.
/// `None` for any other format, and for a path that is not there.
pub fn read_resource(format: DocFormat, bytes: &[u8], path: &str) -> Option<Resource> {
    if format != DocFormat::Epub || bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return None;
    }
    catch_unwind(AssertUnwindSafe(|| epub::resource(bytes, path))).ok().flatten()
}

/// Run a reader, turning a panic in it -- or in a library under it -- into
/// an answer for the person. The release profile unwinds, so this holds.
fn guarded<T>(read: impl FnOnce() -> Result<T, ParseError>) -> Result<T, ParseError> {
    catch_unwind(AssertUnwindSafe(read)).unwrap_or_else(|_| Err(ParseError(UNREADABLE.into())))
}

/// What every reader of a document without pages of its own does last: the
/// headings stand in for a table of contents the file lacks, the blocks are
/// given made-up pages, and the sections are settled against them.
fn finish_flow(doc: &mut DocText) {
    if doc.sections.is_empty() {
        doc.sections_from_headings();
    }
    doc.assign_synthetic_pages();
    doc.link_sections();
}

// ---- shared by the readers ------------------------------------------------

/// Words with every run of whitespace made one space and the ends trimmed.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `ch` written so it is text and never markup, wherever it lands in a page.
fn push_escaped(out: &mut String, ch: char) {
    match ch {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '"' => out.push_str("&quot;"),
        '\'' => out.push_str("&#39;"),
        other => out.push(other),
    }
}

/// `text` with every character that could be markup written as text.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        push_escaped(&mut out, ch);
    }
    out
}

/// Names out of one string a file gives them in -- an author field, a
/// `<meta name="author">`: split on `;`, `&` and " and ", never on a comma,
/// which is how "Austen, Jane" is written. Without repeats.
fn split_names(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in text.replace(" and ", ";").replace('&', ";").split(';') {
        let name = collapse(name);
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Keywords or subjects out of one string: split on commas and semicolons,
/// without repeats.
fn split_keywords(text: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for word in text.split([',', ';']) {
        let word = collapse(word);
        if !word.is_empty() && !words.contains(&word) {
            words.push(word);
        }
    }
    words
}

/// A file's bytes as text: UTF-8, or the UTF-16 that a byte order mark
/// announces, and otherwise UTF-8 with what is not UTF-8 replaced. With
/// `sniff`, markup that is not UTF-8 and says which encoding it is in (its
/// `charset=`, or an XML declaration's `encoding=`) is read in that.
fn decode(bytes: &[u8], sniff: bool) -> String {
    if let Some((encoding, skip)) = encoding_rs::Encoding::for_bom(bytes) {
        return encoding.decode_without_bom_handling(&bytes[skip..]).0.into_owned();
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    if sniff && let Some(encoding) = declared_encoding(bytes) {
        return encoding.decode_without_bom_handling(bytes).0.into_owned();
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// The encoding the first kilobyte of a markup file names, if it names one
/// that is known.
fn declared_encoding(bytes: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).to_ascii_lowercase();
    ["charset=", "encoding="].iter().find_map(|key| {
        let after = &head[head.find(*key)? + key.len()..];
        let label: String = after
            .trim_start_matches(['"', '\'', ' '])
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
            .collect();
        encoding_rs::Encoding::for_label(label.as_bytes())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_over_the_cap_is_refused_before_it_is_read() {
        let err = parse_within(DocFormat::Text, b"hello", 4).unwrap_err();
        assert_eq!(err.0, TOO_LARGE);
        assert!(parse_within(DocFormat::Text, b"hello", 5).is_ok());
    }

    #[test]
    fn a_panic_in_a_reader_becomes_words_for_the_person() {
        let err = guarded::<()>(|| panic!("a library gave up")).unwrap_err();
        assert_eq!(err.0, UNREADABLE);
        assert_eq!(guarded(|| Ok(7)).unwrap(), 7);
    }

    #[test]
    fn garbage_is_an_error_in_every_format_and_never_a_panic() {
        let noise: Vec<u8> =
            (0..4000u32).map(|n| (n.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        for format in [DocFormat::Pdf, DocFormat::Epub] {
            assert!(parse(format, &noise).is_err(), "{format:?}");
            assert!(parse(format, b"").is_err(), "{format:?}");
        }
        // Text-like formats take anything and make what they can of it.
        for format in [DocFormat::Markdown, DocFormat::Text, DocFormat::Html] {
            assert!(parse(format, &noise).is_ok(), "{format:?}");
            assert!(parse(format, b"").is_ok(), "{format:?}");
        }
        assert!(parse(DocFormat::Pdf, b"%PDF-1.4\n1 0 obj << /Type /Catalog").is_err());
        assert!(parse(DocFormat::Epub, b"PK\x03\x04 not really a zip").is_err());
    }

    #[test]
    fn only_an_epub_has_files_inside() {
        assert!(read_resource(DocFormat::Pdf, b"%PDF", "a.png").is_none());
        assert!(read_resource(DocFormat::Text, b"hello", "a.png").is_none());
        assert!(read_resource(DocFormat::Epub, b"not a zip", "a.png").is_none());
    }

    #[test]
    fn bytes_are_read_in_the_encoding_a_mark_or_a_declaration_gives() {
        assert_eq!(decode(b"plain", false), "plain");
        assert_eq!(decode(b"\xef\xbb\xbfwith a mark", false), "with a mark");
        // UTF-16, little and big endian, each with its mark.
        assert_eq!(decode(b"\xff\xfeh\0i\0", false), "hi");
        assert_eq!(decode(b"\xfe\xff\0h\0i", false), "hi");
        // Not UTF-8, and no way to tell without the page saying so.
        assert_eq!(decode(b"caf\xe9", false), "caf\u{fffd}");
        let page = decode(b"<meta charset=\"iso-8859-1\">caf\xe9", true);
        assert!(page.ends_with("caf\u{e9}"), "{page}");
        let xml = decode(b"<?xml version=\"1.0\" encoding=\"windows-1252\"?>caf\xe9", true);
        assert!(xml.ends_with("caf\u{e9}"), "{xml}");
    }

    #[test]
    fn names_and_keywords_split_the_way_files_write_them() {
        assert_eq!(
            split_names("Ada Lovelace; Charles Babbage and Mary Somerville & Ada Lovelace"),
            ["Ada Lovelace", "Charles Babbage", "Mary Somerville"]
        );
        assert_eq!(split_names("Austen, Jane"), ["Austen, Jane"]);
        assert_eq!(
            split_keywords("engines, notes;  analysis ,notes,"),
            ["engines", "notes", "analysis"]
        );
    }

    #[test]
    fn escaping_leaves_nothing_that_is_markup() {
        assert_eq!(
            escape(r#"<a href="x">Tom & 'Jerry'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;Tom &amp; &#39;Jerry&#39;&lt;/a&gt;"
        );
        assert_eq!(collapse("  a \n\t b  "), "a b");
    }
}
