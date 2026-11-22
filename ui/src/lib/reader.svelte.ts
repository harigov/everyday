// The reader: one document open on the stage -- a PDF drawn by pdf.js, or an
// EPUB, Markdown, text or web page drawn block by block from what the service
// parsed -- with its contents, a search inside it, and the assistant's rail
// beside it. Opened from Files and from a library item; see
// `docs/plans/reading.md`.
//
// This store is the one place that knows what is open and where in it they
// are. The views report what is in view (`reportView`) and obey jump requests
// (`target`); everything else -- the outline, the search, the screen context,
// saving their place -- reads and writes here.
//
// # Where somebody is
//
// A place is a page and, for anything but a PDF, a block: the index of a
// paragraph, a heading or a picture in the document's reading order. The
// views mark every block they draw with `data-block`, and that index is the
// same one the assistant reads by and a search hit names -- see
// `karmayoga-core/src/documents.rs`. What the assistant is told is on screen
// (`at`) is that place, never the words there.
//
// # Rules that shape it
//
//   1. One document at a time, and only its newest open lands. Opening is two
//      round trips, and somebody who clicks through three citations quickly
//      must end up in the third, whichever answer arrives last.
//   2. Their place is written to the file's properties while they read, but
//      not on every scroll event: at most once in `SAVE_EVERY_MS`, and
//      whatever is left over when the document is closed, another is opened,
//      or the window is told to flush. Writing it can never be a reason for
//      the reader to fail, so it never throws.
//   3. The place the view reports is the place the assistant is told. A jump
//      (`goTo`) moves it at once, ahead of the view's first report, so a
//      message sent in the moment between the two names where they asked to
//      be and not where they were.
//   4. Nothing outlives a lock. A document's words are what a lock exists to
//      put away, and a chapter's pictures are the heaviest thing here.

import { agent } from './agent.svelte'
import { api } from './api'
import { chapterOfBlock, clampPage, pageOfBlock, sectionOfBlock, sectionTrail } from './documents'
import { library } from './library.svelte'
import { notify } from './notify.svelte'
import { seen, type Showing } from './onscreen'
import { panels } from './panels.svelte'
import { app, errorMessage, handle, isLocked, quietly, type Section } from './state.svelte'
import { latest } from './store/latest'
import type {
  DocChapterView,
  DocFormat,
  DocHit,
  DocOutline,
  DocSection,
  FileEntry,
  FileId,
  ItemId,
  ReaderAt,
  ReaderSelection,
} from './types'
import { MAX_SELECTION_QUOTE, VaultError } from './types'

/** Which pane is open beside the document. */
export type ReaderPanel = 'outline' | 'search' | null

/** Where to open, or jump to. A section is an index into `outline.sections`. */
export interface ReaderPlaceRequest {
  page?: number
  block?: number
  section?: number
}

export interface ReaderOpenOptions extends ReaderPlaceRequest {
  /** The library item it was opened from, if any: named on screen. */
  item?: ItemId | null
}

/** A jump the open view should make. `seq` changes on every request, so the
 *  same place asked for twice is obeyed twice. */
export interface ReaderTarget {
  page: number
  block?: number | null
  seq: number
}

/** The longest between two writes of their place while they read. */
const SAVE_EVERY_MS = 5000

/** How many places the search pane lists. */
const SEARCH_LIMIT = 30

/** A page, and the block at the top of it where the document has blocks. */
interface Place {
  page: number
  block: number | null
}

/**
 * Turn a request into a place in `outline`.
 *
 * A section is its own block and page; a block is on the page a hit or a
 * loaded chapter says, else the page of the section it falls under, else
 * `fallback` -- the page they are on, which is the one thing never wrong to
 * keep; a page is just that. A block that is not in the document, or is
 * named for a PDF (whose pages pdf.js draws, so no block is ever in view), is
 * left out rather than believed.
 */
