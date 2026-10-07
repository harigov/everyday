// Repeats, in the words a person picks them in.
//
// The core holds a repeat as a small structure rather than as `RRULE` text
// -- see `everyday-core`'s `recurrence` module for why -- and this file is
// the interface's half of that: the handful of presets a "Repeat" menu
// offers for a given start ("Every week on Tuesday", "Every month on the
// second Tuesday"), the sentence a chosen rule reads as, and which preset,
// if any, a rule loaded from a server already is.
//
// It also knows which days a rule falls on, for one reader only: the
// in-browser mock, which has to expand a series the way the core would
// without a second engine of its own. The grid never asks -- every repeat it
// draws arrives already written out, one row per occurrence.
//
// Pure: no store, no component, no clock. `scripts/recurrence.test.mjs`
// checks it, because every rule in here is a sentence somebody reads before
// they press Save, and a preset that quietly meant a different day would be
// a wrong meeting on forty Tuesdays.

import { dateFormat } from './format'
import { addDays, isoDate, startOfDay } from './time'
import type { Frequency, Recurrence, RecurrenceDay } from './types'

/**
 * A day a repeat falls on, as the wire spells it: `"monday"` -- never a
 * routine trigger's `Weekday`, which spells its days `"mon"`.
 */
export type RepeatDay = RecurrenceDay

/** Monday first, the way the core and RFC 5545 both count a week. */
export const WEEKDAYS: readonly RepeatDay[] = [
  'monday',
  'tuesday',
  'wednesday',
  'thursday',
  'friday',
  'saturday',
  'sunday',
]

/** Monday to Friday: what "every weekday" means here, and in every calendar. */
export const WORKDAYS: readonly RepeatDay[] = WEEKDAYS.slice(0, 5)

const TITLES: Record<RepeatDay, string> = {
  monday: 'Monday',
  tuesday: 'Tuesday',
  wednesday: 'Wednesday',
  thursday: 'Thursday',
  friday: 'Friday',
  saturday: 'Saturday',
  sunday: 'Sunday',
}

const UNITS: Record<Frequency, string> = {
  daily: 'day',
  weekly: 'week',
  monthly: 'month',
  yearly: 'year',
}

const ORDINALS: Record<number, string> = {
  1: 'first',
  2: 'second',
  3: 'third',
  4: 'fourth',
  [-1]: 'last',
}

const DAY = /^\d{4}-\d{2}-\d{2}$/

/** "Monday". English, like every other sentence this file writes. */
export function weekdayTitle(day: RepeatDay): string {
  return TITLES[day]
}

/** The weekday a `YYYY-MM-DD` falls on. */
export function weekdayOf(day: string): RepeatDay {
  // `getDay` counts from Sunday; the wire counts from Monday.
  return WEEKDAYS[(startOfDay(day).getDay() + 6) % 7]!
}

/** "first", "second", … "last", for a week of the month. */
export function ordinalWeek(week: number): string {
  return ORDINALS[week] ?? 'last'
}

// ── Days in a zone ────────────────────────────────────────────────────────
//
// A repeat is anchored to the zone its event is quoted in, not to the
// machine looking at it: "every Tuesday at nine" in London is still a
// Tuesday when it is read in Chennai. So the day a start falls on is asked
// of that zone, and an all-day event's midnights are that zone's midnights.

/**
 * The `YYYY-MM-DD` that `at` falls on in `tz` -- the machine's own zone when
 * `tz` is missing or not one the platform knows. A bare day is its own
 * answer, so a caller holding either can ask.
 */
export function dayIn(at: string, tz?: string | null): string {
  if (DAY.test(at)) return at
  const when = new Date(at)
  if (!tz) return isoDate(when)
  try {
    const parts = new Intl.DateTimeFormat('en-US', {
      timeZone: tz,
      year: 'numeric',
      month: '2-digit',
      day: '2-digit',
    }).formatToParts(when)
    const part = (type: string) => parts.find((p) => p.type === type)?.value ?? ''
    return `${part('year')}-${part('month')}-${part('day')}`
  } catch {
    return isoDate(when)
  }
}

