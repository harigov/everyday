// Dictation: talking instead of typing. Mail's "write by voice" is the first
// user: what is said becomes the instruction the model writes the email from.
//
// A session lives in the desktop shell -- it has the microphone -- and tells
// this window what it heard over a channel (`dictationStart`,
// `DictationEvent`). One at a time; it ends on Stop, after 15 seconds of
// quiet, or after three minutes.
//
// See `docs/plans/listen.md`.

import { api, dictationStart, hasShell, isMock } from './api'
import { app, errorMessage, isLocked, quietly } from './state.svelte'
import type { DictationEnd, DictationEvent, DictationState } from './types'

/** How long a Stop waits for the session to say it has ended. The session
 *  waits up to 20 seconds for the last words to be transcribed
 *  (`speech::dictate`'s `FINISH_WAIT`), so this is longer than that; it is
 *  only for one that never says. */
const ENDED_GRACE_MS = 25_000

class Dictation {
  /** A session is starting or running. */
  active = $state(false)
  state = $state<DictationState | null>(null)
  /** Everything heard so far. */
  text = $state('')
  /** Which field started it, so only that one draws as listening. */
  owner = $state<string | null>(null)
  /** Why it could not start or why it failed. */
  error = $state<string | null>(null)

  /**
   * Which session events are being listened to. Bumped whenever one ends, so
   * a straggler from the session before changes nothing.
   */
  #run = 0
  /** What to tell whoever started it when it ends, once. */
  #done: ((text: string, reason: DictationEnd) => void) | null = null
  /** Stop has been asked for, and the session has not said it is over. */
  #stopping = false
  #grace: ReturnType<typeof setTimeout> | null = null

  constructor() {
    // Gone with the key: what is said is not kept anywhere.
    app.onLock(() => this.#lock())
  }

  /** This window can listen: the desktop app, on a vault of its own. */
  get available(): boolean {
    return isMock || (hasShell && app.remote === null)
  }

  /**
   * Start listening for `owner`. `onDone` is called once, when the session
   * ends other than by cancelling, with everything heard and why
   * it ended.
   *
   * Does nothing while a session is running, whoever's: a second microphone
   * button cannot take the first one's words from it. The owner's own button
   * reads `owner` to draw as listening, and is Stop by then.
   */
  async start(owner: string, onDone: (text: string, reason: DictationEnd) => void): Promise<void> {
    if (this.active) return
    this.error = null
    if (!this.available) {
      this.error = 'Writing by voice needs the desktop app, on this computer.'
      return
    }
    const run = ++this.#run
    this.#done = onDone
    this.active = true
    this.owner = owner
    this.text = ''
    this.state = 'listening'
    try {
      await dictationStart((event) => this.#hear(run, event))
    } catch (e) {
      // It may already have ended, and said why, on the channel.
      if (run === this.#run) this.#reset(isLocked(e) ? null : errorMessage(e))
      await quietly(e)
    }
  }

  /** Stop listening and finish with what was heard. */
  stop(): void {
    if (!this.active || this.#stopping) return
    this.#stopping = true
    const run = this.#run
    void api.dictationStop().then(
      () => {
        if (run !== this.#run) return
        // The session answers Stop with `ended`, which carries the words.
        // Should it not, what was heard so far is still what was said.
        if (this.#grace) clearTimeout(this.#grace)
        this.#grace = setTimeout(() => {
          if (run === this.#run) this.#finish(this.text, 'stopped', null)
        }, ENDED_GRACE_MS)
      },
      async (e: unknown) => {
        if (run === this.#run) this.#reset(isLocked(e) ? null : errorMessage(e))
        await quietly(e)
      },
    )
  }

  /** Stop listening and throw away what was heard. */
  cancel(): void {
    if (!this.active) return
    void api.dictationCancel().catch((e: unknown) => quietly(e))
    // At once: the words are not wanted, so there is nothing to wait for.
    this.#reset(null)
  }

  /** One thing the session reported. */
  #hear(run: number, event: DictationEvent) {
    if (run !== this.#run || !this.active) return
    switch (event.type) {
      case 'state':
        this.state = event.state
        break
      case 'heard':
        this.text = event.text
        break
      case 'trouble':
        // The session carries on, so the field stays listening.
        this.error = event.message
        break
      case 'ended':
        this.#finish(event.text, event.reason, event.error)
        break
    }
  }

  /** The session is over: put everything back, then tell whoever asked. */
  #finish(text: string, reason: DictationEnd, error: string | null) {
    const done = this.#done
    this.#reset(reason === 'failed' ? (error ?? 'Dictation stopped working.') : null)
    // A failure still hands over what was heard: the words said before the
    // microphone went are the person's, and theirs to keep.
    if (reason !== 'cancelled') done?.(text, reason)
  }

  /** Back to nothing, keeping `error` for the field to show. */
  #reset(error: string | null) {
    this.#run += 1
    this.#stopping = false
    if (this.#grace) clearTimeout(this.#grace)
    this.#grace = null
    this.#done = null
    this.active = false
    this.state = null
    this.text = ''
    this.owner = null
    this.error = error
  }

  #lock() {
    if (this.active) void api.dictationCancel().catch(() => {})
    this.#reset(null)
  }
}

export const dictation = new Dictation()
