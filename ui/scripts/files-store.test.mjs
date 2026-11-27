// Behaviour checks for `files.svelte.ts` itself -- the class -- and the pure
// helpers in `files.ts` beside it.
//
// Driven through the real mock backend (`mock-files.ts`), the way
// `screentime-store.test.mjs` and `money-store.test.mjs` are: a fake `api`
// would be a second copy of what the mock already does, and what is worth
// checking here is the round trip -- a navigation that loads what is on screen
// and only the newest answer, the change that shows at once and is put back
// when refused, the destructive act that asks first, and a file that goes up
// in chunks.
//
// Written against the mock's seed: forty-five pictures and films across
// 2023 to 2026 (one hidden, one in the trash, one RAW under its JPEG), thirteen
// other files (one sealed, one missing, one in the trash), a folder tree
// (Photos, Documents, Journal, Assistant) and two albums. Later sections
// change what earlier ones read, so the order below matters.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

const stored = new Map()

stubBrowser({
  window: globalThis,
  localStorage: {
    getItem: (key) => stored.get(key) ?? null,
    setItem: (key, value) => stored.set(key, String(value)),
    removeItem: (key) => stored.delete(key),
  },
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
  location: new URL('http://localhost/?unlocked=1'),
  createObjectURL: () => 'blob:stub',
})
Object.defineProperty(globalThis, 'navigator', {
  value: { userAgent: 'node', language: 'en-GB' },
  configurable: true,
})

const {
  modules: [
    storeModule,
    helpers,
    stateModule,
    apiModule,
    notifyModule,
    iconsModule,
    symbolsModule,
    uiModule,
    healthModule,
  ],
  close,
} = await load(
  [
    '/src/lib/files.svelte.ts',
    '/src/lib/files.ts',
    '/src/lib/state.svelte.ts',
    '/src/lib/api.ts',
    '/src/lib/notify.svelte.ts',
    '/src/lib/icons.ts',
    '/src/lib/symbols.ts',
    '/src/lib/files-ui.svelte.ts',
    '/src/lib/health.svelte.ts',
  ],
  { svelte: true },
)
const { files, FILES_VIEWS, PAGE_SIZE, UPLOAD_CHUNK } = storeModule
const { formatSize, previewOf, kindIcon, fileWhen, addFiles } = helpers
const { app } = stateModule
const { api } = apiModule
const { notify } = notifyModule
const { ICONS } = iconsModule
const { SYMBOLS } = symbolsModule
const { ORIGIN_LABELS, USE_LABELS, fileMenu, openUse } = uiModule
const { health } = healthModule

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

/** Wait for something that happens in the background, up to two seconds. */
async function until(what, condition) {
  for (let i = 0; i < 100; i++) {
    if (condition()) return
    await sleep(20)
  }
  assert.fail(`never happened: ${what}`)
}

/** Wait for the view on screen to finish loading. */
const loaded = () => until('the view loads', () => !files.loading)

/** Answer the question on screen. */
async function answer(yes) {
  assert.ok(files.question, 'a question is on screen')
  await files.answer(yes)
  assert.equal(files.question, null, 'and it comes down')
}

/** When a timeline files a file: taken, else modified -- `FileEntry.when`. */
const whenOf = (f) => f.media.takenAt ?? f.modifiedAt
const names = (list) => list.map((f) => f.name)
/** The selection as a plain array: the store's own is a reactive proxy. */
const selected = () => [...files.selection]
const toast = (key) => notify.toasts.find((t) => t.key === key)
const file = (name) => files.visibleFiles.find((f) => f.name === name)

// ── the pure helpers ─────────────────────────────────────────────────

assert.equal(formatSize(0), '0 bytes')
assert.equal(formatSize(1), '1 byte')
assert.equal(formatSize(3), '3 bytes')
assert.equal(formatSize(1023), '1023 bytes')
assert.equal(formatSize(1024), '1 KB')
assert.equal(formatSize(1536), '1.5 KB')
assert.equal(formatSize(820 * 1024), '820 KB')
assert.equal(formatSize(1.4 * 1024 * 1024), '1.4 MB')
assert.equal(formatSize(1024 * 1024), '1 MB')
assert.equal(formatSize(10.4 * 1024 * 1024), '10 MB')
assert.equal(formatSize(1023.9 * 1024), '1 MB', '1024 KB is a megabyte, not 1024 of something')
assert.equal(formatSize(5 * 1024 ** 3), '5 GB')
assert.equal(formatSize(-1), '')
assert.equal(formatSize(NaN), '')

