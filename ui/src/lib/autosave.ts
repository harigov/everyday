// Debounce-and-write, once.
//
// All four apps do the same thing with an edit: mutate the record in place
// so every view of it redraws from the same object, mark it dirty, and put it
// on disk a beat after the typing stops. That was written out three times --
// a private `Set` of ids, a private timer, a `patch` that reset the timer and
// a `flush` that cleared the set and wrote -- with three declarations of the
// delay, one of which had drifted to a different number while its comment
// still claimed it matched the others.
//
// The subtle part, and the reason this is worth having in one place rather
// than three: `flush` takes the pending ids and clears the set *before* it
// awaits the write. An edit made while that write is in flight lands in a
// fresh set and schedules its own flush, so it is written rather than
// silently dropped. Get that ordering wrong in one of three copies and the
// bug is a keystroke that vanishes under a slow disk, once a fortnight.

import { VaultError } from './types'

/**
 * The vault locking under a write, as opposed to the write failing.
 *
 * Duplicated from `state.svelte.ts`'s `isLocked` rather than imported: that
 * module imports this one, and a cycle between them is not worth introducing
 * for one comparison.
 */
function isLockedError(e: unknown): boolean {
  return e instanceof VaultError && e.code === 'locked'
}

/**
 * Idle delay before an edited record is written.
 *
 * Long enough that a sentence is one write rather than forty, short enough
 * that closing a laptop mid-thought does not lose the thought.
 */
export const AUTOSAVE_MS = 700

/**
 * Longest gap between retries of a write that keeps failing.
 *
 * The delay doubles from `AUTOSAVE_MS` and stops here, so a vault on a full
 * disk or an unplugged drive is retried about twice a minute -- often enough
 * to recover the moment the problem goes away, rarely enough not to be a spin
 * loop behind an error banner.
 */
const MAX_RETRY_MS = 30_000

/**
 * A set of records waiting to be written, and the timer that writes them.
 *
 * `write` is handed the ids that changed. It is the store's job to turn those
 * into records and call the backend, because only the store knows where its
 * records live and what a failure should do to the screen.
 *
 * `write` must **reject** if the write did not land, after doing whatever it
 * wants to the screen. That is what lets the ids go back into the dirty set
 * to be tried again: an edit is only forgotten once it is on disk. A `write`
 * that swallows its own errors silently discards the edits it was given.
 */
export class Autosave<Id> {
  #dirty = new Set<Id>()
  #timer: ReturnType<typeof setTimeout> | null = null
  #write: (ids: ReadonlySet<Id>) => Promise<void>
  #delay: number
  #retryDelay: number | null = null
  /**
   * The tail of the chain of writes, so that only one is ever in the air.
   *
   * Without it, `flush` was "cancel the timer, and if nothing is dirty
   * return" -- which is a lie while a previous write is still on the wire.
   * The save-before-close handshake calls `flush` on all four stores and
   * closes the window when they resolve, so a flush that returned early
   * during an in-flight write reported "everything is on disk" about a write
   * that had not landed yet. Chaining also keeps two writes of the same
   * record from racing each other into the backend.
   */
  #queue: Promise<void> = Promise.resolve()

  constructor(write: (ids: ReadonlySet<Id>) => Promise<void>, delay = AUTOSAVE_MS) {
    this.#write = write
    this.#delay = delay
  }

  /** Note that `id` has changed, and start (or restart) the countdown. */
  touch(id: Id) {
    this.#dirty.add(id)
    this.#restart()
  }

  /** The same for several at once: what a board reorder produces. */
  touchAll(ids: Iterable<Id>) {
    for (const id of ids) this.#dirty.add(id)
    this.#restart()
  }

  /** Forget a record without writing it. It has been deleted. */
  forget(id: Id) {
    this.#dirty.delete(id)
  }

  get pending(): boolean {
    return this.#dirty.size > 0
  }

  /**
   * Write everything outstanding now. Safe to call when nothing is dirty.
   *
   * Resolves once every write queued before it has landed, not merely once
   * the dirty set looks empty -- see `#queue`.
   */
  flush(): Promise<void> {
    this.#cancelTimer()
    const next = this.#queue.then(() => this.#drain())
    // The tail never rejects, so one failed write cannot poison every flush
    // that follows it. `#drain` already reports failures its own way.
    this.#queue = next.catch(() => {})
    return next
  }

  async #drain(): Promise<void> {
    if (this.#dirty.size === 0) return
    // Taken and cleared before the await, so an edit arriving during the
    // write is queued again rather than lost with the set that held it.
    const writing: ReadonlySet<Id> = new Set(this.#dirty)
    this.#dirty.clear()
    try {
      await this.#write(writing)
      this.#retryDelay = null
    } catch (e) {
      // The write failed, so these edits are still only in memory. Clearing
      // the set above was right -- a newer edit to the same record must not
      // be overwritten by this retry -- but dropping the ids would mean the
      // text is never written again unless the author happens to touch the
      // same record. They go back, and the timer starts again.
      //
      // Except for a lock. There is nothing to retry against a locked vault,
      // the records have already been dropped from memory by the reset that
      // a lock triggers, and rescheduling would leave a timer firing into a
      // vault that is not open.
      if (isLockedError(e)) return
      for (const id of writing) this.#dirty.add(id)
      this.#retryDelay = Math.min(
        this.#retryDelay === null ? this.#delay : this.#retryDelay * 2,
        MAX_RETRY_MS,
      )
      this.#restart()
    }
  }

  /** Is a previous write being retried? Drives the "not saved" indicator. */
  get retrying(): boolean {
    return this.#retryDelay !== null && this.#dirty.size > 0
  }

  /** Drop everything outstanding without writing it. What a lock does. */
  cancel() {
    this.#cancelTimer()
    this.#dirty.clear()
    this.#retryDelay = null
  }

  #restart() {
    this.#cancelTimer()
    this.#timer = setTimeout(() => void this.flush(), this.#retryDelay ?? this.#delay)
  }

  #cancelTimer() {
    if (this.#timer) clearTimeout(this.#timer)
    this.#timer = null
  }
}
