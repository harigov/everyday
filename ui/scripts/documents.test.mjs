// Behaviour checks for `documents.ts` -- the pure half of reading -- and for
// the `files:` links `markdown.ts` draws from it.
//
// What is made of rules here, and fails quietly:
//
//   - which files are documents. The list is `DocFormat::of_file` in the
//     core written out a second time, and the two drifting apart is a button
//     that ends in an error or a book nobody can open;
//   - the address of a place, `files:<id>#p=47` and `#b=812`: written by the
//     assistant, drawn as a chip, read back by the reader. A link that came
//     back a page out, or that let something other than digits through to the
//     markup, is invisible until somebody follows it;
//   - the arithmetic on an outline -- which section a block is in, which
//     chapter, which page -- that every jump the reader makes is built on.
//
// No test framework, deliberately: one dependency-free file, run by
// `npm test`, with the TypeScript loaded through Vite so it compiles exactly
// as the application compiles it.

import { load, makeCheck } from './harness.mjs'

const {
  modules: [documents, markdown],
  close,
} = await load(['/src/lib/documents.ts', '/src/lib/markdown.ts'])
const {
  authorLine,
  chapterOfBlock,
  clampPage,
  docFormatOf,
  docFormatOfName,
  documentTitle,
  fileLink,
  isReadable,
  pageLabel,
  pageOfBlock,
  parseFileLink,
  readingProgress,
  sectionOfBlock,
  sectionTrail,
} = documents
const { renderMarkdown } = markdown

const { check, ok, finish } = makeCheck()

// ── which files are documents ───────────────────────────────────────────

const named = (name, mime = 'application/octet-stream') => ({
  name,
  mime,
  status: { state: 'present' },
})

check('a PDF', docFormatOfName('thesis.pdf'), 'pdf')
check('an EPUB, any case', docFormatOfName('The Hound.EPUB'), 'epub')
check(
  'Markdown has two spellings',
  [docFormatOfName('a.md'), docFormatOfName('a.markdown')],
  ['markdown', 'markdown'],
)
check('text has two', [docFormatOfName('a.txt'), docFormatOfName('a.text')], ['text', 'text'])
check('a web page has three', ['a.html', 'a.htm', 'a.xhtml'].map(docFormatOfName), [
  'html',
  'html',
  'html',
])
check('no extension, no format', docFormatOfName('README'), null)
check('Office is left out on purpose', docFormatOfName('letter.docx'), null)
check('a name that is all extension is still one', docFormatOfName('.pdf'), 'pdf')
check('only the last extension counts', docFormatOfName('book.pdf.zip'), null)

check('the name comes first', docFormatOf(named('a.pdf', 'text/plain')), 'pdf')
check('then the type', docFormatOf(named('a', 'application/epub+zip')), 'epub')
check('a type that names a format', docFormatOf(named('notes', 'text/markdown')), 'markdown')
check(
  'a type with a charset is not one the core knows',
  docFormatOf(named('n', 'text/plain; charset=utf-8')),
  null,
)
check(
  'a name the core does not know falls through to the type',
  docFormatOf(named('a.docx', 'application/pdf')),
  'pdf',
)
check(
  'and a Word file that says so is not read',
  docFormatOf(named('a.docx', 'application/msword')),
  null,
)
check('an unknown name and type', docFormatOf(named('a.bin')), null)

ok('present and a document is readable', isReadable(named('a.pdf')))
ok(
  'a trashed one is not',
  !isReadable({ ...named('a.pdf'), status: { state: 'trashed', at: 'x', from: '' } }),
)
ok(
  'nor a missing one',
  !isReadable({ ...named('a.pdf'), status: { state: 'missing', since: 'x' } }),
)
ok('nor a spreadsheet', !isReadable(named('budget.csv', 'text/csv')))

// ── links to a place ────────────────────────────────────────────────────

const ID = '0f3a9c1e-7b24-4d5e-8a61-2c9b5d7e4f10'

