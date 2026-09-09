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

import { createServer } from 'vite'

const server = await createServer({
  configFile: false,
  root: new URL('..', import.meta.url).pathname,
  server: { middlewareMode: true },
  appType: 'custom',
  logLevel: 'error',
})

const { describeStreak, periodEnd, periodStart, periodsBetween, summarise } =
  await server.ssrLoadModule('/src/lib/habits.ts')
const { byRole, neglected, roleOf, totalMinutes } =
  await server.ssrLoadModule('/src/lib/balance.ts')

let failed = 0
function check(what, got, want) {
  const a = JSON.stringify(got)
  const b = JSON.stringify(want)
  if (a === b) return
  failed++
  console.error(`✗ ${what}\n  got  ${a}\n  want ${b}`)
}

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
  'the periods between two days cover both ends',
  periodsBetween('2026-09-07', '2026-09-20', 'week', MONDAY),
  ['2026-09-07', '2026-09-14'],
)

// ── streaks ────────────────────────────────────────────────────────────

const days = (...list) => list.map((date) => ({ trackerId: 't', date, count: 1, sum: 1, max: 1 }))
const week3 = { times: 3, per: 'week' }

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
    week3,
    { from: '2026-08-24', to: '2026-09-13', today: '2026-09-09' },
    MONDAY,
  )
  check('a week still running does not break the streak', summary.streak, 2)
  check('...and does not count against the rate', summary.rate, 1)
}

{
  // A missed week in the middle: the streak stops there, the best run does
  // not forget what came before it.
  const summary = summarise(
    days('2026-08-24', '2026-08-26', '2026-08-28', '2026-09-07', '2026-09-08', '2026-09-09'),
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
  // three-a-week cadence on its own.
  const summary = summarise(
    [
      { trackerId: 't', date: '2026-09-07', count: 3, sum: 3, max: 1 },
      { trackerId: 't', date: '2026-09-08', count: 1, sum: 1, max: 1 },
    ],
    week3,
    { from: '2026-09-07', to: '2026-09-13', today: '2026-09-09' },
    MONDAY,
  )
  check('a day with three readings is still one day', summary.periods[0].hits, 2)
  check('so a three-a-week cadence is not met by it', summary.periods[0].met, false)
}

{
  // No cadence at all falls back to daily, which is what an untracked habit
  // should read as rather than as a crash.
  const summary = summarise(
    days('2026-09-08', '2026-09-09'),
    null,
    { from: '2026-09-07', to: '2026-09-09', today: '2026-09-09' },
    MONDAY,
  )
  check('no cadence means daily', summary.streak, 2)
  check(
    'an empty history has no rate rather than a zero one',
    summarise([], week3, { from: '2026-09-07', to: '2026-09-13', today: '2026-09-09' }, MONDAY)
      .rate,
    null,
  )
}

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

await server.close()

if (failed) {
  console.error(`\noverview: ${failed} check(s) failed`)
  process.exit(1)
}
console.log('overview: all checks passed')
