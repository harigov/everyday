// State for Files: the files folder, the catalog, albums and the Photos view.
//
// A sibling of `screentime.svelte.ts`. Everything here goes through `api` --
// the mock backend (`mock-files.ts`) stands in for the vault and the shell in
// a browser.
//
// The public names below are the contract `components/Files*.svelte` and
// `components/Photos*.svelte` are written against: each name and signature
// was laid down before the bodies were.
//
// Rules that shape it:
//
//   1. Every navigation reloads through a generation counter, so when
//      somebody clicks through three folders quickly only the last answer
//      lands.
//   2. Deleting for good, emptying the trash, forgetting missing files and
//      deleting an album ask first, through `question`, which `App.svelte`
//      draws. Trashing does not ask: the trash is the undo.
//   3. Adding files never goes through the webview as one buffer: a window
//      on the vault's machine imports by path (`api.importFiles`,
//      `api.pickAndImportFiles`); anything else uploads in chunks of
//      `UPLOAD_CHUNK` bytes (`uploadFiles`).
//   4. The assistant is told the view, the folder and the selection's ids,
//      never a file's name or caption (`showing`).
//   5. A change shows at once and is put back, with a notice, when the vault
//      refuses it. Whatever changed, the view is read again afterwards; and
//      a reload keeps as many files loaded as were, so a list somebody has
//      scrolled down does not jump back to its first page.
//
// What is loaded follows what is on screen, as Screen time's is: Browse
// reads a folder (`listFolder`), every other view reads a page of the
// catalog (`listFiles`) -- Albums reads the album list until one is open --
// and Photos reads the facets for its sidebar. The status (folder state,
// settings, usage) and the album list are small and wanted by more than one
// pane, so they are read with every view.
//
// Members added after the skeleton are marked as such in their own comments.

import { api, fileUrl, fileVersion, hasShell } from './api'
import { humanBytes, plural } from './format'
import { seen, type Showing } from './onscreen'
import { notify } from './notify.svelte'
import { pref } from './prefs'
import { reader, type ReaderOpenOptions } from './reader.svelte'
import { app, errorMessage, handle, isLocked, quietly } from './state.svelte'
import { debounce } from './store/debounce'
import { latest } from './store/latest'
import type {
  Album,
  AlbumId,
  AlbumView,
  DocIndexStatus,
  DocSearchHit,
  FileDetail,
  FileEntry,
  FileId,
  FileOrigin,
  FilePage,
  FileQuery,
  FileSort,
  FileUpdate,
  FilesFacets,
  FilesSettings,
  FilesStatus,
  FolderListing,
  FolderState,
} from './types'
import { VaultError } from './types'

/** The panes of the Files app, in sidebar order. */
export const FILES_VIEWS = [
  'browse',
  'recent',
  'photos',
  'albums',
  'favourites',
  'encrypted',
  'unused',
  'missing',
  'trash',
] as const
export type FilesView = (typeof FILES_VIEWS)[number]

/** What the Photos view is narrowed to, from its sidebar. */
export type PhotoFacet =
  | { kind: 'tag'; name: string }
  | { kind: 'person'; name: string }
  | { kind: 'place'; name: string }
  | { kind: 'year'; name: string }
  | { kind: 'camera'; name: string }

export type FilesLayout = 'grid' | 'list'

/** How one selection change combines with the last. */
export type SelectMode = 'replace' | 'toggle' | 'range'

/** One file on its way in. */
export interface UploadProgress {
  /** Local, for keying the row. */
  key: string
  name: string
  size: number
  sent: number
  state: 'sending' | 'done' | 'failed' | 'cancelled'
  error?: string
  file?: FileEntry
}

/** A confirmation `App.svelte` draws over everything. */
export interface FilesQuestion {
  title: string
  body: string
  confirm: string
  danger: boolean
  run: () => Promise<void>
}

/** How new files are kept, for every way of adding them. */
export interface AddOptions {
  /** Folder relative to the files folder; the origin's own when absent. */
  dir?: string
  origin?: FileOrigin
  encrypted?: boolean
}

/** A passage the search found inside a document, with the file it is in. */
export interface DocumentFound {
  file: FileEntry
  hit: DocSearchHit
}

/** What makes two found places the same place: the file and the block. The
 *  key the Files list draws them under. */
export function placeKey(found: DocumentFound): string {
  return `${found.file.id}:${found.hit.block}`
}

/** Bytes per upload chunk. */
export const UPLOAD_CHUNK = 4 * 1024 * 1024

/** Files per page in the list views. */
export const PAGE_SIZE = 200

/** The most a reload asks for at once to keep a scrolled list where it was. */
const MAX_KEPT = 1000

/** Passages the search inside documents lists at a time. */
const DOCUMENT_HITS = 20

/** The fewest characters worth searching inside documents for: one letter
 *  matches nearly every block of every book. */
const DOCUMENT_MIN_QUERY = 2

/** How long a failed chunk waits before its one retry. */
const RETRY_AFTER_MS = 500

/** What Files tints the window with: the app's own colour from the theme, the
 *  same token the sidebar and the app switcher use. */
const FILES_ACCENT = 'var(--app-files)'

const SORTS: readonly FileSort[] = ['when', 'added', 'name', 'size']

/**
 * Which pane was last open, how it is laid out and in what order -- chrome,
 * remembered on this device, read in the constructor so the app does not draw
 * Browse and visibly flip.
 */
const viewPref = pref<FilesView>(
  'jatayu.files.view',
  (raw) => FILES_VIEWS.find((v) => v === raw) ?? 'browse',
  'browse',
)
const layoutPref = pref<FilesLayout>(
  'jatayu.files.layout',
  (raw) => (raw === 'list' ? 'list' : 'grid'),
  'grid',
)
const sortPref = pref<FileSort>(
  'jatayu.files.sort',
  (raw) => SORTS.find((s) => s === raw) ?? 'when',
  'when',
)
const descendingPref = pref<boolean>(
  'jatayu.files.descending',
  (raw) => raw !== '0',
  true,
  (on) => (on ? '1' : '0'),
)

let uploadCount = 0

/** A fresh id for an album that has not been saved yet -- the stopgap Money's
 *  `mintId` documents: `save_album` is a plain upsert, and the app's origins
 *  are secure contexts. */
function mintId(): string {
  return crypto.randomUUID()
}

/** The folder above `dir` (`''` is the top, which has none). */
function parentOf(dir: string): string {
  const at = dir.lastIndexOf('/')
  return at < 0 ? '' : dir.slice(0, at)
}

/** A folder path as the vault spells it: `/`-separated, no slash at either end. */
function cleanDir(dir: string): string {
  return dir
    .split('/')
    .filter((part) => part !== '')
    .join('/')
}

/** Is `path` the folder `folder` or something inside it? */
function within(path: string, folder: string): boolean {
  return path === folder || path.startsWith(`${folder}/`)
}

/** `path` after the folder `from` became `to`. */
function moved(path: string, from: string, to: string): string {
  return within(path, from) ? to + path.slice(from.length) : path
}

/** When a file belongs on a timeline: taken, else modified -- `files.ts`'s
 *  `fileWhen`, which this module cannot import without a cycle. */
function whenOf(file: FileEntry): string {
  return file.media.takenAt ?? file.modifiedAt
}

/** Browse sorts what the disk listed here; the catalog's views are sorted by
 *  the vault. Ties are broken by id, in the same direction, as the vault does. */
function sortFiles(list: FileEntry[], sort: FileSort, descending: boolean): FileEntry[] {
  const sign = descending ? -1 : 1
  const by = (a: FileEntry, b: FileEntry): number => {
    switch (sort) {
      case 'name':
        return a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' })
      case 'size':
        return a.size - b.size
      case 'added':
        return a.addedAt < b.addedAt ? -1 : a.addedAt > b.addedAt ? 1 : 0
      case 'when': {
        const x = whenOf(a)
        const y = whenOf(b)
        return x < y ? -1 : x > y ? 1 : 0
      }
    }
  }
  const tie = (a: FileEntry, b: FileEntry) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)
  return [...list].sort((a, b) => sign * (by(a, b) || tie(a, b)))
}

/** `file` as it will be once `update` has been applied -- what is shown while
 *  the vault is asked. The vault's own answer replaces it. */
