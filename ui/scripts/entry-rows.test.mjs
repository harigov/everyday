// Behaviour checks for flattening the journal's grouped entry list into rows
// `VirtualList` can draw -- see `entry-rows.ts`.
//
// Dates are built relative to "today" (`todayIso`/`addDays`) rather than
// hard-coded, the same reason `format.test.mjs` avoids doing so: `groupLabel`
// answers "Today", "Yesterday" and "Earlier this week" from the clock the
// suite happens to run under, so a fixed date would pass or fail depending on
// when it runs rather than on what the code does.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

// One server for both modules: a second `load` was a second Vite server that
// nothing closed, and when it was the one holding the HMR port the test
// printed its pass and then never exited.
const {
  modules: [entryRows, timeModule],
  close,
} = await load(['/src/lib/entry-rows.ts', '/src/lib/time.ts'])
const { toRows, pieces } = entryRows
const { todayIso, addDays } = timeModule

function entry(id, daysAgo) {
  return {
    id,
    journalId: 'j1',
    title: `Entry ${id}`,
    excerpt: '',
    localDate: addDays(todayIso(), -daysAgo),
    createdAt: '2026-01-01T00:00:00.000Z',
    updatedAt: '2026-01-01T00:00:00.000Z',
    tags: [],
    starred: false,
    wordCount: 0,
    attachmentCount: 0,
  }
}

// ── An empty list flattens to nothing ──────────────────────────────────

assert.deepEqual(toRows([]), [])

// ── One header per run of the same label, not per entry ──────────────────

const today1 = entry('a', 0)
const today2 = entry('b', 0)
const yesterday = entry('c', 1)

const rows = toRows([today1, today2, yesterday])
assert.deepEqual(
  rows.map((r) => r.kind),
  ['header', 'entry', 'entry', 'header', 'entry'],
  'one heading per run, not one per entry',
)
assert.equal(rows[0].label, 'Today')
assert.equal(rows[1].entry.id, 'a')
assert.equal(rows[2].entry.id, 'b')
assert.equal(rows[3].label, 'Yesterday')
assert.equal(rows[4].entry.id, 'c')

// ── Every row key is unique, even across two runs of the same label ──────
//
// A future-dated entry and a much-later one can both be "Later" without
// being adjacent, and a repeated key is a row `{#each}` -- and `VirtualList`,
// which uses the same key to remember scroll position and measured height --
// would conflate.

const laterA = entry('later-a', -30) // 30 days ahead -- "Later"
const today = entry('today', 0) // "Today", splitting the two "Later" runs apart
const laterB = entry('later-b', -2) // 2 days ahead -- "Later" again
const splitRows = toRows([laterA, today, laterB])
const keys = splitRows.map((r) => r.key)
assert.equal(new Set(keys).size, keys.length, 'every row key is distinct')
assert.equal(
  splitRows.filter((r) => r.kind === 'header' && r.label === 'Later').length,
  2,
  'two runs of "Later" get two headings, not one merged into the other',
)

// ── Entry rows carry the whole summary; header rows carry only the label ──

for (const row of rows) {
  if (row.kind === 'entry') assert.equal(row.key, row.entry.id)
  else assert.equal(typeof row.label, 'string')
}

// ── Search highlights ───────────────────────────────────────────────────
// The vault's ranges are UTF-8 bytes; the marks must land on the word anyway.
const marked = (text, ranges) =>
  pieces(text, ranges)
    .filter((p) => p.hit)
    .map((p) => p.text)
const bytes = (s) => new TextEncoder().encode(s).length
{
  const text = '…the budget for Q3'
  const from = bytes('…the ')
  assert.deepEqual(marked(text, [[from, from + bytes('budget')]]), ['budget'], 'after a leading …')
}
{
  const text = 'Café crème, then the budget'
  const from = bytes('Café crème, then the ')
  assert.deepEqual(marked(text, [[from, from + 6]]), ['budget'], 'after accented letters')
}
{
  const text = 'plain budget and budget'
  assert.deepEqual(
    marked(text, [
      [6, 12],
      [17, 23],
    ]),
    ['budget', 'budget'],
    'ASCII is unchanged, and every range is marked',
  )
  assert.equal(
    pieces(text, [[6, 12]])
      .map((p) => p.text)
      .join(''),
    text,
    'nothing is lost between the pieces',
  )
}

await close()
console.log('entry-rows: all checks passed')
