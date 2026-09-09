// Behaviour checks for the live-refresh router.
//
// Three rules, each of which fails quietly rather than loudly.
//
//   origin        a window must not reload for its own write, or the list
//                 moves under the cursor that caused it.
//
//   coalescing    a calendar sync writes a thousand events and each one
//                 announces itself. One reload, not a thousand.
//
//   dedupe        several kinds in one batch reload the *same* store -- a task
//                 and a project, an item and a log row -- and running it twice
//                 is a wasted round trip on a connection that may be a
//                 phone's. This was broken: the map held a separate arrow
//                 function per kind, so comparing them never matched.
//
// The module reaches into all four stores, so this checks the routing table
// rather than the stores: what matters is which target a kind asks for and how
// many distinct targets a batch produces.

import assert from 'node:assert/strict'
import { createServer } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

// The router imports the four stores, and they were written for a browser.
// Enough of one to let the modules evaluate; nothing below runs a reload, so
// nothing depends on the stubs being faithful. Same reasoning as
// `actions.test.mjs`.
globalThis.window ??= {}
globalThis.localStorage ??= { getItem: () => null, setItem: () => {}, removeItem: () => {} }
globalThis.matchMedia ??= () => ({
  matches: false,
  addEventListener: () => {},
  removeEventListener: () => {},
})
globalThis.document ??= {
  querySelector: () => null,
  documentElement: { style: { setProperty: () => {} }, classList: { toggle: () => {} } },
  addEventListener: () => {},
}
globalThis.navigator ??= { userAgent: 'node' }
globalThis.location ??= { search: '', href: 'http://localhost/' }
globalThis.URL.createObjectURL ??= () => 'blob:stub'

// The Svelte plugin by name rather than the project config, and no watcher.
// See the same note in `actions.test.mjs`.
const server = await createServer({
  configFile: false,
  root: new URL('..', import.meta.url).pathname,
  plugins: [svelte()],
  server: { middlewareMode: true, watch: null },
  appType: 'custom',
  logLevel: 'error',
})

const { RELOAD, RELOADS, targetsFor } = await server.ssrLoadModule('/src/lib/live.svelte.ts')

// ── Every kind has an answer, and it is one that exists ───────────────

const KINDS = [
  'journal',
  'entry',
  'project',
  'task',
  'block',
  'calendar',
  'event',
  'shelf',
  'item',
  'log',
  'tracker',
  'reading',
  'role',
  'goal',
  'conversation',
  'memory',
  'settings',
]

for (const kind of KINDS) {
  assert.ok(kind in RELOADS, `no routing for a change of kind ${kind}`)
  const target = RELOADS[kind]
  if (target === null) continue
  assert.ok(target in RELOAD, `${kind} asks for ${target}, which is not a reload`)
}

// ...and nothing routes to a target nobody declared.
for (const [kind, target] of Object.entries(RELOADS)) {
  assert.ok(KINDS.includes(kind), `${kind} is routed and is not a change kind`)
  assert.ok(target === null || target in RELOAD, `${kind} -> ${target}`)
}

// ── A batch collapses to one reload per store ─────────────────────────

// The regression this file exists for. A task and a project both reload the
// todo app; before the fix these were two different arrow literals and the
// dedupe compared them by identity, so both ran.
assert.deepEqual(targetsFor(['task', 'project']), ['todo'])
assert.deepEqual(targetsFor(['item', 'log']), ['library'])
assert.deepEqual(targetsFor(['block', 'calendar', 'event']), ['calendar'])
assert.deepEqual(targetsFor(['tracker', 'reading']), ['tracking'])
// The fifth app's two records, which both mean "redraw every chart".
assert.deepEqual(targetsFor(['role', 'goal']), ['overview'])

// Distinct stores stay distinct.
assert.deepEqual(new Set(targetsFor(['task', 'item'])), new Set(['todo', 'library']))

// A kind that deliberately reloads nothing contributes nothing.
assert.deepEqual(targetsFor(['conversation', 'memory']), [])
assert.deepEqual(targetsFor(['conversation', 'task']), ['todo'])

// ── A purpose changes the reports without naming them ────────────────
//
// A purpose is a pointer on a task, a block, an entry, a shelf item and a
// tracker, and it inherits -- so filing one task under a goal changes every
// chart the Overview draws, while the change event says only "task".
//
// The Overview therefore reloads for those kinds, but only while it is the app
// on screen: doing it always would redraw a year of balance on every
// autosave.
assert.deepEqual(targetsFor(['task']), ['todo'], 'the Overview is not showing')
assert.deepEqual(
  new Set(targetsFor(['task'], true)),
  new Set(['todo', 'overview']),
  'a task filed under a goal changes the charts',
)
// And a kind that cannot carry one does not drag the Overview along.
assert.deepEqual(targetsFor(['memory'], true), [])
assert.deepEqual(targetsFor(['journal'], true), ['journals'])
// Its own records still reload it either way.
assert.deepEqual(targetsFor(['goal'], false), ['overview'])
// One reload, not two, when a batch has both kinds in it.
assert.deepEqual(
  targetsFor(['goal', 'task'], true).filter((t) => t === 'overview'),
  ['overview'],
)

// A whole sync's worth of kinds is still a handful of reloads, not a hundred.
const everything = targetsFor([...KINDS, ...KINDS, ...KINDS], true)
assert.equal(new Set(everything).size, everything.length, 'a target ran twice')
assert.ok(everything.length <= Object.keys(RELOAD).length)

await server.close()
console.log('live: all checks passed')
