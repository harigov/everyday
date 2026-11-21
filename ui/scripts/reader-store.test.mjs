// Behaviour checks for `reader.svelte.ts` -- the store behind the reader -- and
// for the pieces of the interface that hand a document to it: the screen
// context, Files' search inside documents, and the library's files.
//
// Driven through the real mock backend (`mock-documents.ts`), the way the other
// `*-store.test.mjs` files are: a fake `api` would be a second copy of what the
// mock already does, and what is worth checking is the round trip -- an open
// that only the newest wins, a chapter loaded once however often it is asked
// for, a jump that moves where the assistant is told they are, a place that is
// written a few times and not on every scroll.
//
// Written against the mock's seed: *The Hound of the Baskervilles* as an EPUB
// (a title page, four chapters and a page of notes -- six spine items, 36
// blocks, eight sections), the Markdown note `kyoto-itinerary.md` (seven
// blocks, four sections), a PDF the mock draws as one page, and a spreadsheet,
// which is no document. Later sections change what earlier ones read -- the
// place saved, the item made -- so the order below matters.

import { load, makeCheck, stubBrowser } from './harness.mjs'

stubBrowser({
  window: globalThis,
  localStorage: true,
  matchMedia: true,
  document: {
    querySelector: () => null,
    documentElement: {
      style: { setProperty: () => {} },
      classList: { toggle: () => {} },
      setAttribute: () => {},
      removeAttribute: () => {},
    },
    addEventListener: () => {},
  },
  navigator: { userAgent: 'node', language: 'en-GB' },
  location: new URL('http://localhost/?unlocked=1'),
  createObjectURL: () => 'blob:stub',
})

const {
  modules: [
    readerModule,
    stateModule,
    apiModule,
    filesModule,
    libraryModule,
    screenModule,
    onscreen,
    agentModule,
    notifyModule,
  ],
  close,
} = await load(
  [
    '/src/lib/reader.svelte.ts',
    '/src/lib/state.svelte.ts',
    '/src/lib/api.ts',
    '/src/lib/files.svelte.ts',
    '/src/lib/library.svelte.ts',
    '/src/lib/screen.svelte.ts',
    '/src/lib/onscreen.ts',
    '/src/lib/agent.svelte.ts',
    '/src/lib/notify.svelte.ts',
  ],
  { svelte: true },
)
const { reader } = readerModule
const { app } = stateModule
const { api } = apiModule
const { files } = filesModule
const { library } = libraryModule
const { onScreenNow } = screenModule
const { toWire } = onscreen
const { agent } = agentModule
const { notify } = notifyModule

const { check, ok, finish } = makeCheck()

/** Wait for `condition`, or give up after `ms` and let the check fail. */
async function until(condition, ms = 4000) {
  const stop = Date.now() + ms
  while (!condition() && Date.now() < stop) await new Promise((r) => setTimeout(r, 20))
  return condition()
}

/** Count the calls a method of `api` gets, and keep what it answered. */
function spy(name) {
  const real = api[name]
  const calls = []
  api[name] = (...args) => {
    calls.push(args)
    return real(...args)
  }
  return calls
}

await app.start()
check('the mock vault opens unlocked', app.screen, 'main')

const find = async (text) => (await api.listFiles({ text, status: 'all' })).files[0]
const hound = await find('Hound of the Baskervilles')
const note = await find('kyoto-itinerary')
const pdf = await find('dishwasher-manual')
const sheet = await find('budget-2026')
ok(
  'the mock catalog holds the book, a note, a PDF and a spreadsheet',
  hound && note && pdf && sheet,
)
check(
  'the book is an EPUB the catalog calls a document',
  [hound.kind, hound.mime],
  ['document', 'application/epub+zip'],
)

const outlineCalls = spy('documentOutline')
const chapterCalls = spy('documentChapter')
const saves = spy('saveReaderPosition')

// ── before anything ─────────────────────────────────────────────────────

