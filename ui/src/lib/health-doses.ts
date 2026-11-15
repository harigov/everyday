// Counting doses, shared with the condition dashboard, which lists the same
// medicines with the same adherence.
//
// What was due is worked out from the schedule rather than asked of the
// vault, so a list of thirty medicines does not make thirty reads: the
// schedule says which days a dose falls on, `started` and `stopped` bound
// them, and the doses already on screen say what was taken.

import { addDays, daysApart, isoDate, startOfDay } from './time'
import type { Dose, Medication } from './types'

/** 1 Monday .. 7 Sunday, as a schedule's `days` count them. */
function isoWeekday(iso: string): number {
  const d = startOfDay(iso).getDay()
  return d === 0 ? 7 : d
}

/**
 * The last local day `med`'s schedule asks for anything, or null while it
 * runs on: the vault's own rule (`schedule::due_times`), so this count and
 * the vault's adherence agree. A stop ends it on its `stopped` day, else the
 * day it was stopped; a pause the day before it was paused (or its `stopped`
 * day, if that is earlier). The day it changed is read in this computer's
 * zone, as every day here is.
 */
export function lastDueDay(med: Medication): string | null {
  const changed = isoDate(new Date(med.updatedAt))
  switch (med.status) {
    case 'active':
      return med.stopped ?? null
    case 'stopped':
      return med.stopped ?? changed
    case 'paused': {
      const beforePause = addDays(changed, -1)
      return med.stopped && med.stopped < beforePause ? med.stopped : beforePause
    }
  }
}

/** How many doses `med`'s schedule puts on local days `from..=to`. */
export function expectedDoses(med: Medication, from: string, to: string): number {
  const s = med.schedule
  if (s.type === 'asNeeded' || s.times.length === 0) return 0
  const last = lastDueDay(med)
  let count = 0
  for (let day = from, guard = 0; day <= to && guard < 400; day = addDays(day, 1), guard++) {
    if (med.started && day < med.started) continue
    if (last && day > last) continue
    if (s.type === 'weekly' && !s.days.includes(isoWeekday(day))) continue
    if (s.type === 'everyNDays') {
      const gap = daysApart(s.from, day)
      if (gap < 0 || s.n < 1 || gap % s.n !== 0) continue
    }
    count += s.times.length
  }
  return count
}

export interface Steadiness {
  expected: number
  taken: number
  skipped: number
  /** Taken over expected, `0..=1`; null when nothing was expected. */
  share: number | null
}

/** What was taken of what was due, over local days `from..=to`. */
export function steadiness(med: Medication, doses: Dose[], from: string, to: string): Steadiness {
  const expected = expectedDoses(med, from, to)
  let taken = 0
  let skipped = 0
  for (const d of doses) {
    if (d.medicationId !== med.id) continue
    const day = isoDate(new Date(d.at))
    if (day < from || day > to) continue
    if (d.status === 'taken') taken += 1
    else if (d.status === 'skipped') skipped += 1
  }
  return { expected, taken, skipped, share: expected > 0 ? Math.min(1, taken / expected) : null }
}

/** How many times a day the schedule asks for it, on average. */
export function takesPerDay(med: Medication): number {
  const s = med.schedule
  switch (s.type) {
    case 'daily':
      return s.times.length
    case 'weekly':
      return (s.days.length * s.times.length) / 7
    case 'everyNDays':
      return s.times.length / Math.max(1, s.n)
    default:
      return 0
  }
}

/** Days the supply lasts at the scheduled rate, if it is tracked and the
 *  medicine is not taken only as needed. `remaining` counts what a take
 *  uses up in the dose's own unit, so a take of two tablets uses two. */
export function daysOfSupply(med: Medication): number | null {
  if (!med.supply) return null
  const use = takesPerDay(med) * (med.dose > 0 ? med.dose : 1)
  return use > 0 ? med.supply.remaining / use : null
}
