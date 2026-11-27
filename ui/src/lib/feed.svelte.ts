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

import { accounts } from './accounts.svelte'
import { api, callCommand, isMock, isWeb } from './api'
import {
  FEED_KINDS,
  KIND_LABELS,
  bodyAboutOf,
  bodyOriginOf,
  filterStories,
  groupFolders,
  isPickable,
  isWebUrl,
  listedSources,
  notNewslettersNote,
  sameFeedAddress,
  scopeSources,
  scopeUnread,
  sortStories,
  sourceLabel,
  unsubscribedNote,
  type BodyOrigin,
  type KindChoice,
} from './feed'
import { allowWebOrigin, applyBodyTheme, bodyDocument } from './mailview'
import { mediaBase } from './web'
import { notify } from './notify.svelte'
import { seen, type Showing } from './onscreen'
import { openExternal } from './open-external'
import { schemeOrigin } from './platform'
import { pref } from './prefs'
import { readingPage, withReading } from './reading'
import { reading } from './reading.svelte'
import { app, errorMessage, handle, isLocked, quietly } from './state.svelte'
import { debounce } from './store/debounce'
import { latest } from './store/latest'
import { optimisticRemove } from './store/optimistic-remove'
import { guardedRefresh } from './store/refresh'
import { needsAttention } from './unsubscribe'
import { upnext } from './upnext.svelte'
import type {
  FeedHit,
  FoundFeed,
  OpmlImportReport,
  ItemId,
  MailCategory,
  NotNewslettersReport,
  NewSource,
  NewsSource,
  NewsSourceId,
  NewsletterSender,
  PodcastGenre,
  PodcastHit,
  PodcastSettings,
  RefreshReport,
  SourceSummary,
  Story,
  StoryPart,
  StoryCursor,
  StoryId,
  StoryPage,
  StoryQuery,
  UnsubscribeOutcome,
} from './types'

/** How many stories a page holds. The service allows 500; a screen shows far fewer. */
const PAGE = 100
/** The most the service will return at once, which bounds how much a reload covers. */
const MOST = 500

/** What the Feed tints the window with: the app's own colour from the theme, so it follows the palette. */
const FEED_ACCENT = 'var(--app-feed)'

/**
 * What the sidebar has selected.
 *
 * `upnext`, `recommended` and `history` are not lists of stories this store
 * loads: they are the queue, the ranking and the record of what was read and
 * heard, which `upnext.svelte.ts` holds, and selecting one asks it to load.
 */
export type FeedScope =
  | { kind: 'all' }
  | { kind: 'folder'; name: string }
  | { kind: 'source'; id: NewsSourceId }
  | { kind: 'upnext' }
  | { kind: 'recommended' }
  | { kind: 'history' }

/** How many hits a directory search asks for. */
const DIRECTORY_LIMIT = 40

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
 * A story's body as the pane draws it: the document, whether its pictures were
 * held back, and what it was written from -- `null` when the document does not
 * say, which is every document from a service that predates transcripts. When
 * the article was read from the page itself, who wrote it and what the
 * publication calls itself, if the page said.
 */
export interface LoadedBody {
  html: string
  imagesHidden: boolean
  origin: BodyOrigin | null
  byline: string | null
  site: string | null
}

/**
 * Which chip was last on, remembered on this device.
 *
 * Chrome rather than a fact about the vault, so it is kept locally the way
 * the library's view is. Read in the constructor and not in `start`: the
 * list would otherwise draw once as Unread and visibly flip.
 */
const filterPref = pref<FeedFilter>(
  'jatayu.feed.filter',
  (raw) => FEED_FILTERS.find((f) => f === raw) ?? 'unread',
  'unread',
)

/**
 * Which kind of feed was last chosen over the sidebar, remembered on this
 * device for the reason the chip above is: it is how somebody likes to look
 * at their feeds, not a fact about the vault.
 */
const kindPref = pref<KindChoice>(
  'jatayu.feed.kind',
  (raw) => FEED_KINDS.find((k) => k === raw) ?? 'all',
  'all',
)

/**
 * Whether every newsletter in Mail has been followed for somebody once,
 * unasked -- so the Feed shows what Mail files under Newsletters without a
 * trip through Add source, and a person who then unfollows it is not
 * followed again. Kept here rather than in the vault, which has nowhere for
 * a news setting: another device may offer it once more.
 */
