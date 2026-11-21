//! Reading text and Markdown. See [`super`].
//!
//! Plain text is its paragraphs, split where a line is blank, with the line
//! breaks inside each kept: a file written to be read in a fixed-width window
//! is drawn that way. Markdown goes through `pulldown-cmark` and comes out as
//! the same blocks a web page does, so a `# heading` and an `<h1>` are the
//! same thing to the reader and the assistant. Raw HTML in Markdown is left
//! out, as are the destinations of links and pictures: a link is its words,
//! and a picture -- which has no file to draw from -- is kept for its
//! description.

use karmayoga_core::documents::{DocBlock, DocBlockKind, DocChapter, DocFormat, DocMeta, DocText};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use super::html::Run;
use super::{ParseError, collapse, decode, escape, split_names};

/// Read a plain text file: a paragraph where lines are not blank, in one
/// chapter.
pub fn parse_text(bytes: &[u8]) -> Result<DocText, ParseError> {
    let text = normalise(&decode(bytes, false));
    let mut doc = DocText::new(DocFormat::Text);
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if !lines.is_empty() {
                doc.blocks.push(paragraph(&lines));
                lines.clear();
            }
        } else {
            lines.push(line.trim_end());
        }
    }
    one_chapter(&mut doc, "This file has no text in it.");
    super::finish_flow(&mut doc);
    Ok(doc)
}

/// A paragraph of lines, with its line breaks.
fn paragraph(lines: &[&str]) -> DocBlock {
    DocBlock {
        kind: DocBlockKind::Paragraph,
        text: lines.join("\n"),
        html: lines.iter().map(|line| escape(line)).collect::<Vec<_>>().join("<br>"),
        ..DocBlock::default()
    }
}

