// The mock backend's documents: what the service would read out of the files
// in `mock-files.ts`, in a browser with no service behind it. `mock.ts`
// routes every documents command here (`mockDocumentsInvoke`) --
// `docs/plans/reading.md` has the list.
//
// What can be read, so a screen can be tried against every shape the reader
// draws:
//
//   - an EPUB, *The Hound of the Baskervilles*: a title page, four chapters
//     and a page of notes, each its own spine item (so the reader loads them
//     one at a time); two levels of headings; a quotation, a list, a picture,
//     a footnote-style link and a link to another chapter (`<a data-block>`);
//     and a good many mentions of Dr. Mortimer, for the search to find;
//   - Markdown notes: the three the files mock keeps text for, read the way
//     the service reads Markdown -- one chapter, sections from the headings,
//     a table as a block of preformatted text;
//   - a PDF, which the mock draws as one page -- the sample `mock-files.ts`
//     builds -- so it has one page here too, and the two lines on it as its
//     blocks. Nothing is drawn from them: a PDF's pages are pdf.js's.
//
// Anything else the files mock holds -- a spreadsheet, a picture -- is not a
// document, and is refused as the service refuses it.
//
// The search is a small stand-in for the service's BM25: every word of the
// query counts, a word matches the front of a word, and a block holding all of
// them is ranked above any that holds some. Enough that "mortimer" finds
// where Mortimer is, and "mortimer manuscript" finds the one block with both.
//
// Where somebody left off (`save_reader_position`) is kept in memory, so a
// document opens where it was closed for as long as the page is open.
//
// Nothing runs at module load but defining functions: the documents are built
// on first use and there are no timers, so loading this file cannot hold a
// test process open.

import type {
  DocBlockKind,
  DocBlockView,
  DocChapter,
  DocChapterView,
  DocFormat,
  DocHit,
  DocIndexStatus,
  DocMeta,
  DocOutline,
  DocSearchHit,
  DocSearchPage,
  DocSection,
  FileEntry,
  FilesStatus,
  ReaderPlace,
} from './types'
import { VaultError } from './types'
import { mockFileUrl, mockFilesInvoke } from './mock-files'

// ── The commands this file answers ───────────────────────────────────────

const COMMANDS = new Set([
  'document_outline',
  'document_chapter',
  'search_document',
  'search_documents',
  'save_reader_position',
  'documents_status',
  'reindex_documents',
])

/** Is `cmd` one of the documents commands this file answers? `create_item_from_file`
 *  is not: it makes an item, which is `mock.ts`'s, and asks `mockDocumentFields`. */
export function isMockDocumentsCommand(cmd: string): boolean {
  return COMMANDS.has(cmd)
}

// ── What a document is ───────────────────────────────────────────────────

/** About a printed paperback page, as the core makes up pages: see
 *  `SYNTHETIC_PAGE_CHARS`. */
const PAGE_CHARS = 1_800

/** A block as it is written down here. */
interface Source {
  kind: DocBlockKind
  level?: number
  /** The words, in a little markup: `*emphasis*`, `{#key|label}` a link to
   *  the block called `key`, `{^key|label}` the same raised, as a note's. */
  raw: string
  /** What other blocks call it. */
  key?: string
  /** A picture, as a `data:` address. */
  src?: string
}

/** One block, laid out. */
interface Block {
  kind: DocBlockKind
  level: number
  page: number
  text: string
  html: string
  src: string
}

/** A document, read. */
interface Parsed {
  format: DocFormat
  pages: number
  meta: DocMeta
  sections: DocSection[]
  chapters: DocChapter[]
  blocks: Block[]
  warnings: string[]
}

const NO_META: DocMeta = {
  title: '',
  authors: [],
  language: '',
  publisher: '',
  published: '',
  identifiers: [],
  description: '',
  subjects: [],
  cover: '',
}

function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;')
}

/** The words of `raw`, with the markup taken out. A note's marker is no part
 *  of what the sentence says, so it goes; a link's label stays. */
