// Up Next, Recommended, downloads, background work, History and Settings →
// Feed: the data behind the player's queue, the Feed's Up Next, Recommended and
// History pages, and the episode buttons everywhere. See `docs/plans/podcasts.md`.
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
//   3. Nothing runs by itself. The timers are the download poll and the work
//      poll (transcripts, articles written from them, audio versions), each
//      armed only while something is waiting or under way and disarmed the
//      moment nothing is; nothing starts at load.
//
// CONTRACT (laid by the supervisor; agents G1 and G2 code against the public
// names): every public name and signature below is fixed; extras are listed
// at the end of the class.

import { api } from './api'
import { findWork, sameWork, workActive } from './podcast'
import { app, errorMessage, handle, isLocked, quietly } from './state.svelte'
import { debounce } from './store/debounce'
import { latest } from './store/latest'
import { guardedRefresh } from './store/refresh'
import {
  VaultError,
  type ConsumeMode,
  type Consumption,
  type ConsumptionId,
  type ContentRef,
  type DownloadStatus,
  type FeedSettings,
  type FileId,
  type ProgressReport,
  type QueueEntryId,
  type QueueItem,
  type QueuePlace,
  type RankingStatus,
  type Recommendation,
  type Story,
  type StoryId,
  type WorkItem,
} from './types'

/** How often the downloads are asked after while one is under way. */
const DOWNLOAD_POLL_MS = 1000

/** The same for background work: a transcript, an article, an audio version. */
const WORK_POLL_MS = 1000

/** How many records a page of History asks for. */
const HISTORY_PAGE = 100

/** The most the service will return at once, which bounds how much a reload covers. */
const HISTORY_MOST = 500

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

/**
 * What changed about an episode or an article under the Feed's list: its files
 * (a download, a transcript, an audio version arrived or left) or, as well, its
 * body (an article was written from a transcript).
 */
