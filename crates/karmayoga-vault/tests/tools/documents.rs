//! The document tools -- `document_outline`, `read_document`,
//! `search_document` and `search_documents` -- through `tools::dispatch` over
//! a real vault and a real files folder, with a documents service that is a
//! fake: it hands back the text of a book built here and answers an index
//! search with whatever the test put in it. What the service's cache and index
//! do is the service's own tests'; what is checked here is what the model is
//! told, what it is refused, and where the document's words are kept.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use karmayoga_core::agent::tools::{self, ToolContext};
use karmayoga_core::documents::{
    DocBlock, DocBlockKind, DocFormat, DocIndexStatus, DocSearch, DocSearchHit, DocSearchPage,
    DocText, Documents,
};
use karmayoga_core::files::{FileOrigin, ImportOptions};
use karmayoga_core::{FileEntry, FileId, Vault};
use serde_json::{Value, json};

use super::support::{ctx, vault};

/// A documents service over a few texts. `found` is what an index search
/// answers; `asked` is what it was asked.
struct Library {
    texts: HashMap<FileId, Arc<DocText>>,
    found: DocSearchPage,
    asked: Mutex<Vec<DocSearch>>,
}

impl Documents for Library {
    fn text(&self, file: &FileEntry) -> karmayoga_core::Result<Arc<DocText>> {
        self.texts.get(&file.id).cloned().ok_or_else(|| {
            karmayoga_core::Error::Invalid(format!("the fake has no text for {}", file.name))
        })
    }

    fn search(&self, query: &DocSearch) -> karmayoga_core::Result<DocSearchPage> {
        self.asked.lock().unwrap().push(query.clone());
        Ok(self.found.clone())
    }

    fn index_status(&self) -> DocIndexStatus {
        DocIndexStatus::default()
    }
}

fn heading(level: u8, text: &str) -> DocBlock {
    DocBlock { kind: DocBlockKind::Heading, level, text: text.into(), ..Default::default() }
}

fn para(text: String) -> DocBlock {
    DocBlock { text, ..Default::default() }
}

/// A book of fifty blocks and forty-seven made-up pages: two parts, three
/// chapters, and a thousand characters a paragraph, so that a part is far more
/// than one read holds.
///
/// ```text
///  0 # Part One            43 # Part Two
///  1 ## Chapter 1          44 ## Chapter 3
///  2..=21 paragraphs       45..=49 paragraphs about Mortimer
/// 22 ## Chapter 2
/// 23..=42 paragraphs
/// ```
fn a_long_book() -> DocText {
    let filler = "word ".repeat(200);
    let mut book = DocText::new(DocFormat::Epub);
    // A tag character is invisible and a model reads it as ASCII.
    book.meta.title = format!("A Long Book{}", '\u{e0041}');
    book.meta.authors = vec!["Ann Author".into()];
    book.warnings = vec!["Page 9 has no text in it.".into()];
    book.blocks.push(heading(1, "Part One"));
    book.blocks.push(heading(2, "Chapter 1"));
    for n in 0..20 {
        book.blocks.push(para(format!("First chapter paragraph {n}. {filler}")));
    }
    book.blocks.push(heading(2, "Chapter 2"));
    for n in 0..20 {
        book.blocks.push(para(format!("Second chapter paragraph {n}. {filler}")));
    }
    book.blocks.push(heading(1, "Part Two"));
    book.blocks.push(heading(2, "Chapter 3"));
    for n in 0..5 {
        book.blocks.push(para(format!("Mortimer rode out for the {n}th time. {filler}")));
    }
    book.assign_synthetic_pages();
    book.sections_from_headings();
    book.link_sections();
    book
}

/// A page of notes in Markdown.
fn some_notes() -> DocText {
    let mut notes = DocText::new(DocFormat::Markdown);
    notes.blocks = vec![heading(1, "Notes"), para("Mortimer called.".into())];
    notes.assign_synthetic_pages();
    notes.sections_from_headings();
    notes.link_sections();
    notes
}

