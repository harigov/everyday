//! Reading html: the walk that turns a page's tree into blocks. See
//! [`super`].
//!
//! One walk serves an EPUB's chapters and a standalone `.html` file alike,
//! so what a book's paragraph, heading, quotation, list item, code block,
//! picture and table row come to is the same in both. The tree comes from
//! `dom_query` (html5ever, which makes sense of what a browser would, so a
//! chapter's XHTML need not be well-formed). Nothing is copied out of it
//! but words: a block's `html` is written here, from `em`, `strong`, `i`,
//! `b`, `sup`, `sub`, `code`, `br` and links, with every character of the
//! file's own text escaped and no attribute carried over. A link becomes
//! `<a data-block="N">` when it points somewhere in the same document, and
//! is its text and nothing more otherwise.
//!
//! A link can point forward -- a footnote marker to a note at the end of the
//! book -- so the walk writes a placeholder where one begins and ends, and
//! [`Walk::finish`] settles them once every file is walked and every block
//! has its number. The placeholders are control characters, which the text
//! of a page can never carry: [`Run::text`] drops every one it is given.

use std::collections::HashMap;

use dom_query::{Document, Node};
use karmayoga_core::documents::{DocBlock, DocBlockKind, DocChapter, DocFormat, DocMeta, DocText};

use super::{ParseError, collapse, decode, escape, push_escaped, split_keywords, split_names};

/// The most blocks one document is read into. A book longer than this is cut
/// short, with a warning, rather than held whole.
pub const MAX_BLOCKS: usize = 200_000;

/// How deep the walk goes into a page before it takes what is left as plain
/// text. Pages in the wild nest a few dozen levels at most; a stranger's can
/// nest as many as it likes, and the recursion here is on the stack.
const MAX_DEPTH: usize = 160;

/// What says whether a file named by a page is in the archive the page is
/// in, and under what name: an EPUB's picture is drawn, and its link
/// followed, only if the file is there. A page on its own has no archive.
pub type Exists<'a> = &'a dyn Fn(&str) -> Option<String>;

// ---- one block's worth of words -------------------------------------------

/// The words of one block as they are gathered: the plain text the assistant
/// reads and the HTML the reader draws, kept in step.
#[derive(Debug, Default)]
pub struct Run {
    text: String,
    html: String,
    /// Whitespace has gone by since the last word. It is written as one
    /// space before the next, and never at either end of the block.
    space: bool,
    /// A table cell ended since the last word. Its separator is written
    /// before the next word, so a cell with nothing in it leaves none.
    cell: bool,
    /// Line breaks count as spaces: a heading or a table row is one line.
    flat: bool,
    /// Pictures met on the way, as (path in the file, description). They are
    /// set down as blocks after the text, in the order they came.
    pictures: Vec<(String, String)>,
}

impl Run {
    /// A run in which line breaks are spaces.
    pub fn flat() -> Self {
        Self { flat: true, ..Self::default() }
    }

    /// The words so far, as plain text.
    pub fn plain(&self) -> &str {
        &self.text
    }

    /// Add words from the file. Whitespace of any kind and any length is one
    /// space; control characters and the invisible characters that only get
    /// in the way of a search (soft hyphens, zero-width spaces) are dropped.
    pub fn text(&mut self, words: &str) {
        for ch in words.chars() {
            if ch.is_whitespace() {
                self.space = true;
            } else if !is_dropped(ch) {
                self.settle();
                self.text.push(ch);
                push_escaped(&mut self.html, ch);
            }
        }
    }

    /// Write what is owed before the next word or tag: a cell's separator,
    /// or else the space that whitespace left.
    fn settle(&mut self) {
        if self.cell {
            self.cell = false;
            if !self.text.is_empty() {
                self.text.push_str(" · ");
                self.html.push_str(" · ");
                self.space = false;
                return;
            }
        }
        if self.space {
            self.space = false;
            if !self.text.is_empty() && !self.text.ends_with([' ', '\n']) {
                self.text.push(' ');
                self.html.push(' ');
            }
        }
    }

    /// Begin `tag`, one of the inline elements the reader is let draw.
    pub fn open(&mut self, tag: &str) {
        self.settle();
        self.html.push('<');
        self.html.push_str(tag);
        self.html.push('>');
    }

    /// End `tag`. No space is settled first, so the whitespace after an
    /// emphasised word is written outside the emphasis.
    pub fn close(&mut self, tag: &str) {
        self.html.push_str("</");
        self.html.push_str(tag);
        self.html.push('>');
    }

    /// Where link number `n` begins, to be settled by [`Walk::finish`].
    fn open_link(&mut self, n: usize) {
        self.settle();
        self.html.push('\u{1}');
        self.html.push_str(&n.to_string());
        self.html.push('\u{2}');
    }

    /// Where link number `n` ends.
    fn close_link(&mut self, n: usize) {
        self.html.push('\u{3}');
        self.html.push_str(&n.to_string());
        self.html.push('\u{2}');
    }

