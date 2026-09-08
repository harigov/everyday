// Behaviour checks for the two parts of the library that are made of
// decisions rather than of types.
//
// The same reasoning as the three test files beside this one. Most of the
// library is checked by the type checker and by the fact that it compiles;
// the storage rules, the query filters and the metadata merge are all
// checked in Rust, where they live. What is left on this side, and what is
// here, is:
//
//   `rating.ts`     the conversion between the stored 0-100 scale and the
//                   five stars the control draws. Its bug is silent -- four
//                   and a half stars quietly becoming four on reload -- and
//                   it is the sort of thing a later "simplification" to
//                   `Math.round` would break without failing to build.
//
//   `LiveSearch`    the race guard in `websearch.ts`. Its bug is worse than
//                   silent: the answer to "dun" arriving after the answer to
//                   "dune" and *winning*, so the suggestion list settles on
//                   the wrong thing and stays there. Nothing about that is a
//                   type error, and it is invisible on a fast network.
//
// No test framework, deliberately -- one dependency-free file, run by
// `npm run check`. The TypeScript is loaded through Vite so it is compiled
// exactly as the application compiles it.

import assert from 'node:assert/strict'
import { createServer } from 'vite'

// `websearch.ts` imports `api.ts`, which decides at module scope whether it
// is talking to Tauri or to the mock -- by looking in `window` -- and then
// loads the mock, which reads `?unlocked` off the address bar. Neither
// exists in Node, so both are lent to it, and that is the whole of the shim.
//
// Nothing below touches `api` or the mock. `LiveSearch` is handed its
// searcher, which is exactly why it takes one rather than reaching for the
// singleton: the class can be exercised without a backend of any kind.
globalThis.window ??= globalThis
globalThis.location ??= new URL('http://localhost/')

const server = await createServer({
  configFile: false,
  root: new URL('..', import.meta.url).pathname,
  server: { middlewareMode: true },
  appType: 'custom',
  logLevel: 'error',
})

const { MAX_STARS, fromStars, ratingLabel, ratingTitle, starFill, stars } =
  await server.ssrLoadModule('/src/lib/rating.ts')

// ── ratings ───────────────────────────────────────────────────────────

// Every half-star position survives the round trip through storage. This is
// the whole contract: set four and a half stars, reload, and the control is
// still on four and a half rather than having drifted.
for (let tenth = 0; tenth <= MAX_STARS * 2; tenth++) {
  const want = tenth / 2
  assert.equal(stars(fromStars(want)), want, `${want} stars did not survive the round trip`)
}

// Matches `library::stars` in the core, which is what actually stores them.
assert.equal(stars(100), 5)
assert.equal(stars(0), 0)
assert.equal(stars(90), 4.5)
// Somebody else's 82% lands on the nearest half we can draw.
assert.equal(stars(82), 4)
assert.equal(stars(85), 4.5)

// Out-of-range input is clamped rather than drawn as six stars or as a
// negative one. A score off a source that overshot its own scale is the
// realistic way this arrives.
assert.equal(stars(140), 5)
assert.equal(stars(-20), 0)
assert.equal(fromStars(9), 100)
assert.equal(fromStars(-1), 0)

// A whole number prints as a whole number. `toFixed(1)` everywhere would
// print "4.0", which reads as a precision nobody claimed.
assert.equal(ratingLabel(80), '4')
assert.equal(ratingLabel(90), '4.5')
assert.equal(ratingTitle(null), 'Not rated')
assert.equal(ratingTitle(20), '1 out of 5 stars')
assert.equal(ratingTitle(90), '4.5 out of 5 stars')

// A half star is drawn as half a star: a fraction per glyph, not a boolean.
// Without this the row would need a second glyph and would stop looking like
// one row.
assert.deepEqual(
  [1, 2, 3, 4, 5].map((n) => starFill(90, n)),
  [1, 1, 1, 1, 0.5],
)
assert.deepEqual(
  [1, 2, 3, 4, 5].map((n) => starFill(0, n)),
  [0, 0, 0, 0, 0],
)

// ── the search race guard ─────────────────────────────────────────────

const { LiveSearch, SEARCH_DEBOUNCE_MS } = await server.ssrLoadModule('/src/lib/websearch.ts')

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
const outcome = (query, results) => ({ query, results, error: null })