struct World {
    vault: Vault,
    docs: Library,
    novel: FileEntry,
    notes: FileEntry,
    labs: FileEntry,
    csv: FileEntry,
    /// Last, so that it goes after the vault that is open inside it.
    _dir: tempfile::TempDir,
}

impl World {
    fn ctx(&self) -> ToolContext<'_> {
        ToolContext { documents: Some(&self.docs), ..ctx(&self.vault) }
    }

    fn call(&self, tool: &str, args: Value) -> Value {
        tools::dispatch(&self.ctx(), tool, &args).unwrap_or_else(|e| panic!("{tool} failed: {e}"))
    }

    fn refused(&self, tool: &str, args: Value) -> String {
        tools::dispatch(&self.ctx(), tool, &args)
            .map(|v| panic!("{tool} unexpectedly succeeded: {v}"))
            .unwrap_err()
            .to_string()
    }

    fn asked(&self) -> Vec<DocSearch> {
        self.docs.asked.lock().unwrap().clone()
    }

    /// The assistant shut out of their files.
    fn shut_out(&self) {
        let mut settings = self.vault.files_settings().unwrap();
        settings.assistant_access = false;
        self.vault.save_files_settings(&settings).unwrap();
    }
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    // A folder of its own: the default one is shared by every test here.
    vault.set_up_files_folder(Some(&dir.path().join("Karma Yoga"))).unwrap();
    let into = |folder: &str| ImportOptions {
        dir: Some(folder.into()),
        ..ImportOptions::new(FileOrigin::Files)
    };
    let novel = vault.import_bytes(b"epub", "novel.epub", &into("Books")).unwrap();
    let notes = vault.import_bytes(b"# Notes", "notes.md", &into("Papers")).unwrap();
    let csv = vault.import_bytes(b"a,b", "data.csv", &into("Books")).unwrap();
    let sealed = ImportOptions { encrypted: Some(true), ..ImportOptions::new(FileOrigin::Health) };
    let labs = vault.import_bytes(b"TSH 5.6", "labs.md", &sealed).unwrap();

    let mut texts = HashMap::new();
    texts.insert(novel.id, Arc::new(a_long_book()));
    texts.insert(notes.id, Arc::new(some_notes()));
    texts.insert(labs.id, Arc::new(some_notes()));
    let docs = Library { texts, found: DocSearchPage::default(), asked: Mutex::new(Vec::new()) };
    World { vault, docs, novel, notes, labs, csv, _dir: dir }
}

fn id(file: &FileEntry) -> String {
    file.id.to_string()
}

// ---- document_outline -------------------------------------------------------

#[test]
fn the_outline_says_how_a_book_is_laid_out_with_its_words_kept_apart() {
    let w = world();
    let out = w.call("document_outline", json!({ "id": id(&w.novel) }));

    assert_eq!(out["name"], "novel.epub");
    assert_eq!(out["id"], id(&w.novel));
    assert_eq!(out["format"], "epub");
    assert_eq!(out["pages"], 47);
    assert_eq!(out["ownPages"], false, "an EPUB's pages are made up");
    assert_eq!(out["blocks"], 50);
    assert_eq!(out["sectionCount"], 5);
    assert!(out.get("more").is_none(), "everything was listed: {out}");

    // The contents, by the index `read_document` takes.
    let words = &out["untrusted_text"];
    let sections = words["sections"].as_array().unwrap();
    assert_eq!(sections.len(), 5);
    assert_eq!(sections[0], json!({ "index": 0, "level": 1, "title": "Part One", "page": 1 }));
    assert_eq!(sections[4]["title"], "Chapter 3");
    assert_eq!(sections[4]["level"], 2);
    assert_eq!(sections[4]["page"], 43);

    // What the file says about itself, and the parser's warnings, are the
    // document's words, with the invisible characters taken out.
    assert_eq!(words["title"], "A Long Book");
    assert_eq!(words["authors"], json!(["Ann Author"]));
    assert_eq!(words["warnings"], json!(["Page 9 has no text in it."]));
    for key in ["title", "authors", "sections", "warnings", "description"] {
        assert!(out.get(key).is_none(), "{key} is only ever under untrusted_text: {out}");
    }
}