    /// A break in the line: a real one, except at the start of a block or
    /// twice running, and in a run where lines are one.
    pub fn line_break(&mut self) {
        if self.flat {
            self.space = true;
        } else if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.text.push('\n');
            self.html.push_str("<br>");
            self.space = false;
        }
    }

    /// The next word starts a new table cell.
    pub fn cell_break(&mut self) {
        if !self.text.is_empty() {
            self.cell = true;
        }
    }

    /// A picture met in the middle of the words: path in the file (empty for
    /// a page on its own) and what it is of.
    pub fn picture(&mut self, src: String, alt: String) {
        self.pictures.push((src, alt));
    }

    /// Everything gathered, as blocks: a block of `kind` for the words, if
    /// there are any, then one picture block each. The run is empty after.
    pub fn take_blocks(&mut self, kind: DocBlockKind, level: u8) -> Vec<DocBlock> {
        let mut text = std::mem::take(&mut self.text);
        let mut html = std::mem::take(&mut self.html);
        while text.ends_with('\n') {
            text.pop();
        }
        while html.ends_with("<br>") {
            html.truncate(html.len() - "<br>".len());
        }
        self.space = false;
        self.cell = false;
        let mut blocks = Vec::new();
        if !text.is_empty() {
            blocks.push(DocBlock { kind, level, text, html, ..DocBlock::default() });
        }
        for (src, alt) in std::mem::take(&mut self.pictures) {
            blocks.push(DocBlock {
                kind: DocBlockKind::Image,
                text: alt,
                src,
                ..DocBlock::default()
            });
        }
        blocks
    }
}

/// Characters that are never part of the words: control characters (which
/// is also what keeps a file from smuggling in a link placeholder), and the
/// ones that are invisible and only split a word for a search.
fn is_dropped(ch: char) -> bool {
    ch.is_control() || matches!(ch, '\u{ad}' | '\u{200b}' | '\u{2060}' | '\u{feff}')
}

// ---- the walk ---------------------------------------------------------------

/// What a link has to be settled against once everything is walked.
struct Link {
    /// The file it points into, by its name in the archive.
    path: String,
    /// The element in it, decoded; empty for the file as a whole.
    fragment: String,
}

/// What an element is, for the walk.
enum Kind {
    /// Nothing in it is read: scripts, styles, forms, embedded objects.
    Skip,
    /// Part of a line of words.
    Inline,
    Paragraph,
    Heading(u8),
    Quote,
    Item,
    /// A term, a definition, a figure's or a table's caption: a paragraph,
    /// whatever it is inside.
    Caption,
    Pre,
    Rule,
    Table,
    /// Holds blocks: `div`, `section`, `ul` and whatever is not known.
    /// Only the runs of text directly inside it are blocks of its own.
    Container,
}

/// The walk of one or several pages into one list of blocks.
///
/// Each page is walked with [`Walk::page`], then [`Walk::finish`] settles
/// the links, and [`Walk::resolve`] says where a place a table of contents
/// names has ended up.
pub struct Walk<'a> {
    /// The blocks so far, in reading order.
    pub blocks: Vec<DocBlock>,
    /// Whether the block limit was reached, and the rest left unread.
    pub capped: bool,
    max_blocks: usize,
    exists: Option<Exists<'a>>,
    /// The file being walked, by its name in the archive; empty for a page
    /// on its own.
    path: String,
    /// Where each `id` in each file landed: the block it is on, or the next
    /// one when it is on something that holds blocks.
    ids: HashMap<(String, String), usize>,
    /// Ids met in this file that are still waiting for a block to land on.
    waiting: Vec<String>,
    /// The first block of each file.
    starts: HashMap<String, usize>,
    links: Vec<Link>,
}

impl<'a> Walk<'a> {
    /// A walk that keeps at most `max_blocks`. With `exists`, the pages are in
    /// an archive: a picture is a block only if its file is there, and a link
    /// to another file is followed only to one that is. Without it, pictures
    /// are kept only for their description.
    pub fn new(exists: Option<Exists<'a>>, max_blocks: usize) -> Self {
        Self {
            blocks: Vec::new(),
            capped: false,
            max_blocks,
            exists,
            path: String::new(),
            ids: HashMap::new(),
            waiting: Vec::new(),
            starts: HashMap::new(),
            links: Vec::new(),
        }
    }

    /// Walk one page, which is the file `path` in the archive (empty for a
    /// page on its own).
    pub fn page(&mut self, page: &Document, path: &str) {
        self.path = path.to_string();
        self.starts.entry(self.path.clone()).or_insert(self.blocks.len());
        self.container(&page.root(), DocBlockKind::Paragraph, 0);
        // Ids at the very end of a file belong to whatever comes next.
        self.land(self.blocks.len());
    }

