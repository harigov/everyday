//! Reading pdf. See [`super`].
//!
//! `pdf-extract` reads a page's text out of its content stream by where each
//! glyph lands; `lopdf`, which it is built on, is loaded once here and drives
//! it page by page, so a page that cannot be read costs that page and not the
//! pages after it (`pdf-extract`'s own whole-file call stops at the first
//! page that fails). The same loaded document gives the outline and the
//! information dictionary. A PDF's words come out with no structure beyond
//! lines and the gaps between them, so a paragraph is a run of lines between
//! blank ones, with a little help from where a line ends short.
//!
//! The outline is walked here rather than through `lopdf`'s `get_toc`, which
//! keys its entries by title: a book with a "Summary" at the end of every
//! chapter would be left with one.

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};

use karmayoga_core::documents::{DocBlock, DocBlockKind, DocFormat, DocMeta, DocSection, DocText};
use lopdf::{Dictionary, Document, Object, ObjectId};
use pdf_extract::PlainTextOutput;

use super::{ParseError, collapse, split_keywords, split_names};

const DAMAGED: &str = "This PDF could not be read. It may be damaged.";
const PASSWORD: &str = "This PDF is protected with a password, so it cannot be read.";
const NO_PAGES: &str = "This PDF has no pages in it.";

/// The most outline entries read. A real contents list has hundreds; one
/// with more is a file built to be slow to read.
const MAX_OUTLINE: usize = 20_000;

/// Read a PDF: one paragraph block per paragraph on each page, the outline as
/// sections, and what the information dictionary and the first pages say of
/// the document.
pub fn parse(bytes: &[u8]) -> Result<DocText, ParseError> {
    let pdf = load(bytes)?;
    let numbers: Vec<u32> = pdf.get_pages().keys().copied().collect();
    if numbers.is_empty() {
        return Err(ParseError(NO_PAGES.into()));
    }

    let mut doc = DocText::new(DocFormat::Pdf);
    let mut blank = Vec::new();
    let mut failed = Vec::new();
    // What the first two pages say, for a DOI.
    let mut opening = String::new();
    for &page in &numbers {
        let Some(raw) = page_text(&pdf, page) else {
            failed.push(page);
            continue;
        };
        let raw = tidy(&raw);
        if page <= 2 {
            opening.push_str(&raw);
            opening.push('\n');
        }
        let before = doc.blocks.len();
        for text in paragraphs(&raw) {
            doc.blocks.push(DocBlock {
                kind: DocBlockKind::Paragraph,
                page,
                text,
                ..DocBlock::default()
            });
        }
        if doc.blocks.len() == before {
            blank.push(page);
        }
    }
    doc.pages = numbers.len() as u32;

    if !blank.is_empty() {
        doc.warnings.push(match blank.as_slice() {
            [page] => format!("Page {page} has no text in it; it may be a scanned picture."),
            _ => format!(
                "Pages {} have no text in them; they may be scanned pictures.",
                page_ranges(&blank)
            ),
        });
    }
    if !failed.is_empty() {
        doc.warnings.push(match failed.as_slice() {
            [page] => format!("Page {page} could not be read."),
            _ => format!("Pages {} could not be read.", page_ranges(&failed)),
        });
    }

    doc.meta = guarded(|| meta(&pdf)).unwrap_or_default();
    if let Some(doi) = find_doi(&opening) {
        let doi = format!("doi:{doi}");
        if !doc.meta.identifiers.contains(&doi) {
            doc.meta.identifiers.push(doi);
        }
    }
    doc.sections = guarded(|| sections(&pdf, &doc.blocks)).unwrap_or_default();
    doc.link_sections();
    Ok(doc)
}

/// Run `read`, and if it panics -- these libraries are fed a stranger's file
/// -- say so with `None`, so what is lost is the one thing it was reading.
fn guarded<T>(read: impl FnOnce() -> T) -> Option<T> {
    catch_unwind(AssertUnwindSafe(read)).ok()
}

/// The file as `lopdf` reads it, opened with the empty password that a file
/// protected only against copying and printing is opened with.
fn load(bytes: &[u8]) -> Result<Document, ParseError> {
    let mut doc = Document::load_mem(bytes).map_err(|error| match error {
        lopdf::Error::Decryption(_) => ParseError(PASSWORD.into()),
        _ => ParseError(DAMAGED.into()),
    })?;
    if doc.is_encrypted() && doc.decrypt("").is_err() {
        return Err(ParseError(PASSWORD.into()));
    }
    Ok(doc)
}

