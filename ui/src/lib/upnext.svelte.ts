// Up Next, Recommended, downloads and Settings → Feed: the data behind the
// player's queue, the Feed's Up Next and Recommended pages, and the episode
// buttons everywhere. See `docs/plans/podcasts.md`.
//
// The service is the truth: everything here is a copy, reloaded when a
// `queue`, `consumption` or `story` change arrives (`live.svelte.ts`), and
// while a download is under way the list of downloads is polled.
//
// Three rules shape most of what is below:
//
//   1. A write is shown at once and put right by the service afterwards. A
//      removal or a move changes the list on the screen before the call is
//      answered, and a refusal reloads it, so the list never keeps a lie.
//   2. A read lands only if nothing newer was asked for since. Up Next is
//      reloaded after nearly every write, and two answers can cross.
//   3. Nothing runs by itself. The one timer is the download poll, armed only
//      while an episode is waiting or downloading and disarmed the moment none
//      is; nothing starts at load.
//
// CONTRACT (laid by the supervisor; agents G1 and G2 code against the public
// names): every public name and signature below is fixed; extras are listed
// at the end of the class.

import { api } from './api'
import { app, errorMessage, handle, isLocked, quietly } from './state.svelte'
import { debounce } from './store/debounce'
import { latest } from './store/latest'
import { guardedRefresh } from './store/refresh'
import type {
  ConsumeMode,
  Consumption,
  ContentRef,
  DownloadStatus,
  FeedSettings,
  ProgressReport,
  QueueEntryId,
  QueueItem,
  QueuePlace,
  RankingStatus,
  Recommendation,
  StoryId,
} from './types'

/** How often the downloads are asked after while one is under way. */
const DOWNLOAD_POLL_MS = 1000

/** How many recommendations a page asks for. The service ranks far fewer than it stores. */
const RECOMMENDED = 60

/** How long `loadConsumed` leaves the record alone after reading it, unless it is made to. */
const CONSUMED_THROTTLE_MS = 2000

/** How many of the most recently touched records the lookup holds beyond Up Next's own. */
const CONSUMED_LIMIT = 200

/**
 * How long a burst of `consumption` changes is gathered before the lists are
 * read again. Another device reports where it is every fifteen seconds, and a
 * reload for each would be a reload of the whole queue for a progress bar.
 */
const CONSUMPTION_SETTLE_MS = 1500

/** After a re-rank, how often the status is asked for, and how many times. The
 *  ranker raises no change as it works, so this is the only way the page sees
 *  scores arrive; the cap is for a judge that never finishes. */
const RERANK_POLL_MS = 4000
const RERANK_POLLS = 30

/**
 * Let a process end with `timer` still waiting. A browser's timer is a number
 * with no such notion; a test runner's is an object that would otherwise hold
 * it open for the seconds a mock download takes to finish.
 */
function unheld(timer: ReturnType<typeof setTimeout>): ReturnType<typeof setTimeout> {
  ;(timer as unknown as { unref?: () => void }).unref?.()
  return timer
}

/** What a thing is called in a lookup: `story:<id>`, as the service's `ContentRef::key`. */
export function refKey(what: ContentRef): string {
  return `${what.type}:${what.id}`
}

/** Whether two references are the same thing. */
export function sameRef(a: ContentRef, b: ContentRef): boolean {
  return a.type === b.type && a.id === b.id
}

class UpNext {
  /** Up Next, in order. */
  items = $state<QueueItem[]>([])
  loading = $state(false)
  error = $state<string | null>(null)
  /** Recommended, best first. */
  recommended = $state<Recommendation[]>([])
  recommendedLoading = $state(false)
  ranking = $state<RankingStatus | null>(null)
  /** Settings → Feed; `null` until loaded. */
  settings = $state<FeedSettings | null>(null)
  /** Episodes downloading, waiting, or failed. */
  downloads = $state<DownloadStatus[]>([])

  /**
   * What is known of how far each thing was taken in, by `refKey`: what Up
   * Next's rows carry, what the player was told and what a report answered.
   * Replaced whole rather than edited, so a reader of one key is told when any
   * changes -- the cost is a copy of a small table.
   */
  #known = $state<Record<string, Consumption>>({})
  /** How far a recommendation is, by `refKey`, where it is all the list says. */
  #recommendedProgress = $state<Record<string, number>>({})
  /**
   * Who came after an entry that has since left Up Next. Finishing a thing
   * takes it out of the queue, and the player asks "what is after this?" once
   * it has ended; without this the answer would be the head of the queue.
   */
  #successors = new Map<string, string>()