#[test]
fn a_pdfs_pages_are_its_own() {
    let mut w = world();
    let mut pdf = DocText::new(DocFormat::Pdf);
    pdf.blocks = vec![para("One page.".into())];
    pdf.assign_synthetic_pages();
    w.docs.texts.insert(w.notes.id, Arc::new(pdf));
    let out = w.call("document_outline", json!({ "id": id(&w.notes) }));
    assert_eq!(out["format"], "pdf");
    assert_eq!(out["ownPages"], true);
    assert!(out["note"].as_str().unwrap().contains("no table of contents"), "{out}");
}

// ---- read_document ----------------------------------------------------------

fn text_of(out: &Value) -> &str {
    out["untrusted_text"]["text"].as_str().unwrap()
}

#[test]
fn pages_are_read_with_their_markers_and_the_document_stays_under_untrusted_text() {
    let w = world();
    let out = w.call("read_document", json!({ "id": id(&w.novel), "pages": "43" }));
    assert_eq!(out["name"], "novel.epub");
    assert_eq!((out["fromPage"].as_u64(), out["toPage"].as_u64()), (Some(43), Some(43)));
    assert_eq!((out["fromBlock"].as_u64(), out["toBlock"].as_u64()), (Some(44), Some(45)));
    assert_eq!(out["truncated"], false);
    assert!(out.get("nextBlock").is_none());
    assert_eq!(out["totalPages"], 47);
    assert_eq!(out["totalBlocks"], 50);
    let text = text_of(&out);
    assert!(text.starts_with("[Page 43]\n\n## Chapter 3\n\nMortimer rode out for the 0th time."));
    assert!(out.get("text").is_none(), "the words are only ever under untrusted_text");

    // A range, a number sent as a number, and the pages of a range that runs
    // past the end.
    let two = w.call("read_document", json!({ "id": id(&w.novel), "pages": "46-47" }));
    assert_eq!((two["fromPage"].as_u64(), two["toPage"].as_u64()), (Some(46), Some(47)));
    assert!(text_of(&two).contains("[Page 46]") && text_of(&two).contains("[Page 47]"));
    let number = w.call("read_document", json!({ "id": id(&w.novel), "pages": 47 }));
    assert_eq!(number["fromPage"], 47);
    let clamped = w.call("read_document", json!({ "id": id(&w.novel), "pages": "46-99" }));
    assert_eq!(clamped["toPage"], 47, "a range is cut to the end of the document");
}

#[test]
fn a_page_past_the_end_a_bad_range_and_two_ways_of_asking_are_refused_in_words() {
    let w = world();
    let book = id(&w.novel);
    let past = w.refused("read_document", json!({ "id": book, "pages": "48" }));
    assert!(past.contains("past the end") && past.contains("47 pages"), "{past}");
    let zero = w.refused("read_document", json!({ "id": book, "pages": "0" }));
    assert!(zero.contains("start at 1"), "{zero}");
    let backwards = w.refused("read_document", json!({ "id": book, "pages": "9-3" }));
    assert!(backwards.contains("backwards"), "{backwards}");
    let nonsense = w.refused("read_document", json!({ "id": book, "pages": "the end" }));
    assert!(nonsense.contains("like 47"), "{nonsense}");
    let both = w.refused("read_document", json!({ "id": book, "pages": "1", "section": 1 }));
    assert!(both.contains("only one of"), "{both}");
}

#[test]
fn a_section_is_read_with_everything_under_it() {
    let w = world();
    let out = w.call("read_document", json!({ "id": id(&w.novel), "section": 4 }));
    assert_eq!(out["section"], 4);
    assert_eq!((out["fromBlock"].as_u64(), out["toBlock"].as_u64()), (Some(44), Some(49)));
    assert_eq!(out["untrusted_text"]["section"], "Chapter 3");
    assert!(text_of(&out).contains("## Chapter 3"));
    assert_eq!(out["truncated"], false);

    let missing = w.refused("read_document", json!({ "id": id(&w.novel), "section": 9 }));
    assert!(missing.contains("no section 9") && missing.contains("0 to 4"), "{missing}");
    assert!(missing.contains("document_outline"), "{missing}");
}

