// Behaviour checks for the two modules the Overview is made of rules from.
//
// Same reasoning as `tracking.test.mjs` and `menu.test.mjs`: the type checker
// covers most of the interface, and what it cannot cover is arithmetic whose
// failures are quiet. Every case below is a number that would be wrong
// rather than an exception that would be seen.
//
// Streaks are the worst of them. A rest day counted as a break turns three
// runs a week into a chain that snaps every Tuesday, which is exactly the
// shape of habit tracking that makes people give up — and it neither throws
// nor fails to compile. A week you are two days into counted as a miss does
// the same thing to a hit rate. And on the balance side: a goal folded into
// the wrong role reports a balanced life to somebody who has not got one,
// and an unattributed share quietly dropped makes every chart flattering.
//
// No test framework, deliberately: one dependency-free file, run by
// `npm run check`, with the TypeScript loaded through Vite so it compiles
// exactly as the application compiles it.

import { load, makeCheck } from './harness.mjs'

const {
  modules: [habits, balance],
  close,
} = await load(['/src/lib/habits.ts', '/src/lib/balance.ts'])
const {
  DAILY,
  describeStreak,
  periodEnd,
  periodStart,
  periodsBetween,
  periodValue,
  progress,
  recorded,
  streakTarget,
  summarise,
} = habits
const { byRole, neglected, roleOf, totalMinutes } = balance

const { check, finish } = makeCheck()

// Monday-start weeks throughout, which is what `localeWeekStart` answers in
// most of the world and is the value the Overview passes.
const MONDAY = 1

// ── periods ────────────────────────────────────────────────────────────

check('a daily period is the day itself', periodStart('2026-09-09', 'day', MONDAY), '2026-09-09')
// 2026-09-09 is a Wednesday.
check('a week starts on the chosen day', periodStart('2026-09-09', 'week', MONDAY), '2026-09-07')
check('...and honours a Sunday start', periodStart('2026-09-09', 'week', 0), '2026-09-06')
check('a month starts on the first', periodStart('2026-09-09', 'month', MONDAY), '2026-09-01')

check('a week ends six days later', periodEnd('2026-09-07', 'week'), '2026-09-13')
check('a month ends on its own last day', periodEnd('2026-09-01', 'month'), '2026-09-30')
// February, because a month is not 30 days and pretending it is puts a
// hit in the wrong bucket once a year.
check('a short month ends where it ends', periodEnd('2027-02-01', 'month'), '2027-02-28')
check('a leap February does too', periodEnd('2028-02-01', 'month'), '2028-02-29')

check(
  'a quarter starts on its first month',
  periodStart('2026-09-09', 'quarter', MONDAY),
  '2026-07-01',
)
check('a year starts in January', periodStart('2026-09-09', 'year', MONDAY), '2026-01-01')
check('a quarter ends where its third month does', periodEnd('2026-07-01', 'quarter'), '2026-09-30')
check('a year ends on the last of December', periodEnd('2026-01-01', 'year'), '2026-12-31')

check(
  'the periods between two days cover both ends',
  periodsBetween('2026-09-07', '2026-09-20', 'week', MONDAY),
  ['2026-09-07', '2026-09-14'],
)

// ── streaks ────────────────────────────────────────────────────────────

const days = (...list) => list.map((date) => ({ trackerId: 't', date, count: 1, sum: 1, max: 1 }))
const check3 = { kind: 'check' }
const week3 = { min: 3, per: 'week', tally: 'days' }

{
  // Three runs in each of two closed weeks, and three already this week.
  const summary = summarise(
    days(
      '2026-08-24',
      '2026-08-26',
      '2026-08-28',
      '2026-08-31',
      '2026-09-02',
      '2026-09-04',
      '2026-09-07',
      '2026-09-08',
      '2026-09-09',
    ),
    check3,
    week3,
    { from: '2026-08-24', to: '2026-09-13', today: '2026-09-09' },
    MONDAY,
  )
  check('three met weeks in a row is a streak of three', summary.streak, 3)
  check('and the best run is the same three', summary.best, 3)
  check('the hit rate counts only closed weeks', summary.rate, 1)
}

{
  // The same, but only one run so far this week. The week is not over, so it
  // is not yet a break -- this is the case that would otherwise snap every
  // chain mid-week.
  const summary = summarise(
    days(
      '2026-08-24',
      '2026-08-26',
      '2026-08-28',
      '2026-08-31',
      '2026-09-02',
      '2026-09-04',
      '2026-09-07',
    ),
    check3,
    week3,
    { from: '2026-08-24', to: '2026-09-13', today: '2026-09-09' },
    MONDAY,
  )
  check('a week still running does not break the streak', summary.streak, 2)
  check('...and does not count against the rate', summary.rate, 1)
  check('...and reads as open, not missed', summary.periods.at(-1).state, 'open')
}

