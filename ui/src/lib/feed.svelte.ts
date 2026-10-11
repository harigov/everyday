// State for the Feed.
//
// A sibling of `library.svelte.ts` and `mail.svelte.ts`. "Feed" is the app;
// `news` is what the Rust side calls the domain, because `feed` already
// meant a subscribed calendar there.
//
// Three rules shape most of what is below:
//
//   1. Reading is quick and local. Opening a story marks it read on the
//      screen before the vault has been asked, and if the vault refuses, the
//      list is put back to what the vault says rather than left telling a lie.
//   2. A story that is open stays where it is. Opening one marks it read, and
//      a background refresh lands while somebody is reading; if either of
//      those pulled the row out of an "Unread" list the pane would lose its
//      place and `j` would not know where "below" was. So a reload keeps the
//      open story in the list, in its own position, until somebody navigates.
//   3. The network is only asked on purpose. Nothing here fetches a feed by
//      itself: the service's refresher does that whether or not anybody is
//      looking, and tells this store by a change event (`live.svelte.ts`).

import { api, callCommand, isMock } from './api'
import { filterStories, groupFolders, sortStories } from './feed'
import { applyBodyTheme, bodyDocument } from './mailview'
import { notify } from './notify.svelte'
import { seen, type Showing } from './onscreen'
import { openExternal } from './open-external'
import { pref } from './prefs'
import { app, errorMessage, handle, isLocked, quietly } from './state.svelte'
import { debounce } from './store/debounce'
import { latest } from './store/latest'
import { optimisticRemove } from './store/optimistic-remove'
import { guardedRefresh } from './store/refresh'
import type {
  FoundFeed,
  OpmlImportReport,
  ItemId,
  NewSource,
  NewsSource,
  NewsSourceId,
  NewsletterSender,
  RefreshReport,
  SourceSummary,
  Story,
  StoryCursor,
  StoryId,
  StoryPage,
  StoryQuery,
} from './types'

/** How many stories a page holds. The service allows 500; a screen shows far fewer. */
const PAGE = 100
/** The most the service will return at once, which bounds how much a reload covers. */
const MOST = 500

/** What the Feed tints the window with: the palette's blue, the default accent being orange. */
const FEED_ACCENT = '#0369a1'

/** What the sidebar has selected. */
export type FeedScope =
  { kind: 'all' } | { kind: 'folder'; name: string } | { kind: 'source'; id: NewsSourceId }

/** The chips over the list. Unread first: a feed is read to the end. */
export const FEED_FILTERS = ['unread', 'all', 'saved'] as const
export type FeedFilter = (typeof FEED_FILTERS)[number]

/** Newest first everywhere; points only means something on Hacker News. */
export type FeedSort = 'newest' | 'points'

/** One sidebar folder and what is in it. `name` is empty for unfiled. */
export interface FeedFolder {
  name: string
  sources: SourceSummary[]
  unread: number
}

/**
 * Which chip was last on, remembered on this device.
 *
 * Chrome rather than a fact about the vault, so it is kept locally the way
 * the library's view is. Read in the constructor and not in `start`: the
 * list would otherwise draw once as Unread and visibly flip.
 */
const filterPref = pref<FeedFilter>(
  'everyday.feed.filter',
  (raw) => FEED_FILTERS.find((f) => f === raw) ?? 'unread',
  'unread',
)

/** Does `a` come before `b` in a newest-first list? The service's own order. */
function precedes(a: Story, b: Story): boolean {
  const x = Date.parse(a.publishedAt)
  const y = Date.parse(b.publishedAt)
  return x !== y ? x > y : a.id > b.id
}

