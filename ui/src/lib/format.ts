// Date and number presentation.
//
// Everything is locale-aware via `Intl`, so a journal reads correctly for
// whoever is keeping it rather than in hard-coded English.
//
// This module *presents* a date. The arithmetic on one lives in `time.ts`
// and is imported from there. That line had been drawn in the comments and
// then crossed: this file grew its own `parseLocalDate`, its own
// `startOfDay` and its own `todayIso`, each a second implementation of
// something `time.ts` already exported under another name. Four spellings
// of "the local calendar day" is four places for the same off-by-a-timezone
// bug to be fixed in three of.

import { isoDate, startOfDay } from './time'

const locale = () => navigator.language || 'en'

export function daysBetween(a: Date, b: Date): number {
  return Math.round(
    (startOfDay(isoDate(b)).getTime() - startOfDay(isoDate(a)).getTime()) / 86_400_000,
  )
}

/** "Today", "Yesterday", "Tuesday", or a date — whichever a reader expects. */
export function friendlyDate(iso: string): string {
  const d = startOfDay(iso)
  const ago = daysBetween(d, new Date())
  if (ago === 0) return 'Today'
  if (ago === 1) return 'Yesterday'
  if (ago === -1) return 'Tomorrow'
  if (ago > 1 && ago < 7) {
    return new Intl.DateTimeFormat(locale(), { weekday: 'long' }).format(d)
  }
  const sameYear = d.getFullYear() === new Date().getFullYear()
  return new Intl.DateTimeFormat(locale(), {
    day: 'numeric',
    month: 'long',
    year: sameYear ? undefined : 'numeric',
  }).format(d)
}

/** The big date shown above an open entry. */
export function longDate(iso: string): string {
  return new Intl.DateTimeFormat(locale(), {
    weekday: 'long',
    day: 'numeric',
    month: 'long',
    year: 'numeric',
  }).format(startOfDay(iso))
}

/** Heading for a group of entries in the list. */
export function groupLabel(iso: string): string {
  const d = startOfDay(iso)
  const ago = daysBetween(d, new Date())
  // An entry can legitimately be dated ahead of today -- back-dating works
  // in both directions -- so guard the lower bound too, or a future date
  // lands in "earlier this week".
  if (ago < 0) return 'Later'
  if (ago === 0) return 'Today'
  if (ago === 1) return 'Yesterday'
  if (ago < 7) return 'Earlier this week'
  const sameYear = d.getFullYear() === new Date().getFullYear()
  return new Intl.DateTimeFormat(locale(), {
    month: 'long',
    year: sameYear ? undefined : 'numeric',
  }).format(d)
}

export function dayNumber(iso: string): string {
  return String(startOfDay(iso).getDate())
}

export function weekdayShort(iso: string): string {
  return new Intl.DateTimeFormat(locale(), { weekday: 'short' })
    .format(startOfDay(iso))
    .replace('.', '')
}

export function relativeTime(isoTimestamp: string): string {
  const then = new Date(isoTimestamp).getTime()
  const secs = Math.round((then - Date.now()) / 1000)
  const rtf = new Intl.RelativeTimeFormat(locale(), { numeric: 'auto' })
  const units: [Intl.RelativeTimeFormatUnit, number][] = [
    ['year', 31_536_000],
    ['month', 2_592_000],
    ['week', 604_800],
    ['day', 86_400],
    ['hour', 3600],
    ['minute', 60],
  ]
  for (const [unit, size] of units) {
    if (Math.abs(secs) >= size) return rtf.format(Math.round(secs / size), unit)
  }
  return rtf.format(Math.round(secs), 'second')
}

export function humanBytes(n: number): string {
  if (n < 1024) return `${n} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let v = n / 1024
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i++
  }
  return `${v.toFixed(1)} ${units[i]}`
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n.toLocaleString(locale())} ${n === 1 ? one : many}`
}

/**
 * A duration in minutes, as people say it: `45m`, `2h`, `1h 30m`.
 *
 * Compact rather than spelled out, because these appear in list rows beside
 * a title and have to stay out of the way of it.
 */
export function formatMinutes(minutes: number): string {
  const n = Math.max(0, Math.round(minutes))
  if (n < 60) return `${n}m`
  const hours = Math.floor(n / 60)
  const rest = n % 60
  return rest === 0 ? `${hours}h` : `${hours}h ${rest}m`
}

/** `HH:MM:SS` from the core as a local-looking clock time. */
export function formatClock(hms: string): string {
  const [h, m] = hms.split(':').map(Number)
  const at = new Date()
  at.setHours(h ?? 0, m ?? 0, 0, 0)
  return new Intl.DateTimeFormat(locale(), { hour: 'numeric', minute: '2-digit' }).format(at)
}

/** The time of day an instant falls on, for a row of time blocks. */
export function formatInstantTime(isoTimestamp: string): string {
  return new Intl.DateTimeFormat(locale(), { hour: 'numeric', minute: '2-digit' }).format(
    new Date(isoTimestamp),
  )
}

/**
 * `YYYY-MM-DDTHH:MM` for an `<input type="datetime-local">`.
 *
 * `toISOString().slice(0, 16)` is the obvious spelling and it is wrong: that
 * is UTC, so the field would open an hour or ten off wherever the user is.
 */
export function toLocalInputValue(at: Date): string {
  const p = (n: number) => String(n).padStart(2, '0')
  return `${isoDate(at)}T${p(at.getHours())}:${p(at.getMinutes())}`
}
