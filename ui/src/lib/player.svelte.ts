// The one player: every podcast episode, every recording and film in Files,
// the daily podcast. One media element for the whole window, mounted once by
// `PlayerHost.svelte` in `App.svelte` and handed here with `attach`, so
// switching apps never stops what is playing.
//
// It plays files only -- `fileUrl(id)`, which the files protocol serves with
// `Range` -- never a remote address: an episode is downloaded first. An
// article in Up Next is heard through Listen (`listen.svelte.ts`) instead,
// and the queue moves on when either ends.
//
// Progress goes to the service with `record_progress` every 15 s while
// playing, on pause, on seek and at the end, so where something was left and
// what of it was heard are remembered -- see `docs/plans/podcasts.md`.
//
// Three rules shape most of what is below:
//
//   1. What was *heard* is the contiguous stretch played since the last
//      report. A seek closes it, so a jump over twenty minutes is never
//      counted as twenty minutes listened to.
//   2. Reports are sent in order, one after another. A pause and a periodic
//      tick can fall within a moment of each other, and the service keeps the
//      position of whichever arrives last.
//   3. The queue moves on from what was heard to the end and from nothing
//      else. Stop, an error, a lock and a replaced session do not advance it.
//
// A conversation with the assistant out loud shares the speakers, so a voice
// session pauses whatever is playing -- this and Listen -- and, when it ends,
// picks it up where it was (`pauseForVoice`, `resumeAfterVoice`; Settings →
// Feed says whether to). `voice.svelte.ts` calls them: this file cannot
// import it, since the voice store reads the screen and the screen reads
// this. And `playingAt` is what is playing, sent with every message so
// "what did he just say?" has a place in a recording to be answered from.
//
// CONTRACT (laid by the supervisor; agent F implements the bodies, agents G1
// and G2 code against the public names): keep every public name and
// signature below.

import { mediaUrl, playUrl } from './api'
import { isReadable } from './documents'
import { feed } from './feed.svelte'
import { library } from './library.svelte'
import { listening } from './listen.svelte'
import { notify } from './notify.svelte'
import { reader } from './reader.svelte'
import { app, errorMessage } from './state.svelte'
import type {
  ContentRef,
  EnclosureKind,
  FileId,
  ListenSource,
  PlayingAt,
  ProgressReport,
  QueueItem,
  Span,
} from './types'
import { upnext } from './upnext.svelte'

/** The speeds the player offers. */
export const PLAYER_SPEEDS = [0.8, 1, 1.2, 1.5, 1.75, 2, 2.5] as const

/** Where the chosen speed is kept, on this device. See `listen.svelte.ts`. */
const SPEED_KEY = 'karmayoga.player.speed'

/** How much playing passes between two reports, in real time. */
const REPORT_EVERY_MS = 15_000

/**
 * How far playback may move between two clock ticks and still be one
 * stretch. A tick is a quarter of a second of real time -- up to a second of
 * the recording at the fastest speed -- and a coarser clock still is well
 * inside this; a jump the element did not announce is not.
 */
const GAP_MS = 5_000

/** The shortest stretch worth writing down. */
const MIN_HEARD_MS = 1_000

/** A start this close to the end is the end: start from the top. */
const NEAR_END_MS = 3_000

/** Fallback skips, until Settings → Feed is read. */
const SKIP_BACK_S = 15
const SKIP_FORWARD_S = 30

/** The speed kept from last time, or normal speed. */
function savedSpeed(): number {
  try {
    const kept = Number(localStorage.getItem(SPEED_KEY))
    return Number.isFinite(kept) && kept >= 0.5 && kept <= 3 ? kept : 1
  } catch {
    return 1
  }
}