/// Line endings made `\n`, and everything else that is a control character
/// but a tab dropped: a NUL in a text file is a mistake, and in a page it is
/// a character no font draws.
fn normalise(text: &str) -> String {
    let mut out: String = text.replace("\r\n", "\n").replace('\r', "\n");
    out.retain(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'));
    out
}

/// The whole of a document is one chapter, if it has anything in it; and if
/// it has not, say so.
fn one_chapter(doc: &mut DocText, empty: &str) {
    if doc.blocks.is_empty() {
        doc.warnings.push(empty.into());
    } else {
        doc.chapters.push(DocChapter {
            first_block: 0,
            blocks: doc.blocks.len() as u32,
            href: String::new(),
        });
    }
}

// ---- Markdown ---------------------------------------------------------------

/// Read a Markdown file: headings, paragraphs, list items, quotations, code,
/// rules, table rows and pictures' descriptions as blocks; emphasis, strong
/// emphasis and code spans as the inline elements the reader draws; one
/// chapter; and the title from the first heading of level one.
pub fn parse_markdown(bytes: &[u8]) -> Result<DocText, ParseError> {
    let text = normalise(&decode(bytes, false));
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    let mut read = Reading::default();
    for event in Parser::new_ext(&text, options) {
        read.event(event);
    }
    read.flush();

    let mut doc = DocText::new(DocFormat::Markdown);
    let (title, authors) = front_matter(&read.front);
    doc.meta = DocMeta {
        title: if read.title.is_empty() { title } else { read.title },
        authors,
        ..DocMeta::default()
    };
    doc.blocks = read.blocks;
    one_chapter(&mut doc, "This file has no text in it.");
    super::finish_flow(&mut doc);
    Ok(doc)
}

/// Where a pass over a Markdown file has got to.
#[derive(Default)]
struct Reading {
    blocks: Vec<DocBlock>,
    /// The words of the block being read.
    run: Run,
    /// The block each open quotation or list item makes of its words. The
    /// innermost is last; with none open, a paragraph.
    inside: Vec<DocBlockKind>,
    /// The level of the heading being read.
    heading: Option<u8>,
    /// A code block's text, while one is open.
    code: Option<String>,
    /// A picture's description, while one is open.
    alt: Option<String>,
    /// The title the picture was given, for want of a description.
    alt_title: String,
    /// A footnote's label, until the first words of its text have it written.
    note: Option<String>,
    /// The front matter, if the file has any.
    front: String,
    in_front: bool,
    /// The metadata block being read is not at the start, so it is not front
    /// matter at all but a stretch of text between two rules.
    late: bool,
    /// The first heading of level one.
    title: String,
}

impl Reading {
    fn kind(&self) -> DocBlockKind {
        self.inside.last().copied().unwrap_or(DocBlockKind::Paragraph)
    }

    /// End the block being read, as the kind it is read inside.
    fn flush(&mut self) {
        let kind = self.kind();
        self.end(kind, 0);
    }

    fn end(&mut self, kind: DocBlockKind, level: u8) {
        self.blocks.extend(self.run.take_blocks(kind, level));
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.stop(tag),
            Event::Text(words) => {
                if self.in_front && self.late {
                    self.run.text(&words);
                } else if self.in_front {
                    self.front.push_str(&words);
                } else if let Some(code) = &mut self.code {
                    code.push_str(&words);
                } else if let Some(alt) = &mut self.alt {
                    alt.push_str(&words);
                } else {
                    self.run.text(&words);
                }
            }
            Event::Code(words) => {
                if let Some(alt) = &mut self.alt {
                    alt.push_str(&words);
                } else {
                    self.run.open("code");
                    self.run.text(&words);
                    self.run.close("code");
                }
            }
            Event::SoftBreak => match &mut self.alt {
                Some(alt) => alt.push(' '),
                None => self.run.text(" "),
            },
            Event::HardBreak => self.run.line_break(),
            Event::Rule => {
                self.flush();
                self.blocks.push(DocBlock { kind: DocBlockKind::Rule, ..DocBlock::default() });
            }
            Event::TaskListMarker(done) => self.run.text(if done { "[x] " } else { "[ ] " }),
            Event::FootnoteReference(label) => {
                self.run.open("sup");
                self.run.text(&format!("[{label}]"));
                self.run.close("sup");
            }
            // Raw HTML is not read: it is a stranger's markup, not words.
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                self.flush();
                if let Some(label) = self.note.take() {
                    self.run.text(&format!("[{label}] "));
                }
            }
            Tag::Heading { level, .. } => {
                self.flush();
                self.run = Run::flat();
                self.heading = Some(level as u8);
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.inside.push(DocBlockKind::Quote);
            }
            Tag::Item => {
                self.flush();
                self.inside.push(DocBlockKind::ListItem);
            }
            Tag::List(_) | Tag::Table(_) | Tag::HtmlBlock => self.flush(),
            Tag::CodeBlock(_) => {
                self.flush();
                self.code = Some(String::new());
            }
            Tag::TableHead | Tag::TableRow => {
                self.flush();
                self.run = Run::flat();
            }
            Tag::TableCell => self.run.cell_break(),
            Tag::Emphasis => self.run.open("em"),
            Tag::Strong => self.run.open("strong"),
            Tag::Image { title, .. } => {
                self.alt = Some(String::new());
                self.alt_title = title.to_string();
            }
            Tag::FootnoteDefinition(label) => {
                self.flush();
                self.note = Some(label.to_string());
            }
            Tag::MetadataBlock(_) => {
                self.flush();
                self.in_front = true;
                self.late = !self.blocks.is_empty();
            }
            _ => {}
        }
    }

    fn stop(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::FootnoteDefinition | TagEnd::List(_) | TagEnd::Table => {
                self.flush();
            }
            TagEnd::Heading(_) => {
                let level = self.heading.take().unwrap_or(1);
                if level == 1 && self.title.is_empty() {
                    self.title = collapse(self.run.plain());
                }
                self.end(DocBlockKind::Heading, level);
                self.run = Run::default();
            }
            TagEnd::BlockQuote(_) | TagEnd::Item => {
                self.flush();
                self.inside.pop();
            }
            TagEnd::CodeBlock => {
                if let Some(code) = self.code.take() {
                    let code = code.trim_end();
                    if !code.is_empty() {
                        self.blocks.push(DocBlock {
                            kind: DocBlockKind::Code,
                            text: code.to_string(),
                            html: escape(code),
                            ..DocBlock::default()
                        });
                    }
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                self.flush();
                self.run = Run::default();
            }
            TagEnd::Emphasis => self.run.close("em"),
            TagEnd::Strong => self.run.close("strong"),
            TagEnd::Image => {
                let alt = collapse(&self.alt.take().unwrap_or_default());
                let alt = if alt.is_empty() { collapse(&self.alt_title) } else { alt };
                if !alt.is_empty() {
                    self.run.picture(String::new(), alt);
                }
            }
            TagEnd::MetadataBlock(_) => {
                self.in_front = false;
                if self.late {
                    self.flush();
                }
            }
            _ => {}
        }
    }
}