const preview = (mime, name, kind = 'other') => previewOf({ mime, name, kind })
assert.equal(preview('image/jpeg', 'a.jpg', 'image'), 'image')
assert.equal(preview('', 'photo.JPG'), 'image', 'by the name when the type says nothing')
assert.equal(preview('image/svg+xml', 'logo.svg', 'image'), 'image', 'an <img> runs no script')
assert.equal(preview('image/heic', 'IMG_1.HEIC', 'image'), 'none', 'a webview cannot draw HEIC')
assert.equal(preview('image/x-sony-arw', 'DSC1.ARW', 'image'), 'none', 'nor a RAW file')
assert.equal(preview('video/mp4', 'clip.mp4', 'video'), 'video')
assert.equal(preview('video/quicktime', 'clip.mov', 'video'), 'video')
assert.equal(preview('video/x-matroska', 'clip.mkv', 'video'), 'none')
assert.equal(preview('audio/mp4', 'memo.m4a', 'audio'), 'audio')
assert.equal(preview('application/pdf', 'doc.pdf', 'document'), 'pdf')
assert.equal(preview('text/markdown', 'notes.md', 'document'), 'text')
assert.equal(preview('text/csv', 'budget.csv', 'document'), 'text')
assert.equal(preview('application/json', 'data', 'document'), 'text')
assert.equal(preview('application/octet-stream', 'notes.txt'), 'text', 'a mailer that says nothing')
assert.equal(preview('application/octet-stream', '.env'), 'text', 'a name that is all extension')
assert.equal(preview('text/rtf', 'a.rtf', 'document'), 'none', 'rich text is control words')
assert.equal(preview('application/vnd.ms-word', 'letter.docx', 'document'), 'none', 'no Office')
assert.equal(preview('text/plain', 'letter.docx', 'document'), 'none', 'whatever it claims')
assert.equal(preview('text/plain', 'photo.png', 'image'), 'none', 'text is not a picture')
assert.equal(preview('application/zip', 'a.zip'), 'none')
assert.equal(preview('application/octet-stream', 'archive.tar.gz'), 'none')

assert.equal(kindIcon('image'), 'image')
assert.equal(kindIcon('video'), 'film')
assert.equal(kindIcon('audio'), 'music')
assert.equal(kindIcon('document'), 'file')
assert.equal(kindIcon('other'), 'file')
for (const kind of ['image', 'video', 'audio', 'document', 'other']) {
  assert.ok(kindIcon(kind) in ICONS, `${kind}'s icon is drawn`)
}
for (const name of ['folder', 'image', 'file']) {
  assert.ok(name in SYMBOLS, `${name} has a filled form`)
}
assert.equal(fileWhen({ media: { takenAt: 'taken' }, modifiedAt: 'modified' }), 'taken')
assert.equal(fileWhen({ media: {}, modifiedAt: 'modified' }), 'modified')

// ── start ────────────────────────────────────────────────────────────

assert.equal(files.supported, false, 'nothing is supported before a vault is open')
await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')
assert.equal(app.supportsFiles, true, 'and carry files')
assert.equal(FILES_VIEWS.length, 9)
assert.equal(PAGE_SIZE, 200)

app.section = 'files'
await files.start()
assert.equal(files.loaded, true)
assert.equal(files.supported, true)
assert.equal(files.view, 'browse', 'Browse is where it opens')
assert.equal(files.dir, '')
assert.equal(files.available, true)
assert.equal(files.local, true, 'the mock is the vault’s own computer')
assert.equal(files.folderState.state, 'available')
assert.equal(files.settings.trashDays, 30)
assert.equal(files.settings.folder, '/home/you/Jatayu')
assert.deepEqual(
  files.listing.folders.map((f) => f.name),
  ['Assistant', 'Documents', 'Journal', 'Photos'],
)
assert.equal(files.listing.folders.find((f) => f.name === 'Photos').items, 4, 'a year each')
assert.equal(files.listing.files.length, 0, 'a sealed file is in no folder')
assert.equal(files.visibleFiles.length, 0)
assert.equal(files.albums.length, 2)
const usage = files.status.usage
assert.equal(usage.files, 57)
assert.equal(usage.sealedFiles, 1)
assert.equal(usage.trashedFiles, 2)
assert.equal(usage.missingFiles, 1)
assert.equal(files.accent, 'var(--app-files)')
assert.equal(files.view, FILES_VIEWS[0])

// ── browsing ─────────────────────────────────────────────────────────

files.openFolder('Photos/2024')
await loaded()
assert.equal(files.dir, 'Photos/2024')
assert.deepEqual(
  files.listing.folders.map((f) => f.name),
  ['Birthday', 'Tahoe', 'Yosemite'],
)
files.up()
await loaded()
assert.equal(files.dir, 'Photos', 'up is the folder above')
files.openFolder('/Photos//2024/Yosemite/')
await loaded()
assert.equal(files.dir, 'Photos/2024/Yosemite', 'slashes are tidied')
assert.equal(files.visibleFiles.length, 8, 'neither the hidden picture nor the trashed one shows')
assert.equal(files.visibleFiles[0].name, 'MVI_0231.MP4', 'newest first by default')
files.setShowHidden(true)
assert.equal(files.visibleFiles.length, 9, 'hidden files shown on request')
assert.ok(file('screenshot-trail-map.png').hidden)
files.setShowHidden(false)
assert.equal(files.visibleFiles.length, 8)

// The order, and that it is remembered on this device.
files.setSort('name', false)
assert.equal(files.visibleFiles[0].name, 'DSC00112.JPG')
assert.equal(stored.get('jatayu.files.sort'), 'name')
assert.equal(stored.get('jatayu.files.descending'), '0')
files.setSort('size', true)
assert.equal(files.visibleFiles[0].name, 'MVI_0231.MP4', 'the film is the largest')
files.setSort('name', false)
files.setLayout('list')
assert.equal(stored.get('jatayu.files.layout'), 'list')
files.setLayout('grid')