/** What to play. */
export interface PlayRequest {
  /** What it is: for progress, Up Next and "is this playing". */
  what: ContentRef
  fileId: FileId
  kind: EnclosureKind
  title: string
  /** The show, the folder. */
  source: string
  /** A picture to show beside it: an address the webview may load. */
  artwork?: string | null
  /** Where to start, in ms. Absent: where it was left, else the start. */
  startMs?: number | null
  durationMs?: number | null
  /** The show's own speed, over the player's. */
  speed?: number | null
  /** Played from Up Next: when it ends, the next entry plays (if autoplay). */
  fromQueue?: boolean
}

/** What the element says went wrong, in words. */
function mediaErrorText(error: MediaError | null): string {
  switch (error?.code) {
    case 1:
      return 'Playback was stopped.'
    case 2:
      return 'The file could not be read.'
    case 3:
      return 'The file is damaged, or is not something this window can decode.'
    case 4:
      return 'This is not a kind of file this window can play.'
    default:
      return 'This could not be played.'
  }
}

class Player {
  /** What is loaded; `null` when nothing is. */
  current = $state<PlayRequest | null>(null)
  playing = $state(false)
  /** Waiting for data. */
  buffering = $state(false)
  positionMs = $state(0)
  durationMs = $state(0)
  /** The chosen speed, kept on this device. */
  speed = $state(savedSpeed())
  volume = $state(1)
  muted = $state(false)
  /** The panel is open: Up Next, the controls, and a video's picture. */
  expanded = $state(false)
  /** Why it could not play. */
  error = $state<string | null>(null)

  #el: HTMLMediaElement | null = null
  #detach: (() => void) | null = null
  /** The speed this device chose, apart from a show's own for what is loaded. */
  #deviceSpeed = savedSpeed()
  /** Where to jump once the file's length is known: set before it is. */
  #pendingStart: number | null = null
  /** Which `play` is the newest. A slow lookup for an older one must not start it. */
  #token = 0

  /** The stretch being played, in ms of the recording. `null` while none is. */
  #heardFrom: number | null = null
  #heardTo = 0
  /** Something has played, or moved, since the last report. */
  #unsent = false
  #lastSentAt = 0
  /** Reports sent and not yet answered, so a flush can wait for them. */
  #sending: Promise<unknown> = Promise.resolve()

  /** What the queue is playing aloud through Listen, so its end is the queue's. */
  #listened: ContentRef | null = null

  /**
   * What a voice session paused, to be picked up when it ends: the media
   * that was playing and the story Listen was reading, either or neither.
   */
  #held: { media: ContentRef | null; listen: ListenSource | null } | null = null
  /**
   * The second the media is at, whole. Read by `playingAt`, so what is
   * on screen is told again when the position has moved on -- once a second,
   * where `positionMs` changes four times a second -- and the position sent
   * with a typed message is never more than a second old.
   */
  #second = $state(0)

