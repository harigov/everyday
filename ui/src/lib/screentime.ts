// Pure helpers for Screen time: ranges, durations, colours, labels.
//
// No store, no `api` -- tested by `scripts/screentime.test.mjs`. The names
// below are the contract `screentime.svelte.ts` and `components/ScreenTime*`
// are written against.
//
// Days travel as `YYYY-MM-DD` in the reader's local zone, the way every
// other app's do, and the arithmetic on them is `time.ts`'s; what a reader
// sees is built by `format.ts`'s cached `Intl` formatters, so a range reads
// "3–9 Nov" in London and "Nov 3 – 9" in New York without a word of English
// hard-coded here.

import { DEFAULT_COLORS } from './colors'
import { dateFormat, monthYear, timeOfDay } from './format'
import { addDays, addMonths, isoDate, startOfDay, startOfWeek } from './time'
import type { ScreenAppRow, ScreenCategory, ScreenItem, SpanKind } from './types'

/** How long a summary covers. */
export type RangeKind = 'day' | 'week' | 'month'

/** Is this a `YYYY-MM-DD` that names a real day? */
function isDay(value: string): boolean {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) return false
  return isoDate(startOfDay(value)) === value
}

/** The first and last day (`YYYY-MM-DD`, inclusive) of the range of `kind`
 *  that holds `anchor`. Weeks start on `weekStart` (0 = Sunday). */
export function rangeOf(
  kind: RangeKind,
  anchor: string,
  weekStart = 1,
): { from: string; to: string } {
  if (kind === 'week') {
    const from = startOfWeek(anchor, ((weekStart % 7) + 7) % 7)
    return { from, to: addDays(from, 6) }
  }
  if (kind === 'month') {
    const from = `${anchor.slice(0, 7)}-01`
    // The day before the first of next month: `addMonths` from the first
    // never has a 31st to clamp.
    return { from, to: addDays(addMonths(from, 1), -1) }
  }
  return { from: anchor, to: anchor }
}

/** `anchor` moved by `delta` ranges of `kind`. */
export function stepAnchor(kind: RangeKind, anchor: string, delta: number): string {
  if (kind === 'week') return addDays(anchor, 7 * delta)
  // Clamped by `addMonths`: a step forward from 31 January lands on the last
  // of February, still inside the month it means.
  if (kind === 'month') return addMonths(anchor, delta)
  return addDays(anchor, delta)
}

/**
 * "Today", "Yesterday", "Mon 3 Nov", "3–9 Nov" / "28 Oct – 3 Nov",
 * "November 2026" -- in the reader's own order and words. The year is added
 * to a day or a week only when it is not `today`'s.
 */
export function rangeLabel(kind: RangeKind, from: string, to: string, today: string): string {
  if (!isDay(from) || !isDay(to)) return ''
  if (kind === 'month') return monthYear(startOfDay(from))
  const year = (d: string) => d.slice(0, 4)
  const otherYear = year(from) !== year(today) || year(to) !== year(today)
  if (kind === 'day') {
    if (from === today) return 'Today'
    if (from === addDays(today, -1)) return 'Yesterday'
    return dateFormat({
      weekday: 'short',
      day: 'numeric',
      month: 'short',
      year: otherYear ? 'numeric' : undefined,
    }).format(startOfDay(from))
  }
  const format = dateFormat({
    day: 'numeric',
    month: 'short',
    year: otherYear ? 'numeric' : undefined,
  })
  // `formatRange` knows which parts two dates share in this locale -- the
  // month once in "3–9 Nov", twice in "28 Oct – 3 Nov". ES2021, so every
  // engine the app runs in (WebKitGTK, WebView2, WKWebView) has it.
  return format.formatRange(startOfDay(from), startOfDay(to))
}

/**
 * "2 h 05 min", "3 h", "45 min", "under a minute".
 *
 * Nothing at all is "0 min", not "under a minute": an empty day is empty,
 * and saying otherwise would claim some time was recorded. Minutes are
 * rounded to the nearest, so 59 min 40 s is "1 h", never "60 min".
 */
export function formatDuration(secs: number): string {
  const s = Number.isFinite(secs) ? Math.max(0, secs) : 0
  if (s === 0) return '0 min'
  const minutes = Math.round(s / 60)
  if (minutes === 0) return 'under a minute'
  if (minutes < 60) return `${minutes} min`
  const hours = Math.floor(minutes / 60)
  const rest = minutes % 60
  return rest === 0 ? `${hours} h` : `${hours} h ${String(rest).padStart(2, '0')} min`
}

/** "2:05" for a chart axis or a tight cell; "0:45" under an hour. */
export function shortDuration(secs: number): string {
  const s = Number.isFinite(secs) ? Math.max(0, secs) : 0
  const minutes = Math.round(s / 60)
  return `${Math.floor(minutes / 60)}:${String(minutes % 60).padStart(2, '0')}`
}