function applyUpdate(file: FileEntry, update: FileUpdate): FileEntry {
  const next: FileEntry = { ...file, media: { ...file.media } }
  if (update.caption !== undefined) next.caption = update.caption ?? ''
  let tags = next.tags ? [...next.tags] : []
  if (update.tags !== undefined) tags = update.tags ? [...update.tags] : []
  for (const t of update.addTags ?? []) if (!tags.includes(t)) tags.push(t)
  const dropTags = update.removeTags ?? []
  tags = tags.filter((t) => !dropTags.includes(t))
  if (update.tags !== undefined || update.addTags || update.removeTags) next.tags = tags
  if (update.rating !== undefined) {
    if (update.rating === null || update.rating <= 0) delete next.rating
    else next.rating = Math.min(5, Math.round(update.rating))
  }
  if (typeof update.favourite === 'boolean') next.favourite = update.favourite
  if (typeof update.hidden === 'boolean') next.hidden = update.hidden
  let people = next.people ? [...next.people] : []
  if (update.people !== undefined) people = update.people ? [...update.people] : []
  for (const p of update.addPeople ?? []) if (!people.includes(p)) people.push(p)
  const dropPeople = update.removePeople ?? []
  people = people.filter((p) => !dropPeople.includes(p))
  if (update.people !== undefined || update.addPeople || update.removePeople) next.people = people
  if (update.place !== undefined) {
    if (update.place) next.place = update.place
    else delete next.place
  }
  if (update.properties) {
    const properties = { ...next.properties }
    for (const [key, value] of Object.entries(update.properties)) {
      if (value === null) delete properties[key]
      else properties[key] = value
    }
    next.properties = properties
  }
  if (update.takenAt) next.media.takenAt = update.takenAt
  return next
}

class FilesState {
  // ---- status ----------------------------------------------------------
  status = $state<FilesStatus | null>(null)
  loaded = $state(false)
  loading = $state(false)
  error = $state<string | null>(null)

  // ---- where the person is --------------------------------------------
  view = $state<FilesView>('browse')
  /** Browse: the folder shown, relative to the files folder. */
  dir = $state('')
  layout = $state<FilesLayout>('grid')
  sort = $state<FileSort>('when')
  descending = $state(true)
  /** Narrows whatever is listed. */
  search = $state('')
  /** Albums: the album open, or null for the list of albums. */
  album = $state<AlbumId | null>(null)
  /** Photos: what the timeline is narrowed to. */
  facet = $state<PhotoFacet | null>(null)
  /** Show files marked hidden too. */
  showHidden = $state(false)

  // ---- what is loaded ----------------------------------------------------
  /** Browse: the folder's contents. */
  listing = $state<FolderListing | null>(null)
  /** Every other view: the files listed, page by page. */
  page = $state<FilePage | null>(null)
  albums = $state<AlbumView[]>([])
  facets = $state<FilesFacets | null>(null)

  // ---- selection and detail ---------------------------------------------
  selection = $state<FileId[]>([])
  /** The file the inspector describes. */
  focused = $state<FileId | null>(null)
  detail = $state<FileDetail | null>(null)
  /** The file open in the full-window viewer. */
  viewing = $state<FileId | null>(null)

  // ---- in flight ---------------------------------------------------------
  uploads = $state<UploadProgress[]>([])
  question = $state.raw<FilesQuestion | null>(null)

  // -- added after the skeleton ---------------------------------------------
  /** A rescan, or bringing old attachments in, is running. */
  busy = $state(false)

  // ---- inside documents ----------------------------------------------------
  /** What the search found inside documents -- `docs/plans/reading.md` -- one
   *  passage a row, with the file it is in. Empty while the search is shorter
   *  than `DOCUMENT_MIN_QUERY`, while indexing is off in Settings, and in the
   *  views that list pictures. */
  documentHits = $state.raw<DocumentFound[]>([])
  /** How many passages the index has for it in all. */
  documentTotal = $state(0)
  /** The index is still being built, so more may match later. */
  documentIndexing = $state(false)
  documentSearching = $state(false)
  /** How the documents' index stands; null until `refreshIndexStatus` asks. */
  indexStatus = $state.raw<DocIndexStatus | null>(null)

  /** Which load is the current one, per thing loaded. */
  #statusGen = latest()
  #viewGen = latest()
  #albumsGen = latest()
  #facetsGen = latest()
  #detailGen = latest()
  /** The token of the view load that is current, for `loadMore` to check. */
  #viewToken = 0
  /** View loads in flight, so `loading` lasts until the last answers. */
  #loads = 0
  /** A page is being appended. */
  #more = false
  /** What `listFolder` answered, before the search and hidden files narrow
   *  it into `listing`. */
  #raw: FolderListing | null = null
  /** Where a shift-click range starts. */
  #anchor: FileId | null = null
  /** The reload in flight, and whether another was asked for meanwhile. */
  #reloading: Promise<void> | null = null
  #again = false
  /** Bumped by `reset`, so an upload that outlives a lock stops quietly. */
  #epoch = 0
  #cancelled = new Set<string>()
  /** The vault's name for each upload under way, by row key. */
  #tickets = new Map<string, string>()
  /** Writes sent and not yet answered, so the close handshake can wait. */
  #writes = new Set<Promise<unknown>>()
  #searchDebounce = debounce(() => void this.#loadView(true, false), 250)
  #documentGen = latest()
  #documentDebounce = debounce(() => void this.#loadDocumentHits(false), 350)
  /** How many of the index's passages have been read, found in the catalog
   *  or not: where the next page starts. */
  #documentTaken = $state(0)