// A search narrows a folder by name, with nothing to ask.
files.setSearch('00112')
assert.deepEqual(names(files.visibleFiles), ['DSC00112.JPG'])
assert.equal(files.listing.folders.length, 0, 'and the folders by name too')
files.setSearch('')
assert.equal(files.visibleFiles.length, 8)

// Three clicks through folders in a hurry: only the last answer lands.
files.openFolder('Photos/2023')
files.openFolder('Photos/2026')
files.openFolder('Photos/2025')
await loaded()
assert.equal(files.dir, 'Photos/2025')
assert.equal(files.listing.dir, 'Photos/2025')
assert.deepEqual(
  files.listing.folders.map((f) => f.name),
  ['Family', 'Kyoto'],
)

// A folder that went away under us: the one above is shown instead.
files.openFolder('Photos/Nowhere')
await loaded()
assert.equal(files.dir, 'Photos')
assert.equal(files.error, null)
files.up()
await loaded()
files.up()
assert.equal(files.dir, '', 'the top has nothing above it')
files.up()

// ── selection, the inspector and the viewer ──────────────────────────

files.openFolder('Photos/2024/Yosemite')
await loaded()
const [a, b, c, d] = files.visibleFiles
assert.deepEqual(names([a, b, c, d]), [
  'DSC00112.JPG',
  'DSC00131.JPG',
  'DSC00158.JPG',
  'DSC00170.JPG',
])
files.select(a.id)
assert.deepEqual(selected(), [a.id])
assert.equal(files.focused, a.id, 'the inspector follows a lone selection')
await until('the detail loads', () => files.detail?.file.id === a.id)
assert.equal(files.detail.uses.length, 0)
files.select(c.id, 'range')
assert.deepEqual(selected(), [a.id, b.id, c.id], 'shift-click is a range from the last click')
files.select(d.id, 'toggle')
assert.deepEqual(selected(), [a.id, b.id, c.id, d.id])
files.select(b.id, 'toggle')
assert.deepEqual(selected(), [a.id, c.id, d.id], 'toggling a selected file takes it out')
assert.equal(files.selectedFiles.length, 3)
files.selectAll()
assert.equal(files.selection.length, 8)
assert.equal(files.fileById(d.id).name, 'DSC00170.JPG')
assert.equal(files.fileById('no-such-file'), undefined)
files.clearSelection()
assert.deepEqual(selected(), [])
assert.equal(files.focused, null)
assert.equal(files.detail, null)

files.open(a.id)
assert.equal(files.viewing, a.id)
assert.equal(files.focused, a.id, 'the viewer describes what it shows')
files.step(1)
assert.equal(files.viewing, b.id, 'the next in the list')
files.step(-1)
files.step(-1)
assert.equal(files.viewing, a.id, 'and no further back than the first')
files.closeViewer()
assert.equal(files.viewing, null)
files.focus(null)

// What a file is used by, and what an app wrote and nothing uses.
files.openFolder('Journal/2026/11')
await loaded()
assert.deepEqual(names(files.visibleFiles), ['IMG_7720.jpg', 'IMG_7741.jpg'])
files.select(file('IMG_7741.jpg').id)
await until('the uses load', () => files.detail?.uses.length === 1)
assert.equal(files.detail.uses[0].title, 'A quiet Thursday')
assert.equal(files.detail.uses[0].owner.kind, 'entry')
assert.equal(file('IMG_7741.jpg').origin, 'journal')
files.clearSelection()

// ── the other views ──────────────────────────────────────────────────

files.setView('recent')
await loaded()
assert.equal(files.view, 'recent')
assert.equal(files.page.total, 55, 'present, not hidden, not in the trash')
assert.equal(files.visibleFiles[0].name, 'IMG_7741.jpg', 'newest added first')
for (let i = 1; i < files.visibleFiles.length; i++) {
  assert.ok(files.visibleFiles[i - 1].addedAt >= files.visibleFiles[i].addedAt, 'in order added')
}
assert.equal(files.hasMore, false)
assert.equal(stored.get('jatayu.files.view'), 'recent', 'the view is remembered')

files.setView('photos')
await loaded()
assert.equal(files.page.total, 43, 'pictures and films, one of a stack, none hidden or trashed')
assert.ok(files.visibleFiles.every((f) => f.kind === 'image' || f.kind === 'video'))
assert.ok(
  files.visibleFiles.every((f) => !f.stack),
  'only the top of each stack',
)
assert.ok(!names(files.visibleFiles).includes('DSC02011.ARW'), 'the RAW is under its JPEG')
for (let i = 1; i < files.visibleFiles.length; i++) {
  assert.ok(
    whenOf(files.visibleFiles[i - 1]) >= whenOf(files.visibleFiles[i]),
    'by the date taken (else modified), newest first',
  )
}
await until('the facets load', () => files.facets !== null)
assert.deepEqual(
  files.facets.years.map((y) => y.name),
  ['2026', '2025', '2024', '2023'],
  'years, newest first',
)
assert.ok(files.facets.people.some((p) => p.name === 'Asha'))
assert.ok(files.facets.cameras.some((p) => p.name === 'Sony ILCE-7M4'))
assert.ok(files.facets.places.some((p) => p.name === 'Kyoto, Japan'))
assert.ok(files.facets.tags.some((t) => t.name === 'sunset'))