/** What the range before one of each kind is called. */
const PREVIOUS: Record<RangeKind, string> = {
  day: 'the day before',
  week: 'the week before',
  month: 'the month before',
}

/**
 * "+12% on the week before", "−8% on the day before", "About the same as
 * the month before" -- or empty when there is nothing to compare with
 * (nothing recorded in the range before). "The … before" rather than "last
 * week", because the range on screen need not be the current one.
 */
export function comparison(current: number, previous: number, kind: RangeKind): string {
  if (!Number.isFinite(current) || !Number.isFinite(previous) || previous <= 0) return ''
  const pct = Math.round(((Math.max(0, current) - previous) / previous) * 100)
  if (pct === 0) return `About the same as ${PREVIOUS[kind]}`
  // A real minus sign, which is as wide as the plus beside it.
  const sign = pct > 0 ? '+' : '−'
  return `${sign}${Math.abs(pct)}% on ${PREVIOUS[kind]}`
}

/** The text an item key shows when nothing better is known
 *  (`site:youtube.com` → `youtube.com`). */
export function keyText(key: string): string {
  const i = key.indexOf(':')
  return i < 0 ? key : key.slice(i + 1)
}

/** The kind an item key names. */
export function keyKind(key: string): SpanKind {
  return key.startsWith('site:') ? 'site' : 'app'
}

/** What a row of the apps list is called: the label's name, else the
 *  collector's. */
export function rowName(row: ScreenAppRow): string {
  return row.label?.name || row.defaultName
}

/** What a report item is called, with "(other)" for a browser's own time. */
export function itemName(item: ScreenItem): string {
  return item.browserRest ? `${item.name} (other)` : item.name
}

/** The colours a new category is offered, in order: the journal palette,
 *  chosen to stay legible on both themes. */
export const CATEGORY_COLORS: readonly string[] = DEFAULT_COLORS

/** What uncategorised time is drawn in: a text token, not a hue, so it reads
 *  as "nothing chosen" beside the categories' own colours. */
export const NEUTRAL_COLOR = 'var(--fg-faint)'

/** The colour a category is drawn in: its own, else a stable one from the
 *  palette by its place in the order, else the neutral colour for
 *  uncategorised. */
export function categoryColor(
  category: ScreenCategory | undefined,
  categories: ScreenCategory[],
): string {
  if (!category) return NEUTRAL_COLOR
  if (category.color) return category.color
  // By place in the sorted order, not in whatever order the array arrived:
  // the same list always gives the same colours.
  const sorted = [...categories].sort(
    (a, b) => a.sortOrder - b.sortOrder || a.name.localeCompare(b.name) || a.id.localeCompare(b.id),
  )
  const at = sorted.findIndex((c) => c.id === category.id)
  const index = at < 0 ? sorted.length : at
  return CATEGORY_COLORS[index % CATEGORY_COLORS.length] ?? NEUTRAL_COLOR
}

/** `HH:MM` in the reader's locale for an ISO instant. */
export function clock(iso: string): string {
  const at = new Date(iso)
  return Number.isNaN(at.getTime()) ? '' : timeOfDay(at)
}

/** The reader's IANA time zone. */
export function localZone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone
}

/** What a connection code starts with. */
export const CONNECTION_PREFIX = 'karmayoga-screentime:'

/** Base64url (no padding) of a string's UTF-8 -- how a connection code
 *  carries its JSON. The mock makes codes with it. */
export function base64Url(text: string): string {
  let binary = ''
  for (const byte of new TextEncoder().encode(text)) binary += String.fromCharCode(byte)
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

function fromBase64Url(text: string): string {
  const b64 = text.replace(/-/g, '+').replace(/_/g, '/')
  const binary = atob(b64 + '='.repeat((4 - (b64.length % 4)) % 4))
  const bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0))
  return new TextDecoder('utf-8', { fatal: true }).decode(bytes)
}

/** A browser extension's connection code, taken apart -- for showing what a
 *  pasted code points at. `null` when it is not one. */
export function parseConnectionCode(
  code: string,
): { endpoint: string; token: string; device: string } | null {
  const text = code.trim()
  if (!text.startsWith(CONNECTION_PREFIX)) return null
  const body = text.slice(CONNECTION_PREFIX.length)
  if (!/^[A-Za-z0-9_-]+$/.test(body)) return null
  try {
    const parsed: unknown = JSON.parse(fromBase64Url(body))
    if (!parsed || typeof parsed !== 'object') return null
    const { endpoint, token, device } = parsed as Record<string, unknown>
    if (typeof endpoint !== 'string' || !/^https?:\/\//.test(endpoint)) return null
    if (typeof token !== 'string' || !token) return null
    return { endpoint, token, device: typeof device === 'string' ? device : '' }
  } catch {
    return null
  }
}
