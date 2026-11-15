// The Photos view's maths: which day a picture belongs under, how a day's
// pictures are cut into rows, which of those rows are near the screen, and
// where an arrow key goes. Pure -- no store, no DOM -- so `scripts/photos.test.mjs`
// can check it, and so `PhotosTimeline.svelte` is left with drawing.
//
// # The grid is windowed by arithmetic, not measurement
//
// Every tile is a square of one size, and a heading's height is a constant,
// so the height of every line of the timeline -- and so of the whole page --
// is known before anything is drawn. That is what lets the timeline draw only
// the lines near the viewport (a library of fifty thousand pictures keeps a
// few dozen tiles in the DOM) without measuring a single one, and lets it
// give the scrolling pane a definite height. WebKitGTK scrolls a pane like
// that far better than one whose height is whatever its tiles add up to.
//
// # Days are the viewer's days
//
// A picture is filed under the local calendar day of the instant it was taken
// (`fileWhen` in `files.ts`), in the zone the machine is in now -- the rule
// every other list here follows (`time.ts`). The listing is ordered by that
// instant, so the headings have to follow it: a picture taken just after
// midnight on the far side of the world can land under the day before, which
// the camera's own wall-clock time (`takenLocal`) would not say, but filing by
// that would put a day's heading in two places.

import { dayHeading, monthYear } from './format'
import { isoDate, startOfDay } from './time'
import type { FileEntry, FileId, FileQuery } from './types'

// ---- Sizes (CSS pixels) --------------------------------------------------

/** Space between tiles. */
export const GAP = 3
/** Space between the pane's edge and the first tile. */
export const PAD = 16
/** A month's heading, with the room above it. */
export const MONTH_H = 58
/** A day's heading, with the room above it. */
export const DAY_H = 38

/** How big the tiles are asked to be; `gridMetrics` rounds it to fit a row. */
export const ZOOMS = [
  { id: 'small', label: 'Small', target: 108 },
  { id: 'medium', label: 'Medium', target: 156 },
  { id: 'large', label: 'Large', target: 228 },
] as const
export type Zoom = (typeof ZOOMS)[number]['id']

/** What was chosen this session, kept across views and mounts so leaving
 *  Photos and coming back finds the tiles as large as they were left. */
export const remembered: { zoom: Zoom; facets: boolean } = { zoom: 'medium', facets: true }

/** The tile size a zoom asks for. */
export function zoomTarget(zoom: Zoom): number {
  return ZOOMS.find((z) => z.id === zoom)?.target ?? 156
}

// ---- Days and months -----------------------------------------------------

/** `YYYY-MM-DD` in the local zone for an instant, or `''` for one that does
 *  not parse -- those are filed under "No date". */
export function dayKey(iso: string): string {
  const at = new Date(iso)
  return Number.isNaN(at.getTime()) ? '' : isoDate(at)
}

/** `YYYY-MM` of a day key; `''` for "No date". */
export function monthKey(day: string): string {
  return day ? day.slice(0, 7) : ''
}

/** "November 2026". */
export function monthLabel(month: string): string {
  if (!month) return 'No date'
  return monthYear(startOfDay(`${month}-01`))
}

/** "Today", "Yesterday", or "Tuesday 4 March": the month is already in the
 *  heading above, and the year with it. */
export function dayLabel(day: string, now = new Date()): string {
  if (!day) return 'No date'
  const today = isoDate(now)
  if (day === today) return 'Today'
  const yesterday = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1)
  if (day === isoDate(yesterday)) return 'Yesterday'
  return dayHeading(startOfDay(day))
}

// ---- The timeline's layout -----------------------------------------------

/** What the layout needs of a file: who it is, and the instant to file it by. */
export interface TimelineItem {
  id: string
  when: string
}

export type Line =
  | { kind: 'month'; key: string; top: number; height: number; label: string; count: number }
  | {
      kind: 'day'
      key: string
      top: number
      height: number
      label: string
      count: number
      /** Index of the day's first item. */
      first: number
      /** One past its last. */
      end: number
    }
  | {
      kind: 'row'
      key: string
      top: number
      height: number
      /** Index of the row's first item, and one past its last. */
      start: number
      end: number
    }

export interface Layout {
  lines: Line[]
  /** The whole page's height. */
  height: number
  /** Tiles to a row, and a tile's edge. */
  cols: number
  cell: number
  /** The line index of every `row`, ascending: what an item is found in. */
  rows: number[]
}

const EMPTY: Layout = { lines: [], height: 0, cols: 1, cell: 0, rows: [] }