const narrowed = async (facet, total, check) => {
  files.setFacet(facet)
  await loaded()
  assert.equal(files.view, 'photos')
  assert.equal(files.page.total, total, `${facet.kind} ${facet.name}`)
  assert.ok(files.visibleFiles.every(check), `${facet.kind} ${facet.name} keeps to its own`)
}
await narrowed({ kind: 'year', name: '2024' }, 15, (f) => f.media.takenLocal.startsWith('2024'))
await narrowed({ kind: 'person', name: 'Asha' }, 6, (f) => f.people.includes('Asha'))
await narrowed({ kind: 'tag', name: 'sunset' }, 4, (f) => f.tags.includes('sunset'))
await narrowed({ kind: 'place', name: 'Kyoto, Japan' }, 5, (f) => f.place === 'Kyoto, Japan')
await narrowed({ kind: 'camera', name: 'Fujifilm X-T5' }, 5, (f) => f.media.cameraModel === 'X-T5')
files.setFacet(null)
await loaded()
assert.equal(files.page.total, 43, 'no facet is every photo')
files.setFacet({ kind: 'person', name: 'Mira' })
files.setView('photos')
await loaded()
assert.equal(files.facet, null, 'choosing Photos again drops what narrowed it')
assert.equal(files.page.total, 43)

files.setView('albums')
await loaded()
assert.equal(files.album, null)
assert.equal(files.visibleFiles.length, 0, 'the album list is not a list of files')
assert.deepEqual(
  files.albums.map((x) => x.count),
  [8, 4],
)
assert.equal(files.albums[0].cover.name, 'DSC01980.JPG')
files.setSearch('birthday')
assert.equal(files.visibleAlbums.length, 1, 'a search narrows the albums by name')
files.setSearch('')
files.openAlbum(files.albums[0].album.id)
await loaded()
assert.equal(files.view, 'albums')
assert.equal(files.page.total, 8)
files.select(file('DSC01980.JPG').id)
await until('the detail loads', () => files.detail?.albums.length === 1)
assert.ok(files.detail.albums[0].name.startsWith('Kyoto'), 'what it is an album member of')
files.clearSelection()
files.setView('albums')
assert.equal(files.album, null, 'choosing Albums again leaves the album')

files.setView('favourites')
await loaded()
assert.ok(files.page.total > 3)
assert.ok(files.visibleFiles.every((f) => f.favourite))

files.setView('encrypted')
await loaded()
assert.deepEqual(names(files.visibleFiles), ['passport-scan.pdf'])
assert.equal(files.visibleFiles[0].storage.kind, 'sealed')

files.setView('unused')
await loaded()
assert.deepEqual(names(files.visibleFiles).sort(), ['IMG_7720.jpg', 'budget-chart.png'])

files.setView('missing')
await loaded()
assert.deepEqual(names(files.visibleFiles), ['2022-return.pdf'])
assert.equal(files.visibleFiles[0].status.state, 'missing')

files.setView('trash')
await loaded()
assert.deepEqual(names(files.visibleFiles).sort(), ['IMG_0412-blurry.jpg', 'old-draft.md'])
assert.equal(files.visibleFiles[0].status.state, 'trashed')

// A search in a catalog view looks through captions and tags as well as names.
files.setView('photos')
files.setSearch('half dome')
await until('the search lands', () => files.page?.total === 1)
assert.equal(files.visibleFiles[0].name, 'DSC00158.JPG')
files.setSearch('')
await until('the search clears', () => files.page?.total === 43)

// ── changing what the person added ───────────────────────────────────

files.setView('browse')
files.openFolder('Photos/2024/Yosemite')
await loaded()
const tunnel = file('DSC00112.JPG')
assert.equal(tunnel.favourite, true)

let pending = files.toggleFavourite(tunnel.id)
assert.equal(files.fileById(tunnel.id).favourite, false, 'shown at once')
await pending
assert.equal(files.fileById(tunnel.id).favourite, false)
assert.equal((await api.getFile(tunnel.id)).file.favourite, false, 'and the vault agrees')
await files.setRating([tunnel.id], 3)
assert.equal(files.fileById(tunnel.id).rating, 3)
await files.setRating([tunnel.id], 0)
assert.equal(files.fileById(tunnel.id).rating, undefined, 'zero clears it')
await files.update([tunnel.id], {
  addTags: ['keeper'],
  removeTags: ['hiking'],
  caption: 'Tunnel View',
})
assert.ok(files.fileById(tunnel.id).tags.includes('keeper'))
assert.ok(!files.fileById(tunnel.id).tags.includes('hiking'))
assert.equal(files.fileById(tunnel.id).caption, 'Tunnel View')
await files.update([tunnel.id], { place: 'Yosemite Valley', addPeople: ['Dev'] })
assert.equal(files.fileById(tunnel.id).place, 'Yosemite Valley')
assert.deepEqual([...files.fileById(tunnel.id).people], ['Dev'])

