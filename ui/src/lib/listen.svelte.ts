// Listening to an email or an article read aloud: the one session the player
// in the top bar draws, and the one place that listens to it.
//
// A session lives in the desktop shell -- it has the speakers -- and tells
// this window what it is doing over a channel (`listenStart`, `ListenEvent`).
// One thing is read at a time: starting another ends the one before.
//
// See `docs/plans/listen.md`.

import { api, hasShell, isMock, listenStart } from './api'
import { app, errorMessage, isLocked, quietly } from './state.svelte'
import type { ListenEvent, ListenSource, ListenState } from './types'

/** The speeds the player offers, as multiples of the voice's own. */
export const LISTEN_SPEEDS = [0.75, 1, 1.25, 1.5, 1.75, 2] as const

/**
 * Where the chosen speed is kept, on this device. Written out here rather than
 * through `prefs.ts`, whose keys are a closed list of their own.
 */
const SPEED_KEY = 'jatayu.listen.speed'

/** How long a Stop waits for the session to say it has ended before the
 *  player goes anyway. The shell answers Stop after the session is wound up,
 *  so this is only for one that never says. */
const ENDED_GRACE_MS = 3000

/** The speed kept from last time, or normal speed. */
function savedSpeed(): number {
  try {
    const kept = Number(localStorage.getItem(SPEED_KEY))
    return (LISTEN_SPEEDS as readonly number[]).includes(kept) ? kept : 1
  } catch {
    return 1
  }
}

/** Whether two sources are the same thing to read. */
function sameSource(a: ListenSource, b: ListenSource): boolean {
  if (a.type === 'mailThread') return b.type === 'mailThread' && a.threadId === b.threadId
  if (a.type === 'brief') return b.type === 'brief' && a.runId === b.runId
  return b.type === 'story' && a.storyId === b.storyId
}

class Listening {
  /** A session is starting or running. */
  active = $state(false)
  /** What it is doing; `null` when there is no session. */
  state = $state<ListenState | null>(null)
  /** What is being read, for the player. */
  title = $state('')
  source = $state<ListenSource | null>(null)
  /** Every sentence said so far, by index. */
  sentences = $state<string[]>([])
  /** The sentence being said now. */
  current = $state<number | null>(null)
  /** The chosen speed, kept between sessions. */
  speed = $state(savedSpeed())
  /** Why it could not start or why it ended. */
  error = $state<string | null>(null)

  /**
   * Which session events are being listened to. Bumped whenever one ends, so
   * a straggler from the session before -- or an event that lands after the
   * player has already gone -- changes nothing.
   */
  #run = 0
  /** Stop has been asked for, and the session has not said it is over. */
  #stopping = false
  #grace: ReturnType<typeof setTimeout> | null = null
  /** Told when a session is about to start, and when one ends of its own
   *  accord. See `onStart` and `onEnded`. */
  #startListeners = new Set<(source: ListenSource) => void>()
  #endListeners = new Set<(source: ListenSource) => void>()