function placeOf(
  outline: DocOutline,
  want: ReaderPlaceRequest,
  fallback: number,
  chapter: DocChapterView | null,
): Place {
  const blocky = outline.format !== 'pdf'
  const clamp = (page: number) => clampPage(page, outline.pages)
  if (want.section !== undefined) {
    const section = outline.sections[want.section]
    if (section) return { page: clamp(section.page), block: blocky ? section.block : null }
  }
  const block = want.block
  if (block !== undefined && Number.isInteger(block) && block >= 0 && block < outline.blocks) {
    const page = want.page ?? pageOfBlock(outline, block, chapter) ?? fallback
    return { page: clamp(page), block: blocky ? block : null }
  }
  return { page: clamp(want.page ?? fallback), block: null }
}

/** A selection as the service will take it: a quote of at most
 *  `MAX_SELECTION_QUOTE` words, and nothing at all for one with nothing in it. */
function clipSelection(selection: ReaderSelection): ReaderSelection | null {
  const words = (selection.quote ?? '').trim().split(/\s+/).filter(Boolean)
  const quote = words.slice(0, MAX_SELECTION_QUOTE).join(' ')
  const blocks = selection.from != null || selection.to != null
  if (!blocks && quote === '') return null
  const clipped: ReaderSelection = { page: selection.page }
  if (selection.from != null) clipped.from = selection.from
  if (selection.to != null) clipped.to = selection.to
  if (quote !== '') clipped.quote = quote
  return clipped
}

/** The apps the reader is drawn in. */
const HOSTS: readonly Section[] = ['files', 'library']

class ReaderState {
  /** Whether a document is open in the reader. */
  open = $state(false)
  /**
   * The app the reader belongs to: Files, or the Library for a book opened
   * from its shelf. It is drawn on that app's stage and nowhere else, so
   * choosing Mail shows Mail and coming back to Files finds the book where
   * it was. Opened from anywhere else -- a citation in the rail over Mail --
   * it takes the reader to Files.
   */
  host = $state<Section>('files')
  /** The file open, as the catalog has it. */
  file = $state.raw<FileEntry | null>(null)
  /** What the service read of it; null while loading or after a failure. */
  outline = $state.raw<DocOutline | null>(null)
  loading = $state(false)
  /** Why it could not be opened or read. */
  error = $state<string | null>(null)
  /** The library item it was opened from. */
  item = $state<ItemId | null>(null)

  /** The page at the top of the view, from 1. */
  page = $state(1)
  /** The last page at least partly in view, when it is not `page`. */
  lastPage = $state<number | null>(null)
  /** The first block in view (anything but a PDF). */
  block = $state<number | null>(null)
  /** What they have selected, if anything. */
  selection = $state.raw<ReaderSelection | null>(null)

  /** Chapters loaded so far, by index. */
  chapters = $state.raw<Record<number, DocChapterView>>({})
  /** The jump the view should make next, if any. */
  target = $state.raw<ReaderTarget | null>(null)

  /** The pane beside the document. */
  panel = $state<ReaderPanel>('outline')
  /** What is typed into the search pane. */
  query = $state('')
  hits = $state.raw<DocHit[]>([])
  searching = $state(false)

  // -- added after the skeleton -----------------------------------------
  /** Why a chapter could not be read, by index. Cleared when it is asked
   *  for again; a chapter that is neither here nor in `chapters` is still
   *  loading. */
  chapterErrors = $state.raw<Record<number, string>>({})

  /** Which open is the current one, and the one it handed out. */
  #gen = latest()
  #token = 0
  #searchGen = latest()
  /** The last `target.seq` handed out. Never reset: a jump asked for in a
   *  document opened a moment later must not look like the one before it. */
  #seq = 0
  /** Chapter loads under way, so two views asking for one ask once. */
  #loads = new Map<number, Promise<DocChapterView | null>>()
  /** Their place has moved since it was last written. */
  #unsaved = false
  /** The place last written, or the one the document opened on. */
  #written: Place | null = null
  #saveTimer: ReturnType<typeof setTimeout> | null = null
  /** Writes of their place sent and not yet answered, for the close handshake. */
  #writes = new Set<Promise<unknown>>()

  constructor() {
    // Registered once, here, as the other stores' are.
    app.onLock(() => this.reset())
    app.onFlush(() => this.flush())
  }

  // ---- opening and closing -------------------------------------------------

