// Behaviour checks for the quick-add parser.
//
// The rest of the interface is checked by the type checker and by the fact
// that it compiles. This one module is different: it is a small grammar with
// real semantics, it is the thing standing between someone typing and their
// task being right, and every one of its rules is a decision that a later
// change could silently reverse. So it gets tests.
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

const { parseQuickAdd, parseDueDate, parseTime, parseEstimate } =
  await server.ssrLoadModule('/src/lib/quickadd.ts')

// A fixed Sunday, so "next Wednesday" is a fact rather than a coin toss.
const TODAY = '2026-09-06'
const line = (s) => parseQuickAdd(s, TODAY)

let failed = 0
function check(what, got, want) {
  if (JSON.stringify(got) === JSON.stringify(want)) return
  failed += 1
  console.error(`FAIL  ${what}\n        got  ${JSON.stringify(got)}\n        want ${JSON.stringify(want)}`)
}

// ── A whole line ─────────────────────────────────────────────────────────

check('a plain line is all title', line('Buy milk').title, 'Buy milk')

const full = line('Book the flights #travel #Work !high ~1h30 @fri @16:30')
check('every field at once', [
  full.title, full.tags, full.priority, full.estimateMinutes, full.dueDate, full.dueTime,
], ['Book the flights', ['travel', 'Work'], 'high', 90, '2026-09-11', '16:30:00'])

check('a status flag', line('Chase the invoice !blocked').status, 'blocked')
check('duplicate tags collapse, keeping the first spelling', line('x #a #A').tags, ['a'])
check('a line of nothing but sigils has no title', line('#tag !high').title, '')

// ── Nothing a person typed may disappear ─────────────────────────────────

check('an unknown flag stays in the title', line('Careful! this is !urgnt').title, 'Careful! this is !urgnt')
check('and does not set a priority', line('!urgnt thing').priority, 'none')
check('a sigil mid-word is not a token', line('Learn C# properly').title, 'Learn C# properly')
check('nor is one inside a range', line('Invite 10~15 people').title, 'Invite 10~15 people')
check('nor one inside an address', line('Reply to ana@example.com').title, 'Reply to ana@example.com')
check('an unparseable date stays put', line('Ship it @sometime').title, 'Ship it @sometime')

// ── Dates ────────────────────────────────────────────────────────────────

check('today and tomorrow', [parseDueDate('today', TODAY), parseDueDate('tmr', TODAY)], [TODAY, '2026-09-07'])
check('a weekday that is today means today', parseDueDate('sun', TODAY), TODAY)
check('a weekday ahead is this coming one', parseDueDate('wednesday', TODAY), '2026-09-09')
check('offsets in days and weeks', [
  parseDueDate('3d', TODAY), parseDueDate('+3', TODAY), parseDueDate('2w', TODAY),
], ['2026-09-09', '2026-09-09', '2026-09-20'])
check('an offset crosses a month end', parseDueDate('30d', TODAY), '2026-10-06')
check('an explicit date', parseDueDate('2026-12-25', TODAY), '2026-12-25')
check('a date that does not exist is refused, not rolled forward', parseDueDate('2026-02-31', TODAY), null)
check('an ambiguous date is refused outright', parseDueDate('12/09', TODAY), null)

// ── Times ────────────────────────────────────────────────────────────────

check('clock and meridiem times', [
  parseTime('16:30'), parseTime('9am'), parseTime('12am'), parseTime('12pm'), parseTime('9:30pm'),
], ['16:30:00', '09:00:00', '00:00:00', '12:00:00', '21:30:00'])
check('a bare number is an offset, never an hour', parseTime('3'), null)
check('impossible clock times are refused', [parseTime('25:00'), parseTime('13pm')], [null, null])

const timed = line('Standup @9am')
check('a time with no date means today', [timed.dueDate, timed.dueTime], [TODAY, '09:00:00'])

// ── Estimates ────────────────────────────────────────────────────────────

check('every estimate spelling', [
  parseEstimate('90'), parseEstimate('90m'), parseEstimate('2h'),
  parseEstimate('1h30'), parseEstimate('1h30m'),
], [90, 90, 120, 90, 90])
check('an unparseable estimate is refused', parseEstimate('soon'), null)

await server.close()

if (failed > 0) {
  console.error(`\n${failed} quick-add ${failed === 1 ? 'check' : 'checks'} failed`)
  process.exit(1)
}
console.log('quick-add: all checks passed')
