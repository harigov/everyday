// What the interface knows about documents without asking the service: which
// files are ones the reader can open, the address of a place in one, and the
// arithmetic on a document's outline -- which section a block is in, which
// chapter, which page. See `docs/plans/reading.md`.
//
// Pure, and imports types only, so the markdown renderer can use it for the
// `files:<id>#p=47` links the assistant writes without pulling a store in.
//
// # What counts as a document
//
// `docFormatOf` is `DocFormat::of_file` in `jatayu-core/src/documents.rs`
// written out again: the file's extension first, then its type. The two must
// agree, because a file this offers to read and the service refuses is a
// button that ends in an error, and one this does not offer and the service
// would read is a document nobody can open. When the Rust list grows, so
// does this one -- `documents.test.mjs` pins the pairs.
//
// # A place in a document
//
// A page, and for anything but a PDF a block: the index of a paragraph, a
// heading or a picture in the document's reading order. Written as a link
// they are `files:<id>#p=47` and `files:<id>#b=812`; the assistant cites
// with them and the reader opens on them.

import type {
  DocChapterView,
  DocFormat,
  DocMeta,
  DocOutline,
  DocSection,
  FileEntry,
  FileId,
} from './types'

/** The extensions the reader opens, as the service reads them. */
export const DOC_EXTENSIONS: Readonly<Record<string, DocFormat>> = {
  pdf: 'pdf',
  epub: 'epub',
  md: 'markdown',
  markdown: 'markdown',
  txt: 'text',
  text: 'text',
  html: 'html',
  htm: 'html',
  xhtml: 'html',
}

/** The types that say a document is one when its name does not. */
const DOC_TYPES: Readonly<Record<string, DocFormat>> = {
  'application/pdf': 'pdf',
  'application/epub+zip': 'epub',
  'text/markdown': 'markdown',
  'text/plain': 'text',
  'text/html': 'html',
  'application/xhtml+xml': 'html',
}

/** What a person calls each format, as `DocFormat::phrase` does. */
export const DOC_FORMAT_PHRASES: Readonly<Record<DocFormat, string>> = {
  pdf: 'a PDF',
  epub: 'an EPUB book',
  markdown: 'a Markdown document',
  text: 'a text file',
  html: 'a web page',
}

/** What a document is called in a line: `PDF`, `EPUB`, `Web page`. */
const FORMAT_NAMES: Readonly<Record<DocFormat, string>> = {
  pdf: 'PDF',
  epub: 'EPUB',
  markdown: 'Markdown',
  text: 'Text',
  html: 'Web page',
}

/** The steps a PDF is zoomed through, shared by `PdfView` and the reader's
 *  bar. Here rather than in `PdfView.svelte`'s module script, which a
 *  component's typed lint cannot see into. */
export const PDF_ZOOMS = [0.5, 0.67, 0.8, 1, 1.25, 1.5, 2, 3] as const

/** The format a name says, by its extension in any case. */
export function docFormatOfName(name: string): DocFormat | null {
  const dot = name.lastIndexOf('.')
  if (dot < 0) return null
  return DOC_EXTENSIONS[name.slice(dot + 1).toLowerCase()] ?? null
}

/** The format of a catalogued file: its name first, then its type. */
export function docFormatOf(file: Pick<FileEntry, 'name' | 'mime'>): DocFormat | null {
  return docFormatOfName(file.name) ?? DOC_TYPES[file.mime] ?? null
}

/**
 * Whether the reader can open this file now: it is a document the service
 * reads, and it is in the folder -- a trashed or missing file has nothing to
 * read from. A sealed file is readable like any other; it is only left out
 * of the search index.
 */
export function isReadable(file: Pick<FileEntry, 'name' | 'mime' | 'status'>): boolean {
  return file.status.state === 'present' && docFormatOf(file) !== null
}

// ── Links to a place ──────────────────────────────────────────────────────

/** A place in a document, as a link names it. */
export interface FilePlace {
  page?: number
  block?: number
}

/** What `parseFileLink` finds in a `files:` link. */
export interface FileLink extends FilePlace {
  /** The file's id, lower-case with hyphens, the way the catalog writes it. */
  file: FileId
}

