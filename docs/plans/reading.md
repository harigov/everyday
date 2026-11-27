# Reading: documents in the files folder, read with the assistant

A plan, written 21 November 2026 against `f8495110`. Hari's calls are marked
as his.

## What this is

1. **A reader.** A PDF, an EPUB, a Markdown or text file or a web page in the
   files catalog opens on the stage -- with its contents beside it, a search
   inside it, and the assistant's rail on the right -- from Files or from a
   library item. It opens where they left off.
2. **Chatting with it, by text or voice.** The screen context says which
   document is open, which pages are in view and what is selected; the
   assistant reads it by its structure (contents, pages, sections, search)
   rather than whole.
3. **Asking about a document that is not open.** A library item's file, a
   file found by name, or a search over every indexed document.
4. **An index**, over the documents in the files folder, with the folders it
   covers chosen in Settings → Files.
5. **Library items with files.** A book's EPUB, an article's PDF; journal
   articles on the Articles shelf (Hari's call), which gains a DOI, volume,
   issue and pages beside the web articles it already held; an item made
   from a file, filled from what the file says about itself, a DOI looked up
   with Crossref.

## Decisions

- **Only files in the catalog** (Hari's call). Nothing outside the folder the
  app owns is opened, read, indexed or backed up: a file has to be copied in
  first. No "Open with" from the system.
- **The folder is the person's choice** (Hari's call) -- Settings → Files, as
  today -- and indexing covers only that folder: plain files, under
  `FilesSettings.index_folders` (empty is the whole folder) and not under
  `skip_folders`. A sealed file is read and searched on its own like any
  other document, but not indexed: it has no folder for the two lists to
  name.
- **Pure Rust parsers** (Hari's call: a reliable build over a perfect
  reading). PDF: `pdf-extract` for text page by page, `lopdf` (the version
  pdf-extract is built on) for the outline and information dictionary.
  EPUB: `zip` + `roxmltree` for the container, package and navigation, and
  `dom_query` for each chapter's XHTML. Markdown: `pulldown-cmark`. Every
  parse runs under `catch_unwind` with a size cap.
- **Parsed once, kept in the cache directory** (Hari's call), sealed under a
  subkey of the vault key: the folder may be on an encrypted volume the cache
  directory is not. Keyed by the file's content hash and `DOC_TEXT_VERSION`,
  so a moved or renamed file is not parsed again. Never inside the files
  folder, which a sync tool carries everywhere.
- **The visible pages are not sent with every message** (Hari's call). The
  screen context says where they are, by file, page and block -- never a
  heading's or a passage's words -- and the model reads what it needs with a
  tool. A PDF selection travels as up to 400 of its words, which the service
  finds in its own copy of the page; the model is told the blocks, not the
  words.
- **One address for a place**: a block's index in the document's reading
  order. The reader marks every block it draws with `data-block`; the
  assistant reads by it; a search hit and a citation (`files:<id>#b=812`,
  `files:<id>#p=47`) name it.
- **Structure as well as words.** Every document has sections (its own table
  of contents, else its headings), pages (a PDF's own, else made-up ones of
  about 1,800 characters that never split a block), and a per-document BM25
  search over its blocks -- so "who is Mortimer in this book" is a search and
  a read of where it lands.
- **The document tools are in the Files domain**, gated as `read_file` is:
  `assistant_access`, or the file handed over in the conversation. Health's
  files are never read. Every one arms the fetch check: a document's words
  are somebody else's.
- **Not built here:** semantic (embedding) search, OCR of scanned pages,
  reading aloud from the page, highlights. The index's passages and the
  cache are the places they would plug in.

## Where things are

| Layer | Files | Owner |
|---|---|---|
| Types, navigation, per-document search | `jatayu-core/src/documents.rs` | C |
| Assistant tools | `jatayu-core/src/agent/tools/documents.rs` | C |
| Screen context | `jatayu-core/src/agent/onscreen.rs` (`reading`) | C |
| Settings | `FilesSettings.index_documents/index_folders/skip_folders`, `FilesSettings::indexes` | contract |
| Parsers | `jatayu-service/src/documents/parse/` | A |
| Cache, state, index task | `jatayu-service/src/documents/{mod,cache,task}.rs` | B |
| Search index | `crates/jatayu-docindex` | B |
| Commands | `jatayu-service/src/domains/documents.rs` | B |
| Library: `Item.files`, Articles, item from a file, Crossref | `jatayu-core/src/library.rs`, `vault/library*`, `vault/files.rs` (`RefKind::Item`), `agent/tools/library.rs`, `websearch`, `domains/library.rs` | D |
| Wiring into turns (taint, handed-over tools, cards) | `jatayu-service/src/agent.rs`, `domains/meta.rs` | contract |
| UI data: store, api, mock, screen context, links | `ui/src/lib/reader.svelte.ts`, `documents.ts`, `mock-documents.ts`, `mock.ts`, `screen.svelte.ts`, `markdown.ts`, `library.svelte.ts`, `files.svelte.ts` | E |
| UI reader | `ReaderView.svelte`, `ReaderFlow.svelte`, `ReaderOutline.svelte`, `ReaderSearch.svelte`, `PdfView.svelte`, `App.svelte` | F |
| UI in Files, Library, Settings | `FilesView`, `FileViewer`, `FileInspector`, `ItemDetail`, `FilesSettingsPanel` | G |

## Commands (scope Files unless said)

| Command | Args | Returns |
|---|---|---|
| `document_outline` | `id` | `DocOutline` |
| `document_chapter` | `id, chapter` | `DocChapterView` |
| `search_document` | `id, query, limit?` | `DocHit[]` |
| `search_documents` | `query, limit?, offset?` | `DocSearchPage` |
| `save_reader_position` | `id, page, block?` | `void` |
| `documents_status` | | `DocIndexStatus` |
| `reindex_documents` | | `DocIndexStatus` |
| `create_item_from_file` (Library, needs Files too) | `file, kindId?` | `Item` |

## Assistant tools (Domain::Files)

| Tool | Does |
|---|---|
| `document_outline(id)` | format, pages, title and authors, the contents (index, level, title, page), warnings |
| `read_document(id, pages? \| section? \| blocks? , start?)` | up to ~24k characters with `[Page N]` lines, `truncated` and `next_block` |
| `search_document(id, query, limit?)` | places in one document: block, page, section trail, snippet |
| `search_documents(query, folder?, limit?, offset?)` | passages across the index, by file |

Every result names the file (`name`) and is under `untrusted_text` where it
carries the document's words. A reply cites a place as
`[p. 47](files:<id>#p=47)` or `[the passage](files:<id>#b=812)`.