#[test]
fn blocks_are_read_by_their_numbers() {
    let w = world();
    let book = id(&w.novel);
    let out = w.call("read_document", json!({ "id": book, "blocks": "45-46" }));
    assert_eq!((out["fromBlock"].as_u64(), out["toBlock"].as_u64()), (Some(45), Some(46)));
    assert!(text_of(&out).contains("for the 1th time"), "{}", text_of(&out));
    let one = w.call("read_document", json!({ "id": book, "blocks": "49" }));
    assert_eq!((one["fromBlock"].as_u64(), one["toBlock"].as_u64()), (Some(49), Some(49)));
    let clamped = w.call("read_document", json!({ "id": book, "blocks": "48-900" }));
    assert_eq!(clamped["toBlock"], 49);

    let past = w.refused("read_document", json!({ "id": book, "blocks": "50" }));
    assert!(past.contains("past the end") && past.contains("50 blocks"), "{past}");
}

#[test]
fn a_long_read_stops_on_a_block_and_carries_on_from_where_it_stopped() {
    let w = world();
    let book = id(&w.novel);
    // Part One is forty blocks of a thousand characters: far more than one read.
    let first = w.call("read_document", json!({ "id": book, "section": 0 }));
    assert_eq!(first["truncated"], true);
    let next = first["nextBlock"].as_u64().unwrap();
    assert!(next > 5 && next < 43, "{first}");
    assert_eq!(first["toBlock"].as_u64().unwrap() + 1, next, "it stops where it says it does");
    assert!(first["nextPage"].as_u64().unwrap() > 1);
    assert!(first["note"].as_str().unwrap().contains("start"), "{first}");
    assert!(text_of(&first).chars().count() <= 24_000, "{}", text_of(&first).chars().count());

    // Carrying on keeps to the same section: it ends where Part One does.
    let more = w.call("read_document", json!({ "id": book, "section": 0, "start": next }));
    assert_eq!(more["fromBlock"].as_u64().unwrap(), next);
    assert!(more["toBlock"].as_u64().unwrap() <= 42, "{more}");
    assert!(!text_of(&more).contains("Mortimer"), "Part Two is not in Part One");

    // With nothing named it is from the top, and `start` carries on from there.
    let top = w.call("read_document", json!({ "id": book }));
    assert_eq!(top["fromBlock"], 0);
    assert_eq!(top["truncated"], true);
    let onward = w.call("read_document", json!({ "id": book, "start": top["nextBlock"] }));
    assert_eq!(onward["fromBlock"], top["nextBlock"]);

    let beyond = w.refused("read_document", json!({ "id": book, "section": 4, "start": 50 }));
    assert!(beyond.contains("past the end"), "{beyond}");
}

// ---- search_document --------------------------------------------------------