/// What `pdf-extract` makes of page `page`: `None` if it could not, whether
/// by an error or by a panic. Pages go one at a time for that reason.
fn page_text(doc: &Document, page: u32) -> Option<String> {
    guarded(|| {
        let mut text = String::new();
        {
            let mut output = PlainTextOutput::new(&mut text);
            pdf_extract::output_doc_page(doc, &mut output, page).ok()?;
        }
        Some(text)
    })?
}

// ---- words ------------------------------------------------------------------

/// Page text with the ligatures that fonts store as one glyph written out as
/// the letters, so that a search for "efficient" finds "eﬃcient"; and with
/// what is not text taken out.
fn tidy(page: &str) -> String {
    let mut out = String::with_capacity(page.len());
    for ch in page.chars() {
        match ch {
            '\u{fb00}' => out.push_str("ff"),
            '\u{fb01}' => out.push_str("fi"),
            '\u{fb02}' => out.push_str("fl"),
            '\u{fb03}' => out.push_str("ffi"),
            '\u{fb04}' => out.push_str("ffl"),
            '\u{fb05}' | '\u{fb06}' => out.push_str("st"),
            '\n' => out.push('\n'),
            '\t' => out.push(' '),
            ch if ch.is_control() => {}
            ch => out.push(ch),
        }
    }
    out
}

/// A page's text as paragraphs: runs of lines between blank ones, each
/// paragraph's lines joined with a space (and a word hyphenated across a line
/// rejoined), cut further where a line ends a sentence short of the others
/// and the next begins a new one, or where a bullet begins.
fn paragraphs(page: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut group: Vec<&str> = Vec::new();
    for line in page.lines().chain(std::iter::once("")) {
        let line = line.trim();
        if line.is_empty() {
            if !group.is_empty() {
                split_group(&group, &mut out);
                group.clear();
            }
        } else {
            group.push(line);
        }
    }
    out
}

/// One run of lines as paragraphs.
fn split_group(lines: &[&str], out: &mut Vec<String>) {
    let longest = lines.iter().map(|line| line.chars().count()).max().unwrap_or(0);
    let mut current = String::new();
    let mut previous = "";
    for &line in lines {
        if current.is_empty() {
            current.push_str(line);
        } else if breaks_before(previous, line, longest) {
            push_paragraph(out, &current);
            current.clear();
            current.push_str(line);
        } else {
            join_line(&mut current, line);
        }
        previous = line;
    }
    push_paragraph(out, &current);
}

/// A paragraph with its whitespace made single spaces, and the soft hyphens
/// that were not at a line's end -- invisible, and in the way of a search --
/// taken out. Nothing is added if there is nothing in it.
fn push_paragraph(out: &mut Vec<String>, text: &str) {
    let text = collapse(text).replace('\u{ad}', "");
    if !text.is_empty() {
        out.push(text);
    }
}

/// Add `line` to the paragraph so far. A word broken with a hyphen at the end
/// of the one line and carried on in lowercase at the start of the next is
/// made one word again.
fn join_line(current: &mut String, line: &str) {
    let hyphenated = current.ends_with(['-', '\u{ad}'])
        && current.chars().rev().nth(1).is_some_and(char::is_alphabetic)
        && line.chars().next().is_some_and(char::is_lowercase);
    if hyphenated {
        current.pop();
    } else {
        current.push(' ');
    }
    current.push_str(line);
}

/// Whether a new paragraph begins at `line`, which follows `previous` in a run
/// whose longest line is `longest` characters: a bullet always starts one,
/// and so does a capital after a sentence that ended well short of the margin
/// -- the last line of a paragraph.
fn breaks_before(previous: &str, line: &str, longest: usize) -> bool {
    if starts_bullet(line) {
        return true;
    }
    let short = previous.chars().count() * 100 < longest * 55;
    let sentence_ended = previous
        .trim_end_matches(['"', '\'', '”', '’', ')', ']', '»'])
        .ends_with(['.', '!', '?', '…']);
    let new_sentence = line
        .chars()
        .next()
        .is_some_and(|ch| ch.is_uppercase() || ch.is_ascii_digit() || "\"“‘([".contains(ch));
    short && sentence_ended && new_sentence
}

