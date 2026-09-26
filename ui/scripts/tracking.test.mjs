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

import { load, makeCheck } from './harness.mjs'

const { module: trackerLib, close } = await load('/src/lib/tracker.ts')
const {
  dayValue,
  describeProgress,
  describeTarget,
  durationMinutes,
  formatAmount,
  formatDay,
  formatNumber,
  formatValue,
  shownTrackers,
} = trackerLib

const { check, finish } = makeCheck()

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
    targets: [],
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

check(
  'a derived tracker is never a chip, even when a journal names it',
  shownTrackers({ shownTrackers: ['a', 'd'] }, [
    tracker({ id: 'a' }),
    tracker({ id: 'd', kind: 'amount', source: { type: 'time' } }),
  ]).map((t) => t.id),
  ['a'],
)

// ── targets in words ───────────────────────────────────────────────────

const minutes = tracker({ kind: 'amount', unit: 'min' })
const books = tracker({ kind: 'amount', unit: '', source: { type: 'finished' } })

check('time reads as time', formatAmount(minutes, 90), '1h 30m')
check('...including none of it', formatAmount(minutes, 0), '0m')
check('a count reads as a number', formatAmount(books, 12), '12')
check('days read as days', formatAmount(minutes, 3, 'days'), '3 days')
check('one day is one day', formatAmount(minutes, 1, 'days'), '1 day')

check(
  'a range reads in hours',
  describeTarget(minutes, { min: 60, max: 120, per: 'week', tally: 'value' }),
  'between 1h and 2h a week',
)
check(
  'a limit',
  describeTarget(minutes, { max: 60, per: 'day', tally: 'value' }),
  'at most 1h a day',
)
check(
  'a count over a year',
  describeTarget(books, { min: 12, per: 'year', tally: 'value' }),
  'at least 12 a year',
)
check(
  'a habit',
  describeTarget(tracker(), { min: 3, per: 'week', tally: 'days' }),
  'at least 3 days a week',
)
check(
  'a daily habit is said the way people say it',
  describeTarget(tracker(), { min: 1, per: 'day', tally: 'days' }),
  'every day',
)

{
  const year = { min: 12, per: 'year', tally: 'value' }
  const words = describeProgress(books, {
    target: year,
    start: '2026-01-01',
    end: '2026-12-31',
    value: 1,
    standing: 'short',
    state: 'open',
    expected: 8.8,
  })
  check('progress is compact for a sidebar', words.short, '1/12')
  check('...and a line for the rail', words.line, '1 this year · at least 12 a year')
  check('...and says how far behind pace, in words', words.status, '8 behind pace')
  check('...and is not over anything', words.over, false)
}

{
  const words = describeProgress(minutes, {
    target: { max: 60, per: 'day', tally: 'value' },
    start: '2026-09-26',
    end: '2026-09-26',
    value: 80,
    standing: 'over',
    state: 'missed',
    expected: null,
  })
  check('past a limit says by how much', words.status, 'over by 20m')
  check('...and is the one state marked over', words.over, true)
}

{
  const words = describeProgress(minutes, {
    target: { min: 60, max: 120, per: 'week', tally: 'value' },
    start: '2026-09-21',
    end: '2026-09-27',
    value: 40,
    standing: 'short',
    state: 'open',
    expected: 51,
  })
  check('a week speaks of no pace', words.status, null)
  check('its compact form is against the minimum', words.short, '40m/1h')
}

await close()
finish('tracking')