#[test]
fn a_name_is_found_with_its_page_section_and_snippet() {
    let w = world();
    let out = w.call("search_document", json!({ "id": id(&w.novel), "query": "Mortimer" }));
    assert_eq!(out["name"], "novel.epub");
    assert_eq!(out["query"], "Mortimer");
    let hits = out["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 5);
    assert_eq!(hits[0]["block"], 45);
    assert_eq!(hits[0]["page"], 43);
    assert_eq!(hits[0]["sectionIndex"], 4);
    let words = &hits[0]["untrusted_text"];
    assert_eq!(words["section"], "Part Two \u{203a} Chapter 3");
    assert!(words["snippet"].as_str().unwrap().contains("Mortimer"), "{words}");
    assert!(hits[0].get("snippet").is_none() && hits[0].get("section").is_none());
    assert!(out.get("more").is_none());

    // A page of results is cut where it is asked to be, and says there is more.
    let args = json!({ "id": id(&w.novel), "query": "mortimer", "limit": 2 });
    let few = w.call("search_document", args);
    assert_eq!(few["hits"].as_array().unwrap().len(), 2);
    assert_eq!(few["more"], true);
}

#[test]
fn a_search_that_finds_nothing_says_what_to_try() {
    let w = world();
    let out = w.call("search_document", json!({ "id": id(&w.novel), "query": "zeppelin" }));
    assert_eq!(out["hits"], json!([]));
    let note = out["note"].as_str().unwrap();
    assert!(note.contains("Try other words") && note.contains("document_outline"), "{note}");

    let empty = w.refused("search_document", json!({ "id": id(&w.novel), "query": " ?! " }));
    assert!(empty.contains("no words"), "{empty}");
    let missing = w.refused("search_document", json!({ "id": id(&w.novel) }));
    assert!(missing.contains("`query` is required"), "{missing}");
}

// ---- who may read what ------------------------------------------------------

#[test]
fn every_tool_says_so_when_there_are_no_documents_to_read() {
    let w = world();
    let bare = ctx(&w.vault);
    for (tool, args) in [
        ("document_outline", json!({ "id": id(&w.novel) })),
        ("read_document", json!({ "id": id(&w.novel) })),
        ("search_document", json!({ "id": id(&w.novel), "query": "mortimer" })),
        ("search_documents", json!({ "query": "mortimer" })),
    ] {
        let err = tools::dispatch(&bare, tool, &args).unwrap_err().to_string();
        assert!(err.contains("not available right now"), "{tool}: {err}");
    }
}

#[test]
fn a_file_that_is_not_a_document_is_sent_to_the_tools_that_read_it() {
    let w = world();
    for tool in ["document_outline", "read_document"] {
        let err = w.refused(tool, json!({ "id": id(&w.csv) }));
        assert!(err.contains("not a document") && err.contains("read_file"), "{tool}: {err}");
    }
    let err = w.refused("search_document", json!({ "id": id(&w.csv), "query": "a" }));
    assert!(err.contains("read_file"), "{err}");

    // And read_file sends a book the other way.
    let epub = w.refused("read_file", json!({ "id": id(&w.novel) }));
    assert!(epub.contains("read_document") && epub.contains("an EPUB book"), "{epub}");
}

#[test]
fn healths_files_are_never_read_here_and_neither_is_the_trash() {
    let w = world();
    for (tool, args) in [
        ("document_outline", json!({ "id": id(&w.labs) })),
        ("read_document", json!({ "id": id(&w.labs) })),
        ("search_document", json!({ "id": id(&w.labs), "query": "tsh" })),
    ] {
        let err = w.refused(tool, args);
        assert!(err.contains("one of Health's"), "{tool}: {err}");
    }

    w.vault.trash_files(&[w.notes.id]).unwrap();
    let err = w.refused("document_outline", json!({ "id": id(&w.notes) }));
    assert!(err.contains("in the trash"), "{err}");
}

#[test]
fn with_the_assistant_shut_out_of_their_files_nothing_is_read_or_searched() {
    let w = world();
    w.shut_out();
    for (tool, args) in [
        ("document_outline", json!({ "id": id(&w.novel) })),
        ("read_document", json!({ "id": id(&w.novel) })),
        ("search_document", json!({ "id": id(&w.novel), "query": "mortimer" })),
        ("search_documents", json!({ "query": "mortimer" })),
    ] {
        let err = w.refused(tool, args);
        assert!(err.contains("has not allowed") && err.contains("Settings"), "{tool}: {err}");
    }
    assert!(w.asked().is_empty(), "the index was never asked");
}

// ---- search_documents -------------------------------------------------------

fn hit(file: &FileEntry, block: u32, page: u32, section: &str, snippet: &str) -> DocSearchHit {
    DocSearchHit {
        file: file.id,
        block,
        page,
        section: section.into(),
        snippet: snippet.into(),
        score: 1.0,
    }
}

/// An index that found four passages: two in the novel, one in the notes and
/// one in Health's sealed file, in that order of score.
fn with_hits(mut w: World) -> World {
    w.docs.found = DocSearchPage {
        hits: vec![
            hit(&w.novel, 5, 2, "Chapter 1", "Mortimer rode"),
            hit(&w.notes, 1, 1, "Notes", "Mortimer called"),
            hit(&w.novel, 9, 3, "Chapter 1", "and Mortimer wept"),
            hit(&w.labs, 1, 1, "Labs", "TSH 5.6"),
        ],
        total: 4,
        indexing: false,
    };
    w
}

#[test]
fn a_search_across_documents_is_grouped_by_file_and_leaves_out_healths() {
    let w = with_hits(world());
    let out = w.call("search_documents", json!({ "query": "mortimer" }));
    assert_eq!(out["query"], "mortimer");
    assert_eq!(out["total"], 4);
    assert_eq!(out["indexing"], false);
    assert!(out.get("more").is_none(), "{out}");

    let hits = out["hits"].as_array().unwrap();
    let seen: Vec<(&str, u64)> =
        hits.iter().map(|h| (h["name"].as_str().unwrap(), h["block"].as_u64().unwrap())).collect();
    // The novel's two passages together, ahead of the notes, which scored lower
    // than the first and higher than the second; Health's file is not there.
    assert_eq!(seen, [("novel.epub", 5), ("novel.epub", 9), ("notes.md", 1)]);
    assert_eq!(hits[0]["id"], id(&w.novel));
    assert_eq!(hits[0]["path"], "Books/novel.epub");
    assert_eq!(hits[0]["page"], 2);
    assert_eq!(hits[0]["untrusted_text"]["section"], "Chapter 1");
    assert_eq!(hits[0]["untrusted_text"]["snippet"], "Mortimer rode");
    assert!(hits.iter().all(|h| h["name"] != "labs.md"));

    let asked = w.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!((asked[0].text.as_str(), asked[0].limit, asked[0].offset), ("mortimer", 10, 0));
    assert!(asked[0].files.is_empty());
}

#[test]
fn a_folder_narrows_a_search_after_it_and_a_short_page_says_where_to_go_on() {
    let w = with_hits(world());
    let out = w.call("search_documents", json!({ "query": "mortimer", "folder": "Papers/" }));
    let hits = out["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["name"], "notes.md");
    assert_eq!(out["folder"], "Papers");
    assert!(out["note"].as_str().unwrap().contains("every folder"), "{out}");
    // More were fetched than were wanted, since some would be dropped.
    assert_eq!(w.asked()[0].limit, 40);

    // A folder that is not a folder is refused rather than matching nothing.
    let bad = w.refused("search_documents", json!({ "query": "mortimer", "folder": "../etc" }));
    assert!(bad.contains(".."), "{bad}");

    // A page cut by the limit says where the next one starts.
    let cut = w.call("search_documents", json!({ "query": "mortimer", "limit": 1 }));
    assert_eq!(cut["hits"].as_array().unwrap().len(), 1);
    assert_eq!(cut["more"], true);
    assert_eq!(cut["nextOffset"], 1);
}

#[test]
fn a_search_across_documents_says_when_the_index_is_off_and_does_not_search() {
    let w = with_hits(world());
    let mut settings = w.vault.files_settings().unwrap();
    settings.index_documents = false;
    w.vault.save_files_settings(&settings).unwrap();

    let out = w.call("search_documents", json!({ "query": "mortimer" }));
    assert_eq!(out["hits"], json!([]));
    assert_eq!(out["indexing"], false);
    let note = out["note"].as_str().unwrap();
    assert!(note.contains("switched off") && note.contains("find_files"), "{note}");
    assert!(note.contains("search_document"), "{note}");
    assert!(w.asked().is_empty(), "there is no index to ask");
}

#[test]
fn nothing_found_across_documents_says_what_to_try_and_whether_it_is_still_indexing() {
    let mut w = world();
    w.docs.found = DocSearchPage { hits: Vec::new(), total: 0, indexing: true };
    let out = w.call("search_documents", json!({ "query": "zeppelin" }));
    assert_eq!(out["hits"], json!([]));
    assert_eq!(out["indexing"], true);
    assert!(out["note"].as_str().unwrap().contains("indexing is still under way"), "{out}");

    w.docs.found.indexing = false;
    let out = w.call("search_documents", json!({ "query": "zeppelin" }));
    assert!(out["note"].as_str().unwrap().contains("Try other words"), "{out}");
}
