//! Reading epub. See [`super`].
//!
//! An EPUB is a zip archive: `META-INF/container.xml` names the package file;
//! the package gives the book's metadata, the manifest of every file in it
//! and the spine, the order the chapters are read in; and a navigation
//! document (EPUB 3) or an NCX (EPUB 2) gives the table of contents. Each
//! spine item is walked with [`super::html`]'s walk into one [`DocChapter`].
//!
//! The archive is a stranger's file, so what is read out of it is capped:
//! files read, each file's size once decompressed, all of them together, and
//! the blocks made of them. Without that, a few kilobytes of zip that
//! decompress to gigabytes would be read into memory whole.

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};

use dom_query::{Document as Dom, Node};
use jatayu_core::documents::{DocChapter, DocFormat, DocMeta, DocSection, DocText};
use zip::ZipArchive;

use super::html::{Exists, MAX_BLOCKS, Walk, link_target, resolve_path};
use super::{ParseError, Resource, collapse, decode};

const NOT_AN_EPUB: &str = "This is not an EPUB book, or it is damaged.";
const TOO_MANY_FILES: &str = "This book has too many files inside it to be read.";
const NO_PACKAGE: &str = "This EPUB book has no package file in it, so it could not be read.";
const PROTECTED: &str = "This book is protected by copy protection, so it cannot be read.";
const EMPTY: &str = "This book has no text in it that could be read.";

/// The largest picture handed back by [`resource`].
const MAX_RESOURCE_BYTES: u64 = 16 * 1024 * 1024;

/// The most entries a table of contents is read for.
const MAX_ENTRIES: usize = 20_000;

/// Algorithms that only scramble a font, which a reader does not need to
/// read: anything else in `encryption.xml` is copy protection.
const FONT_OBFUSCATION: [&str; 2] =
    ["http://www.idpf.org/2008/embedding", "http://ns.adobe.com/pdf/enc#RC"];

/// What an archive may cost to read. See the module docs.
#[derive(Clone, Copy)]
struct Limits {
    /// Files the archive may list.
    listed: usize,
    /// Files that may be decompressed.
    entries: usize,
    /// One file's size once decompressed.
    entry: u64,
    /// All the files read, together.
    total: u64,
    /// Blocks made of them.
    blocks: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            listed: 100_000,
            entries: 10_000,
            entry: 64 * 1024 * 1024,
            total: 512 * 1024 * 1024,
            blocks: MAX_BLOCKS,
        }
    }
}

// ---- the archive ------------------------------------------------------------

/// The names in an archive, by exact path and, failing that, by path without
/// regard to case: a book whose manifest says `Images/Map.PNG` for a file the
/// archive has as `images/map.png` is read by the readers that do the same.
#[derive(Default)]
struct Names {
    /// Every entry's name by its index, an empty one for a directory.
    list: Vec<String>,
    exact: HashMap<String, usize>,
    folded: HashMap<String, usize>,
}

impl Names {
    fn add(&mut self, index: usize, name: String) {
        let name = name.trim_start_matches('/').to_string();
        if !name.is_empty() && !name.ends_with('/') {
            self.exact.insert(name.clone(), index);
            self.folded.entry(name.to_lowercase()).or_insert(index);
        }
        self.list.push(name);
    }

    fn find(&self, path: &str) -> Option<usize> {
        self.exact.get(path).or_else(|| self.folded.get(&path.to_lowercase())).copied()
    }

    /// The name the archive has `path` under.
    fn canonical(&self, path: &str) -> Option<&str> {
        self.find(path).map(|index| self.list[index].as_str())
    }
}

/// An archive being read, and what it has cost so far.
struct Reader<'a> {
    zip: ZipArchive<Cursor<&'a [u8]>>,
    entries: usize,
    bytes: u64,
    limits: Limits,
}

impl Reader<'_> {
    /// The file at `path`, decompressed, if it is there and under `cap` bytes
    /// and within what is left of the archive's limits. The size the archive
    /// declares is not believed: the read stops a byte past what is allowed.
    fn read(&mut self, names: &Names, path: &str, cap: u64) -> Option<Vec<u8>> {
        let index = names.find(path)?;
        if self.entries >= self.limits.entries {
            return None;
        }
        let allowed = cap.min(self.limits.total.saturating_sub(self.bytes));
        let file = self.zip.by_index(index).ok()?;
        if file.size() > allowed {
            return None;
        }
        let mut out = Vec::new();
        file.take(allowed + 1).read_to_end(&mut out).ok()?;
        if out.len() as u64 > allowed {
            return None;
        }
        self.entries += 1;
        self.bytes += out.len() as u64;
        Some(out)
    }
}

fn open(bytes: &[u8], limits: Limits) -> Result<(Reader<'_>, Names), ParseError> {
    let zip = ZipArchive::new(Cursor::new(bytes)).map_err(|_| ParseError(NOT_AN_EPUB.into()))?;
    if zip.len() > limits.listed {
        return Err(ParseError(TOO_MANY_FILES.into()));
    }
    let mut names = Names::default();
    for index in 0..zip.len() {
        let name = match zip.name_for_index(index) {
            Some(Ok(name)) => name.replace('\\', "/"),
            _ => String::new(),
        };
        names.add(index, name);
    }
    Ok((Reader { zip, entries: 0, bytes: 0, limits }, names))
}

/// The file at `path` in the book `bytes`, if it is a picture and it is
/// there: what the reader draws, or a library item's cover.
pub fn resource(bytes: &[u8], path: &str) -> Option<Resource> {
    let mime = mime_of(path)?;
    let (mut reader, names) = open(bytes, Limits::default()).ok()?;
    let found = reader.read(&names, path, MAX_RESOURCE_BYTES)?;
    Some(Resource { mime: mime.to_string(), bytes: found })
}