/// A title and authors out of the plain `key: value` lines of a file's front
/// matter, which is the most of it that is read: the words are the file's own
/// and mean whatever its author's tools made them mean.
fn front_matter(front: &str) -> (String, Vec<String>) {
    let mut title = String::new();
    let mut authors = Vec::new();
    for line in front.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim().trim_matches(['[', ']', '"', '\'']).trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "title" if title.is_empty() => title = collapse(value),
            "author" | "authors" if authors.is_empty() => authors = split_names(value),
            _ => {}
        }
    }
    (title, authors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(doc: &DocText) -> Vec<(DocBlockKind, &str)> {
        doc.blocks.iter().map(|block| (block.kind, block.text.as_str())).collect()
    }

    #[test]
    fn text_is_a_paragraph_wherever_lines_are_not_blank() {
        let doc =
            parse_text(b"First line\r\nsecond <line>.\r\n\r\n\r\n   \r\nNext & last\n").unwrap();
        assert_eq!(doc.format, DocFormat::Text);
        assert_eq!(
            texts(&doc),
            [
                (DocBlockKind::Paragraph, "First line\nsecond <line>."),
                (DocBlockKind::Paragraph, "Next & last")
            ]
        );
        assert_eq!(doc.blocks[0].html, "First line<br>second &lt;line&gt;.");
        assert_eq!(doc.chapters.len(), 1);
        assert_eq!(doc.chapters[0].blocks, 2);
        assert!(doc.warnings.is_empty());
    }

    #[test]
    fn text_in_utf16_is_read_by_its_mark() {
        let mut bytes = vec![0xff, 0xfe];
        for unit in "Caf\u{e9} au lait\n\nTwo".encode_utf16() {
            bytes.extend(unit.to_le_bytes());
        }
        let doc = parse_text(&bytes).unwrap();
        assert_eq!(
            texts(&doc),
            [(DocBlockKind::Paragraph, "Caf\u{e9} au lait"), (DocBlockKind::Paragraph, "Two")]
        );
    }

    #[test]
    fn an_empty_file_is_a_document_with_a_warning() {
        for bytes in [&b""[..], b"\n \n\t\n", b"\0\0"] {
            let doc = parse_text(bytes).unwrap();
            assert!(doc.blocks.is_empty());
            assert!(doc.chapters.is_empty());
            assert_eq!(doc.warnings, ["This file has no text in it."]);
        }
        assert!(parse_markdown(b"").unwrap().blocks.is_empty());
    }

    #[test]
    fn markdown_is_headings_paragraphs_lists_quotes_code_and_rules() {
        let doc = parse_markdown(
            b"# The Title\n\nSome *emphasis*, **strong** and `code <b>` with a [link](http://x.org) \
              and <span onclick=x>raw html</span>.\n\n## Second\n\n- one\n- two with **bold**\n  - nested\n\n\
              1. first\n2. second\n\n> quoted\n> across lines\n\n> second para\n>\n> and another\n\n\
              ```rust\nfn main() {}\n\n```\n\n---\n\nSoft\nbreak and hard  \nbreak.\n",
        )
        .unwrap();
        assert_eq!(doc.format, DocFormat::Markdown);
        assert_eq!(doc.meta.title, "The Title");
        assert_eq!(
            texts(&doc),
            [
                (DocBlockKind::Heading, "The Title"),
                (
                    DocBlockKind::Paragraph,
                    "Some emphasis, strong and code <b> with a link and raw html."
                ),
                (DocBlockKind::Heading, "Second"),
                (DocBlockKind::ListItem, "one"),
                (DocBlockKind::ListItem, "two with bold"),
                (DocBlockKind::ListItem, "nested"),
                (DocBlockKind::ListItem, "first"),
                (DocBlockKind::ListItem, "second"),
                (DocBlockKind::Quote, "quoted across lines"),
                (DocBlockKind::Quote, "second para"),
                (DocBlockKind::Quote, "and another"),
                (DocBlockKind::Code, "fn main() {}"),
                (DocBlockKind::Rule, ""),
                (DocBlockKind::Paragraph, "Soft break and hard\nbreak."),
            ]
        );
        assert_eq!(doc.blocks[0].level, 1);
        assert_eq!(doc.blocks[2].level, 2);
        assert_eq!(
            doc.blocks[1].html,
            "Some <em>emphasis</em>, <strong>strong</strong> and <code>code &lt;b&gt;</code> with a link and raw html."
        );
        assert_eq!(doc.blocks[11].html, "fn main() {}");
        assert_eq!(doc.chapters.len(), 1);
        assert_eq!(doc.chapters[0].blocks as usize, doc.blocks.len());
    }

    #[test]
    fn markdown_tables_pictures_footnotes_and_front_matter() {
        let doc = parse_markdown(
            b"---\ntitle: \"From the front\"\nauthor: A. One and B. Two\n---\n\n\
              | Name | Age |\n|------|-----|\n| Ann  | 31  |\n\n\
              ![A map of the county](map.png \"the map\")\n\n![](none.png \"Only a title\")\n\n![](bare.png)\n\n\
              Text[^1] and - [ ] not a task.\n\n- [x] done\n\n[^1]: The note.\n",
        )
        .unwrap();
        assert_eq!(doc.meta.title, "From the front");
        assert_eq!(doc.meta.authors, ["A. One", "B. Two"]);
        assert_eq!(
            texts(&doc),
            [
                (DocBlockKind::Paragraph, "Name · Age"),
                (DocBlockKind::Paragraph, "Ann · 31"),
                (DocBlockKind::Image, "A map of the county"),
                (DocBlockKind::Image, "Only a title"),
                (DocBlockKind::Paragraph, "Text[1] and - [ ] not a task."),
                (DocBlockKind::ListItem, "[x] done"),
                (DocBlockKind::Paragraph, "[1] The note."),
            ]
        );
        assert_eq!(doc.blocks[2].src, "");
        assert_eq!(doc.blocks[4].html, "Text<sup>[1]</sup> and - [ ] not a task.");
    }

    #[test]
    fn markdown_title_is_the_first_heading_of_level_one_and_not_a_later_one() {
        let doc = parse_markdown(b"## Not it\n\n# It\n\n# Not this either\n").unwrap();
        assert_eq!(doc.meta.title, "It");
        assert!(parse_markdown(b"No headings.").unwrap().meta.title.is_empty());
    }

    #[test]
    fn nothing_a_markdown_file_says_reaches_the_html_but_its_words() {
        let doc = parse_markdown(
            b"<script>alert(1)</script>\n\nText <img src=x onerror=alert(1)> [x](javascript:alert(2)) \
              <b onclick=\"y()\">bold</b>\n\n<div>block html</div>\n\n[ref]: javascript:alert(3)\n",
        )
        .unwrap();
        for block in &doc.blocks {
            for bad in ["onerror", "onclick", "javascript", "<script", "<img", "<b "] {
                assert!(!block.html.contains(bad), "{bad} in {}", block.html);
            }
        }
        assert_eq!(texts(&doc), [(DocBlockKind::Paragraph, "Text x bold")]);
    }
}
