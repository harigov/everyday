// Behaviour checks for the calendar grid's arithmetic.
//
// The same reasoning as `quickadd.test.mjs`: most of the interface is
// checked by the type checker and by the fact that it compiles, and these
// two modules are the exceptions. `time.ts` is pure maths that decides where
// every rectangle on the grid is drawn, and every rule in it is a decision a
// later change could silently reverse — a week that starts on the wrong day,
// a month view that loses a row, two clashing meetings stacked on top of one
// another so only the later one is visible.
//
// It is also the sort of code where the bugs are invisible rather than loud.
// A grid with the lane packing wrong still renders; it just quietly hides an
// appointment.
//
// No test framework, deliberately -- one dependency-free file, run by
// `npm run check`. The TypeScript is loaded through Vite so it is compiled
// exactly as the application compiles it.

import { createServer } from 'vite'

const server = await createServer({
  configFile: false,
  root: new URL('..', import.meta.url).pathname,
  server: { middlewareMode: true },
  appType: 'custom',
  logLevel: 'error',
})

const { addDays, addMonths, daysFrom, monthGrid, packLanes, snap, startOfWeek } =
  await server.ssrLoadModule('/src/lib/time.ts')

let failed = 0
function check(what, got, want) {
  if (JSON.stringify(got) === JSON.stringify(want)) return
  failed += 1
  console.error(
    `FAIL  ${what}\n        got  ${JSON.stringify(got)}\n        want ${JSON.stringify(want)}`,
  )
}

// ── Days and weeks ───────────────────────────────────────────────────────

check(
  'a day steps forward and back',
  [addDays('2026-09-06', 1), addDays('2026-09-06', -1)],
  ['2026-09-07', '2026-09-05'],
)
check('stepping crosses a month boundary', addDays('2026-09-30', 1), '2026-10-01')
check('and a year boundary', addDays('2026-12-31', 1), '2027-01-01')

// The bug this guards: `new Date('2026-09-06')` parses as UTC midnight, so
// anywhere west of Greenwich the day comes out one earlier. Every date in
// this file has to be read as a *local* day or the whole grid slips.
check('a date is read as a local day, not a UTC instant', addDays('2026-09-06', 0), '2026-09-06')

check('a Monday-start week begins on Monday', startOfWeek('2026-09-09', 1), '2026-09-07')
check('a Sunday-start week begins on Sunday', startOfWeek('2026-09-09', 0), '2026-09-06')
check('a week that already starts right is left alone', startOfWeek('2026-09-07', 1), '2026-09-07')
check(
  'the week of a Sunday, counting from Monday, is the one before',
  [startOfWeek('2026-09-06', 1)],
  ['2026-08-31'],
)

check('a week is seven consecutive days', daysFrom('2026-09-07', 7), [
  '2026-09-07',
  '2026-09-08',
  '2026-09-09',
  '2026-09-10',
  '2026-09-11',
  '2026-09-12',
  '2026-09-13',
])

// ── Months ───────────────────────────────────────────────────────────────

check(
  'a month steps forward and back',
  [addMonths('2026-09-15', 1), addMonths('2026-09-15', -1)],
  ['2026-10-15', '2026-08-15'],
)
// The bug this guards: 31 January + 1 month landing on 3 March, so paging
// forward through the year skips February entirely.
check(
  'stepping from the 31st clamps to the shorter month',
  addMonths('2026-01-31', 1),
  '2026-02-28',
)
check('and does so in a leap year too', addMonths('2028-01-31', 1), '2028-02-29')
check('stepping a month crosses a year', addMonths('2026-12-10', 1), '2027-01-10')

const september = monthGrid('2026-09-15', 1)
check('a month grid is always six weeks', september.length, 42)
check('a month grid starts on the first day of a week', september[0], '2026-08-31')
check('a month grid contains its own month', september.includes('2026-09-15'), true)
// Six rows even when five would do: a view whose height changes as you page
// through the year makes everything below it jump.
check('a short month still fills six rows', monthGrid('2026-02-10', 1).length, 42)

// ── Snapping ─────────────────────────────────────────────────────────────

check('minutes snap to the nearest quarter', [snap(0), snap(7), snap(8), snap(22)], [0, 0, 15, 15])

// ── Overlap packing ──────────────────────────────────────────────────────
//
// The rule: things that clash share the width; things that do not, do not.

const lanesOf = (items) => packLanes(items).map(({ item, lane, lanes }) => [item.id, lane, lanes])

check('one thing alone takes the whole column', lanesOf([{ id: 'a', start: 540, end: 600 }]), [
  ['a', 0, 1],
])

check(
  'two clashing things take half each',
  lanesOf([
    { id: 'a', start: 540, end: 660 },
    { id: 'b', start: 600, end: 720 },
  ]),
  [
    ['a', 0, 2],
    ['b', 1, 2],
  ],
)

// The bug this guards: one pair of clashing meetings in the morning making
// every other meeting in the day half-width, because the whole column was
// treated as a single cluster.
check(
  'a later, unrelated thing is unaffected by an earlier clash',
  lanesOf([
    { id: 'a', start: 540, end: 660 },
    { id: 'b', start: 600, end: 720 },
    { id: 'c', start: 840, end: 900 },
  ]),
  [
    ['a', 0, 2],
    ['b', 1, 2],
    ['c', 0, 1],
  ],
)

// Back-to-back is not a clash: a meeting that ends at ten and one that
// starts at ten should each have the full width.
check(
  'things that merely touch do not clash',
  lanesOf([
    { id: 'a', start: 540, end: 600 },
    { id: 'b', start: 600, end: 660 },
  ]),
  [
    ['a', 0, 1],
    ['b', 0, 1],
  ],
)

check(
  'a lane is reused once the thing in it has finished',
  lanesOf([
    { id: 'long', start: 540, end: 780 },
    { id: 'first', start: 545, end: 600 },
    { id: 'second', start: 610, end: 660 },
  ]),
  [
    ['long', 0, 2],
    ['first', 1, 2],
    ['second', 1, 2],
  ],
)

check(
  'three clashing things take a third each',
  lanesOf([
    { id: 'a', start: 540, end: 660 },
    { id: 'b', start: 550, end: 660 },
    { id: 'c', start: 560, end: 660 },
  ]).map(([, , lanes]) => lanes),
  [3, 3, 3],
)

// A zero-length item -- a timer that has only just been started -- must
// still be given a lane rather than drawn as a hairline behind its
// neighbour.
check(
  'a zero-length item still clashes with what it sits inside',
  lanesOf([
    { id: 'meeting', start: 540, end: 600 },
    { id: 'justStarted', start: 545, end: 545 },
  ]).map(([, , lanes]) => lanes),
  [2, 2],
)

check('nothing to pack is not an error', packLanes([]), [])

await server.close()

if (failed > 0) {
  console.error(`\n${failed} calendar ${failed === 1 ? 'check' : 'checks'} failed`)
  process.exit(1)
}
console.log('calendar: all checks passed')
