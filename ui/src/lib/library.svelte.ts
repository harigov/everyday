// State for the library app.
//
// The fourth store, a sibling of `state.svelte.ts`, `todo.svelte.ts` and
// `calendar.svelte.ts`. Where the calendar store is unusual for reading
// across domains, this one is unusual for reading across the *network*: it is
// the only store whose contents can be filled in by somebody else's server.
//
// That shapes almost everything here. Three rules, and the third is the one
// that keeps coming up:
//
//   1. Adding something never waits on the network. `add` writes the item
//      first and enriches it after, so a train tunnel costs you a cover and
//      not the note you were trying to make.
//   2. Metadata fills gaps and never argues. The rule is enforced in the
//      Rust core (`websearch::apply`), where it is tested; this file does not
//      get to have a second opinion about it.
//   3. A picture is downloaded, never linked. Covers arrive as blob ids and
//      are drawn through the `everyday://` protocol, so opening the app does
//      not tell a stranger's server what is on your shelf.

import { api } from './api'
import { Autosave } from './autosave'
import { app, errorMessage, handle, isLocked } from './state.svelte'
import { TRAY_ORDER, tray } from './tray.svelte'
import { web } from './websearch'
import type {
  Item,
  ItemId,
  ItemSort,
  ItemStatus,
  Kind,
  KindId,
  KindInfo,
  LibraryStats,
  LogEntry,
  LogEvent,
  SearchResult,
} from './types'
import { ITEM_STATUSES, isAhead } from './types'

/** How many items a shelf loads at once. A shelf, not a database. */
const PAGE = 500

/** Which shelf is on screen. `null` is the everything view. */
export type Shelf = KindId | null

/**
 * The status filter, as the filter bar offers it.
 *
 * `ahead` is a set rather than a status, and it is the default because it is
 * the question the app exists to answer: what have I said I want to get to.
 */
export const FILTERS = ['ahead', 'all', ...ITEM_STATUSES] as const
export type Filter = (typeof FILTERS)[number]

/** The statuses a filter selects. Empty means "do not filter". */
export function statusesFor(filter: Filter): ItemStatus[] {
  if (filter === 'all') return []
  if (filter === 'ahead') return ITEM_STATUSES.filter(isAhead)
  return [filter]
}

export type View = 'grid' | 'list'

class LibraryState {
  // -- what is on screen ------------------------------------------------
  view = $state<View>('grid')
  shelf = $state<Shelf>(null)
  filter = $state<Filter>('ahead')
  sort = $state<ItemSort>('addedDesc')
  query = $state('')
  /** Star filter, the same affordance the journal's starred view has. */
  favouritesOnly = $state(false)

  // -- what has been loaded for it --------------------------------------
  kinds = $state<KindInfo[]>([])
  items = $state<Item[]>([])
  stats = $state<LibraryStats | null>(null)
  /** The open item's history, newest first. Loaded when it is opened. */
  logs = $state<LogEntry[]>([])
  selected = $state<ItemId | null>(null)

  loading = $state(false)
  /** Set while a lookup is in flight for the open item. */
  enriching = $state(false)
  /** A line under the shelf after something happened. Cleared on navigation. */
  note = $state<string | null>(null)

  #queryTimer: ReturnType<typeof setTimeout> | null = null
  /**
   * How to put the cursor in the capture field, registered by the view.
   *
   * The same arrangement the todo app's capture line uses, and for the same
   * reason: a quick action from the menu bar has no reference to a component
   * and should not have to be handed one down through the view tree.
   */
  #capture: (() => void) | null = null
  /**
   * A focus asked for before the view was there to take it. The ordinary
   * case for the tray: the request arrives while the journal is on screen and
   * the library view mounts a frame later.
   */
  #captureWanted = false

  constructor() {
    // Registered once, here, rather than from `start()`.
    //
    // `LibraryView` calls `start` on every mount, and it mounts every time
    // somebody switches back to this app. A hook registered there would be
    // added again each time, so the list would grow for the life of the
    // process and `reset` would run once per entry in it. This is the
    // arrangement the todo store already uses.
    app.onLock(() => this.reset())

    // Which view somebody last chose is chrome state, remembered locally the
    // way the open section is. Reading it in `start` would be too late: the
    // grid has already drawn by then, so it would visibly flip to the list.
    const remembered = localStorage.getItem('everyday.library.view')
    if (remembered === 'grid' || remembered === 'list') this.view = remembered
  }