  constructor() {
    app.onLock(() => this.#lock())
    // Before a lock or the window closing: where it is is written first.
    app.onFlush(() => this.#flush())
    // Reading aloud and playing cannot share the speakers.
    listening.onStart((source) => this.#listenStarting(source))
    listening.onEnded((source) => void this.#listenEnded(source))
  }

  /** This window can play: anywhere the files protocol serves files. */
  get available(): boolean {
    return true
  }

  /** What is loaded is a video. */
  get video(): boolean {
    return this.current?.kind === 'video'
  }

  /** Whether `what` is what is loaded (playing or paused). */
  isCurrent(what: ContentRef): boolean {
    const c = this.current?.what
    return !!c && c.type === what.type && c.id === what.id
  }

  /** Whether `what` is playing now. */
  isPlaying(what: ContentRef): boolean {
    return this.playing && this.isCurrent(what)
  }

  // ── the element ──────────────────────────────────────────────────────

  /** Hand over the one media element (`PlayerHost.svelte`). */
  attach(element: HTMLMediaElement): void {
    this.detach()
    this.#el = element
    element.volume = this.volume
    element.muted = this.muted

    const on: [string, () => void][] = [
      ['timeupdate', () => this.#tick()],
      ['durationchange', () => this.#measured()],
      ['loadedmetadata', () => this.#loaded()],
      ['play', () => (this.playing = true)],
      ['playing', () => this.#started()],
      ['pause', () => this.#paused()],
      ['waiting', () => (this.buffering = true)],
      ['canplay', () => (this.buffering = false)],
      ['seeking', () => this.#seeking()],
      ['seeked', () => this.#seeked()],
      ['ended', () => void this.#ended()],
      ['error', () => this.#failed()],
      ['ratechange', () => this.#rated()],
    ]
    for (const [name, handler] of on) element.addEventListener(name, handler)
    this.#detach = () => {
      for (const [name, handler] of on) element.removeEventListener(name, handler)
    }
    this.#setSessionActions()
  }

  detach(): void {
    this.#detach?.()
    this.#detach = null
    this.#clearSessionActions()
    this.#el = null
  }

  // ── playing ──────────────────────────────────────────────────────────

  /** Load `request` and play it, ending whatever was loaded (its progress reported first). */
  async play(request: PlayRequest): Promise<void> {
    const el = this.#el
    const token = ++this.#token
    this.error = null

    // What is already loaded, and is the same: carry on rather than start over.
    const loaded = this.current
    if (el && loaded && this.isCurrent(request.what) && loaded.fileId === request.fileId) {
      this.#listened = null
      // Asked for again from somewhere else -- Up Next, after the Feed -- it
      // takes on the new request's say over what follows it and how fast.
      this.current = { ...loaded, ...request, startMs: loaded.startMs }
      if (request.speed != null) el.playbackRate = request.speed
      if (request.startMs != null) this.seek(request.startMs)
      this.resume()
      return
    }

    // The thing before it: where it got to is written down, and it stops.
    this.#sync()
    this.#report()
    this.#listened = null
    if (listening.active) listening.stop()
    el?.pause()

    if (!el) {
      this.error = 'The player is not ready.'
      return
    }
    // The server's word on where it was left, unless a place was asked for.
    // Looked up before anything is changed, so a slow answer leaves the last
    // thing playing a moment longer rather than a silent player.
    const start = request.startMs ?? (await this.#resumeAt(request))
    if (token !== this.#token) return

    this.#resetStretch()
    this.current = request
    this.positionMs = start
    this.durationMs = request.durationMs ?? 0
    this.playing = false
    this.buffering = true
    this.#unsent = false
    const speed = request.speed ?? this.#deviceSpeed
    this.speed = speed
    this.#pendingStart = start > 0 ? start : null
    el.defaultPlaybackRate = speed
    el.src = playUrl(request.fileId)
    el.playbackRate = speed
    // Not every webview takes a start before the length is known; the
    // `loadedmetadata` handler makes sure of it.
    this.#describe(request)
    if (!upnext.settings) void upnext.loadSettings()
    try {
      await el.play()
    } catch (e) {
      // A newer load interrupting this one is not a failure.
      if (token !== this.#token || (e instanceof DOMException && e.name === 'AbortError')) return
      this.playing = false
      this.buffering = false
      this.error =
        e instanceof DOMException && e.name === 'NotAllowedError'
          ? 'Press play to start.'
          : errorMessage(e)
    }
  }

  /** Play an Up Next entry: its file in the player, or an article through Listen. */
  async playItem(item: QueueItem): Promise<void> {
    const entry = item.entry
    const what = entry.what

    if (item.play) {
      const { speed, artwork } = await this.#showOf(item)
      await this.play({
        what,
        fileId: item.play.fileId,
        kind: item.play.kind,
        title: entry.title,
        source: entry.source || item.sourceTitle,
        artwork,
        startMs: item.play.startMs ?? null,
        durationMs: item.play.durationMs ?? entry.durationMs ?? null,
        speed,
        fromQueue: true,
      })
      return
    }

    // An episode with no file yet cannot be played; it is fetched, not streamed.
    if (item.story?.media) {
      if (!item.downloading) void upnext.download(item.story.id)
      notify.info('That episode is being downloaded. It plays from your files when it is there.')
      return
    }

    // An article is heard through Listen, or read where Listen cannot.
    if (what.type === 'story') {
      await this.#listen(item)
      return
    }

    // What is to be read, not played: opened, and the queue stops there.
    if (what.type === 'file' && item.file) {
      if (isReadable(item.file)) await reader.openFile(item.file.id)
      else if (await app.goTo('files')) notify.info('Find that file in Files.')
      return
    }
    if (what.type === 'item') {
      if (!(await app.goTo('library'))) return
      await library.open(what.id)
    }
  }

  /** Play what comes after the current thing in Up Next. */
  async playNext(): Promise<void> {
    const what = this.current?.what ?? this.#listened
    const next = this.#nextOf(what)
    if (next) await this.playItem(next)
    else this.stop()
  }

  pause(): void {
    this.#el?.pause()
  }

  resume(): void {
    const el = this.#el
    if (!el || !this.current) return
    this.error = null
    // Played to the end and pressed again: from the top.
    if (el.ended) el.currentTime = 0
    void el.play().catch((e: unknown) => {
      if (e instanceof DOMException && e.name === 'AbortError') return
      this.playing = false
      this.error = errorMessage(e)
    })
  }

  toggle(): void {
    if (this.playing) this.pause()
    else this.resume()
  }

  /** Jump to `ms`. */
  seek(ms: number): void {
    const el = this.#el
    if (!el || !this.current) return
    const longest = this.durationMs > 0 ? this.durationMs : Number.POSITIVE_INFINITY
    const to = Math.round(Math.min(Math.max(0, ms), longest))
    // What was heard up to here is written down before the jump, and the
    // stretch ends: where it lands is a new one.
    this.#sync()
    this.#report()
    el.currentTime = to / 1000
    this.positionMs = to
    this.#unsent = true
  }

  /** Jump by `seconds`, back when negative. */
  skip(seconds: number): void {
    this.seek(this.positionMs + seconds * 1000)
  }

  setSpeed(speed: number): void {
    this.#deviceSpeed = speed
    this.speed = speed
    try {
      localStorage.setItem(SPEED_KEY, String(speed))
    } catch {
      // Not kept; this session still has it.
    }
    if (this.#el) {
      this.#el.defaultPlaybackRate = speed
      this.#el.playbackRate = speed
    }
  }

  setVolume(volume: number): void {
    this.volume = Math.min(1, Math.max(0, volume))
    if (this.#el) this.#el.volume = this.volume
    // Raising it again is what somebody pressing "louder" on a muted player means.
    if (this.volume > 0 && this.muted) this.toggleMute()
  }

  toggleMute(): void {
    this.muted = !this.muted
    if (this.#el) this.#el.muted = this.muted
  }

  /** Mark what is loaded finished and move on. */
  async finish(): Promise<void> {
    const current = this.current
    if (!current) return
    const next = this.#nextOf(current.what)
    await this.#reportFinished(current, this.positionMs)
    // Asked for something else while the report was on its way: leave it be.
    if (this.current !== current) return
    if (next) await this.playItem(next)
    else this.stop()
  }

  /** Stop and unload, reporting progress first. */
  stop(): void {
    this.#token += 1
    this.#sync()
    this.#report()
    this.#unload()
  }

  // ── what the element reports ─────────────────────────────────────────

  /** The clock moved. */
  #tick(): void {
    const el = this.#el
    if (!el || !this.current) return
    const ms = Math.round(el.currentTime * 1000)
    this.positionMs = ms
    this.#second = Math.floor(ms / 1000)
    if (this.playing && !el.seeking) {
      if (this.#heardFrom === null) {
        this.#heardFrom = ms
        this.#heardTo = ms
      } else if (ms >= this.#heardTo && ms - this.#heardTo <= GAP_MS) {
        this.#heardTo = ms
      } else {
        // A jump that was not announced: what came before it ends where it was.
        this.#unsent = true
        this.#report({ at: this.#heardTo })
        this.#heardFrom = ms
        this.#heardTo = ms
      }
      this.#unsent = true
      if (Date.now() - this.#lastSentAt >= REPORT_EVERY_MS) this.#report({ carryOn: true })
    }
    this.#updatePosition()
  }

  /** Read where the element is, for a report that cannot wait for the next tick. */
  #sync(): void {
    const el = this.#el
    if (!el || !this.current) return
    this.positionMs = Math.round(el.currentTime * 1000)
    this.#extend(this.positionMs)
  }

  /** The recording's length became known or changed. */
  #measured(): void {
    const seconds = this.#el?.duration
    if (seconds !== undefined && Number.isFinite(seconds) && seconds > 0) {
      this.durationMs = Math.round(seconds * 1000)
    }
  }

  /** The file's header is read: now a start can be taken. */
  #loaded(): void {
    this.#measured()
    const el = this.#el
    if (el && this.#pendingStart !== null) {
      el.currentTime = this.#pendingStart / 1000
      this.#pendingStart = null
    }
  }

  /** Sound is coming out. */
  #started(): void {
    this.playing = true
    this.buffering = false
    this.#lastSentAt = Date.now()
    this.#setSessionState('playing')
  }

  #paused(): void {
    const el = this.#el
    this.playing = false
    this.#setSessionState('paused')
    // At the end `pause` comes first, and `ended` reports the whole of it.
    if (!el || el.ended || !this.current) return
    this.positionMs = Math.round(el.currentTime * 1000)
    this.#extend(this.positionMs)
    this.#report()
  }

  /** A jump the element made without being asked: the stretch ends where it was. */
  #seeking(): void {
    if (this.#heardFrom !== null) this.#report({ at: this.positionMs })
  }

  #seeked(): void {
    const el = this.#el
    if (el) {
      this.positionMs = Math.round(el.currentTime * 1000)
      this.#second = Math.floor(this.positionMs / 1000)
    }
    this.#updatePosition()
  }

  async #ended(): Promise<void> {
    const current = this.current
    if (!current) return
    this.playing = false
    this.#setSessionState('paused')
    const end = this.durationMs > 0 ? this.durationMs : this.positionMs
    this.positionMs = end
    const next =
      current.fromQueue && (upnext.settings?.autoplay ?? true) ? this.#nextOf(current.what) : null
    await this.#reportFinished(current, end)
    // Something else was started while the report was on its way.
    if (this.current !== current) return
    if (next) await this.playItem(next)
    else if (current.fromQueue) this.stop()
  }

  #failed(): void {
    const el = this.#el
    // Unloading a file is itself reported by some webviews, and is not a fault.
    if (!el || !this.current || !el.getAttribute('src')) return
    this.playing = false
    this.buffering = false
    this.error = mediaErrorText(el.error)
    // An episode whose file cannot be found -- trashed or deleted in Files --
    // is fetched again rather than left saying it cannot be read.
    const what = this.current.what
    if (what.type === 'story' && el.error?.code === 2) {
      this.error = 'This episode’s file is gone; it is being downloaded again.'
      void upnext.download(what.id)
    }
  }

  #rated(): void {
    const el = this.#el
    if (el && this.current && Math.abs(el.playbackRate - this.speed) > 0.001) {
      this.speed = el.playbackRate
    }
  }

  // ── progress ─────────────────────────────────────────────────────────

  #resetStretch(): void {
    this.#heardFrom = null
    this.#heardTo = 0
  }

  /** Carry the open stretch forward to `ms`, when it is where playing has got to. */
  #extend(ms: number): void {
    if (this.#heardFrom !== null && ms >= this.#heardTo && ms - this.#heardTo <= GAP_MS) {
      this.#heardTo = ms
    }
  }

  /** End the stretch at `ms` and answer it, when it was long enough to count. */
  #takeStretch(ms: number): Span | null {
    this.#extend(ms)
    const from = this.#heardFrom
    this.#heardFrom = null
    return from !== null && this.#heardTo - from >= MIN_HEARD_MS
      ? { startMs: from, endMs: this.#heardTo }
      : null
  }

  /**
   * Report where it is and what was played since the last report, when
   * anything has been played or moved. `at` is where the stretch ends when
   * that is not where the position says -- before an announced jump the
   * element has already moved on. `carryOn` starts the next stretch here, for
   * a report in the middle of playing; every other report ends it, and the
   * next tick begins one.
   */
  #report(options: { at?: number; carryOn?: boolean } = {}): void {
    const current = this.current
    if (!current || !this.#unsent) return
    const position = options.at ?? this.positionMs
    const heard = this.#takeStretch(position)
    if (options.carryOn && this.playing) {
      this.#heardFrom = this.positionMs
      this.#heardTo = this.positionMs
    }
    this.#unsent = false
    void this.#queue(current, position, heard, false)
  }

  /** The end, said so: from the end of the recording, or a button. */
  #reportFinished(current: PlayRequest, position: number): Promise<unknown> {
    const heard = this.#takeStretch(position)
    this.#unsent = false
    return this.#queue(current, position, heard, true)
  }

  /** Send a report after the one before it, so the service sees them in order. */
  #queue(
    current: PlayRequest,
    position: number,
    heard: Span | null,
    finished: boolean,
  ): Promise<unknown> {
    const duration = this.durationMs > 0 ? this.durationMs : (current.durationMs ?? null)
    const progress: ProgressReport = {
      what: current.what,
      mode: current.kind === 'video' ? 'watch' : 'listen',
      positionMs: position,
      durationMs: duration,
      heard,
      ...(finished ? { finished: true } : {}),
    }
    this.#lastSentAt = Date.now()
    this.#sending = this.#sending.then(() => upnext.report(progress)).catch(() => {})
    return this.#sending
  }

  /** Where to start `request`: where it was left, unless it was finished or nearly. */
  async #resumeAt(request: PlayRequest): Promise<number> {
    const stored = await upnext.lookup(request.what)
    if (!stored || stored.finished) return 0
    const at = stored.positionMs ?? 0
    const length = request.durationMs ?? stored.durationMs ?? 0
    return length > 0 && at >= length - NEAR_END_MS ? 0 : at
  }

  // ── what comes next ──────────────────────────────────────────────────

  /**
   * The next thing the queue can play after `what`: past an episode still
   * being downloaded, which cannot be played yet and is not worth stopping
   * for.
   */
  #nextOf(what: ContentRef | null): QueueItem | null {
    let next = upnext.after(what)
    // Bounded by the queue's length: a ring cannot be walked forever.
    for (let i = 0; next && i < upnext.items.length; i++) {
      if (next.play || !next.story?.media) return next
      next = upnext.after(next.entry.what)
    }
    return null
  }

  /** The show an episode belongs to: its own speed, and its picture. */
  async #showOf(item: QueueItem): Promise<{ speed: number | null; artwork: string | null }> {
    const story = item.story
    if (!story) return { speed: null, artwork: null }
    // The Feed may not have been opened yet, and a show's speed is in its sources.
    if (feed.sources.length === 0) await feed.refreshSources()
    const show = feed.sourceOf(story)?.source
    return {
      speed: show?.podcast?.speed ?? null,
      artwork: show?.icon ? mediaUrl(show.icon) : null,
    }
  }

  // ── an article, heard ────────────────────────────────────────────────

  async #listen(item: QueueItem): Promise<void> {
    const what = item.entry.what
    if (what.type !== 'story') return
    if (!listening.available) {
      // Nowhere to say it aloud: it can still be read.
      if (await app.goTo('feed')) await feed.open(what.id)
      return
    }
    // What was playing is put down first; Listen's own start would pause it,
    // but the report should be sent and the file let go of.
    this.stop()
    this.#listened = what
    await listening.start({ type: 'story', storyId: what.id, asWritten: false }, item.entry.title)
  }

  /** Listen is starting: the media pauses, and a session not the queue's ends the queue's claim. */
  #listenStarting(source: ListenSource): void {
    if (this.playing) this.pause()
    const mine = this.#listened
    if (mine && !(source.type === 'story' && mine.type === 'story' && source.storyId === mine.id)) {
      this.#listened = null
    }
  }