// A change the vault refuses is put back, and said so.
{
  const real = api.updateFiles
  api.updateFiles = async () => {
    throw new Error('the vault said no')
  }
  const before = files.fileById(tunnel.id).rating
  pending = files.setRating([tunnel.id], 5)
  assert.equal(files.fileById(tunnel.id).rating, 5, 'shown at once')
  await pending
  assert.equal(files.fileById(tunnel.id).rating, before, 'put back when refused')
  const failed = toast('files-failed')
  assert.ok(failed, 'a notice says so')
  assert.equal(failed.title, 'The change could not be saved.')
  assert.equal(failed.body, 'the vault said no')
  api.updateFiles = real
}

// Renaming, and a name that clashes.
const hill = file('DSC00170.JPG')
await files.renameFile(hill.id, 'Ravi at the top.JPG')
assert.ok(file('Ravi at the top.JPG'), 'renamed')
assert.equal(file('DSC00170.JPG'), undefined)
await files.renameFile(file('DSC00204.JPG').id, 'Ravi at the top.JPG')
assert.ok(file('DSC00204.JPG'), 'a clash is put back')
assert.equal(toast('files-failed').title, 'The file could not be renamed.')

// Stacking one picture under another, and taking it apart.
await files.stack(file('DSC00204.JPG').id, [file('DSC00260.JPG').id])
assert.equal(file('DSC00260.JPG').stack, file('DSC00204.JPG').id)
await files.unstack([file('DSC00204.JPG').id])
assert.equal(file('DSC00260.JPG').stack, undefined)

// Sealing a file takes it out of its folder; unsealing puts it back.
const sealed = file('DSC00233.JPG')
await files.setEncrypted(sealed.id, true)
assert.equal(file('DSC00233.JPG'), undefined, 'a sealed file is in no folder')
files.setView('encrypted')
await loaded()
assert.equal(file('DSC00233.JPG').storage.kind, 'sealed')
await files.setEncrypted(sealed.id, false)
files.setView('browse')
await loaded()
assert.equal(file('DSC00233.JPG').storage.kind, 'plain', 'back where it was')

// Moving files out of the folder on screen.
const mist = file('DSC00233.JPG')
pending = files.moveFiles([mist.id], 'Photos/2024/Tahoe')
assert.equal(file('DSC00233.JPG'), undefined, 'gone from this folder at once')
await pending
files.openFolder('Photos/2024/Tahoe')
await loaded()
assert.equal(files.visibleFiles.length, 4)
assert.ok(file('DSC00233.JPG'))
await files.moveFiles([mist.id], 'Photos/2024/Yosemite')
files.openFolder('Photos/2024/Yosemite')
await loaded()

// Things that happen on this computer.
await files.openExternally(mist.id)
await files.reveal(mist.id)
await files.exportFiles([mist.id, tunnel.id])
assert.equal(toast('files-saved').title, 'Saved 2 files.')

// ── the trash ────────────────────────────────────────────────────────

const doomed = file('DSC00131.JPG')
const before = files.listing.files.length
pending = files.trash([doomed.id])
assert.equal(files.listing.files.length, before - 1, 'leaves the list at once')
assert.equal(files.question, null, 'and the trash does not ask')
await pending
const trashed = toast('files-trashed')
assert.equal(trashed.title, 'Moved 1 file to the trash.')
assert.equal(trashed.action.label, 'Undo', 'the trash is the undo')
assert.equal(files.status.usage.trashedFiles, 3)
trashed.action.run()
await until('the undo restores it', () => file('DSC00131.JPG') !== undefined)
await until('and the count follows', () => files.status.usage.trashedFiles === 2)

await files.trash([doomed.id])
files.setView('trash')
await loaded()
assert.equal(files.page.total, 3)
await files.restore([doomed.id])
assert.equal(files.page.total, 2, 'out of the trash at once')

// Deleting for good asks first, and "no" is the way out.
const blurry = file('IMG_0412-blurry.jpg')
files.deleteForever([blurry.id])
assert.match(files.question.title, /Delete this file for good/)
assert.equal(files.question.danger, true)
assert.equal(files.question.confirm, 'Delete')
await answer(false)
assert.equal(files.page.total, 2, 'nothing was deleted')
files.deleteForever([blurry.id])
await answer(true)
assert.equal(files.page.total, 1)
assert.equal(files.status.usage.trashedFiles, 1)
files.emptyTrash()
assert.equal(files.question.title, 'Empty the trash?')
assert.match(files.question.body, /^1 file in the trash is deleted/)
await answer(true)
assert.equal(files.page.total, 0)
assert.equal(files.status.usage.trashedFiles, 0)

// Forgetting what went missing asks first as well.
files.setView('missing')
await loaded()
const gone = files.visibleFiles[0]
files.forgetMissing([gone.id])
assert.match(files.question.title, /Forget this missing file/)
await answer(false)
assert.equal(files.page.total, 1)
files.forgetMissing([gone.id])
await answer(true)
assert.equal(files.page.total, 0)
assert.equal(files.status.usage.missingFiles, 0)

// A second question replaces the first, which is answered no.
files.deleteForever(['one'])
const first = files.question
files.emptyTrash()
assert.notEqual(files.question, first)
assert.equal(files.question.title, 'Empty the trash?')
await answer(false)

// ── folders ──────────────────────────────────────────────────────────