{
  // A missed week in the middle: the streak stops there, the best run does
  // not forget what came before it.
  const summary = summarise(
    days('2026-08-24', '2026-08-26', '2026-08-28', '2026-09-07', '2026-09-08', '2026-09-09'),
    check3,
    week3,
    { from: '2026-08-24', to: '2026-09-13', today: '2026-09-09' },
    MONDAY,
  )
  check('a missed week stops the streak at the current one', summary.streak, 1)
  check('but the best run remembers the earlier one', summary.best, 1)
  check('and the rate is one closed week in two', summary.rate, 0.5)
}

{
  // Several readings on one day are one day. "Did I do it" is not "how many
  // times did I write it down", or three doses on Monday would meet a
  // three-a-week target on its own.
  const summary = summarise(
    [
      { trackerId: 't', date: '2026-09-07', count: 3, sum: 3, max: 1 },
      { trackerId: 't', date: '2026-09-08', count: 1, sum: 1, max: 1 },
    ],
    check3,
    week3,
    { from: '2026-09-07', to: '2026-09-13', today: '2026-09-09' },
    MONDAY,
  )
  check('a day with three readings is still one day', summary.periods[0].value, 2)
  check('so a three-a-week target is not met by it yet', summary.periods[0].state, 'open')
}

{
  // A check ticked as *not* done is a zero, and is not a day it was done.
  const notDone = { trackerId: 't', date: '2026-09-08', count: 1, sum: 0, max: 0 }
  check('a check recorded as not done is not a hit', recorded(notDone, check3), false)
  check('...but a scale of 0 is a real answer', recorded(notDone, { kind: 'scale' }), true)
}

{
  // No target at all falls back to daily, which is what an untracked habit
  // should read as rather than as a crash.
  const summary = summarise(
    days('2026-09-08', '2026-09-09'),
    check3,
    null,
    { from: '2026-09-07', to: '2026-09-09', today: '2026-09-09' },
    MONDAY,
  )
  check('no target means daily', summary.streak, 2)
  check(
    'an empty history has no rate rather than a zero one',
    summarise(
      [],
      check3,
      week3,
      { from: '2026-09-07', to: '2026-09-13', today: '2026-09-09' },
      MONDAY,
    ).rate,
    null,
  )
}

// ── which target a streak is held to ───────────────────────────────────

{
  // What every tracker's old daily goal became. It drives the chip's ring
  // and must not break a chain: a 25-minute run on a 30-minute goal is still
  // a run, where a day off is not.
  const run = { targets: [{ min: 30, per: 'day', tally: 'value' }] }
  check('a daily amount is not what a streak counts', streakTarget(run), DAILY)
  const summary = summarise(
    [{ trackerId: 't', date: '2026-09-08', count: 1, sum: 25, max: 25 }],
    { kind: 'amount' },
    streakTarget(run),
    { from: '2026-09-08', to: '2026-09-09', today: '2026-09-09' },
    MONDAY,
  )
  check('...so a short run still keeps the streak', summary.streak, 1)

  const both = {
    targets: [
      { min: 30, per: 'day', tally: 'value' },
      { min: 3, per: 'week', tally: 'days' },
    ],
  }
  check('three days a week is the habit, whichever came first', streakTarget(both).per, 'week')
  const tvLimit = { targets: [{ max: 60, per: 'day', tally: 'value' }] }
  check('a limit is its own streak', streakTarget(tvLimit).max, 60)
  const piano = { targets: [{ min: 60, max: 120, per: 'week', tally: 'value' }] }
  check('so is a weekly amount', streakTarget(piano).per, 'week')
  check('no targets is daily, as it always was', streakTarget({}), DAILY)
}

// ── limits and ranges ──────────────────────────────────────────────────

const tv = { kind: 'amount', source: { type: 'manual' } }
const atMostAnHour = { max: 60, per: 'day', tally: 'value' }
function mins(date, sum) {
  return { trackerId: 't', date, count: 1, sum, max: sum }
}