  constructor() {
    // Gone with the key: a session reads the vault aloud.
    app.onLock(() => this.#lock())
  }

  /** This window can play aloud: the desktop app, on a vault of its own. */
  get available(): boolean {
    return isMock || (hasShell && app.remote === null)
  }

  /**
   * Be told when a session is about to start, with what it will read, before
   * whatever is being read is ended. Answers the way to stop being told.
   *
   * A registry rather than an import because the media player
   * (`player.svelte.ts`) imports this file -- it hands an article in Up Next
   * to it -- and Listen has to make the player pause when it starts.
   */
  onStart(listener: (source: ListenSource) => void): () => void {
    this.#startListeners.add(listener)
    return () => this.#startListeners.delete(listener)
  }

  /**
   * Be told when a session reaches its end on its own: the last sentence was
   * said, and nobody pressed Stop. Not for Stop, an error, a lock, or another
   * session replacing it -- the player moves on to the next thing in Up Next
   * only for a thing that was heard to the end.
   */
  onEnded(listener: (source: ListenSource) => void): () => void {
    this.#endListeners.add(listener)
    return () => this.#endListeners.delete(listener)
  }

  /** Whether `source` is what is being read now. */
  isReading(source: ListenSource): boolean {
    return this.active && this.source !== null && sameSource(this.source, source)
  }

  /** Start reading `source` aloud, ending whatever was being read. */
  async start(source: ListenSource, title: string): Promise<void> {
    this.error = null
    if (!this.available) {
      this.error = 'Listening needs the desktop app, on this computer.'
      return
    }
    this.#tell(this.#startListeners, source)
    if (this.active) {
      // The shell would replace it anyway. Ending it here first means its
      // last events cannot land in the session about to begin.
      const before = this.#run
      this.#reset(null)
      try {
        await api.listenStop()
      } catch (e) {
        await quietly(e)
      }
      // Another start got in while that was being asked.
      if (before + 1 !== this.#run) return
    }

    const run = ++this.#run
    this.active = true
    this.state = 'preparing'
    this.title = title
    this.source = source
    this.sentences = []
    this.current = null
    try {
      await listenStart(source, this.speed, (event) => this.#hear(run, event))
    } catch (e) {
      // It may already have ended, and said why, on the channel.
      if (run === this.#run) this.#reset(isLocked(e) ? null : errorMessage(e))
      await quietly(e)
    }
  }

  pause(): void {
    if (!this.active || this.state === 'paused') return
    this.#send(() => api.listenPause())
  }

  resume(): void {
    if (!this.active || this.state !== 'paused') return
    this.#send(() => api.listenResume())
  }

  /** Pause when playing, resume when paused. */
  toggle(): void {
    if (this.state === 'paused') this.resume()
    else this.pause()
  }

  /** Back a sentence. */
  back(): void {
    if (!this.active) return
    this.#send(() => api.listenSkip(-1))
  }

  /** Forward a sentence. */
  forward(): void {
    if (!this.active) return
    this.#send(() => api.listenSkip(1))
  }

  setSpeed(speed: number): void {
    this.speed = speed
    try {
      localStorage.setItem(SPEED_KEY, String(speed))
    } catch {
      // Not kept; this session still has it.
    }
    if (this.active) this.#send(() => api.listenSpeed(speed))
  }

  stop(): void {
    if (!this.active || this.#stopping) return
    this.#stopping = true
    const run = this.#run
    void api.listenStop().then(
      () => {
        if (run !== this.#run) return
        // The session answers Stop with `ended`, which is what puts
        // everything back. Should it not, the player does not stay up over a
        // session that is over.
        if (this.#grace) clearTimeout(this.#grace)
        this.#grace = setTimeout(() => {
          if (run === this.#run) this.#reset(null)
        }, ENDED_GRACE_MS)
      },
      async (e: unknown) => {
        if (run === this.#run) this.#reset(isLocked(e) ? null : errorMessage(e))
        await quietly(e)
      },
    )
  }

  /** One control, sent to the shell; a refusal is shown and the session goes on. */
  #send(call: () => Promise<void>) {
    const run = this.#run
    void call().catch(async (e: unknown) => {
      if (run === this.#run && !isLocked(e)) this.error = errorMessage(e)
      await quietly(e)
    })
  }

  /** One thing the session reported. */
  #hear(run: number, event: ListenEvent) {
    if (run !== this.#run || !this.active) return
    switch (event.type) {
      case 'state':
        this.state = event.state
        break
      case 'saying':
        this.sentences[event.index] = event.text
        this.current = event.index
        break
      case 'trouble':
        // The session carries on, so the player stays.
        this.error = event.message
        break
      case 'ended': {
        // Read before the reset, which clears both. A Stop is also answered
        // with `ended`, and with no error: only `#stopping` tells it from
        // the end of the text.
        const source = this.source
        const ofItsOwn = !this.#stopping && event.error === null
        this.#reset(event.error)
        if (ofItsOwn && source) this.#tell(this.#endListeners, source)
        break
      }
    }
  }

  /** Call each listener. One that throws must not stop the session, or the rest. */
  #tell(listeners: Set<(source: ListenSource) => void>, source: ListenSource) {
    for (const listener of [...listeners]) {
      try {
        listener(source)
      } catch {
        // A listener's trouble is its own.
      }
    }
  }

  /** Back to nothing, keeping `error` for the player to show. */
  #reset(error: string | null) {
    this.#run += 1
    this.#stopping = false
    if (this.#grace) clearTimeout(this.#grace)
    this.#grace = null
    this.active = false
    this.state = null
    this.title = ''
    this.source = null
    this.sentences = []
    this.current = null
    this.error = error
  }

  #lock() {
    if (this.active) void api.listenStop().catch(() => {})
    this.#reset(null)
  }
}

export const listening = new Listening()