/**
 * The instant `day` begins in `tz`, as RFC 3339 -- what an all-day event
 * starts at, and the midnight after its last day is what it ends at.
 *
 * Asked twice: once at a guess, and again at the instant that guess
 * produced, so a day that begins on a change of clocks still lands on its
 * first minute rather than an hour either side of it.
 */
export function midnightIn(day: string, tz?: string | null): string {
  if (!tz) return startOfDay(day).toISOString()
  try {
    const [y = 1970, m = 1, d = 1] = day.split('-').map(Number)
    const wall = Date.UTC(y, m - 1, d)
    let at = wall - offsetAt(wall, tz)
    at = wall - offsetAt(at, tz)
    return new Date(at).toISOString()
  } catch {
    return startOfDay(day).toISOString()
  }
}

/** How far `tz`'s wall clock is ahead of UTC at `ms`, in milliseconds. */
function offsetAt(ms: number, tz: string): number {
  const parts = new Intl.DateTimeFormat('en-US', {
    timeZone: tz,
    hourCycle: 'h23',
    year: 'numeric',
    month: 'numeric',
    day: 'numeric',
    hour: 'numeric',
    minute: 'numeric',
    second: 'numeric',
  }).formatToParts(new Date(ms))
  const n = (type: string) => Number(parts.find((p) => p.type === type)?.value ?? 0)
  const wall = Date.UTC(
    n('year'),
    n('month') - 1,
    n('day'),
    n('hour') % 24,
    n('minute'),
    n('second'),
  )
  // Whole minutes: no zone in use is offset by seconds, and the second the
  // formatter rounded away would otherwise ride along into every midnight.
  return Math.round((wall - ms) / 60_000) * 60_000
}

// ── The presets ───────────────────────────────────────────────────────────

/** Which row of the Repeat menu a rule is, or `custom` for anything else. */
export type PresetKey =
  'none' | 'daily' | 'weekdays' | 'weekly' | 'monthly' | 'monthlyNth' | 'monthlyLast' | 'yearly'

export interface RepeatPreset {
  key: PresetKey
  label: string
  rule: Recurrence | null
}

function every(frequency: Frequency): Recurrence {
  return { frequency, interval: 1 }
}

/** Which week of its month `day` is in, and whether it is the last one. */
export function weekOfMonth(day: string): { nth: number; last: boolean } {
  const [y = 1970, m = 1, d = 1] = day.split('-').map(Number)
  const length = new Date(y, m, 0).getDate()
  return { nth: Math.ceil(d / 7), last: d + 7 > length }
}

/** "October 7" or "7 October", in the reader's own order. */
function monthDay(day: string): string {
  return dateFormat({ month: 'long', day: 'numeric' }).format(startOfDay(day))
}

/** "31 Dec 2026" or "Dec 31, 2026", in the reader's own order. */
function fullDate(day: string): string {
  return dateFormat({ day: 'numeric', month: 'short', year: 'numeric' }).format(startOfDay(day))
}

/**
 * What the Repeat menu offers for an event starting at `start`.
 *
 * Derived from the start rather than fixed, because the useful choices are
 * about *that* day: an event on Tuesday the 14th is offered "every week on
 * Tuesday", "every month on day 14" and "every month on the second
 * Tuesday" -- the three things a person might mean by "the same time next
 * month", spelled out so they do not have to know there is a difference.
 * "The last Tuesday" appears only when the 14th really is in the last week,
 * and "the fifth" never does: no calendar offers it, because most months do
 * not have one.
 */
export function presetsFor(start: string, tz?: string | null): RepeatPreset[] {
  const day = dayIn(start, tz)
  const weekday = weekdayOf(day)
  const name = weekdayTitle(weekday)
  const { nth, last } = weekOfMonth(day)
  const out: RepeatPreset[] = [
    { key: 'none', label: 'Does not repeat', rule: null },
    { key: 'daily', label: 'Every day', rule: every('daily') },
    {
      key: 'weekdays',
      label: 'Every weekday (Monday to Friday)',
      rule: { ...every('weekly'), weekdays: [...WORKDAYS] },
    },
    {
      key: 'weekly',
      label: `Every week on ${name}`,
      rule: { ...every('weekly'), weekdays: [weekday] },
    },
    { key: 'monthly', label: `Every month on day ${Number(day.slice(8))}`, rule: every('monthly') },
  ]
  if (nth <= 4) {
    out.push({
      key: 'monthlyNth',
      label: `Every month on the ${ordinalWeek(nth)} ${name}`,
      rule: { ...every('monthly'), weekOfMonth: nth, weekdays: [weekday] },
    })
  }
  if (last) {
    out.push({
      key: 'monthlyLast',
      label: `Every month on the last ${name}`,
      rule: { ...every('monthly'), weekOfMonth: -1, weekdays: [weekday] },
    })
  }
  out.push({ key: 'yearly', label: `Every year on ${monthDay(day)}`, rule: every('yearly') })
  return out
}