function plural(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`
}

/**
 * The origin every `news/…` address hangs off, which is not the same string
 * on every platform.
 *
 * The same reasoning as `mailview.ts`'s `mailOrigin`: Tauri maps a custom
 * scheme onto `http://everyday.localhost` on Windows and Android, where the
 * scheme's own host has nowhere to ride but the first path segment. Under
 * server mode the shell's protocol handler forwards to `/v1/news/body/{id}`
 * on the machine holding the vault, so the interface never builds that
 * address itself.
 */
function newsOrigin(): string {
  return navigator.userAgent.includes('Windows') || navigator.userAgent.includes('Android')
    ? 'http://everyday.localhost/news'
    : 'everyday://news'
}

/**
 * Point a stored body's own `everyday://news/img/…` addresses at the origin
 * this platform actually resolves.
 *
 * A body is sanitised once, when it is fetched, long before anybody knows
 * which webview will draw it -- so its picture addresses cannot be written
 * for the right platform in the first place, and are corrected here instead.
 * A no-op everywhere the scheme resolves natively.
 */
function applyNewsOrigin(html: string): string {
  const origin = newsOrigin()
  return origin === 'everyday://news' ? html : html.split('everyday://news/').join(`${origin}/`)
}

/** Is the window drawing dark? What `MailThread` asks before it themes a body. */
function darkNow(): boolean {
  if (app.theme === 'dark') return true
  if (app.theme === 'light') return false
  return typeof window !== 'undefined' && window.matchMedia('(prefers-color-scheme: dark)').matches
}

class FeedState {
  // -- what is on screen ------------------------------------------------
  scope = $state<FeedScope>({ kind: 'all' })
  filter = $state<FeedFilter>('unread')
  sort = $state<FeedSort>('newest')
  /** The bar's search text, applied to titles, summaries and source names. */
  query = $state('')
  selected = $state<StoryId | null>(null)
  /** The add sheet is open. */
  adding = $state(false)
  /**
   * An address the add sheet should start with: what was typed in the bar
   * before "Follow" was chosen. Set by `follow`, read by the sheet when it
   * opens, and dropped when it closes so the next opening starts empty.
   */
  addPrefill = $state('')

  // -- what has been loaded for it --------------------------------------
  sources = $state<SourceSummary[]>([])
  stories = $state<Story[]>([])
  /** Where the next page starts; `null` when the last page is loaded. */
  next = $state<StoryCursor | null>(null)
  loading = $state(false)
  /** A network refresh is running. */
  refreshing = $state(false)
  /** Set while "fetch the full article" runs for the open story. */
  extracting = $state(false)
  /** A line under the list after something happened. Cleared on navigation. */
  note = $state<string | null>(null)

  /** Which list load is the current one. See `#load`. */
  #generation = latest()
  /** The same for the sources, which are reloaded after almost every write. */
  #sourcesGeneration = latest()
  /** Which page-on-its-way is the current one; a reload retires it. */
  #paging = latest()
  /** A page is being fetched. */
  #more = false
  /**
   * The open story when it is not in `stories`: opened from the Overview or
   * a link, or too old for the pages loaded. The pane still has to draw it.
   * Raw, because it is replaced whole and never edited in place.
   */
  #detached = $state.raw<Story | null>(null)
  /** Refreshes in flight. A folder's menu starts one per source at once. */
  #refreshes = 0
  /** Writes sent and not yet answered, so the close handshake can wait for them. */
  #writes = new Set<Promise<unknown>>()
  /** The bar's search narrows what is loaded; this loads more of it. See `setQuery`. */
  #queryDebounce = debounce(() => void this.#fillForQuery(), 220)

  constructor() {
    // Registered once, here, for the reason the library store gives:
    // `FeedView` calls `start` on every mount, and a hook added there would
    // be added again each time the app is switched back to.
    app.onLock(() => this.reset())
    app.onFlush(() => this.flush())
    this.filter = filterPref.get()
  }

  // ── lifecycle ────────────────────────────────────────────────────────

  /**
   * Load the sources and the current list. Called by `FeedView` on mount.
   *
   * Idempotent: mounting the view again -- which happens on every switch
   * back to this app -- reloads over what is there rather than from nothing,
   * so the list does not blank out on the way in. The sources come first
   * because a folder is a list of them: it cannot say what it holds until
   * they are known.
   */
  async start(): Promise<void> {
    if (!app.supportsNews) return
    await this.#loadSources(false)
    await this.#load(false)
  }

  /**
   * Drop everything. Registered with `app.onLock` in the constructor.
   *
   * The chip stays: it is chrome, not content. The rest is a stranger's
   * words about what somebody chose to read, which is what a lock exists to
   * put away.
   */
  reset(): void {
    this.#queryDebounce.cancel()
    // Nothing on its way may land after this: those rows are the decrypted
    // contents this reset exists to drop.
    this.#generation.next()
    this.#sourcesGeneration.next()
    this.#paging.next()
    this.#more = false
    this.scope = { kind: 'all' }
    this.sort = 'newest'
    this.query = ''
    this.selected = null
    this.adding = false
    this.addPrefill = ''
    this.sources = []
    this.stories = []
    this.next = null
    this.loading = false
    this.refreshing = false
    this.extracting = false
    this.note = null
    this.#detached = null
    this.#refreshes = 0
  }

  /**
   * Wait for anything outstanding. Part of the close handshake.
   *
   * Nothing here is held back to be written later -- there is no autosave
   * queue, because a read mark or a save is a single call made at once. What
   * can be outstanding is one of those calls that has been sent and not yet
   * answered, and a window that closed without waiting for it would lose a
   * "mark all read" the person has already seen happen.
   */
  async flush(): Promise<void> {
    await Promise.allSettled([...this.#writes])
  }

  /** Keep a write in view until it settles. See `flush`. */
  #track<T>(write: Promise<T>): Promise<T> {
    this.#writes.add(write)
    const done = () => this.#writes.delete(write)
    void write.then(done, done)
    return write
  }

  // ── navigation ───────────────────────────────────────────────────────

  async select(scope: FeedScope): Promise<void> {
    this.scope = scope
    // A different list is a different place: what was open there is not.
    this.close()
    this.note = null
    await this.#load(true)
  }

  async setFilter(filter: FeedFilter): Promise<void> {
    if (filter === this.filter) return
    this.filter = filter
    filterPref.set(filter)
    this.close()
    this.note = null
    await this.#load(true)
  }

  setSort(sort: FeedSort): void {
    this.sort = sort
  }

  setQuery(query: string): void {
    this.query = query
    this.#queryDebounce.call()
  }

  /** Reload the sidebar's sources and counts. */
  async refreshSources(): Promise<void> {
    await this.#loadSources(false)
  }

  /**
   * Reload the list from the first page.
   *
   * Over as much as is loaded, not one page: a background refresh that
   * landed while somebody was three pages down must not shrink the list to
   * one and throw their place away. Up to what the service will give at
   * once, which is more than anybody scrolls.
   */
  async reload(): Promise<void> {
    await this.#load(false)
  }

  /** The next page, appended. */
  async loadMore(): Promise<void> {
    const cursor = this.next
    if (!cursor || this.#more || !app.supportsNews) return
    const query = this.#queryFor(cursor, PAGE)
    if (!query) return
    this.#more = true
    const token = this.#paging.next()
    try {
      const page = await api.stories(query)
      // A scope or filter change while this was on its way retired the token;
      // these rows belong to a list that is no longer showing.
      if (!this.#paging.isCurrent(token) || this.next?.id !== cursor.id) return
      // Not a bare append: a story kept in place because it was open can be
      // on the very page that follows it.
      const have = new Set(this.stories.map((s) => s.id))
      this.stories = [...this.stories, ...page.stories.filter((s) => !have.has(s.id))]
      this.next = page.next ?? null
    } catch (e) {
      await quietly(e)
    } finally {
      this.#more = false
    }
  }

  /** Open a story, and mark it read. */
  async open(id: StoryId): Promise<void> {
    this.selected = id
    this.note = null
    // Not in the list: it came from the Overview or a link, or is older than
    // the pages loaded. Fetch it so the pane has something to draw.
    if (this.stories.some((s) => s.id === id)) {
      this.#detached = null
    } else {
      try {
        const fetched = await api.story(id)
        if (this.selected !== id) return
        this.#detached = fetched
      } catch (e) {
        if (this.selected === id) this.selected = null
        await handle(e)
        return
      }
    }
    const story = this.#find(id)
    if (story && !story.read) await this.markRead([id], true)
  }

  /** Back to the list. */
  close(): void {
    this.selected = null
    this.#detached = null
  }

  /** Open the story `delta` rows below (positive) or above (negative). */
  async move(delta: number): Promise<void> {
    let rows = this.visible
    if (rows.length === 0) return
    const at = rows.findIndex((s) => s.id === this.selected)
    // With nothing open either key lands on the first row, which is where
    // somebody who has not started reading wants to be.
    let to = at < 0 ? 0 : at + delta
    // The end of what is loaded is not the end of the feed. Fetch on rather
    // than stopping dead, and fetch ahead so `j` held down never waits.
    if (this.next && to >= rows.length - 5) {
      if (to >= rows.length) {
        await this.loadMore()
        rows = this.visible
      } else {
        void this.loadMore()
      }
    }
    to = Math.min(rows.length - 1, Math.max(0, to))
    const target = rows[to]
    if (target && target.id !== this.selected) await this.open(target.id)
  }

  // ── stories ──────────────────────────────────────────────────────────

  async markRead(ids: StoryId[], read: boolean): Promise<void> {
    if (ids.length === 0) return
    // On the screen first. The sidebar's counts move with the rows, by one
    // each, and are then put right by what the vault says.
    for (const id of ids) {
      const story = this.#find(id)
      if (!story || story.read === read) continue
      this.#patch(id, { read })
      this.#bump(story.sourceId, read ? -1 : 1)
    }
    try {
      await this.#track(api.markStoriesRead(ids, read))
      void this.#loadSources(true)
    } catch (e) {
      await handle(e, () => this.#reloadAll())
    }
  }

  async toggleRead(id: StoryId): Promise<void> {
    const story = this.#find(id)
    if (story) await this.markRead([id], !story.read)
  }

  /**
   * Everything in view read, with an undo.
   *
   * "In view" is the sidebar's scope, not just the rows loaded: marking the
   * three pages that happen to be on screen and leaving a fourth behind
   * would say the feed was finished when it was not. The service takes a
   * date to mark up to, which is the newest unread story seen, so a story
   * that arrived a moment ago and has not been looked at stays unread.
   *
   * Two cases cannot be said by source and date, and are marked by id
   * instead: the Saved chip, since marking a source's whole backlog because
   * its saved stories were in view would be a surprise, and a search, which
   * narrows what is in view without narrowing the source.
   *
   * Newsletters are the third. A story that is a mail message has the
   * message's seen flag for its read state, so the service leaves it out of
   * a by-date sweep on purpose; the ones in view are marked by id as well,
   * and are part of what the undo puts back.
   */
  async markAllRead(): Promise<void> {
    const unread = this.visible.filter((s) => !s.read)
    if (unread.length === 0) {
      this.note = 'Everything here is read already.'
      return
    }
    let ids: StoryId[]
    try {
      if (this.filter === 'saved' || this.query.trim() !== '') {
        ids = unread.map((s) => s.id)
        await this.#track(api.markStoriesRead(ids, true))
      } else {
        const upTo = unread
          .map((s) => s.publishedAt)
          .reduce((a, b) => (Date.parse(b) > Date.parse(a) ? b : a))
        ids = await this.#track(api.markAllStoriesRead(this.#scopeSourceIds(), upTo))
        const mail = unread.filter((s) => s.mailMessage).map((s) => s.id)
        if (mail.length > 0) {
          await this.#track(api.markStoriesRead(mail, true))
          ids = [...ids, ...mail]
        }
      }
    } catch (e) {
      await handle(e)
      return
    }
    if (ids.length === 0) {
      this.note = 'Everything here is read already.'
      return
    }
    const marked = new Set(ids)
    for (const story of this.stories) if (marked.has(story.id)) story.read = true
    const open = this.#detached
    if (open && marked.has(open.id)) this.#detached = { ...open, read: true }
    // Reloaded rather than left: this is the one act whose point is an empty
    // list, and an "Unread" chip over read rows says the opposite.
    await Promise.all([this.#loadSources(true), this.#load(false)])
    notify.info(`Marked ${plural(ids.length, 'story', 'stories')} read`, {
      key: 'feed-marked-read',
      timeout: 10_000,
      action: { label: 'Undo', run: () => void this.#undoRead(ids) },
    })
  }

  /** Put back what `markAllRead` marked: exactly those, and nothing read before. */
  async #undoRead(ids: StoryId[]): Promise<void> {
    try {
      await this.#track(api.markStoriesRead(ids, false))
    } catch (e) {
      await handle(e)
    }
    await this.#reloadAll()
  }

  async toggleSaved(id: StoryId): Promise<void> {
    const story = this.#find(id)
    if (!story) return
    const saved = !story.saved
    // Left in the list if the Saved chip is up and it was just unsaved, the
    // way an opened story is left in Unread: it settles on the next load.
    this.#patch(id, { saved })
    try {
      await this.#track(api.saveStories([id], saved))
    } catch (e) {
      await handle(e, async () => {
        this.#patch(id, { saved: !saved })
      })
    }
  }

  /** Fetch, extract and keep the linked article as the story's body. */
  async fetchFullArticle(id: StoryId): Promise<void> {
    if (this.extracting) return
    this.extracting = true
    this.note = null
    try {
      // Replaces the story, so `hasBody` flips and the pane knows to ask for
      // the body again.
      this.#patch(id, await api.fetchFullArticle(id))
    } catch (e) {
      // A page that would not parse is something to say beside the button,
      // not a banner over the window.
      if (isLocked(e)) await app.lock()
      else this.note = errorMessage(e)
    } finally {
      this.extracting = false
    }
  }

  /** Put the story on the library's Articles shelf. */
  async addToLibrary(id: StoryId): Promise<ItemId | null> {
    const title = this.#find(id)?.title.trim()
    try {
      const item = await api.addStoryToLibrary(id)
      this.note = title ? `Added “${title}” to the Library.` : 'Added to the Library.'
      return item
    } catch (e) {
      await handle(e)
      return null
    }
  }

  /** Open the story's own address in the browser. */
  async openOriginal(id: StoryId): Promise<void> {
    await this.#openLink(this.#find(id)?.url, 'This story has no web address.')
  }

  /** Open the story's discussion (Hacker News) in the browser. */
  async openDiscussion(id: StoryId): Promise<void> {
    await this.#openLink(this.#find(id)?.discussionUrl, 'This story has no discussion.')
  }

  async #openLink(url: string | undefined, none: string): Promise<void> {
    if (!url) {
      this.note = none
      return
    }
    // Where there is no browser to hand it to -- a window on another
    // machine's vault -- the address is said instead, to copy.
    if (!(await openExternal(url))) {
      this.note = `This window cannot open links. The address is ${url}`
    }
  }

  // ── sources ──────────────────────────────────────────────────────────

  /** Ask the network: one source, or every source when `null`. */
  async refresh(source: NewsSourceId | null = null): Promise<RefreshReport[]> {
    if (!app.supportsNews) return []
    // Counted, not flagged: a folder's menu asks for each of its sources at
    // once, and the spinner must last until the last of them has answered.
    this.#refreshes += 1
    this.refreshing = true
    try {
      const reports = await api.refreshNews(source)
      const found = reports.reduce((n, r) => n + r.inserted, 0)
      const failed = reports.filter((r) => r.error).length
      this.note = this.#refreshNote(found, failed)
      await Promise.all([this.#loadSources(true), this.#load(false)])
      return reports
    } catch (e) {
      await handle(e)
      return []
    } finally {
      this.#refreshes = Math.max(0, this.#refreshes - 1)
      this.refreshing = this.#refreshes > 0
    }
  }

  /** What a refresh says under the list. */
  #refreshNote(found: number, failed: number): string {
    const trouble = failed > 0 ? `${plural(failed, 'source', 'sources')} could not be reached` : ''
    if (found > 0) {
      return `${plural(found, 'new story', 'new stories')}${trouble ? `, and ${trouble}` : ''}.`
    }
    return trouble ? `Nothing new, and ${trouble}.` : 'Nothing new.'
  }

  openAdd(): void {
    this.adding = true
  }

  closeAdd(): void {
    this.adding = false
    this.addPrefill = ''
  }

  /** Open the add sheet with an address already in it -- "Follow" in the bar. */
  follow(address: string): void {
    this.addPrefill = address.trim()
    this.adding = true
  }

  /** Every feed at or advertised by a pasted address. */
  async discover(url: string): Promise<FoundFeed[]> {
    if (!app.supportsNews) return []
    try {
      return await api.discoverFeeds(url.trim())
    } catch (e) {
      await handle(e)
      return []
    }
  }

  /** Follow something. Selects it on success. */
  async addSource(input: NewSource): Promise<NewsSource | null> {
    try {
      const added = await api.addNewsSource(input)
      await this.#loadSources(true)
      await this.select({ kind: 'source', id: added.id })
      // Nothing is in it yet, and the service's own timer could be minutes
      // away. Ask now, so what was just followed is seen to have worked.
      void this.refresh(added.id)
      return added
    } catch (e) {
      await handle(e)
      return null
    }
  }

  async saveSource(source: NewsSource): Promise<void> {
    try {
      await this.#track(api.saveNewsSource($state.snapshot(source)))
      await this.#loadSources(true)
      // A source moved into or out of the folder on screen changes the list.
      if (this.scope.kind === 'folder') await this.#load(false)
    } catch (e) {
      await handle(e)
    }
  }

  /** Unfollow, taking its stories with it. */
  async deleteSource(id: NewsSourceId): Promise<void> {
    await optimisticRemove({
      optimistic: () => {
        // The pane goes with its source: a story with nothing to belong to
        // would be the last of it left on screen.
        if (this.story?.sourceId === id) this.close()
        this.sources = this.sources.filter((s) => s.source.id !== id)
        this.stories = this.stories.filter((s) => s.sourceId !== id)
        if (this.scope.kind === 'source' && this.scope.id === id) this.scope = { kind: 'all' }
      },
      call: () => this.#track(api.deleteNewsSource(id)),
      onSuccess: () => void this.#reloadAll(),
      rollback: () => this.#reloadAll(),
    })
  }

  async importOpml(xml: string): Promise<OpmlImportReport | null> {
    try {
      const report = await api.importOpml(xml)
      await this.#loadSources(true)
      // They are new to the service too, and have nothing in them yet. What
      // the import did is the report's to say, beside the file it came from.
      if (report.added > 0) void this.refresh()
      return report
    } catch (e) {
      await handle(e)
      return null
    }
  }

  async exportOpml(): Promise<string | null> {
    try {
      return await api.exportOpml()
    } catch (e) {
      await handle(e)
      return null
    }
  }

  /** Who sends newsletters to this vault's inboxes, for "From your mail". */
  async newsletterSenders(days = 90): Promise<NewsletterSender[]> {
    if (!this.mailAvailable) return []
    try {
      return await api.newsletterSenders(days)
    } catch (e) {
      await handle(e)
      return []
    }
  }

  // ── derived ──────────────────────────────────────────────────────────

  /** The open story. */
  get story(): Story | null {
    const id = this.selected
    if (!id) return null
    const listed = this.stories.find((s) => s.id === id)
    return listed ?? (this.#detached?.id === id ? this.#detached : null)
  }

  /** The source a story came from. */
  sourceOf(story: Story): SourceSummary | null {
    return this.sources.find((s) => s.source.id === story.sourceId) ?? null
  }

  /** The selected source, when one is. */
  get source(): SourceSummary | null {
    const scope = this.scope
    return scope.kind === 'source'
      ? (this.sources.find((s) => s.source.id === scope.id) ?? null)
      : null
  }

  /** The sidebar's folders, unfiled first, each with its unread total. */
  get folders(): FeedFolder[] {
    return groupFolders(this.sources)
  }

  get unreadTotal(): number {
    return this.sources.reduce((n, s) => n + s.unread, 0)
  }

  /** The list as drawn: sorted, and narrowed by the bar's query. */
  get visible(): Story[] {
    return filterStories(sortStories(this.stories, this.sort), this.query, this.sources)
  }

  /** Newsletters can be offered: the backend has mail and an account. */
  get mailAvailable(): boolean {
    return app.supportsMail
  }

  get accent(): string {
    return FEED_ACCENT
  }

  /**
   * What is on screen, for the assistant -- see `onscreen.ts`.
   *
   * `view` is the interface's own words for how the list is narrowed, and
   * never a title: a folder's name is something somebody typed, so a folder
   * is said by the sources in it, which go by id and are named from the
   * vault.
   */
  get showing(): Showing {
    const scope = this.scope
    const within =
      scope.kind === 'source'
        ? seen('newsSource', scope.id, this.source?.source.title ?? '')
        : scope.kind === 'folder'
          ? this.#inFolder(scope.name).flatMap((s) =>
              seen('newsSource', s.source.id, s.source.title),
            )
          : []
    const words = [
      scope.kind === 'folder' ? 'a folder' : null,
      this.filter === 'unread' ? 'unread only' : this.filter === 'saved' ? 'saved' : null,
      this.sort === 'points' ? 'by points' : null,
    ].filter(Boolean)
    const story = this.story
    return {
      view: words.length > 0 ? words.join(', ') : null,
      within,
      open: seen('story', this.selected, story?.title ?? ''),
      query: this.query,
    }
  }

  /** The address the reading pane's frame loads a story's body from. */
  bodyUrl(story: Story): string {
    return `${newsOrigin()}/body/${encodeURIComponent(story.id)}`
  }

  /**
   * A story's body as a whole document for the sandboxed frame, and whether
   * its pictures were held back. Works against the mock backend too, which
   * has no `everyday://` protocol to fetch from. `null` when it could not be
   * loaded (the reason is shown the usual way).
   *
   * The document is already drawn for the window's light or dark theme and
   * its picture addresses already point at this platform's origin, so the
   * pane has only to assign it to `srcdoc`.
   *
   * The pane asks again when the story's `hasBody` changes, which is what
   * `fetchFullArticle` flips: the address is the same and the body is not.
   */
  async loadBody(story: Story): Promise<{ html: string; imagesHidden: boolean } | null> {
    try {
      const loaded = isMock ? await this.#mockBody(story) : await this.#fetchBody(story)
      return {
        html: applyNewsOrigin(applyBodyTheme(loaded.html, darkNow())),
        imagesHidden: loaded.imagesHidden,
      }
    } catch (e) {
      await handle(e)
      return null
    }
  }

  /**
   * The one `fetch()` in the Feed: the body is a response from the shell's
   * own protocol and not a command, because it is HTML for a frame and not a
   * value, and because its pictures are addressed relative to it.
   *
   * Fetched rather than given to the frame as its `src`, for the reason the
   * mail view gives: the header saying whether pictures were held back is on
   * the response, and an `<iframe src>` has no hook to read it. Not cached,
   * since whether they are held back can change between two requests.
   */
  async #fetchBody(story: Story): Promise<{ html: string; imagesHidden: boolean }> {
    const res = await fetch(this.bodyUrl(story), { cache: 'no-store' })
    if (!res.ok) throw new Error(`could not load the story (${res.status})`)
    const imagesHidden = res.headers.get('x-news-images-hidden') === '1'
    return { html: await res.text(), imagesHidden }
  }

  /**
   * The mock backend's answer, in the same frame mock mail uses. Asked for by
   * name, as the dev-only mail triggers are: no real command is behind it, so
   * the generated client has nothing to type it from.
   */
  async #mockBody(story: Story): Promise<{ html: string; imagesHidden: boolean }> {
    const body = await callCommand<{ html: string; imagesHidden: boolean }>('mock_story_body', {
      id: story.id,
    })
    const framed = bodyDocument(story.id, { html: body.html, imagesHidden: body.imagesHidden })
    return { html: framed.html ?? '', imagesHidden: framed.imagesHidden ?? false }
  }

  // ── loading ──────────────────────────────────────────────────────────

  /** The sources in a folder. Compared trimmed, as the sidebar groups them. */
  #inFolder(name: string): SourceSummary[] {
    const wanted = name.trim()
    return this.sources.filter((s) => s.source.folder.trim() === wanted)
  }

  /** The sources the scope on screen covers, for a service call that takes them by id. */
  #scopeSourceIds(): NewsSourceId[] {
    const scope = this.scope
    if (scope.kind === 'source') return [scope.id]
    const covered = scope.kind === 'folder' ? this.#inFolder(scope.name) : this.sources
    return covered.map((s) => s.source.id)
  }

  /**
   * The query for the list on screen, or `null` when it can show nothing.
   *
   * A folder with no sources in it is nothing, not everything: sent as an
   * empty list the service would read it as "every source", which is the
   * opposite of what a folder somebody has just emptied should show.
   */
  #queryFor(before: StoryCursor | null, limit: number): StoryQuery | null {
    const query: StoryQuery = { limit }
    if (this.filter === 'unread') query.unreadOnly = true
    if (this.filter === 'saved') query.savedOnly = true
    if (before) query.before = before
    if (this.scope.kind !== 'all') {
      const ids = this.#scopeSourceIds()
      if (ids.length === 0) return null
      query.sources = ids
    }
    return query
  }

  async #loadSources(quiet: boolean): Promise<void> {
    if (!app.supportsNews) return
    // Numbered like the list, and for the same reason: a mark, a refresh and
    // a save each ask for the sources again, and an answer that was asked for
    // before a later write must not land over one asked for after it.
    const token = this.#sourcesGeneration.next()
    try {
      const sources = await api.newsSources()
      if (!this.#sourcesGeneration.isCurrent(token)) return
      this.sources = sources
      if (this.#scopeGone()) {
        // Unfollowed on another machine, or a folder emptied by a move.
        // Everything, rather than a list of nothing under a name that is no
        // longer anywhere in the sidebar.
        this.scope = { kind: 'all' }
        void this.#load(true)
      }
    } catch (e) {
      if (quiet) await quietly(e)
      else await handle(e)
    }
  }

  #scopeGone(): boolean {
    const scope = this.scope
    if (scope.kind === 'source') return !this.sources.some((s) => s.source.id === scope.id)
    if (scope.kind === 'folder') return this.#inFolder(scope.name).length === 0
    return false
  }

  /**
   * Reload the list.
   *
   * Numbered, because these overlap -- a chip, a folder and a background
   * refresh can all be in the air at once, and nothing guarantees the answers
   * come back in order. Only the newest may land. `fresh` is a navigation:
   * one page, from the top. Otherwise it covers as much as was loaded.
   */
  async #load(fresh: boolean): Promise<void> {
    if (!app.supportsNews) return
    // A page on its way belongs to the list being replaced.
    this.#paging.next()
    await guardedRefresh(
      this.#generation,
      async (isCurrent) => {
        const covering = fresh ? PAGE : Math.min(MOST, Math.max(PAGE, this.stories.length))
        const query = this.#queryFor(null, covering)
        const page: StoryPage = query ? await api.stories(query) : { stories: [], next: null }
        if (!isCurrent()) return
        this.#land(page.stories, page.next ?? null)
      },
      { setLoading: (v) => (this.loading = v), onError: (e) => handle(e) },
    )
  }

  /**
   * Put a fresh page in place, keeping the open story where it was.
   *
   * See the second rule at the top of this file. If the story has fallen out
   * of the page it goes back in at its own position, as long as that position
   * is within what is loaded. If it is older than everything loaded, a later
   * page will bring it, and until then the pane keeps a copy of its own.
   */
  #land(fresh: Story[], next: StoryCursor | null): void {
    const open = this.story
    let list = fresh
    let detached: Story | null = null
    if (open && !fresh.some((s) => s.id === open.id)) {
      const kept = $state.snapshot(open)
      const at = fresh.findIndex((s) => precedes(kept, s))
      if (at >= 0) list = [...fresh.slice(0, at), kept, ...fresh.slice(at)]
      else if (next === null) list = [...fresh, kept]
      else detached = kept
    }
    this.stories = list
    this.next = next
    this.#detached = detached
  }

  /** Both halves, after something that could have changed either. */
  async #reloadAll(): Promise<void> {
    await Promise.all([this.#loadSources(true), this.#load(false)])
  }

  /**
   * Load more while a search has too little to show.
   *
   * The bar's search narrows what is loaded and cannot see further. Typing a
   * word that matches three of the hundred stories on screen and none of the
   * thousand behind them would report a feed with three stories about it.
   * So while the matches are few and there is more to fetch, fetch -- up to
   * a limit, since a word that matches nothing would otherwise read the
   * whole feed. Stops the moment a page fails to move the cursor.
   */
  async #fillForQuery(): Promise<void> {
    const asked = this.query.trim()
    if (!asked) return
    for (let pages = 0; pages < 10; pages++) {
      if (!this.next || this.query.trim() !== asked || this.visible.length >= 25) return
      const before = this.next.id
      await this.loadMore()
      if (this.next?.id === before) return
    }
  }

  // ── in place ─────────────────────────────────────────────────────────

  /** A story wherever the screen holds it: the list, or the open copy. */
  #find(id: StoryId): Story | undefined {
    const listed = this.stories.find((s) => s.id === id)
    if (listed) return listed
    return this.#detached?.id === id ? this.#detached : undefined
  }

  /**
   * Change a story where it is held.
   *
   * In place for a listed one, so a row and the pane redraw from one object;
   * replaced whole for the detached copy, which is raw.
   */
  #patch(id: StoryId, changes: Partial<Story>): void {
    const listed = this.stories.find((s) => s.id === id)
    if (listed) {
      Object.assign(listed, changes)
      return
    }
    const open = this.#detached
    if (open?.id === id) this.#detached = { ...open, ...changes }
  }

  /** Move a source's unread count by `by`, until the vault's own arrives. */
  #bump(id: NewsSourceId, by: number): void {
    const summary = this.sources.find((s) => s.source.id === id)
    if (summary) summary.unread = Math.max(0, summary.unread + by)
  }
}

export const feed = new FeedState()