    /// A file that is a picture and nothing else -- a comic's page in the
    /// reading order.
    pub fn image(&mut self, path: &str) {
        self.path = path.to_string();
        self.starts.entry(self.path.clone()).or_insert(self.blocks.len());
        self.push(DocBlock {
            kind: DocBlockKind::Image,
            src: path.to_string(),
            ..DocBlock::default()
        });
    }

    /// Where a place ended up: the block the element `fragment` is on in the
    /// file `path`, or with no fragment the file's first block. `None` for a
    /// place that is not there (a fragment nothing in the file answers to is
    /// not the start of the file, whatever a link hopes) or that has no
    /// block after it.
    pub fn resolve(&self, path: &str, fragment: &str) -> Option<usize> {
        let at = if fragment.is_empty() {
            *self.starts.get(path)?
        } else {
            *self.ids.get(&(path.to_string(), fragment.to_string()))?
        };
        (at < self.blocks.len()).then_some(at)
    }

    /// Settle the links: one that points to a block becomes
    /// `<a data-block="N">`, and any other is left as its words.
    pub fn finish(&mut self) {
        if self.links.is_empty() {
            return;
        }
        let targets: Vec<Option<usize>> =
            self.links.iter().map(|link| self.resolve(&link.path, &link.fragment)).collect();
        for block in &mut self.blocks {
            if block.html.contains(['\u{1}', '\u{3}']) {
                block.html = settle_links(&block.html, &targets);
            }
        }
    }

    // ---- blocks

    fn push(&mut self, block: DocBlock) {
        if self.blocks.len() >= self.max_blocks {
            self.capped = true;
            return;
        }
        self.land(self.blocks.len());
        self.blocks.push(block);
    }

    /// Every id waiting lands on block `at`.
    fn land(&mut self, at: usize) {
        for id in std::mem::take(&mut self.waiting) {
            self.ids.entry((self.path.clone(), id)).or_insert(at);
        }
    }

    fn flush(&mut self, run: &mut Run, kind: DocBlockKind, level: u8) {
        for block in run.take_blocks(kind, level) {
            self.push(block);
        }
    }

    /// An element's `id` (and an anchor's `name`) waits for the next block.
    fn note_id(&mut self, node: &Node<'_>) {
        if let Some(id) = attr(node, "id").filter(|id| !id.is_empty()) {
            self.waiting.push(id);
        }
        if node.has_name("a")
            && let Some(name) = attr(node, "name").filter(|name| !name.is_empty())
        {
            self.waiting.push(name);
        }
    }

    // ---- elements that hold blocks

    /// Walk what is inside `node`, which holds blocks. The words that sit
    /// directly in it, between its block children, become blocks of `kind`.
    fn container(&mut self, node: &Node<'_>, kind: DocBlockKind, depth: usize) {
        let mut run = Run::default();
        if depth > MAX_DEPTH {
            run.text(&node.text());
            self.flush(&mut run, kind, 0);
            return;
        }
        for child in node.children_it(false) {
            if self.capped {
                return;
            }
            if child.is_text() {
                run.text(&child.text());
                continue;
            }
            if !child.is_element() {
                continue;
            }
            if is_page_break(&child) {
                self.note_id(&child);
                continue;
            }
            match classify(&tag(&child)) {
                Kind::Skip => {}
                Kind::Inline if !has_blocks(&child) => self.inline(&child, &mut run, depth + 1),
                Kind::Inline | Kind::Container => {
                    self.flush(&mut run, kind, 0);
                    self.note_id(&child);
                    self.container(&child, kind, depth + 1);
                }
                Kind::Paragraph => {
                    self.flush(&mut run, kind, 0);
                    self.note_id(&child);
                    self.words(&child, kind, 0, depth);
                }
                Kind::Caption => {
                    self.flush(&mut run, kind, 0);
                    self.note_id(&child);
                    self.words(&child, DocBlockKind::Paragraph, 0, depth);
                }
                Kind::Heading(level) => {
                    self.flush(&mut run, kind, 0);
                    self.note_id(&child);
                    self.words(&child, DocBlockKind::Heading, level, depth);
                }
                Kind::Quote => {
                    self.flush(&mut run, kind, 0);
                    self.note_id(&child);
                    self.container(&child, DocBlockKind::Quote, depth + 1);
                }
                Kind::Item => {
                    self.flush(&mut run, kind, 0);
                    self.note_id(&child);
                    self.container(&child, DocBlockKind::ListItem, depth + 1);
                }
                Kind::Pre => {
                    self.flush(&mut run, kind, 0);
                    self.note_id(&child);
                    self.code(&child);
                }
                Kind::Rule => {
                    self.flush(&mut run, kind, 0);
                    self.note_id(&child);
                    self.push(DocBlock { kind: DocBlockKind::Rule, ..DocBlock::default() });
                }
                Kind::Table => {
                    self.flush(&mut run, kind, 0);
                    self.note_id(&child);
                    self.table(&child, kind, depth);
                }
            }
        }
        self.flush(&mut run, kind, 0);
    }

