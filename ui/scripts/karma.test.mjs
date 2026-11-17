// Behaviour checks for the interface's copy of the Karma rules.
//
// The rules belong to `karmayoga_core::karma` and are tested there; this
// copy answers the mock backend and says what "Default" means in the
// tracker editor. These are the same cases the Rust tests pin, so the two
// cannot drift apart without one of them failing.

import { load, makeCheck } from './harness.mjs'

const { module: karma, close } = await load('/src/lib/karma.ts')
const {
  DAY_CEILING,
  HABIT_DEFAULT,
  HABIT_LIMIT,
  defaultKarma,
  habitPoints,
  karmaLevel,
  karmaSummary,
  scoreDay,
  signed,
} = karma

const { check, finish } = makeCheck()

const tracker = (kind, rest = {}) => ({ id: 't', name: 't', kind, targets: [], ...rest })
const day = (count, sum) => ({ trackerId: 't', date: '2026-11-16', count, sum, max: sum })
const deed = (kind, points) => ({ kind, label: '', points })

check('a kept habit earns', habitPoints(tracker('check'), day(1, 1)), HABIT_DEFAULT)
check('a missed habit does not', habitPoints(tracker('check'), undefined), 0)
check('a deliberate "not done" is not kept', habitPoints(tracker('check'), day(1, 0)), 0)

const drink = tracker('check', { karma: -20 })
check('a habit being broken costs on a day it happens', habitPoints(drink, day(1, 1)), -20)
check('"none today" costs nothing', habitPoints(drink, day(1, 0)), 0)

const tv = tracker('amount', { targets: [{ max: 60, per: 'day', tally: 'value' }] })
check('a limit costs by default', defaultKarma(tv), -HABIT_DEFAULT)
check('under the limit costs nothing', habitPoints(tv, day(1, 45)), 0)
check('the limit is inclusive', habitPoints(tv, day(1, 60)), 0)
check('over the limit costs', habitPoints(tv, day(2, 90)), -HABIT_DEFAULT)

const drinks = tracker('amount', { karma: -20 })
check('a recorded nought costs nothing, whatever the kind', habitPoints(drinks, day(1, 0)), 0)
check('nought minutes is no run', habitPoints(tracker('amount'), day(1, 0)), 0)
check(
  'a weekly limit is a habit being broken',
  defaultKarma(tracker('amount', { targets: [{ max: 7, per: 'week', tally: 'value' }] })),
  -HABIT_DEFAULT,
)
check(
  'a minimum anywhere makes it one being built',
  defaultKarma(
    tracker('amount', { targets: [{ min: 60, max: 120, per: 'week', tally: 'value' }] }),
  ),
  HABIT_DEFAULT,
)

const read = tracker('amount', { targets: [{ min: 30, per: 'day', tally: 'value' }] })
check('short of a daily minimum earns nothing', habitPoints(read, day(1, 10)), 0)
check('a daily minimum reached earns', habitPoints(read, day(1, 30)), HABIT_DEFAULT)

check('a dose earns nothing by default', defaultKarma(tracker('dose')), 0)
check('a symptom earns nothing by default', defaultKarma(tracker('scale')), 0)
check(
  'time on a goal is not counted twice',
  defaultKarma(tracker('amount', { source: { type: 'time' } })),
  0,
)
check(
  'a setting is held to the limit',
  habitPoints(tracker('check', { karma: 1000 }), day(1, 1)),
  HABIT_LIMIT,
)

check(
  'each kind is capped',
  scoreDay(
    '2026-11-16',
    Array.from({ length: 20 }, () => deed('task', 10)),
  ).earned,
  40,
)
check(
  'then the day',
  scoreDay('2026-11-16', [
    deed('task', 100),
    deed('habit', 100),
    deed('journal', 10),
    deed('time', 100),
    deed('finished', 100),
  ]).earned,
  DAY_CEILING,
)
const bad = scoreDay('2026-11-16', [deed('task', 10), deed('habit', -50), deed('habit', -50)])
check('losses are never capped', [bad.earned, bad.lost, bad.net], [10, -100, -90])
check('gains are listed first', bad.deeds[0].points > 0, true)

check('a day of nothing is the neutral square', karmaLevel(0), 0)
check('a little earned is the first step', karmaLevel(5), 1)
check('a well-lived day is the top step', karmaLevel(100), 4)
check('a loss steps the other way', karmaLevel(-60), -3)
check('signed numbers use a real minus', [signed(5), signed(-20), signed(0)], ['+5', '−20', '0'])

const net = (date, n) => ({ date, earned: Math.max(n, 0), lost: Math.min(n, 0), net: n, deeds: [] })
const days = [
  net('2026-11-01', 40),
  net('2026-11-10', -20),
  net('2026-11-12', 15),
  net('2026-11-13', 10),
  net('2026-11-14', 0),
  net('2026-11-15', 25),
  net('2026-11-16', 5),
]
// 2026-11-16 is a Monday.
check(
  'the week runs from a Monday start through today',
  karmaSummary(days, '2026-11-16', 1).week,
  5,
)
check('a Sunday start takes in yesterday too', karmaSummary(days, '2026-11-16', 0).week, 30)
check('a Saturday start takes in the whole weekend', karmaSummary(days, '2026-11-16', 6).week, 30)
check('the best day is the best net', karmaSummary(days, '2026-11-16', 1).best, 40)
check('a day of nothing ends a streak', karmaSummary(days, '2026-11-16', 1).streak, 2)
check(
  'a streak is not broken while today has nothing yet',
  karmaSummary(days.slice(0, -1), '2026-11-16', 1).streak,
  1,
)
check(
  'no good day yet has no best day',
  karmaSummary([net('2026-11-16', -5)], '2026-11-16', 1).best,
  null,
)
check('nothing at all', karmaSummary([], '2026-11-16', 1), { week: 0, best: null, streak: 0 })

await close()
finish('karma')
