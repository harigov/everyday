// Behaviour checks for how the Overview draws mail and meetings.
//
// The counting is in Rust and tested there (`everyday_core::insights`). What
// is left here is the drawing, and both of its rules fail quietly:
//
//   bucketing       a quarter drawn a column a day is a comb of ninety
//                   slivers; a week drawn a column a week is one bar. Neither
//                   throws, and both look like a chart.
//   joining people  the mail list knows addresses and the meeting list often
//                   only names. Joined carelessly, one colleague is two rows
//                   half as high as they should be -- or two Sams become one.
//
// No test framework, deliberately: one dependency-free file, run by
// `npm run check`, with the TypeScript loaded through Vite so it compiles
// exactly as the application compiles it.

import { load, makeCheck } from './harness.mjs'

const { module: insights, close } = await load('/src/lib/insights.ts')
const { DAILY_UP_TO, columnUnit, columns, inTouch, machineTag, touchpoints } = insights

const { check, ok, finish } = makeCheck()

/** `n` consecutive days from `first`, each worth `value(i)`. */
function days(first, n, value = () => 1) {
  const out = []
  const at = new Date(`${first}T00:00:00`)
  for (let i = 0; i < n; i++) {
    const pad = (x) => String(x).padStart(2, '0')
    out.push({
      date: `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}`,
      n: value(i),
    })
    at.setDate(at.getDate() + 1)
  }
  return out
}

// ── columns ─────────────────────────────────────────────────────────────

{
  const week = days('2026-09-28', 7, (i) => i)
  check(
    'a week is a column a day',
    columns(week, (d) => d.n, 1).map((c) => c.value),
    [0, 1, 2, 3, 4, 5, 6],
  )
  check('...dated by the day', columns(week, (d) => d.n, 1)[0].date, '2026-09-28')

  const month = days('2026-09-06', DAILY_UP_TO)
  check('a month is still a column a day', columns(month, (d) => d.n, 1).length, DAILY_UP_TO)

  // 2026-07-08 is a Wednesday. With weeks starting Monday, the first column
  // is the five days of that week the window holds, dated by its Monday --
  // not padded out with two days nobody asked about.
  const quarter = days('2026-07-08', 90)
  const weeks = columns(quarter, (d) => d.n, 1)
  check('a quarter is a column a week', weeks.length, 14)
  check('...the first dated by its Monday', weeks[0].date, '2026-07-06')
  check('...holding only the days in the window', weeks[0].value, 5)
  check(
    '...and nothing is lost between them',
    weeks.reduce((sum, c) => sum + c.value, 0),
    90,
  )
  check(
    'a Sunday-start locale cuts the weeks on Sunday',
    columns(quarter, (d) => d.n, 0)[0].date,
    '2026-07-05',
  )
  check('the unit follows the same line', [columnUnit(30), columnUnit(90)], ['day', 'week'])
}

// ── who is a machine ────────────────────────────────────────────────────

check(
  'only newsletters and notifications wear a tag',
  ['important', 'other', 'newsletter', 'notification', null].map(machineTag),
  [null, null, 'Newsletter', 'Notification', null],
)

// ── one list of people ──────────────────────────────────────────────────

function correspondent(email, name, received, sent) {
  return { email, name, received, sent, last: '2026-10-01T09:00:00Z' }
}

function mail(...correspondents) {
  return { received: 0, sent: 0, unread: 0, days: [], categories: [], senders: [], correspondents }
}

function meetings(...people) {
  return { events: 0, minutes: 0, days: [], titles: [], people, crowded: 0 }
}

{
  const joined = inTouch(
    mail(correspondent('ana@example.com', 'Ana Lima', 4, 2)),
    meetings({ name: 'Ana Lima', email: 'ana@example.com', meetings: 3, minutes: 90 }),
  )
  check('an address on both sides is one person', joined.length, 1)
  check(
    '...with both sides added up',
    [joined[0].received, joined[0].sent, joined[0].meetings],
    [4, 2, 3],
  )
  check('...ranked by everything exchanged', touchpoints(joined[0]), 9)
}

{
  const joined = inTouch(
    mail(correspondent('ana@example.com', 'Ana Lima', 1, 0)),
    meetings({ name: 'ana lima', meetings: 2, minutes: 60 }),
  )
  check('a name the calendar gave alone finds its address', joined.length, 1)
  check('...case aside', joined[0].key, 'ana@example.com')
}

{
  const joined = inTouch(
    mail(
      correspondent('sam@one.example', 'Sam', 1, 0),
      correspondent('sam@two.example', 'Sam', 1, 0),
    ),
    meetings({ name: 'Sam', meetings: 1, minutes: 30 }),
  )
  check('two correspondents sharing a name are not guessed between', joined.length, 3)
}

{
  const joined = inTouch(
    mail(correspondent('ana@example.com', 'ana@example.com', 1, 0)),
    meetings({ name: 'Ana Lima', email: 'ana@example.com', meetings: 1, minutes: 30 }),
  )
  check('the calendar names somebody mail only had an address for', joined[0].name, 'Ana Lima')
}

{
  const joined = inTouch(
    mail(correspondent('a@example.com', 'A', 5, 0), correspondent('b@example.com', 'B', 1, 0)),
    meetings(
      { name: 'B', email: 'b@example.com', meetings: 6, minutes: 360 },
      { name: 'C', meetings: 1, minutes: 30 },
    ),
  )
  check(
    'people are ranked by mail and meetings together',
    joined.map((p) => p.name),
    ['B', 'A', 'C'],
  )
}

check('nothing on either side is nobody', inTouch(null, null), [])
ok(
  'one side alone is enough',
  inTouch(null, meetings({ name: 'C', meetings: 1, minutes: 30 })).length === 1,
)

await close()
finish('insights')