files.setView('browse')
files.openFolder('Documents/Scans')
await loaded()
assert.equal(files.visibleFiles.length, 0, 'a folder with nothing in it')
assert.equal(await files.createFolder('2026'), 'Documents/Scans/2026')
assert.deepEqual(
  files.listing.folders.map((f) => f.name),
  ['2026'],
)
assert.equal(await files.createFolder('   '), null, 'a blank name makes nothing')
assert.equal(await files.createFolder('2026'), null, 'nor does one already there')
assert.equal(toast('files-failed').title, 'The folder could not be made.')
files.openFolder('Documents/Scans/2026')
await loaded()
await files.renameFolder('Documents/Scans/2026', 'Receipts')
assert.equal(files.dir, 'Documents/Scans/Receipts', 'inside a folder renamed, Browse follows it')
files.openFolder('Documents/Scans')
await loaded()
await files.moveFolder('Documents/Scans/Receipts', 'Documents')
assert.deepEqual(files.listing.folders, [])
files.openFolder('Documents')
await loaded()
assert.ok(files.listing.folders.some((f) => f.name === 'Receipts'))
files.openFolder('Documents/Receipts')
await loaded()
await files.trashFolder('Documents/Receipts')
assert.equal(files.dir, 'Documents', 'in a folder trashed, Browse goes to the one above')
assert.ok(!files.listing.folders.some((f) => f.name === 'Receipts'))
await files.trashFolder('Assistant/Images')
files.openFolder('Assistant')
await loaded()
assert.deepEqual(
  files.listing.folders.map((f) => f.name),
  ['Reports', 'Research'],
)
assert.equal(toast('files-trashed').title, 'Moved 1 file to the trash.')

// ── albums ───────────────────────────────────────────────────────────

files.openFolder('Photos/2024/Yosemite')
await loaded()
const [p1, p2, p3] = files.visibleFiles
const album = await files.createAlbum('  Weekend  ', [p1.id, p2.id])
assert.equal(album.name, 'Weekend')
assert.equal(files.albums.length, 3)
const view = () => files.albums.find((x) => x.album.id === album.id)
assert.equal(view().count, 2)
assert.equal(await files.createAlbum('   '), null)
await files.renameAlbum(album, 'Long weekend')
assert.equal(view().album.name, 'Long weekend')
await files.setAlbumCover(view().album, p2.id)
assert.equal(view().cover.id, p2.id)
await files.addToAlbum(album.id, [p3.id])
assert.equal(view().count, 3)
files.openAlbum(album.id)
await loaded()
assert.equal(files.page.total, 3)
const removing = files.removeFromAlbum(album.id, [p1.id])
assert.equal(files.visibleFiles.length, 2, 'out of the open album at once')
await removing
assert.equal(view().count, 2)
files.deleteAlbum(view().album)
assert.match(files.question.title, /Delete “Long weekend”/)
await answer(false)
assert.ok(view(), 'not deleted')
files.deleteAlbum(view().album)
await answer(true)
await until('the album goes', () => view() === undefined)
assert.equal(files.album, null, 'the album that was open is left')
assert.equal(files.albums.length, 2)

// ── adding files ─────────────────────────────────────────────────────

files.setView('browse')
files.openFolder('Documents/Notes')
await loaded()

// A small one, in a single chunk.
const hello = await files.uploadFiles([new File(['hello world'], 'hello.txt')], {
  dir: 'Documents/Notes',
})
assert.equal(hello.length, 1)
assert.equal(hello[0].name, 'hello.txt')
assert.equal(hello[0].size, 11)
assert.equal(files.uploads.at(-1).state, 'done')
assert.equal(files.uploads.at(-1).sent, 11)
assert.equal(files.uploads.at(-1).file.id, hello[0].id)
assert.ok(file('hello.txt'), 'and the folder on screen shows it')
const again = await files.uploadFiles([new File(['hello again'], 'hello.txt')], {
  dir: 'Documents/Notes',
})
assert.equal(again[0].name, 'hello (2).txt', 'a clash never overwrites')
assert.deepEqual(await files.uploadFiles([]), [])

// A big one goes up in chunks, with progress.
const real = api.uploadChunk
const calls = []
api.uploadChunk = async (upload, offset, bytes) => {
  calls.push([offset, bytes.length, files.uploads.at(-1).sent])
  return real(upload, offset, bytes)
}
const big = new File([new Uint8Array(UPLOAD_CHUNK * 2 + 10)], 'big.bin')
const gotBig = await files.uploadFiles([big], { dir: 'Documents/Notes' })
assert.deepEqual(calls, [
  [0, UPLOAD_CHUNK, 0],
  [UPLOAD_CHUNK, UPLOAD_CHUNK, UPLOAD_CHUNK],
  [UPLOAD_CHUNK * 2, 10, UPLOAD_CHUNK * 2],
])
assert.equal(gotBig[0].size, UPLOAD_CHUNK * 2 + 10)
assert.equal(files.uploads.at(-1).sent, UPLOAD_CHUNK * 2 + 10)

// A chunk that fails is sent once more.
let dropped = false
api.uploadChunk = async (upload, offset, bytes) => {
  if (offset === UPLOAD_CHUNK && !dropped) {
    dropped = true
    throw new Error('connection reset')
  }
  return real(upload, offset, bytes)
}
const flaky = new File([new Uint8Array(UPLOAD_CHUNK + 5)], 'retried.bin')
const retried = await files.uploadFiles([flaky])
assert.equal(retried.length, 1, 'a chunk is sent again, once')
assert.equal(files.uploads.at(-1).state, 'done')