/// The type of a picture by its file's extension: the ones a webview draws.
fn mime_of(path: &str) -> Option<&'static str> {
    let extension = path.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match extension.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

// ---- the package ------------------------------------------------------------

/// One file the manifest lists.
struct Item {
    id: String,
    /// Its name in the archive.
    path: String,
    media: String,
    properties: String,
}

/// What a spine item is.
#[derive(PartialEq, Eq)]
enum Part {
    /// A page to be walked.
    Markup,
    /// A picture that is a page of its own.
    Picture,
    /// Anything else: not read.
    Other,
}

impl Item {
    fn part(&self) -> Part {
        let media = self.media.to_ascii_lowercase();
        let extension =
            self.path.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).unwrap_or_default();
        if matches!(
            media.as_str(),
            "application/xhtml+xml" | "text/html" | "image/svg+xml" | "application/x-dtbook+xml"
        ) {
            Part::Markup
        } else if media.starts_with("image/") {
            Part::Picture
        } else if media.is_empty() && matches!(extension.as_str(), "xhtml" | "html" | "htm" | "xml")
        {
            Part::Markup
        } else {
            Part::Other
        }
    }
}

/// What the package file says.
struct Package {
    meta: DocMeta,
    items: Vec<Item>,
    /// The reading order, as indexes into `items`.
    spine: Vec<usize>,
    /// The EPUB 3 navigation document.
    nav: Option<usize>,
    /// The EPUB 2 NCX.
    ncx: Option<usize>,
}

/// XML read leniently as to its DTD: an NCX has one, and a package may. The
/// parser's own limits on what a DTD can expand to are what stand against a
/// hostile one.
fn xml(text: &str) -> Option<roxmltree::Document<'_>> {
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        nodes_limit: 2_000_000,
        ..roxmltree::ParsingOptions::default()
    };
    roxmltree::Document::parse_with_options(text.trim_start_matches('\u{feff}'), options).ok()
}

/// Everything written inside `node`, tags and all taken out.
fn all_text(node: roxmltree::Node<'_, '_>) -> String {
    node.descendants().filter(|inner| inner.is_text()).filter_map(|inner| inner.text()).collect()
}

/// Whether `list`, a space-separated attribute, has `token`.
fn has_token(list: &str, token: &str) -> bool {
    list.split_ascii_whitespace().any(|word| word.eq_ignore_ascii_case(token))
}

/// Where the package file is: the first `rootfile` of the container, or
/// failing that the first `.opf` there is.
fn package_path(reader: &mut Reader<'_>, names: &Names) -> Option<String> {
    let cap = reader.limits.entry;
    if let Some(raw) = reader.read(names, "META-INF/container.xml", cap) {
        let text = decode(&raw, true);
        let path = xml(&text)
            .and_then(|doc| {
                let rootfile = doc.descendants().find(|node| node.has_tag_name("rootfile"))?;
                rootfile.attribute("full-path").map(|path| resolve_path("", path))
            })
            .and_then(|path| names.canonical(&path).map(str::to_string));
        if path.is_some() {
            return path;
        }
    }
    names.list.iter().find(|name| name.to_ascii_lowercase().ends_with(".opf")).cloned()
}

fn read_package(opf_path: &str, text: &str, names: &Names) -> Option<Package> {
    let doc = xml(text)?;
    let located = |href: &str| {
        let path = resolve_path(opf_path, href);
        names.canonical(&path).map(str::to_string).unwrap_or(path)
    };
    let mut meta = DocMeta::default();
    let mut cover_id = None;
    let mut items: Vec<Item> = Vec::new();
    let mut spine_ids: Vec<String> = Vec::new();
    let mut toc_id = None;
    for section in doc.root_element().children().filter(|node| node.is_element()) {
        match section.tag_name().name() {
            "metadata" => read_metadata(section, &mut meta, &mut cover_id),
            "manifest" => {
                for item in section.children().filter(|node| node.has_tag_name("item")) {
                    let Some(href) = item.attribute("href") else { continue };
                    items.push(Item {
                        id: item.attribute("id").unwrap_or_default().to_string(),
                        path: located(href),
                        media: item.attribute("media-type").unwrap_or_default().to_string(),
                        properties: item.attribute("properties").unwrap_or_default().to_string(),
                    });
                }
            }
            "spine" => {
                toc_id = section.attribute("toc").map(str::to_string);
                spine_ids.extend(
                    section
                        .children()
                        .filter(|node| node.has_tag_name("itemref"))
                        .filter_map(|node| node.attribute("idref"))
                        .map(str::to_string),
                );
            }
            _ => {}
        }
    }

    let by_id: HashMap<&str, usize> =
        items.iter().enumerate().map(|(at, item)| (item.id.as_str(), at)).collect();
    // The cover: an EPUB 3 item that says so, else the one an EPUB 2
    // `<meta name="cover">` names -- if it is a picture, not a cover page.
    let cover =
        items.iter().position(|item| has_token(&item.properties, "cover-image")).or_else(|| {
            let at = cover_id.as_deref().and_then(|id| by_id.get(id).copied())?;
            items[at].media.to_ascii_lowercase().starts_with("image/").then_some(at)
        });
    if let Some(at) = cover
        && names.find(&items[at].path).is_some()
    {
        meta.cover = items[at].path.clone();
    }
    let mut spine: Vec<usize> =
        spine_ids.iter().filter_map(|id| by_id.get(id.as_str()).copied()).collect();
    if spine.is_empty() {
        spine = (0..items.len()).filter(|&at| items[at].part() == Part::Markup).collect();
    }
    let nav = items.iter().position(|item| has_token(&item.properties, "nav"));
    let ncx = toc_id.as_deref().and_then(|id| by_id.get(id).copied()).or_else(|| {
        items.iter().position(|item| item.media.eq_ignore_ascii_case("application/x-dtbncx+xml"))
    });
    Some(Package { meta, items, spine, nav, ncx })
}