fn starts_bullet(line: &str) -> bool {
    let mut chars = line.chars();
    match chars.next() {
        Some('•' | '◦' | '▪' | '●' | '■' | '□' | '‣' | '⁃' | '∙') => true,
        Some('-' | '–' | '—' | '*') => chars.next().is_some_and(char::is_whitespace),
        _ => false,
    }
}

/// Page numbers as ranges, in words: "3–5, 9 and 12–40". Past ten ranges the
/// rest are "others".
fn page_ranges(pages: &[u32]) -> String {
    let mut spans: Vec<(u32, u32)> = Vec::new();
    for &page in pages {
        match spans.last().copied() {
            Some((from, end)) if end + 1 == page => {
                let last = spans.len() - 1;
                spans[last] = (from, page);
            }
            _ => spans.push((page, page)),
        }
    }
    let mut shown: Vec<String> = spans
        .iter()
        .take(10)
        .map(|&(from, to)| if from == to { from.to_string() } else { format!("{from}–{to}") })
        .collect();
    if spans.len() > 10 {
        shown.push("others".into());
    }
    match shown.pop() {
        Some(last) if !shown.is_empty() => format!("{} and {last}", shown.join(", ")),
        Some(last) => last,
        None => String::new(),
    }
}

// ---- what the file says of itself ---------------------------------------------

/// The first DOI in `text`: `10.`, four to nine digits, a slash and then a
/// run of characters that are not whitespace, without the punctuation a
/// sentence puts after one. Lowercase.
fn find_doi(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(found) = text[from..].find("10.") {
        let at = from + found;
        from = at + 3;
        // Not the end of a longer number or word.
        if text[..at].chars().next_back().is_some_and(char::is_alphanumeric) {
            continue;
        }
        let digits = bytes[from..].iter().take_while(|byte| byte.is_ascii_digit()).count();
        if !(4..=9).contains(&digits) || bytes.get(from + digits) != Some(&b'/') {
            continue;
        }
        let rest = &text[from + digits + 1..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let suffix = rest[..end].trim_end_matches(['.', ',', ';', ')', ']']);
        if suffix.is_empty() {
            continue;
        }
        return Some(format!("10.{}/{suffix}", &text[from..from + digits]).to_lowercase());
    }
    None
}

/// A PDF text string as text: UTF-16 with a byte order mark, UTF-8 with one,
/// or else PDFDocEncoding (`lopdf` knows it; Latin-1 is what is left if it
/// will not say).
fn text_of(object: &Object) -> Option<String> {
    let bytes = object.as_str().ok()?;
    Some(if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        utf16(rest, false)
    } else if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        utf16(rest, true)
    } else if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        lopdf::decode_text_string(object)
            .unwrap_or_else(|_| bytes.iter().map(|&byte| char::from(byte)).collect())
    })
}