/** A fake `WebSearch` whose answers can be made to arrive out of order. */
function slowSearch(delays) {
  return {
    search: async (query) => {
      await sleep(delays[query] ?? 0)
      return outcome(query, [{ title: query }])
    },
    lookup: async (_kind, query) => {
      await sleep(delays[query] ?? 0)
      return outcome(query, [{ title: query }])
    },
  }
}

// The bug this class exists to prevent: a slow answer to "dun" landing after
// the fast answer to "dune", and the list ending up showing sand dunes.
{
  const seen = []
  const web = slowSearch({ dun: 120, dune: 5 })
  const live = new LiveSearch(web, (o) => {
    if (!o.searching) seen.push(o.results[0]?.title ?? null)
  })

  live.type('dun')
  await sleep(SEARCH_DEBOUNCE_MS / 3)
  live.type('dune')
  await sleep(SEARCH_DEBOUNCE_MS + 250)

  assert.deepEqual(seen, ['dune'], 'a stale answer must never reach the list')
}

// Typing quickly costs one request, not one per keystroke.
{
  let calls = 0
  const web = {
    search: async (query) => {
      calls++
      return outcome(query, [])
    },
    lookup: async () => outcome('', []),
  }
  const live = new LiveSearch(web, () => {})
  for (const q of ['d', 'du', 'dun', 'dune']) {
    live.type(q)
    await sleep(SEARCH_DEBOUNCE_MS / 6)
  }
  await sleep(SEARCH_DEBOUNCE_MS + 60)
  assert.equal(calls, 1, `four keystrokes inside the debounce made ${calls} requests`)
}

// Clearing the field clears the list at once. Waiting out the debounce to
// show nothing is the one case where the delay is felt purely as lag.
{
  const seen = []
  const live = new LiveSearch(slowSearch({}), (o) => seen.push(o))
  live.type('')
  assert.equal(seen.length, 1)
  assert.equal(seen[0].searching, false, 'an empty field must not read as searching')
  assert.deepEqual(seen[0].results, [])
}

// `stop` means stop: an answer already in flight is ignored rather than
// rendered into a component that has been unmounted.
{
  const seen = []
  const live = new LiveSearch(slowSearch({ dune: 80 }), (o) => {
    if (!o.searching) seen.push(o)
  })
  live.type('dune')
  await sleep(SEARCH_DEBOUNCE_MS + 20)
  live.stop()
  await sleep(200)
  assert.deepEqual(seen, [], 'a stopped search must not report')
}

// A failing search reports the reason rather than rejecting: every caller of
// this wants to put the message on screen beside an empty list.
{
  let got = null
  const web = {
    search: async () => {
      throw new Error('the network is off')
    },
    lookup: async () => outcome('', []),
  }
  const live = new LiveSearch(web, (o) => {
    if (!o.searching) got = o
  })
  live.type('dune')
  await sleep(SEARCH_DEBOUNCE_MS + 80)
  assert.ok(got, 'a failure must still report')
  assert.match(got.error, /network is off/)
  assert.deepEqual(got.results, [])
}

// ── the two predicates the filters are built from ─────────────────────
//
// `library.svelte.ts` itself is not loaded here: it is a runes module, and
// this bare Vite server has no Svelte compiler. What its filters are made of
// is these two functions, and they must agree with `ItemStatus::is_open` and
// `LogEvent::is_completion` in the core -- the counts in the sidebar come
// from Rust and the filter that populates the grid comes from here, so a
// disagreement shows up as a shelf whose badge says three and whose grid
// shows four.

const { ITEM_STATUSES, LOG_EVENTS, isAhead, isCompletion } =
  await server.ssrLoadModule('/src/lib/types.ts')

assert.deepEqual(ITEM_STATUSES.filter(isAhead), ['wishlist', 'active', 'paused'])
assert.ok(!isAhead('done'), 'a finished thing is not still ahead of you')
assert.ok(!isAhead('abandoned'), 'nor is one you gave up on')

// Both ways of getting to the end of something count, and nothing else does
// -- this is what "eleven finished this year" is a count of.
assert.deepEqual(LOG_EVENTS.filter(isCompletion), ['finished', 'revisited'])
assert.ok(!isCompletion('started'), 'starting something is not finishing it')
assert.ok(!isCompletion('progress'))

await server.close()
console.log('library: all checks passed')
