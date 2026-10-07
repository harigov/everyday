// Behaviour checks for `recurrence.ts`: the Repeat menu's presets, the
// sentence a rule reads as, and which days a rule falls on.
//
// The same reasoning as `calendar.test.mjs`. Every preset here is a promise
// made in words -- "every month on the second Tuesday" -- and the rule
// behind it is what is sent to Google; a preset that quietly meant the
// fourteenth instead would be forty wrong meetings, all of them looking
// right in the menu that made them.

import { load, makeCheck } from './harness.mjs'

// `dateFormat` reads `navigator.language`. Node's own `navigator` is a
// getter-only property, so it is redefined rather than assigned -- see
// `format.test.mjs` for the same move.
Object.defineProperty(globalThis, 'navigator', {
  value: { userAgent: 'node', language: 'en-GB' },
  configurable: true,
})

const { module: r, close } = await load('/src/lib/recurrence.ts')
const { check, ok, finish } = makeCheck()

// ── Weekdays ─────────────────────────────────────────────────────────────

check('a day knows its weekday', r.weekdayOf('2026-10-13'), 'tuesday')
check('Sunday is the end of the week, not its start', r.weekdayOf('2026-10-18'), 'sunday')
check('the second Tuesday of October is in its second week', r.weekOfMonth('2026-10-13'), {
  nth: 2,
  last: false,
})
check('the 27th of October is its last Tuesday', r.weekOfMonth('2026-10-27'), {
  nth: 4,
  last: true,
})

// ── Presets ──────────────────────────────────────────────────────────────

{
  const presets = r.presetsFor('2026-10-13')
  check(
    'a Tuesday in the second week is offered these',
    presets.map((p) => p.key),
    ['none', 'daily', 'weekdays', 'weekly', 'monthly', 'monthlyNth', 'yearly'],
  )
  check(
    'and they say so',
    presets.map((p) => p.label),
    [
      'Does not repeat',
      'Every day',
      'Every weekday (Monday to Friday)',
      'Every week on Tuesday',
      'Every month on day 13',
      'Every month on the second Tuesday',
      'Every year on 13 October',
    ],
  )
  check(
    'the second Tuesday is a monthly rule by week',
    presets.find((p) => p.key === 'monthlyNth').rule,
    { frequency: 'monthly', interval: 1, weekOfMonth: 2, weekdays: ['tuesday'] },
  )
}

check(
  'a day in the last week is offered both its week and "the last"',
  r.presetsFor('2026-10-27').map((p) => p.key),
  ['none', 'daily', 'weekdays', 'weekly', 'monthly', 'monthlyNth', 'monthlyLast', 'yearly'],
)
check(
  'a day in the fifth week is offered only "the last"',
  r.presetsFor('2026-10-29').map((p) => p.key),
  ['none', 'daily', 'weekdays', 'weekly', 'monthly', 'monthlyLast', 'yearly'],
)

// An instant is read in the zone it is quoted in: 23:30 on Monday in New
// York is already Tuesday in UTC, and the repeat is New York's.
check(
  'an instant is read in its own zone',
  r.presetsFor('2026-10-13T03:30:00Z', 'America/New_York').find((p) => p.key === 'weekly').label,
  'Every week on Monday',
)

// ── Matching a rule back to a preset ─────────────────────────────────────

check('no rule is "does not repeat"', r.matchPreset(null, '2026-10-13'), 'none')
check(
  'a weekly rule with no days is the start’s own day',
  r.matchPreset({ frequency: 'weekly', interval: 1 }, '2026-10-13'),
  'weekly',
)
check(
  'Monday to Friday is "every weekday", in any order',
  r.matchPreset(
    {
      frequency: 'weekly',
      interval: 1,
      weekdays: ['friday', 'monday', 'tuesday', 'thursday', 'wednesday'],
    },
    '2026-10-13',
  ),
  'weekdays',
)
check(
  'an ending makes any rule custom',
  r.matchPreset({ frequency: 'daily', interval: 1, count: 5 }, '2026-10-13'),
  'custom',
)
check(
  'every other week is custom',
  r.matchPreset({ frequency: 'weekly', interval: 2 }, '2026-10-13'),
  'custom',
)
check(
  'the last Tuesday matches only on a day in the last week',
  [
    r.matchPreset(
      { frequency: 'monthly', interval: 1, weekOfMonth: -1, weekdays: ['tuesday'] },
      '2026-10-27',
    ),
    r.matchPreset(
      { frequency: 'monthly', interval: 1, weekOfMonth: -1, weekdays: ['tuesday'] },
      '2026-10-13',
    ),
  ],
  ['monthlyLast', 'custom'],
)

