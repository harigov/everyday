// `lib/photos.ts`: the Photos timeline's days and months, how a day is cut
// into rows, which rows are near the screen, and where an arrow key goes.
//
// Dates are built in the local zone (`new Date(y, m, d, h)`) and not written
// out as `Z` strings, because the rules under test are about the local
// calendar day and a hard-coded instant would fall on a different one for
// someone in another zone.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: photos, close } = await load('/src/lib/photos.ts')
const {
  GAP,
  MONTH_H,
  DAY_H,
  ZOOMS,
  zoomTarget,
  dayKey,
  monthKey,
  monthLabel,
  dayLabel,
  gridMetrics,
  layoutTimeline,
  visibleLines,
  rowLine,
  stepIndex,
  scrollFor,
  showWithStacks,
  stackSize,
  stackQuery,
  limitFacets,
} = photos

/** The instant of local `day` (`YYYY-MM-DD`) at `hour`. */
function at(day, hour = 12) {
  const [y, m, d] = day.split('-').map(Number)
  return new Date(y, m - 1, d, hour).toISOString()
}

/** Items for the layout: one per day given, in that order. */
function items(...days) {
  return days.map((day, i) => ({ id: `f${i}`, when: at(day) }))
}

// ── days and months ───────────────────────────────────────────────────

assert.equal(dayKey(at('2026-11-13')), '2026-11-13', 'the local day, whatever the zone')
assert.equal(dayKey(at('2026-11-13', 0)), '2026-11-13', 'just after midnight stays that day')
assert.equal(dayKey(at('2026-11-13', 23)), '2026-11-13', 'just before midnight too')
assert.equal(dayKey('not a date'), '', 'unparseable is "no date"')
assert.equal(monthKey('2026-11-13'), '2026-11')
assert.equal(monthKey(''), '')
assert.ok(monthLabel('2026-11').includes('2026'), 'a month heading carries its year')
assert.equal(monthLabel(''), 'No date')

{
  const now = new Date(2026, 10, 13, 15)
  assert.equal(dayLabel('2026-11-13', now), 'Today')
  assert.equal(dayLabel('2026-11-12', now), 'Yesterday')
  assert.equal(dayLabel('', now), 'No date')
  const older = dayLabel('2026-03-04', now)
  assert.ok(older.includes('4'), 'an older day names its date')
  assert.ok(!older.includes('2026'), 'the year is in the month heading above, not here')
  // The first of a month and a day across a year's end are "yesterday" too.
  assert.equal(dayLabel('2026-10-31', new Date(2026, 10, 1, 9)), 'Yesterday')
  assert.equal(dayLabel('2025-12-31', new Date(2026, 0, 1, 9)), 'Yesterday')
}

assert.equal(zoomTarget('small') < zoomTarget('medium'), true)
assert.equal(zoomTarget('medium') < zoomTarget('large'), true)
assert.equal(zoomTarget('nonsense'), 156, 'an unknown zoom is the middle one')
assert.equal(ZOOMS.length, 3)

// ── the grid's metrics ────────────────────────────────────────────────

{
  const { cols, cell } = gridMetrics(1000, 150)
  assert.equal(cols, 6, 'as many tiles as keep each at least the target wide')
  assert.ok(cell >= 150, 'a tile is never smaller than asked')
  const across = cell * cols + GAP * (cols - 1)
  assert.ok(Math.abs(across - 1000) < 1e-9, 'a row ends flush with the edge')
}
assert.deepEqual(gridMetrics(0, 150), { cols: 1, cell: 0 }, 'no width yet, nothing to lay out')
assert.deepEqual(gridMetrics(100, 0), { cols: 1, cell: 0 })
assert.equal(gridMetrics(90, 150).cols, 1, 'narrower than a tile is still one column')
assert.equal(gridMetrics(90, 150).cell, 90, '...and that tile takes the width')

// ── the layout ────────────────────────────────────────────────────────

{
  const empty = layoutTimeline([], 500, 100)
  assert.equal(empty.height, 0)
  assert.deepEqual(empty.lines, [])
  assert.equal(layoutTimeline(items('2026-11-13'), 0, 100).height, 0, 'no width, no layout')
}