  /** Every file the lists hold, by id. */
  #listed = $derived.by(() => {
    const byId = new Map<FileId, FileEntry>()
    for (const f of this.listing?.files ?? []) byId.set(f.id, f)
    for (const f of this.page?.files ?? []) byId.set(f.id, f)
    return byId
  })

  /** Browse's files, in the order asked for. */
  #browsed = $derived.by(() => sortFiles(this.listing?.files ?? [], this.sort, this.descending))

  constructor() {
    // Registered once, here, as Screen time's are: the view calls `start` on
    // every mount.
    app.onLock(() => this.reset())
    app.onFlush(() => this.flush())
    this.view = viewPref.get()
    this.layout = layoutPref.get()
    this.sort = sortPref.get()
    this.descending = descendingPref.get()
  }

  // ---- derived -----------------------------------------------------------

  get supported(): boolean {
    return app.supportsFiles && this.status?.supported !== false
  }

  get folderState(): FolderState | null {
    return this.status?.state ?? null
  }

  /** The folder can be used now. */
  get available(): boolean {
    return this.status?.state.state === 'available'
  }

  /** This window is on the vault's machine: it may import by path and open
   *  files in other programs. */
  get local(): boolean {
    return this.status?.local === true
  }

  get settings(): FilesSettings | null {
    return this.status?.settings ?? null
  }

  /** What the main pane lists now: the folder's files, or the page's. */
  get visibleFiles(): FileEntry[] {
    if (this.view === 'browse') return this.#browsed
    if (this.view === 'albums' && this.album === null) return []
    return this.page?.files ?? []
  }

  /** There are more files than the page holds. */
  get hasMore(): boolean {
    if (this.view === 'browse' || (this.view === 'albums' && this.album === null)) return false
    const page = this.page
    return page !== null && page.files.length < page.total
  }

  /** The search found more inside documents than `documentHits` lists. */
  get hasMoreDocumentHits(): boolean {
    return this.#documentTaken < this.documentTotal
  }

  /** The files selected, as loaded. */
  get selectedFiles(): FileEntry[] {
    return this.selection.flatMap((id) => {
      const file = this.fileById(id)
      return file ? [file] : []
    })
  }

  get accent(): string {
    return FILES_ACCENT
  }

  /**
   * What the assistant is told is on screen -- see `onscreen.ts`.
   *
   * The pane, in the interface's words, with the folder's path when Browse
   * is in one; an album and the files open or selected, by id. Never a
   * file's name or caption, nor the name of a tag, a person, a place or a
   * camera -- they are the person's, and the service looks each reference up
   * again itself, behind the assistant's access setting.
   */
  get showing(): Showing {
    const facet = this.facet
    const facetWords: Record<PhotoFacet['kind'], string> = {
      tag: 'narrowed to one tag',
      person: 'narrowed to one person',
      place: 'narrowed to one place',
      year: `narrowed to the year ${facet?.name ?? ''}`,
      camera: 'narrowed to one camera',
    }
    const where: Record<FilesView, string> = {
      browse: this.dir
        ? `their files folder, in the folder ${this.dir}`
        : 'the top of their files folder',
      recent: 'the files added most recently',
      photos: 'their photos, by the date taken',
      albums: this.album ? 'one of their photo albums' : 'their photo albums',
      favourites: 'their favourite files',
      encrypted: 'their encrypted files, kept inside the vault',
      unused: 'files that nothing refers to any more',
      missing: 'files that have gone missing from the folder',
      trash: 'the files in the trash',
    }
    const words = [
      where[this.view],
      this.view === 'photos' && facet ? facetWords[facet.kind] : null,
      this.showHidden ? 'hidden files included' : null,
    ].filter(Boolean)
    const albumId = this.view === 'albums' ? this.album : null
    const albumName = this.albums.find((a) => a.album.id === albumId)?.album.name ?? ''
    const named = (id: FileId) => this.fileById(id)?.name ?? ''
    const ids = [
      ...new Set(
        [this.viewing, this.focused, ...this.selection].filter((id): id is FileId => id !== null),
      ),
    ]
    return {
      view: words.join(', '),
      within: seen('album', albumId, albumName),
      open: ids.flatMap((id) => seen('file', id, named(id))),
      query: this.search.trim() || null,
    }
  }

  /** The query the current view lists with (not browse). */
  get currentQuery(): FileQuery {
    const { sort, descending } = this.#orderOf(this.view)
    const query: FileQuery = {
      sort,
      descending,
      includeHidden: this.showHidden,
      limit: PAGE_SIZE,
      offset: 0,
    }
    const text = this.search.trim()
    if (text) query.text = text
    switch (this.view) {
      case 'browse':
        query.dir = this.dir
        break
      case 'recent':
        query.status = 'present'
        break
      case 'photos': {
        query.kinds = ['image', 'video']
        query.stackTopsOnly = true
        query.status = 'present'
        const facet = this.facet
        if (facet?.kind === 'tag') query.tag = facet.name
        else if (facet?.kind === 'person') query.person = facet.name
        else if (facet?.kind === 'place') query.place = facet.name
        else if (facet?.kind === 'year') {
          const year = Number(facet.name)
          if (Number.isInteger(year)) {
            query.from = new Date(year, 0, 1).toISOString()
            query.to = new Date(year + 1, 0, 1).toISOString()
          }
        } else if (facet?.kind === 'camera') {
          query.text = [text, facet.name].filter(Boolean).join(' ')
        }
        break
      }
      case 'albums':
        if (this.album) query.album = this.album
        break
      case 'favourites':
        query.favourite = true
        break
      case 'encrypted':
        query.storage = 'sealed'
        break
      case 'unused':
        query.unused = true
        break
      case 'missing':
        query.status = 'missing'
        break
      case 'trash':
        query.status = 'trashed'
        break
    }
    return query
  }

  /** The albums the search leaves in the Albums list. Added after the skeleton. */
  get visibleAlbums(): AlbumView[] {
    const words = this.search.toLowerCase().split(/\s+/).filter(Boolean)
    if (words.length === 0) return this.albums
    return this.albums.filter((a) => words.every((w) => a.album.name.toLowerCase().includes(w)))
  }

  /** Look a loaded file up by id. */
  fileById(id: FileId): FileEntry | undefined {
    const listed = this.#listed.get(id)
    if (listed) return listed
    if (this.detail?.file.id === id) return this.detail.file
    return this.albums.find((a) => a.cover?.id === id)?.cover
  }

  /**
   * The order a view lists in. Recent and Photos are their own order (when
   * added, when taken, newest first) whatever was chosen for the others.
   */
  #orderOf(view: FilesView): { sort: FileSort; descending: boolean } {
    if (view === 'recent') return { sort: 'added', descending: true }
    if (view === 'photos') return { sort: 'when', descending: true }
    return { sort: this.sort, descending: this.descending }
  }

  // ---- lifecycle -----------------------------------------------------------

  /** First visit: status, then whatever the view needs. */
  async start(): Promise<void> {
    if (!app.supportsFiles) return
    // Later visits read quietly over what is already drawn.
    if (this.loaded) return this.reload()
    await this.#loadStatus(false)
    await Promise.all([
      this.#loadView(false, false),
      this.#loadAlbums(true),
      this.#loadFacets(true),
    ])
    if (app.screen === 'main') this.loaded = true
  }

  /**
   * Reload what the view shows, quietly.
   *
   * One at a time: a scan raises a change for every file it touches, and a
   * reload asked for while one is in flight waits for it and runs once more
   * afterwards, however many were asked for.
   */
  reload(): Promise<void> {
    if (!app.supportsFiles || app.screen !== 'main') return Promise.resolve()
    if (this.#reloading) {
      this.#again = true
      return this.#reloading
    }
    const run = (async () => {
      try {
        do {
          this.#again = false
          // A folder not usable the last time is read again before what is in
          // it: the view would otherwise land on the old answer.
          const status = this.#loadStatus(true)
          if (!this.available) await status
          await Promise.all([
            status,
            this.#loadView(true, true),
            this.#loadAlbums(true),
            this.#loadFacets(true),
            this.focused ? this.#loadDetail(this.focused, true) : Promise.resolve(),
          ])
        } while (this.#again)
      } finally {
        this.#reloading = null
      }
    })()
    this.#reloading = run
    return run
  }

  /**
   * The vault locked or changed: forget everything.
   *
   * The pane, the layout and the order stay: they are chrome. What is in the
   * person's folder is exactly what a lock exists to put away. Uploads under
   * way stop at their next chunk, and what they had sent is the vault's to
   * sweep.
   */
  reset(): void {
    this.question = null
    for (const gen of [
      this.#statusGen,
      this.#viewGen,
      this.#albumsGen,
      this.#facetsGen,
      this.#detailGen,
    ]) {
      gen.next()
    }
    this.#searchDebounce.cancel()
    this.#documentGen.next()
    this.#documentDebounce.cancel()
    this.#documentTaken = 0
    this.documentHits = []
    this.documentTotal = 0
    this.documentIndexing = false
    this.documentSearching = false
    this.indexStatus = null
    this.#epoch += 1
    this.#cancelled.clear()
    this.#tickets.clear()
    this.#loads = 0
    this.#more = false
    this.#raw = null
    this.#anchor = null
    this.#again = false
    this.status = null
    this.loaded = false
    this.loading = false
    this.error = null
    this.dir = ''
    this.search = ''
    this.album = null
    this.facet = null
    this.showHidden = false
    this.listing = null
    this.page = null
    this.albums = []
    this.facets = null
    this.selection = []
    this.focused = null
    this.detail = null
    this.viewing = null
    this.uploads = []
    this.busy = false
  }

  /** Wait for anything outstanding. Part of the close handshake. */
  async flush(): Promise<void> {
    await Promise.allSettled([...this.#writes])
  }

  /** A `file` or `album` change arrived from elsewhere. Read again only while
   *  Files is the app on screen: it reads fresh when it is opened. */
  changed(): Promise<void> | undefined {
    // The reader is on the stage over whichever app is open, so the document
    // in it is looked at whether or not Files is the app on screen.
    const reading = reader.fileChanged()
    if (!this.loaded || app.screen !== 'main' || app.section !== 'files') return reading
    const reloading = this.reload()
    return reading ? Promise.all([reading, reloading]).then(() => undefined) : reloading
  }

  /** The app bar's "+": add files. */
  create(): void {
    if (!this.supported) return
    // Which picker depends on whether this is the vault's own computer, which
    // the status says; asked for from the palette before Files was ever
    // opened, there is none yet.
    if (this.status === null) {
      void this.#loadStatus(true).then(() => (this.status ? this.create() : undefined))
      return
    }
    const opts: AddOptions = {
      dir: this.view === 'browse' ? this.dir : undefined,
      encrypted: this.view === 'encrypted' ? true : undefined,
    }
    if (this.local) void this.pickAndImport(opts)
    else this.#pickInBrowser(opts)
  }

  // ---- navigation ----------------------------------------------------------

  setView(view: FilesView): void {
    const same = view === this.view
    // Choosing Albums again from inside an album is the way back to the list,
    // and choosing Photos again drops its narrowing.
    if (same && this.album === null && this.facet === null) return
    this.view = view
    viewPref.set(view)
    this.album = null
    this.facet = null
    this.#leave(!same)
  }

  openFolder(dir: string): void {
    const clean = cleanDir(dir)
    if (this.view === 'browse' && clean === this.dir) return
    this.view = 'browse'
    viewPref.set('browse')
    this.dir = clean
    this.album = null
    this.facet = null
    this.#leave(false)
  }

  /** Browse: the folder above. */
  up(): void {
    if (this.view !== 'browse' || this.dir === '') return
    this.openFolder(parentOf(this.dir))
  }

  openAlbum(id: AlbumId | null): void {
    if (this.view === 'albums' && this.album === id) return
    this.view = 'albums'
    viewPref.set('albums')
    this.album = id
    this.facet = null
    this.#leave(true)
  }

  setFacet(facet: PhotoFacet | null): void {
    if (this.view === 'photos' && this.#sameFacet(facet)) return
    this.view = 'photos'
    viewPref.set('photos')
    this.album = null
    this.facet = facet
    this.#leave(true)
  }

  setSearch(text: string): void {
    if (text === this.search) return
    this.search = text
    this.#searchDocuments()
    if (this.view === 'browse') {
      // The folder is already here: narrow it by name, with nothing to ask.
      this.#searchDebounce.cancel()
      this.#applyListing()
      this.#prune()
    } else if (this.view === 'albums' && this.album === null) {
      // The album list is narrowed in `visibleAlbums`.
      this.#searchDebounce.cancel()
    } else {
      this.#searchDebounce.call()
    }
  }

  setSort(sort: FileSort, descending: boolean): void {
    this.sort = sort
    this.descending = descending
    sortPref.set(sort)
    descendingPref.set(descending)
    // Browse sorts what it has; Recent and Photos keep their own order.
    if (this.view !== 'browse' && this.view !== 'recent' && this.view !== 'photos') {
      void this.#loadView(true, false)
    }
  }

  setLayout(layout: FilesLayout): void {
    this.layout = layout
    layoutPref.set(layout)
  }

  setShowHidden(on: boolean): void {
    if (on === this.showHidden) return
    this.showHidden = on
    if (this.view === 'browse') this.#applyListing()
    else void this.#loadView(true, false)
    if (this.view === 'photos') void this.#loadFacets(true)
  }

  async loadMore(): Promise<void> {
    const page = this.page
    if (!this.hasMore || !page || this.#more || !this.supported) return
    this.#more = true
    // What lands has to belong to the load that is current when it was asked.
    const token = this.#viewToken
    const view = this.view
    try {
      const next = await api.listFiles({ ...this.currentQuery, offset: page.files.length })
      if (!this.#viewGen.isCurrent(token) || view !== this.view || !this.page) return
      const have = new Set(this.page.files.map((f) => f.id))
      // The list may have moved under the offset; a file seen twice is shown once.
      this.page = {
        files: [...this.page.files, ...next.files.filter((f) => !have.has(f.id))],
        total: next.total,
      }
    } catch (e) {
      await quietly(e)
    } finally {
      this.#more = false
    }
  }

  // ---- documents -----------------------------------------------------------

  /** Open a document in the reader, at `options`' place or where they left
   *  off. The one way into it from Files; the reader is the store that does
   *  it. */
  read(id: FileId, options: ReaderOpenOptions = {}): Promise<void> {
    return reader.openFile(id, options)
  }

  /** The next passages the search found inside documents. */
  async moreDocumentHits(): Promise<void> {
    if (this.documentSearching || this.#documentTaken >= this.documentTotal) return
    await this.#loadDocumentHits(true)
  }

  /** How the documents' index stands now. Read when Settings is shown, and
   *  again after it was changed. */
  async refreshIndexStatus(): Promise<void> {
    if (!this.supported) return
    try {
      this.indexStatus = await api.documentsStatus()
    } catch (e) {
      await quietly(e)
    }
  }

  /** Empty the index and read every document into it again. */
  async reindexDocuments(): Promise<void> {
    if (!this.supported) return
    try {
      this.indexStatus = await this.#track(api.reindexDocuments())
    } catch (e) {
      await this.#fail(e, 'The documents could not be read again.')
    }
  }

  /** Whether what is typed can be looked for inside documents now. */
  #canSearchDocuments(): boolean {
    return (
      this.supported &&
      this.settings?.indexDocuments === true &&
      this.search.trim().length >= DOCUMENT_MIN_QUERY &&
      // Pictures are what these views list; a passage of a book has no place
      // among them.
      this.view !== 'photos' &&
      this.view !== 'albums'
    )
  }

  /** Look inside documents for what is typed, a beat after typing stops --
   *  or drop what was found when it no longer applies. */
  #searchDocuments(): void {
    this.#documentDebounce.cancel()
    if (this.#canSearchDocuments()) {
      this.#documentDebounce.call()
      return
    }
    this.#documentGen.next()
    this.#documentTaken = 0
    this.documentHits = []
    this.documentTotal = 0
    this.documentIndexing = false
    this.documentSearching = false
  }

  /** One page of what the index found, with the catalog's entry for each
   *  file: the index knows ids, and a list is drawn from names. A passage
   *  whose file is not in the catalog any more is left out. */
  async #loadDocumentHits(more: boolean): Promise<void> {
    if (!this.#canSearchDocuments()) return
    const text = this.search.trim()
    const token = this.#documentGen.next()
    const offset = more ? this.#documentTaken : 0
    this.documentSearching = true
    try {
      const found = await api.searchDocuments(text, DOCUMENT_HITS, offset)
      if (!this.#documentGen.isCurrent(token)) return
      const entries = await this.#entriesOf(found.hits.map((h) => h.file))
      if (!this.#documentGen.isCurrent(token)) return
      // One row per place: a page never repeats a passage the one before it
      // listed, but a title and a first passage can name the same block.
      const held = new Set(more ? this.documentHits.map(placeKey) : [])
      const rows = found.hits.flatMap((hit) => {
        const file = entries.get(hit.file)
        if (!file || held.has(placeKey({ file, hit }))) return []
        held.add(placeKey({ file, hit }))
        return [{ file, hit }]
      })
      this.documentHits = more ? [...this.documentHits, ...rows] : rows
      this.documentTotal = found.total
      this.documentIndexing = found.indexing
      // Where the index says it stopped, not how many came back: it leaves
      // out passages of files no longer indexed after it has counted them.
      this.#documentTaken = Math.max(found.nextOffset, offset + found.hits.length)
    } catch (e) {
      if (!this.#documentGen.isCurrent(token)) return
      if (!more) this.documentHits = []
      await quietly(e)
    } finally {
      if (this.#documentGen.isCurrent(token)) this.documentSearching = false
    }
  }

  /** The catalog's entries for `ids`: what is loaded already, and one read
   *  for the rest. */
  async #entriesOf(ids: FileId[]): Promise<Map<FileId, FileEntry>> {
    const entries = new Map<FileId, FileEntry>()
    const missing: FileId[] = []
    for (const id of new Set(ids)) {
      const known = this.fileById(id)
      if (known) entries.set(id, known)
      else missing.push(id)
    }
    if (missing.length > 0) {
      const page = await api.listFiles({
        ids: missing,
        status: 'present',
        includeHidden: true,
        limit: missing.length,
      })
      for (const file of page.files) entries.set(file.id, file)
    }
    return entries
  }

  // ---- selection -------------------------------------------------------------

  select(id: FileId, mode: SelectMode = 'replace'): void {
    const list = this.visibleFiles
    const anchor = this.#anchor
    if (mode === 'range' && anchor && list.some((f) => f.id === anchor)) {
      const a = list.findIndex((f) => f.id === anchor)
      const b = list.findIndex((f) => f.id === id)
      if (b < 0) return
      this.selection = list.slice(Math.min(a, b), Math.max(a, b) + 1).map((f) => f.id)
    } else if (mode === 'toggle') {
      this.selection = this.selection.includes(id)
        ? this.selection.filter((s) => s !== id)
        : [...this.selection, id]
      this.#anchor = id
    } else {
      this.selection = [id]
      this.#anchor = id
    }
    // The inspector follows the file just chosen, or the last still selected.
    this.focus(this.selection.includes(id) ? id : (this.selection.at(-1) ?? null))
  }

  selectAll(): void {
    this.selection = this.visibleFiles.map((f) => f.id)
    this.#anchor = this.selection[0] ?? null
    if (this.selection.length > 0 && (!this.focused || !this.selection.includes(this.focused))) {
      this.focus(this.selection[0] ?? null)
    }
  }

  clearSelection(): void {
    this.selection = []
    this.#anchor = null
    if (this.viewing === null) this.focus(null)
  }

  /** Describe this file in the inspector (loads `detail`). */
  focus(id: FileId | null): void {
    if (id === this.focused) return
    this.focused = id
    this.detail = null
    this.#detailGen.next()
    if (id) void this.#loadDetail(id, false)
  }

  /** Open the full-window viewer on a file. A PDF or an EPUB is passed on to
   *  the reader by `FilesView`, which sees the viewer open on one; this stays
   *  the one way into the viewer, which the arrow keys also step through. */
  open(id: FileId): void {
    this.viewing = id
    this.focus(id)
  }

  closeViewer(): void {
    this.viewing = null
  }

  /** The viewer's ←/→: the next or previous file in `visibleFiles`. */
  step(delta: -1 | 1): void {
    const list = this.visibleFiles
    const at = list.findIndex((f) => f.id === this.viewing)
    const next = at < 0 ? undefined : list[at + delta]
    if (next) this.open(next.id)
    // Near the end of a page, the next is on its way before it is wanted.
    if (at >= 0 && at + delta >= list.length - 3 && this.hasMore) void this.loadMore()
  }

  // ---- adding ------------------------------------------------------------------

  /** Pick files with the system picker and import them (local only). */
  async pickAndImport(opts: AddOptions = {}): Promise<FileEntry[]> {
    if (!this.supported || !this.local) return []
    try {
      const added = await this.#track(api.pickAndImportFiles(this.#addOptions(opts)))
      if (added.length > 0) await this.#afterAdding()
      return added
    } catch (e) {
      await handle(e)
      return []
    }
  }

  /** Import files by path: a drop on the desktop window (local only). */
  async importPaths(paths: string[], opts: AddOptions = {}): Promise<FileEntry[]> {
    if (!this.supported || paths.length === 0) return []
    if (!this.local) {
      // Paths name files on this computer, and the vault is on another.
      await handle(
        new VaultError(
          'unsupported',
          'Files can only be dropped in on the computer that holds the vault. Use Add files in the Files app instead.',
        ),
      )
      return []
    }
    try {
      const added = await this.#track(api.importFiles(paths, this.#addOptions(opts)))
      if (added.length > 0) await this.#afterAdding()
      return added
    } catch (e) {
      await handle(e)
      return []
    }
  }

  /** Upload files from the page (an input, a drop, a paste), in chunks, with
   *  progress in `uploads`. Answers what arrived. */
  async uploadFiles(list: File[], opts: AddOptions = {}): Promise<FileEntry[]> {
    if (!this.supported || list.length === 0) return []
    const epoch = this.#epoch
    const keyed = list.map((file) => ({ file, key: `upload-${++uploadCount}` }))
    this.uploads.push(
      ...keyed.map(({ file, key }): UploadProgress => ({
        key,
        name: file.name,
        size: file.size,
        sent: 0,
        state: 'sending',
      })),
    )
    const arrived: FileEntry[] = []
    // One file at a time: gentler on a phone's connection, and a row that has
    // not started says so.
    for (const { file, key } of keyed) {
      const entry = await this.#uploadOne(file, key, opts, epoch)
      if (entry) arrived.push(entry)
    }
    if (arrived.length > 0 && epoch === this.#epoch) await this.#afterAdding()
    return arrived
  }

  cancelUpload(key: string): void {
    const row = this.uploads.find((r) => r.key === key)
    if (!row || row.state !== 'sending') return
    row.state = 'cancelled'
    this.#cancelled.add(key)
    const upload = this.#tickets.get(key)
    // Before the vault has named the upload there is nothing to cancel there;
    // `#uploadOne` cancels it when the name arrives.
    if (upload) void api.cancelUpload(upload).catch(() => {})
  }

  /** Forget finished and failed rows. */
  clearUploads(): void {
    this.uploads = this.uploads.filter((r) => r.state === 'sending')
  }

  // ---- organising -----------------------------------------------------------

  /** Make a folder inside the one open in Browse; answers its path. */
  async createFolder(name: string): Promise<string | null> {
    const trimmed = name.trim()
    if (!trimmed || !this.supported) return null
    try {
      const inside = this.view === 'browse' ? this.dir : ''
      const path = await this.#track(api.createFolder(inside, trimmed))
      await this.reload()
      return path
    } catch (e) {
      await this.#fail(e, 'The folder could not be made.')
      return null
    }
  }

  async renameFile(id: FileId, name: string): Promise<void> {
    const trimmed = name.trim()
    const before = this.fileById(id)
    if (!trimmed || !before || trimmed === before.name || !this.supported) return
    const old = $state.snapshot(before)
    this.#replace({ ...old, name: trimmed })
    try {
      this.#replace(await this.#track(api.renameFile(id, trimmed)))
      await this.reload()
    } catch (e) {
      await this.#fail(e, 'The file could not be renamed.', () => this.#replace(old))
    }
  }

  async moveFiles(ids: FileId[], dir: string): Promise<void> {
    if (ids.length === 0 || !this.supported) return
    const to = cleanDir(dir)
    // Out of the folder on screen, it leaves the list at once.
    if (this.view === 'browse' && to !== this.dir) this.#dropLocal(ids)
    try {
      await this.#track(api.moveFiles(ids, to))
      await this.reload()
    } catch (e) {
      await this.#fail(e, 'The files could not be moved.', () => this.reload())
    }
  }

  async renameFolder(path: string, name: string): Promise<void> {
    const trimmed = name.trim()
    if (!trimmed || !this.supported) return
    try {
      const now = await this.#track(api.renameFolder(path, trimmed))
      // Inside the folder renamed, Browse follows it.
      this.dir = moved(this.dir, path, now)
      await this.reload()
    } catch (e) {
      await this.#fail(e, 'The folder could not be renamed.')
    }
  }

  async moveFolder(path: string, into: string): Promise<void> {
    if (!this.supported) return
    try {
      const now = await this.#track(api.moveFolder(path, cleanDir(into)))
      this.dir = moved(this.dir, path, now)
      await this.reload()
    } catch (e) {
      await this.#fail(e, 'The folder could not be moved.')
    }
  }

  /** Change what the person added; shown at once, put back if refused. */
  async update(ids: FileId[], update: FileUpdate): Promise<void> {
    if (ids.length === 0 || !this.supported) return
    const before = ids.flatMap((id) => {
      const file = this.fileById(id)
      return file ? [$state.snapshot(file)] : []
    })
    for (const file of before) this.#replace(applyUpdate(file, update))
    try {
      for (const file of await this.#track(api.updateFiles(ids, update))) this.#replace(file)
      await this.reload()
    } catch (e) {
      await this.#fail(e, 'The change could not be saved.', () => {
        for (const file of before) this.#replace(file)
      })
    }
  }

  async toggleFavourite(id: FileId): Promise<void> {
    const file = this.fileById(id)
    if (!file) return
    await this.update([id], { favourite: !file.favourite })
  }

  async setRating(ids: FileId[], rating: number): Promise<void> {
    await this.update(ids, { rating: Math.max(0, Math.min(5, Math.round(rating))) })
  }

  async trash(ids: FileId[]): Promise<void> {
    if (ids.length === 0 || !this.supported) return
    this.#dropLocal(ids)
    try {
      const trashed = await this.#track(api.trashFiles(ids))
      const back = trashed.map((f) => f.id)
      // The trash is the undo: no question, a way back instead.
      notify.info(`Moved ${plural(trashed.length, 'file')} to the trash.`, {
        key: 'files-trashed',
        action: { label: 'Undo', run: () => void this.restore(back) },
      })
      await this.reload()
    } catch (e) {
      await this.#fail(e, 'The files could not be moved to the trash.', () => this.reload())
    }
  }

  async trashFolder(path: string): Promise<void> {
    if (!this.supported) return
    try {
      const count = await this.#track(api.trashFolder(path))
      // Inside the folder trashed, Browse goes to the one above it.
      if (within(this.dir, path)) this.dir = parentOf(path)
      notify.info(
        count === 0
          ? 'The folder had no files in it. Nothing was moved to the trash.'
          : `Moved ${plural(count, 'file')} to the trash.`,
        { key: 'files-trashed' },
      )
      await this.reload()
    } catch (e) {
      await this.#fail(e, 'The folder could not be moved to the trash.')
    }
  }

  async restore(ids: FileId[]): Promise<void> {
    if (ids.length === 0 || !this.supported) return
    if (this.view === 'trash') this.#dropLocal(ids)
    try {
      await this.#track(api.restoreFiles(ids))
      await this.reload()
    } catch (e) {
      await this.#fail(e, 'The files could not be restored.', () => this.reload())
    }
  }

  /** Asks first. */
  deleteForever(ids: FileId[]): void {
    if (ids.length === 0) return
    const n = plural(ids.length, 'file')
    this.#ask({
      title: ids.length === 1 ? 'Delete this file for good?' : `Delete ${n} for good?`,
      body: 'They are removed from the trash and from the disk. This cannot be undone.',
      confirm: 'Delete',
      run: async () => {
        this.#dropLocal(ids)
        try {
          await this.#track(api.deleteFilesForever(ids))
          await this.reload()
        } catch (e) {
          await this.#fail(e, 'The files could not be deleted.', () => this.reload())
        }
      },
    })
  }

  /** Asks first. */
  emptyTrash(): void {
    const count = this.status?.usage.trashedFiles
    this.#ask({
      title: 'Empty the trash?',
      body:
        (count
          ? `${plural(count, 'file')} in the trash ${count === 1 ? 'is' : 'are'}`
          : 'Everything in the trash is') + ' deleted from the disk. This cannot be undone.',
      confirm: 'Empty the trash',
      run: async () => {
        try {
          await this.#track(api.emptyFilesTrash())
          await this.reload()
        } catch (e) {
          await this.#fail(e, 'The trash could not be emptied.')
        }
      },
    })
  }

  /** Asks first. */
  forgetMissing(ids: FileId[]): void {
    if (ids.length === 0) return
    this.#ask({
      title: ids.length === 1 ? 'Forget this missing file?' : `Forget ${ids.length} missing files?`,
      body:
        'What was added to them -- captions, tags, albums -- is deleted with them. ' +
        'Nothing on the disk is touched, and a file that comes back later is found as a new one.',
      confirm: 'Forget',
      run: async () => {
        this.#dropLocal(ids)
        try {
          await this.#track(api.forgetMissingFiles(ids))
          await this.reload()
        } catch (e) {
          await this.#fail(e, 'The files could not be forgotten.', () => this.reload())
        }
      },
    })
  }

  async setEncrypted(id: FileId, encrypted: boolean): Promise<void> {
    if (!this.supported) return
    try {
      this.#replace(await this.#track(api.setFileEncrypted(id, encrypted)))
      await this.reload()
    } catch (e) {
      await this.#fail(
        e,
        encrypted ? 'The file could not be encrypted.' : 'The file could not be decrypted.',
      )
    }
  }

  async stack(top: FileId, others: FileId[]): Promise<void> {
    if (others.length === 0 || !this.supported) return
    try {
      await this.#track(api.stackFiles(top, others))
      await this.reload()
    } catch (e) {
      await this.#fail(e, 'The files could not be stacked.')
    }
  }

  async unstack(ids: FileId[]): Promise<void> {
    if (ids.length === 0 || !this.supported) return
    try {
      await this.#track(api.unstackFiles(ids))
      await this.reload()
    } catch (e) {
      await this.#fail(e, 'The files could not be unstacked.')
    }
  }

  async openExternally(id: FileId): Promise<void> {
    if (!this.local) return
    try {
      await api.openFileExternally(id)
    } catch (e) {
      await this.#fail(e, 'The file could not be opened in another program.')
    }
  }

  async reveal(id: FileId): Promise<void> {
    if (!this.local) return
    try {
      await api.revealFile(id)
    } catch (e) {
      await this.#fail(e, 'The file could not be shown in the file manager.')
    }
  }

  /** Save copies elsewhere: the desktop asks where; a browser downloads. */
  async exportFiles(ids: FileId[]): Promise<void> {
    if (ids.length === 0) return
    try {
      const saved = hasShell ? await api.exportFiles(ids) : await this.#exportOrDownload(ids)
      if (saved > 0) notify.success(`Saved ${plural(saved, 'file')}.`, { key: 'files-saved' })
    } catch (e) {
      await this.#fail(e, 'The files could not be saved.')
    }
  }

  async restoreFromBackup(id: FileId): Promise<void> {
    if (!this.supported) return
    try {
      this.#replace(await this.#track(api.restoreFileFromBackup(id)))
      notify.success('Restored from the backup.', { key: 'files-restored' })
      await this.reload()
    } catch (e) {
      await this.#fail(e, 'The file could not be restored from the backup.')
    }
  }

  // ---- albums ------------------------------------------------------------------

  async createAlbum(name: string, files: FileId[] = []): Promise<Album | null> {
    const trimmed = name.trim()
    if (!trimmed || !this.supported) return null
    const now = new Date().toISOString()
    const made: Album = {
      id: mintId(),
      name: trimmed,
      description: '',
      createdAt: now,
      updatedAt: now,
    }
    try {
      const saved = await this.#track(api.saveAlbum(made))
      if (files.length > 0) await this.#track(api.addToAlbum(saved.id, files))
      await this.reload()
      return saved
    } catch (e) {
      await this.#fail(e, 'The album could not be made.', () => this.reload())
      return null
    }
  }

  async renameAlbum(album: Album, name: string): Promise<void> {
    const trimmed = name.trim()
    if (!trimmed || trimmed === album.name) return
    const renamed = { ...$state.snapshot(album), name: trimmed }
    await this.#saveAlbum(renamed, 'The album could not be renamed.')
  }

  async setAlbumCover(album: Album, cover: FileId): Promise<void> {
    const covered = { ...$state.snapshot(album), cover }
    await this.#saveAlbum(covered, 'The cover could not be changed.')
  }

  /** Asks first. */
  deleteAlbum(album: Album): void {
    const id = album.id
    this.#ask({
      title: `Delete “${album.name}”?`,
      body: 'The pictures stay where they are. Only the album itself goes.',
      confirm: 'Delete',
      run: async () => {
        this.albums = this.albums.filter((a) => a.album.id !== id)
        if (this.album === id) this.openAlbum(null)
        try {
          await this.#track(api.deleteAlbum(id))
          await this.reload()
        } catch (e) {
          await this.#fail(e, 'The album could not be deleted.', () => this.reload())
        }
      },
    })
  }

  async addToAlbum(id: AlbumId, files: FileId[]): Promise<void> {
    if (files.length === 0 || !this.supported) return
    try {
      await this.#track(api.addToAlbum(id, files))
      await this.reload()
    } catch (e) {
      await this.#fail(e, 'The pictures could not be added to the album.')
    }
  }

  async removeFromAlbum(id: AlbumId, files: FileId[]): Promise<void> {
    if (files.length === 0 || !this.supported) return
    // Out of the album on screen, they leave the list at once.
    if (this.view === 'albums' && this.album === id) this.#dropLocal(files)
    try {
      await this.#track(api.removeFromAlbum(id, files))
      await this.reload()
    } catch (e) {
      await this.#fail(e, 'The pictures could not be taken out of the album.', () => this.reload())
    }
  }

  // ---- settings ----------------------------------------------------------------

  async refreshStatus(): Promise<void> {
    await this.#loadStatus(false)
  }

  async saveSettings(settings: FilesSettings): Promise<void> {
    if (!this.supported) return
    const plain = $state.snapshot(settings)
    const before = this.status ? $state.snapshot(this.status) : null
    if (this.status) this.status.settings = plain
    this.#statusGen.next()
    try {
      this.status = await this.#track(api.saveFilesSettings(plain))
      // What is indexed is one of the settings, so the counts beside it are
      // read again -- when something has asked for them at all.
      if (this.indexStatus !== null) void this.refreshIndexStatus()
    } catch (e) {
      await this.#fail(e, 'The settings could not be saved.', () => {
        if (before) this.status = before
        return this.#loadStatus(true)
      })
    }
  }

  /** Set the folder up at `path`, or the default. */
  async setUpFolder(path?: string): Promise<void> {
    if (!this.supported) return
    this.busy = true
    this.#statusGen.next()
    try {
      this.status = await this.#track(api.setUpFilesFolder(path))
      await this.#afterFolderChanged()
    } catch (e) {
      await this.#fail(e, 'The folder could not be set up.')
    } finally {
      this.busy = false
    }
  }

  /** Pick a folder with the system picker and set it up (local only). */
  async chooseFolder(): Promise<void> {
    if (!this.local) return
    try {
      const path = await api.pickFilesFolder()
      if (path) await this.setUpFolder(path)
    } catch (e) {
      await this.#fail(e, 'The folder could not be chosen.')
    }
  }

  async rescan(): Promise<void> {
    if (!this.supported || this.busy) return
    this.busy = true
    try {
      const report = await this.#track(api.rescanFiles())
      const found = [
        report.added > 0 ? `${report.added} new` : null,
        report.changed > 0 ? `${report.changed} changed` : null,
        report.moved > 0 ? `${report.moved} moved` : null,
        report.returned > 0 ? `${report.returned} back` : null,
        report.missing > 0 ? `${report.missing} missing` : null,
      ].filter(Boolean)
      if (report.unavailable) {
        notify.warn('The files folder is not available.', {
          body: 'Nothing was changed. Is its drive mounted, or its volume unlocked?',
          key: 'files-scan',
        })
      } else if (report.heldBack) {
        notify.warn('So many files vanished at once that none were marked missing.', {
          body: 'That looks like a drive that is not mounted, not files that were deleted.',
          key: 'files-scan',
        })
      } else {
        const summary = found.length > 0 ? `Found ${found.join(', ')}.` : 'Nothing has changed.'
        notify.info(summary, { key: 'files-scan' })
      }
      await this.reload()
    } catch (e) {
      await this.#fail(e, 'The folder could not be scanned.')
    } finally {
      this.busy = false
    }
  }

  async bringInAttachments(): Promise<void> {
    if (!this.supported || this.busy) return
    this.busy = true
    try {
      const report = await this.#track(api.bringInAttachments())
      notify.success(
        report.files === 0
          ? 'There was nothing to bring in.'
          : `Brought in ${plural(report.files, 'file')} (${humanBytes(report.bytes)}).`,
        {
          body: report.errors?.length
            ? `${plural(report.errors.length, 'file')} could not be read.`
            : undefined,
          key: 'files-brought-in',
        },
      )
      await this.reload()
    } catch (e) {
      await this.#fail(e, 'The pictures could not be brought in.')
    } finally {
      this.busy = false
    }
  }

  // ---- confirming --------------------------------------------------------

  /**
   * The answer to the question on screen, which `App.svelte` calls from its
   * dialog's buttons. Added after the skeleton.
   *
   * The question comes down first, then its act runs, so a second click on
   * the same button does nothing.
   */
  async answer(yes: boolean): Promise<void> {
    const question = this.question
    this.question = null
    if (yes && question) await question.run()
  }

  /** Ask before something that cannot be taken back. One question at a time:
   *  a second replaces the first, which is answered "no". */
  #ask(q: Omit<FilesQuestion, 'danger'> & { danger?: boolean }): void {
    this.question = { danger: true, ...q }
  }

  // ---- loading ----------------------------------------------------------------

  /** Keep a write in view until it settles. See `flush`. */
  #track<T>(write: Promise<T>): Promise<T> {
    this.#writes.add(write)
    const done = () => this.#writes.delete(write)
    void write.then(done, done)
    return write
  }

  /** A failed write: say so, then put back what was shown ahead of it. A
   *  lock is not a failure, it is a screen to go to. */
  async #fail(e: unknown, what: string, revert?: () => unknown): Promise<void> {
    if (isLocked(e)) await handle(e)
    else notify.error(what, { body: errorMessage(e), key: 'files-failed' })
    await revert?.()
  }

  async #loadStatus(quiet: boolean): Promise<void> {
    if (!app.supportsFiles) return
    const token = this.#statusGen.next()
    try {
      const status = await api.filesStatus()
      if (this.#statusGen.isCurrent(token)) this.status = status
    } catch (e) {
      if (!this.#statusGen.isCurrent(token)) return
      if (quiet || isLocked(e)) return quietly(e)
      this.error = errorMessage(e)
    }
  }

  /**
   * What the pane on screen lists. `keep` asks for as many files as are
   * loaded, so a reload does not take a scrolled list back to its first page.
   */
  async #loadView(quiet: boolean, keep: boolean): Promise<void> {
    if (!app.supportsFiles || app.screen !== 'main') return
    const view = this.view
    const token = this.#viewGen.next()
    this.#viewToken = token
    // Nothing to list in a folder that has not been set up.
    if (this.folderState?.state === 'notSetUp') {
      this.#raw = null
      this.listing = null
      this.page = null
      this.loading = false
      return
    }
    if (!quiet) {
      this.#loads += 1
      this.loading = true
    }
    try {
      if (view === 'browse') {
        const dir = this.dir
        const listing = await api.listFolder(dir)
        if (!this.#viewGen.isCurrent(token)) return
        this.#raw = listing
        this.#applyListing()
        this.page = null
      } else if (view === 'albums' && this.album === null) {
        // The list itself is `#loadAlbums`'s; this only retires a page.
        this.page = null
      } else {
        const query = this.currentQuery
        if (keep) {
          query.limit = Math.min(Math.max(PAGE_SIZE, this.page?.files.length ?? 0), MAX_KEPT)
        }
        const page = await api.listFiles(query)
        if (!this.#viewGen.isCurrent(token)) return
        this.page = page
      }
      this.error = null
      this.#prune()
    } catch (e) {
      if (!this.#viewGen.isCurrent(token)) return
      const gone = e instanceof VaultError && e.code === 'not_found'
      if (view === 'browse' && this.dir !== '' && gone) {
        // The folder was renamed or deleted from somewhere else: one up.
        this.dir = parentOf(this.dir)
        return this.#loadView(quiet, false)
      }
      if (isLocked(e)) await handle(e)
      else if (quiet) await quietly(e)
      else this.error = errorMessage(e)
    } finally {
      if (!quiet) {
        this.#loads = Math.max(0, this.#loads - 1)
        this.loading = this.#loads > 0
      }
    }
  }

  async #loadAlbums(quiet: boolean): Promise<void> {
    if (!app.supportsFiles || app.screen !== 'main') return
    if (this.folderState?.state === 'notSetUp') return
    const token = this.#albumsGen.next()
    try {
      const albums = await api.listAlbums()
      if (this.#albumsGen.isCurrent(token)) this.albums = albums
    } catch (e) {
      if (quiet || isLocked(e)) await quietly(e)
      else await handle(e)
    }
  }

  /** The Photos sidebar's tags, people, places, years and cameras. */
  async #loadFacets(quiet: boolean): Promise<void> {
    if (this.view !== 'photos' || !app.supportsFiles || app.screen !== 'main') return
    if (this.folderState?.state === 'notSetUp') return
    const token = this.#facetsGen.next()
    try {
      const facets = await api.filesFacets()
      if (this.#facetsGen.isCurrent(token)) this.facets = facets
    } catch (e) {
      if (quiet || isLocked(e)) await quietly(e)
      else await handle(e)
    }
  }

  async #loadDetail(id: FileId, quiet: boolean): Promise<void> {
    if (!app.supportsFiles || app.screen !== 'main') return
    const token = this.#detailGen.next()
    try {
      const detail = await api.getFile(id)
      if (this.#detailGen.isCurrent(token) && this.focused === id) this.detail = detail
    } catch (e) {
      if (!this.#detailGen.isCurrent(token)) return
      if (e instanceof VaultError && e.code === 'not_found') {
        // Gone while it was being looked at: the inspector has nothing to say.
        this.detail = null
        this.focused = null
        this.selection = this.selection.filter((s) => s !== id)
        return
      }
      if (quiet || isLocked(e)) await quietly(e)
      else await handle(e)
    }
  }

  /** After a folder, view or album was chosen: put away what belonged to the
   *  last, and read the new. `clear` empties the page so one view's files are
   *  not drawn under another's heading; Browse keeps the folder it had until
   *  the next one lands. */
  #leave(clear: boolean): void {
    this.#searchDebounce.cancel()
    this.selection = []
    this.#anchor = null
    this.focus(null)
    if (clear) this.page = null
    // A search typed in one view goes on into the next, so what it found
    // inside documents is asked again -- or dropped, in a view of pictures.
    if (this.search.trim() !== '') this.#searchDocuments()
    void this.#loadView(false, false)
    void this.#loadAlbums(true)
    void this.#loadFacets(true)
  }

  #sameFacet(facet: PhotoFacet | null): boolean {
    const now = this.facet
    if (now === null || facet === null) return now === facet
    return now.kind === facet.kind && now.name === facet.name
  }

  /** Browse's listing, narrowed by the search and the hidden files. */
  #applyListing(): void {
    const raw = this.#raw
    if (!raw) {
      this.listing = null
      return
    }
    const words = this.search.toLowerCase().split(/\s+/).filter(Boolean)
    const named = (name: string) => words.every((w) => name.toLowerCase().includes(w))
    this.listing = {
      ...raw,
      folders: words.length > 0 ? raw.folders.filter((f) => named(f.name)) : raw.folders,
      files: raw.files.filter(
        (f) => (this.showHidden || !f.hidden) && (words.length === 0 || named(f.name)),
      ),
    }
  }

  /** Drop what is selected or focused that a load no longer lists. */
  #prune(): void {
    if (this.selection.some((id) => !this.#listed.has(id))) {
      this.selection = this.selection.filter((id) => this.#listed.has(id))
    }
    if (this.focused && !this.#listed.has(this.focused) && this.viewing !== this.focused) {
      this.focus(this.selection.at(-1) ?? null)
    }
  }

  /** Swap a file's entry for a newer one wherever it is held. */
  #replace(entry: FileEntry): void {
    const swap = (list: FileEntry[] | undefined) => {
      const at = list?.findIndex((f) => f.id === entry.id) ?? -1
      if (list && at >= 0) list[at] = entry
    }
    swap(this.#raw?.files)
    swap(this.listing?.files)
    swap(this.page?.files)
    if (this.detail?.file.id === entry.id) this.detail.file = entry
    for (const view of this.albums) if (view.cover?.id === entry.id) view.cover = entry
  }

  /** Take files off the lists on screen, ahead of the vault's answer. The
   *  viewer moves to a neighbour, or closes, if its file is one of them. */
  #dropLocal(ids: FileId[]): void {
    const gone = new Set(ids)
    const keep = (f: FileEntry) => !gone.has(f.id)
    if (this.viewing && gone.has(this.viewing)) {
      const list = this.visibleFiles
      const at = list.findIndex((f) => f.id === this.viewing)
      const next = [...list.slice(at + 1), ...list.slice(0, Math.max(0, at)).reverse()].find(keep)
      this.viewing = next && at >= 0 ? next.id : null
    }
    if (this.#raw) this.#raw = { ...this.#raw, files: this.#raw.files.filter(keep) }
    if (this.listing) this.listing.files = this.listing.files.filter(keep)
    if (this.page) {
      const before = this.page.files.length
      this.page.files = this.page.files.filter(keep)
      this.page.total = Math.max(0, this.page.total - (before - this.page.files.length))
    }
    this.selection = this.selection.filter((id) => !gone.has(id))
    if (this.focused && gone.has(this.focused)) this.focus(this.viewing)
  }

  /** What every way of adding needs of a caller's options. */
  #addOptions(opts: AddOptions): { dir?: string; origin: FileOrigin; encrypted?: boolean } {
    return {
      ...(opts.dir !== undefined ? { dir: cleanDir(opts.dir) } : {}),
      origin: opts.origin ?? 'files',
      ...(opts.encrypted !== undefined ? { encrypted: opts.encrypted } : {}),
    }
  }

  /** After files arrived: the status's counts and whatever is listed. */
  async #afterAdding(): Promise<void> {
    if (this.loaded) await this.reload()
  }

  async #afterFolderChanged(): Promise<void> {
    this.dir = ''
    this.selection = []
    this.focus(null)
    this.#raw = null
    this.listing = null
    this.page = null
    await Promise.all([
      this.#loadView(false, false),
      this.#loadAlbums(true),
      this.#loadFacets(true),
    ])
  }

  /** An album saved, shown at once and put back if refused. */
  async #saveAlbum(album: Album, what: string): Promise<void> {
    const next: Album = { ...album, updatedAt: new Date().toISOString() }
    const before = $state.snapshot(this.albums)
    this.albums = this.albums.map((a) => (a.album.id === next.id ? { ...a, album: next } : a))
    try {
      await this.#track(api.saveAlbum(next))
      await this.reload()
    } catch (e) {
      await this.#fail(e, what, () => {
        this.albums = before
      })
    }
  }

  // ---- uploading ---------------------------------------------------------------

  /**
   * One file, in chunks. Answers the file as the vault made it, or `null`
   * when it was cancelled or failed -- its row says which.
   *
   * A chunk that fails is sent once more from what the vault says it has
   * (`received`), after a short wait: a connection that dropped for a moment
   * should not cost a gigabyte film its place. A second failure is the end.
   */
  async #uploadOne(
    file: File,
    key: string,
    opts: AddOptions,
    epoch: number,
  ): Promise<FileEntry | null> {
    const row = () => this.uploads.find((r) => r.key === key)
    const stopped = () => this.#cancelled.has(key) || epoch !== this.#epoch
    let upload: string | null = null
    try {
      const ticket = await api.beginUpload(file.name, file.size, this.#addOptions(opts))
      upload = ticket.upload
      this.#tickets.set(key, upload)
      if (stopped()) {
        void api.cancelUpload(upload).catch(() => {})
        return null
      }
      let received = ticket.received
      const first = row()
      if (first) first.sent = received
      let retried = false
      while (received < file.size) {
        const end = Math.min(received + UPLOAD_CHUNK, file.size)
        const bytes = new Uint8Array(await file.slice(received, end).arrayBuffer())
        let next: number
        try {
          next = await api.uploadChunk(upload, received, bytes)
        } catch (e) {
          if (isLocked(e) || stopped() || retried) throw e
          retried = true
          await new Promise((resolve) => setTimeout(resolve, RETRY_AFTER_MS))
          continue
        }
        if (stopped()) return null
        if (next <= received) throw new VaultError('io', 'The upload did not move on.')
        retried = false
        received = next
        const r = row()
        if (r) r.sent = Math.min(received, file.size)
      }
      const entry = await api.finishUpload(upload)
      const done = row()
      if (done) {
        done.state = 'done'
        done.sent = file.size
        done.file = entry
      }
      return entry
    } catch (e) {
      if (stopped()) return null
      const failed = row()
      if (failed) {
        failed.state = 'failed'
        failed.error = errorMessage(e)
      }
      // Half a file is the vault's to sweep now, not to keep.
      if (upload) void api.cancelUpload(upload).catch(() => {})
      if (isLocked(e)) await handle(e)
      return null
    } finally {
      this.#tickets.delete(key)
      this.#cancelled.delete(key)
    }
  }

  /** The browser's picker, for a window that cannot import by path. */
  #pickInBrowser(opts: AddOptions): void {
    if (typeof document === 'undefined') return
    const input = document.createElement('input')
    input.type = 'file'
    input.multiple = true
    input.onchange = () => {
      const chosen = [...(input.files ?? [])]
      if (chosen.length > 0) void this.uploadFiles(chosen, opts)
    }
    input.click()
  }

  /** The shell's save dialog where there is one; a download per file where
   *  there is not (a browser on web access). */
  async #exportOrDownload(ids: FileId[]): Promise<number> {
    try {
      return await api.exportFiles(ids)
    } catch (e) {
      if (isLocked(e) || typeof document === 'undefined') throw e
    }
    let n = 0
    for (const id of ids) {
      const file = this.fileById(id)
      if (!file) continue
      const link = document.createElement('a')
      link.href = fileUrl(id, fileVersion(file))
      link.download = file.name
      link.click()
      n += 1
    }
    return n
  }
}

export const files = new FilesState()