check(
  'nothing is open',
  [reader.open, reader.file, reader.outline, reader.loading, reader.error, reader.at],
  [false, null, null, false, null, null],
)
check('there is nothing to say about it', reader.showing, {})
check(
  'nothing to read, nothing to ask for',
  [await reader.loadChapter(0), reader.section, reader.trail],
  [null, null, []],
)
check('the contents are the pane it opens on', reader.panel, 'outline')

// ── opening ─────────────────────────────────────────────────────────────

const opening = reader.openFile(hound.id)
check('it is open at once, and loading', [reader.open, reader.loading], [true, true])
await opening
check('then loaded', [reader.loading, reader.error], [false, null])
check('the file as the catalog has it', [reader.file.id, reader.file.name], [hound.id, hound.name])
const outline = reader.outline
check(
  'what the service read of it',
  [outline.format, outline.chapters.length, outline.blocks, outline.sections.length],
  ['epub', 6, 36, 8],
)
ok('in made-up pages', outline.pages > 5)
check(
  'its title and author',
  [outline.meta.title, outline.meta.authors],
  ['The Hound of the Baskervilles', ['Arthur Conan Doyle']],
)
check('it starts at the first page', [reader.page, reader.lastPage, reader.block], [1, null, null])
check('and the view is asked to be there', [reader.target.page, reader.target.block], [1, null])
check('the assistant is told where, by file and page', reader.at, { file: hound.id, page: 1 })
check('and which file', reader.showing, {
  open: [{ kind: 'file', id: hound.id, label: hound.name }],
  within: [],
})
check('not opened from an item', reader.item, null)
check('the pane stays what it was', reader.panel, 'outline')

const section = (title) => outline.sections.findIndex((s) => s.title.startsWith(title))
check(
  'the contents have levels',
  outline.sections.map((s) => [s.level, s.parent ?? null]),
  [
    [1, null],
    [1, null],
    [1, null],
    [2, 2],
    [2, 2],
    [1, null],
    [1, null],
    [1, null],
  ],
)

// ── a chapter at a time ─────────────────────────────────────────────────

const before = chapterCalls.length
const [first, again] = await Promise.all([reader.loadChapter(1), reader.loadChapter(1)])
check('two asks for a chapter are one load', chapterCalls.length - before, 1)
ok('and the same chapter', first !== null && first === again)
check(
  'its blocks, by index in the book',
  [first.firstBlock, first.blocks[0].index, first.blocks.length],
  [5, 5, 7],
)
check('a heading', [first.blocks[0].kind, first.blocks[0].level], ['heading', 1])
ok('kept', reader.chapters[1] === first)
await reader.loadChapter(1)
check('asked for again, it is not read again', chapterCalls.length - before, 1)

const notes = outline.sections[section('Notes')]
ok(
  'a footnote is a link to the block it names',
  first.blocks[2].html.includes(`<sup><a data-block="${notes.block + 1}">1</a></sup>`),
)
check(
  'a chapter that is not there is nothing, and is not asked for',
  [
    await reader.loadChapter(99),
    await reader.loadChapter(-1),
    await reader.loadChapter(1.5),
    chapterCalls.length - before,
  ],
  [null, null, null, 1],
)

const third = await reader.loadChapter(3)
const picture = third.blocks.find((b) => b.kind === 'image')
ok('a picture comes as a data address', picture.src.startsWith('data:image/svg+xml'))
ok('and is described', picture.html.includes('sketch'))
check('nothing failed', reader.chapterErrors, {})

// ── jumping ─────────────────────────────────────────────────────────────

let seq = reader.target.seq
const iii = section('Chapter III')
reader.goTo({ section: iii })
check(
  'a section is its own block and page',
  [reader.target.block, reader.target.page],
  [outline.sections[iii].block, outline.sections[iii].page],
)
ok('and the request is new', reader.target.seq > seq)
check(
  'the assistant is told at once, before the view reports',
  [reader.page, reader.block],
  [outline.sections[iii].page, outline.sections[iii].block],
)
check(
  'the section they are in',
  [reader.section, reader.trail.map((s) => s.title)],
  [iii, ['Chapter III. The Problem']],
)

