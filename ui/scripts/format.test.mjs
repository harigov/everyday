// How often a date formatter is built, and whether the cache can go stale.
//
// `Intl` objects are constructed, not looked up: making one resolves a
// locale, loads its data and compiles a pattern, and it costs some eighty
// times what formatting a date with the result does. Every function in
// `format.ts` used to make a fresh one per call, which put that price on
// every row of every list — and on the typing path, because the notes
// editor's status bar reads `updatedAt` and that moves on every keystroke.
//
// So they are kept. Both halves of that are checked here, and the second
// matters more than the first: a cache keyed on the options alone would go on
// formatting in the language the window started in, which is a bug nobody in
// one locale would ever see.

import assert from 'node:assert/strict'
import { createServer } from 'vite'

globalThis.window ??= globalThis
globalThis.location ??= new URL('http://localhost/')

// `locale()` reads `navigator.language`, and the last check below moves it.
// Node's own `navigator` is a getter-only property, so it is redefined rather
// than assigned.
function speaks(language) {
  Object.defineProperty(globalThis, 'navigator', {
    value: { userAgent: 'node', language },
    configurable: true,
  })
}
speaks('en-GB')

// Counted before the module is loaded, so the formatters it makes are made
// through these. Everything else about them is the real thing.
const RealDateTimeFormat = Intl.DateTimeFormat
const RealRelativeTimeFormat = Intl.RelativeTimeFormat
let built = 0
Intl.DateTimeFormat = function (...args) {
  built += 1
  return new RealDateTimeFormat(...args)
}
Intl.RelativeTimeFormat = function (...args) {
  built += 1
  return new RealRelativeTimeFormat(...args)
}

const server = await createServer({
  configFile: false,
  root: new URL('..', import.meta.url).pathname,
  server: { middlewareMode: true, watch: null },
  appType: 'custom',
  logLevel: 'error',
})

const { formatInstantTime, friendlyDate, longDate, relativeTime, weekdayShort } =
  await server.ssrLoadModule('/src/lib/format.ts')

// ── Built once, however often it is asked ─────────────────────────────
//
// A hundred rows of a list, or a hundred characters typed into a note: what
// the number must not do is track the number of calls.

const day = '2026-03-04'
const stamp = '2026-03-04T09:00:00.000Z'

// The first of each pays for its formatter; the rest are free.
weekdayShort(day)
longDate(day)
relativeTime(stamp)

built = 0
for (let i = 0; i < 100; i++) {
  weekdayShort(day)
  longDate(day)
  relativeTime(stamp)
}
assert.equal(built, 0, `three hundred calls built ${built} formatters; they should all be cached`)

// ── ...and the answers are still right ────────────────────────────────
//
// Checked against a formatter built on the spot rather than against a literal
// string, and that is not squeamishness: the exact rendering is ICU's, it
// moves between versions, and it moved between two the repository is expected
// to build on -- `Wednesday 4 March 2026` under ICU 75, `Wednesday, 4 March
// 2026` under 77. A literal here does not test the cache, it tests which
// Node happens to be on the path. What the cache actually promises is that a
// kept formatter says what a fresh one would, and that is what is asked.

const asFreshlyBuilt = (options, at) => new RealDateTimeFormat('en-GB', options).format(at)
// The `.replace` mirrors `weekdayShort`, which strips the full stop some
// locales put on an abbreviation.
const freshWeekday = (at) => asFreshlyBuilt({ weekday: 'short' }, at).replace('.', '')
const LONG = { weekday: 'long', day: 'numeric', month: 'long', year: 'numeric' }

assert.equal(weekdayShort(day), freshWeekday(new Date(2026, 2, 4)))
assert.equal(longDate(day), asFreshlyBuilt(LONG, new Date(2026, 2, 4)))
assert.equal(friendlyDate(new Date().toISOString().slice(0, 10)), 'Today')

// A second date through the same kept formatter, because a cache that handed
// out the right object once could still be handing out the same *answer*.
assert.notEqual(weekdayShort('2026-03-05'), weekdayShort(day))
assert.equal(weekdayShort('2026-03-05'), freshWeekday(new Date(2026, 2, 5)))

// ── A cache that would go stale is not a cache ────────────────────────
//
// `locale()` is `navigator.language`, which is not a constant: a machine
// whose language is changed under a running window has to be formatted in the
// new one. Keying on the options alone would silently keep the old.

const british = longDate(day)
speaks('de-DE')
const german = longDate(day)
assert.notEqual(german, british, `the locale changed and the formatting did not: ${german}`)

// ...and going back gets the first one, from the cache, unchanged.
speaks('en-GB')
assert.equal(longDate(day), british, 'switching back must give the same formatting as before')

// The same for the zone, which a formatter resolves at construction and never
// revisits. A laptop is carried between them; a window stays open for days.
// Built in London and asked about 09:00Z, a stale formatter goes on saying
// 9:00 in Tokyo, where the answer is 18:00.
const nineAm = '2026-03-04T09:00:00.000Z'
const before = process.env.TZ
// Both zones named outright rather than one of them being whatever the machine
// happens to be set to, which on a machine in Tokyo would compare a thing with
// itself and pass regardless.
process.env.TZ = 'Europe/London'
const inLondon = formatInstantTime(nineAm)
process.env.TZ = 'Asia/Tokyo'
const inTokyo = formatInstantTime(nineAm)
assert.notEqual(inTokyo, inLondon, `the time zone changed and the clock did not: ${inTokyo}`)
assert.equal(
  inTokyo,
  new RealDateTimeFormat('en-GB', { hour: 'numeric', minute: '2-digit' }).format(new Date(nineAm)),
  'the rebuilt formatter does not agree with a fresh one in the new zone',
)

// ...and home again, which comes from the cache rather than being built afresh.
process.env.TZ = 'Europe/London'
assert.equal(formatInstantTime(nineAm), inLondon, 'coming home must read as it did before')

if (before === undefined) delete process.env.TZ
else process.env.TZ = before

await server.close()
console.log('format: all checks passed')