check('a file', fileLink(ID), `files:${ID}`)
check('a page', fileLink(ID, { page: 47 }), `files:${ID}#p=47`)
check('a block', fileLink(ID, { block: 812 }), `files:${ID}#b=812`)
check('block zero is a block', fileLink(ID, { block: 0 }), `files:${ID}#b=0`)
check('both, when both are known', fileLink(ID, { page: 47, block: 812 }), `files:${ID}#p=47&b=812`)
check('a page that is not one is left out', fileLink(ID, { page: 0 }), `files:${ID}`)
check('nor a fraction', fileLink(ID, { block: 1.5 }), `files:${ID}`)

check('reads a file', parseFileLink(`files:${ID}`), { file: ID })
check('reads a page', parseFileLink(`files:${ID}#p=47`), { file: ID, page: 47 })
check('reads a block', parseFileLink(`files:${ID}#b=812`), { file: ID, block: 812 })
check('reads both', parseFileLink(`files:${ID}#p=47&b=812`), { file: ID, page: 47, block: 812 })
check('and both the other way round', parseFileLink(`files:${ID}#b=812&p=47`), {
  file: ID,
  block: 812,
  page: 47,
})
check(
  'the id may lose its hyphens, and its case',
  parseFileLink(`files:${ID.replaceAll('-', '').toUpperCase()}`),
  { file: ID },
)
check('page 0 names nothing', parseFileLink(`files:${ID}#p=0`), { file: ID })
check('a link made by `fileLink` reads back', parseFileLink(fileLink(ID, { page: 3, block: 9 })), {
  file: ID,
  page: 3,
  block: 9,
})
check('not a files link', parseFileLink('https://example.com'), null)
check('not a UUID', parseFileLink('files:123'), null)
check('a place that is not digits', parseFileLink(`files:${ID}#p=4x`), null)
check('a place that is markup', parseFileLink(`files:${ID}#p="><script>`), null)
check('an unknown key', parseFileLink(`files:${ID}#x=4`), null)
check('too many digits to be a place', parseFileLink(`files:${ID}#p=12345678901`), null)
check('text after it', parseFileLink(`files:${ID}/more`), null)

// ── the chip the markdown draws ─────────────────────────────────────────

const chip = (source) => renderMarkdown(source)
check(
  'a link to a file is a chip',
  chip(`See [the book](files:${ID}).`),
  `<p>See <button type="button" class="file-chip" data-file="${ID}">the book</button>.</p>`,
)
check(
  'a page rides on it',
  chip(`[p. 47](files:${ID}#p=47)`),
  `<p><button type="button" class="file-chip" data-file="${ID}" data-page="47">p. 47</button></p>`,
)
check(
  'and so does a block',
  chip(`[the passage](files:${ID}#b=812)`),
  `<p><button type="button" class="file-chip" data-file="${ID}" data-block="812">the passage</button></p>`,
)
check(
  'and both',
  chip(`[there](files:${ID}#p=47&b=812)`),
  `<p><button type="button" class="file-chip" data-file="${ID}" data-page="47" data-block="812">there</button></p>`,
)
ok(
  'a place that is not digits is not a chip',
  !chip(`[x](files:${ID}#p=4"onclick=alert(1))`).includes('file-chip'),
)
ok(
  'the id is written the way the catalog writes it',
  chip(`[x](files:${ID.replaceAll('-', '').toUpperCase()}#p=2)`).includes(`data-file="${ID}"`),
)

// ── the outline ─────────────────────────────────────────────────────────
//
// Part One holds Chapters I and II (the second with a section of its own);
// Part Two holds Chapter III. A tail of blocks past the last section.

const outline = {
  sections: [
    { title: 'Part One', level: 1, page: 1, block: 0 },
    { title: 'Chapter I', level: 2, page: 1, block: 2, parent: 0 },
    { title: 'Chapter II', level: 2, page: 3, block: 10, parent: 0 },
    { title: 'The manuscript', level: 3, page: 4, block: 14, parent: 2 },
    { title: 'Part Two', level: 1, page: 6, block: 30 },
    { title: 'Chapter III', level: 2, page: 6, block: 31, parent: 4 },
  ],
  chapters: [
    { firstBlock: 0, blocks: 10 },
    { firstBlock: 10, blocks: 20 },
    { firstBlock: 30, blocks: 10 },
  ],
}