seq = reader.target.seq
reader.goTo({ section: iii })
ok('the same place asked for twice is obeyed twice', reader.target.seq > seq)

reader.goTo({ block: picture.index })
check('a block in a loaded chapter is on the page it says', reader.page, picture.page)
check('and the block is kept', [reader.target.block, reader.block], [picture.index, picture.index])

const manuscript = section('The manuscript')
reader.goTo({ section: manuscript })
check(
  'a section inside another has a trail',
  reader.trail.map((s) => s.title),
  ['Chapter II. The Curse of the Baskervilles', 'The manuscript'],
)

const ivBlock = outline.sections[section('Chapter IV')].block + 1
reader.goTo({ block: ivBlock })
check(
  'a block in a chapter not loaded is on the page of the section it is under',
  reader.page,
  outline.sections[section('Chapter IV')].page,
)

reader.goTo({ block: ivBlock, page: 2 })
check('a page that came with the block is believed', [reader.page, reader.block], [2, ivBlock])

reader.goTo({ page: 3 })
check(
  'a page is a page',
  [reader.page, reader.block, reader.target.page, reader.target.block],
  [3, null, 3, null],
)
reader.goTo({ page: 9999 })
check('and is kept inside the document', reader.page, outline.pages)
reader.goTo({ page: 3 })
reader.goTo({ block: 5000 })
check('a block that is not there is not believed', [reader.page, reader.block], [3, null])
reader.goTo({ section: 99 })
check('nor a section', [reader.page, reader.block], [3, null])
seq = reader.target.seq
reader.goTo({})
check('a request for nowhere is nothing', reader.target.seq, seq)

// ── what the view reports ───────────────────────────────────────────────

reader.reportView({ page: 2, lastPage: 3, block: 6 })
check('the place', [reader.page, reader.lastPage, reader.block], [2, 3, 6])
check('is what the assistant is told', reader.at, {
  file: hound.id,
  page: 2,
  lastPage: 3,
  block: 6,
})
reader.reportView({ page: 2, lastPage: 2, block: 6 })
check(
  'the last page in view is only said when it is another',
  [reader.lastPage, reader.at],
  [null, { file: hound.id, page: 2, block: 6 }],
)
reader.reportView({ page: 0, block: -4 })
check('a place that is not one is kept to the document', [reader.page, reader.block], [1, null])
reader.reportView({ page: 2, block: 6.9 })
check('a block is a whole number', reader.block, 6)

reader.setSelection({ page: 2, from: 6, to: 7 })
check('a selection by blocks', reader.at.selection, { page: 2, from: 6, to: 7 })
reader.setSelection({ page: 2, quote: `  ${Array(500).fill('word').join('   ')}  ` })
check('a quote is at most 400 words, spaced plainly', reader.selection.quote.split(' ').length, 400)
reader.setSelection({ page: 2 })
check('a selection of nothing is none', reader.selection, null)
reader.setSelection({ page: 2, from: 1 })
reader.setSelection(null)
check('and it goes when it goes', [reader.selection, 'selection' in reader.at], [null, false])

// ── their place is saved, but not on every scroll ───────────────────────

saves.length = 0
reader.reportView({ page: 2, block: 6 })
reader.reportView({ page: 3, block: 9 })
reader.reportView({ page: 4, block: 14 })
check('scrolling writes nothing by itself', saves.length, 0)
await reader.flush()
check('the flush writes the last place, once', saves, [[hound.id, 4, 14]])
reader.reportView({ page: 4, block: 14 })
await reader.flush()
check('and a place already written is not written again', saves.length, 1)

reader.reportView({ page: 5, block: 20 })
reader.close()
check('closing writes where they were', saves.at(-1), [hound.id, 5, 20])
check(
  'and the reader is empty',
  [reader.open, reader.file, reader.outline, reader.at],
  [false, null, null, null],
)
check(
  'everything it held is gone',
  [reader.chapters, reader.target, reader.hits, reader.query],
  [{}, null, [], ''],
)
const written = saves.length
reader.close()
check('closing twice writes nothing more', saves.length, written)