  #itemsGeneration = latest()
  #recommendedGeneration = latest()
  #downloadsGeneration = latest()
  #settingsGeneration = latest()
  #itemsLoaded = false
  #recommendedLoaded = false
  #consumedLoaded = false
  #consumedAt = 0
  #consumedGeneration = latest()
  #pollTimer: ReturnType<typeof setTimeout> | null = null
  #rerankTimer: ReturnType<typeof setTimeout> | null = null
  /** Whom to tell that an episode changed under the Feed's own list. */
  #episodeListeners = new Set<(stories: StoryId[]) => void>()
  #settle = debounce(() => void this.#reloadForConsumption(), CONSUMPTION_SETTLE_MS)

  constructor() {
    // Registered once, here, for the reason the Feed gives: a hook added by a
    // view would be added again each time it mounted.
    app.onLock(() => this.reset())
  }

  // ── lifecycle ────────────────────────────────────────────────────────

  /**
   * Drop everything. A queue and a history are a record of what somebody
   * reads and hears, which is what a lock exists to put away.
   */
  reset(): void {
    this.#itemsGeneration.next()
    this.#recommendedGeneration.next()
    this.#downloadsGeneration.next()
    this.#settingsGeneration.next()
    this.#consumedGeneration.next()
    this.#disarmPoll()
    if (this.#rerankTimer) clearTimeout(this.#rerankTimer)
    this.#rerankTimer = null
    this.#settle.cancel()
    this.items = []
    this.loading = false
    this.error = null
    this.recommended = []
    this.recommendedLoading = false
    this.ranking = null
    this.settings = null
    this.downloads = []
    this.#known = {}
    this.#recommendedProgress = {}
    this.#successors.clear()
    this.#itemsLoaded = false
    this.#recommendedLoaded = false
    this.#consumedLoaded = false
    this.#consumedAt = 0
  }

  /** Up Next has been read since the vault opened. */
  get loaded(): boolean {
    return this.#itemsLoaded
  }

  // ── reading ──────────────────────────────────────────────────────────

  /** Reload Up Next. */
  async load(): Promise<void> {
    if (!app.supportsNews) return
    await guardedRefresh(
      this.#itemsGeneration,
      async (isCurrent) => {
        const view = await api.listQueue()
        if (!isCurrent()) return
        this.items = view.items
        this.error = null
        this.#itemsLoaded = true
        this.#learnItems()
        // An episode the service is fetching for an entry -- asked for when it
        // was added, perhaps by a show on its own -- is news to this window.
        if (view.items.some((i) => i.downloading) && this.#pollTimer === null) {
          void this.refreshDownloads()
        }
      },
      { setLoading: (v) => (this.loading = v), onError: (e) => this.#failed(e) },
    )
  }

  /** Reload Recommended and the ranking's status. */
  async loadRecommended(): Promise<void> {
    if (!app.supportsNews) return
    await guardedRefresh(
      this.#recommendedGeneration,
      async (isCurrent) => {
        const [list, status] = await Promise.all([
          api.recommended(RECOMMENDED),
          api.rankingStatus(),
        ])
        if (!isCurrent()) return
        this.recommended = list
        this.ranking = status
        this.#recommendedLoaded = true
        this.#learnRecommended()
      },
      { setLoading: (v) => (this.recommendedLoading = v), onError: (e) => this.#failed(e) },
    )
  }

  async loadSettings(): Promise<void> {
    if (!app.supportsNews) return
    const token = this.#settingsGeneration.next()
    try {
      const settings = await api.feedSettings()
      if (this.#settingsGeneration.isCurrent(token)) this.settings = settings
    } catch (e) {
      // A settings page that could not be read says so itself; nothing else
      // needs them to draw.
      await quietly(e)
    }
  }

  /** Save Settings → Feed; answers whether it was saved. */
  async saveSettings(settings: FeedSettings): Promise<boolean> {
    // A save that lands retires a read still on its way: it would put back
    // what was just replaced.
    this.#settingsGeneration.next()
    try {
      this.settings = await api.saveFeedSettings($state.snapshot(settings))
    } catch (e) {
      await handle(e)
      return false
    }
    // The criteria, the window and the switch all change what is offered.
    void this.loadRecommended()
    return true
  }

  // ── Up Next ──────────────────────────────────────────────────────────

  /** Put `what` in Up Next (at the end unless `place` says). */
  async add(what: ContentRef, place?: QueuePlace): Promise<void> {
    try {
      await api.addToQueue(what, place)
    } catch (e) {
      await handle(e)
      return
    }
    this.#flagRecommended(what, true)
    // The row needs the story, the file and the playable, which only the
    // service puts together.
    await this.load()
    // An episode that was not downloaded is asked for as it is queued.
    await this.refreshDownloads()
  }

  async remove(id: QueueEntryId): Promise<void> {
    const gone = this.items.find((i) => i.entry.id === id)
    if (!gone) return
    this.#leave(gone)
    try {
      await api.removeFromQueue([id])
    } catch (e) {
      await handle(e, () => this.load())
    }
  }

  /** Take `what` out of Up Next, wherever it is. */
  async removeFor(what: ContentRef): Promise<void> {
    const entry = this.itemFor(what)
    if (entry) await this.remove(entry.entry.id)
  }

  /** Move an entry to just before `before`, or to the end. */
  async move(id: QueueEntryId, before: QueueEntryId | null): Promise<void> {
    const from = this.items.findIndex((i) => i.entry.id === id)
    if (from < 0 || id === before) return
    const rest = this.items.filter((i) => i.entry.id !== id)
    const at = before === null ? rest.length : rest.findIndex((i) => i.entry.id === before)
    if (at < 0) return
    const moved = this.items[from]!
    this.items = [...rest.slice(0, at), moved, ...rest.slice(at)]
    try {
      await api.moveInQueue(id, before)
    } catch (e) {
      await handle(e, () => this.load())
    }
  }

  isQueued(what: ContentRef): boolean {
    return this.itemFor(what) !== undefined
  }

  itemFor(what: ContentRef): QueueItem | undefined {
    return this.items.find((i) => sameRef(i.entry.what, what))
  }

  /** The entry after `what` (the first when `what` is absent or not queued). */
  after(what: ContentRef | null): QueueItem | undefined {
    if (!what) return this.items[0]
    const at = this.items.findIndex((i) => sameRef(i.entry.what, what))
    if (at >= 0) return this.items[at + 1]
    // Not queued, which for a thing that has just been heard to the end is
    // because finishing took it out: its successor was written down then.
    const successor = this.#successors.get(refKey(what))
    const kept = successor ? this.items.find((i) => refKey(i.entry.what) === successor) : undefined
    return kept ?? this.items[0]
  }

  // ── what was taken in ────────────────────────────────────────────────

  /** What is known of how far `what` was taken in, from Up Next or Recommended. */
  progressOf(what: ContentRef): Consumption | undefined {
    const key = refKey(what)
    const known = this.#known[key]
    if (known) return known
    const fraction = this.#recommendedProgress[key]
    if (!fraction || fraction <= 0) return undefined
    const rec = this.recommended.find((r) => this.#isStory(what, r))
    // All Recommended says is how far, so that is all this has: enough for a
    // bar under a row, not for a place to resume from.
    return {
      id: '',
      what,
      title: rec?.story.title ?? '',
      source: rec?.sourceTitle ?? '',
      url: rec?.story.url ?? '',
      mode: this.#modeOf(what),
      firstAt: '',
      lastAt: '',
      progress: fraction,
      finished: false,
    }
  }

  /** Report progress; answers the stored record, or `null` when refused. */
  async report(progress: ProgressReport): Promise<Consumption | null> {
    // Whether it was finished before this report: what finishing does below
    // happens once, at the moment it becomes so, and not on every report
    // that follows (the service keeps `finished` set from then on).
    const wasFinished = !!this.progressOf(progress.what)?.finished
    let stored: Consumption
    try {
      stored = await api.recordProgress(progress)
    } catch (e) {
      // Not worth a banner: a report is sent every fifteen seconds, and a
      // refusal changes nothing the person is looking at.
      await quietly(e)
      return null
    }
    this.#remember(stored)
    const queued = this.itemFor(progress.what)
    if (queued) queued.consumption = stored
    if (stored.finished && !wasFinished) {
      // Finishing takes a thing out of Up Next, and out of Recommended.
      if (queued) this.#leave(queued)
      this.recommended = this.recommended.filter((r) => !this.#isStory(progress.what, r))
      // Reloaded as well: it may have topped the queue up from Recommended.
      void this.load()
    } else {
      const rec = this.recommended.find((r) => this.#isStory(progress.what, r))
      if (rec) rec.progress = stored.progress
    }
    return stored
  }

  /** Say `what` is finished (or not). */
  async markFinished(what: ContentRef, finished = true): Promise<void> {
    await this.report({ what, mode: this.#modeOf(what), finished })
  }

  // ── downloads ────────────────────────────────────────────────────────

  async download(story: StoryId): Promise<void> {
    try {
      await api.downloadEpisode(story)
    } catch (e) {
      await handle(e)
      return
    }
    // Answered before the file is there. The list is what says where it is.
    await this.refreshDownloads()
  }

  async cancelDownload(story: StoryId): Promise<void> {
    // Off the screen first: a cancelled episode has nothing left to show.
    this.downloads = this.downloads.filter((d) => d.storyId !== story)
    try {
      await api.cancelDownload(story)
    } catch (e) {
      await handle(e)
    }
    await this.refreshDownloads()
  }

  /** Move a downloaded episode's file to the trash. */
  async removeDownload(story: StoryId): Promise<void> {
    try {
      await api.removeEpisodeDownload(story)
    } catch (e) {
      await handle(e)
      return
    }
    // The story no longer links a file, and so no longer plays from one.
    this.#tellEpisodeChanged([story])
    if (this.#itemsLoaded) await this.load()
  }

  downloadOf(story: StoryId): DownloadStatus | undefined {
    return this.downloads.find((d) => d.storyId === story)
  }

  // ── ranking ──────────────────────────────────────────────────────────

  /** Forget every rank and start again. */
  async rerank(): Promise<void> {
    try {
      await api.rerank()
    } catch (e) {
      await handle(e)
      return
    }
    await this.loadRecommended()
    this.#pollRanking(RERANK_POLLS)
  }

  // ── extras, beyond the contract ──────────────────────────────────────

  /**
   * Ask for the downloads and, while any is waiting or under way, again every
   * second. When one has left the list -- it arrived, or was cancelled -- the
   * lists that show episodes are read again, since an episode that has a file
   * now plays from it.
   */
  async refreshDownloads(): Promise<void> {
    if (!app.supportsNews) return
    const token = this.#downloadsGeneration.next()
    try {
      const list = await api.listDownloads()
      if (!this.#downloadsGeneration.isCurrent(token)) return
      const before = this.downloads
      this.downloads = list
      const arrived = before
        .filter((d) => d.state !== 'failed' && !list.some((n) => n.storyId === d.storyId))
        .map((d) => d.storyId)
      if (arrived.length > 0) {
        if (this.#itemsLoaded) void this.load()
        if (this.#recommendedLoaded) void this.loadRecommended()
        this.#tellEpisodeChanged(arrived)
      }
    } catch (e) {
      await quietly(e)
      this.#disarmPoll()
      return
    }
    this.#armPoll()
  }

  /** The file these episodes were downloaded to arrived or left: what the Feed shows of them changed. */
  onEpisodeChange(listener: (stories: StoryId[]) => void): () => void {
    this.#episodeListeners.add(listener)
    return () => this.#episodeListeners.delete(listener)
  }

  /**
   * Where `what` was left, asking the service when Up Next and Recommended do
   * not know. The player's way to resume something started from Files, which
   * is in neither. A thing never taken in answers `undefined`.
   */
  async lookup(what: ContentRef): Promise<Consumption | undefined> {
    const known = this.#known[refKey(what)]
    if (known) return known
    try {
      const stored = await api.getConsumption(what)
      if (!stored) return undefined
      this.#remember(stored)
      return stored
    } catch (e) {
      await quietly(e)
      return undefined
    }
  }

  /** How far through `what` is, 0 to 1; finished is all of it. */
  fractionOf(what: ContentRef): number {
    const c = this.progressOf(what)
    return !c ? 0 : c.finished ? 1 : Math.min(1, Math.max(0, c.progress))
  }

  /**
   * Take the recent record of what was read, heard and watched into the lookup
   * `progressOf` reads, so a row of an episode that is in neither Up Next nor
   * Recommended still shows how far it got. The most recent few hundred, and
   * not more often than every couple of seconds unless `force` says so.
   */
  async loadConsumed(force = false): Promise<void> {
    if (!app.supportsNews) return
    if (!force && Date.now() - this.#consumedAt < CONSUMED_THROTTLE_MS) return
    this.#consumedAt = Date.now()
    const token = this.#consumedGeneration.next()
    try {
      const list = await api.listConsumed({ limit: CONSUMED_LIMIT })
      if (!this.#consumedGeneration.isCurrent(token)) return
      const next = { ...this.#known }
      for (const c of list) {
        const key = refKey(c.what)
        const have = next[key]
        // A report this window sent while the list was on its way is newer.
        if (!have || Date.parse(c.lastAt) >= Date.parse(have.lastAt)) next[key] = c
      }
      this.#known = next
      this.#consumedLoaded = true
    } catch (e) {
      await quietly(e)
    }
  }

  /** A `queue` change arrived from elsewhere: the live router's call. */
  async queueChanged(): Promise<void> {
    await this.load()
  }

  /** A `consumption` change arrived: what is loaded is read again, once the burst is over. */
  consumptionChanged(): Promise<void> {
    const held = this.#itemsLoaded || this.#recommendedLoaded || this.#consumedLoaded
    if (held) this.#settle.call()
    return Promise.resolve()
  }

  /** A `story` change arrived: an episode may have finished downloading. */
  async storyChanged(): Promise<void> {
    if (this.downloads.length > 0) await this.refreshDownloads()
  }

  // ── private ──────────────────────────────────────────────────────────

  async #reloadForConsumption(): Promise<void> {
    await Promise.all([
      this.#itemsLoaded ? this.load() : undefined,
      this.#recommendedLoaded ? this.loadRecommended() : undefined,
      this.#consumedLoaded ? this.loadConsumed(true) : undefined,
    ])
  }

  /** A read that failed: a lock is the lock screen's; anything else is the page's to say. */
  async #failed(e: unknown): Promise<void> {
    if (isLocked(e)) {
      await quietly(e)
      return
    }
    this.error = errorMessage(e)
  }

  /** What `report` and `lookup` learned. */
  #remember(stored: Consumption): void {
    this.#known = { ...this.#known, [refKey(stored.what)]: stored }
  }

  /** Take what Up Next's rows carry into the lookup. Whatever else was learned is kept. */
  #learnItems(): void {
    const next = { ...this.#known }
    for (const item of this.items) {
      if (item.consumption) next[refKey(item.entry.what)] = item.consumption
    }
    this.#known = next
  }

  #learnRecommended(): void {
    const next: Record<string, number> = {}
    for (const r of this.recommended) {
      if (r.progress && r.progress > 0) next[`story:${r.story.id}`] = r.progress
    }
    this.#recommendedProgress = next
  }

  /** Whether `rec` is the story `what` names. */
  #isStory(what: ContentRef, rec: Recommendation): boolean {
    return what.type === 'story' && rec.story.id === what.id
  }

  /** Whether Recommended says `what` is queued. Its flag, not a removal: it may stay on the page. */
  #flagRecommended(what: ContentRef, queued: boolean): void {
    if (what.type !== 'story') return
    const rec = this.recommended.find((r) => r.story.id === what.id)
    if (rec) rec.queued = queued
  }

  /** Take an entry off the screen, writing down who followed it. */
  #leave(item: QueueItem): void {
    const at = this.items.findIndex((i) => i.entry.id === item.entry.id)
    if (at < 0) return
    const after = this.items[at + 1]
    if (after) this.#successors.set(refKey(item.entry.what), refKey(after.entry.what))
    else this.#successors.delete(refKey(item.entry.what))
    this.items = this.items.filter((i) => i.entry.id !== item.entry.id)
    this.#flagRecommended(item.entry.what, false)
  }

  /** What a thing is taken in as, for a report that has nothing but the thing. */
  #modeOf(what: ContentRef): ConsumeMode {
    const key = refKey(what)
    const queued = this.itemFor(what)
    if (queued) return queued.entry.mode
    const known = this.#known[key]
    if (known) return known.mode
    if (what.type === 'story') {
      const media = this.recommended.find((r) => this.#isStory(what, r))?.story.media
      if (media) return media.mime.startsWith('video') ? 'watch' : 'listen'
    }
    return 'read'
  }

  #tellEpisodeChanged(stories: StoryId[]): void {
    for (const listener of [...this.#episodeListeners]) {
      try {
        listener(stories)
      } catch {
        // A listener's trouble is its own.
      }
    }
  }

  /** Ask again in a second if anything is still on its way. */
  #armPoll(): void {
    this.#disarmPoll()
    if (!this.downloads.some((d) => d.state === 'waiting' || d.state === 'downloading')) return
    this.#pollTimer = unheld(
      setTimeout(() => {
        this.#pollTimer = null
        void this.refreshDownloads()
      }, DOWNLOAD_POLL_MS),
    )
  }

  #disarmPoll(): void {
    if (this.#pollTimer) clearTimeout(this.#pollTimer)
    this.#pollTimer = null
  }

  /** Read the status again, up to `left` times, until nothing waits to be ranked. */
  #pollRanking(left: number): void {
    if (this.#rerankTimer) clearTimeout(this.#rerankTimer)
    this.#rerankTimer = null
    if (left <= 0 || !this.ranking || this.ranking.waiting === 0) return
    this.#rerankTimer = unheld(
      setTimeout(() => {
        this.#rerankTimer = null
        void this.loadRecommended().then(() => this.#pollRanking(left - 1))
      }, RERANK_POLL_MS),
    )
  }
}

export const upnext = new UpNext()