// Moving the start carries what the preset meant, not the day it named.
check(
  '"every week on Tuesday" moved to a Thursday is every Thursday',
  r.carryRule(
    { frequency: 'weekly', interval: 1, weekdays: ['tuesday'] },
    '2026-10-13',
    '2026-10-15',
  ),
  { frequency: 'weekly', interval: 1, weekdays: ['thursday'] },
)
check(
  'a custom rule is left exactly as chosen',
  r.carryRule(
    { frequency: 'weekly', interval: 1, weekdays: ['monday', 'wednesday'] },
    '2026-10-13',
    '2026-10-15',
  ),
  { frequency: 'weekly', interval: 1, weekdays: ['monday', 'wednesday'] },
)

// ── Sentences ────────────────────────────────────────────────────────────

check(
  'two days and an end date',
  r.describeRecurrence(
    { frequency: 'weekly', interval: 1, weekdays: ['wednesday', 'monday'], until: '2026-12-31' },
    '2026-10-12',
  ),
  'Every week on Monday and Wednesday, until 31 Dec 2026',
)
check(
  'every other week, a number of times',
  r.describeRecurrence({ frequency: 'weekly', interval: 2, count: 6 }, '2026-10-13'),
  'Every 2 weeks on Tuesday, 6 times',
)
check(
  'Monday to Friday reads as weekdays',
  r.describeRecurrence({ frequency: 'weekly', interval: 1, weekdays: r.WORKDAYS }, '2026-10-13'),
  'Every weekday',
)
check(
  'a month by week',
  r.describeRecurrence({
    frequency: 'monthly',
    interval: 1,
    weekOfMonth: -1,
    weekdays: ['friday'],
  }),
  'Every month on the last Friday',
)
check(
  'a month by date takes its day from the start',
  r.describeRecurrence({ frequency: 'monthly', interval: 3 }, '2026-10-13'),
  'Every 3 months on day 13',
)
check(
  'without a start, nothing is guessed',
  r.describeRecurrence({ frequency: 'yearly', interval: 1 }),
  'Every year',
)

// ── Expansion ────────────────────────────────────────────────────────────

check(
  'daily, through a date',
  r.expandDays({ frequency: 'daily', interval: 1 }, '2026-10-13', '2026-10-16'),
  ['2026-10-13', '2026-10-14', '2026-10-15', '2026-10-16'],
)
check(
  'every other week on two days',
  r.expandDays(
    { frequency: 'weekly', interval: 2, weekdays: ['tuesday', 'thursday'] },
    '2026-10-13',
    '2026-11-01',
  ),
  ['2026-10-13', '2026-10-15', '2026-10-27', '2026-10-29'],
)
check(
  'a count includes the first',
  r.expandDays({ frequency: 'weekly', interval: 1, count: 3 }, '2026-10-13', '2027-10-13'),
  ['2026-10-13', '2026-10-20', '2026-10-27'],
)
check(
  'until is inclusive',
  r.expandDays(
    { frequency: 'daily', interval: 1, until: '2026-10-14' },
    '2026-10-13',
    '2027-10-13',
  ),
  ['2026-10-13', '2026-10-14'],
)
check(
  'the second Tuesday of each month',
  r.expandDays(
    { frequency: 'monthly', interval: 1, weekOfMonth: 2, weekdays: ['tuesday'] },
    '2026-10-13',
    '2027-01-31',
  ),
  ['2026-10-13', '2026-11-10', '2026-12-08', '2027-01-12'],
)
check(
  'the last Friday of each month',
  r.expandDays(
    { frequency: 'monthly', interval: 1, weekOfMonth: -1, weekdays: ['friday'] },
    '2026-10-30',
    '2026-12-31',
  ),
  ['2026-10-30', '2026-11-27', '2026-12-25'],
)
check(
  'the 31st is skipped in a month without one',
  r.expandDays({ frequency: 'monthly', interval: 1 }, '2026-10-31', '2027-01-31'),
  ['2026-10-31', '2026-12-31', '2027-01-31'],
)

// ── Midnights in a zone ──────────────────────────────────────────────────

check(
  'midnight in a zone is that zone’s midnight',
  r.midnightIn('2026-10-13', 'America/New_York'),
  '2026-10-13T04:00:00.000Z',
)
check(
  'and the day an instant falls on is that zone’s day',
  r.dayIn('2026-10-13T03:30:00.000Z', 'America/New_York'),
  '2026-10-12',
)
ok(
  'an unknown zone falls back to this machine rather than throwing',
  typeof r.midnightIn('2026-10-13', 'Not/AZone') === 'string',
)

await close()
finish('recurrence')