await reader.openFile(hound.id)
check('it opens where they left off', [reader.page, reader.block], [5, 20])
check('and asks the view to go there', [reader.target.page, reader.target.block], [5, 20])
ok(
  'the chapter they land in is already on its way',
  await until(() => reader.chapters[2] !== undefined),
)
reader.reportView({ page: 5, block: 22 })
await reader.openFile(note.id, { page: 3 })
check(
  'opened at a place of its own, that place wins',
  [reader.page, reader.outline.format],
  [3, 'markdown'],
)
check('opening another file writes the place in the one left', saves.at(-1), [hound.id, 5, 22])

// ── only the newest open lands ──────────────────────────────────────────

const slow = reader.openFile(hound.id)
const quick = reader.openFile(note.id)
await slow
ok(
  'the answer to the first open is not drawn',
  reader.outline === null || reader.outline.file === note.id,
)
await quick
check(
  'the second is what is open',
  [reader.file.id, reader.outline.format, reader.loading],
  [note.id, 'markdown', false],
)

// ── a note: Markdown, one chapter ───────────────────────────────────────

const calls = outlineCalls.length
await reader.openFile(note.id, { page: 2 })
check(
  'the file already open is a jump, not another open',
  [outlineCalls.length - calls, reader.page],
  [0, 2],
)
check('and the view is asked to go', reader.target.page, 2)
const kyoto = reader.outline
check(
  'its headings are its contents',
  kyoto.sections.map((s) => s.title),
  ['Kyoto, the plan', 'Saturday', 'Sunday', 'Monday'],
)
check('in one chapter', [kyoto.chapters.length, kyoto.blocks], [1, 7])
const day = await reader.loadChapter(0)
ok('whose words are the file’s', day.blocks[2].html.includes('Fushimi Inari'))
check(
  'a section is drawn with its level',
  day.blocks.slice(0, 2).map((b) => [b.kind, b.level]),
  [
    ['heading', 1],
    ['heading', 2],
  ],
)
await reader.search('bamboo')
check(
  'a search finds the block and its page',
  reader.hits.map((h) => [h.block, h.page]),
  [[4, 3]],
)
ok('with the words around it', reader.hits[0].snippet.includes('bamboo grove'))

// ── a PDF: pages are not blocks ─────────────────────────────────────────

await reader.openFile(pdf.id)
check(
  'a PDF',
  [reader.outline.format, reader.outline.chapters.length, reader.outline.pages],
  ['pdf', 0, 1],
)
check('it has no chapters to load', await reader.loadChapter(0), null)
check('and no error for it', reader.chapterErrors, {})
reader.reportView({ page: 1, block: 1 })
check('a PDF has no block in view', [reader.block, reader.at], [null, { file: pdf.id, page: 1 }])
reader.goTo({ block: 1 })
check('so a jump to a block is a jump to a page', [reader.target.block, reader.block], [null, null])

// ── searching inside the open document ──────────────────────────────────

await reader.openFile(hound.id)
await reader.search('mortimer')
ok('the places Mortimer is named', reader.hits.length >= 5)
ok(
  'each with its block and page',
  reader.hits.every((h) => h.block >= 0 && h.page >= 1),
)
ok(
  'and the words around it',
  reader.hits.every((h) => /mortimer/i.test(h.snippet)),
)
check('searching is over', reader.searching, false)
await reader.search('mortimer manuscript')
ok(
  'a block with every word is first',
  /manuscript/i.test(reader.hits[0].snippet) && /mortimer/i.test(reader.hits[0].snippet),
)
await reader.search('zzyzx')
check('nothing found', reader.hits, [])
await reader.search('  ')
check('a blank clears, and keeps what was typed', [reader.hits, reader.query], [[], '  '])