check(
  'a section trail is outermost first',
  sectionTrail(outline, 3).map((s) => s.title),
  ['Part One', 'Chapter II', 'The manuscript'],
)
check(
  'a top-level section is its own trail',
  sectionTrail(outline, 4).map((s) => s.title),
  ['Part Two'],
)
check('an index that is not one has no trail', sectionTrail(outline, 99), [])
check('a negative one neither', sectionTrail(outline, -1), [])
const loop = { sections: [{ title: 'a', level: 1, page: 1, block: 0, parent: 0 }] }
ok('a section that is its own parent cannot hang it', sectionTrail(loop, 0).length > 0)

check('the block a section starts at is in it', sectionOfBlock(outline, 14), 3)
check('a block inside is under the last section started', sectionOfBlock(outline, 20), 3)
const late = { sections: [{ title: 'a', level: 1, page: 1, block: 5 }] }
check('a block before the first section is under none', sectionOfBlock(late, 2), null)
check('and a document with none', sectionOfBlock({ sections: [] }, 7), null)
check('the last block of the book', sectionOfBlock(outline, 39), 5)

check('chapter of the first block', chapterOfBlock(outline, 0), 0)
check('chapter of a boundary', chapterOfBlock(outline, 10), 1)
check('chapter of the last', chapterOfBlock(outline, 39), 2)
check('a block past the end is in none', chapterOfBlock(outline, 40), null)
check('a PDF has no chapters', chapterOfBlock({ chapters: [] }, 3), null)

const loaded = {
  blocks: [
    { index: 10, page: 3 },
    { index: 11, page: 3 },
    { index: 12, page: 4 },
  ],
}
check('a loaded chapter knows the exact page', pageOfBlock(outline, 12, loaded), 4)
check('else the page of the section it is under', pageOfBlock(outline, 20, null), 4)
check('else nothing, for the caller to keep its own', pageOfBlock({ sections: [] }, 20, null), null)
check(
  'a block not in the chapter loaded falls back to its section',
  pageOfBlock(outline, 33, loaded),
  6,
)

check(
  'a page is kept inside the document',
  [clampPage(0, 10), clampPage(11, 10), clampPage(4.9, 10)],
  [1, 10, 4],
)
check('with no page count, only the floor', clampPage(500, 0), 500)
check('a page that is not a number is the first', clampPage(Number.NaN, 10), 1)

// ── words for a place ───────────────────────────────────────────────────

check('one page', pageLabel(47, 312), 'Page 47 of 312')
check('two in view', pageLabel(47, 312, 48), 'Pages 47–48 of 312')
check('the last in view is not before the first', pageLabel(47, 312, 40), 'Page 47 of 312')
check('with no count', pageLabel(47), 'Page 47')
check(
  'how far through',
  [readingProgress(1, 11), readingProgress(6, 11), readingProgress(11, 11)],
  [0, 0.5, 1],
)
check('a one-page document is all there', readingProgress(1, 1), 1)
check('one of none', readingProgress(1, 0), 0)

check(
  'a title of its own',
  documentTitle({ name: 'x.epub' }, { title: ' The Hound ' }),
  'The Hound',
)
check(
  'else the file’s name',
  documentTitle({ name: 'Q3 report.final.pdf' }, { title: '' }),
  'Q3 report.final',
)
check('with no outline at all', documentTitle({ name: 'notes.md' }, null), 'notes')
check('a name that is all extension', documentTitle({ name: '.md' }, null), '.md')

check('no authors', authorLine({ authors: [] }), '')
check('none given', authorLine(null), '')
check('one', authorLine({ authors: ['Arthur Conan Doyle'] }), 'Arthur Conan Doyle')
check('two', authorLine({ authors: ['A', 'B'] }), 'A and B')
check('three', authorLine({ authors: ['A', 'B', 'C'] }), 'A, B and C')
check('more', authorLine({ authors: ['A', 'B', 'C', 'D', 'E'] }), 'A, B and 3 others')
check('blanks are not authors', authorLine({ authors: [' ', 'A'] }), 'A')

finish('documents')
await close()