  /** Listen read to the end: the article is heard, and the queue goes on. */
  async #listenEnded(source: ListenSource): Promise<void> {
    const what = this.#listened
    this.#listened = null
    if (!what || what.type !== 'story' || source.type !== 'story' || source.storyId !== what.id) {
      return
    }
    const next = (upnext.settings?.autoplay ?? true) ? this.#nextOf(what) : null
    await upnext.report({ what, mode: 'listen', finished: true })
    // Something was started while the report was on its way.
    if (this.current || listening.active) return
    if (next) await this.playItem(next)
  }

  // ── a conversation ───────────────────────────────────────────────────

  /**
   * What is being heard or watched, for the screen the assistant is told
   * about: the media's thing with where it has got to and whether it is
   * paused, or -- when Listen is reading a story aloud and nothing is
   * playing -- the story and whether it is paused, with no position. `null`
   * when there is nothing, and for what Listen reads that is not content: a
   * mail thread, a brief.
   *
   * Reads the media element for the position rather than `positionMs`, which
   * moves four times a second and would have every screen redrawn with it;
   * `#second` is what asks to be asked again.
   */
  playingAt(): PlayingAt | null {
    void this.#second
    const current = this.current
    const source = listening.active ? listening.source : null
    const story = source?.type === 'story' ? source : null
    // Whatever is making a sound is what is playing: each starting pauses the
    // other, so only a story Listen has paused or been paused can be beside
    // media that is loaded.
    if (current && (this.playing || !story)) {
      const el = this.#el
      const duration = this.durationMs > 0 ? this.durationMs : (current.durationMs ?? null)
      return {
        what: current.what,
        positionMs: el ? Math.round(el.currentTime * 1000) : this.positionMs,
        durationMs: duration,
        // `pause()` is answered at once by the element and a moment later by
        // the `pause` event that clears `playing`.
        paused: !this.playing || !!el?.paused,
      }
    }
    if (story) {
      // Paused for a conversation: the shell is told, and says so a moment later.
      const held = this.#held?.listen
      const paused = listening.state === 'paused' || (!!held && listening.isReading(held))
      return { what: { type: 'story', id: story.storyId }, paused }
    }
    return null
  }

  /**
   * A voice session is starting: whatever is playing -- the media, a story
   * Listen reads aloud -- is paused, and remembered so that it can be picked
   * up. Asked before the screen is described to the session, so it says
   * paused.
   */
  pauseForVoice(): void {
    const media = this.playing && this.current ? this.current.what : null
    const reading = listening.active && listening.state !== 'paused' ? listening.source : null
    if (media) this.pause()
    if (reading) listening.pause()
    this.#held = media || reading ? { media, listen: reading } : null
  }

  /**
   * The voice session is over: pick up what it paused, if Settings → Feed
   * says to and nothing else has taken its place -- not what was stopped
   * meanwhile, nor what somebody started or restarted by hand. `pickUp` is
   * false when there is to be nothing to carry on, as at a lock.
   */
  resumeAfterVoice(pickUp = true): void {
    const held = this.#held
    this.#held = null
    if (!held || !pickUp || !(upnext.settings?.resumeAfterVoice ?? true)) return
    if (held.media && this.current && this.isCurrent(held.media) && !this.playing) this.resume()
    const reading = held.listen
    if (reading && listening.isReading(reading) && listening.state === 'paused') listening.resume()
  }

  // ── lifetime ─────────────────────────────────────────────────────────

  /** Write down where it is, and wait for every report on its way. */
  async #flush(): Promise<void> {
    this.#sync()
    this.#report()
    await this.#sending
  }

  /** The vault locked: nothing can be written, and nothing is kept. */
  #lock(): void {
    this.#token += 1
    this.#unsent = false
    this.#listened = null
    this.#held = null
    this.#unload()
    this.#sending = Promise.resolve()
  }

  /** Let go of the file and put everything back to nothing. */
  #unload(): void {
    const el = this.#el
    if (el) {
      el.pause()
      // Not `src = ''`, which some webviews answer with an error.
      el.removeAttribute('src')
      el.load()
    }
    this.#resetStretch()
    this.#unsent = false
    this.#pendingStart = null
    this.current = null
    this.playing = false
    this.buffering = false
    this.positionMs = 0
    this.durationMs = 0
    this.expanded = false
    this.error = null
    this.speed = this.#deviceSpeed
    this.#clearSession()
  }

  // ── the system's media controls ──────────────────────────────────────

  #session(): MediaSession | null {
    return typeof navigator !== 'undefined' && 'mediaSession' in navigator
      ? navigator.mediaSession
      : null
  }

  /** What the lock screen and the keyboard's media keys show. */
  #describe(request: PlayRequest): void {
    const session = this.#session()
    if (!session || typeof MediaMetadata === 'undefined') return
    try {
      session.metadata = new MediaMetadata({
        title: request.title,
        artist: request.source,
        artwork: request.artwork ? [{ src: request.artwork }] : [],
      })
    } catch {
      // A picture the system will not take is not a reason not to play.
    }
  }

  #setSessionActions(): void {
    const session = this.#session()
    if (!session) return
    const set = (action: MediaSessionAction, handler: MediaSessionActionHandler | null) => {
      try {
        session.setActionHandler(action, handler)
      } catch {
        // An action this webview does not know.
      }
    }
    set('play', () => this.resume())
    set('pause', () => this.pause())
    set('stop', () => this.stop())
    set('seekbackward', () => this.skip(-(upnext.settings?.skipBackS ?? SKIP_BACK_S)))
    set('seekforward', () => this.skip(upnext.settings?.skipForwardS ?? SKIP_FORWARD_S))
    set('seekto', (details) => {
      if (details.seekTime !== undefined && details.seekTime !== null) {
        this.seek(details.seekTime * 1000)
      }
    })
    set('nexttrack', () => void this.playNext())
  }

  #clearSessionActions(): void {
    const session = this.#session()
    if (!session) return
    for (const action of [
      'play',
      'pause',
      'stop',
      'seekbackward',
      'seekforward',
      'seekto',
      'nexttrack',
    ] as const) {
      try {
        session.setActionHandler(action, null)
      } catch {
        // As above.
      }
    }
  }

  #setSessionState(state: MediaSessionPlaybackState): void {
    const session = this.#session()
    if (session) session.playbackState = state
  }

  #updatePosition(): void {
    const session = this.#session()
    if (!session || this.durationMs <= 0 || this.positionMs > this.durationMs) return
    try {
      session.setPositionState({
        duration: this.durationMs / 1000,
        position: this.positionMs / 1000,
        playbackRate: this.speed,
      })
    } catch {
      // Refused for a length the system does not believe yet.
    }
  }

  #clearSession(): void {
    const session = this.#session()
    if (!session) return
    session.metadata = null
    session.playbackState = 'none'
  }
}

export const player = new Player()