function sortDays(days: readonly RepeatDay[]): RepeatDay[] {
  return WEEKDAYS.filter((d) => days.includes(d))
}

/**
 * A rule in one spelling, so two that mean the same are equal.
 *
 * The wire allows several: a weekly rule with no days means the start's own
 * day, `interval` may be missing, an `until` beside a `count` is ignored.
 * Comparing raw objects would call "every week" and "every week on Tuesday"
 * different for an event on a Tuesday, and the Repeat menu would show a
 * series somebody made from its own preset as "Custom".
 */
function canonical(rule: Recurrence, startDay: string | null) {
  const monthlyByWeek = rule.frequency === 'monthly' && !!rule.weekOfMonth
  const weekdays =
    rule.frequency === 'weekly'
      ? sortDays(rule.weekdays?.length ? rule.weekdays : startDay ? [weekdayOf(startDay)] : [])
      : monthlyByWeek
        ? sortDays(rule.weekdays ?? [])
        : []
  return {
    frequency: rule.frequency,
    interval: Math.max(1, Math.round(rule.interval || 1)),
    weekdays,
    weekOfMonth: monthlyByWeek ? (rule.weekOfMonth ?? null) : null,
    count: rule.count ?? null,
    until: rule.count ? null : (rule.until ?? null),
  }
}

/** Do two rules mean the same repeat, for an event starting at `start`? */
export function sameRecurrence(
  a: Recurrence | null | undefined,
  b: Recurrence | null | undefined,
  start?: string | null,
  tz?: string | null,
): boolean {
  if (!a || !b) return !a && !b
  const day = start ? dayIn(start, tz) : null
  return JSON.stringify(canonical(a, day)) === JSON.stringify(canonical(b, day))
}

/**
 * Which preset `rule` is for an event starting at `start` -- `none` for no
 * repeat, `custom` for anything the menu has no row for, an ending included.
 */
export function matchPreset(
  rule: Recurrence | null | undefined,
  start: string,
  tz?: string | null,
): PresetKey | 'custom' {
  if (!rule) return 'none'
  for (const preset of presetsFor(start, tz)) {
    if (preset.rule && sameRecurrence(preset.rule, rule, start, tz)) return preset.key
  }
  return 'custom'
}

/**
 * Move a rule to a new start, keeping what it *meant*.
 *
 * "Every week on Tuesday" picked for a Tuesday, then the event moved to
 * Thursday, should become "every week on Thursday" -- the person chose
 * "the same day each week", not "Tuesdays", and the menu said as much. A
 * custom rule is a deliberate choice of days and is left exactly as it was.
 */
export function carryRule(
  rule: Recurrence | null | undefined,
  from: string,
  to: string,
  tz?: string | null,
): Recurrence | null {
  if (!rule) return null
  const key = matchPreset(rule, from, tz)
  if (key === 'custom' || key === 'none') return rule
  const presets = presetsFor(to, tz)
  const same = presets.find((p) => p.key === key)
  // The second Tuesday has no equivalent for a day in the fifth week; the
  // last one is the nearest honest reading, and the date itself the next.
  const near =
    key === 'monthlyNth' || key === 'monthlyLast'
      ? (presets.find((p) => p.key === 'monthlyLast') ?? presets.find((p) => p.key === 'monthly'))
      : undefined
  return (same ?? near)?.rule ?? rule
}

function joinAnd(words: string[]): string {
  if (words.length <= 1) return words.join('')
  return `${words.slice(0, -1).join(', ')} and ${words[words.length - 1]}`
}

