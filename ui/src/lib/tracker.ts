// The arithmetic of tracking, with no state in it.
//
// Split from `tracking.svelte.ts` for the reason `time.ts` is split from
// `calendar.svelte.ts`: this is the part that decides what a number *means*
// -- whether a day of doses adds up or a day of severities averages, whether
// a reading is a moment or a span -- and it is worth being able to test that
// without a store, a backend or a browser.

import type { Journal, Reading, Tracker } from './types'
import { aggregateOf } from './types'

/** Rounded the way a person writes a number: 45, not 45.0000001. */
export function formatNumber(n: number): string {
  if (!Number.isFinite(n)) return '0'
  return String(Math.round(n * 100) / 100)
}

/** How one reading of this tracker reads: `400 mg`, `6/10`, `Done`. */
export function formatValue(tracker: Tracker, value: number): string {
  if (tracker.kind === 'check') return value > 0 ? 'Done' : 'Not done'
  if (tracker.kind === 'scale') return `${formatNumber(value)}/${formatNumber(tracker.scaleMax)}`
  return tracker.unit ? `${formatNumber(value)} ${tracker.unit}` : formatNumber(value)
}

/**
 * How a *day* of this tracker reads, given everything recorded on it.
 *
 * Not the same question as the one above, and the difference is the whole
 * reason `aggregateOf` exists: two doses of 400 mg is "800 mg", two
 * headaches of 3 and 7 is "5/10 avg" and never "10/10", and a habit is a
 * tick rather than a number at all.
 */
export function formatDay(tracker: Tracker, readings: Reading[]): string {
  if (readings.length === 0) return ''
  const total = dayValue(tracker, readings)
  // A check can be recorded as *not* done -- 0 rather than no row at all --
  // which is a different fact from having forgotten to answer, and has to
  // read as one.
  if (tracker.kind === 'check') return readings.some((r) => r.value > 0) ? 'Done' : 'Not done'
  if (tracker.kind === 'scale') {
    const avg = `${formatNumber(total)}/${formatNumber(tracker.scaleMax)}`
    return readings.length > 1 ? `${avg} avg` : avg
  }
  const value = tracker.unit ? `${formatNumber(total)} ${tracker.unit}` : formatNumber(total)
  // "2 x 400 mg" would be a lie when the two doses differed, so the count is
  // shown beside the total rather than as a multiplication.
  return readings.length > 1 ? `${value} · ${readings.length}` : value
}

/** The one number a day is worth, under this tracker's aggregate. */
export function dayValue(tracker: Tracker, readings: Reading[]): number {
  if (readings.length === 0) return 0
  const sum = readings.reduce((n, r) => n + r.value, 0)
  switch (aggregateOf(tracker.kind)) {
    case 'count':
      return readings.length
    case 'mean':
      return sum / readings.length
    default:
      return sum
  }
}

/**
 * Minutes a reading occupies on the calendar, or `null` for a moment.
 *
 * Only a quantity measured in time has a length: "45 min run" is 07:00 to
 * 07:45, while a 500 mg dose is a point however it is measured. Mirrors
 * `Tracker::duration_minutes` in the core, and is the one piece of that
 * logic the interface needs its own copy of, because the grid asks about
 * every reading on screen sixty times a minute.
 */
export function durationMinutes(tracker: Tracker, value: number): number | null {
  if (tracker.kind !== 'amount') return null
  const unit = tracker.unit.trim().toLowerCase()
  let minutes: number
  if (['min', 'mins', 'minute', 'minutes'].includes(unit)) minutes = value
  else if (['h', 'hr', 'hrs', 'hour', 'hours'].includes(unit)) minutes = value * 60
  else return null
  return Number.isFinite(minutes) && minutes > 0 ? minutes : null
}

/** The trackers a day's chips should offer, in the order they were arranged. */
export function activeTrackers(journal: Journal | null): Tracker[] {
  if (!journal) return []
  return journal.trackers
    .filter((t) => !t.archived)
    .sort((a, b) => a.sortOrder - b.sortOrder || a.createdAt.localeCompare(b.createdAt))
}