{
  // Three on the 13th, two on the 12th, one on October 31st; width 500 and a
  // target of 100 make four to a row.
  const list = items(
    '2026-11-13',
    '2026-11-13',
    '2026-11-13',
    '2026-11-12',
    '2026-11-12',
    '2026-10-31',
  )
  const layout = layoutTimeline(list, 500, 100)
  assert.equal(layout.cols, 4)
  assert.deepEqual(
    layout.lines.map((l) => l.kind),
    ['month', 'day', 'row', 'day', 'row', 'month', 'day', 'row'],
    'a month heading when the month changes, a day heading when the day does',
  )
  const months = layout.lines.filter((l) => l.kind === 'month')
  assert.deepEqual(
    months.map((m) => m.count),
    [5, 1],
    'a month heading counts the pictures under it',
  )
  const days = layout.lines.filter((l) => l.kind === 'day')
  assert.deepEqual(
    days.map((d) => [d.first, d.end, d.count]),
    [
      [0, 3, 3],
      [3, 5, 2],
      [5, 6, 1],
    ],
  )

  // Lines stack with no gap and no overlap, and add up to the page.
  let y = 0
  for (const line of layout.lines) {
    assert.equal(line.top, y, `${line.key} starts where the line above ends`)
    y += line.height
  }
  assert.equal(layout.height, y)
  assert.equal(layout.lines[0].height, MONTH_H)
  assert.equal(layout.lines[1].height, DAY_H)
  assert.ok(Math.abs(layout.lines[2].height - (layout.cell + GAP)) < 1e-9)

  // Every item is in exactly one row.
  const rows = layout.lines.filter((l) => l.kind === 'row')
  assert.deepEqual(
    rows.map((r) => [r.start, r.end]),
    [
      [0, 3],
      [3, 5],
      [5, 6],
    ],
  )
  assert.deepEqual(
    layout.rows.map((i) => layout.lines[i].kind),
    ['row', 'row', 'row'],
    '`rows` points at the row lines',
  )
  assert.equal(new Set(layout.lines.map((l) => l.key)).size, layout.lines.length, 'keys are unique')
}

{
  // A long day wraps into rows of `cols`.
  const layout = layoutTimeline(items(...Array(9).fill('2026-11-13')), 500, 100)
  const rows = layout.lines.filter((l) => l.kind === 'row')
  assert.deepEqual(
    rows.map((r) => [r.start, r.end]),
    [
      [0, 4],
      [4, 8],
      [8, 9],
    ],
  )
}

{
  // Grouping follows the order given: a day that comes back gets a second
  // heading rather than being gathered up.
  const layout = layoutTimeline(items('2026-11-13', '2026-11-12', '2026-11-13'), 500, 100)
  assert.equal(layout.lines.filter((l) => l.kind === 'day').length, 3)
  assert.equal(layout.lines.filter((l) => l.kind === 'month').length, 1)
}

{
  // Pictures with no date are one group, with their own heading.
  const list = [{ id: 'x', when: 'garbage' }, ...items('2026-11-13')]
  const layout = layoutTimeline(list, 500, 100)
  assert.deepEqual(
    layout.lines.map((l) => l.kind),
    ['month', 'day', 'row', 'month', 'day', 'row'],
  )
  assert.equal(layout.lines[0].label, 'No date')
  assert.equal(layout.lines[1].label, 'No date')
  assert.ok(layout.lines[3].label.includes('2026'))
}

{
  // No headings: one grid.
  const layout = layoutTimeline(items('2026-11-13', '2026-10-01', '2026-01-01'), 500, 100, false)
  assert.deepEqual(
    layout.lines.map((l) => l.kind),
    ['row'],
  )
  assert.equal(layout.lines[0].top, 0)
}

{
  // Keys stay put when a page is appended, so the rows already drawn are kept.
  const first = layoutTimeline(items('2026-11-13', '2026-11-13'), 500, 100)
  const more = layoutTimeline(items('2026-11-13', '2026-11-13', '2026-11-12'), 500, 100)
  for (const line of first.lines) {
    const same = more.lines.find((l) => l.key === line.key)
    assert.ok(same, `${line.key} survives an appended page`)
    assert.equal(same.top, line.top)
  }
}

// ── what is near the screen ───────────────────────────────────────────

{
  // 400 pictures on one day: a heading pair and a hundred rows of four.
  const layout = layoutTimeline(items(...Array(400).fill('2026-11-13')), 500, 100)
  const row = layout.lines.find((l) => l.kind === 'row')
  const view = 600
  const near = visibleLines(layout, 0, view, 0)
  assert.equal(near.from, 0, 'at the top the first line is drawn')
  const last = layout.lines[near.to - 1]
  assert.ok(last.top < view, 'the last line drawn starts on screen')
  assert.ok(layout.lines[near.to].top >= view, 'the next one starts past it')
  assert.ok(near.to < layout.lines.length / 4, 'only a few lines are kept in the DOM')

  const padded = visibleLines(layout, 0, view, 300)
  assert.ok(padded.to >= near.to, 'overscan draws more below')

  const deep = visibleLines(layout, 20 * row.height, view, 0)
  const first = layout.lines[deep.from]
  assert.ok(first.top + first.height > 20 * row.height, 'the first drawn line reaches the pane')
  assert.ok(layout.lines[deep.from - 1].top + layout.lines[deep.from - 1].height <= 20 * row.height)

  const end = visibleLines(layout, layout.height - view, view, 0)
  assert.equal(end.to, layout.lines.length, 'at the bottom the last line is drawn')

  assert.deepEqual(visibleLines(layoutTimeline([], 500, 100), 0, 600), { from: 0, to: 0 })
}

// ── arrow keys ────────────────────────────────────────────────────────