export type EpisodeChange = 'files' | 'body'

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
  /** Transcripts, articles and audio versions waiting, under way, or failed. */
  work = $state<WorkItem[]>([])
  /** History: what was read, heard and watched, newest first. */
  history = $state<Consumption[]>([])
  historyLoading = $state(false)
  historyError = $state<string | null>(null)
  /** There may be records older than the ones loaded. */
  historyMore = $state(false)
  /**
   * Whether each file a story or a recording links to is still there, by id, as
   * the vault last said. Replaced whole, so a reader of one id is told when any
   * changes. See `linkedFile`.
   */
  #links = $state<Record<FileId, boolean>>({})

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
  #workGeneration = latest()
  #historyGeneration = latest()
  /** A page of History on its way; a reload retires it. */
  #historyPaging = latest()
  #historyMoreBusy = false
  /** The History page is on screen, so a `consumption` change reads it again. */
  #historyOpen = false
  #workAsked = false
  #workLoaded = false
  /** Links being looked up now, so one is not asked for twice at once. */
  #linksBusy = new Set<FileId>()
  #pollTimer: ReturnType<typeof setTimeout> | null = null
  #workTimer: ReturnType<typeof setTimeout> | null = null
  #rerankTimer: ReturnType<typeof setTimeout> | null = null
  /** Whom to tell that an episode changed under the Feed's own list. */
  #episodeListeners = new Set<(stories: StoryId[], change: EpisodeChange) => void>()
  /** Whom to tell that a piece of work is over. */
  #workListeners = new Set<(item: WorkItem) => void>()
  /**
   * Stories an article was written for from a transcript in this window. The
   * document the pane draws says what its body was written from when the
   * service says so; this is what is known when it does not.
   */
  #written = new Set<StoryId>()
  /** Episodes to write up as an article once their transcript is there. */
  #articleAfter = new Set<StoryId>()
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
    this.#workGeneration.next()
    this.#historyGeneration.next()
    this.#historyPaging.next()
    this.#disarmPoll()
    this.#disarmWorkPoll()
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
    this.work = []
    this.history = []
    this.historyLoading = false
    this.historyError = null
    this.historyMore = false
    this.#historyMoreBusy = false
    this.#historyOpen = false
    this.#workAsked = false
    this.#workLoaded = false
    this.#links = {}
    this.#linksBusy.clear()
    this.#written.clear()
    this.#articleAfter.clear()
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

  // ── transcripts, articles and audio ──────────────────────────────────
  //
  // Three kinds of work the service does in the background, one at a time, for
  // a story or a file: write down what was said in a recording, write an
  // episode up as an article from that, and record an article in a voice. The
  // service answers a request at once and raises a change when the work is
  // done; what a button needs in between is the list of work, which is read
  // again every second while any is waiting or under way.

  /** The work asked for a story or a file, of `kind` when one is named. */
  workFor(
    target: { story?: StoryId | null; file?: FileId | null },
    kind?: WorkItem['kind'],
  ): WorkItem | undefined {
    return findWork(this.work, target, kind)
  }

  /**
   * Read the work once, the first time anything that shows it is on screen.
   * Idempotent: a row asks for it, and a list of rows is a list of asks.
   */
  async ensureWork(): Promise<void> {
    if (this.#workAsked) return
    this.#workAsked = true
    await this.refreshWork()
  }

  /**
   * Ask for the work and, while any is waiting or under way, again every
   * second. When a piece has left the list -- it was done -- what it made is
   * news to whoever shows it: the Feed patches the story, Files its list.
   */
  async refreshWork(): Promise<void> {
    if (!app.supportsNews) return
    const token = this.#workGeneration.next()
    try {
      const list = await api.listeningWork()
      if (!this.#workGeneration.isCurrent(token)) return
      const before = this.work
      this.work = list
      this.#workLoaded = true
      this.#workMoved(before, list)
    } catch (e) {
      await quietly(e)
      this.#disarmWorkPoll()
      return
    }
    this.#armWorkPoll()
  }

  /** Write down what was said in an episode. */
  async transcribe(story: StoryId): Promise<void> {
    await this.#ask(() => api.transcribeEpisode(story), { kind: 'transcript', storyId: story })
  }

  /** Write down what was said in a recording or a film in Files. */
  async transcribeFile(file: FileId): Promise<void> {
    await this.#ask(() => api.transcribeFile(file), { kind: 'transcript', fileId: file })
  }

  /** Write an episode up as an article from its transcript. */
  async writeArticle(story: StoryId): Promise<void> {
    await this.#ask(() => api.writeArticle(story), { kind: 'article', storyId: story })
  }

  /**
   * Read an episode as an article: written from its transcript, which is made
   * first when there is none. The two are one wish -- nobody wants a
   * transcript to look at -- so the second follows the first by itself.
   */
  async readAsArticle(story: Pick<Story, 'id' | 'files'>): Promise<void> {
    if (story.files?.transcript) {
      await this.writeArticle(story.id)
      return
    }
    this.#articleAfter.add(story.id)
    const asked = await this.#ask(() => api.transcribeEpisode(story.id), {
      kind: 'transcript',
      storyId: story.id,
    })
    if (!asked) this.#articleAfter.delete(story.id)
  }

  /** Record an article in a voice, and keep it in Files. */
  async renderAudio(story: StoryId): Promise<void> {
    await this.#ask(() => api.renderStoryAudio(story), { kind: 'audio', storyId: story })
  }

  /** Whether an article was written from a transcript by this window: see `#written`. */
  wroteFromTranscript(story: StoryId): boolean {
    return this.#written.has(story)
  }

  /** Be told when a piece of work is over. Answers the way to stop being told. */
  onWorkDone(listener: (item: WorkItem) => void): () => void {
    this.#workListeners.add(listener)
    return () => this.#workListeners.delete(listener)
  }

  /** A `story` or `file` change arrived: work may have finished, or been started elsewhere. */
  async workChanged(): Promise<void> {
    if (this.#workLoaded) await this.refreshWork()
  }

  /**
   * Ask for `job` and then for the work, so the button that asked shows it at
   * once. Answers whether it was asked for: a refusal is said the usual way
   * and is not work.
   */
  async #ask(
    call: () => Promise<void>,
    job: Pick<WorkItem, 'kind' | 'storyId' | 'fileId'>,
  ): Promise<boolean> {
    const have = this.work.find((w) => sameWork(w, job))
    if (have && workActive(have)) return true
    try {
      await call()
    } catch (e) {
      await handle(e)
      return false
    }
    await this.refreshWork()
    return true
  }

  /** What the list of work gaining or losing a piece means to the rest. */
  #workMoved(before: WorkItem[], after: WorkItem[]): void {
    // A transcript that could not be made leaves nothing to write up from.
    for (const failed of after) {
      if (failed.state === 'failed' && failed.kind === 'transcript' && failed.storyId) {
        this.#articleAfter.delete(failed.storyId)
      }
    }
    for (const gone of before) {
      if (!workActive(gone) || after.some((w) => sameWork(w, gone))) continue
      for (const listener of [...this.#workListeners]) {
        try {
          listener(gone)
        } catch {
          // A listener's trouble is its own.
        }
      }
      const story = gone.storyId
      if (!story) continue
      if (gone.kind === 'article') this.#written.add(story)
      // The story's files changed -- and, for an article, its body.
      this.#tellEpisodeChanged([story], gone.kind === 'article' ? 'body' : 'files')
      // An episode that has an audio version now plays from Up Next.
      if (gone.kind === 'audio' && this.#itemsLoaded) void this.load()
      // The transcript is there: write it up, unless the service already is.
      if (
        gone.kind === 'transcript' &&
        this.#articleAfter.delete(story) &&
        !findWork(after, { story }, 'article')
      ) {
        void this.writeArticle(story)
      }
    }
  }

  /** Ask again in a second if anything is still waiting or under way. */
  #armWorkPoll(): void {
    this.#disarmWorkPoll()
    if (!this.work.some(workActive)) return
    this.#workTimer = unheld(
      setTimeout(() => {
        this.#workTimer = null
        void this.refreshWork()
      }, WORK_POLL_MS),
    )
  }

  #disarmWorkPoll(): void {
    if (this.#workTimer) clearTimeout(this.#workTimer)
    this.#workTimer = null
  }

  // ── linked files ─────────────────────────────────────────────────────
  //
  // A story links the transcript and the audio made for it, and a recording
  // links its transcript, by a file's id; the link is not the file. Whoever
  // trashes the file in Files leaves the link where it was until the service
  // sweeps it, and a button that goes by the link alone offers to open what is
  // not there and never offers to make it again. So where an action is drawn
  // for the story that is open or the file that is inspected, the file is
  // looked up -- and a file that is trashed, missing or unknown to the vault
  // is treated as if there were no link.

  /**
   * `id` when the file it names is there to be opened, `null` when there is no
   * link or the vault says the file is gone. A file not looked up yet is taken
   * as there: showing "Open transcript" for the moment it takes to ask is
   * better than a button that changes its mind for every story that is opened.
   */
  linkedFile(id: FileId | null | undefined): FileId | null {
    if (!id) return null
    return this.#links[id] === false ? null : id
  }

  /**
   * Ask the vault whether each of these files is there, and remember the answer.
   * Looked up every time and not once: this is called as a story is opened, and
   * a file trashed since the last look is the case this exists for. A refusal
   * other than "no such file" -- a network that is down -- leaves what was known.
   */
  async checkLinks(ids: readonly (FileId | null | undefined)[]): Promise<void> {
    if (!app.supportsNews) return
    const wanted = [...new Set(ids.filter((id): id is FileId => !!id))]
    await Promise.all(wanted.filter((id) => !this.#linksBusy.has(id)).map((id) => this.#lookUp(id)))
  }

  /** A `file` change arrived: every link that has been looked up is looked up again. */
  async recheckLinks(): Promise<void> {
    await this.checkLinks(Object.keys(this.#links))
  }

  /** A `file` change arrived: what it may have finished or taken away is looked at again. */
  async filesChanged(): Promise<void> {
    await Promise.all([this.workChanged(), this.recheckLinks()])
  }

  async #lookUp(id: FileId): Promise<void> {
    this.#linksBusy.add(id)
    try {
      const detail = await api.getFile(id)
      this.#links = { ...this.#links, [id]: detail.file.status.state === 'present' }
    } catch (e) {
      if (e instanceof VaultError && e.code === 'not_found') {
        this.#links = { ...this.#links, [id]: false }
      } else {
        await quietly(e)
      }
    } finally {
      this.#linksBusy.delete(id)
    }
  }

  // ── history ──────────────────────────────────────────────────────────

  /**
   * Say whether the History page is on screen. While it is, a `consumption`
   * change reads the record again; while it is not, nothing does.
   */
  watchHistory(open: boolean): void {
    this.#historyOpen = open
  }

  /**
   * Read History from the newest record. `keep` covers as many records as are
   * loaded -- a background change must not shrink a list somebody has scrolled
   * down -- and a fresh read is one page.
   */
  async loadHistory(keep = false): Promise<void> {
    if (!app.supportsNews) return
    // A page on its way belongs to the list being replaced.
    this.#historyPaging.next()
    await guardedRefresh(
      this.#historyGeneration,
      async (isCurrent) => {
        const limit = keep
          ? Math.min(HISTORY_MOST, Math.max(HISTORY_PAGE, this.history.length))
          : HISTORY_PAGE
        const list = await api.listConsumed({ limit })
        if (!isCurrent()) return
        this.history = list
        this.historyMore = list.length >= limit
        this.historyError = null
      },
      {
        setLoading: (v) => (this.historyLoading = v),
        onError: (e) => this.#historyFailed(e),
      },
    )
  }

  /** The page of records older than the last one loaded, appended. */
  async loadMoreHistory(): Promise<void> {
    const last = this.history.at(-1)
    if (!last || !this.historyMore || this.#historyMoreBusy || !app.supportsNews) return
    this.#historyMoreBusy = true
    const token = this.#historyPaging.next()
    try {
      const page = await api.listConsumed({ until: last.lastAt, limit: HISTORY_PAGE })
      // A reload while this was on its way retired the token.
      if (!this.#historyPaging.isCurrent(token) || this.history.at(-1)?.id !== last.id) return
      const have = new Set(this.history.map((c) => c.id))
      this.history = [...this.history, ...page.filter((c) => !have.has(c.id))]
      this.historyMore = page.length >= HISTORY_PAGE
    } catch (e) {
      await this.#historyFailed(e)
    } finally {
      this.#historyMoreBusy = false
    }
  }

  /**
   * Forget that something was read, heard or watched: its record goes, and
   * with it how far it got. The thing itself, and any file made from it, stay.
   * Taken off the screen first and put right by the service afterwards.
   */
  async forget(id: ConsumptionId): Promise<void> {
    const gone = this.history.find((c) => c.id === id)
    if (gone) this.history = this.history.filter((c) => c.id !== id)
    try {
      await api.forgetConsumption(id)
    } catch (e) {
      await handle(e, () => this.loadHistory(true))
      return
    }
    // What was known of how far it got, here and in Up Next, is no longer so.
    if (gone) {
      const key = refKey(gone.what)
      if (this.#known[key]?.id === id) {
        const next = { ...this.#known }
        delete next[key]
        this.#known = next
      }
    }
    void this.#reloadForConsumption()
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
  onEpisodeChange(listener: (stories: StoryId[], change: EpisodeChange) => void): () => void {
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
    const held =
      this.#itemsLoaded || this.#recommendedLoaded || this.#consumedLoaded || this.#historyOpen
    if (held) this.#settle.call()
    return Promise.resolve()
  }

  /** A `story` change arrived: an episode may have finished downloading, or been transcribed. */
  async storyChanged(): Promise<void> {
    await Promise.all([
      this.downloads.length > 0 ? this.refreshDownloads() : undefined,
      this.workChanged(),
    ])
  }

  // ── private ──────────────────────────────────────────────────────────

  async #reloadForConsumption(): Promise<void> {
    await Promise.all([
      this.#itemsLoaded ? this.load() : undefined,
      this.#recommendedLoaded ? this.loadRecommended() : undefined,
      this.#consumedLoaded ? this.loadConsumed(true) : undefined,
      this.#historyOpen ? this.loadHistory(true) : undefined,
    ])
  }

  /** A read of History that failed: a lock is the lock screen's; anything else, the page's to say. */
  async #historyFailed(e: unknown): Promise<void> {
    if (isLocked(e)) {
      await quietly(e)
      return
    }
    this.historyError = errorMessage(e)
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

  #tellEpisodeChanged(stories: StoryId[], change: EpisodeChange = 'files'): void {
    for (const listener of [...this.#episodeListeners]) {
      try {
        listener(stories, change)
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