/// The `<metadata>` of the package into `meta`; and, for an EPUB 2 book, the
/// manifest id its `<meta name="cover">` names into `cover`.
fn read_metadata(node: roxmltree::Node<'_, '_>, meta: &mut DocMeta, cover: &mut Option<String>) {
    for field in node.children().filter(|inner| inner.is_element()) {
        let text = collapse(&all_text(field));
        match field.tag_name().name() {
            "title" if meta.title.is_empty() => meta.title = text,
            "creator" => push_new(&mut meta.authors, text),
            "language" if meta.language.is_empty() => meta.language = text,
            "publisher" if meta.publisher.is_empty() => meta.publisher = text,
            "date" if meta.published.is_empty() => meta.published = text,
            "description" if meta.description.is_empty() => {
                meta.description = plain(&all_text(field));
            }
            "subject" => push_new(&mut meta.subjects, text),
            "identifier" => {
                let scheme = field
                    .attributes()
                    .find(|attribute| attribute.name() == "scheme")
                    .map(|attribute| attribute.value());
                if let Some(id) = identifier(&text, scheme) {
                    push_new(&mut meta.identifiers, id);
                }
            }
            "meta" if field.attribute("name") == Some("cover") => {
                *cover = field.attribute("content").map(str::to_string);
            }
            _ => {}
        }
    }
}

fn push_new(list: &mut Vec<String>, value: String) {
    if !value.is_empty() && !list.contains(&value) {
        list.push(value);
    }
}

/// What `text` says once any markup in it is gone and any entity decoded: a
/// description is often HTML that has been escaped into XML.
fn plain(text: &str) -> String {
    if !text.contains(['<', '&']) {
        return collapse(text);
    }
    let fragment = Dom::fragment(text);
    fragment.select("script, style").remove();
    collapse(&fragment.formatted_text())
}

/// A `dc:identifier` as `isbn:…` or `doi:…`, lowercase, or `None` for one that
/// is neither (a UUID, a URL). An ISBN is told by `urn:isbn:` or a scheme of
/// ISBN, or by being thirteen digits that begin 978 or 979.
fn identifier(text: &str, scheme: Option<&str>) -> Option<String> {
    let lower = text.trim().to_ascii_lowercase();
    let bare = lower.strip_prefix("urn:").unwrap_or(&lower);
    if let Some(doi) = bare.strip_prefix("doi:") {
        return doi_of(doi);
    }
    if let Some(isbn) = bare.strip_prefix("isbn:") {
        return isbn_of(isbn);
    }
    match scheme.map(str::to_ascii_lowercase).as_deref() {
        Some("isbn") => return isbn_of(bare),
        Some("doi") => return doi_of(bare),
        _ => {}
    }
    if bare.starts_with("10.") {
        return doi_of(bare);
    }
    // A bare number is an ISBN only when it is unmistakably one: thirteen
    // digits under a Bookland prefix, not any ten digits a publisher used.
    isbn_of(bare).filter(|isbn| {
        let digits = isbn.strip_prefix("isbn:").unwrap_or(isbn);
        digits.len() == 13 && (digits.starts_with("978") || digits.starts_with("979"))
    })
}

fn doi_of(text: &str) -> Option<String> {
    let doi = text.trim();
    (doi.starts_with("10.") && doi.contains('/')).then(|| format!("doi:{doi}"))
}

fn isbn_of(text: &str) -> Option<String> {
    let digits: String = text.chars().filter(|ch| !matches!(ch, '-' | ' ')).collect();
    let body = digits.strip_suffix('x').unwrap_or(&digits);
    let valid = (digits.len() == 13 && digits.bytes().all(|byte| byte.is_ascii_digit()))
        || (digits.len() == 10 && body.bytes().all(|byte| byte.is_ascii_digit()));
    valid.then(|| format!("isbn:{digits}"))
}

// ---- the table of contents ------------------------------------------------------

/// One entry of a table of contents, aimed at a place in a file.
struct Entry {
    level: u8,
    title: String,
    path: String,
    fragment: String,
}

/// The entries of an EPUB 3 navigation document's `toc`, nested lists and
/// all, `nav_path` being the file it is in.
fn nav_entries(html: &str, nav_path: &str, exists: Exists<'_>) -> Vec<Entry> {
    let page = Dom::from(html);
    let navs = page.select("nav");
    let toc = navs
        .nodes()
        .iter()
        .find(|nav| nav.attr("epub:type").is_some_and(|kinds| has_token(&kinds, "toc")))
        .or_else(|| navs.nodes().first());
    let mut entries = Vec::new();
    if let Some(nav) = toc
        && let Some(list) = nav.descendants_it().find(|node| node.has_name("ol"))
    {
        nav_list(&list, 1, nav_path, exists, &mut entries, 0);
    }
    entries
}

