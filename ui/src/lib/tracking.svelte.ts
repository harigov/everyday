// What a day recorded in numbers, rather than in prose.
//
// A fourth store beside `app`, `todo` and `calendar`, and the smallest of
// them, because most of this domain is not state at all: the *definitions*
// live on the journal, which `app` already holds, and the aggregate a chart
// wants is a query rather than a cache. What is here is one day's readings —
// the ones the strip under the editor draws and writes.

import { api } from './api'
import { app, handle, isLocked } from './state.svelte'
import type { JournalId, Reading, Tracker, TrackerDay } from './types'

class TrackingState {
  /** The readings of the day the editor is showing. */
  readings = $state<Reading[]>([])
  /** Which journal and day `readings` holds, so a reload can be skipped. */
  #key = ''
  #journalId: JournalId | null = null
  #date = ''
  /** Set while a chip is mid-write, so a double-click cannot double-log. */
  busy = $state<string | null>(null)

  constructor() {
    app.onLock(() => this.reset())
  }

  reset() {
    this.readings = []
    this.#key = ''
    this.#journalId = null
    this.#date = ''
    this.busy = null
  }

  /** Does this vault's backend hold readings at all? */
  get enabled(): boolean {
    return app.status?.capabilities?.trackers === true
  }

  get journalId(): JournalId | null {
    return this.#journalId
  }

  get date(): string {
    return this.#date
  }

  /**
   * Show the readings for one journal on one day.
   *
   * Idempotent: the editor calls this whenever the open entry changes, which
   * includes every time it is saved, so re-asking for the day already on
   * screen has to cost nothing.
   */
  async open(journalId: JournalId, date: string, force = false) {
    const key = `${journalId}/${date}`
    if (key === this.#key && !force) return
    this.#journalId = journalId
    this.#date = date
    // Note where the key is *not* set: a call that happened before the
    // vault said whether it holds readings has not loaded the day, and must
    // not leave a key behind claiming it has.
    if (!this.enabled) return
    this.#key = key
    try {
      const rows = await api.readings({ journalId, from: date, to: date })
      // Guard against an out-of-order response: two day changes in quick
      // succession must not leave yesterday's readings under today's chips.
      if (this.#key === key) this.readings = rows
    } catch (e) {
      if (isLocked(e)) await app.lock()
      else this.readings = []
    }
  }

  /** Re-read the current day. What every write ends with. */
  async refresh() {
    if (this.#journalId) await this.open(this.#journalId, this.#date, true)
  }

  of(trackerId: string): Reading[] {
    return this.readings.filter((r) => r.trackerId === trackerId)
  }

  /** Has anything been recorded for this tracker today? */
  has(trackerId: string): boolean {
    return this.readings.some((r) => r.trackerId === trackerId)
  }

  /**
   * Record a value.
   *
   * `at` is left to the backend unless the caller has a time in mind: today
   * takes the current minute, a past day takes no time at all. See
   * `api.logReading`.
   */
  async log(tracker: Tracker, value: number, at?: string | null) {
    if (!this.#journalId || this.busy) return
    this.busy = tracker.id
    try {
      await api.logReading({
        journalId: this.#journalId,
        trackerId: tracker.id,
        value,
        date: this.#date,
        at: at ?? null,
        entryId: app.entry?.id ?? null,
      })
      await this.refresh()
    } catch (e) {
      await handle(e)
    } finally {
      this.busy = null
    }
  }

  /** Change a reading that exists: a corrected dose, a note, a time. */
  async save(reading: Reading) {
    try {
      await api.saveReading(reading)
      await this.refresh()
    } catch (e) {
      await handle(e)
    }
  }

  async remove(reading: Reading) {
    // Optimistic: a chip that stays lit after you have cleared it reads as a
    // failed click, and the refresh below puts it back if the write failed.
    this.readings = this.readings.filter((r) => r.id !== reading.id)
    try {
      await api.deleteReading(reading.id)
    } catch (e) {
      await handle(e)
    }
    await this.refresh()
  }

  /** Clear everything recorded for one tracker on the open day. */
  async clear(trackerId: string) {
    const mine = this.of(trackerId)
    this.readings = this.readings.filter((r) => r.trackerId !== trackerId)
    // One failure must not abandon the rest. They have all been removed from
    // the screen already, so stopping at the first would leave the others
    // both invisible and stored -- and the refresh below would put them back
    // with no explanation.
    let failure: unknown = null
    for (const r of mine) {
      try {
        await api.deleteReading(r.id)
      } catch (e) {
        failure ??= e
      }
    }
    if (failure) await handle(failure)
    await this.refresh()
  }

  /**
   * The aggregate over a window: one row per tracker per day.
   *
   * Not cached here. This is the analytics primitive rather than view state,
   * it is answered by a `GROUP BY` over an index in the backend, and the
   * caller knows the window it wants far better than this store does.
   */
  async days(opts: {
    journalId?: JournalId | null
    trackerIds?: string[]
    from: string
    to: string
  }): Promise<TrackerDay[]> {
    if (!this.enabled) return []
    try {
      return await api.trackerDays(opts)
    } catch (e) {
      if (isLocked(e)) await app.lock()
      return []
    }
  }
}

export const tracking = new TrackingState()
