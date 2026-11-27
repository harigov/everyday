// `lib/library-grid.ts`: the Library's shelf cut into lines of equal height,
// and which of them are near enough the viewport to draw.
//
// The one property everything else leans on is that the padding standing for
// the lines not drawn, and the lines drawn, always add up to the whole
// shelf's height: otherwise the scroll bar would change length as the shelf
// is scrolled and the page would jump under the pointer.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: grid, close } = await load('/src/lib/library-grid.ts')
const { CARD_MIN, frameHeight, gridMetrics, keepLine, lineTop, shelfLines, shelfWindow } = grid

// ── the frame and the columns ────────────────────────────────────────

assert.equal(frameHeight('2 / 3', 146), 219, 'a jacket is half as tall again as it is wide')
assert.equal(frameHeight('4 / 3', 120), 90, 'a place is wider than it is tall')
assert.equal(frameHeight('nonsense', 100), 100, 'a ratio that is not one is a square')

// The grid's own `repeat(auto-fill, minmax(146px, 1fr))`, worked out the
// same way: as many columns as keep a card at least 146 px wide.
{
  const { cols, cell } = gridMetrics(800, CARD_MIN, 16)
  assert.equal(cols, 5, 'five 146 px cards and four 16 px gaps fit in 800 px, six do not')
  assert.equal(cell, (800 - 4 * 16) / 5, 'and they share what is left over')
  assert.equal(gridMetrics(100, CARD_MIN, 16).cols, 1, 'never fewer than one column')
}

// ── lines ────────────────────────────────────────────────────────────

{
  const shelf = shelfLines(545, 6, 300, 20)
  assert.equal(shelf.lines, 91, '545 cards six to a line is 91 lines, the last one short')
  assert.equal(shelf.pitch, 320, 'a line is a card and the gap under it')
  assert.equal(shelf.height, 91 * 320 - 20, 'and there is no gap under the last line')
  assert.deepEqual(shelfLines(0, 6, 300, 20), { cols: 6, pitch: 320, gap: 20, lines: 0, height: 0 })
  assert.equal(shelfLines(10, 0, 300, 20).cols, 1, 'no columns measured yet is one')
  assert.equal(lineTop(shelf, 0), 0)
  assert.equal(lineTop(shelf, 5), 0, 'the sixth card is on the first line')
  assert.equal(lineTop(shelf, 6), 320, 'the seventh starts the second')
  assert.equal(lineTop(shelf, 544), 90 * 320)
}

// ── the window ───────────────────────────────────────────────────────

{
  const count = 545
  const shelf = shelfLines(count, 6, 300, 20)

  // At the top: the screen's worth of lines and as many again below it.
  const top = shelfWindow(shelf, count, 0, 800)
  assert.equal(top.from, 0)
  assert.equal(top.to, 5 * 6, 'lines 0-4 cover the screen and the screen below it')
  assert.equal(top.before, 0)

  // Scrolled down: a screen above and below what shows.
  const middle = shelfWindow(shelf, count, 10_000, 800)
  assert.equal(middle.from, 28 * 6, 'from the line a screen above the top')
  assert.equal(middle.to, 37 * 6, 'to the line a screen below the bottom')
  assert.equal(middle.before, 28 * 320)

  // The last line is short, and is drawn short.
  const end = shelfWindow(shelf, count, shelf.height - 800, 800)
  assert.equal(end.to, count, 'the last card is the last drawn')
  assert.equal(end.after, 0)

  // A shelf further down the pane than the screen reaches: its first lines,
  // ready for the scroll that brings it up.
  const below = shelfWindow(shelf, count, -500, 800)
  assert.equal(below.from, 0)
  assert.equal(below.to, 4 * 6)

  // Scrolled past it altogether: nothing drawn, and still the whole height.
  const past = shelfWindow(shelf, count, 1e6, 800)
  assert.deepEqual([past.from, past.to], [0, 0])
  assert.equal(past.before + past.after, shelf.height)

  assert.deepEqual(shelfWindow(shelfLines(0, 6, 300, 20), 0, 0, 800), {
    from: 0,
    to: 0,
    before: 0,
    after: 0,
  })

  // Wherever it is scrolled to: whole lines from the first, and the padding
  // and the lines drawn are the shelf's whole height, so nothing moves.
  for (const [cols, gap] of [
    [6, 20],
    [1, 0],
  ]) {
    const lines = shelfLines(count, cols, 300, gap)
    for (let at = -1000; at < lines.height + 1000; at += 137) {
      const w = shelfWindow(lines, count, at, 800)
      if (w.to === w.from) continue
      assert.equal(w.from % cols, 0, `the window starts a line (${cols} across, at ${at})`)
      const drawnLines = Math.ceil((w.to - w.from) / cols)
      const drawn = drawnLines * 300 + (drawnLines - 1) * gap
      assert.equal(w.before + drawn + w.after, lines.height, `the height holds at ${at}`)
      assert.ok(w.before <= Math.max(0, at), `what shows is drawn (${at})`)
      assert.ok(
        w.before + drawn >= Math.min(lines.height, at + 800),
        `down to the bottom of the screen (${at})`,
      )
    }
  }
}

