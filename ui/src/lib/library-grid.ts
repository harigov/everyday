// The Library's shelf, drawn a window at a time.
//
// A shelf of five hundred covers was five hundred cards in the DOM, all
// mounted on every visit to the app. Every card on a shelf is one height --
// the cover's frame is one ratio for the whole grid (`coverRatio`), and the
// text under it is held to two lines of title and a byline -- and every row
// of the list is one height too, so the height of each line, and of the whole
// shelf, follows from the width, and only the lines near the viewport need to
// be drawn. The lines not drawn are stood in for by the shelf's padding.
//
// Pure -- no store, no DOM -- so `scripts/library-grid.test.mjs` can check it;
// `LibraryView.svelte` measures and draws. The columns are `photos.ts`'s
// `gridMetrics`, which works them out as the grid's own `auto-fill` did.

export { gridMetrics } from './photos'

/** How wide a card is asked to be at least, as the grid's `minmax` had it. */
export const CARD_MIN = 146
/** The same on a phone: two across a phone held upright. */
export const CARD_MIN_PHONE = 140

/**
 * About how tall a card is under its cover: two lines of title, a byline and
 * the row of stars. Only until a card has been drawn and measured, which is
 * what the window is then cut by.
 */
export const CARD_TEXT_H = 92
/** About how tall a row of the list is, before one has been measured. */
export const ROW_H = 56
export const ROW_H_PHONE = 60

/** A cover frame's height for its width, from a ratio as CSS writes one:
 *  `2 / 3` is two wide for every three tall. */
export function frameHeight(ratio: string, width: number): number {
  const [w, h] = ratio.split('/').map((part) => Number(part.trim()))
  return w && h ? (width * h) / w : width
}

/** The shelf as lines of equal height. */
export interface Lines {
  /** Cards to a line: one, for the list. */
  cols: number
  /** From one line's top to the next one's: a card and the gap under it. */
  pitch: number
  /** The gap between one line and the next. */
  gap: number
  /** How many lines the shelf makes. */
  lines: number
  /** The whole shelf's height: every line, and the gaps between them. */
  height: number
}

/** `count` items, `cols` to a line, each line `itemHeight` tall with `gap`
 *  between lines. */
export function shelfLines(count: number, cols: number, itemHeight: number, gap: number): Lines {
  const across = Math.max(1, Math.floor(cols) || 1)
  const lines = count > 0 ? Math.ceil(count / across) : 0
  const between = Math.max(0, gap)
  const pitch = Math.max(0, itemHeight) + between
  return {
    cols: across,
    pitch,
    gap: between,
    lines,
    height: lines > 0 ? lines * pitch - between : 0,
  }
}

/** What to draw: items `[from, to)`, whole lines, and the padding that stands
 *  for the lines above and below them. */
export interface ShelfWindow {
  from: number
  to: number
  before: number
  after: number
}

const NOTHING: ShelfWindow = { from: 0, to: 0, before: 0, after: 0 }

/**
 * The lines to draw for a viewport `height` tall whose top is `top` below the
 * shelf's own top -- negative while the shelf starts further down the page --
 * and `overscan` more either side, so a scroll finds the next lines drawn.
 *
 * `before` and `after` add up, with the lines drawn, to the shelf's height,
 * so the scrolling pane is as tall as the whole shelf whichever lines are in
 * it.
 */
export function shelfWindow(
  shelf: Lines,
  count: number,
  top: number,
  height: number,
  overscan = height,
): ShelfWindow {
  if (shelf.lines === 0 || !(shelf.pitch > 0) || count === 0) return NOTHING
  const clamp = (line: number) => Math.min(shelf.lines, Math.max(0, line))
  const first = clamp(Math.floor((top - overscan) / shelf.pitch))
  const last = Math.max(first, clamp(Math.ceil((top + height + overscan) / shelf.pitch)))
  if (last === first) return { from: 0, to: 0, before: shelf.height, after: 0 }
  return {
    from: first * shelf.cols,
    to: Math.min(count, last * shelf.cols),
    before: first * shelf.pitch,
    after: (shelf.lines - last) * shelf.pitch,
  }
}

/** The window, and the line of one card more when it is outside it. */
export interface DrawnShelf extends ShelfWindow {
  /** Items drawn after the first run, `[from, to)`, with `skip` standing
   *  for the lines between the two; all nought when there are none. */
  then: { from: number; to: number; skip: number }
}

/**
 * The window `win`, with item `keep`'s line drawn too when it is outside it.
 *
 * `keep` is the card that has focus. Scrolled out of the window it would be
 * unmounted, and focus would go with it to the page's body: out of reach of
 * the shelf's arrow keys, and Tab would start again from the top of the
 * window. So its line stays drawn wherever it is -- joined to the window
 * when it is the line next to it, or apart from it with a space standing
 * for the lines between. Always in shelf order, so the arrow keys and Tab go
 * through the cards in the order they are on the shelf.
 */
export function keepLine(
  win: ShelfWindow,
  shelf: Lines,
  count: number,
  keep: number | null,
): DrawnShelf {
  const none = { from: 0, to: 0, skip: 0 }
  if (keep === null || keep < 0 || keep >= count || !(shelf.pitch > 0)) {
    return { ...win, then: none }
  }
  if (win.from < win.to && keep >= win.from && keep < win.to) return { ...win, then: none }
  const line = Math.floor(keep / shelf.cols)
  const from = line * shelf.cols
  const to = Math.min(count, from + shelf.cols)
  const above = line * shelf.pitch
  const below = (shelf.lines - line - 1) * shelf.pitch
  // Nothing else is drawn: the shelf is scrolled out of sight.
  if (win.from === win.to) return { from, to, before: above, after: below, then: none }
  const first = Math.floor(win.from / shelf.cols)
  const last = Math.ceil(win.to / shelf.cols)
  if (line === first - 1) return { ...win, from, before: above, then: none }
  if (line === last) return { ...win, to, after: below, then: none }
  if (line < first) {
    const skip = (first - line - 1) * shelf.pitch - shelf.gap
    return { from, to, before: above, after: win.after, then: { from: win.from, to: win.to, skip } }
  }
  const skip = (line - last) * shelf.pitch - shelf.gap
  return { ...win, after: below, then: { from, to, skip } }
}

/** Where item `index`'s line starts, below the shelf's own top. */
export function lineTop(shelf: Lines, index: number): number {
  return Math.floor(Math.max(0, index) / shelf.cols) * shelf.pitch
}