// ...and a second failure is the end of it.
api.uploadChunk = async (upload, offset, bytes) => {
  if (offset >= UPLOAD_CHUNK) throw new Error('offline')
  return real(upload, offset, bytes)
}
const lost = await files.uploadFiles([new File([new Uint8Array(UPLOAD_CHUNK + 5)], 'lost.bin')])
assert.deepEqual(lost, [])
assert.equal(files.uploads.at(-1).state, 'failed')
assert.equal(files.uploads.at(-1).error, 'offline')
api.uploadChunk = real

// Cancelling stops it, and nothing arrives.
pending = files.uploadFiles([new File([new Uint8Array(UPLOAD_CHUNK + 1)], 'cancelled.bin')])
const row = files.uploads.at(-1)
assert.equal(row.state, 'sending')
files.cancelUpload(row.key)
assert.equal(row.state, 'cancelled')
assert.deepEqual(await pending, [])
assert.equal((await api.listFiles({ text: 'cancelled' })).total, 0)
files.cancelUpload('no-such-row')
files.clearUploads()
assert.equal(files.uploads.length, 0, 'finished and failed rows are forgotten')

// What any other app adds files through.
const viaAdd = await addFiles([new File(['x'], 'via-add.txt')], { dir: 'Documents/Notes' })
assert.equal(viaAdd[0].name, 'via-add.txt')
assert.deepEqual(await addFiles([]), [])
const viaPath = await addFiles(['/home/you/Downloads/trip.pdf'], { dir: 'Documents/Notes' })
assert.equal(viaPath[0].name, 'trip.pdf')

// By path, from a window on the vault's own computer.
const imported = await files.importPaths(['/home/you/Downloads/a.jpg', 'C:\\Users\\you\\b.png'], {
  dir: 'Documents/Notes',
})
assert.deepEqual(names(imported), ['a.jpg', 'b.png'])
assert.equal(imported[0].kind, 'image')
const picked = await files.pickAndImport({ dir: 'Documents/Notes' })
assert.equal(picked.length, 1)
assert.match(picked[0].name, /^Picked photo/)
files.status.local = false
assert.deepEqual(await files.importPaths(['/home/you/Downloads/c.jpg']), [], 'not from elsewhere')
assert.deepEqual(await files.pickAndImport(), [])
files.status.local = true

// Which apps keep their files sealed is the vault's setting.
await files.saveSettings({ ...files.settings, trashDays: 7, encrypted: ['mail'] })
assert.equal(files.settings.trashDays, 7)
const [invoice] = await files.importPaths(['/home/you/Downloads/invoice.pdf'], { origin: 'mail' })
assert.equal(invoice.storage.kind, 'sealed', 'a mail attachment is sealed now')
const [receipt] = await files.importPaths(['/home/you/Downloads/receipt.pdf'], { origin: 'notes' })
assert.equal(receipt.storage.kind, 'plain')

// ── the folder, and what to bring in ─────────────────────────────────

await files.setUpFolder('/data/Jatayu')
assert.equal(files.folderState.path, '/data/Jatayu')
assert.equal(files.settings.folder, '/data/Jatayu')
assert.equal(files.dir, '', 'a new folder is opened at its top')
await files.chooseFolder()
assert.equal(files.folderState.path, '/home/you/Documents/Jatayu')
const scanned = files.status.lastScan
await sleep(5)
await files.rescan()
assert.notEqual(files.status.lastScan, scanned)
assert.equal(files.busy, false)
assert.equal(toast('files-scan').title, 'Nothing has changed.')
assert.equal(files.status.legacy.files, 17)
await files.bringInAttachments()
assert.equal(files.status.legacy.files, 0, 'the old attachments are in')
assert.equal(toast('files-brought-in').title, 'Brought in 17 files (96.0 MB).')

// ── a list longer than a page ────────────────────────────────────────

const paths = Array.from({ length: 250 }, (_, i) => `/home/you/bulk/bulk-${i}.txt`)
await files.importPaths(paths, { dir: 'Bulk' })
files.setView('recent')
await loaded()
const total = files.page.total
assert.ok(total > PAGE_SIZE, 'more than a page')
assert.equal(files.visibleFiles.length, PAGE_SIZE)
assert.equal(files.hasMore, true)
await files.loadMore()
assert.equal(files.visibleFiles.length, Math.min(total, PAGE_SIZE * 2), 'the next page, appended')
while (files.hasMore) await files.loadMore()
assert.equal(files.visibleFiles.length, total, 'and the rest')
assert.equal(new Set(files.visibleFiles.map((f) => f.id)).size, total, 'none twice')
assert.equal(files.hasMore, false)
await files.loadMore()
assert.equal(files.visibleFiles.length, total, 'nothing more to load')
await files.reload()
assert.equal(files.visibleFiles.length, total, 'a reload keeps the list as long as it was')

// ── what is on screen ────────────────────────────────────────────────

files.setView('browse')
files.openFolder('Photos/2024')
await loaded()
assert.equal(files.showing.view, 'their files folder, in the folder Photos/2024')
assert.deepEqual(files.showing.open, [])
files.setView('trash')
assert.equal(files.showing.view, 'the files in the trash')