  /**
   * Edits are written a beat after typing stops, like everywhere else.
   *
   * `write` re-reads from `items` at flush time rather than closing over the
   * record, so a field edited twice inside one window is written once, with
   * the later value.
   */
  #saves = new Autosave<ItemId>(async (ids) => {
    const dirty = this.items.filter((i) => ids.has(i.id))
    if (dirty.length === 0) return
    await api.saveItems(dirty.map((i) => $state.snapshot(i)))
    // Counts move when a status does, and the sidebar shows them.
    void this.refreshCounts()
  })

  // ── lifecycle ────────────────────────────────────────────────────────

  /** Register (or with `null`, retire) the capture field's focus. */
  bindCapture(fn: (() => void) | null) {
    this.#capture = fn
    if (fn && this.#captureWanted) {
      this.#captureWanted = false
      fn()
    }
  }

  /** Put the cursor in the capture field, now or as soon as there is one. */
  focusCapture() {
    if (this.#capture) this.#capture()
    else this.#captureWanted = true
  }

  /**
   * Load the shelves and the current view. Called by `LibraryView` on mount.
   *
   * Idempotent: mounting the view again -- which happens on every switch back
   * to this app -- refreshes rather than reloading from nothing, so the grid
   * does not blank out on the way in.
   */
  async start() {
    if (!app.supportsLibrary) return
    await this.refreshKinds()
    await this.refresh()
  }

  /**
   * Drop everything. Registered with `app.onLock` in the constructor.
   *
   * The cached search results go too: a result is a blurb about something
   * somebody is interested in, which is exactly the sort of thing a lock
   * screen is meant to put away -- the same reason the search index goes.
   */
  reset() {
    this.#saves.cancel()
    if (this.#queryTimer) clearTimeout(this.#queryTimer)
    this.#queryTimer = null
    this.#captureWanted = false
    this.kinds = []
    this.items = []
    this.logs = []
    this.stats = null
    this.selected = null
    this.query = ''
    this.note = null
    web.clear()
  }

  /** Write anything outstanding now. Part of the close handshake. */
  async flush() {
    await this.#saves.flush()
  }

  // ── shelves ──────────────────────────────────────────────────────────

  async refreshKinds() {
    try {
      // Also what seeds the built-in shelves into an empty library; see
      // `list_kinds` in the Rust shell for why that hangs off a read.
      this.kinds = await api.kinds()
    } catch (e) {
      await handle(e)
    }
  }

  /** The counts only, for after a write. Cheaper than reloading the shelves. */
  async refreshCounts() {
    try {
      const [kinds, stats] = await Promise.all([api.kinds(), api.libraryStats()])
      this.kinds = kinds
      this.stats = stats
    } catch (e) {
      if (isLocked(e)) await app.lock()
    }
  }

  async selectShelf(shelf: Shelf) {
    await this.flush()
    this.shelf = shelf
    this.selected = null
    this.logs = []
    this.note = null
    await this.refresh()
  }

  setFilter(filter: Filter) {
    this.filter = filter
    void this.refresh()
  }

  setSort(sort: ItemSort) {
    this.sort = sort
    void this.refresh()
  }

  setView(view: View) {
    this.view = view
    localStorage.setItem('everyday.library.view', view)
  }

  /** Debounced, so typing does not fire a query per keystroke. */
  setQuery(text: string) {
    this.query = text
    if (this.#queryTimer) clearTimeout(this.#queryTimer)
    this.#queryTimer = setTimeout(() => {
      this.#queryTimer = null
      void this.refresh()
    }, 160)
  }

  async newShelf(name: string): Promise<boolean> {
    const trimmed = name.trim()
    if (!trimmed) return false
    try {
      const kind = await api.newKind(trimmed, trimmed)
      // The id, slug, timestamps and defaults come from the core. Minting
      // them here would mean depending on `crypto.randomUUID`, which needs a
      // secure context the packaged webview does not always provide.
      kind.sortOrder = this.kinds.length
      kind.color = DEFAULT_SHELF_COLORS[this.kinds.length % DEFAULT_SHELF_COLORS.length]!
      await api.saveKind(kind)
      await this.refreshKinds()
      await this.selectShelf(kind.id)
      return true
    } catch (e) {
      await handle(e)
      return false
    }
  }

  async saveShelf(kind: Kind) {
    try {
      await api.saveKind($state.snapshot(kind))
      await this.refreshKinds()
    } catch (e) {
      await handle(e)
    }
  }

  async deleteShelf(id: KindId) {
    try {
      await api.deleteKind(id)
    } catch (e) {
      return void (await handle(e))
    }
    if (this.shelf === id) this.shelf = null
    await this.refreshKinds()
    await this.refresh()
  }

  // ── items ────────────────────────────────────────────────────────────

  async refresh() {
    if (!app.supportsLibrary) return
    this.loading = true
    try {
      const [items, stats] = await Promise.all([
        api.items({
          kindId: this.shelf,
          statuses: statusesFor(this.filter),
          text: this.query.trim(),
          favourite: this.favouritesOnly ? true : null,
          sort: this.sort,
          limit: PAGE,
        }),
        api.libraryStats(),
      ])
      this.items = items
      this.stats = stats
      // A selection that has scrolled out of the filter is dropped rather
      // than left pointing at a card nobody can see.
      if (this.selected && !items.some((i) => i.id === this.selected)) {
        this.selected = null
        this.logs = []
      }
    } catch (e) {
      await handle(e)
    } finally {
      this.loading = false
    }
  }

  /**
   * Add something, and go and find out what it is.
   *
   * One backend call, and it writes the item before it searches -- see the
   * rules at the top of this file. Returns the item so the caller can open
   * it; returns null only if the write itself failed.
   */
  async add(kindId: KindId, title: string, lookup = true): Promise<Item | null> {
    const trimmed = title.trim()
    if (!trimmed) return null
    try {
      const { item, lookedUp } = await api.addItem(kindId, trimmed, lookup)
      // Prepended rather than refetched: the card should appear under the
      // cursor at once, and the shelf's own order settles on the next load.
      //
      // ...but only if the filter on screen would have shown it. Adding
      // something to a shelf while "Read" is selected must not put an unread
      // thing in a list of read ones -- that is a list the interface has
      // just made untrue. It is said in words instead.
      const shown = this.wouldShow(item)
      if (shown) this.items = [item, ...this.items]
      this.note = !shown
        ? `Added to ${this.kinds.find((k) => k.id === kindId)?.name ?? 'the library'}, which this filter does not show.`
        : lookup && !lookedUp
          ? `Added. Nothing was found online for "${trimmed}".`
          : null
      void this.refreshCounts()
      return item
    } catch (e) {
      await handle(e)
      return null
    }
  }

  /**
   * Would the filters on screen have included this item?
   *
   * The interface's copy of what the backend's `ItemQuery` does, and only for
   * the three filters a newly added thing can fall foul of. It is not a
   * general reimplementation of the query -- that would be a second source of
   * truth to keep in step -- just enough to know whether to draw a card or
   * say a sentence.
   */
  private wouldShow(item: Item): boolean {
    if (this.shelf && item.kindId !== this.shelf) return false
    if (this.favouritesOnly && !item.favourite) return false
    const statuses = statusesFor(this.filter)
    return statuses.length === 0 || statuses.includes(item.status)
  }

  /** Note an edit made in place. Written a beat after typing stops. */
  touch(id: ItemId) {
    const item = this.items.find((i) => i.id === id)
    if (item) item.updatedAt = new Date().toISOString()
    this.#saves.touch(id)
  }

  async open(id: ItemId) {
    await this.flush()
    this.selected = id
    this.logs = []
    try {
      // Re-read rather than trusting the list: the list may be a page old,
      // and the detail panel is where somebody edits.
      const fresh = await api.item(id)
      const at = this.items.findIndex((i) => i.id === id)
      if (at >= 0) this.items[at] = fresh
      this.logs = await api.logs({ itemId: id })
    } catch (e) {
      await handle(e)
    }
  }

  close() {
    this.selected = null
    this.logs = []
  }

  /**
   * Move an item to a status, letting the backend date it and log it.
   *
   * Optimistic on the screen and authoritative from the answer: the card
   * changes at once, and is then replaced by what was actually written --
   * which carries the start and finish dates this could not have worked out
   * on its own.
   */
  async setStatus(id: ItemId, status: ItemStatus, log = true) {
    const at = this.items.findIndex((i) => i.id === id)
    const before = at >= 0 ? $state.snapshot(this.items[at]!) : null
    if (at >= 0) this.items[at]!.status = status
    try {
      const saved = await api.setItemStatus(id, status, log)
      // Replaced in place, and deliberately *not* re-filtered. Marking a book
      // read while the "To read" filter is up should leave the card where it
      // is, showing its new state, rather than making it vanish from under
      // the cursor that just clicked it. It settles on the next navigation.
      const now = this.items.findIndex((i) => i.id === id)
      if (now >= 0) this.items[now] = saved
      if (this.selected === id) this.logs = await api.logs({ itemId: id })
      void this.refreshCounts()
    } catch (e) {
      await handle(e, async () => {
        const now = this.items.findIndex((i) => i.id === id)
        if (now >= 0 && before) this.items[now] = before
      })
    }
  }

  /** Record where you have got to. Starts the item if it was only wished for. */
  async setProgress(id: ItemId, position: number, total: number | null, log = true) {
    try {
      const saved = await api.setItemProgress(id, position, total, log)
      const at = this.items.findIndex((i) => i.id === id)
      if (at >= 0) this.items[at] = saved
      if (this.selected === id) this.logs = await api.logs({ itemId: id })
      void this.refreshCounts()
    } catch (e) {
      await handle(e, () => this.refresh())
    }
  }

  async toggleFavourite(id: ItemId) {
    const item = this.items.find((i) => i.id === id)
    if (!item) return
    item.favourite = !item.favourite
    this.touch(id)
  }

  async rate(id: ItemId, rating: number | null) {
    const item = this.items.find((i) => i.id === id)
    if (!item) return
    item.rating = rating
    this.touch(id)
  }

  async remove(id: ItemId) {
    this.items = this.items.filter((i) => i.id !== id)
    if (this.selected === id) this.close()
    try {
      await api.deleteItem(id)
      void this.refreshCounts()
    } catch (e) {
      await handle(e, () => this.refresh())
    }
  }

  // ── the log ──────────────────────────────────────────────────────────

  /** Add a dated row to the open item's history. */
  async addLog(id: ItemId, event: LogEvent, note = '') {
    try {
      const entry = await api.newLog(id, event)
      entry.note = note
      await api.saveLog(entry)
      if (this.selected === id) this.logs = await api.logs({ itemId: id })
      void this.refreshCounts()
    } catch (e) {
      await handle(e)
    }
  }

  async saveLogEntry(entry: LogEntry) {
    try {
      await api.saveLog($state.snapshot(entry))
      if (this.selected) this.logs = await api.logs({ itemId: this.selected })
    } catch (e) {
      await handle(e)
    }
  }

  async removeLog(id: string) {
    this.logs = this.logs.filter((l) => l.id !== id)
    try {
      await api.deleteLog(id)
      void this.refreshCounts()
    } catch (e) {
      await handle(e)
    }
  }

  // ── metadata ─────────────────────────────────────────────────────────

  /**
   * Look a title up on behalf of a shelf.
   *
   * A thin pass-through to `web.lookup`, which owns the debouncing, the
   * caching and the race guard. It is here so a component can ask the store
   * rather than reach past it, and so the shelf's id is the only thing a
   * caller has to know.
   */
  async lookup(kindId: KindId, query: string, limit?: number): Promise<SearchResult[]> {
    const outcome = await web.lookup(kindId, query, limit)
    if (outcome.error) this.note = outcome.error
    return outcome.results
  }

  /**
   * Apply a chosen result to an item, cover and all.
   *
   * `overwrite` is the explicit re-fetch: it replaces the title, byline and
   * facts a previous lookup filled in. It still cannot touch notes, your
   * rating or the status -- that rule lives in the Rust core, where it is
   * tested, and this call has no way to ask for an exception.
   */
  async applyMetadata(id: ItemId, result: SearchResult, overwrite = false) {
    this.enriching = true
    try {
      const saved = await api.applyMetadata(id, result, overwrite)
      const at = this.items.findIndex((i) => i.id === id)
      if (at >= 0) this.items[at] = saved
      this.note = null
    } catch (e) {
      if (isLocked(e)) await app.lock()
      else this.note = errorMessage(e)
    } finally {
      this.enriching = false
    }
  }

  /**
   * Fetch the cover an item already knows the address of.
   *
   * The case this exists for: a lookup that found the metadata while the
   * network was flaky and could not get the picture. The address is kept on
   * the item precisely so this is one button rather than a second search.
   */
  async fetchCover(id: ItemId) {
    const item = this.items.find((i) => i.id === id)
    if (!item?.coverUrl) return
    this.enriching = true
    try {
      const blob = await api.fetchImage(item.coverUrl)
      item.cover = blob
      this.touch(id)
    } catch (e) {
      if (isLocked(e)) await app.lock()
      else this.note = errorMessage(e)
    } finally {
      this.enriching = false
    }
  }

  // ── derived ──────────────────────────────────────────────────────────

  get kind(): KindInfo | null {
    return this.kinds.find((k) => k.id === this.shelf) ?? null
  }

  /** The shelf an item belongs to, for its colour, icon and verbs. */
  kindOf(item: Item): KindInfo | null {
    return this.kinds.find((k) => k.id === item.kindId) ?? null
  }

  get item(): Item | null {
    return this.items.find((i) => i.id === this.selected) ?? null
  }

  get accent(): string {
    return this.kind?.color ?? 'var(--accent)'
  }

  /** Shelves worth showing: everything the person has left ticked. */
  get visibleKinds(): KindInfo[] {
    return this.kinds.filter((k) => k.visible)
  }

  /** The shelf a new item goes on when the everything view is open. */
  get defaultShelf(): KindInfo | null {
    return this.kind ?? this.visibleKinds[0] ?? this.kinds[0] ?? null
  }

  /** What this shelf calls a status. Falls back to the plain word. */
  label(kind: Kind | null, status: ItemStatus): string {
    if (!kind) return PLAIN_STATUS[status]
    switch (status) {
      case 'wishlist':
        return kind.verbs.wishlist
      case 'active':
        return kind.verbs.active
      case 'done':
        return kind.verbs.done
      default:
        return PLAIN_STATUS[status]
    }
  }
}