fn utf16(bytes: &[u8], little_endian: bool) -> String {
    let units: Vec<u16> = bytes
        .chunks(2)
        .map(|pair| {
            let pair = [pair[0], pair.get(1).copied().unwrap_or(0)];
            if little_endian { u16::from_le_bytes(pair) } else { u16::from_be_bytes(pair) }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

/// A PDF date, `D:20190301120000+01'00'`, as `2019-03-01` -- or `2019-03` or
/// `2019` when that is all it has -- and as written when it is not a date.
fn pdf_date(raw: &str) -> String {
    let raw = raw.trim();
    let body = raw.strip_prefix("D:").unwrap_or(raw);
    let digits: String = body.chars().take_while(char::is_ascii_digit).collect();
    let part = |from: usize, to: usize| digits.get(from..to).and_then(|s| s.parse::<u32>().ok());
    match (digits.len(), part(0, 4), part(4, 6), part(6, 8)) {
        (len, Some(year), Some(month), Some(day))
            if len >= 8 && (1..=12).contains(&month) && (1..=31).contains(&day) =>
        {
            format!("{year:04}-{month:02}-{day:02}")
        }
        (len, Some(year), Some(month), _) if len >= 6 && (1..=12).contains(&month) => {
            format!("{year:04}-{month:02}")
        }
        (len, Some(year), _, _) if len >= 4 => format!("{year:04}"),
        _ => raw.to_string(),
    }
}

/// The information dictionary and the catalogue's language, as what a
/// document says about itself.
fn meta(doc: &Document) -> DocMeta {
    let mut meta = DocMeta::default();
    if let Ok(info) = doc.trailer.get_deref(b"Info", doc).and_then(Object::as_dict) {
        let field = |key: &[u8]| {
            info.get_deref(key, doc)
                .ok()
                .and_then(text_of)
                .map(|text| collapse(&text))
                .filter(|text| !text.is_empty())
        };
        meta.title = field(b"Title").unwrap_or_default();
        meta.authors = field(b"Author").map(|names| split_names(&names)).unwrap_or_default();
        meta.description = field(b"Subject").unwrap_or_default();
        meta.subjects = field(b"Keywords").map(|words| split_keywords(&words)).unwrap_or_default();
        meta.published = field(b"CreationDate").map(|date| pdf_date(&date)).unwrap_or_default();
    }
    if let Some(language) = doc
        .catalog()
        .ok()
        .and_then(|catalog| catalog.get_deref(b"Lang", doc).ok())
        .and_then(text_of)
    {
        meta.language = language.trim().to_string();
    }
    meta
}

// ---- the outline --------------------------------------------------------------

/// One entry of a PDF's outline.
struct Entry {
    level: u8,
    title: String,
    page: u32,
}

/// The outline as sections. An outline names a page, not a place on it, and a
/// paper's page often starts three sections; so each entry is put at the
/// block on its page that begins with its title ("3.1 Encoder and Decoder
/// Stacks" for "Encoder and Decoder Stacks"), searched from where the entry
/// before it on that page was found, and otherwise at the first block of its
/// page, or of the next page that has one. An entry past the last block is
/// left out.
fn sections(doc: &Document, blocks: &[DocBlock]) -> Vec<DocSection> {
    let mut sections = Vec::new();
    // Where the last entry landed, so two headings on a page keep their order.
    let mut last: Option<(u32, usize)> = None;
    for entry in outline(doc) {
        let first = blocks.partition_point(|block| block.page < entry.page);
        if first >= blocks.len() {
            continue;
        }
        let page = blocks[first].page;
        let from = match last {
            Some((on, at)) if on == page => at.max(first),
            _ => first,
        };
        let end = blocks.partition_point(|block| block.page <= page);
        let at = (from..end).find(|&at| starts_with_title(&blocks[at].text, &entry.title));
        let at = at.unwrap_or(first);
        last = Some((page, at));
        sections.push(DocSection {
            title: entry.title,
            level: entry.level,
            page: 0,
            block: at as u32,
            parent: None,
        });
    }
    sections
}

/// Whether a block opens with a heading's title, its number aside: case,
/// spacing and punctuation do not count, and a leading "3.1" or "IV." in the
/// block need not be in the title.
fn starts_with_title(text: &str, title: &str) -> bool {
    let squash = |s: &str| -> String {
        s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
    };
    let title = squash(title);
    if title.len() < 3 {
        return false;
    }
    let head: String = text.chars().take(title.chars().count() + 24).collect();
    let head = squash(&head);
    let numbered = head.trim_start_matches(|c: char| c.is_ascii_digit());
    head.starts_with(&title) || numbered.starts_with(&title)
}

/// The outline's entries in order, with their depth. Walked with a list of
/// what is left to visit rather than by recursion, and never into an item
/// twice, so a file whose outline loops back on itself is read to the point
/// it does and no further.
fn outline(doc: &Document) -> Vec<Entry> {
    let mut entries = Vec::new();
    let Ok(catalog) = doc.catalog() else { return entries };
    let Ok(root) = catalog.get_deref(b"Outlines", doc).and_then(Object::as_dict) else {
        return entries;
    };
    let Some(first) = reference(root, b"First") else { return entries };
    let pages: HashMap<ObjectId, u32> =
        doc.get_pages().into_iter().map(|(number, id)| (id, number)).collect();
    let named = named_destinations(doc);

    let mut seen = HashSet::new();
    let mut todo = vec![(first, 1usize)];
    while let Some((id, level)) = todo.pop() {
        if entries.len() >= MAX_OUTLINE || !seen.insert(id) {
            continue;
        }
        let Ok(item) = doc.get_dictionary(id) else { continue };
        if let Some(entry) = entry_of(doc, item, level, &pages, &named) {
            entries.push(entry);
        }
        // The next sibling goes under the children, so the children come first.
        if let Some(next) = reference(item, b"Next") {
            todo.push((next, level));
        }
        if let Some(child) = reference(item, b"First") {
            todo.push((child, level + 1));
        }
    }
    entries
}

fn reference(item: &Dictionary, key: &[u8]) -> Option<ObjectId> {
    item.get(key).ok()?.as_reference().ok()
}

/// An outline item as an entry: its title, and the page it goes to.
fn entry_of(
    doc: &Document,
    item: &Dictionary,
    level: usize,
    pages: &HashMap<ObjectId, u32>,
    named: &HashMap<Vec<u8>, &Object>,
) -> Option<Entry> {
    let title = collapse(&text_of(item.get_deref(b"Title", doc).ok()?)?);
    if title.is_empty() {
        return None;
    }
    let destination = item.get_deref(b"Dest", doc).ok().or_else(|| {
        let action = item.get_deref(b"A", doc).ok()?.as_dict().ok()?;
        action.get_deref(b"D", doc).ok()
    })?;
    let page = page_of(doc, destination, pages, named, 0)?;
    Some(Entry { level: level.clamp(1, 255) as u8, title, page })
}

/// The page a destination names: an array that begins with a page, a
/// dictionary that holds one under `D`, or the name of one of the document's
/// named destinations.
fn page_of(
    doc: &Document,
    destination: &Object,
    pages: &HashMap<ObjectId, u32>,
    named: &HashMap<Vec<u8>, &Object>,
    depth: usize,
) -> Option<u32> {
    if depth > 4 {
        return None;
    }
    match destination {
        Object::Array(items) => match items.first()? {
            Object::Reference(id) => pages.get(id).copied(),
            _ => None,
        },
        Object::Dictionary(dict) => {
            page_of(doc, dict.get_deref(b"D", doc).ok()?, pages, named, depth + 1)
        }
        Object::Name(name) | Object::String(name, _) => {
            page_of(doc, named.get(name)?, pages, named, depth + 1)
        }
        Object::Reference(_) => {
            let (_, target) = doc.dereference(destination).ok()?;
            page_of(doc, target, pages, named, depth + 1)
        }
        _ => None,
    }
}

/// Every named destination the catalogue has, from the old `Dests` dictionary
/// and the `Names` tree both.
fn named_destinations(doc: &Document) -> HashMap<Vec<u8>, &Object> {
    let mut named = HashMap::new();
    let Ok(catalog) = doc.catalog() else { return named };
    if let Ok(dests) = catalog.get_deref(b"Dests", doc).and_then(Object::as_dict) {
        for (name, value) in dests.iter() {
            if let Ok((_, target)) = doc.dereference(value) {
                named.insert(name.clone(), target);
            }
        }
    }
    let tree = catalog
        .get_deref(b"Names", doc)
        .and_then(Object::as_dict)
        .and_then(|names| names.get_deref(b"Dests", doc))
        .and_then(Object::as_dict);
    if let Ok(tree) = tree {
        collect_names(doc, tree, &mut named, &mut HashSet::new(), 0);
    }
    named
}

/// One node of a name tree and everything under it, into `named`.
fn collect_names<'a>(
    doc: &'a Document,
    node: &'a Dictionary,
    named: &mut HashMap<Vec<u8>, &'a Object>,
    seen: &mut HashSet<ObjectId>,
    depth: usize,
) {
    if depth > 32 {
        return;
    }
    if let Ok(names) = node.get_deref(b"Names", doc).and_then(Object::as_array) {
        for pair in names.chunks(2) {
            let [key, value] = pair else { continue };
            let (Ok((_, key)), Ok((_, target))) = (doc.dereference(key), doc.dereference(value))
            else {
                continue;
            };
            if let Ok(name) = key.as_str() {
                named.insert(name.to_vec(), target);
            }
        }
    }
    if let Ok(kids) = node.get_deref(b"Kids", doc).and_then(Object::as_array) {
        for kid in kids {
            let Ok((id, Object::Dictionary(child))) = doc.dereference(kid) else { continue };
            if id.is_some_and(|id| !seen.insert(id)) {
                continue;
            }
            collect_names(doc, child, named, seen, depth + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use lopdf::content::{Content, Operation};
    use lopdf::{Stream, text_string};

    use super::*;

    #[test]
    fn a_heading_is_found_by_its_title_whatever_its_number() {
        assert!(starts_with_title("3.1 Encoder and Decoder Stacks", "Encoder and Decoder Stacks"));
        assert!(starts_with_title("1 Introduction\nRecurrent neural networks", "Introduction"));
        assert!(starts_with_title("ATTENTION. An attention function", "Attention"));
        assert!(!starts_with_title("An attention function can be described", "Attention"));
        assert!(!starts_with_title("Anything at all", "A"), "too short a title to match on");
    }

    #[test]
    fn lines_between_blank_ones_are_a_paragraph_and_wrapped_words_are_whole() {
        assert_eq!(
            paragraphs(
                "\nThe exam-\nple of a para-\ngraph that wraps\nover three lines.\n\n\n  Second   paragraph \nwell-\nKnown and 1990-\n2000.\n"
            ),
            [
                "The example of a paragraph that wraps over three lines.",
                "Second paragraph well- Known and 1990- 2000."
            ]
        );
        assert!(paragraphs("").is_empty());
        assert!(paragraphs("\n \n\t\n").is_empty());
    }

    #[test]
    fn a_sentence_that_ends_short_of_the_margin_ends_the_paragraph() {
        let text = "The first paragraph runs on to the full width of the column and\n\
                    then ends here, short.\n\
                    The second begins with a capital and fills the column to the\n\
                    margin as well as it can, then goes on a little more.\n\
                    • A bullet\n\
                    - another bullet\n\
                    and its wrapped line which is lowercase\n\
                    Not short but ends a sentence at the width of the whole column.\n\
                    Neither is this.";
        assert_eq!(
            paragraphs(text),
            [
                "The first paragraph runs on to the full width of the column and then ends here, short.",
                "The second begins with a capital and fills the column to the margin as well as it can, then goes on a little more.",
                "• A bullet",
                "- another bullet and its wrapped line which is lowercase Not short but ends a sentence at the width of the whole column. Neither is this.",
            ]
        );
    }

    #[test]
    fn ligatures_are_written_out_and_controls_dropped() {
        assert_eq!(tidy("e\u{fb03}cient \u{fb01}sh\u{fb02}y\0\u{7}\ttab"), "efficient fishfly tab");
    }

    #[test]
    fn ranges_of_pages_are_said_in_words() {
        assert_eq!(page_ranges(&[7]), "7");
        assert_eq!(page_ranges(&[3, 4, 5]), "3–5");
        assert_eq!(page_ranges(&[3, 4, 5, 9, 12, 13]), "3–5, 9 and 12–13");
        let many: Vec<u32> = (1..=40).step_by(2).collect();
        assert_eq!(page_ranges(&many), "1, 3, 5, 7, 9, 11, 13, 15, 17, 19 and others");
    }

    #[test]
    fn a_doi_is_found_where_a_sentence_leaves_it() {
        assert_eq!(find_doi("see doi:10.1038/NATURE12373."), Some("10.1038/nature12373".into()));
        assert_eq!(
            find_doi("https://doi.org/10.1016/S0140-6736(20)30183-5, and more"),
            Some("10.1016/s0140-6736(20)30183-5".into())
        );
        assert_eq!(find_doi("(10.12345/abc)"), Some("10.12345/abc".into()));
        // Too few digits, no slash, part of a longer number, nothing after the slash.
        assert_eq!(find_doi("10.12/abc 10.1234 x110.1234/abc 10.1234/ 10.1234/)"), None);
        assert_eq!(find_doi("version 10.5 then 10.5555/real"), Some("10.5555/real".into()));
        assert_eq!(find_doi(""), None);
    }

    #[test]
    fn dates_are_cut_down_to_what_they_say() {
        assert_eq!(pdf_date("D:20190301120000+01'00'"), "2019-03-01");
        assert_eq!(pdf_date("D:201903"), "2019-03");
        assert_eq!(pdf_date("D:2019"), "2019");
        assert_eq!(pdf_date("20190301"), "2019-03-01");
        assert_eq!(pdf_date("D:20191301"), "2019");
        assert_eq!(pdf_date("last Tuesday"), "last Tuesday");
    }

    #[test]
    fn text_strings_are_decoded_by_their_mark() {
        assert_eq!(text_of(&Object::string_literal("plain")).unwrap(), "plain");
        assert_eq!(text_of(&text_string("Caf\u{e9} \u{2713}")).unwrap(), "Caf\u{e9} \u{2713}");
        let little = Object::string_literal(vec![0xff, 0xfe, b'h', 0, b'i', 0]);
        assert_eq!(text_of(&little).unwrap(), "hi");
        let utf8 = Object::string_literal(vec![0xef, 0xbb, 0xbf, b'o', b'k']);
        assert_eq!(text_of(&utf8).unwrap(), "ok");
        assert_eq!(text_of(&Object::Integer(3)), None);
    }

    // ---- a PDF made here, for the whole of it

    /// A page whose content puts each `(y, text)` at the left margin, 12 point
    /// Helvetica, `y` up from the foot of the page.
    fn page_of_lines(doc: &mut Document, parent: ObjectId, lines: &[(i64, &str)]) -> ObjectId {
        let mut operations =
            vec![Operation::new("BT", vec![]), Operation::new("Tf", vec!["F1".into(), 12.into()])];
        let (mut x, mut y) = (0i64, 0i64);
        for &(at, text) in lines {
            operations.push(Operation::new("Td", vec![(72 - x).into(), (at - y).into()]));
            operations.push(Operation::new("Tj", vec![Object::string_literal(text)]));
            (x, y) = (72, at);
        }
        operations.push(Operation::new("ET", vec![]));
        let content = Content { operations };
        let content_id = doc.add_object(Stream::new(Dictionary::new(), content.encode().unwrap()));

        let mut font = Dictionary::new();
        font.set("Type", "Font");
        font.set("Subtype", "Type1");
        font.set("BaseFont", "Helvetica");
        let mut fonts = Dictionary::new();
        fonts.set("F1", font);
        let mut resources = Dictionary::new();
        resources.set("Font", fonts);
        let media_box: Vec<Object> = vec![0.into(), 0.into(), 612.into(), 792.into()];

        let mut page = Dictionary::new();
        page.set("Type", "Page");
        page.set("Parent", parent);
        page.set("Contents", content_id);
        page.set("Resources", resources);
        page.set("MediaBox", media_box);
        doc.add_object(page)
    }

    fn outline_item(title: &str, parent: ObjectId, page: ObjectId) -> Dictionary {
        let mut item = Dictionary::new();
        item.set("Title", text_string(title));
        item.set("Parent", parent);
        item.set("Dest", vec![Object::Reference(page), "Fit".into()]);
        item
    }

    /// Three pages, the third without text; an outline with a repeated title
    /// and a child; and an information dictionary.
    fn tiny_pdf() -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let one = page_of_lines(
            &mut doc,
            pages_id,
            &[
                (700, "First paragraph of the first page,"),
                (686, "which wraps onto a second line."),
                (646, "A separate second paragraph."),
                (600, "doi:10.1038/nature12373."),
            ],
        );
        let two = page_of_lines(&mut doc, pages_id, &[(700, "Page two says something else.")]);
        let three = page_of_lines(&mut doc, pages_id, &[]);

        let mut pages = Dictionary::new();
        pages.set("Type", "Pages");
        pages.set(
            "Kids",
            vec![Object::Reference(one), Object::Reference(two), Object::Reference(three)],
        );
        pages.set("Count", 3);
        doc.objects.insert(pages_id, Object::Dictionary(pages));

        let outlines_id = doc.new_object_id();
        let [a, a1, b, c, d] = [(); 5].map(|()| doc.new_object_id());
        let mut item_a = outline_item("Summary", outlines_id, one);
        item_a.set("Next", b);
        item_a.set("First", a1);
        item_a.set("Last", a1);
        let item_a1 = outline_item("Detail", a, one);
        let mut item_b = outline_item("Summary", outlines_id, two);
        item_b.set("Prev", a);
        item_b.set("Next", c);
        let mut item_c = outline_item("Scanned", outlines_id, three);
        item_c.set("Prev", b);
        item_c.set("Next", d);
        let mut item_d = Dictionary::new();
        item_d.set("Title", "no title object");
        item_d.set("Parent", outlines_id);
        for (id, item) in [(a, item_a), (a1, item_a1), (b, item_b), (c, item_c), (d, item_d)] {
            doc.objects.insert(id, Object::Dictionary(item));
        }
        let mut outlines = Dictionary::new();
        outlines.set("Type", "Outlines");
        outlines.set("First", a);
        outlines.set("Last", d);
        outlines.set("Count", 4);
        doc.objects.insert(outlines_id, Object::Dictionary(outlines));

        let mut catalog = Dictionary::new();
        catalog.set("Type", "Catalog");
        catalog.set("Pages", pages_id);
        catalog.set("Outlines", outlines_id);
        catalog.set("Lang", Object::string_literal("en-GB"));
        let catalog_id = doc.add_object(catalog);
        doc.trailer.set("Root", catalog_id);

        let mut info = Dictionary::new();
        info.set("Title", text_string("A Tiny T\u{e9}st"));
        info.set(
            "Author",
            Object::string_literal("Ada Lovelace; Charles Babbage and Mary Somerville"),
        );
        info.set("Subject", Object::string_literal("Engines"));
        info.set("Keywords", Object::string_literal("analysis, engines; notes"));
        info.set("CreationDate", Object::string_literal("D:20190301120000Z"));
        let info_id = doc.add_object(info);
        doc.trailer.set("Info", info_id);

        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn a_pdf_is_read_page_by_page_with_its_outline_and_what_it_says_of_itself() {
        let doc = parse(&tiny_pdf()).unwrap();
        assert_eq!(doc.format, DocFormat::Pdf);
        assert_eq!(doc.pages, 3);

        let on_page = |page: u32| -> Vec<&str> {
            doc.blocks.iter().filter(|b| b.page == page).map(|b| b.text.as_str()).collect()
        };
        let first = on_page(1);
        assert!(
            first.iter().any(|t| t.contains("First paragraph") && t.contains("second line")),
            "{first:?}"
        );
        assert!(first.iter().any(|t| t.contains("separate second paragraph")), "{first:?}");
        // The blank between them is a paragraph break.
        assert!(first.len() >= 2, "{first:?}");
        assert!(on_page(2).iter().any(|t| t.contains("Page two says")));
        assert!(on_page(3).is_empty());
        assert!(doc.blocks.iter().all(|b| b.kind == DocBlockKind::Paragraph && b.html.is_empty()));
        assert_eq!(doc.warnings, ["Page 3 has no text in it; it may be a scanned picture."]);

        assert_eq!(doc.meta.title, "A Tiny T\u{e9}st");
        assert_eq!(doc.meta.authors, ["Ada Lovelace", "Charles Babbage", "Mary Somerville"]);
        assert_eq!(doc.meta.description, "Engines");
        assert_eq!(doc.meta.subjects, ["analysis", "engines", "notes"]);
        assert_eq!(doc.meta.published, "2019-03-01");
        assert_eq!(doc.meta.language, "en-GB");
        assert_eq!(doc.meta.identifiers, ["doi:10.1038/nature12373"]);

        // Both "Summary" entries survive, each at the first block of its page;
        // the entry for the page with no text is past the last block.
        assert_eq!(doc.sections.len(), 3);
        let mut summaries: Vec<u32> = doc
            .sections
            .iter()
            .filter(|s| s.title == "Summary")
            .map(|s| doc.blocks[s.block as usize].page)
            .collect();
        summaries.sort_unstable();
        assert_eq!(summaries, [1, 2]);
        let detail = doc.sections.iter().find(|s| s.title == "Detail").unwrap();
        assert_eq!(detail.level, 2);
        assert_eq!(doc.blocks[detail.block as usize].page, 1);
        assert!(doc.sections.iter().all(|s| s.title != "Scanned"));
    }

    #[test]
    fn a_pdf_with_no_text_at_all_is_still_a_document_with_a_warning() {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let blank = page_of_lines(&mut doc, pages_id, &[]);
        let mut pages = Dictionary::new();
        pages.set("Type", "Pages");
        pages.set("Kids", vec![Object::Reference(blank)]);
        pages.set("Count", 1);
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let mut catalog = Dictionary::new();
        catalog.set("Type", "Catalog");
        catalog.set("Pages", pages_id);
        let catalog_id = doc.add_object(catalog);
        doc.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();

        let doc = parse(&bytes).unwrap();
        assert_eq!(doc.pages, 1);
        assert!(doc.blocks.is_empty());
        assert!(doc.sections.is_empty());
        assert_eq!(doc.warnings, ["Page 1 has no text in it; it may be a scanned picture."]);
    }

    #[test]
    fn a_damaged_pdf_says_so() {
        let mut bytes = tiny_pdf();
        bytes.truncate(40);
        assert!(super::super::guarded(|| parse(&bytes)).is_err());
        assert!(super::super::guarded(|| parse(b"hello")).is_err());
    }
}