{
  // Day A: nine pictures (rows 0-3, 4-7, 8); day B: two (9-10). Four to a row.
  const layout = layoutTimeline(
    items(...Array(9).fill('2026-11-13'), '2026-11-12', '2026-11-12'),
    500,
    100,
  )
  const count = 11
  const go = (from, key) => stepIndex(layout, count, from, key)
  assert.equal(go(3, 'ArrowRight'), 4, 'right goes on into the next row')
  assert.equal(go(10, 'ArrowRight'), 10, 'and stops at the end')
  assert.equal(go(4, 'ArrowLeft'), 3)
  assert.equal(go(0, 'ArrowLeft'), 0)
  assert.equal(go(5, 'ArrowDown'), 8, 'down keeps the column, short of the row end')
  assert.equal(go(1, 'ArrowDown'), 5)
  assert.equal(go(3, 'ArrowDown'), 7)
  assert.equal(go(7, 'ArrowDown'), 8, 'a short row is landed on its last tile')
  assert.equal(go(8, 'ArrowDown'), 9, 'down crosses a day heading')
  assert.equal(go(9, 'ArrowUp'), 8)
  assert.equal(go(10, 'ArrowUp'), 8, 'up lands on the last tile of a short row')
  assert.equal(go(5, 'ArrowUp'), 1)
  assert.equal(go(2, 'ArrowUp'), 2, 'up from the first row stays')
  assert.equal(go(10, 'ArrowDown'), 10, 'down from the last row stays')
  assert.equal(go(6, 'Home'), 0)
  assert.equal(go(6, 'End'), 10)
  assert.equal(stepIndex(layout, 0, 0, 'ArrowDown'), -1, 'nothing to move over')
  assert.equal(go(99, 'ArrowDown'), 10, 'an index past the end is brought back')

  assert.equal(rowLine(layout, 6).kind, 'row')
  assert.equal(rowLine(layout, 6).start, 4)
  assert.equal(rowLine(layout, 99), null)
}

// ── scrolling a row into view ─────────────────────────────────────────

{
  const line = { top: 500, height: 100 }
  assert.equal(scrollFor(line, 450, 400), 450, 'already on screen: no movement')
  assert.equal(scrollFor(line, 0, 400), 208, 'below: just enough to show it, with the margin')
  assert.equal(scrollFor(line, 900, 400), 492, 'above: its top, with the margin')
  assert.equal(scrollFor({ top: 2, height: 100 }, 0, 400), 0, 'never negative')
}

// ── stacks ─────────────────────────────────────────────────────────────

/** A file with just what these rules read. */
function file(id, extra = {}) {
  return { id, media: { takenAt: at('2026-11-13', 9) }, modifiedAt: at('2026-01-01'), ...extra }
}

{
  const tops = [file('a'), file('b')]
  const edit = file('a1', { stack: 'a', media: { takenAt: at('2026-02-02') } })
  const shown = showWithStacks(tops, { a: [edit, file('a2', { stack: 'a' })] })
  assert.deepEqual(
    shown.map((s) => [s.file.id, s.member]),
    [
      ['a', false],
      ['a1', true],
      ['a2', true],
      ['b', false],
    ],
    'an open stack unfolds right after its top',
  )
  assert.equal(shown[1].when, shown[0].when, 'a member is filed under its top day, not its own')

  assert.equal(showWithStacks(tops, {}).length, 2, 'nothing open, nothing added')
  const dup = showWithStacks(tops, { a: [file('b'), file('a1'), file('a1')] })
  assert.deepEqual(
    dup.map((s) => s.file.id),
    ['a', 'a1', 'b'],
    'a file already listed, or listed twice, is shown once',
  )
}

assert.equal(stackSize(file('a')), 0, 'no counts, no stack')
assert.equal(stackSize(file('a'), { a: 2 }), 2)
assert.equal(stackSize(file('a'), { b: 2 }), 0, 'another file tops that stack')
assert.equal(stackSize(file('a'), { a: -1 }), 0)
assert.equal(stackSize(file('a'), { a: 'x' }), 0)

{
  const q = stackQuery('top', {
    kinds: ['image'],
    tag: 'x',
    stackTopsOnly: true,
    offset: 400,
    includeHidden: true,
  })
  assert.equal(q.stack, 'top')
  assert.equal(q.stackTopsOnly, undefined, 'members are what is asked for')
  assert.equal(q.offset, 0)
  assert.equal(q.kinds, undefined, "a member need not match the view's filters")
  assert.equal(q.tag, undefined)
  assert.equal(q.includeHidden, true, 'hidden members show when the view shows hidden files')
}

// ── facets ────────────────────────────────────────────────────────────

{
  const rows = [
    { name: 'a', count: 5 },
    { name: 'b', count: 4 },
    { name: 'c', count: 3 },
    { name: 'd', count: 2 },
  ]
  assert.deepEqual(
    limitFacets(rows, 2, null, false).map((r) => r.name),
    ['a', 'b'],
  )
  assert.deepEqual(
    limitFacets(rows, 2, 'd', false).map((r) => r.name),
    ['a', 'b', 'd'],
    'the active facet is never cut off',
  )
  assert.equal(limitFacets(rows, 2, null, true).length, 4, 'all of them on request')
  assert.equal(limitFacets(rows, 9, null, false).length, 4)
}

await close()
console.log('photos: all checks passed')