    /// The words inside `node`, as one block (and the pictures in them as
    /// blocks after it). A heading is one line, whatever is in it.
    fn words(&mut self, node: &Node<'_>, kind: DocBlockKind, level: u8, depth: usize) {
        let mut run = if kind == DocBlockKind::Heading { Run::flat() } else { Run::default() };
        self.inline_children(node, &mut run, depth + 1);
        self.flush(&mut run, kind, level);
    }

    /// A `pre`: the text exactly as it is, indentation and line breaks and
    /// all, but for blank lines before it and whitespace after it.
    fn code(&mut self, node: &Node<'_>) {
        let mut text = String::new();
        pre_text(node, &mut text, 0);
        text.retain(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'));
        let text = text.trim_start_matches('\n').trim_end();
        if text.is_empty() {
            return;
        }
        self.push(DocBlock {
            kind: DocBlockKind::Code,
            text: text.to_string(),
            html: escape(text),
            ..DocBlock::default()
        });
    }

    /// A table. A table of data is a paragraph per row, the cells joined by
    /// " · ", and its caption a paragraph; but a table that holds paragraphs,
    /// lists or other tables is a page laid out in a grid, and its cells are
    /// walked as the containers they are.
    fn table(&mut self, node: &Node<'_>, kind: DocBlockKind, depth: usize) {
        if node.descendants_it().any(|inner| inner.is_element() && is_flow(&tag(&inner))) {
            self.container(node, kind, depth + 1);
            return;
        }
        for part in node.descendants_it() {
            if self.capped {
                return;
            }
            if !part.is_element() {
                continue;
            }
            match tag(&part).as_str() {
                "caption" => {
                    self.note_id(&part);
                    self.words(&part, DocBlockKind::Paragraph, 0, depth);
                }
                "tr" => {
                    self.note_id(&part);
                    let mut run = Run::flat();
                    for cell in part.children_it(false) {
                        if cell.has_name("td") || cell.has_name("th") {
                            run.cell_break();
                            self.note_id(&cell);
                            self.inline_children(&cell, &mut run, depth + 1);
                        }
                    }
                    self.flush(&mut run, DocBlockKind::Paragraph, 0);
                }
                _ => {}
            }
        }
    }

    // ---- what is inside a line

    fn inline_children(&mut self, node: &Node<'_>, run: &mut Run, depth: usize) {
        for child in node.children_it(false) {
            self.inline(&child, run, depth);
        }
    }

    /// Add `node`, a text or an element met in the middle of a line, to
    /// `run`.
    fn inline(&mut self, node: &Node<'_>, run: &mut Run, depth: usize) {
        if node.is_text() {
            run.text(&node.text());
            return;
        }
        if !node.is_element() {
            return;
        }
        if depth > MAX_DEPTH {
            run.text(&node.text());
            return;
        }
        let name = tag(node);
        self.note_id(node);
        if is_page_break(node) {
            return;
        }
        if let Some(wrapper) = wrapper(&name) {
            run.open(wrapper);
            self.inline_children(node, run, depth + 1);
            run.close(wrapper);
            return;
        }
        match name.as_str() {
            "br" => run.line_break(),
            "a" => self.link(node, run, depth),
            "img" | "image" => self.picture(node, run),
            "svg" => {
                for image in node.descendants_it().filter(|inner| inner.has_name("image")) {
                    self.picture(&image, run);
                }
            }
            "wbr" | "rt" | "rp" => {}
            other => match classify(other) {
                Kind::Skip => {}
                Kind::Inline => self.inline_children(node, run, depth + 1),
                // A block where only words were expected: its words are kept,
                // with a space either side so they do not run into the next.
                _ => {
                    run.space = true;
                    self.inline_children(node, run, depth + 1);
                    run.space = true;
                }
            },
        }
    }

    /// An `a`: its words, and if it points somewhere in this document, a
    /// placeholder either side of them for [`Walk::finish`] to settle.
    fn link(&mut self, node: &Node<'_>, run: &mut Run, depth: usize) {
        let target =
            attr(node, "href").and_then(|href| link_target(&self.path, &href, self.exists));
        match target {
            Some((path, fragment)) => {
                let n = self.links.len();
                self.links.push(Link { path, fragment });
                run.open_link(n);
                self.inline_children(node, run, depth + 1);
                run.close_link(n);
            }
            None => self.inline_children(node, run, depth + 1),
        }
    }

    /// An `img`, or an SVG's `image`: kept if the archive has its file, or in
    /// a page on its own if it says what it is of.
    fn picture(&mut self, node: &Node<'_>, run: &mut Run) {
        let alt = ["alt", "aria-label", "title"]
            .iter()
            .find_map(|key| attr(node, key).map(|alt| collapse(&alt)).filter(|alt| !alt.is_empty()))
            .unwrap_or_default();
        let href = attr(node, "src").or_else(|| attr(node, "href")).unwrap_or_default();
        let href = href.trim();
        match self.exists {
            Some(exists) => {
                if href.is_empty() || has_scheme(href) {
                    return;
                }
                let (path, _) = split_href(href);
                if let Some(file) = exists(&resolve_path(&self.path, path)) {
                    run.picture(file, alt);
                }
            }
            None => {
                if !alt.is_empty() {
                    run.picture(String::new(), alt);
                }
            }
        }
    }
}

/// The text of a `pre` with its line breaks, which `text()` leaves out where
/// they are `<br>`.
fn pre_text(node: &Node<'_>, out: &mut String, depth: usize) {
    for child in node.children_it(false) {
        if child.is_text() {
            out.push_str(&child.text());
        } else if child.is_element() {
            if depth > MAX_DEPTH {
                out.push_str(&child.text());
            } else if child.has_name("br") {
                out.push('\n');
            } else {
                pre_text(&child, out, depth + 1);
            }
        }
    }
}

/// `html` with each link placeholder replaced: by `<a data-block="N">` and
/// `</a>` where its target is a block, and by nothing where it is not.
fn settle_links(html: &str, targets: &[Option<usize>]) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find(['\u{1}', '\u{3}']) {
        out.push_str(&rest[..at]);
        let opening = rest[at..].starts_with('\u{1}');
        let after = &rest[at + 1..];
        let Some(end) = after.find('\u{2}') else {
            rest = after;
            continue;
        };
        let target =
            after[..end].parse::<usize>().ok().and_then(|n| targets.get(n).copied().flatten());
        match (opening, target) {
            (true, Some(block)) => {
                out.push_str("<a data-block=\"");
                out.push_str(&block.to_string());
                out.push_str("\">");
            }
            (false, Some(_)) => out.push_str("</a>"),
            _ => {}
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

// ---- elements ---------------------------------------------------------------

/// The lowercase name of an element.
fn tag(node: &Node<'_>) -> String {
    node.node_name().map(|name| name.to_ascii_lowercase()).unwrap_or_default()
}

/// An attribute's value, if the element has it.
fn attr(node: &Node<'_>, name: &str) -> Option<String> {
    node.attr(name).map(|value| value.to_string())
}

fn classify(name: &str) -> Kind {
    match name {
        "script" | "style" | "head" | "template" | "noscript" | "iframe" | "object" | "embed"
        | "audio" | "video" | "canvas" | "select" | "textarea" | "button" | "input" | "link"
        | "meta" | "title" | "base" | "map" => Kind::Skip,
        "p" => Kind::Paragraph,
        "h1" => Kind::Heading(1),
        "h2" => Kind::Heading(2),
        "h3" => Kind::Heading(3),
        "h4" => Kind::Heading(4),
        "h5" => Kind::Heading(5),
        "h6" => Kind::Heading(6),
        "blockquote" => Kind::Quote,
        "li" => Kind::Item,
        "dt" | "dd" | "figcaption" | "caption" => Kind::Caption,
        "pre" | "listing" | "xmp" => Kind::Pre,
        "hr" => Kind::Rule,
        "table" => Kind::Table,
        other if is_inline(other) => Kind::Inline,
        _ => Kind::Container,
    }
}

/// Elements that are part of a line of words.
fn is_inline(name: &str) -> bool {
    matches!(
        name,
        "a" | "abbr"
            | "acronym"
            | "b"
            | "bdi"
            | "bdo"
            | "big"
            | "br"
            | "cite"
            | "code"
            | "data"
            | "del"
            | "dfn"
            | "em"
            | "font"
            | "i"
            | "image"
            | "img"
            | "ins"
            | "kbd"
            | "label"
            | "mark"
            | "math"
            | "q"
            | "rp"
            | "rt"
            | "ruby"
            | "s"
            | "samp"
            | "small"
            | "span"
            | "strike"
            | "strong"
            | "sub"
            | "sup"
            | "svg"
            | "time"
            | "tt"
            | "u"
            | "var"
            | "wbr"
    )
}

/// Elements whose place is between lines, not inside one: what makes a table
/// a layout, and an inline element that holds one a container.
fn is_flow(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "li"
            | "dl"
            | "blockquote"
            | "pre"
            | "table"
            | "hr"
            | "section"
            | "article"
            | "aside"
            | "header"
            | "footer"
            | "main"
            | "nav"
            | "figure"
            | "details"
            | "address"
    )
}

/// Whether an element holds a block somewhere inside it. An `a` or a `span`
/// wrapped round a whole paragraph is a container, not part of a line.
fn has_blocks(node: &Node<'_>) -> bool {
    node.descendants_it().any(|inner| inner.is_element() && is_flow(&tag(&inner)))
}

/// The inline element the reader draws for `name`, if it draws one.
fn wrapper(name: &str) -> Option<&'static str> {
    match name {
        "em" | "cite" | "dfn" => Some("em"),
        "i" => Some("i"),
        "strong" => Some("strong"),
        "b" => Some("b"),
        "sup" => Some("sup"),
        "sub" => Some("sub"),
        "code" | "kbd" | "samp" | "tt" | "var" => Some("code"),
        _ => None,
    }
}

/// A printed page's number, marked in the text of a book: not words, but a
/// place a link can go to.
fn is_page_break(node: &Node<'_>) -> bool {
    attr(node, "role").is_some_and(|role| role.eq_ignore_ascii_case("doc-pagebreak"))
        || attr(node, "epub:type")
            .is_some_and(|kinds| kinds.split_ascii_whitespace().any(|kind| kind == "pagebreak"))
}

// ---- addresses --------------------------------------------------------------

/// Whether `href` names somewhere outside the file: `https:`, `mailto:`, and
/// so on.
pub fn has_scheme(href: &str) -> bool {
    let Some((scheme, _)) = href.split_once(':') else { return false };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|ch| ch.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

/// A reference split into its path and its fragment, the query left off.
pub fn split_href(href: &str) -> (&str, &str) {
    let href = href.trim();
    let (path, fragment) = href.split_once('#').unwrap_or((href, ""));
    (path.split('?').next().unwrap_or(path), fragment)
}

/// What `%XX` escapes in `text` stand for. One that is not valid is left as
/// it is.
pub fn percent_decode(text: &str) -> String {
    if !text.contains('%') {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' && at + 2 < bytes.len() {
            let high = (bytes[at + 1] as char).to_digit(16);
            let low = (bytes[at + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                out.push((high * 16 + low) as u8);
                at += 3;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The path `href` names, from the archive's root, resolved against the file
/// `base_file` that names it: `..` and `.` taken out, `%XX` decoded, and
/// nothing let climb above the root.
pub fn resolve_path(base_file: &str, href: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !href.starts_with('/')
        && let Some((dir, _)) = base_file.rsplit_once('/')
    {
        parts.extend(dir.split('/').filter(|part| !part.is_empty()).map(str::to_string));
    }
    for segment in href.split('/') {
        let segment = percent_decode(segment);
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            parts.pop();
        } else {
            parts.push(segment);
        }
    }
    parts.join("/")
}

/// Where a link (or a table of contents entry) in the file `base` points, as
/// the file's name in the archive and an element's id in it -- or `None`
/// for a link to somewhere else, and for a file that is not in the archive.
pub fn link_target(base: &str, href: &str, exists: Option<Exists<'_>>) -> Option<(String, String)> {
    let href = href.trim();
    if href.is_empty() || href.starts_with("//") || has_scheme(href) {
        return None;
    }
    let (path, fragment) = split_href(href);
    let fragment = percent_decode(fragment);
    if path.is_empty() {
        return Some((base.to_string(), fragment));
    }
    let target = resolve_path(base, path);
    let target = match exists {
        Some(exists) => exists(&target)?,
        None => target,
    };
    Some((target, fragment))
}

// ---- a page on its own --------------------------------------------------------

/// Read a web page: its title, author and description from the head, its
/// body as blocks, one chapter, and its headings as the contents.
pub fn parse(bytes: &[u8]) -> Result<DocText, ParseError> {
    let source = decode(bytes, true);
    let page = Document::from(source.as_str());
    let mut walk = Walk::new(None, MAX_BLOCKS);
    walk.page(&page, "");
    walk.finish();

    let mut doc = DocText::new(DocFormat::Html);
    doc.meta = page_meta(&page);
    if walk.capped {
        doc.warnings.push(format!(
            "This page is very long; only the first {MAX_BLOCKS} paragraphs were read."
        ));
    }
    doc.blocks = walk.blocks;
    if doc.blocks.is_empty() {
        doc.warnings.push("This page has no text in it.".into());
    } else {
        doc.chapters.push(DocChapter {
            first_block: 0,
            blocks: doc.blocks.len() as u32,
            href: String::new(),
        });
    }
    if doc.meta.title.is_empty()
        && let Some(heading) =
            doc.blocks.iter().find(|block| block.kind == DocBlockKind::Heading && block.level == 1)
    {
        doc.meta.title = heading.text.clone();
    }
    super::finish_flow(&mut doc);
    Ok(doc)
}

/// What a page says about itself in its head.
fn page_meta(page: &Document) -> DocMeta {
    let title = collapse(&page.select("title").first().text());
    let language = page.select("html").first().attr("lang").map(|lang| lang.trim().to_string());
    let mut authors = Vec::new();
    let mut description = String::new();
    let mut subjects = Vec::new();
    for node in page.select("meta").nodes() {
        let (Some(name), Some(content)) = (attr(node, "name"), attr(node, "content")) else {
            continue;
        };
        let content = collapse(&content);
        if content.is_empty() {
            continue;
        }
        match name.to_ascii_lowercase().as_str() {
            "author" => authors = split_names(&content),
            "description" => description = content,
            "keywords" => subjects = split_keywords(&content),
            _ => {}
        }
    }
    DocMeta {
        title,
        authors,
        language: language.unwrap_or_default(),
        description,
        subjects,
        ..DocMeta::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(html: &str) -> DocText {
        parse(html.as_bytes()).unwrap()
    }

    fn texts(doc: &DocText) -> Vec<(DocBlockKind, &str)> {
        doc.blocks.iter().map(|block| (block.kind, block.text.as_str())).collect()
    }

    #[test]
    fn a_page_is_read_block_by_block() {
        let doc = read(
            "<html><head><title>The Page</title><style>p { color: red }</style></head><body>\
             <h1>Top</h1><p>One  <em>two</em>\n three.</p><div>Loose text</div>\
             <blockquote><p>Said one.</p><p>Said two.</p></blockquote>\
             <ul><li>a</li><li>b <b>c</b></li></ul><pre>  x\n y</pre><hr><dl><dt>Term</dt><dd>Meaning</dd></dl>\
             <script>var gone = 1;</script></body></html>",
        );
        assert_eq!(
            texts(&doc),
            [
                (DocBlockKind::Heading, "Top"),
                (DocBlockKind::Paragraph, "One two three."),
                (DocBlockKind::Paragraph, "Loose text"),
                (DocBlockKind::Quote, "Said one."),
                (DocBlockKind::Quote, "Said two."),
                (DocBlockKind::ListItem, "a"),
                (DocBlockKind::ListItem, "b c"),
                (DocBlockKind::Code, "  x\n y"),
                (DocBlockKind::Rule, ""),
                (DocBlockKind::Paragraph, "Term"),
                (DocBlockKind::Paragraph, "Meaning"),
            ]
        );
        assert_eq!(doc.blocks[0].level, 1);
        assert_eq!(doc.blocks[1].html, "One <em>two</em> three.");
        assert_eq!(doc.blocks[7].html, "  x\n y");
        assert_eq!(doc.meta.title, "The Page");
        assert_eq!(doc.chapters.len(), 1);
        assert_eq!(doc.chapters[0].blocks as usize, doc.blocks.len());
    }

    #[test]
    fn the_head_says_who_wrote_it_and_what_it_is() {
        let doc = read(
            "<html lang=\"fr\"><head><title> Un  titre </title>\
             <meta name=\"Author\" content=\"A. One; B. Two and C. Three\">\
             <meta name=\"description\" content=\"About   things\">\
             <meta name=\"keywords\" content=\"x, y\"></head><body><p>Hi</p></body></html>",
        );
        assert_eq!(doc.meta.title, "Un titre");
        assert_eq!(doc.meta.authors, ["A. One", "B. Two", "C. Three"]);
        assert_eq!(doc.meta.description, "About things");
        assert_eq!(doc.meta.subjects, ["x", "y"]);
        assert_eq!(doc.meta.language, "fr");
    }

    #[test]
    fn without_a_title_the_first_big_heading_names_the_page() {
        let doc = read("<body><h2>Small</h2><h1>Big</h1></body>");
        assert_eq!(doc.meta.title, "Big");
    }

    #[test]
    fn a_table_of_data_is_a_paragraph_a_row_and_a_layout_is_walked() {
        let doc = read(
            "<table><caption>Ages</caption><tr><th>Name</th><th>Age</th></tr>\
             <tr><td>Ann</td><td></td><td>31</td></tr></table>\
             <table><tr><td><p>Left side.</p></td><td><p>Right side.</p></td></tr></table>",
        );
        assert_eq!(
            texts(&doc),
            [
                (DocBlockKind::Paragraph, "Ages"),
                (DocBlockKind::Paragraph, "Name · Age"),
                (DocBlockKind::Paragraph, "Ann · 31"),
                (DocBlockKind::Paragraph, "Left side."),
                (DocBlockKind::Paragraph, "Right side."),
            ]
        );
    }

    #[test]
    fn a_picture_on_a_page_of_its_own_is_kept_for_what_it_says_it_is_of() {
        let doc = read(
            "<p>Before <img src=\"a.png\" alt=\"A cat\"> after.</p><p><img src=\"b.png\"></p>\
             <svg><image href=\"c.png\"/></svg>",
        );
        assert_eq!(
            texts(&doc),
            [(DocBlockKind::Paragraph, "Before after."), (DocBlockKind::Image, "A cat")]
        );
        assert_eq!(doc.blocks[1].src, "");
    }

    #[test]
    fn links_within_the_page_point_at_blocks_and_the_rest_are_words() {
        let doc = read(
            "<p id=\"top\">Start <a href=\"#end\">go to the end</a>, <a href=\"https://example.com\">the web</a>, \
             <a href=\"other.html\">another file</a>, <a href=\"#nowhere\">nowhere</a>.</p>\
             <div id=\"end\"><p>The end.</p></div>",
        );
        let end = doc.blocks.iter().position(|block| block.text == "The end.").unwrap();
        assert_eq!(
            doc.blocks[0].html,
            format!(
                "Start <a data-block=\"{end}\">go to the end</a>, the web, another file, nowhere."
            )
        );
        assert_eq!(doc.blocks[0].text, "Start go to the end, the web, another file, nowhere.");
    }

    #[test]
    fn nothing_the_page_says_reaches_the_html_but_its_words() {
        let doc = read(
            "<p onclick=\"steal()\" style=\"x\">Hi<script>alert(1)</script> <b onmouseover=\"x()\">there</b>\
             <img src=\"x\" onerror=\"alert(1)\"> &lt;script&gt;alert(2)&lt;/script&gt; \
             <a href=\"javascript:alert(3)\">click</a> <a href=\"data:text/html,x\" id=\"i\">data</a>\
             \u{1}7\u{2}</p>",
        );
        for block in &doc.blocks {
            for bad in [
                "onclick",
                "onerror",
                "onmouseover",
                "javascript",
                "<script",
                "style=",
                "href",
                "\u{1}",
            ] {
                assert!(!block.html.contains(bad), "{bad} in {}", block.html);
            }
        }
        assert_eq!(
            doc.blocks[0].html,
            "Hi <b>there</b> &lt;script&gt;alert(2)&lt;/script&gt; click data7"
        );
    }

    #[test]
    fn hidden_and_embedded_things_are_not_read() {
        let doc = read(
            "<body><p>Kept</p><noscript>no</noscript><iframe>no</iframe>\
             <video>no</video><form><input value=\"no\"><button>no</button></form>\
             <span epub:type=\"pagebreak\" id=\"p9\">9</span></body>",
        );
        assert_eq!(texts(&doc), [(DocBlockKind::Paragraph, "Kept")]);
    }

    #[test]
    fn a_line_break_is_a_line_break_and_a_heading_is_one_line() {
        let doc = read("<p>Roses are red<br>Violets are blue<br><br></p><h2>Two<br>lines</h2>");
        assert_eq!(doc.blocks[0].text, "Roses are red\nViolets are blue");
        assert_eq!(doc.blocks[0].html, "Roses are red<br>Violets are blue");
        assert_eq!(doc.blocks[1].text, "Two lines");
    }

    #[test]
    fn an_inline_element_round_a_block_does_not_swallow_it() {
        let doc = read(
            "<body><a href=\"#x\"><h2>Linked heading</h2></a><span><p>In a span.</p></span></body>",
        );
        assert_eq!(
            texts(&doc),
            [(DocBlockKind::Heading, "Linked heading"), (DocBlockKind::Paragraph, "In a span.")]
        );
    }

    #[test]
    fn a_page_nested_without_end_is_read_to_a_depth_and_no_further() {
        let deep = format!("{}deep words{}", "<div>".repeat(5000), "</div>".repeat(5000));
        let doc = read(&deep);
        assert_eq!(doc.blocks.len(), 1);
        assert!(doc.blocks[0].text.contains("deep words"));
    }

    #[test]
    fn the_block_limit_cuts_a_page_short() {
        let mut walk = Walk::new(None, 3);
        let page = Document::from("<p>1</p><p>2</p><p>3</p><p>4</p><p>5</p>");
        walk.page(&page, "");
        assert_eq!(walk.blocks.len(), 3);
        assert!(walk.capped);
    }

    #[test]
    fn addresses_are_resolved_against_the_file_that_names_them() {
        assert_eq!(
            resolve_path("OEBPS/text/ch1.xhtml", "../images/a%20b.png"),
            "OEBPS/images/a b.png"
        );
        assert_eq!(resolve_path("OEBPS/text/ch1.xhtml", "./ch2.xhtml"), "OEBPS/text/ch2.xhtml");
        assert_eq!(resolve_path("ch1.xhtml", "ch2.xhtml"), "ch2.xhtml");
        assert_eq!(resolve_path("OEBPS/ch1.xhtml", "/META-INF/x.xml"), "META-INF/x.xml");
        assert_eq!(resolve_path("a/ch1.xhtml", "../../../x.png"), "x.png");
        assert_eq!(percent_decode("a%2Fb%zz%4"), "a/b%zz%4");
        assert!(
            has_scheme("https://x.org")
                && has_scheme("mailto:a@b.c")
                && !has_scheme("ch1.xhtml#a:b")
        );
        assert_eq!(split_href("ch2.xhtml?x=1#sec"), ("ch2.xhtml", "sec"));
        assert_eq!(
            link_target("a/b.xhtml", "#n%201", None),
            Some(("a/b.xhtml".into(), "n 1".into()))
        );
        assert_eq!(link_target("a/b.xhtml", "https://x.org/#a", None), None);
        assert_eq!(link_target("a/b.xhtml", "//x.org/a", None), None);
    }
}