// ── the focused card's line, kept drawn ──────────────────────────────
//
// A card with focus that scrolls out of the window would be unmounted, and
// focus would fall to the page's body, out of the shelf's arrow keys. Its
// line stays drawn wherever the window is: the shelf keeps its height, the
// line sits where it belongs, and everything is in shelf order.

{
  const count = 545

  /** How tall what `d` draws is: its lines, the space between two runs,
   *  and the gaps either side of that space. */
  const drawnHeight = (d, lines) => {
    const one = Math.ceil((d.to - d.from) / lines.cols)
    const two = Math.ceil((d.then.to - d.then.from) / lines.cols)
    if (two > 0) return (one + two) * lines.pitch + d.then.skip
    return one > 0 ? one * lines.pitch - lines.gap : 0
  }

  for (const [cols, gap] of [
    [6, 20],
    [1, 0],
  ]) {
    const lines = shelfLines(count, cols, 300, gap)
    for (let at = -1000; at < lines.height + 1000; at += 911) {
      const w = shelfWindow(lines, count, at, 800)
      assert.deepEqual(
        keepLine(w, lines, count, null),
        { ...w, then: { from: 0, to: 0, skip: 0 } },
        'nothing focused is the window as it was',
      )
      for (const keep of [0, 1, cols, 7 * cols + 1, 200, 400, count - 1]) {
        const d = keepLine(w, lines, count, keep)
        const where = `(${cols} across, at ${at}, card ${keep})`
        const inFirst = keep >= d.from && keep < d.to
        const inThen = keep >= d.then.from && keep < d.then.to
        assert.ok(inFirst || inThen, `the focused card is drawn ${where}`)
        assert.equal(d.from % cols, 0, `the first run starts a line ${where}`)
        assert.equal(
          d.before + drawnHeight(d, lines) + d.after,
          lines.height,
          `the height holds ${where}`,
        )
        if (d.then.from < d.then.to) {
          assert.equal(d.then.from % cols, 0, `so does the second ${where}`)
          assert.ok(d.to < d.then.from, `in shelf order, with lines between ${where}`)
          assert.ok(d.then.skip >= 300, `the space between is at least a line ${where}`)
          assert.equal(
            d.before + Math.ceil((d.to - d.from) / cols) * lines.pitch + d.then.skip + gap,
            (d.then.from / cols) * lines.pitch,
            `the second run sits on its own line ${where}`,
          )
        }
        const window = w.to - w.from
        const drawn = d.to - d.from + d.then.to - d.then.from
        assert.ok(drawn <= window + cols, `one line more than the window at most ${where}`)
      }
    }
  }

  // The cases one by one, six across, 320 from line to line.
  const lines = shelfLines(count, 6, 300, 20)
  const middle = shelfWindow(lines, count, 10_000, 800) // lines 28 to 36
  assert.deepEqual(keepLine(middle, lines, count, 30 * 6), {
    ...middle,
    then: { from: 0, to: 0, skip: 0 },
  })
  assert.deepEqual(
    keepLine(middle, lines, count, 27 * 6 + 2),
    {
      ...middle,
      from: 27 * 6,
      before: 27 * 320,
      then: { from: 0, to: 0, skip: 0 },
    },
    'the line just above joins the window',
  )
  assert.deepEqual(
    keepLine(middle, lines, count, 37 * 6),
    {
      ...middle,
      to: 38 * 6,
      after: (91 - 38) * 320,
      then: { from: 0, to: 0, skip: 0 },
    },
    'and so does the line just below',
  )
  assert.deepEqual(
    keepLine(middle, lines, count, 3 * 6 + 4),
    {
      from: 3 * 6,
      to: 4 * 6,
      before: 3 * 320,
      after: middle.after,
      then: { from: middle.from, to: middle.to, skip: (28 - 4) * 320 - 20 },
    },
    'a line far above is drawn first, apart',
  )
  assert.deepEqual(
    keepLine(middle, lines, count, count - 1),
    {
      ...middle,
      after: 0,
      then: { from: 90 * 6, to: count, skip: (90 - 37) * 320 - 20 },
    },
    'the short last line, far below, is drawn last, apart',
  )
  const past = shelfWindow(lines, count, 1e6, 800)
  assert.deepEqual(
    keepLine(past, lines, count, 12),
    { from: 12, to: 18, before: 2 * 320, after: (91 - 3) * 320, then: { from: 0, to: 0, skip: 0 } },
    'scrolled out of sight, the focused line is all that is drawn',
  )
  assert.deepEqual(
    keepLine(middle, lines, count, count + 3),
    { ...middle, then: { from: 0, to: 0, skip: 0 } },
    'a card no longer on the shelf keeps nothing',
  )
}

await close()
console.log('library-grid: all checks passed')
