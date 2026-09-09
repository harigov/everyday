// Behaviour checks for the tracking arithmetic.
//
// Same reasoning as `calendar.test.mjs`: the type checker covers most of the
// interface, and this module is one of the exceptions. `tracker.ts` decides
// what a recorded number *means* — whether a day of doses adds up or a day of
// severities averages, whether a reading is a moment on the calendar or a
// span across it — and every rule in it is one a later change could reverse
// without anything failing to compile.
//
// The failure mode is quiet, too. Sum a column of severities instead of
// averaging them and the chart still draws; it is simply wrong about how bad
// the week was.
//
// No test framework, deliberately -- one dependency-free file, run by
// `npm run check`, loading the TypeScript through Vite so it is compiled
// exactly as the application compiles it.

import { createServer } from 'vite'

const server = await createServer({
  configFile: false,
  root: new URL('..', import.meta.url).pathname,
  // `watch: null` because a test loads a module once and exits. Vite's
  // watcher is on by default even in middleware mode, and a watcher is a
  // per-user resource: a suite that starts one server per file exhausts the
  // supply (`EMFILE`) on any machine that already has a dev server running.
  server: { middlewareMode: true, watch: null },
  appType: 'custom',
  logLevel: 'error',
})

const { dayValue, durationMinutes, formatDay, formatNumber, formatValue, shownTrackers } =
  await server.ssrLoadModule('/src/lib/tracker.ts')

let failed = 0
function check(what, got, want) {
  if (JSON.stringify(got) === JSON.stringify(want)) return
  failed += 1
  console.error(
    `FAIL  ${what}\n        got  ${JSON.stringify(got)}\n        want ${JSON.stringify(want)}`,
  )
}

/** A tracker with the defaults every field needs, overridden as asked. */
function tracker(rest = {}) {
  return {
    id: 't1',
    name: 'Thing',
    kind: 'check',
    icon: 'dot',
    color: '#000',
    unit: '',
    defaultValue: 1,
    target: null,
    scaleMax: 10,
    onCalendar: false,
    archived: false,
    sortOrder: 0,
    createdAt: '2026-01-01T00:00:00Z',
    updatedAt: '2026-01-01T00:00:00Z',
    ...rest,
  }
}

function reading(value, at = null) {
  return { id: `r${value}${at ?? ''}`, value, at }
}

// ── how a number is written ─────────────────────────────────────────────

check('a whole number loses its decimals', formatNumber(45), '45')
check('a fraction keeps them', formatNumber(0.5), '0.5')
check('nonsense reads as zero rather than as NaN', formatNumber(NaN), '0')

check('a dose carries its unit', formatValue(tracker({ kind: 'dose', unit: 'mg' }), 400), '400 mg')
check('a severity is out of its scale', formatValue(tracker({ kind: 'scale' }), 6), '6/10')
check('a check is words, not a number', formatValue(tracker(), 1), 'Done')
check('and says so when it is explicitly not done', formatValue(tracker(), 0), 'Not done')

// ── how a day of them combines ──────────────────────────────────────────
//
// The heart of it. Two doses of 400 mg is 800 mg; two headaches of 3 and 7
// is a day averaging 5 and never a day of 10.

const dose = tracker({ kind: 'dose', unit: 'mg' })
check('doses add up', dayValue(dose, [reading(400), reading(400)]), 800)
check(
  'and the day says how many there were',
  formatDay(dose, [reading(400), reading(400)]),
  '800 mg · 2',
)
check('one dose does not need a count', formatDay(dose, [reading(400)]), '400 mg')

const pain = tracker({ kind: 'scale' })
check('severities average', dayValue(pain, [reading(3), reading(7)]), 5)
check('and the day says it is an average', formatDay(pain, [reading(3), reading(7)]), '5/10 avg')
check('a single reading is not an average of anything', formatDay(pain, [reading(6)]), '6/10')

const habit = tracker()
check('a habit counts rather than sums', dayValue(habit, [reading(1)]), 1)
check('and reads as a tick', formatDay(habit, [reading(1)]), 'Done')
check('nothing recorded reads as nothing at all', formatDay(habit, []), '')
check(
  'and a habit recorded as *not* done is not the same as unanswered',
  formatDay(habit, [reading(0)]),
  'Not done',
)

const pages = tracker({ kind: 'amount', unit: 'pages' })
check('quantities add up', formatDay(pages, [reading(20), reading(15)]), '35 pages · 2')

// ── what draws as a span on the calendar ────────────────────────────────
//
// Only a quantity measured in time has a length. Everything else is a
// moment, and drawing it as a rectangle would invent minutes nobody spent.

check('minutes are a span', durationMinutes(tracker({ kind: 'amount', unit: 'min' }), 45), 45)
check(
  'hours are a longer one',
  durationMinutes(tracker({ kind: 'amount', unit: 'hours' }), 7.5),
  450,
)
check('pages are not a span', durationMinutes(tracker({ kind: 'amount', unit: 'pages' }), 30), null)
check(
  'and a dose is a moment however it is measured',
  durationMinutes(tracker({ kind: 'dose', unit: 'min' }), 45),
  null,
)
check(
  'a zero-length span is no span',
  durationMinutes(tracker({ kind: 'amount', unit: 'min' }), 0),
  null,
)

// ── which trackers a day offers ─────────────────────────────────────────
//
// A tracker is a vault record and a journal names the ones it draws, so this
// resolves ids against the vault's list. Three ways it can go wrong and
// none of them throws: an id naming a tracker that has been deleted, one
// naming an archived tracker, and an order that disagrees with the array.

const all = [
  tracker({ id: 'b', name: 'Second', sortOrder: 2 }),
  tracker({ id: 'a', name: 'First', sortOrder: 1 }),
  tracker({ id: 'z', name: 'Retired', sortOrder: 0, archived: true }),
]
const journal = { shownTrackers: ['b', 'a', 'z'] }

check(
  'the strip is in the arranged order, and archived ones have left it',
  shownTrackers(journal, all).map((t) => t.id),
  ['a', 'b'],
)
check('no journal, no trackers', shownTrackers(null, all), [])
check('a journal that shows nothing draws nothing', shownTrackers({ shownTrackers: [] }, all), [])

// A stale id is the ordinary consequence of deleting a tracker: every
// journal that showed it keeps the id, and the strip has to skip it rather
// than fail to draw at all.
check(
  'an id naming a tracker that is gone is skipped, not an error',
  shownTrackers({ shownTrackers: ['a', 'deleted', 'b'] }, all).map((t) => t.id),
  ['a', 'b'],
)

await server.close()

if (failed) {
  console.error(`\ntracking: ${failed} check(s) failed`)
  process.exit(1)
}
console.log('tracking: all checks passed')
