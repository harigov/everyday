// Calendar arithmetic.
//
// Kept apart from `format.ts`, which is about *presenting* a date to a
// reader. This is about the maths a grid needs: which days a week contains,
// where an instant sits between two horizontal lines, and how to snap a
// pointer to a quarter hour.
//
// One rule runs through the whole file: **the grid works in the machine's
// local zone, and the vault works in instants.** Every record crossing the
// boundary carries an RFC 3339 timestamp, so `Date` is used only to ask the
// platform where that instant falls on the wall clock in front of the
// person looking at it. That is the same split the core makes between a
// block's `start` and its `localDate`, and it is why a meeting booked in
// Berlin still draws at the right hour when you open the vault in Chennai.

/** Minutes a scheduled thing snaps to when dragged. */
export const SNAP_MINUTES = 15

/** Shortest block the grid will create or resize to. */
export const MIN_BLOCK_MINUTES = 15

export const MINUTES_IN_DAY = 24 * 60

/** `YYYY-MM-DD` for a `Date`, in local time. */
export function isoDate(at: Date): string {
  const p = (n: number) => String(n).padStart(2, '0')
  return `${at.getFullYear()}-${p(at.getMonth() + 1)}-${p(at.getDate())}`
}

/** Midnight at the start of the local day `iso` names. */
export function startOfDay(iso: string): Date {
  const [y, m, d] = iso.split('-').map(Number)
  return new Date(y ?? 1970, (m ?? 1) - 1, d ?? 1, 0, 0, 0, 0)
}

export function addDays(iso: string, days: number): string {
  const at = startOfDay(iso)
  at.setDate(at.getDate() + days)
  return isoDate(at)
}

export function addMonths(iso: string, months: number): string {
  const at = startOfDay(iso)
  // Clamp to the last day of the target month: stepping forward from the
  // 31st must land on the 28th of February, not the 3rd of March.
  const day = at.getDate()
  at.setDate(1)
  at.setMonth(at.getMonth() + months)
  at.setDate(Math.min(day, daysInMonth(at.getFullYear(), at.getMonth())))
  return isoDate(at)
}

function daysInMonth(year: number, monthIndex: number): number {
  return new Date(year, monthIndex + 1, 0).getDate()
}

/**
 * Today, as `YYYY-MM-DD` in local time.
 *
 * Named for what it returns, not just for what it means: nearly every date
 * in this interface travels as a `YYYY-MM-DD` string, and telling those
 * apart from a `Date` at a glance is most of reading this code.
 */
export function todayIso(): string {
  return isoDate(new Date())
}

/**
 * The first day of the week `iso` falls in.
 *
 * `weekStart` is 0 for Sunday and 1 for Monday, taken from the locale rather
 * than hard-coded — a calendar that starts on the wrong day is a calendar
 * that is subtly wrong every single time you look at it.
 */
export function startOfWeek(iso: string, weekStart: number): string {
  const at = startOfDay(iso)
  const shift = (at.getDay() - weekStart + 7) % 7
  at.setDate(at.getDate() - shift)
  return isoDate(at)
}

/** `count` consecutive days beginning at `iso`. */
export function daysFrom(iso: string, count: number): string[] {
  return Array.from({ length: count }, (_, i) => addDays(iso, i))
}

/**
 * Which day the week starts on here.
 *
 * `Intl.Locale.prototype.getWeekInfo` knows, and most engines now implement
 * it; where they do not, Sunday for the handful of locales that use it and
 * Monday for the rest is a better guess than assuming one for everybody.
 */
export function localeWeekStart(): number {
  const locale = navigator.language || 'en'
  try {
    const info = new Intl.Locale(locale) as unknown as {
      getWeekInfo?: () => { firstDay: number }
      weekInfo?: { firstDay: number }
    }
    const first = info.getWeekInfo?.().firstDay ?? info.weekInfo?.firstDay
    // The spec numbers Monday 1 … Sunday 7; `Date#getDay` numbers Sunday 0.
    if (first) return first % 7
  } catch {
    /* fall through to the guess */
  }
  return /^(en-US|en-CA|ja|he|ar|pt-BR|ko)/i.test(locale) ? 0 : 1
}

/** The six-week block a month view draws, beginning on `weekStart`. */
export function monthGrid(iso: string, weekStart: number): string[] {
  const at = startOfDay(iso)
  at.setDate(1)
  const first = startOfWeek(isoDate(at), weekStart)
  // Always six rows. A month view whose height changes as you page through
  // the year makes every other element on screen jump with it.
  return daysFrom(first, 42)
}

/** Minutes from local midnight to `at`. */
export function minutesOfDay(at: Date): number {
  return at.getHours() * 60 + at.getMinutes() + at.getSeconds() / 60
}

/** Minutes from the start of the local day `iso` to the instant `ts`. */
export function offsetInDay(ts: string, iso: string): number {
  return (Date.parse(ts) - startOfDay(iso).getTime()) / 60_000
}

/** An instant `minutes` after the start of the local day `iso`. */
export function instantAt(iso: string, minutes: number): string {
  const at = startOfDay(iso)
  at.setMinutes(at.getMinutes() + Math.round(minutes))
  return at.toISOString()
}

/** Round to the nearest `SNAP_MINUTES`. */
export function snap(minutes: number, step = SNAP_MINUTES): number {
  return Math.round(minutes / step) * step
}

/** Whole minutes between two RFC 3339 instants, never negative. */
export function minutesBetween(start: string, end: string): number {
  return Math.max(0, Math.round((Date.parse(end) - Date.parse(start)) / 60_000))
}

/** Do two half-open intervals share any instant? Touching does not count. */
export function overlaps(aStart: number, aEnd: number, bStart: number, bEnd: number): boolean {
  return aStart < bEnd && bStart < aEnd
}

/**
 * Lay overlapping items out side by side, as every calendar grid does.
 *
 * Items are grouped into *clusters* of things that transitively overlap, and
 * within a cluster each is given the leftmost column that is free. The
 * cluster's column count decides the width, so two clashing meetings each
 * take half the day and a third takes a third — and, crucially, an unrelated
 * meeting later in the day is unaffected, because it is in its own cluster.
 *
 * `start` and `end` are in minutes. Zero-length items are given a nominal
 * length first, or they would never overlap anything and would draw as a
 * hairline underneath their neighbours.
 */
export function packLanes<T extends { start: number; end: number }>(
  items: T[],
): { item: T; lane: number; lanes: number }[] {
  const sorted = [...items].sort((a, b) => a.start - b.start || b.end - a.end)
  const out: { item: T; lane: number; lanes: number }[] = []

  let cluster: { item: T; lane: number }[] = []
  let clusterEnd = -Infinity

  const flush = () => {
    if (cluster.length === 0) return
    const lanes = Math.max(...cluster.map((c) => c.lane)) + 1
    for (const c of cluster) out.push({ ...c, lanes })
    cluster = []
    clusterEnd = -Infinity
  }

  for (const item of sorted) {
    const end = Math.max(item.end, item.start + MIN_BLOCK_MINUTES)
    if (item.start >= clusterEnd) flush()

    // The lowest column not already taken by something this overlaps.
    const taken = new Set(
      cluster
        .filter((c) =>
          overlaps(
            item.start,
            end,
            c.item.start,
            Math.max(c.item.end, c.item.start + MIN_BLOCK_MINUTES),
          ),
        )
        .map((c) => c.lane),
    )
    let lane = 0
    while (taken.has(lane)) lane++

    cluster.push({ item, lane })
    clusterEnd = Math.max(clusterEnd, end)
  }
  flush()
  return out
}