/// The `li`s of one `ol`, each a link (or a label over a list of links), and
/// the lists inside them one level down. An entry with a label and no link is
/// aimed where its first link is.
fn nav_list(
    list: &Node<'_>,
    level: u8,
    base: &str,
    exists: Exists<'_>,
    out: &mut Vec<Entry>,
    depth: usize,
) {
    if depth > 32 {
        return;
    }
    for item in list.children_it(false).filter(|node| node.has_name("li")) {
        if out.len() >= MAX_ENTRIES {
            return;
        }
        let label =
            item.children_it(false).find(|node| node.has_name("a") || node.has_name("span"));
        let title = label.as_ref().map(|node| collapse(&node.text())).unwrap_or_default();
        let href = label
            .as_ref()
            .and_then(|node| node.attr("href"))
            .filter(|href| !href.trim().is_empty())
            .or_else(|| {
                item.descendants_it()
                    .find(|node| node.has_name("a") && node.has_attr("href"))
                    .and_then(|node| node.attr("href"))
            });
        if !title.is_empty()
            && let Some(href) = href
            && let Some((path, fragment)) = link_target(base, &href, Some(exists))
        {
            out.push(Entry { level, title, path, fragment });
        }
        for sub in item.children_it(false).filter(|node| node.has_name("ol")) {
            nav_list(&sub, level.saturating_add(1), base, exists, out, depth + 1);
        }
    }
}

/// The entries of an EPUB 2 NCX.
fn ncx_entries(text: &str, ncx_path: &str, exists: Exists<'_>) -> Vec<Entry> {
    let mut entries = Vec::new();
    if let Some(doc) = xml(text)
        && let Some(map) = doc.descendants().find(|node| node.has_tag_name("navMap"))
    {
        ncx_points(map, 1, ncx_path, exists, &mut entries, 0);
    }
    entries
}

fn ncx_points(
    node: roxmltree::Node<'_, '_>,
    level: u8,
    base: &str,
    exists: Exists<'_>,
    out: &mut Vec<Entry>,
    depth: usize,
) {
    if depth > 32 {
        return;
    }
    for point in node.children().filter(|inner| inner.has_tag_name("navPoint")) {
        if out.len() >= MAX_ENTRIES {
            return;
        }
        let title = point
            .children()
            .find(|inner| inner.has_tag_name("navLabel"))
            .map(|label| collapse(&all_text(label)))
            .unwrap_or_default();
        let source = point
            .children()
            .find(|inner| inner.has_tag_name("content"))
            .and_then(|content| content.attribute("src"));
        if !title.is_empty()
            && let Some(source) = source
            && let Some((path, fragment)) = link_target(base, source, Some(exists))
        {
            out.push(Entry { level, title, path, fragment });
        }
        ncx_points(point, level.saturating_add(1), base, exists, out, depth + 1);
    }
}

/// The entries as sections, each at the block its place ended up as. An entry
/// whose place is not in the book is left out.
fn sections_of(entries: &[Entry], walk: &Walk<'_>) -> Vec<DocSection> {
    entries
        .iter()
        .filter_map(|entry| {
            let block = walk.resolve(&entry.path, &entry.fragment)?;
            Some(DocSection {
                title: entry.title.clone(),
                level: entry.level,
                page: 0,
                block: block as u32,
                parent: None,
            })
        })
        .collect()
}

// ---- copy protection ---------------------------------------------------------

/// The files `META-INF/encryption.xml` says are encrypted in a way that is
/// not just a font being scrambled.
fn protected_files(reader: &mut Reader<'_>, names: &Names) -> HashSet<String> {
    let mut files = HashSet::new();
    let cap = reader.limits.entry;
    let Some(raw) = reader.read(names, "META-INF/encryption.xml", cap) else { return files };
    let text = decode(&raw, true);
    let Some(doc) = xml(&text) else { return files };
    for data in doc.descendants().filter(|node| node.has_tag_name("EncryptedData")) {
        let algorithm = data
            .descendants()
            .find(|node| node.has_tag_name("EncryptionMethod"))
            .and_then(|node| node.attribute("Algorithm"))
            .unwrap_or_default();
        if FONT_OBFUSCATION.contains(&algorithm) {
            continue;
        }
        if let Some(uri) = data
            .descendants()
            .find(|node| node.has_tag_name("CipherReference"))
            .and_then(|node| node.attribute("URI"))
        {
            let path = resolve_path("", uri);
            files.insert(names.canonical(&path).map(str::to_string).unwrap_or(path));
        }
    }
    files
}

// ---- the book ---------------------------------------------------------------

/// Read an EPUB: its metadata, its spine as chapters of blocks, its
/// navigation as sections (or its headings, without one), and its cover.
pub fn parse(bytes: &[u8]) -> Result<DocText, ParseError> {
    read_book(bytes, Limits::default())
}

