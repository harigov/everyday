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

const { friendlyDate, longDate, relativeTime, weekdayShort } =
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
// A shared formatter is only worth having if it formats. These are the three
// shapes the interface actually draws: a row's weekday, an entry's date, and
// the word a reader expects instead of today's date.

assert.equal(weekdayShort(day), 'Wed')
assert.equal(longDate(day), 'Wednesday, 4 March 2026')
assert.equal(friendlyDate(new Date().toISOString().slice(0, 10)), 'Today')
// The hundred calls above went through the same objects as these, so this is
// also what says the cache did not hand out a formatter built for something
// else: every one of them agreed.
assert.equal(weekdayShort('2026-03-05'), 'Thu')

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

await server.close()
console.log('format: all checks passed')