const a = reader.search('mortimer')
const b = reader.search('baskerville')
check('the second search is under way', [reader.searching, reader.query], [true, 'baskerville'])
await Promise.all([a, b])
ok(
  'only the newest answer lands',
  reader.hits.length > 0 && reader.hits.every((h) => /baskerville/i.test(h.snippet)),
)
check('and the search is over', reader.searching, false)

// ── a file that cannot be read ──────────────────────────────────────────

await reader.openFile(sheet.id)
check(
  'a spreadsheet is open, with its name, and the reason it is not read',
  [
    reader.open,
    reader.file.name,
    reader.outline,
    reader.loading,
    /cannot be read|can be read/.test(reader.error ?? ''),
  ],
  [true, sheet.name, null, false, true],
)
check('there is nothing to tell the assistant about it', reader.at, null)
check(
  'and a file that is not there',
  await (async () => {
    await reader.openFile('00000000-0000-4000-8000-000000000000')
    return [reader.file, reader.outline, typeof reader.error]
  })(),
  [null, null, 'string'],
)

// ── the screen context ──────────────────────────────────────────────────

await library.start()
await reader.openFile(hound.id, { item: 'i-dune', page: 2, block: 6 })
check('opened from an item', reader.item, 'i-dune')
check('the item is named, by what the library calls it', reader.showing, {
  open: [{ kind: 'file', id: hound.id, label: hound.name }],
  within: [{ kind: 'item', id: 'i-dune', label: 'Dune' }],
})
app.section = 'files'
let now = onScreenNow()
check('whichever app is open, the reader is on its screen', now.app, 'files')
check(
  'the document, first',
  now.showing.open.map((o) => o.id),
  [hound.id],
)
check('with where they are', now.showing.reader, { file: hound.id, page: 2, block: 6 })
ok(
  'and the item it came from',
  now.showing.within.some((w) => w.kind === 'item' && w.id === 'i-dune'),
)
const wire = toWire(now.app, now.showing)
check('it goes over the wire by file and place', wire.reader, { file: hound.id, page: 2, block: 6 })
ok(
  'and nothing the interface calls anything does',
  !JSON.stringify(wire).includes('Dune') && !JSON.stringify(wire).includes('Hound'),
)

files.focus(hound.id)
now = onScreenNow()
check(
  'a file named by the app as well is named once',
  now.showing.open.filter((o) => o.id === hound.id).length,
  1,
)
files.focus(null)

// The reader belongs to the app it was opened in: over another app it is
// not on screen, and coming back finds it there.
app.section = 'library'
check('not over another app', onScreenNow().showing.reader?.file, undefined)
app.section = 'files'
check('and there again on coming back', onScreenNow().showing.reader?.file, hound.id)
reader.close()
now = onScreenNow()
check(
  'closed, it is not on screen',
  [now.showing.reader, now.showing.within?.length ?? 0],
  [undefined, 0],
)
check('and nothing is sent', toWire(now.app, now.showing).reader, undefined)
app.section = 'files'

// ── Files: reading, and the search inside documents ─────────────────────

await files.start()
await files.read(hound.id, { page: 3 })
check(
  'Files opens a document in the reader',
  [reader.open, reader.file.id, reader.page],
  [true, hound.id, 3],
)
reader.close()

check(
  'no hits before a search',
  [files.documentHits, files.documentTotal, files.hasMoreDocumentHits],
  [[], 0, false],
)
files.setSearch('m')
await new Promise((r) => setTimeout(r, 500))
check('one letter is not worth looking for', files.documentHits, [])
files.setSearch('mortimer')
ok('found inside the book', await until(() => files.documentHits.length > 0))
check(
  'each hit has its file as the catalog has it',
  files.documentHits.every((h) => h.file.id === hound.id),
  true,
)
ok(
  'with the passage and where it is',
  files.documentHits.every((h) => /mortimer/i.test(h.hit.snippet) && h.hit.page >= 1),
)
check('and how many there are in all', files.documentTotal >= files.documentHits.length, true)
check('searching is over', files.documentSearching, false)
files.setSearch('')
check('clearing the search clears it at once', [files.documentHits, files.documentTotal], [[], 0])