/**
 * How many tiles fit across `width`, and how wide each then is.
 *
 * As many as keep a tile at least `target` wide, so a wider window shows more
 * tiles rather than bigger ones; the tile then grows to take the slack, so a
 * row ends flush with the pane's edge.
 */
export function gridMetrics(
  width: number,
  target: number,
  gap = GAP,
): { cols: number; cell: number } {
  if (!(width > 0) || !(target > 0)) return { cols: 1, cell: 0 }
  const cols = Math.max(1, Math.floor((width + gap) / (target + gap)))
  return { cols, cell: (width - gap * (cols - 1)) / cols }
}

/**
 * Cut `items` -- already in the order to show them -- into month headings,
 * day headings and rows of square tiles, with every line's offset.
 *
 * Items are grouped as they come: a heading is drawn when the day changes
 * from the one before, so a list sorted some other way shows a heading
 * wherever the day changes rather than gathering days that are not together.
 * `headings: false` gives one undivided grid, for a list that is not in date
 * order at all (sorted by name or size).
 *
 * Keys are the item's index, so lines already drawn keep theirs when a page
 * is appended to the end.
 */
export function layoutTimeline(
  items: readonly TimelineItem[],
  width: number,
  target: number,
  headings = true,
): Layout {
  if (items.length === 0 || !(width > 0)) return EMPTY
  const { cols, cell } = gridMetrics(width, target)

  // Runs of one day, in the order they come.
  const groups: { day: string; start: number; end: number }[] = []
  if (headings) {
    for (let i = 0; i < items.length; i++) {
      const day = dayKey(items[i]!.when)
      const last = groups[groups.length - 1]
      if (last && last.day === day) last.end = i + 1
      else groups.push({ day, start: i, end: i + 1 })
    }
  } else {
    groups.push({ day: '', start: 0, end: items.length })
  }

  const lines: Line[] = []
  const rows: number[] = []
  let y = 0
  let month: string | null = null
  let monthLine: (Line & { kind: 'month' }) | null = null
  for (const group of groups) {
    const count = group.end - group.start
    if (headings) {
      const m = monthKey(group.day)
      if (m !== month || !monthLine) {
        month = m
        monthLine = {
          kind: 'month',
          key: `m${group.start}`,
          top: y,
          height: MONTH_H,
          label: monthLabel(m),
          count: 0,
        }
        lines.push(monthLine)
        y += MONTH_H
      }
      monthLine.count += count
      lines.push({
        kind: 'day',
        key: `d${group.start}`,
        top: y,
        height: DAY_H,
        label: dayLabel(group.day),
        count,
        first: group.start,
        end: group.end,
      })
      y += DAY_H
    }
    for (let start = group.start; start < group.end; start += cols) {
      const end = Math.min(start + cols, group.end)
      rows.push(lines.length)
      lines.push({ kind: 'row', key: `r${start}`, top: y, height: cell + GAP, start, end })
      y += cell + GAP
    }
  }
  return { lines, height: y, cols, cell, rows }
}

/**
 * The lines to draw for a pane scrolled to `top` and `height` tall: those
 * that touch it, and `overscan` pixels either side so a fast scroll finds
 * tiles already there. `[from, to)` into `layout.lines`.
 */
export function visibleLines(
  layout: Layout,
  top: number,
  height: number,
  overscan = height,
): { from: number; to: number } {
  const lines = layout.lines
  if (lines.length === 0) return { from: 0, to: 0 }
  const lo = top - overscan
  const hi = top + height + overscan
  // First line whose bottom is below `lo`.
  let a = 0
  let b = lines.length
  while (a < b) {
    const mid = (a + b) >> 1
    const line = lines[mid]!
    if (line.top + line.height <= lo) a = mid + 1
    else b = mid
  }
  const from = a
  let to = from
  while (to < lines.length && lines[to]!.top < hi) to++
  return { from, to }
}

/** The position in `layout.rows` of the row holding item `index`, or -1. */
function rowAt(layout: Layout, index: number): number {
  let a = 0
  let b = layout.rows.length - 1
  while (a <= b) {
    const mid = (a + b) >> 1
    const line = layout.lines[layout.rows[mid]!]
    if (!line || line.kind !== 'row') return -1
    if (index < line.start) b = mid - 1
    else if (index >= line.end) a = mid + 1
    else return mid
  }
  return -1
}

/** The row line holding item `index`, for scrolling it into view. */
export function rowLine(layout: Layout, index: number): Line | null {
  const at = rowAt(layout, index)
  return at < 0 ? null : (layout.lines[layout.rows[at]!] ?? null)
}