  /** Open `id` in the reader: its outline loaded, at `options`' place or
   *  where they left off. Replaces whatever was open -- except when it is
   *  the file already open, which is a jump: a citation pressed in the rail
   *  should take them there, not draw the whole book again. */
  async openFile(id: FileId, options: ReaderOpenOptions = {}): Promise<void> {
    if (this.open && this.outline && this.file?.id === id) {
      if (options.item !== undefined) this.item = options.item
      this.goTo(options)
      return
    }
    await this.#load(id, options)
  }

  /** Close the reader, saving their place. */
  close(): void {
    this.#writePlace()
    this.#forget()
    this.open = false
  }

  /** The vault locked or changed: forget everything, writing nothing -- a
   *  locked vault would refuse it. The pane stays, as chrome. */
  reset(): void {
    this.#forget()
    this.open = false
  }

  /** Write anything outstanding now. Part of the close handshake. */
  async flush(): Promise<void> {
    this.#writePlace()
    await Promise.allSettled([...this.#writes])
  }

  async #load(id: FileId, options: ReaderOpenOptions): Promise<void> {
    // Where they were in whatever is being left is kept first.
    this.#writePlace()
    this.#forget()
    const token = (this.#token = this.#gen.next())
    this.host = HOSTS.includes(app.section) ? app.section : 'files'
    if (app.section !== this.host) void app.goTo(this.host)
    this.open = true
    this.loading = true
    this.item = options.item ?? null

    // Both at once. When the outline cannot be had the file still can, and
    // the view names what could not be read.
    const [entry, read] = await Promise.allSettled([api.getFile(id), api.documentOutline(id)])
    if (!this.#gen.isCurrent(token)) return
    if (entry.status === 'fulfilled') this.file = entry.value.file

    if (entry.status === 'rejected' || read.status === 'rejected') {
      const failure: unknown =
        entry.status === 'rejected' ? entry.reason : read.status === 'rejected' ? read.reason : null
      this.loading = false
      if (isLocked(failure)) return void (await handle(failure))
      this.error = errorMessage(failure)
      return
    }

    const outline = read.value
    const asked =
      options.section !== undefined || options.block !== undefined || options.page !== undefined
    const left = outline.position
    const want: ReaderPlaceRequest = asked
      ? options
      : left
        ? { page: left.page, block: left.block ?? undefined }
        : {}
    const place = placeOf(outline, want, 1, null)
    this.outline = outline
    this.page = place.page
    this.block = place.block
    this.lastPage = null
    this.#written = place
    this.target = { ...place, seq: ++this.#seq }
    this.loading = false
    // The chapter they land in is wanted at once; the view asks for it too,
    // and the two asks are one load.
    if (place.block !== null) this.#warm(outline, place.block)
  }

  /** Put away everything about the open document, and make anything still on
   *  its way to it land nowhere. */
  #forget(): void {
    this.#token = this.#gen.next()
    this.#searchGen.next()
    this.#loads.clear()
    if (this.#saveTimer !== null) clearTimeout(this.#saveTimer)
    this.#saveTimer = null
    this.#unsaved = false
    this.#written = null
    this.file = null
    this.outline = null
    this.loading = false
    this.error = null
    this.item = null
    this.page = 1
    this.lastPage = null
    this.block = null
    this.selection = null
    this.chapters = {}
    this.chapterErrors = {}
    this.target = null
    this.query = ''
    this.hits = []
    this.searching = false
  }

  // ---- reading -------------------------------------------------------------

  /** The chapter at `index`, loaded once and kept. */
  async loadChapter(index: number): Promise<DocChapterView | null> {
    const file = this.file
    const outline = this.outline
    if (!this.open || !file || !outline) return null
    if (!Number.isInteger(index) || index < 0 || index >= outline.chapters.length) return null
    const have = this.chapters[index]
    if (have) return have
    const running = this.#loads.get(index)
    if (running) return running

    const token = this.#token
    if (this.chapterErrors[index] !== undefined) {
      const rest = { ...this.chapterErrors }
      delete rest[index]
      this.chapterErrors = rest
    }
    const run = (async (): Promise<DocChapterView | null> => {
      try {
        const view = await api.documentChapter(file.id, index)
        if (!this.#gen.isCurrent(token)) return null
        this.chapters = { ...this.chapters, [index]: view }
        return view
      } catch (e) {
        if (!this.#gen.isCurrent(token)) return null
        if (isLocked(e)) await handle(e)
        else this.chapterErrors = { ...this.chapterErrors, [index]: errorMessage(e) }
        return null
      } finally {
        // Only its own: after a close and another open, the map is the new
        // document's and this index may be somebody else's load.
        if (this.#gen.isCurrent(token)) this.#loads.delete(index)
      }
    })()
    this.#loads.set(index, run)
    return run
  }

  /** Start reading the chapter `block` is in, if it is not loaded. */
  #warm(outline: DocOutline, block: number): void {
    const chapter = chapterOfBlock(outline, block)
    if (chapter !== null) void this.loadChapter(chapter)
  }

  /** Ask the view to show a place. */
  goTo(place: ReaderPlaceRequest): void {
    const outline = this.outline
    const asked =
      place.section !== undefined || place.block !== undefined || place.page !== undefined
    if (!this.open || !outline || !asked) return
    const at = place.block === undefined ? null : chapterOfBlock(outline, place.block)
    const chapter = at === null ? null : (this.chapters[at] ?? null)
    const there = placeOf(outline, place, this.page, chapter)
    this.#moveTo(there)
    this.target = { ...there, seq: ++this.#seq }
    if (there.block !== null) this.#warm(outline, there.block)
  }

  /** What the view has in front of it now; called as it scrolls. Saves
   *  their place now and then. */
  reportView(at: { page: number; lastPage?: number | null; block?: number | null }): void {
    const outline = this.outline
    if (!this.open || !outline) return
    const page = clampPage(at.page, outline.pages)
    const lastPage = at.lastPage == null ? null : clampPage(at.lastPage, outline.pages)
    const block =
      outline.format === 'pdf' || at.block == null || !(at.block >= 0) ? null : Math.trunc(at.block)
    this.#moveTo({ page, block }, lastPage !== null && lastPage > page ? lastPage : null)
  }

  /** What they selected, or null when the selection went. */
  setSelection(selection: ReaderSelection | null): void {
    this.selection = selection === null ? null : clipSelection(selection)
  }

  /** Record that they are at `place`, and see that it is written. Assigns
   *  only what differs, so a view that reports on every scroll event wakes
   *  nothing when the report is the one it just made. */
  #moveTo(place: Place, lastPage: number | null = null): void {
    if (this.page !== place.page) this.page = place.page
    if (this.block !== place.block) this.block = place.block
    if (this.lastPage !== lastPage) this.lastPage = lastPage
    const written = this.#written
    if (written && (written.page !== place.page || written.block !== place.block)) {
      this.#unsaved = true
      // One write in `SAVE_EVERY_MS` while they read: a timer started by the
      // first move, not restarted by the rest, so a long unbroken read is
      // saved as it goes and not only when it stops.
      this.#saveTimer ??= setTimeout(() => {
        this.#saveTimer = null
        this.#writePlace()
      }, SAVE_EVERY_MS)
    }
  }

  /** Write their place now, if it moved since it was last written. Never
   *  throws: a place that cannot be saved is not a reason to stop reading. */
  #writePlace(): void {
    if (this.#saveTimer !== null) clearTimeout(this.#saveTimer)
    this.#saveTimer = null
    const file = this.file
    const outline = this.outline
    if (!this.#unsaved || !file || !outline) return
    this.#unsaved = false
    const place: Place = { page: this.page, block: outline.format === 'pdf' ? null : this.block }
    this.#written = place
    const write = api
      .saveReaderPosition(file.id, place.page, place.block)
      .then(
        () => undefined,
        (e: unknown) => quietly(e),
      )
      .catch(() => undefined)
    this.#writes.add(write)
    void write.finally(() => this.#writes.delete(write))
  }

  // ---- searching -----------------------------------------------------------

  /** Search inside the open document; empty clears. */
  async search(query: string): Promise<void> {
    this.query = query
    const text = query.trim()
    const file = this.file
    const token = this.#searchGen.next()
    if (!this.open || !file || text === '') {
      this.hits = []
      this.searching = false
      return
    }
    this.searching = true
    try {
      const hits = await api.searchDocument(file.id, text, SEARCH_LIMIT)
      if (this.#searchGen.isCurrent(token)) this.hits = hits
    } catch (e) {
      if (!this.#searchGen.isCurrent(token)) return
      this.hits = []
      await quietly(e)
    } finally {
      if (this.#searchGen.isCurrent(token)) this.searching = false
    }
  }

  // ---- the rail --------------------------------------------------------------

  /** Open the assistant's rail beside the document, ready to ask. */
  ask(): void {
    if (!agent.supported) return
    void (async () => {
      try {
        if (!agent.open) await agent.toggle()
        await agent.ensureLoaded()
        agent.focusComposer()
      } catch (e) {
        await quietly(e)
      }
    })()
  }

  // ---- from elsewhere ----------------------------------------------------------

  /**
   * A `file` change arrived (`files.changed` hands it here): if it touched
   * the document open, look at it again. Gone from the folder, it is closed
   * with a word; written to by another program, it is read again where they
   * are; renamed or otherwise changed, only its entry is swapped.
   *
   * Their own place being written raises one of these on another window, so
   * nothing here may write in answer.
   */
  fileChanged(): Promise<void> | undefined {
    const file = this.file
    if (!this.open || !file || this.loading) return undefined
    return this.#refresh(file)
  }

  async #refresh(was: FileEntry): Promise<void> {
    const token = this.#token
    try {
      const { file: now } = await api.getFile(was.id)
      if (!this.#gen.isCurrent(token)) return
      if (now.status.state !== 'present') return this.#gone(now.name)
      if (now.hash !== '' && was.hash !== '' && now.hash !== was.hash) {
        const block = this.block ?? undefined
        await this.#load(now.id, { page: this.page, block, item: this.item })
        return
      }
      if (now.updatedAt !== was.updatedAt) this.file = now
    } catch (e) {
      if (!this.#gen.isCurrent(token)) return
      if (e instanceof VaultError && e.code === 'not_found') return this.#gone(was.name)
      await quietly(e)
    }
  }

  #gone(name: string): void {
    notify.info(`${name} is no longer in the files folder.`, { key: 'reader-gone' })
    this.close()
  }

  // ---- derived ---------------------------------------------------------------------

  /** Whether the reader is what is on the stage now: open, and its app the
   *  one in front -- with Settings, drawn in that app's place, closed. */
  get shown(): boolean {
    return this.open && app.section === this.host && panels.settings === null
  }

  /** Where they are, for the assistant; null when nothing is open. */
  get at(): ReaderAt | null {
    const file = this.file
    if (!this.open || !file || !this.outline) return null
    const at: ReaderAt = { file: file.id, page: this.page }
    if (this.lastPage !== null && this.lastPage !== this.page) at.lastPage = this.lastPage
    if (this.block !== null) at.block = this.block
    if (this.selection) at.selection = this.selection
    return at
  }

  /** The open file (and the item it came from), for the screen context.
   *  References, with the names the interface draws for them; see
   *  `onscreen.ts`. Where they are in it is `at`, which `screen.svelte.ts`
   *  adds. */
  get showing(): Showing {
    const file = this.file
    if (!this.open || !file) return {}
    const item = this.item
    const title = item ? (library.items.find((i) => i.id === item)?.title ?? '') : ''
    return { open: seen('file', file.id, file.name), within: seen('item', item, title) }
  }

  /** How many pages it has; 0 until the outline is in. */
  get pages(): number {
    return this.outline?.pages ?? 0
  }

  get format(): DocFormat | null {
    return this.outline?.format ?? null
  }

  /** The section they are in, by index into `outline.sections`: for a
   *  document drawn block by block, the one the first block in view is under;
   *  for a PDF, the last one that starts on or before the page. */
  get section(): number | null {
    const outline = this.outline
    if (!outline) return null
    if (outline.format !== 'pdf' && this.block !== null) return sectionOfBlock(outline, this.block)
    let found: number | null = null
    for (const [i, section] of outline.sections.entries()) if (section.page <= this.page) found = i
    return found
  }

  /** The sections they are inside, outermost first: the part, the chapter. */
  get trail(): DocSection[] {
    const outline = this.outline
    const section = this.section
    return outline && section !== null ? sectionTrail(outline, section) : []
  }
}

export const reader = new ReaderState()