function plain(raw: string): string {
  return raw
    .replace(/\{\^[^|}]*\|[^}]*\}/g, '')
    .replace(/\{#[^|}]*\|([^}]*)\}/g, '$1')
    .replace(/\*([^*]+)\*/g, '$1')
}

/** `raw` as the reader draws it. `at` says where each named block landed. */
function toHtml(raw: string, at: Map<string, number>): string {
  // The label is already escaped: it is cut out of text that was.
  const link = (key: string, label: string): string => {
    const block = at.get(key)
    return block === undefined ? label : `<a data-block="${block}">${label}</a>`
  }
  return escapeHtml(raw)
    .replace(/\{\^([^|}]*)\|([^}]*)\}/g, (_all, key: string, label: string) => {
      return `<sup>${link(key, label)}</sup>`
    })
    .replace(/\{#([^|}]*)\|([^}]*)\}/g, (_all, key: string, label: string) => link(key, label))
    .replace(/\*([^*]+)\*/g, '<em>$1</em>')
}

/** Lay `chapters` out: pages, sections, and the links between blocks. */
function lay(format: DocFormat, meta: DocMeta, chapters: Source[][], hrefs: string[]): Parsed {
  const flat = chapters.flat()
  const at = new Map<string, number>()
  flat.forEach((s, i) => {
    if (s.key) at.set(s.key, i)
  })

  const blocks: Block[] = flat.map((s) => {
    const text = s.kind === 'rule' ? '' : s.kind === 'code' ? s.raw : plain(s.raw)
    const html = s.kind === 'code' || s.kind === 'image' ? escapeHtml(text) : toHtml(s.raw, at)
    return { kind: s.kind, level: s.level ?? 0, page: 1, text, html, src: s.src ?? '' }
  })

  // Pages: about `PAGE_CHARS` of text each, never splitting a block, and a
  // heading of the first two levels starts one of its own.
  let page = 1
  let used = 0
  for (const b of blocks) {
    const starts = b.kind === 'heading' && b.level <= 2 && used > 0
    if (starts || (used > 0 && used + b.text.length > PAGE_CHARS)) {
      page += 1
      used = 0
    }
    b.page = page
    used += b.text.length
  }

  // Sections: one per heading, at its level, inside the nearest one above it.
  const sections: DocSection[] = []
  const open: number[] = []
  blocks.forEach((b, index) => {
    if (b.kind !== 'heading') return
    while (open.length > 0 && sections[open.at(-1)!]!.level >= b.level) open.pop()
    const section: DocSection = { title: b.text, level: b.level, page: b.page, block: index }
    const parent = open.at(-1)
    if (parent !== undefined) section.parent = parent
    open.push(sections.length)
    sections.push(section)
  })

  let first = 0
  const spine: DocChapter[] = chapters.map((c, i) => {
    const chapter: DocChapter = { firstBlock: first, blocks: c.length }
    if (hrefs[i]) chapter.href = hrefs[i]
    first += c.length
    return chapter
  })

  return {
    format,
    pages: blocks.length > 0 ? page : 1,
    meta,
    sections,
    chapters: spine,
    blocks,
    warnings: [],
  }
}

// ── The book ─────────────────────────────────────────────────────────────

const h = (level: number, raw: string, key?: string): Source => ({
  kind: 'heading',
  level,
  raw,
  ...(key ? { key } : {}),
})
const p = (raw: string, key?: string): Source => ({
  kind: 'paragraph',
  raw,
  ...(key ? { key } : {}),
})
const q = (raw: string): Source => ({ kind: 'quote', raw })
const li = (raw: string, key?: string): Source => ({
  kind: 'listItem',
  raw,
  ...(key ? { key } : {}),
})
const rule: Source = { kind: 'rule', raw: '' }
const picture = (description: string, src: string): Source => ({
  kind: 'image',
  raw: description,
  src,
})

/** A sketch of the moor, drawn here: a picture the reader has to draw from a
 *  `data:` address, as it does one pulled out of an EPUB. */
function moorMap(): string {
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="640" height="400" viewBox="0 0 640 400">` +
    `<rect width="640" height="400" fill="#efe9d6"/>` +
    `<path d="M0 300 C120 260 200 330 320 300 S520 250 640 290 V400 H0Z" fill="#d8cfae"/>` +
    `<path d="M300 120 C350 90 420 110 450 160 S430 250 360 255 S270 200 300 120Z" ` +
    `fill="#6f8a5a" opacity=".8"/>` +
    `<path d="M40 60 C160 90 220 40 330 70 S520 60 610 30" fill="none" stroke="#8a6d3b" ` +
    `stroke-width="3" stroke-dasharray="8 6"/>` +
    `<rect x="70" y="110" width="26" height="26" fill="#5a4a3a"/>` +
    `<circle cx="520" cy="200" r="11" fill="#7a3b2e"/>` +
    `<g font-family="serif" font-size="17" fill="#3a2f22">` +
    `<text x="52" y="156">Baskerville Hall</text>` +
    `<text x="305" y="182" font-style="italic">Grimpen Mire</text>` +
    `<text x="468" y="232">Merripit House</text>` +
    `<text x="40" y="380" font-size="14">The moor, as Dr. Mortimer drew it for us</text>` +
    `</g></svg>`
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`
}

function hound(): Parsed {
  const meta: DocMeta = {
    title: 'The Hound of the Baskervilles',
    authors: ['Arthur Conan Doyle'],
    language: 'en',
    publisher: 'George Newnes',
    published: '1902',
    identifiers: ['gutenberg:2852'],
    description:
      'Sherlock Holmes and Dr. Watson are asked to look into the death of Sir Charles ' +
      'Baskerville, and into the legend of a spectral hound that is said to haunt the family ' +
      'on the Devonshire moor.',
    subjects: ['Detective and mystery stories', 'Devon (England) -- Fiction'],
    cover: 'OEBPS/images/cover.jpg',
  }
  const chapters: Source[][] = [
    [
      h(1, 'The Hound of the Baskervilles'),
      p('Another Adventure of Sherlock Holmes'),
      p('Arthur Conan Doyle'),
      q('To Fletcher Robinson, who told me of the legend, and of the moor it grew on.'),
      rule,
    ],
    [
      h(1, 'Chapter I. Mr. Sherlock Holmes', 'ch1'),
      p(
        'Mr. Sherlock Holmes, who was usually very late in the mornings, save upon those not ' +
          'infrequent occasions when he was up all night, was seated at the breakfast table. I ' +
          'stood upon the hearth-rug and picked up the stick which our visitor had left behind ' +
          'him the night before.',
      ),
      p(
        'It was a fine, thick piece of wood, bulbous-headed, of the sort which is known as a ' +
          '“Penang lawyer.”{^n1|1} Just under the head was a broad silver band nearly an ' +
          'inch across. “To James Mortimer, M.R.C.S., from his friends of the C.C.H.,” ' +
          'was engraved upon it, with the date “1884.” It was just such a stick as the ' +
          'old-fashioned family practitioner used to carry—dignified, solid, and reassuring.',
      ),
      p(
        '“Well, Watson, what do you make of it?” Holmes was sitting with his back to ' +
          'me, and I had given him no sign of my occupation. “How did you know what I was ' +
          'doing? I believe you have eyes in the back of your head.”',
      ),
      p(
        '“I have, at least, a well-polished, silver-plated coffee-pot in front of me,” ' +
          'said he. “But, tell me, Watson, what do you make of our visitor’s stick? Since ' +
          'we have been so unfortunate as to miss him and have no notion of his errand, this ' +
          'accidental souvenir becomes of importance.”',
      ),
      p(
        'I reasoned it out as well as I could. Dr. Mortimer was a country practitioner, and ' +
          'a man much walked about; the stick, worn at the ferrule, said so. He was not young, ' +
          'since his friends had thought him worth a present; and he was liked.',
      ),
      p(
        'Holmes was pleased, as he always was when he was not contradicted, and said that for ' +
          'the most part I was right. We were still arguing about the dog—there were ' +
          'tooth-marks on the stick, and a small spaniel had left them—when the visitor ' +
          'himself came up the stairs.',
      ),
    ],
    [
      h(1, 'Chapter II. The Curse of the Baskervilles', 'ch2'),
      p(
        '“I have in my pocket a manuscript,” said Dr. James Mortimer. “I observed ' +
          'it as you entered the room,” said Holmes. “It is an old manuscript.” ' +
          '“Early eighteenth century, unless it is a forgery.”',
      ),
      h(2, 'The manuscript', 'manuscript'),
      q(
        'Of the origin of the Hound of the Baskervilles there have been many statements, yet ' +
          'as I come in a direct line from Hugo Baskerville, and as I had the story from my ' +
          'father, who also had it from his, I have set it down with all belief that it ' +
          'occurred even as is here set forth.',
      ),
      p(
        'Hugo, the story goes, was a wild and godless man. He carried off a farmer’s ' +
          'daughter to the Hall, and when she escaped across the moor he rode after her with ' +
          'his drinking friends and a pack of hounds. In the small hours his friends found ' +
          'the girl dead of fear, and Hugo dead beside her, and over him a thing that was ' +
          'not a dog.',
      ),
      p(
        'Dr. Mortimer folded the manuscript. “Since then,” said he, “the family ' +
          'has had its sorrows, and each of them has been put down to the same cause. I tell you ' +
          'this because Sir Charles was a man who believed it.”',
      ),
      h(2, 'What Holmes asks', 'asks'),
      p('Holmes asked three things before he would say anything of his own:'),
      li('Who found Sir Charles, and where, and at what hour?'),
      li('Whether anything had been seen on the moor that night but the body.'),
      li('Who stood to gain by the death of the last Baskerville in England.'),
      p(
        'Dr. Mortimer answered the first two. The third, he said, was in {#ch3|Chapter III}, ' +
          'since it was not a thing that could be said in a sentence.',
      ),
    ],
    [
      h(1, 'Chapter III. The Problem', 'ch3'),
      p(
        'Dr. Mortimer told us how Sir Charles had been found at the end of the yew alley, ' +
          'his face so contorted that his own doctor had hardly known him. There was no ' +
          'mark of violence. There was, however, something on the gravel beside him, and ' +
          'he had been a long time standing at the gate before he fell.',
      ),
      picture(
        'A rough sketch of the moor: Baskerville Hall, Grimpen Mire and Merripit House.',
        moorMap(),
      ),
      p(
        'The moor itself, Dr. Mortimer said, was the whole difficulty. A man might cross it ' +
          'in an hour in daylight and be lost on it in ten minutes after dark. The Grimpen ' +
          'Mire, in the middle, had taken a pony every year within living memory.',
      ),
      p(
        '“The footmarks, Mr. Holmes,” said Dr. Mortimer, lowering his voice. ' +
          '“They were the footmarks of a gigantic hound.”',
      ),
      p(
        'Holmes leaned back and put the tips of his fingers together. He would, he said, ' +
          'think it over, and send for us in the morning; for the present he wished to hear ' +
          'nothing more about it, and he was to be left in as much smoke as the room would hold.',
      ),
    ],
    [
      h(1, 'Chapter IV. Sir Henry Baskerville', 'ch4'),
      p(
        'The heir arrived from Canada the next week, with one boot of a new pair gone ' +
          'missing from the hotel. Sir Henry took the thing as a joke; Holmes did not, and ' +
          'neither, I think, did Dr. Mortimer.',
      ),
      p(
        'It was agreed that Sir Henry should go down to the Hall at once, that Dr. Mortimer ' +
          'should go with him, and that I should follow and keep my eyes open. I was to write ' +
          'what I saw, and to leave the conclusions to Holmes.',
      ),
    ],
    [
      h(1, 'Notes', 'notes'),
      li(
        '1. A “Penang lawyer” is a walking-stick made from the stem of a palm from ' +
          'Penang, an island off the Malay coast, and is not a lawyer.',
        'n1',
      ),
      li('2. C.C.H.: the Charing Cross Hospital, where Dr. Mortimer had once worked.'),
    ],
  ]
  const hrefs = ['title.xhtml', 'ch1.xhtml', 'ch2.xhtml', 'ch3.xhtml', 'ch4.xhtml', 'notes.xhtml']
  return lay(
    'epub',
    meta,
    chapters,
    hrefs.map((href) => `OEBPS/${href}`),
  )
}

// ── Markdown, text, PDF ──────────────────────────────────────────────────

/** Markdown read the way the service reads it: one chapter, a block for each
 *  heading, paragraph, quotation and list item, and a table as one block of
 *  preformatted text. */
function markdown(text: string): Parsed {
  const sources: Source[] = []
  const lines = text.split('\n')
  let paragraph: string[] = []
  let table: string[] = []
  const flush = () => {
    if (paragraph.length > 0) sources.push(p(paragraph.join(' ')))
    if (table.length > 0) sources.push({ kind: 'code', raw: table.join('\n') })
    paragraph = []
    table = []
  }
  for (const line of lines) {
    const heading = /^(#{1,6})\s+(.*)$/.exec(line)
    const item = /^\s*(?:[-*]|\d+\.)\s+(.*)$/.exec(line)
    if (line.trim() === '') flush()
    else if (heading) {
      flush()
      sources.push(h(heading[1]!.length, heading[2]!))
    } else if (item) {
      flush()
      sources.push(li(item[1]!))
    } else if (line.startsWith('>')) {
      flush()
      sources.push(q(line.replace(/^>\s?/, '')))
    } else if (/^---+$/.test(line.trim())) {
      flush()
      sources.push(rule)
    } else if (line.startsWith('|')) {
      if (paragraph.length > 0) flush()
      table.push(line)
    } else {
      if (table.length > 0) flush()
      paragraph.push(line.trim())
    }
  }
  flush()
  const title = sources.find((s) => s.kind === 'heading')
  const meta: DocMeta = { ...NO_META, title: title ? plain(title.raw) : '' }
  return lay('markdown', meta, [sources], [''])
}

/** What the service makes of the sample PDF `mock-files.ts` draws: one page,
 *  two lines. A PDF's own pages are pdf.js's, so the blocks are only for the
 *  search and the assistant. */
function pdf(file: FileEntry): Parsed {
  const title = file.name.replace(/\.[^.]*$/, '')
  const parsed = lay(
    'pdf',
    { ...NO_META, title },
    [[p(title), p('A sample page from the mock files folder.')]],
    [''],
  )
  parsed.chapters = []
  for (const b of parsed.blocks) b.html = ''
  return parsed
}

// ── Finding and keeping them ─────────────────────────────────────────────

/** `DocFormat::of_file`, as the service has it. */
function formatOf(file: FileEntry): DocFormat | null {
  const ext = file.name.includes('.') ? file.name.slice(file.name.lastIndexOf('.') + 1) : ''
  const byName: Record<string, DocFormat> = {
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
  const named = byName[ext.toLowerCase()]
  if (named) return named
  const byType: Record<string, DocFormat> = {
    'application/pdf': 'pdf',
    'application/epub+zip': 'epub',
    'text/markdown': 'markdown',
    'text/plain': 'text',
    'text/html': 'html',
    'application/xhtml+xml': 'html',
  }
  return byType[file.mime] ?? null
}

/** The text the files mock keeps for a file, which it draws from a `data:`
 *  address; `null` for a file it has none for. */
function textOf(file: FileEntry): string | null {
  const url = mockFileUrl(file.id)
  const m = /^data:text\/[^;,]+;charset=utf-8,([^]*)$/.exec(url)
  return m ? decodeURIComponent(m[1]!) : null
}

const parsed = new Map<string, Parsed>()
const positions = new Map<string, ReaderPlace>()

/** Forget where everybody left off and every document read. For tests. */
export function mockDocumentsReset(): void {
  parsed.clear()
  positions.clear()
}

function fileOf(id: string): FileEntry {
  const detail = mockFilesInvoke('get_file', { id }) as { file: FileEntry }
  return detail.file
}

/** A document, read -- once: a file's contents never change in the mock. */
function documentOf(file: FileEntry): Parsed {
  const format = formatOf(file)
  if (format === null) {
    throw new VaultError('invalid', `${file.name} is not a kind of file that can be read here.`)
  }
  if (file.status.state !== 'present') {
    throw new VaultError('not_found', `${file.name} is not in the files folder.`)
  }
  const have = parsed.get(file.id)
  if (have) return have
  let made: Parsed
  if (format === 'epub' && file.name.startsWith('The Hound of the Baskervilles')) made = hound()
  else if (format === 'pdf') made = pdf(file)
  else if (format === 'epub') made = lay('epub', NO_META, [[]], [''])
  else made = { ...markdown(textOf(file) ?? ''), format }
  if (made.blocks.length === 0 && format !== 'pdf') {
    made.warnings = ['The mock has no text for this file.']
  }
  parsed.set(file.id, made)
  return made
}

/** Whether the files mock's settings have the index on. */
function indexOn(): boolean {
  return (mockFilesInvoke('files_status', {}) as FilesStatus).settings.indexDocuments
}

/** Is it in the search index? Sealed files are read, never indexed. */
function indexed(file: FileEntry): boolean {
  return indexOn() && file.storage.kind === 'plain' && file.status.state === 'present'
}

/** Every file the mock can read and index. */
function indexable(): FileEntry[] {
  const page = mockFilesInvoke('list_files', {
    query: { status: 'present', kinds: ['document'], includeHidden: true },
  }) as { files: FileEntry[] }
  return page.files.filter((f) => formatOf(f) !== null && indexed(f))
}

// ── Searching ────────────────────────────────────────────────────────────

/** Lower-case words, accents off. */
function wordsOf(text: string): string[] {
  return text
    .normalize('NFD')
    .replace(/[̀-ͯ]/g, '')
    .toLowerCase()
    .split(/[^a-z0-9]+/)
    .filter(Boolean)
}

/** The words around the first match, cut from the block, on one line. */
function snippet(text: string, words: string[]): string {
  const flat = text.replace(/\s+/g, ' ').trim()
  const lower = flat.toLowerCase()
  let at = -1
  for (const w of words) {
    const i = lower.indexOf(w)
    if (i >= 0 && (at < 0 || i < at)) at = i
  }
  const start = Math.max(0, at - 60)
  const end = Math.min(flat.length, start + 180)
  return `${start > 0 ? '…' : ''}${flat.slice(start, end)}${end < flat.length ? '…' : ''}`
}

/** Every block of `doc` that holds any word of `query`, best first. */
function search(doc: Parsed, query: string): DocHit[] {
  const wanted = [...new Set(wordsOf(query))]
  if (wanted.length === 0) return []
  const tokens = doc.blocks.map((b) => wordsOf(b.text))
  const average = tokens.reduce((n, t) => n + t.length, 0) / Math.max(1, tokens.length)
  // Blocks holding each word, for how rare it is.
  const holding = wanted.map((w) => tokens.filter((t) => t.some((x) => x.startsWith(w))).length)
  const hits: DocHit[] = []
  doc.blocks.forEach((block, index) => {
    const mine = tokens[index]!
    let score = 0
    let matched = 0
    wanted.forEach((w, i) => {
      const tf = mine.filter((x) => x.startsWith(w)).length
      if (tf === 0) return
      matched += 1
      const idf = Math.log(1 + (tokens.length - holding[i]! + 0.5) / (holding[i]! + 0.5))
      score += (idf * (tf * 2.2)) / (tf + 1.2 * (0.25 + (0.75 * mine.length) / (average || 1)))
    })
    if (matched === 0) return
    // A block with every word is above any with some.
    if (matched === wanted.length) score += 100
    const section = sectionIndexOf(doc, index)
    const hit: DocHit = {
      block: index,
      page: block.page,
      snippet: snippet(block.text, wanted),
      score: Math.round(score * 1000) / 1000,
    }
    if (section !== null) hit.section = section
    hits.push(hit)
  })
  return hits.sort((a, b) => b.score - a.score || a.block - b.block)
}

/** The innermost section a block is in. */
function sectionIndexOf(doc: Parsed, block: number): number | null {
  let found: number | null = null
  for (const [i, s] of doc.sections.entries()) if (s.block <= block) found = i
  return found
}

// ── Answering ────────────────────────────────────────────────────────────

function outline(file: FileEntry): DocOutline {
  const doc = documentOf(file)
  const out: DocOutline = {
    file: file.id,
    format: doc.format,
    pages: doc.pages,
    meta: doc.meta,
    sections: doc.sections,
    chapters: doc.chapters,
    blocks: doc.blocks.length,
    words: doc.blocks.reduce((n, b) => n + b.text.split(/\s+/).filter(Boolean).length, 0),
    warnings: doc.warnings,
    indexed: indexed(file),
  }
  const place = positions.get(file.id)
  if (place) out.position = place
  return out
}

function chapter(file: FileEntry, index: number): DocChapterView {
  const doc = documentOf(file)
  const c = doc.chapters[index]
  if (!c || !Number.isInteger(index)) {
    throw new VaultError('not_found', `${file.name} has no chapter ${index}.`)
  }
  const blocks: DocBlockView[] = doc.blocks
    .slice(c.firstBlock, c.firstBlock + c.blocks)
    .map((b, i) => {
      const view: DocBlockView = {
        index: c.firstBlock + i,
        kind: b.kind,
        page: b.page,
        html: b.html,
      }
      if (b.level > 0) view.level = b.level
      if (b.src) view.src = b.src
      return view
    })
  return { file: file.id, chapter: index, firstBlock: c.firstBlock, blocks }
}

function status(): DocIndexStatus {
  const enabled = indexOn()
  return {
    enabled,
    indexed: enabled ? indexable().length : 0,
    pending: 0,
    failed: [],
    running: false,
    lastPass: '2026-11-20T07:30:00Z',
  }
}

function searchAll(query: string, limit: number, offset: number): DocSearchPage {
  if (!indexOn()) return { hits: [], total: 0, indexing: false, nextOffset: 0 }
  const all: DocSearchHit[] = []
  for (const file of indexable()) {
    const doc = documentOf(file)
    for (const hit of search(doc, query)) {
      const section = hit.section === undefined || hit.section === null ? null : hit.section
      all.push({
        file: file.id,
        block: hit.block,
        page: hit.page,
        section: section === null ? '' : (doc.sections[section]?.title ?? ''),
        snippet: hit.snippet,
        score: hit.score,
      })
    }
  }
  all.sort((a, b) => b.score - a.score || (a.file < b.file ? -1 : a.file > b.file ? 1 : 0))
  const hits = all.slice(offset, offset + limit)
  return { hits, total: all.length, indexing: false, nextOffset: offset + hits.length }
}

/** What an item made from this file is filled with: the document's own title,
 *  authors, year and blurb -- or its file name when it says nothing. Throws as
 *  the service does for a file that is not a document. */
export function mockDocumentFields(id: string): {
  format: DocFormat
  title: string
  creator: string
  year: number | null
  summary: string
} {
  const file = fileOf(id)
  const doc = documentOf(file)
  const year = /\b(\d{4})\b/.exec(doc.meta.published)?.[1]
  return {
    format: doc.format,
    title: doc.meta.title || file.name.replace(/\.[^.]*$/, ''),
    creator: doc.meta.authors.join(', '),
    year: year ? Number(year) : null,
    summary: doc.meta.description,
  }
}

/** A copy through JSON, which is what the real bridge does to every answer. */
function wire<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

/** Answer a documents command, or `undefined` when `cmd` is not one. */
export function mockDocumentsInvoke(cmd: string, args: Record<string, unknown>): unknown {
  switch (cmd) {
    case 'document_outline':
      return wire(outline(fileOf(str(args.id))))
    case 'document_chapter':
      return wire(chapter(fileOf(str(args.id)), Number(args.chapter)))
    case 'search_document': {
      const file = fileOf(str(args.id))
      const limit = typeof args.limit === 'number' ? args.limit : 20
      return wire(search(documentOf(file), str(args.query)).slice(0, Math.max(0, limit)))
    }
    case 'search_documents': {
      const limit = typeof args.limit === 'number' ? args.limit : 20
      const offset = typeof args.offset === 'number' ? args.offset : 0
      return wire(searchAll(str(args.query), limit, offset))
    }
    case 'save_reader_position': {
      const file = fileOf(str(args.id))
      const place: ReaderPlace = { page: Math.max(1, Number(args.page) || 1) }
      if (typeof args.block === 'number') place.block = args.block
      positions.set(file.id, place)
      return undefined
    }
    case 'documents_status':
    case 'reindex_documents':
      return wire(status())
    default:
      return undefined
  }
}
