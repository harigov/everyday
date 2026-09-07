// Debounce-and-write, once.
//
// All three apps do the same thing with an edit: mutate the record in place
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

/**
 * Idle delay before an edited record is written.
 *
 * Long enough that a sentence is one write rather than forty, short enough
 * that closing a laptop mid-thought does not lose the thought.
 */
export const AUTOSAVE_MS = 700

/**
 * A set of records waiting to be written, and the timer that writes them.
 *
 * `write` is handed the ids that changed. It is the store's job to turn those
 * into records and call the backend, because only the store knows where its
 * records live and what a failure should do to the screen.
 */
export class Autosave<Id> {
  #dirty = new Set<Id>()
  #timer: ReturnType<typeof setTimeout> | null = null
  #write: (ids: ReadonlySet<Id>) => Promise<void>
  #delay: number

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

  /** Write everything outstanding now. Safe to call when nothing is dirty. */
  async flush(): Promise<void> {
    this.#cancelTimer()
    if (this.#dirty.size === 0) return
    // Taken and cleared before the await, so an edit arriving during the
    // write is queued again rather than lost with the set that held it.
    const writing: ReadonlySet<Id> = new Set(this.#dirty)
    this.#dirty.clear()
    await this.#write(writing)
  }

  /** Drop everything outstanding without writing it. What a lock does. */
  cancel() {
    this.#cancelTimer()
    this.#dirty.clear()
  }

  #restart() {
    this.#cancelTimer()
    this.#timer = setTimeout(() => void this.flush(), this.#delay)
  }

  #cancelTimer() {
    if (this.#timer) clearTimeout(this.#timer)
    this.#timer = null
  }
}