/** The words for the two statuses no shelf gets to rename. */
const PLAIN_STATUS: Record<ItemStatus, string> = {
  wishlist: 'Wishlist',
  active: 'Underway',
  paused: 'Paused',
  done: 'Done',
  abandoned: 'Given up',
}

/**
 * The frame a shelf's covers are drawn in.
 *
 * A restaurant is not a paperback: a place photographed in a 2:3 frame is a
 * photograph of a wall. Read off the slug rather than stored on the shelf,
 * because it is a drawing decision and not something anyone should have to
 * answer when they add one.
 *
 * `null` -- the everything view, and favourites -- gets the portrait default.
 * One ratio per grid matters more than the right ratio per card: a grid whose
 * rows are different heights has titles at four different baselines, which
 * reads as broken rather than as varied.
 */
export function coverRatio(kind: Kind | null): string {
  const landscape = ['restaurant', 'place', 'recipe', 'article']
  return kind && landscape.includes(kind.slug) ? '4 / 3' : '2 / 3'
}

/** Shelf accents. The journal palette, so one vault has one set of colours. */
export const DEFAULT_SHELF_COLORS = [
  '#b4530f',
  '#0f766e',
  '#7c3aed',
  '#b91c5c',
  '#1d4ed8',
  '#a16207',
  '#4d7c0f',
  '#0e7490',
]

export const library = new LibraryState()

// ── Quick actions ──────────────────────────────────────────────────────
//
// Registered at module scope beside the store whose state they read, which is
// the convention the other three follow. See `lib/tray.svelte.ts`.

tray.register('library', TRAY_ORDER.library, () => {
  if (app.screen !== 'main' || !app.supportsLibrary) return []
  return [
    {
      id: 'library:add',
      // The action a tray is for: something was recommended to you while you
      // were doing something else, and it needs to land somewhere before you
      // forget it.
      label: 'Add to library',
      run: async () => {
        if (await app.goTo('library')) library.focusCapture()
      },
    },
  ]
})