/** A UUID with or without its hyphens: the only thing a `files:` link names. */
const FILE_ID = '[0-9a-f]{8}-?[0-9a-f]{4}-?[0-9a-f]{4}-?[0-9a-f]{4}-?[0-9a-f]{12}'

/** `files:<id>`, then optionally `#p=47`, `#b=812` or both, joined by `&`.
 *  Digits only, and not many of them: nothing here can be more than that. */
const FILE_LINK = new RegExp(`^files:(${FILE_ID})(?:#([pb]=\\d{1,9}(?:&[pb]=\\d{1,9})?))?$`, 'i')

/** An id written the way the catalog writes ids. */
function hyphenated(id: string): string {
  const h = id.replaceAll('-', '').toLowerCase()
  return [h.slice(0, 8), h.slice(8, 12), h.slice(12, 16), h.slice(16, 20), h.slice(20)].join('-')
}

/**
 * The link for a file, or for a place in it: `files:<id>`, `files:<id>#p=47`
 * or `files:<id>#b=812`. A block is the more exact address and is written
 * when there is one; a page and a block together are both kept, since a
 * block on its own does not say which page of a PDF it is on.
 */
export function fileLink(id: FileId, place: FilePlace = {}): string {
  const parts: string[] = []
  if (place.page !== undefined && Number.isInteger(place.page) && place.page >= 1) {
    parts.push(`p=${place.page}`)
  }
  if (place.block !== undefined && Number.isInteger(place.block) && place.block >= 0) {
    parts.push(`b=${place.block}`)
  }
  return parts.length > 0 ? `files:${id}#${parts.join('&')}` : `files:${id}`
}

/** Read a `files:` link; `null` for anything else, including a link whose
 *  place is not plain digits. */
export function parseFileLink(href: string): FileLink | null {
  const m = FILE_LINK.exec(href.trim())
  if (!m) return null
  const link: FileLink = { file: hyphenated(m[1]!) }
  for (const pair of (m[2] ?? '').split('&')) {
    if (pair === '') continue
    const value = Number(pair.slice(2))
    if (pair[0]!.toLowerCase() === 'p') {
      // Pages count from 1; a citation of page 0 names nothing.
      if (value >= 1) link.page = value
    } else {
      link.block = value
    }
  }
  return link
}

// ── Reading the outline ───────────────────────────────────────────────────

/** The part of an outline these read, so a test or a view need not have one whole. */
type Sections = Pick<DocOutline, 'sections'>
type Chapters = Pick<DocOutline, 'chapters'>

/** The sections a section is inside, outermost first, and the section itself
 *  last. Empty for an index that is not one. */
export function sectionTrail(outline: Sections, sectionIndex: number): DocSection[] {
  const trail: DocSection[] = []
  let at: number | null | undefined = sectionIndex
  // `parent` always points backwards in a well-formed outline; the bound is
  // for one that does not, which must not hang the interface.
  for (let steps = 0; steps <= outline.sections.length; steps++) {
    const section: DocSection | undefined =
      at === null || at === undefined ? undefined : outline.sections[at]
    if (!section) break
    trail.unshift(section)
    at = section.parent
  }
  return trail
}

/**
 * The innermost section a block is in, by index: the last one that starts at
 * or before it. `null` before the first section, and for a document with none.
 * The sections are in block order, which is how the service writes them.
 */
export function sectionOfBlock(outline: Sections, block: number): number | null {
  const { sections } = outline
  let low = 0
  let high = sections.length - 1
  let found = -1
  while (low <= high) {
    const mid = (low + high) >> 1
    if (sections[mid]!.block <= block) {
      found = mid
      low = mid + 1
    } else {
      high = mid - 1
    }
  }
  return found < 0 ? null : found
}

/** The chapter a block is in, by index into `outline.chapters`; `null` when
 *  there is none -- a PDF has no chapters, and nor has a block past the end. */