fn read_book(bytes: &[u8], limits: Limits) -> Result<DocText, ParseError> {
    let (mut reader, names) = open(bytes, limits)?;
    let opf_path =
        package_path(&mut reader, &names).ok_or_else(|| ParseError(NO_PACKAGE.into()))?;
    let raw = reader
        .read(&names, &opf_path, limits.entry)
        .ok_or_else(|| ParseError(NO_PACKAGE.into()))?;
    let package = read_package(&opf_path, &decode(&raw, true), &names)
        .ok_or_else(|| ParseError(NO_PACKAGE.into()))?;
    let protected = protected_files(&mut reader, &names);

    // A picture is drawn, and a link followed, only into a file that is there.
    let exists = |path: &str| names.canonical(path).map(str::to_string);
    let mut walk = Walk::new(Some(&exists), limits.blocks);
    let mut chapters = Vec::new();
    let mut unreadable = 0usize;
    let mut copy_protected = false;
    let mut done = HashSet::new();
    for &at in &package.spine {
        let item = &package.items[at];
        if !done.insert(at) {
            continue;
        }
        let first = walk.blocks.len();
        match item.part() {
            Part::Other => continue,
            Part::Picture => walk.image(&item.path),
            Part::Markup => {
                if protected.contains(&item.path) {
                    copy_protected = true;
                    unreadable += 1;
                    continue;
                }
                let Some(raw) = reader.read(&names, &item.path, limits.entry) else {
                    unreadable += 1;
                    continue;
                };
                let source = decode(&raw, true);
                walk.page(&Dom::from(source.as_str()), &item.path);
            }
        }
        let count = walk.blocks.len() - first;
        if count > 0 {
            chapters.push(DocChapter {
                first_block: first as u32,
                blocks: count as u32,
                href: item.path.clone(),
            });
        }
        if walk.capped {
            break;
        }
    }
    walk.finish();

    // The contents: the navigation document if it gives any place that is in
    // the book, else the NCX, else (left to the caller) the headings.
    let mut sections = Vec::new();
    if let Some(nav) = package.nav.map(|at| &package.items[at])
        && let Some(raw) = reader.read(&names, &nav.path, limits.entry)
    {
        sections = sections_of(&nav_entries(&decode(&raw, true), &nav.path, &exists), &walk);
    }
    if sections.is_empty()
        && let Some(ncx) = package.ncx.map(|at| &package.items[at])
        && let Some(raw) = reader.read(&names, &ncx.path, limits.entry)
    {
        sections = sections_of(&ncx_entries(&decode(&raw, true), &ncx.path, &exists), &walk);
    }

    let mut doc = DocText::new(DocFormat::Epub);
    doc.meta = package.meta;
    if unreadable > 0 {
        doc.warnings.push(match unreadable {
            1 => "1 part of this book could not be read.".to_string(),
            n => format!("{n} parts of this book could not be read."),
        });
    }
    if walk.capped {
        doc.warnings.push(format!(
            "This book is very long; only the first {} paragraphs were read.",
            limits.blocks
        ));
    }
    doc.blocks = walk.blocks;
    if doc.blocks.is_empty() {
        let why = if copy_protected { PROTECTED } else { EMPTY };
        return Err(ParseError(why.into()));
    }
    doc.chapters = chapters;
    doc.sections = sections;
    super::finish_flow(&mut doc);
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use jatayu_core::documents::DocBlockKind;

    use super::*;

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in files {
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    const CONTAINER: &str = r##"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"##;

    const OPF3: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
 <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:identifier id="id">urn:isbn:978-0-14-143951-8</dc:identifier>
  <dc:identifier>urn:uuid:1234-5678</dc:identifier>
  <dc:title>The Test Book</dc:title>
  <dc:title>Not This One</dc:title>
  <dc:creator>Jane Austen</dc:creator>
  <dc:creator>Someone Else</dc:creator>
  <dc:language>en-GB</dc:language>
  <dc:publisher>Penguin</dc:publisher>
  <dc:date>1813-01-28</dc:date>
  <dc:description>&lt;p&gt;A &lt;b&gt;test&lt;/b&gt; book &amp;amp; more.&lt;/p&gt;</dc:description>
  <dc:subject>Fiction</dc:subject>
  <dc:subject>Romance</dc:subject>
 </metadata>
 <manifest>
  <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
  <item id="c1" href="text/ch1.xhtml" media-type="application/xhtml+xml"/>
  <item id="c2" href="text/ch2.xhtml" media-type="application/xhtml+xml"/>
  <item id="cover" href="images/cover%20art.jpg" media-type="image/jpeg" properties="cover-image"/>
  <item id="map" href="images/map.png" media-type="image/png"/>
 </manifest>
 <spine><itemref idref="c1"/><itemref idref="c2"/><itemref idref="c1"/><itemref idref="ghost"/></spine>
</package>"##;

    const NAV3: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Contents</title></head><body>
<nav epub:type="landmarks"><ol><li><a href="text/ch2.xhtml">Landmark</a></li></ol></nav>
<nav epub:type="toc" id="toc"><h1>Contents</h1><ol>
 <li><a href="text/ch1.xhtml">Chapter One</a>
   <ol><li><a href="text/ch1.xhtml#sec">A Section</a></li></ol></li>
 <li><span>Part Two</span><ol><li><a href="text/ch2.xhtml">Chapter Two</a></li></ol></li>
 <li><a href="text/missing.xhtml">Missing</a></li>
 <li><a href="text/ch1.xhtml#nowhere">Nowhere</a></li>
</ol></nav>
</body></html>"##;

    const CH1: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Ch 1</title><style>p { color: red }</style></head><body>
<h1 id="top">Chapter One</h1>
<p id="first">It is a truth <em>universally</em> acknowledged, that a <strong>single</strong> man<sup><a href="#fn1" id="r1">1</a></sup> &amp; more.</p>
<div id="wrap"><p>Inside a div.</p></div>
<h2 id="sec">A Section</h2>
<p>See <a href="ch2.xhtml#dest">the next chapter</a>, <a href="ch2.xhtml">chapter two</a> and <a href="https://example.com/">the web</a>.</p>
<blockquote><p>Quoted one.</p><p>Quoted two.</p></blockquote>
<ul><li>First item</li><li>Second <b>item</b></li></ul>
<pre>  indented
code</pre>
<hr/>
<p><img src="../images/map.png" alt="A map of the county"/><img src="../images/gone.png" alt="not in the book"/></p>
<table><tr><th>Name</th><th>Age</th></tr><tr><td>Ann</td><td>31</td></tr></table>
<div>Loose text in a div</div>
<aside id="fn1"><p>The footnote text.</p></aside>
</body></html>"##;

    const CH2: &str = r##"<html xmlns="http://www.w3.org/1999/xhtml"><body>
<h1>Chapter Two</h1>
<p id="dest">The destination paragraph, with a <a href="ch1.xhtml#r1">back link</a>.</p>
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><image xlink:href="../images/map.png" width="10" height="10"/></svg>
<p>Last.</p>
</body></html>"##;

    fn book3() -> Vec<u8> {
        zip_of(&[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", CONTAINER.as_bytes()),
            ("OEBPS/content.opf", OPF3.as_bytes()),
            ("OEBPS/nav.xhtml", NAV3.as_bytes()),
            ("OEBPS/text/ch1.xhtml", CH1.as_bytes()),
            ("OEBPS/text/ch2.xhtml", CH2.as_bytes()),
            ("OEBPS/images/cover art.jpg", b"jpeg bytes"),
            ("OEBPS/images/map.png", b"png bytes"),
        ])
    }

    fn block_of(doc: &DocText, text: &str) -> usize {
        doc.blocks.iter().position(|block| block.text == text).unwrap_or_else(|| panic!("{text}"))
    }

    #[test]
    fn an_epub3_is_read_with_its_metadata_chapters_and_blocks() {
        let doc = parse(&book3()).unwrap();
        assert_eq!(doc.format, DocFormat::Epub);
        assert_eq!(doc.meta.title, "The Test Book");
        assert_eq!(doc.meta.authors, ["Jane Austen", "Someone Else"]);
        assert_eq!(doc.meta.language, "en-GB");
        assert_eq!(doc.meta.publisher, "Penguin");
        assert_eq!(doc.meta.published, "1813-01-28");
        assert_eq!(doc.meta.identifiers, ["isbn:9780141439518"]);
        assert_eq!(doc.meta.description, "A test book & more.");
        assert_eq!(doc.meta.subjects, ["Fiction", "Romance"]);
        assert_eq!(doc.meta.cover, "OEBPS/images/cover art.jpg");
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);

        // One chapter per spine item that is there, each once.
        assert_eq!(doc.chapters.len(), 2);
        assert_eq!(doc.chapters[0].href, "OEBPS/text/ch1.xhtml");
        assert_eq!(doc.chapters[0].first_block, 0);
        assert_eq!(doc.chapters[1].href, "OEBPS/text/ch2.xhtml");
        assert_eq!(doc.chapters[1].first_block, doc.chapters[0].blocks);
        assert_eq!((doc.chapters[0].blocks + doc.chapters[1].blocks) as usize, doc.blocks.len());

        let kinds: Vec<(DocBlockKind, &str)> =
            doc.blocks.iter().map(|b| (b.kind, b.text.as_str())).collect();
        assert_eq!(
            kinds[..16],
            [
                (DocBlockKind::Heading, "Chapter One"),
                (
                    DocBlockKind::Paragraph,
                    "It is a truth universally acknowledged, that a single man1 & more."
                ),
                (DocBlockKind::Paragraph, "Inside a div."),
                (DocBlockKind::Heading, "A Section"),
                (DocBlockKind::Paragraph, "See the next chapter, chapter two and the web."),
                (DocBlockKind::Quote, "Quoted one."),
                (DocBlockKind::Quote, "Quoted two."),
                (DocBlockKind::ListItem, "First item"),
                (DocBlockKind::ListItem, "Second item"),
                (DocBlockKind::Code, "  indented\ncode"),
                (DocBlockKind::Rule, ""),
                (DocBlockKind::Image, "A map of the county"),
                (DocBlockKind::Paragraph, "Name · Age"),
                (DocBlockKind::Paragraph, "Ann · 31"),
                (DocBlockKind::Paragraph, "Loose text in a div"),
                (DocBlockKind::Paragraph, "The footnote text."),
            ]
        );
        assert_eq!(doc.blocks[0].level, 1);
        assert_eq!(doc.blocks[3].level, 2);
        // A picture is a block with its path in the archive, and one the
        // archive does not have is not one.
        assert_eq!(doc.blocks[11].src, "OEBPS/images/map.png");
        let svg = doc.blocks.iter().filter(|b| b.kind == DocBlockKind::Image).nth(1).unwrap();
        assert_eq!((svg.text.as_str(), svg.src.as_str()), ("", "OEBPS/images/map.png"));
        assert_eq!(doc.blocks.iter().filter(|b| b.kind == DocBlockKind::Image).count(), 2);
    }

    #[test]
    fn links_inside_the_book_point_at_blocks_wherever_they_are_and_the_rest_are_words() {
        let doc = parse(&book3()).unwrap();
        let note = block_of(&doc, "The footnote text.");
        assert_eq!(
            doc.blocks[1].html,
            format!(
                "It is a truth <em>universally</em> acknowledged, that a <strong>single</strong> man<sup><a data-block=\"{note}\">1</a></sup> &amp; more."
            )
        );
        let destination = block_of(&doc, "The destination paragraph, with a back link.");
        let chapter_two = block_of(&doc, "Chapter Two");
        assert_eq!(
            doc.blocks[4].html,
            format!(
                "See <a data-block=\"{destination}\">the next chapter</a>, <a data-block=\"{chapter_two}\">chapter two</a> and the web."
            )
        );
        assert_eq!(
            doc.blocks[destination].html,
            "The destination paragraph, with a <a data-block=\"1\">back link</a>."
        );
        for block in &doc.blocks {
            assert!(!block.html.contains(['\u{1}', '\u{2}', '\u{3}']), "{}", block.html);
        }
    }

    #[test]
    fn the_navigation_document_gives_the_sections_where_they_start() {
        let doc = parse(&book3()).unwrap();
        // Landmarks are not the contents; a label with no link is aimed at its
        // first link and so is "Part Two"; entries to a file that is not in
        // the book, or an element that is not in a file, are left out.
        let mut sections: Vec<(&str, u8, usize)> =
            doc.sections.iter().map(|s| (s.title.as_str(), s.level, s.block as usize)).collect();
        sections.sort_by_key(|&(_, _, block)| block);
        let two = block_of(&doc, "Chapter Two");
        let section = block_of(&doc, "A Section");
        assert_eq!(
            sections,
            [
                ("Chapter One", 1, 0),
                ("A Section", 2, section),
                ("Part Two", 1, two),
                ("Chapter Two", 2, two)
            ]
        );
    }

    const OPF2: &str = r##"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="bookid">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
 <dc:title>Old Book</dc:title><dc:creator opf:role="aut">Some Author</dc:creator>
 <dc:identifier id="bookid" opf:scheme="ISBN">0-14-143951-0</dc:identifier>
 <dc:identifier opf:scheme="DOI">10.1000/XYZ123</dc:identifier>
 <meta name="cover" content="cover-img"/>
</metadata>
<manifest>
 <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
 <item id="c1" href="ch1.html" media-type="text/html"/>
 <item id="c2" href="ch2.html" media-type="application/xhtml+xml"/>
 <item id="cover-img" href="cover.jpg" media-type="image/jpeg"/>
</manifest>
<spine toc="ncx"><itemref idref="c1"/><itemref idref="c2"/></spine>
</package>"##;

    const NCX: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE ncx PUBLIC "-//NISO//DTD ncx 2005-1//EN" "http://www.daisy.org/z3986/2005/ncx-2005-1.dtd">
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1"><navMap>
 <navPoint id="n1" playOrder="1"><navLabel><text>Part One</text></navLabel><content src="ch1.html"/>
   <navPoint id="n2" playOrder="2"><navLabel><text>First Chapter</text></navLabel><content src="ch1.html#c1"/></navPoint>
 </navPoint>
 <navPoint id="n3" playOrder="3"><navLabel><text>Second</text></navLabel><content src="ch2.html"/></navPoint>
</navMap></ncx>"##;

    #[test]
    fn an_epub2_has_its_contents_in_an_ncx_and_its_cover_in_a_meta() {
        let book = zip_of(&[
            ("META-INF/container.xml", br##"<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"##),
            ("content.opf", OPF2.as_bytes()),
            ("toc.ncx", NCX.as_bytes()),
            ("ch1.html", b"<html><body><h1 id=\"c1\">First Chapter</h1><p>Hello world.</p></body></html>"),
            ("ch2.html", b"<html><body><h1>Second</h1><p>More.</p></body></html>"),
            ("cover.jpg", b"jpeg"),
        ]);
        let doc = parse(&book).unwrap();
        assert_eq!(doc.meta.title, "Old Book");
        assert_eq!(doc.meta.authors, ["Some Author"]);
        assert_eq!(doc.meta.identifiers, ["isbn:0141439510", "doi:10.1000/xyz123"]);
        assert_eq!(doc.meta.cover, "cover.jpg");
        assert_eq!(doc.blocks.len(), 4);
        assert_eq!(doc.chapters.len(), 2);
        let mut sections: Vec<(&str, u8, u32)> =
            doc.sections.iter().map(|s| (s.title.as_str(), s.level, s.block)).collect();
        sections.sort_by_key(|&(title, level, block)| (block, level, title.len()));
        assert_eq!(sections, [("Part One", 1, 0), ("First Chapter", 2, 0), ("Second", 1, 2)]);
    }

    #[test]
    fn a_book_with_no_spine_is_read_in_the_manifests_order() {
        let opf = r##"<package xmlns="http://www.idpf.org/2007/opf"><metadata/><manifest>
            <item id="a" href="a.xhtml" media-type="application/xhtml+xml"/>
            <item id="b" href="b.xhtml" media-type="application/xhtml+xml"/>
            <item id="i" href="i.png" media-type="image/png"/></manifest></package>"##;
        let book = zip_of(&[
            ("content.opf", opf.as_bytes()),
            ("a.xhtml", b"<p>Alpha</p>"),
            ("b.xhtml", b"<p>Beta</p>"),
        ]);
        let doc = parse(&book).unwrap();
        let words: Vec<&str> = doc.blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(words, ["Alpha", "Beta"]);
    }

    #[test]
    fn the_pictures_of_a_book_are_handed_out_by_their_paths() {
        let book = book3();
        let cover = crate::documents::parse::read_resource(
            DocFormat::Epub,
            &book,
            "OEBPS/images/cover art.jpg",
        )
        .unwrap();
        assert_eq!(
            (cover.mime.as_str(), cover.bytes.as_slice()),
            ("image/jpeg", &b"jpeg bytes"[..])
        );
        let map = resource(&book, "OEBPS/images/MAP.png").unwrap();
        assert_eq!(map.mime, "image/png");
        // Not a picture, not there, and not even a book.
        assert!(resource(&book, "OEBPS/text/ch1.xhtml").is_none());
        assert!(resource(&book, "OEBPS/images/nope.png").is_none());
        assert!(resource(b"not a zip", "a.png").is_none());
    }

    #[test]
    fn a_file_bigger_than_the_cap_is_not_read_and_is_said_to_be_missing() {
        let big = format!("<p>{}</p>", "x".repeat(5000));
        let opf = r##"<package xmlns="http://www.idpf.org/2007/opf"><metadata/><manifest>
            <item id="a" href="a.xhtml" media-type="application/xhtml+xml"/>
            <item id="b" href="b.xhtml" media-type="application/xhtml+xml"/></manifest>
            <spine><itemref idref="a"/><itemref idref="b"/></spine></package>"##;
        let book = zip_of(&[
            ("content.opf", opf.as_bytes()),
            ("a.xhtml", b"<p>Small</p>"),
            ("b.xhtml", big.as_bytes()),
        ]);
        let limits = Limits { entry: 1024, ..Limits::default() };
        let doc = read_book(&book, limits).unwrap();
        assert_eq!(doc.blocks.len(), 1);
        assert_eq!(doc.warnings, ["1 part of this book could not be read."]);
        // With the default limits it is read whole.
        assert_eq!(parse(&book).unwrap().blocks.len(), 2);
        // The total counts too: the package, then the small one, then no room
        // left for the big one.
        let limits = Limits { total: 1000, ..Limits::default() };
        let doc = read_book(&book, limits).unwrap();
        assert_eq!(doc.blocks.len(), 1);
    }

    #[test]
    fn a_zip_that_says_it_is_small_and_is_not_costs_what_is_allowed_and_no_more() {
        // 3 MB of one letter deflates to a few kilobytes; under a 1 MB cap
        // it is refused, however little the archive's directory suggests.
        let bomb = format!("<p>{}</p>", "a".repeat(3 * 1024 * 1024));
        let opf = r##"<package xmlns="http://www.idpf.org/2007/opf"><metadata/><manifest>
            <item id="a" href="a.xhtml" media-type="application/xhtml+xml"/></manifest>
            <spine><itemref idref="a"/></spine></package>"##;
        let book = zip_of(&[("content.opf", opf.as_bytes()), ("a.xhtml", bomb.as_bytes())]);
        assert!(book.len() < 64 * 1024, "{}", book.len());
        let limits = Limits { entry: 1024 * 1024, ..Limits::default() };
        let err = read_book(&book, limits).unwrap_err();
        assert_eq!(err.0, EMPTY);
        let limits = Limits { entries: 1, ..Limits::default() };
        assert!(read_book(&book, limits).is_err());
        let limits = Limits { listed: 1, ..Limits::default() };
        assert_eq!(read_book(&book, limits).unwrap_err().0, TOO_MANY_FILES);
    }

    #[test]
    fn a_long_book_is_cut_at_the_block_limit_with_a_warning() {
        let opf = r##"<package xmlns="http://www.idpf.org/2007/opf"><metadata/><manifest>
            <item id="a" href="a.xhtml" media-type="application/xhtml+xml"/>
            <item id="b" href="b.xhtml" media-type="application/xhtml+xml"/></manifest>
            <spine><itemref idref="a"/><itemref idref="b"/></spine></package>"##;
        let book = zip_of(&[
            ("content.opf", opf.as_bytes()),
            ("a.xhtml", b"<p>1</p><p>2</p><p>3</p>"),
            ("b.xhtml", b"<p>4</p><p>5</p>"),
        ]);
        let limits = Limits { blocks: 4, ..Limits::default() };
        let doc = read_book(&book, limits).unwrap();
        assert_eq!(doc.blocks.len(), 4);
        assert_eq!(
            doc.warnings,
            ["This book is very long; only the first 4 paragraphs were read."]
        );
    }

    #[test]
    fn copy_protected_chapters_are_not_read_but_scrambled_fonts_do_not_matter() {
        let encryption = r##"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"
            xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
            <enc:EncryptedData><enc:EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/>
              <enc:CipherData><enc:CipherReference URI="f.otf"/></enc:CipherData></enc:EncryptedData>
            <enc:EncryptedData><enc:EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes128-cbc"/>
              <enc:CipherData><enc:CipherReference URI="b.xhtml"/></enc:CipherData></enc:EncryptedData>
            </encryption>"##;
        let opf = r##"<package xmlns="http://www.idpf.org/2007/opf"><metadata/><manifest>
            <item id="a" href="a.xhtml" media-type="application/xhtml+xml"/>
            <item id="b" href="b.xhtml" media-type="application/xhtml+xml"/></manifest>
            <spine><itemref idref="a"/><itemref idref="b"/></spine></package>"##;
        let files = |chapter: &'static [u8]| {
            zip_of(&[
                ("META-INF/encryption.xml", encryption.as_bytes()),
                ("content.opf", opf.as_bytes()),
                ("a.xhtml", chapter),
                ("b.xhtml", b"\x01\x02 scrambled"),
            ])
        };
        let doc = parse(&files(b"<p>Open</p>")).unwrap();
        assert_eq!(doc.blocks.len(), 1);
        assert_eq!(doc.warnings, ["1 part of this book could not be read."]);
        // Nothing open at all: it is the protection that is said.
        let all = zip_of(&[
            ("META-INF/encryption.xml", encryption.as_bytes()),
            ("content.opf", opf.as_bytes()),
            ("a.xhtml", b"\x01"),
            ("b.xhtml", b"\x02"),
        ]);
        assert_eq!(parse(&all).unwrap_err().0, PROTECTED);
    }

    #[test]
    fn a_zip_with_no_package_is_not_a_book() {
        let book = zip_of(&[("readme.txt", b"hello")]);
        assert_eq!(parse(&book).unwrap_err().0, NO_PACKAGE);
        assert_eq!(parse(b"plainly not a zip").unwrap_err().0, NOT_AN_EPUB);
    }

    #[test]
    fn identifiers_are_told_apart() {
        assert_eq!(identifier("urn:isbn:978-0-14-143951-8", None).unwrap(), "isbn:9780141439518");
        assert_eq!(identifier("9780141439518", None).unwrap(), "isbn:9780141439518");
        assert_eq!(identifier("0-8044-2957-X", Some("ISBN")).unwrap(), "isbn:080442957x");
        assert_eq!(
            identifier("urn:doi:10.1038/NATURE12373", None).unwrap(),
            "doi:10.1038/nature12373"
        );
        assert_eq!(identifier("10.1000/abc", None).unwrap(), "doi:10.1000/abc");
        assert_eq!(identifier("urn:uuid:0cc33cbd-94e2-49c1-9cf0-6e3d5ca2b0d7", None), None);
        assert_eq!(identifier("1234567890", None), None);
        assert_eq!(identifier("http://example.com/book", None), None);
    }
}