{
  // An hour of TV a day at most: 45 and 60 are within (the bound is
  // inclusive), 90 is over, and a day with nothing written down is not a day
  // you stayed under -- it is one nobody recorded.
  const summary = summarise(
    [mins('2026-09-05', 45), mins('2026-09-06', 90), mins('2026-09-08', 60)],
    tv,
    atMostAnHour,
    { from: '2026-09-05', to: '2026-09-09', today: '2026-09-09' },
    MONDAY,
  )
  check(
    'each day stands against the limit',
    summary.periods.map((p) => p.state),
    ['met', 'missed', 'unrecorded', 'met', 'open'],
  )
  check('an unrecorded day neither extends nor breaks the streak', summary.streak, 1)
  check('...and is left out of the rate', summary.rate, 2 / 3)
}

{
  // Past the limit today is past it for good: the day is already decided.
  const summary = summarise(
    [mins('2026-09-09', 75)],
    tv,
    atMostAnHour,
    { from: '2026-09-09', to: '2026-09-09', today: '2026-09-09' },
    MONDAY,
  )
  check('over the limit today is missed today', summary.periods[0].state, 'missed')
}

{
  // A derived tracker's zero is a real zero: no time filed under the goal
  // is no time, not an unrecorded day.
  const summary = summarise(
    [],
    { kind: 'amount', source: { type: 'time' } },
    atMostAnHour,
    { from: '2026-09-07', to: '2026-09-08', today: '2026-09-09' },
    MONDAY,
  )
  check('nothing derived is nothing, and within a limit', summary.periods[0].state, 'met')
}

{
  // Between one and two hours of piano a week, as minutes.
  const piano = { min: 60, max: 120, per: 'week', tally: 'value' }
  const time = { kind: 'amount', source: { type: 'time' } }
  const week = (sum) =>
    summarise(
      [mins('2026-08-31', sum)],
      time,
      piano,
      { from: '2026-08-31', to: '2026-09-06', today: '2026-09-09' },
      MONDAY,
    ).periods[0]
  check('under the range is short', week(45).standing, 'short')
  check('inside it is within', week(90).state, 'met')
  check('over it is over, and missed', [week(150).standing, week(150).state], ['over', 'missed'])
}

check(
  'a severity averages over every reading in the period',
  periodValue(
    [
      { trackerId: 't', date: '2026-09-07', count: 2, sum: 10, max: 7 },
      { trackerId: 't', date: '2026-09-08', count: 1, sum: 2, max: 2 },
    ],
    { kind: 'scale' },
    'value',
  ),
  4,
)

// ── progress and pace ──────────────────────────────────────────────────

{
  // Twelve books this year, one finished by the 26th of September.
  const p = progress(
    [{ trackerId: 't', date: '2026-09-02', count: 1, sum: 1, max: 1 }],
    { kind: 'amount', source: { type: 'finished' } },
    { min: 12, per: 'year', tally: 'value' },
    '2026-09-26',
    MONDAY,
  )
  check('the year runs from January', [p.start, p.end], ['2026-01-01', '2026-12-31'])
  check('one book so far', p.value, 1)
  // 269 of 365 days gone: nearly nine books would be on pace.
  check('pace is the minimum spread evenly over the period', Math.round(p.expected * 10) / 10, 8.8)
  check('and a year still running is open, not missed', p.state, 'open')
}

check(
  'a daily target has no pace to be behind',
  progress([], tv, atMostAnHour, '2026-09-26', MONDAY).expected,
  null,
)

check('a streak reads in its own period', describeStreak(3, 'week'), '3 weeks')
check('...and in the singular', describeStreak(1, 'day'), '1 day')
check('...and says so when there is none', describeStreak(0, 'week'), 'no streak')

// ── by role ────────────────────────────────────────────────────────────

const roles = [
  { id: 'r-work', name: 'Work', color: '#0369a1', icon: '', archived: false },
  { id: 'r-parent', name: 'Parent', color: '#c2410c', icon: '', archived: false },
  { id: 'r-old', name: 'Retired role', color: '#000', icon: '', archived: true },
]
const goals = [
  { id: 'g-ship', roleId: 'r-work', title: 'Ship it', status: 'active' },
  { id: 'g-bike', roleId: 'r-parent', title: 'Bike', status: 'active' },
]

check(
  'a goal resolves to the role above it',
  roleOf({ type: 'goal', id: 'g-ship' }, goals),
  'r-work',
)
check('a role points at itself', roleOf({ type: 'role', id: 'r-parent' }, goals), 'r-parent')
check('nothing is unattributed', roleOf(null, goals), null)
// Deleting a goal deliberately does not rewrite what pointed at it, so this
// is a normal state rather than a corruption -- and it must not throw.
check('a deleted goal reads as unattributed', roleOf({ type: 'goal', id: 'g-gone' }, goals), null)

