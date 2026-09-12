// What a day recorded in numbers, rather than in prose.
//
// Two things live here. The vault's tracker *definitions*, which used to sit
// on each journal and are records of their own now — held once, because five
// unrelated places need them to draw a chip and none of them should have to
// wait. And one day's readings: the ones the strip under the editor draws
// and writes.
//
// What is deliberately not here is the aggregate a chart wants. That is a
// `GROUP BY` in the backend and the caller knows its window far better than
// this store does, so `days` asks and does not cache.

import { api } from './api'
import { parseQuickTrack } from './quicktrack'
import { app, handle, isLocked, quietly } from './state.svelte'
import { latest } from './store/latest'
import { todayIso } from './time'
import type {
  EntryId,
  JournalId,
  QuickReading,
  Reading,
  Tracker,
  TrackerDay,
  TrackerKind,
} from './types'

class TrackingState {
  /** Every tracker in the vault, archived ones included. */
  trackers = $state<Tracker[]>([])
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

  #loaded = false
  #generation = latest()

  reset() {
    this.#generation.next()
    this.trackers = []
    this.readings = []
    this.#key = ''
    this.#journalId = null
    this.#date = ''
    this.busy = null
    this.#loaded = false
  }

  /**
   * Load the vault's trackers, once.
   *
   * Also where a vault written before trackers became records has its old
   * definitions moved out of its journals — the backend does that on the
   * first `list_trackers`, so this is the call that triggers it.
   */
  async load(force = false) {
    if (!this.enabled) return
    if (this.#loaded && !force) return
    const mine = this.#generation.next()
    try {
      const all = await api.trackers()
      if (!this.#generation.isCurrent(mine)) return
      this.trackers = all
      this.#loaded = true
    } catch (e) {
      await handle(e)
    }
  }

  /** One tracker by id, or undefined if it has been deleted. */
  tracker(id: string): Tracker | undefined {
    return this.trackers.find((t) => t.id === id)
  }

  /** Everything not retired, in the order the chips are arranged. */
  get live(): Tracker[] {
    return this.trackers
      .filter((t) => !t.archived)
      .sort((a, b) => a.sortOrder - b.sortOrder || a.createdAt.localeCompare(b.createdAt))
  }

  /**
   * Draw a tracker's readings on the calendar, or stop drawing them.
   *
   * Here rather than at each call site because three places offer the same
   * switch without owning the settings dialog: the chip under the day, a
   * reading already on the grid, and the Overview's habits list.
   */
  async setOnCalendar(id: string, onCalendar: boolean) {
    const tracker = this.tracker(id)
    if (!tracker || tracker.onCalendar === onCalendar) return
    await this.saveTracker({ ...$state.snapshot(tracker), onCalendar })
  }

  async saveTracker(tracker: Tracker) {
    try {
      await api.saveTracker($state.snapshot(tracker))
      await this.load(true)
    } catch (e) {
      await handle(e)
    }
  }

  /**
   * Mint a tracker, save it, and return it.
   *
   * The id and the timestamps come from the backend for the reason a
   * journal's do: `crypto.randomUUID` needs a secure context the packaged
   * webview does not always provide, and a tracker that silently fails to
   * get an id is one whose readings all pile up under the same one.
   */
  async addTracker(name: string, kind: TrackerKind): Promise<Tracker | null> {
    try {
      const tracker = await api.newTracker(name, kind)
      await api.saveTracker(tracker)
      await this.load(true)
      return this.tracker(tracker.id) ?? null
    } catch (e) {
      await handle(e)
      return null
    }
  }

  /** Delete a tracker and every reading it made. Answers how many went. */
  async deleteTracker(id: string): Promise<number> {
    try {
      const removed = await api.deleteTracker(id)
      await this.load(true)
      await this.refresh()
      return removed
    } catch (e) {
      await handle(e)
      return 0
    }
  }

  /** Fold one tracker into another, keeping both histories. */
  async merge(from: string, into: string): Promise<number> {
    try {
      const moved = await api.mergeTrackers(from, into)
      await this.load(true)
      await app.refreshJournals()
      await this.refresh()
      return moved
    } catch (e) {
      await handle(e)
      return 0
    }
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
    if (this.busy) return
    this.busy = tracker.id
    try {
      await api.logReading({
        // Both absent when there is no page in view, which is what a
        // reading logged from the Overview or the tray looks like.
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

  /**
   * Record one line, making the tracker if there is not one yet.
   *
   * The lazy half of capture, and the reason it is in the store rather than
   * in the popover: three call sites want it — the strip's plus chip, the
   * Overview's today pane, and the tray — and the resolve-or-create step
   * must not be reimplemented in any of them.
   *
   * Returns what happened, so the caller can say it. `null` means the line
   * named nothing, which is a real answer and not a failure.
   */
  async logLine(
    line: string,
    where: { journalId?: JournalId | null; entryId?: EntryId | null; date?: string } = {},
  ): Promise<{ tracker: Tracker; created: boolean } | null> {
    if (!this.enabled) return null
    const parsed = parseQuickTrack(line, this.trackers)
    if (!parsed.target) return null

    let tracker: Tracker | null = null
    let created = false
    if (parsed.target.kind === 'existing') {
      tracker = parsed.target.tracker
    } else {
      const spec = parsed.target
      const made = await this.addTracker(spec.name, spec.trackerKind)
      if (!made) return null
      created = true
      // The unit and the ceiling the line implied. Written as a second save
      // rather than passed to `newTracker`, which mints defaults and knows
      // nothing about a grammar the interface owns.
      const next: Tracker = {
        ...made,
        unit: spec.unit,
        scaleMax: spec.scaleMax,
        defaultValue: spec.trackerKind === 'check' ? 1 : parsed.value || 1,
      }
      await this.saveTracker(next)
      tracker = this.tracker(made.id) ?? next
    }

    const date = where.date ?? this.#date ?? todayIso()
    try {
      await api.logReading({
        trackerId: tracker.id,
        value: parsed.value,
        date,
        journalId: where.journalId ?? null,
        entryId: where.entryId ?? null,
      })
    } catch (e) {
      await handle(e)
      return null
    }
    await this.refresh()
    return { tracker, created }
  }

  /**
   * Record a number the quick model found in a day's writing.
   *
   * Separate from `record` because there is no line to parse: the value, the
   * hour and the tracker have already been decided, either by matching one
   * that exists or -- when the model proposed a name instead -- by making one
   * here. What it shares with `record` is everything after that, which is the
   * part that matters: the same `logReading`, the same refresh, and no
   * special path through the store for a number a model suggested.
   */
  async recordSuggested(
    suggestion: QuickReading,
    where: { journalId?: JournalId; entryId?: EntryId; date?: string } = {},
  ): Promise<Tracker | null> {
    if (!this.enabled) return null
    let tracker = suggestion.trackerId ? this.tracker(suggestion.trackerId) : null
    if (!tracker) {
      // A tracker made by the act of recording, exactly as `record` does it
      // for a typed line. `Amount` rather than a guess at something cleverer:
      // an amount stores the number that was actually said, which is at worst
      // incomplete, where a check would silently discard it.
      const made = await this.addTracker(suggestion.name, 'amount')
      if (!made) return null
      tracker = made
    }
    const date = where.date ?? this.#date ?? todayIso()
    try {
      await api.logReading({
        trackerId: tracker.id,
        value: suggestion.value,
        date,
        journalId: where.journalId ?? null,
        entryId: where.entryId ?? null,
      })
    } catch (e) {
      await handle(e)
      return null
    }
    await this.refresh()
    return tracker
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
      await quietly(e)
      return []
    }
  }
}

export const tracking = new TrackingState()