// ── live updates ─────────────────────────────────────────────────────

files.setView('browse')
await loaded()
assert.ok(files.changed() instanceof Promise, 'a change reloads while Files is on screen')
await files.changed()
app.section = 'journal'
assert.equal(files.changed(), undefined, 'but not while another app is')
app.section = 'files'

// ── Health's files ───────────────────────────────────────────────────

// Every origin a file can have is named in words, Health's included, and
// every kind of record that can use one says which app it is.
{
  const [page] = await addFiles([new File(['x'], 'lab.pdf')], { origin: 'health', encrypted: true })
  assert.equal(page.origin, 'health')
  assert.equal(page.storage.kind, 'sealed')
  assert.equal(ORIGIN_LABELS[page.origin], 'Health')
  assert.equal(ORIGIN_LABELS.mail, 'Mail')
  const origins = new Set((await api.listFiles({ includeHidden: true })).files.map((f) => f.origin))
  for (const origin of origins) assert.ok(ORIGIN_LABELS[origin], `${origin} has a name`)
  assert.deepEqual(Object.keys(USE_LABELS).sort(), [
    'entry',
    'healthDocument',
    'item',
    'meal',
    'message',
    'note',
    'story',
  ])
  assert.equal(USE_LABELS.healthDocument, 'Health document')
  assert.equal(USE_LABELS.meal, 'Meal')

  // Files can be told to leave Health's out, as the assistant's tools do.
  const without = await api.listFiles({ excludeOrigins: ['health'], text: 'lab.pdf' })
  assert.equal(without.total, 0)
  assert.equal((await api.listFiles({ text: 'lab.pdf' })).total, 1)
}

// A "used in" row opens the record it names, in Health.
{
  app.section = 'files'
  await openUse({
    owner: { kind: 'healthDocument', id: 'doc-october' },
    title: 'Lab report, October',
  })
  assert.equal(app.section, 'health')
  assert.equal(health.documentId, 'doc-october')
  assert.equal(health.view, 'records', 'on the pane documents are drawn on')

  app.section = 'files'
  const eaten = new Date(Date.now() - 9 * 86_400_000)
  const day = [eaten.getFullYear(), eaten.getMonth() + 1, eaten.getDate()]
    .map((n, i) => (i === 0 ? String(n) : String(n).padStart(2, '0')))
    .join('-')
  await openUse({
    owner: { kind: 'meal', id: 'meal-1' },
    title: 'Lunch',
    when: eaten.toISOString(),
  })
  assert.equal(app.section, 'health')
  assert.equal(health.view, 'nutrition')
  assert.equal(health.date, day, 'on the day it was eaten')
  assert.equal(health.anchor, day, 'in a window that holds it')

  // Without a time it opens on today.
  app.section = 'files'
  await openUse({ owner: { kind: 'meal', id: 'meal-1' }, title: 'Lunch' })
  assert.equal(health.date, new Date().toLocaleDateString('en-CA'))
  health.reset()
}

// An archive can be imported into Health from its menu.
{
  files.setView('browse')
  const [zip] = await addFiles([new File(['PK'], 'export.zip')], { dir: 'Documents' })
  const [text] = await addFiles([new File(['x'], 'notes.txt')], { dir: 'Documents' })
  const labels = (item) => fileMenu(item).map((i) => i.label)
  assert.ok(labels(zip).includes('Import into Health'))
  assert.ok(!labels(text).includes('Import into Health'), 'only an archive')
  const upper = { ...zip, name: 'EXPORT.ZIP' }
  assert.ok(labels(upper).includes('Import into Health'), 'whatever its case')
  const item = fileMenu(zip).find((i) => i.label === 'Import into Health')
  assert.equal(item.icon, 'heart-pulse')
  assert.ok(item.icon in ICONS, 'and the icon is drawn')
  await item.run()
  assert.equal(health.importStatus?.state, 'running')
  assert.equal(health.importStatus?.fileName, 'export.zip')
  assert.equal(toast('health-import-started').title, 'Importing into Health.')
  assert.match(toast('health-import-started').body, /Settings → Health/)
  health.reset()
}

// ── a lock ───────────────────────────────────────────────────────────

files.deleteForever(['one'])
files.selection = ['somewhere']
files.reset()
assert.equal(files.question, null, 'a question goes with the lock')
assert.equal(files.loaded, false)
assert.equal(files.status, null)
assert.equal(files.listing, null)
assert.equal(files.page, null)
assert.equal(files.albums.length, 0)
assert.equal(files.facets, null)
assert.deepEqual(selected(), [])
assert.equal(files.focused, null)
assert.equal(files.uploads.length, 0)
assert.equal(files.dir, '', 'it comes back at the top')
assert.equal(files.view, 'browse', 'but on the same pane, which is chrome')
assert.equal(files.sort, 'name', 'in the same order')
assert.equal(files.layout, 'grid')
await files.flush()
await files.start()
assert.equal(files.loaded, true, 'and it opens again after an unlock')

await close()
console.log('files-store: all checks passed')

// Same reason `screentime-store.test.mjs` ends like this: `app.start()` arms
// the idle-vault poll, which would otherwise hold this process open for ever.
process.exit(0)