export function chapterOfBlock(outline: Chapters, block: number): number | null {
  const { chapters } = outline
  let low = 0
  let high = chapters.length - 1
  while (low <= high) {
    const mid = (low + high) >> 1
    const chapter = chapters[mid]!
    if (block < chapter.firstBlock) high = mid - 1
    else if (block >= chapter.firstBlock + chapter.blocks) low = mid + 1
    else return mid
  }
  return null
}

/**
 * The page a block is on, as far as the outline can tell.
 *
 * Exactly, when `chapter` -- the chapter it is in, loaded -- has it; else the
 * page of the section it falls under; else `null`, for the caller to keep
 * the page it has. A section's page is where the section starts, so for a
 * block deep inside a long one this is early rather than wrong, and the view
 * corrects it as it scrolls to the block and reports where it landed.
 */
export function pageOfBlock(
  outline: Sections,
  block: number,
  chapter?: Pick<DocChapterView, 'blocks'> | null,
): number | null {
  const exact = chapter?.blocks.find((b) => b.index === block)
  if (exact) return exact.page
  const section = sectionOfBlock(outline, block)
  return section === null ? null : outline.sections[section]!.page
}

/** `page` kept to what the document has: a whole number from 1 to its last
 *  page, or just from 1 when the page count is not known yet. */
export function clampPage(page: number, pages: number): number {
  if (!Number.isFinite(page)) return 1
  const whole = Math.max(1, Math.trunc(page))
  return pages >= 1 ? Math.min(whole, pages) : whole
}

// ── Words for a place ─────────────────────────────────────────────────────

/** "Page 47 of 312", "Pages 47–48 of 312", or "Page 47" when the count is not
 *  known. `lastPage` is the last page at least partly in view. */
export function pageLabel(page: number, pages = 0, lastPage?: number | null): string {
  const span = lastPage && lastPage > page ? `Pages ${page}–${lastPage}` : `Page ${page}`
  return pages >= 1 ? `${span} of ${pages}` : span
}

/** How far through, from 0 to 1, for a bar: the page at the top of the view
 *  over the document's pages, so the first page is not "0%" nor the last a
 *  hair short of "100%". */
export function readingProgress(page: number, pages: number): number {
  if (pages <= 1) return pages === 1 ? 1 : 0
  return Math.min(1, Math.max(0, (page - 1) / (pages - 1)))
}

/** The file's name without its extension: what a document with no title of
 *  its own is called. */
function stem(name: string): string {
  const dot = name.lastIndexOf('.')
  return dot > 0 ? name.slice(0, dot) : name
}

/** What to call the document: its own title when it has one, else the file's
 *  name without the extension. */
export function documentTitle(
  file: Pick<FileEntry, 'name'>,
  meta?: Pick<DocMeta, 'title'> | null,
): string {
  return meta?.title.trim() || stem(file.name)
}

/** "Arthur Conan Doyle", "Joan Didion and Eve Babitz", "A, B and 3 others";
 *  empty when the document names nobody. */
export function authorLine(meta: Pick<DocMeta, 'authors'> | null | undefined): string {
  const names = (meta?.authors ?? []).map((a) => a.trim()).filter(Boolean)
  if (names.length <= 1) return names[0] ?? ''
  if (names.length === 2) return `${names[0]} and ${names[1]}`
  if (names.length === 3) return `${names[0]}, ${names[1]} and ${names[2]}`
  return `${names[0]}, ${names[1]} and ${names.length - 2} others`
}

/** What a document is called in a line -- `PDF`, `EPUB`, `Web page` -- or
 *  empty for a file the reader cannot open. One wording for the picker and
 *  a library item's list of files. */
export function formatName(file: FileEntry): string {
  const format = docFormatOf(file)
  return format ? FORMAT_NAMES[format] : ''
}

/** Where a file lives, as a line: its folder, or "Encrypted" for one kept
 *  inside the vault, which has no folder. */
export function placeOf(file: Pick<FileEntry, 'storage'>): string {
  if (file.storage.kind === 'sealed') return 'Encrypted'
  const at = file.storage.path.lastIndexOf('/')
  return at < 0 ? '' : file.storage.path.slice(0, at)
}