const newslettersPref = pref<boolean>(
  'jatayu.feed.newsletters',
  (raw) => raw === 'followed',
  false,
  () => 'followed',
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
 * scheme onto `http://jatayu.localhost` on Windows and Android, where the
 * scheme's own host has nowhere to ride but the first path segment. Under
 * server mode the shell's protocol handler forwards to `/v1/news/body/{id}`
 * on the machine holding the vault, so the interface never builds that
 * address itself.
 */
function newsOrigin(): string {
  if (isWeb) return `${mediaBase()}/v1/news`
  return schemeOrigin('news')
}

/**
 * Point a stored body's own `jatayu://news/img/…` addresses at the origin
 * this platform actually resolves.
 *
 * A body is sanitised once, when it is fetched, long before anybody knows
 * which webview will draw it -- so its picture addresses cannot be written
 * for the right platform in the first place, and are corrected here instead.
 * A no-op everywhere the scheme resolves natively.
 */
function applyNewsOrigin(html: string): string {
  const origin = newsOrigin()
  return origin === 'jatayu://news'
    ? html
    : allowWebOrigin(html.split('jatayu://news/').join(`${origin}/`))
}

/** A body as it arrives, before it is drawn for this window: see `loadBody`. */
type RawBody = Pick<LoadedBody, 'html' | 'imagesHidden' | 'origin'>

/**
 * The mock backend's stand-in for a story's document (`wrap_as` in
 * `news/view.rs`): the head that says who wrote it, and enough of `STYLE` --
 * the reading properties, the padding, the headings, quotes and pictures -- for
 * the reading settings to be seen at work in a browser tab. Mock-only, and
 * only an approximation: the real stylesheet is the service's.
 */
function mockStoryDocument(inner: string, byline: string | null, site: string | null): string {
  const quote = (v: string) => v.replaceAll('&', '&amp;').replaceAll('"', '&quot;')
  const about =
    (byline ? `<meta name="news-byline" content="${quote(byline)}">` : '') +
    (site ? `<meta name="news-site" content="${quote(site)}">` : '')
  return (
    `<!DOCTYPE html><html><head><meta charset="utf-8">${about}` +
    `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data: blob: https: http:; style-src 'unsafe-inline'">` +
    `<base target="_blank"><style>` +
    `:root{--read-font:sans-serif;--read-size:18px;--read-leading:1.6}` +
    `body{margin:0;padding:20px clamp(16px,4vw,28px) 28px;font-family:var(--read-font);` +
    `font-size:var(--read-size);line-height:var(--read-leading);overflow-wrap:break-word}` +
    `body>:first-child{margin-top:0}h1,h2,h3{margin:1.5em 0 .5em;line-height:1.25;font-weight:650}` +
    `h2{font-size:1.3em}h3{font-size:1.12em}img{max-width:100%;height:auto}figure{margin:1.5em 0}` +
    `figure.lead{margin:0 0 1.5em}figcaption{margin-top:.5em;font-size:.8em;opacity:.72}` +
    `blockquote{margin:1.25em 0;padding-left:1em;border-left:3px solid rgba(128,128,128,.4);opacity:.88}` +
    `a{color:inherit;text-underline-offset:.18em}` +
    `</style></head><body>${inner}</body></html>`
  )
}

class FeedState {
  // -- what is on screen ------------------------------------------------
  scope = $state<FeedScope>({ kind: 'all' })
  filter = $state<FeedFilter>('unread')
  sort = $state<FeedSort>('newest')
  /**
   * Which kind of feed the sidebar shows: everything, newsletters, feeds or
   * podcasts. It filters the rows, and narrows All to the stories of that
   * kind -- see `#queryFor`.
   */
  kind = $state<KindChoice>('all')
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
  /**
   * How many times each story's body was replaced under the pane: an article
   * written from a transcript changes the body while `hasBody` stays true,
   * which is all the pane would otherwise have to go on. See `bodyStamp`.
   */
  bodyStamps = $state<Record<StoryId, number>>({})

  // -- picking several sources ------------------------------------------
  /**
   * The sidebar is picking sources rather than opening them, for what is done
   * to several at once: unsubscribing, or saying they are not newsletters.
   */
  selecting = $state(false)
  /** The sources picked. Raw, and replaced whole rather than changed in place. */
  checkedSources = $state.raw<ReadonlySet<NewsSourceId>>(new Set())
  /**
   * What unsubscribing left for the person to do: pages to finish in the
   * browser, and senders it could not reach. Kept here and not by whoever
   * asked, since a source's settings close as the source goes; the sidebar
   * shows it until it is put away.
   */
  unsubscribeResults = $state.raw<readonly UnsubscribeOutcome[] | null>(null)

  // -- the directory: podcasts and feeds to follow ----------------------
  /** The directory sheet is open. */
  directoryOpen = $state(false)
  directoryTab = $state<'podcasts' | 'feeds'>('podcasts')
  podcastHits = $state<PodcastHit[]>([])
  feedHits = $state<FeedHit[]>([])
  /** The directory's genres, for the charts. Empty until `loadGenres`. */
  genres = $state<PodcastGenre[]>([])
  directoryLoading = $state(false)
  /** Why the last search or chart could not be had. Said beside the results. */
  directoryError = $state<string | null>(null)

  /** Which list load is the current one. See `#load`. */
  #generation = latest()
  /** The same for the sources, which are reloaded after almost every write. */
  #sourcesGeneration = latest()
  /** The same for the directory: a slow chart must not land over a newer search. */
  #directoryGeneration = latest()
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
    // An episode arrived in the files folder or was removed from it, by a
    // download finishing or a button: those stories' files changed. Patched
    // in place rather than by reloading the list, which could land over a
    // selection or a chip somebody changed while the download ran.
    upnext.onEpisodeChange((ids, change) => void this.#refetch(ids, change === 'body'))
    this.filter = filterPref.get()
    this.kind = kindPref.get()
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
    // Which episodes are on their way, for the buttons on a show's rows.
    if (this.podcastSources.length > 0) void upnext.refreshDownloads()
    // After the list, not before it: following reads a month of mail.
    this.following = this.#followNewslettersOnce()
  }

  /** The follow `start` began, if it did -- for a test to wait on. */
  following: Promise<void> | null = null

  /** Follow every newsletter in Mail, the first time there is mail to read
   *  them from -- see `newslettersPref`. Quietly: nothing is selected, and a
   *  failure is tried again the next time the Feed opens. `add_news_source`
   *  reads the first issues before it answers, so nothing else is fetched. */
  async #followNewslettersOnce(): Promise<void> {
    if (newslettersPref.get() || !this.mailAvailable) return
    if (this.sources.some((s) => s.source.kind.type === 'newsletters')) {
      newslettersPref.set(true)
      return
    }
    await accounts.load()
    if (!accounts.list.some((a) => a.services.mail)) return
    try {
      await api.addNewsSource({ type: 'newsletters' })
      newslettersPref.set(true)
      await this.#loadSources(true)
      await this.#load(false)
    } catch (e) {
      await quietly(e)
    }
  }

  /**
   * Drop everything. Registered with `app.onLock` in the constructor.
   *
   * The chips stay: they are chrome, not content. The rest is a stranger's
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
    this.bodyStamps = {}
    this.selecting = false
    this.checkedSources = new Set()
    this.unsubscribeResults = null
    this.#detached = null
    this.#refreshes = 0
    // The directory's answers are public, but which of them are already
    // followed is not, and a sheet left open should not greet the next vault.
    this.#directoryGeneration.next()
    this.directoryOpen = false
    this.directoryTab = 'podcasts'
    this.podcastHits = []
    this.feedHits = []
    this.genres = []
    this.directoryLoading = false
    this.directoryError = null
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
    // A folder the chip leaves nothing of is not in the sidebar to be opened
    // from, but can be asked for -- from the Overview, say. The place asked
    // for wins over the filter that was on: an empty list under "Podcasts"
    // would say the folder was caught up.
    if (
      scope.kind === 'folder' &&
      this.kind !== 'all' &&
      this.sources.length > 0 &&
      scopeSources(scope, this.sources, this.kind).length === 0
    ) {
      this.kind = 'all'
      kindPref.set('all')
    }
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

  /**
   * Show one kind of feed, or all of them.
   *
   * What it changes on screen is the sidebar's rows, always, and the list when
   * that is All or a folder -- a source is what it is whatever the chip says.
   * A folder with nothing of the kind left in it is gone from the sidebar, so
   * the list goes back to All rather than stay on a place that is not there.
   */
  async setKind(kind: KindChoice): Promise<void> {
    if (kind === this.kind) return
    this.kind = kind
    kindPref.set(kind)
    if (this.#scopeGone()) this.scope = { kind: 'all' }
    const scope = this.scope.kind
    if (scope !== 'all' && scope !== 'folder') return
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
      this.#knowEpisodes(page.stories)
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

  /**
   * Open a story in the list of its own source, which is what somebody who
   * found it somewhere else -- History, a link from another app -- wants: the
   * story with its neighbours beside it, and not a pane over a page that has
   * no list. Answers whether there was a story to open: a record outlives its
   * story, and a source unfollowed since takes its stories with it.
   */
  async openInSource(id: StoryId): Promise<boolean> {
    let found: Story
    try {
      found = await api.story(id)
    } catch (e) {
      // Not found is an answer here, not a failure; a locked vault is the lock screen's.
      if (isLocked(e)) await handle(e)
      return false
    }
    await this.select({ kind: 'source', id: found.sourceId })
    await this.open(id)
    return true
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
      // Or a source that changed kind: under a chip, All is only the stories of one.
      const scope = this.scope.kind
      if (scope === 'folder' || (scope === 'all' && this.kind !== 'all')) await this.#load(false)
    } catch (e) {
      await handle(e)
    }
  }

  /** Unfollow, taking its stories with it. */
  async deleteSource(id: NewsSourceId): Promise<void> {
    await optimisticRemove({
      optimistic: () => this.#dropSources([id]),
      call: () => this.#track(api.deleteNewsSource(id)),
      onSuccess: () => void this.#reloadAll(),
      rollback: () => this.#reloadAll(),
    })
  }

  /**
   * Stop getting each of `ids`: a newsletter's sender is asked to take you
   * off the list and it leaves the Feed, and a feed or a show is unfollowed.
   *
   * Off the screen at once, the way `deleteSource` is, and put back if the
   * service refuses -- then the answer is an empty list, and the reason has
   * been said. Asking can take a while, since each sender is asked over the
   * network, so the toast saying how it went comes when it is over, and
   * whatever is left for the person to do is kept in `unsubscribeResults`.
   * Picking ends either way: what was picked has been dealt with.
   */
  async unsubscribeSources(ids: readonly NewsSourceId[]): Promise<UnsubscribeOutcome[]> {
    const wanted = this.#bulkTargets(ids)
    this.stopSelecting()
    if (wanted.length === 0) return []
    // Counted before they leave the screen, which is where their kinds are read from.
    const letters = wanted.filter((id) => this.#isNewsletter(id)).length
    this.#dropSources(wanted)
    let outcomes: UnsubscribeOutcome[]
    try {
      outcomes = await this.#track(api.unsubscribeNewsSources(wanted))
    } catch (e) {
      await handle(e, () => this.#reloadAll())
      return []
    }
    await this.#reloadAll()
    const said = unsubscribedNote(outcomes, letters, wanted.length - letters)
    const opts = { body: said.body, key: 'feed-unsubscribed' }
    if (said.ok) notify.success(said.title, opts)
    else notify.warn(said.title, opts)
    if (needsAttention(outcomes)) this.unsubscribeResults = outcomes
    return outcomes
  }

  /** Put away what `unsubscribeSources` left for the person to do. */
  dismissUnsubscribeResults(): void {
    this.unsubscribeResults = null
  }

  /**
   * Say these newsletters are not newsletters: out of the Feed, not found
   * again, and with a `category` their emails moved there in Mail.
   *
   * Only one sender's newsletter can be said not to be one, so anything else
   * among `ids` is left where it is. Off the screen at once, and back if the
   * service refuses. Picking ends either way.
   */
  async markNotNewsletters(
    ids: readonly NewsSourceId[],
    category: MailCategory | null,
  ): Promise<void> {
    const wanted = this.#bulkTargets(ids).filter((id) => this.#isNewsletter(id))
    this.stopSelecting()
    if (wanted.length === 0) return
    const names = this.sources
      .filter((s) => wanted.includes(s.source.id))
      .map((s) => sourceLabel(s.source))
    this.#dropSources(wanted)
    let report: NotNewslettersReport
    try {
      report = await this.#track(api.markNotNewsletters(wanted, category))
    } catch (e) {
      await handle(e, () => this.#reloadAll())
      return
    }
    await this.#reloadAll()
    const said = notNewslettersNote(names, category, report)
    notify.success(said.title, { body: said.body, key: 'feed-not-newsletters' })
  }

  /**
   * Take sources off the screen before the service has been asked: their
   * rows, their stories, the pane if it shows one of theirs, the place on
   * screen if it was one of them, and the picking of them.
   */
  #dropSources(ids: readonly NewsSourceId[]): void {
    const gone = new Set(ids)
    // The pane goes with its source: a story with nothing to belong to
    // would be the last of it left on screen.
    const open = this.story
    if (open && gone.has(open.sourceId)) this.close()
    this.sources = this.sources.filter((s) => !gone.has(s.source.id))
    this.stories = this.stories.filter((s) => !gone.has(s.sourceId))
    const scope = this.scope
    if (scope.kind === 'source' && gone.has(scope.id)) this.scope = { kind: 'all' }
    this.#pruneChecked()
  }

  /** `ids` once each, without the Newsletters source, which no bulk action takes. */
  #bulkTargets(ids: readonly NewsSourceId[]): NewsSourceId[] {
    return [...new Set(ids)].filter((id) => {
      const known = this.sources.find((s) => s.source.id === id)
      return !known || isPickable(known.source)
    })
  }

  /** Is `id` one sender's newsletter, as the sidebar knows it? */
  #isNewsletter(id: NewsSourceId): boolean {
    return this.sources.some((s) => s.source.id === id && s.source.kind.type === 'mail')
  }

  // ── picking several sources ──────────────────────────────────────────

  /** Start picking sources in the sidebar instead of opening them. */
  startSelecting(): void {
    this.selecting = true
  }

  /** Stop picking, and let go of whatever was picked. */
  stopSelecting(): void {
    this.selecting = false
    this.clearChecked()
  }

  /** Pick a source, or put it back. The Newsletters source cannot be picked. */
  toggleSource(id: NewsSourceId): void {
    const next = new Set(this.checkedSources)
    if (next.has(id)) next.delete(id)
    else if (this.#pickable(id)) next.add(id)
    else return
    this.checkedSources = next
  }

  /** Pick exactly these: the rows the sidebar is showing, for its "all" box. */
  checkAllShown(ids: readonly NewsSourceId[]): void {
    this.checkedSources = new Set(ids.filter((id) => this.#pickable(id)))
  }

  /** Let go of every source picked, and go on picking. */
  clearChecked(): void {
    if (this.checkedSources.size > 0) this.checkedSources = new Set()
  }

  /** The sources picked, in the sidebar's order. */
  get checkedSummaries(): SourceSummary[] {
    return this.sources.filter((s) => this.checkedSources.has(s.source.id))
  }

  /** Is `id` a source the sidebar has, and one that can be picked? */
  #pickable(id: NewsSourceId): boolean {
    const known = this.sources.find((s) => s.source.id === id)
    return !!known && isPickable(known.source)
  }

  /**
   * Let go of picked sources that are no longer followed -- unfollowed here
   * or on another machine -- so nothing is done to one that is not there.
   */
  #pruneChecked(): void {
    if (this.checkedSources.size === 0) return
    const kept = new Set<NewsSourceId>()
    for (const s of this.sources) if (this.checkedSources.has(s.source.id)) kept.add(s.source.id)
    if (kept.size !== this.checkedSources.size) this.checkedSources = kept
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

  // ── podcasts ─────────────────────────────────────────────────────────

  /** Whether a source is a podcast: a feed whose items are episodes. */
  isPodcast(source: SourceSummary | NewsSource): boolean {
    const record = 'source' in source ? source.source : source
    return !!record.podcast
  }

  /**
   * Change one show's own say over downloads and keeping. `null` takes every
   * override away -- the show follows the Feed's defaults again -- and keeps it
   * a podcast, with what the directory said about it.
   */
  async savePodcastSettings(
    sourceId: NewsSourceId,
    settings: PodcastSettings | null,
  ): Promise<void> {
    const found = this.sources.find((s) => s.source.id === sourceId)?.source
    if (!found) return
    const before = found.podcast
    const podcast: PodcastSettings = settings
      ? $state.snapshot(settings)
      : { author: before?.author ?? '', directoryId: before?.directoryId ?? null }
    await this.saveSource({ ...$state.snapshot(found), podcast })
  }

  // ── the directory ────────────────────────────────────────────────────

  /**
   * Open the sheet that finds podcasts and feeds to follow. Opening it asks the
   * directory for its charts and genres: it is asked for, on purpose, and the
   * answer is public.
   */
  openDirectory(tab?: 'podcasts' | 'feeds'): void {
    if (tab) this.directoryTab = tab
    this.directoryOpen = true
    this.directoryError = null
    void this.loadGenres()
    if (this.directoryTab === 'podcasts' && this.podcastHits.length === 0) {
      void this.topPodcasts(null)
    }
  }

  closeDirectory(): void {
    this.directoryOpen = false
    // A search still on its way has nobody to land for.
    this.#directoryGeneration.next()
    this.directoryLoading = false
  }

  /** Podcasts matching `query`, best first. Nothing typed is the charts. */
  async searchPodcasts(query: string): Promise<void> {
    const text = query.trim()
    if (!text) {
      await this.topPodcasts(null)
      return
    }
    await this.#lookUp(
      () => api.searchPodcasts(text, DIRECTORY_LIMIT),
      (hits) => (this.podcastHits = hits ?? []),
    )
  }

  /** The charts, overall or in one genre (`PodcastGenre.id`). */
  async topPodcasts(genre: number | null): Promise<void> {
    await this.#lookUp(
      () => api.topPodcasts(genre, DIRECTORY_LIMIT),
      (hits) => (this.podcastHits = hits ?? []),
    )
  }

  /** The directory's genres, once. */
  async loadGenres(): Promise<void> {
    if (this.genres.length > 0 || !app.supportsNews) return
    try {
      this.genres = await api.podcastGenres()
    } catch (e) {
      // Only a row of chips is missing.
      await quietly(e)
    }
  }

  /**
   * Feeds matching `query`. An address pasted goes through discovery, which
   * reads the page for the feeds it advertises, rather than to a search that
   * knows only names.
   */
  async searchFeeds(query: string): Promise<void> {
    const text = query.trim()
    if (!text) {
      this.#directoryGeneration.next()
      this.feedHits = []
      this.directoryError = null
      this.directoryLoading = false
      return
    }
    await this.#lookUp(
      async () => {
        // A whole address is looked up for the feeds it names; words are searched.
        if (!isWebUrl(text)) return api.searchFeeds(text, DIRECTORY_LIMIT)
        return (await api.discoverFeeds(text)).map((f) => this.#hitFor(f))
      },
      (hits) => (this.feedHits = hits ?? []),
    )
  }

  /** Follow a show the directory found. Stays where it is: the sheet is for finding. */
  async followPodcast(hit: PodcastHit): Promise<NewsSource | null> {
    const followed = await this.#follow({
      type: 'podcast',
      url: hit.feedUrl,
      title: hit.title,
      directoryId: hit.directoryId,
      author: hit.author,
    })
    if (followed) {
      this.podcastHits = this.podcastHits.map((h) =>
        h.feedUrl === hit.feedUrl ? { ...h, followed: true } : h,
      )
    }
    return followed
  }

  /** Follow a feed a search found. */
  async followFeed(hit: FeedHit): Promise<NewsSource | null> {
    const followed = await this.#follow({ type: 'web', url: hit.feedUrl, title: hit.title })
    if (followed) {
      this.feedHits = this.feedHits.map((h) =>
        h.feedUrl === hit.feedUrl ? { ...h, followed: true } : h,
      )
    }
    return followed
  }

  /** A feed discovery found, drawn as a search hit: all it knows is the address. */
  #hitFor(found: FoundFeed): FeedHit {
    return {
      feedUrl: found.url,
      title: found.title,
      siteUrl: '',
      description: '',
      icon: '',
      followed: this.sources.some(
        (s) => s.source.kind.type === 'web' && sameFeedAddress(s.source.kind.url, found.url),
      ),
    }
  }

  /** Add a source and ask for its first stories, without leaving the sheet. */
  async #follow(input: NewSource): Promise<NewsSource | null> {
    try {
      const added = await this.#track(api.addNewsSource(input))
      await this.#loadSources(true)
      // A reload that was already on its way may have answered first, without
      // the new source; the sheet marks a hit followed by finding it here.
      if (!this.sources.some((s) => s.source.id === added.id)) await this.#loadSources(true)
      // Nothing is in it yet, and the refresher's own timer could be minutes away.
      void this.refresh(added.id)
      return added
    } catch (e) {
      await handle(e)
      return null
    }
  }

  /**
   * One directory request, numbered like the list is: only the newest lands,
   * a failure is said beside the results and not over the window, and what was
   * showing is cleared (`land(null)`) so an error never sits under stale
   * answers.
   */
  async #lookUp<T>(fetch: () => Promise<T>, land: (result: T | null) => void): Promise<void> {
    if (!app.supportsNews) return
    this.directoryError = null
    const token = this.#directoryGeneration.next()
    this.directoryLoading = true
    try {
      const result = await fetch()
      if (this.#directoryGeneration.isCurrent(token)) land(result)
    } catch (e) {
      if (!this.#directoryGeneration.isCurrent(token)) return
      if (isLocked(e)) {
        await handle(e)
        return
      }
      land(null)
      this.directoryError = errorMessage(e)
    } finally {
      if (this.#directoryGeneration.isCurrent(token)) this.directoryLoading = false
    }
  }

  // ── derived ──────────────────────────────────────────────────────────

  /** Every podcast followed. */
  get podcastSources(): SourceSummary[] {
    return this.sources.filter((s) => this.isPodcast(s))
  }

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

  /**
   * The sidebar's folders, unfiled first, each with its unread total: the
   * sources under the chip, bar an empty Newsletters source -- see
   * `listedSources`.
   */
  get folders(): FeedFolder[] {
    return groupFolders(listedSources(this.sources, this.kind, this.source?.source.id ?? null))
  }

  /** What All holds unread: every source, or those of the kind chosen. */
  get unreadTotal(): number {
    return scopeUnread({ kind: 'all' }, this.sources, this.kind)
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
    // Up Next, Recommended and History are not narrowed by the chips, which say nothing there.
    const chips =
      scope.kind !== 'upnext' && scope.kind !== 'recommended' && scope.kind !== 'history'
    const words = [
      scope.kind === 'upnext'
        ? 'Up Next'
        : scope.kind === 'recommended'
          ? 'Recommended'
          : scope.kind === 'history'
            ? 'History'
            : null,
      scope.kind === 'all' && this.kind !== 'all' ? `${KIND_LABELS[this.kind]} only` : null,
      scope.kind === 'folder' ? 'a folder' : null,
      chips && this.filter === 'unread' ? 'unread only' : null,
      chips && this.filter === 'saved' ? 'saved' : null,
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

  /**
   * The address the reading pane's frame loads a story's body from. With a
   * `part` it is one document of the pane -- an episode's notes or transcript,
   * or a summary -- and not the whole body; the route answers not found for a
   * transcript or a summary that is not there.
   */
  bodyUrl(story: Story, part?: StoryPart): string {
    const url = `${newsOrigin()}/body/${encodeURIComponent(story.id)}`
    return part ? `${url}?part=${part}` : url
  }

  /**
   * A story's body as a whole document for the sandboxed frame, and whether
   * its pictures were held back. Works against the mock backend too, which
   * has no `jatayu://` protocol to fetch from. `null` when it could not be
   * loaded (the reason is shown the usual way).
   *
   * The document is already drawn for the window's light or dark theme and
   * its picture addresses already point at this platform's origin, so the
   * pane has only to assign it to `srcdoc`. A story that is not a newsletter
   * is also set in the reading settings' type, on their page when they chose
   * one (`reading.ts`), so a pane loads its documents again when those change,
   * as it does when the theme does.
   *
   * The pane asks again when the story's `hasBody` changes, which is what
   * `fetchFullArticle` flips: the address is the same and the body is not.
   * With a `part`, one of the pane's tabs instead -- see `bodyUrl`.
   */
  async loadBody(story: Story, part?: StoryPart): Promise<LoadedBody | null> {
    try {
      const loaded = isMock ? await this.#mockBody(story, part) : await this.#fetchBody(story, part)
      // A newsletter is mail's document, designed down to its fonts as often
      // as not, and keeps the window's theme and its own type.
      const settings = story.mailMessage ? null : reading.settings
      const page = settings ? readingPage(settings) : null
      const themed = applyBodyTheme(loaded.html, page?.dark ?? app.dark, page?.frame ?? app.frame)
      return {
        html: applyNewsOrigin(settings ? withReading(themed, settings) : themed),
        imagesHidden: loaded.imagesHidden,
        origin: loaded.origin,
        ...bodyAboutOf(loaded.html),
      }
    } catch (e) {
      await handle(e)
      return null
    }
  }

  /**
   * How many times `id`'s body has been replaced since it was first seen. A
   * pane that reads this in the effect that loads the body loads it again when
   * an article is written from a transcript, whose address, `hasBody` and
   * everything else about the story are as they were.
   */
  bodyStamp(id: StoryId): number {
    return this.bodyStamps[id] ?? 0
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
  async #fetchBody(story: Story, part?: StoryPart): Promise<RawBody> {
    const res = await fetch(this.bodyUrl(story, part), { cache: 'no-store' })
    if (!res.ok) throw new Error(`could not load the story (${res.status})`)
    const imagesHidden = res.headers.get('x-news-images-hidden') === '1'
    const html = await res.text()
    return { html, imagesHidden, origin: bodyOriginOf(html) }
  }

  /**
   * The mock backend's answer, in the same frame mock mail uses. Asked for by
   * name, as the dev-only mail triggers are: no real command is behind it, so
   * the generated client has nothing to type it from.
   */
  async #mockBody(story: Story, part?: StoryPart): Promise<RawBody> {
    const body = await callCommand<{
      html: string
      imagesHidden: boolean
      origin?: BodyOrigin | null
      byline?: string | null
      site?: string | null
    }>('mock_story_body', { id: story.id, part })
    if (story.mailMessage) {
      const framed = bodyDocument(story.id, { html: body.html, imagesHidden: body.imagesHidden })
      return {
        html: framed.html ?? '',
        imagesHidden: framed.imagesHidden ?? false,
        origin: body.origin ?? null,
      }
    }
    return {
      html: mockStoryDocument(body.html, body.byline ?? null, body.site ?? null),
      imagesHidden: body.imagesHidden,
      origin: body.origin ?? null,
    }
  }

  // ── loading ──────────────────────────────────────────────────────────

  /**
   * The sources in a folder, of the kind chosen. Compared trimmed, as the
   * sidebar groups them: a folder lists what the chip leaves of it, and opens
   * as just that.
   */
  #inFolder(name: string): SourceSummary[] {
    return scopeSources({ kind: 'folder', name }, this.sources, this.kind)
  }

  /** The sources the scope on screen covers, for a service call that takes them by id. */
  #scopeSourceIds(): NewsSourceId[] {
    const scope = this.scope
    // Not looked up: a source can be opened before the sidebar has heard of it.
    if (scope.kind === 'source') return [scope.id]
    return scopeSources(scope, this.sources, this.kind).map((s) => s.source.id)
  }

  /**
   * The query for the list on screen, or `null` when it can show nothing.
   *
   * A folder with no sources in it is nothing, not everything: sent as an
   * empty list the service would read it as "every source", which is the
   * opposite of what a folder somebody has just emptied should show. The same
   * goes for All under a chip when none of that kind is followed.
   *
   * All under a chip is sent by source, since the service has no kind to
   * filter on; the same ids `markAllRead` sweeps and the sidebar counts.
   */
  #queryFor(before: StoryCursor | null, limit: number): StoryQuery | null {
    const query: StoryQuery = { limit }
    if (this.filter === 'unread') query.unreadOnly = true
    if (this.filter === 'saved') query.savedOnly = true
    if (before) query.before = before
    if (this.scope.kind !== 'all' || this.kind !== 'all') {
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
      this.#pruneChecked()
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
        const scope = this.scope
        if (scope.kind === 'upnext' || scope.kind === 'recommended' || scope.kind === 'history') {
          // Not stories: the queue, the ranking and the record of what was
          // read and heard, which have a store of their own. The list is
          // empty so nothing from the last scope shows.
          this.#land([], null)
          if (scope.kind === 'upnext') await upnext.load()
          else if (scope.kind === 'recommended') await upnext.loadRecommended()
          // A reload keeps as many records as are loaded; a navigation starts over.
          else await upnext.loadHistory(!fresh)
          return
        }
        const covering = fresh ? PAGE : Math.min(MOST, Math.max(PAGE, this.stories.length))
        const query = this.#queryFor(null, covering)
        const page: StoryPage = query ? await api.stories(query) : { stories: [], next: null }
        if (!isCurrent()) return
        this.#land(page.stories, page.next ?? null)
        this.#knowEpisodes(page.stories)
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

  /**
   * Episodes on screen show how far they were taken, which only the record of
   * what was heard knows; Up Next and Recommended carry theirs, but a plain
   * row of a show does not. So a list with episodes in it asks for the recent
   * record once -- a few hundred small rows, which the store throttles.
   */
  #knowEpisodes(stories: Story[]): void {
    if (stories.some((s) => s.media)) void upnext.loadConsumed()
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
   * Read these stories again and patch what changed in place, if they are
   * shown. `body` says the body was replaced as well, which the pane is told
   * by a stamp since nothing about the story's own fields says so.
   */
  async #refetch(ids: StoryId[], body = false): Promise<void> {
    for (const id of ids) {
      if (!this.#find(id)) continue
      try {
        const fresh = await api.story(id)
        this.#patch(id, { files: fresh.files, hasBody: fresh.hasBody })
        if (body) this.bodyStamps = { ...this.bodyStamps, [id]: this.bodyStamp(id) + 1 }
      } catch (e) {
        await quietly(e)
      }
    }
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