{
  const report = {
    purposes: [
      {
        purpose: { type: 'goal', id: 'g-ship' },
        actualMinutes: 120,
        plannedMinutes: 180,
        blocks: 2,
      },
      {
        purpose: { type: 'role', id: 'r-parent' },
        actualMinutes: 60,
        plannedMinutes: 0,
        blocks: 1,
      },
      { purpose: { type: 'goal', id: 'g-gone' }, actualMinutes: 15, plannedMinutes: 0, blocks: 1 },
      { actualMinutes: 45, plannedMinutes: 30, blocks: 3 },
    ],
    events: [
      { roleId: 'r-work', minutes: 90, events: 3 },
      { roleId: null, minutes: 30, events: 1 },
    ],
  }
  const rows = byRole(report, roles, goals)

  check(
    'busiest first, and the unattributed row last however big',
    rows.map((r) => r.roleId),
    ['r-work', 'r-parent', null],
  )
  const work = rows.find((r) => r.roleId === 'r-work')
  check('a goal folds into its role', work.actualMinutes, 120)
  check('meetings are counted apart from your own record', work.eventMinutes, 90)
  check('and the plan is kept beside the actual, never summed', work.plannedMinutes, 180)
  check('the goal breakdown names what got the time', work.goals, [
    { goalId: 'g-ship', title: 'Ship it', actualMinutes: 120 },
  ])

  const unfiled = rows.find((r) => r.roleId === null)
  // 45 with no purpose at all, plus 15 against a goal that no longer exists.
  check('unfiled time and a dead pointer land together', unfiled.actualMinutes, 60)
  check('...as do meetings from a feed with no role', unfiled.eventMinutes, 30)
  check('the unattributed row is named rather than blank', unfiled.name, 'Not filed')

  check(
    'an archived role gets no row',
    rows.some((r) => r.roleId === 'r-old'),
    false,
  )
  check('every minute is accounted for somewhere', totalMinutes(rows), 240)
}

{
  // A role with nothing recorded still gets a row: its absence is the
  // finding, and a chart with one fewer bar than last week hides it.
  const rows = byRole({ purposes: [], events: [] }, roles, goals)
  check('a quiet role is drawn at zero rather than dropped', rows.length, 3)
  check('...and an empty report is not an error', totalMinutes(rows), 0)
}

check('no report at all still draws the rows', byRole(null, roles, goals).length, 3)

// ── neglect ────────────────────────────────────────────────────────────

{
  const rows = byRole({ purposes: [], events: [] }, roles, goals)
  const touched = new Map([
    ['g-ship', '2026-09-08T10:00:00Z'],
    ['g-bike', '2026-06-01T10:00:00Z'],
  ])
  const found = neglected(rows, roles, goals, touched, '2026-09-09')
  check(
    'a role whose goals have gone stale is called out',
    found.map((f) => f.role.id),
    ['r-parent'],
  )
  check('and it says how long', found[0].days, 100)
}

{
  // Time this week is a touch on its own: a role you spent six hours on is
  // not neglected however stale its goals' own records look.
  const rows = byRole(
    {
      purposes: [
        {
          purpose: { type: 'role', id: 'r-parent' },
          actualMinutes: 360,
          plannedMinutes: 0,
          blocks: 4,
        },
      ],
      events: [],
    },
    roles,
    goals,
  )
  const touched = new Map([
    ['g-bike', '2026-06-01T10:00:00Z'],
    ['g-ship', '2026-09-08T10:00:00Z'],
  ])
  check(
    'time recorded this window clears the flag',
    neglected(rows, roles, goals, touched, '2026-09-09'),
    [],
  )
}

{
  // A role with no open goals is one you are not using, not one you are
  // neglecting -- and it must not be nagged about.
  const bare = [{ id: 'r-bare', name: 'Bare', color: '#000', icon: '', archived: false }]
  const rows = byRole({ purposes: [], events: [] }, bare, [])
  check(
    'a role with no goals is not neglected',
    neglected(rows, bare, [], new Map(), '2026-09-09'),
    [],
  )
}

{
  // Never touched at all is the strongest case, not the weakest, and sorts
  // ahead of merely stale.
  const touched = new Map([['g-ship', '2026-01-01T10:00:00Z']])
  const rows = byRole({ purposes: [], events: [] }, roles, goals)
  const found = neglected(rows, roles, goals, touched, '2026-09-09')
  check(
    'a goal nothing has ever touched sorts first',
    found.map((f) => f.role.id),
    ['r-parent', 'r-work'],
  )
  check('and reports no age rather than a made-up one', found[0].days, null)
}

await close()
finish('overview')