const setting = files.settings
await files.saveSettings({ ...setting, indexDocuments: false })
files.setSearch('mortimer')
await new Promise((r) => setTimeout(r, 500))
check('with indexing off in Settings, nothing is looked for', files.documentHits, [])
await files.saveSettings({ ...setting, indexDocuments: true })
files.setSearch('')

await files.refreshIndexStatus()
ok('the index says how it stands', files.indexStatus.enabled && files.indexStatus.indexed >= 3)
await files.reindexDocuments()
check(
  'and can be started again',
  [files.indexStatus.running, files.indexStatus.failed],
  [false, []],
)

// ── the library keeps the files ─────────────────────────────────────────

const dune = library.items.find((i) => i.id === 'i-dune')
check('an item starts with no files', dune.files, [])
await library.attachFiles('i-dune', [hound.id, note.id, hound.id])
check('files go on in order, none twice', dune.files, [hound.id, note.id])
check('and are saved', (await api.item('i-dune')).files, [hound.id, note.id])
await library.attachFiles('i-dune', [note.id])
check('one it has is not added again', dune.files, [hound.id, note.id])
await library.detachFile('i-dune', hound.id)
check('a file comes off', [dune.files, (await api.item('i-dune')).files], [[note.id], [note.id]])
await library.detachFile('i-dune', hound.id)
check('one it does not have is no change', dune.files, [note.id])
await library.detachFile('i-dune', note.id)

const made = await library.createFromFile(hound.id)
check(
  'an item made from the book',
  [made.title, made.creator, made.year, made.files],
  ['The Hound of the Baskervilles', 'Arthur Conan Doyle', 1902, [hound.id]],
)
ok('on the Books shelf', library.kindOf(made)?.slug === 'book')
check(
  'it is selected, and on the list',
  [library.selected, library.items.some((i) => i.id === made.id)],
  [made.id, true],
)
check('made again, it is the same item', (await library.createFromFile(hound.id)).id, made.id)
const article = await library.createFromFile(pdf.id)
ok('a PDF goes on Articles', library.kindOf(article)?.slug === 'article')
check('a file that is no document makes nothing', await library.createFromFile(sheet.id), null)

// An item that is not on the shelf on screen is read, changed and written.
library.items = library.items.filter((i) => i.id !== 'i-pnin')
await library.attachFiles('i-pnin', [note.id])
check('an item that is not listed is changed all the same', (await api.item('i-pnin')).files, [
  note.id,
])

// ── the rail ────────────────────────────────────────────────────────────

ok('the assistant is there to ask', agent.supported)
const tick = agent.focusTick
reader.ask()
ok('asking opens the rail', await until(() => agent.open))
ok('and puts the caret in the composer', await until(() => agent.focusTick > tick))

// ── the file goes away ──────────────────────────────────────────────────

await reader.openFile(hound.id)
await reader.fileChanged()
ok('nothing changed under it, so it stays open', reader.open)
await api.trashFiles([hound.id])
await files.changed()
check('trashed, it closes', [reader.open, reader.file], [false, null])
ok(
  'with a word about it',
  notify.toasts.some((t) => t.key === 'reader-gone'),
)
await api.restoreFiles([hound.id])

// ── a lock puts everything away ─────────────────────────────────────────

await reader.openFile(hound.id, { item: 'i-dune' })
await reader.search('mortimer')
reader.reportView({ page: 3, block: 9 })
await app.lock()
check(
  'a lock empties it',
  [reader.open, reader.file, reader.outline, reader.item, reader.hits, reader.chapters, reader.at],
  [false, null, null, null, [], {}, null],
)
await new Promise((r) => setTimeout(r, 100))
check('and nothing lands after it', [reader.open, reader.file], [false, null])

finish('reader-store')
await close()

// `app.start()` arms the idle-vault poll; see `editor.test.mjs`'s own note.
process.exit(process.exitCode ?? 0)
