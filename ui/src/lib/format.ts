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

import { isoDate, locale, startOfDay } from './time'

// ── the formatters ─────────────────────────────────────────────────────
//
// `Intl` objects are *built* rather than looked up: constructing one resolves
// a locale, loads its data and compiles a pattern, and it costs some eighty
// times what formatting a date with the result does. Everything below used to
// construct a fresh one per call, which put that price on every row of every
// list -- and, worse, on the typing path: the notes editor's status bar reads
// `updatedAt`, which moves on every keystroke, so a character typed into a
// note built a relative-time formatter before it was drawn.
//
// So they are made once and kept -- but a formatter is only reusable for as
// long as everything it resolved at construction still holds, and two of those
// things move under a window that stays open for days.
//
// The locale is one: `locale()` reads `navigator.language`, and a machine whose
// language is changed must not go on being formatted in the old one.
//
// The time zone is the other, and it is the one worth spelling out, because a
// formatter does not notice. Built in London and asked about `09:00Z` after the
// laptop has been carried to Tokyo, it still answers 9:00 rather than 18:00 --
// and a journal is exactly the sort of thing that is carried.
//
// `getTimezoneOffset` is asked rather than `resolvedOptions().timeZone`, which
// sounds more correct and is useless here: resolving a zone that way means
// constructing a formatter, which is the thing being avoided. Measured in the
// webview, the offset costs about six tenths of a microsecond against the forty
// that building one of these costs, so the saving survives it nearly whole.
//
// Two consequences of keying on the offset, both acceptable. A daylight-saving
// change rebuilds these, which is a handful of formatters twice a year. And two
// zones on the same offset share an entry -- which would only matter to a format
// that names the zone, and none of these ask for one.

const dateFormats = new Map<string, Intl.DateTimeFormat>()
const relativeFormats = new Map<string, Intl.RelativeTimeFormat>()

/**
 * What a kept formatter is only valid for: this language, in this zone.
 *
 * The options are literals written in this file, so their keys always come out
 * in the same order and the string is stable.
 */
function cacheKey(options: object): string {
  return `${locale()}\u0000${new Date().getTimezoneOffset()}\u0000${JSON.stringify(options)}`
}

/** The `Intl.DateTimeFormat` for these options, made once per locale and zone. */
function dateFormat(options: Intl.DateTimeFormatOptions): Intl.DateTimeFormat {
  const id = cacheKey(options)
  let made = dateFormats.get(id)
  if (!made) {
    made = new Intl.DateTimeFormat(locale(), options)
    dateFormats.set(id, made)
  }
  return made
}

/** The same for `Intl.RelativeTimeFormat`. */
function relativeFormat(options: Intl.RelativeTimeFormatOptions): Intl.RelativeTimeFormat {
  const id = cacheKey(options)
  let made = relativeFormats.get(id)
  if (!made) {
    made = new Intl.RelativeTimeFormat(locale(), options)
    relativeFormats.set(id, made)
  }
  return made
}

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
    return dateFormat({ weekday: 'long' }).format(d)
  }
  const sameYear = d.getFullYear() === new Date().getFullYear()
  return dateFormat({
    day: 'numeric',
    month: 'long',
    year: sameYear ? undefined : 'numeric',
  }).format(d)
}

/** The big date shown above an open entry. */
export function longDate(iso: string): string {
  return dateFormat({
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
  return dateFormat({
    month: 'long',
    year: sameYear ? undefined : 'numeric',
  }).format(d)
}

export function dayNumber(iso: string): string {
  return String(startOfDay(iso).getDate())
}

export function weekdayShort(iso: string): string {
  return dateFormat({ weekday: 'short' }).format(startOfDay(iso)).replace('.', '')
}

export function relativeTime(isoTimestamp: string): string {
  const then = new Date(isoTimestamp).getTime()
  const secs = Math.round((then - Date.now()) / 1000)
  const rtf = relativeFormat({ numeric: 'auto' })
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
  return `${n.toLocaleString(locale())} ${pluralWord(n, one, many)}`
}

/**
 * The noun alone, agreeing with `n`. What `plural` is built from.
 *
 * Worth having separately for the places that have already printed the
 * number, or are about to print it somewhere else in the sentence: "84 of
 * 412 pages" wants the word and not a second count, and reaching for
 * `plural` there produces "84 of 412 412 pages".
 */
export function pluralWord(n: number, one: string, many = `${one}s`): string {
  return n === 1 ? one : many
}

/**
 * "a" or "an", for a noun the interface did not write.
 *
 * Shelf names are typed by the person using the app, so "Add a book" and
 * "Add an album" cannot both be hard-coded. Vowel-initial is the rule that
 * gets it right nearly always; the exceptions ("an hour", "a university")
 * are rarer in this position than the alternative of writing "Add book".
 */
export function article(word: string): string {
  return /^[aeiou]/i.test(word.trim()) ? 'an' : 'a'
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
  return dateFormat({ hour: 'numeric', minute: '2-digit' }).format(at)
}

/** The time of day an instant falls on, for a row of time blocks. */
export function formatInstantTime(isoTimestamp: string): string {
  return dateFormat({ hour: 'numeric', minute: '2-digit' }).format(new Date(isoTimestamp))
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