export type MoveKey = 'ArrowLeft' | 'ArrowRight' | 'ArrowUp' | 'ArrowDown' | 'Home' | 'End'

/**
 * Where an arrow key moves the cursor: one tile along, or the tile in the
 * same column of the row above or below (across a day's heading too; short of
 * the row's end it lands on the last tile). Stays put at an edge.
 */
export function stepIndex(layout: Layout, count: number, index: number, key: MoveKey): number {
  if (count === 0) return -1
  const last = count - 1
  switch (key) {
    case 'Home':
      return 0
    case 'End':
      return last
    case 'ArrowLeft':
      return Math.max(0, index - 1)
    case 'ArrowRight':
      return Math.min(last, index + 1)
  }
  const at = rowAt(layout, index)
  if (at < 0) return Math.min(Math.max(index, 0), last)
  const here = layout.lines[layout.rows[at]!]
  const other = layout.lines[layout.rows[at + (key === 'ArrowUp' ? -1 : 1)] ?? -1]
  if (!here || here.kind !== 'row' || !other || other.kind !== 'row') return index
  return Math.min(other.start + (index - here.start), other.end - 1)
}

/** The scroll offset that brings `line` fully into a pane `viewport` tall,
 *  moving as little as it can; `edge` is the margin kept beside it. */
export function scrollFor(
  line: { top: number; height: number },
  scrollTop: number,
  viewport: number,
  edge = 8,
): number {
  if (line.top - edge < scrollTop) return Math.max(0, line.top - edge)
  const bottom = line.top + line.height + edge
  if (bottom > scrollTop + viewport) return Math.max(0, bottom - viewport)
  return scrollTop
}

// ---- Stacks ----------------------------------------------------------------

/** When a picture was taken, else modified: what `fileWhen` (`files.ts`) says.
 *  Said again here rather than imported, because that module reaches for the
 *  Files store and this one is kept free of every store. */
function whenOf(file: Pick<FileEntry, 'media' | 'modifiedAt'>): string {
  return file.media.takenAt ?? file.modifiedAt
}

/** A file to draw: for a stack that is open, its members follow its top. */
export interface Shown {
  file: FileEntry
  /** Under a stack's top, shown because the stack is open. */
  member: boolean
  /** What to file it by: a member goes under its top's day, so an open stack
   *  never splits across two headings. */
  when: string
}

/**
 * The listing with the open stacks unfolded: each top, then its members.
 * `members` is what has been fetched for each open top.
 */
export function showWithStacks(
  tops: readonly FileEntry[],
  members: Readonly<Record<FileId, readonly FileEntry[] | undefined>>,
): Shown[] {
  const listed = new Set(tops.map((f) => f.id))
  const out: Shown[] = []
  for (const top of tops) {
    const when = whenOf(top)
    out.push({ file: top, member: false, when })
    for (const member of members[top.id] ?? []) {
      if (listed.has(member.id)) continue
      listed.add(member.id)
      out.push({ file: member, member: true, when })
    }
  }
  return out
}

/**
 * How many other files are under this one, when it is the top of a stack.
 *
 * Read off the listing's `FilePage.stackCounts`, which the vault fills for
 * every listed top; zero for anything that is not one, and for a listing
 * that does not say.
 */
export function stackSize(file: FileEntry, counts?: Record<FileId, number> | null): number {
  const n = counts?.[file.id]
  return typeof n === 'number' && n > 0 ? Math.floor(n) : 0
}

/** The query for a stack: its top and every file under it, hidden ones too
 *  when the view shows them. The view's other filters are left out -- a RAW
 *  file under a tagged JPEG is in its stack whether or not it carries the
 *  tag. */
export function stackQuery(top: FileId, base: FileQuery): FileQuery {
  return {
    stack: top,
    includeHidden: base.includeHidden ?? false,
    sort: 'when',
    descending: false,
    offset: 0,
    limit: 200,
  }
}

// ---- Facets ----------------------------------------------------------------

/** A facet sidebar's rows: all of them, or the first `limit` with the active
 *  one never cut off. */
export function limitFacets<T extends { name: string }>(
  rows: readonly T[],
  limit: number,
  active: string | null,
  all: boolean,
): T[] {
  if (all || rows.length <= limit) return [...rows]
  const head = rows.slice(0, limit)
  if (active && !head.some((r) => r.name === active)) {
    const found = rows.find((r) => r.name === active)
    if (found) head.push(found)
  }
  return head
}