/**
 * A rule as a sentence: "Every week on Monday and Wednesday, until 31 Dec
 * 2026".
 *
 * `start` fills in what the rule leaves to it -- the day of the month, the
 * date of the year, the weekday of a weekly rule with none named. Without
 * one, those parts are left out rather than guessed.
 */
export function describeRecurrence(
  rule: Recurrence,
  start?: string | null,
  tz?: string | null,
): string {
  const day = start ? dayIn(start, tz) : null
  const n = Math.max(1, Math.round(rule.interval || 1))
  const unit = UNITS[rule.frequency]
  let out = n === 1 ? `Every ${unit}` : `Every ${n} ${unit}s`

  if (rule.frequency === 'weekly') {
    const days = sortDays(rule.weekdays?.length ? rule.weekdays : day ? [weekdayOf(day)] : [])
    if (n === 1 && days.length === WORKDAYS.length && WORKDAYS.every((d) => days.includes(d))) {
      out = 'Every weekday'
    } else if (days.length > 0) {
      out += ` on ${joinAnd(days.map(weekdayTitle))}`
    }
  } else if (rule.frequency === 'monthly') {
    const weekday = rule.weekdays?.[0]
    if (rule.weekOfMonth && weekday) {
      out += ` on the ${ordinalWeek(rule.weekOfMonth)} ${weekdayTitle(weekday)}`
    } else if (day) {
      out += ` on day ${Number(day.slice(8))}`
    }
  } else if (rule.frequency === 'yearly' && day) {
    out += ` on ${monthDay(day)}`
  }

  if (rule.count) out += `, ${rule.count} ${rule.count === 1 ? 'time' : 'times'}`
  else if (rule.until) out += `, until ${fullDate(rule.until)}`
  return out
}

// ── Expansion ─────────────────────────────────────────────────────────────

function daysBetween(a: string, b: string): number {
  // Rounded, because a change of clocks makes one day in the year 23 hours
  // long and another 25.
  return Math.round((startOfDay(b).getTime() - startOfDay(a).getTime()) / 86_400_000)
}

function mondayOf(day: string): string {
  return addDays(day, -WEEKDAYS.indexOf(weekdayOf(day)))
}

/**
 * The days `rule` falls on, from `first` through `through`, at most `cap` of
 * them -- `first` always among them, as RFC 5545 counts its `DTSTART`.
 *
 * The same reading the core's engine gives each shape: weeks counted from
 * the Monday of the first one, a monthly date that a month lacks (the 31st
 * in April) skipped rather than moved, "the last Friday" being the last.
 */
export function expandDays(rule: Recurrence, first: string, through: string, cap = 750): string[] {
  const n = Math.max(1, Math.round(rule.interval || 1))
  const limit = Math.min(cap, rule.count ?? Infinity)
  const last = rule.until && rule.until < through ? rule.until : through
  const [fy = 1970, fm = 1, fd = 1] = first.split('-').map(Number)
  const weekly = sortDays(rule.weekdays?.length ? rule.weekdays : [weekdayOf(first)])
  const firstMonday = mondayOf(first)

  const falls = (day: string): boolean => {
    const [y = 1970, m = 1, d = 1] = day.split('-').map(Number)
    switch (rule.frequency) {
      case 'daily':
        return daysBetween(first, day) % n === 0
      case 'weekly':
        return (
          weekly.includes(weekdayOf(day)) && (daysBetween(firstMonday, mondayOf(day)) / 7) % n === 0
        )
      case 'monthly': {
        if (((y - fy) * 12 + (m - fm)) % n !== 0) return false
        const weekday = rule.weekdays?.[0]
        if (rule.weekOfMonth && weekday) {
          if (weekdayOf(day) !== weekday) return false
          const at = weekOfMonth(day)
          return rule.weekOfMonth === -1 ? at.last : at.nth === rule.weekOfMonth
        }
        return d === fd
      }
      case 'yearly':
        return (y - fy) % n === 0 && m === fm && d === fd
    }
  }

  const out: string[] = []
  if (first > last || limit <= 0) return out
  out.push(first)
  for (let day = addDays(first, 1); day <= last && out.length < limit; day = addDays(day, 1)) {
    if (falls(day)) out.push(day)
  }
  return out
}
