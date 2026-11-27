// Behaviour checks for `assistant.svelte.ts`'s answer to `routine` and
// `routineRun` changes from elsewhere -- the store's own `apply`, which the
// live router hands a batch before it reloads anything.
//
// Driven through the real mock backend, as `jobs-store.test.mjs` is, with
// the calls that matter counted: what is worth checking is which round trips
// a change costs, not what the rows say.
//
//   off screen    nothing draws the lists but the count on the bar (and
//                 the Overview's "Needs attention", what it remembers), so
//                 that is all that is read.
//   on screen     the run a change names is read again, alone; a batch too
//                 wide to patch, or a log nothing has read yet, asks for the
//                 whole log.
//   slim          the log is asked for without its briefs.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

stubBrowser({
  window: globalThis,
  localStorage: true,
  matchMedia: true,
  document: {
    querySelector: () => null,
    documentElement: {
      style: { setProperty: () => {} },
      classList: { toggle: () => {} },
      setAttribute: () => {},
      removeAttribute: () => {},
    },
    addEventListener: () => {},
  },
  navigator: { userAgent: 'node', language: 'en-GB' },
  location: new URL('http://localhost/?unlocked=1'),
  createObjectURL: () => 'blob:stub',
})

const {
  modules: [assistantModule, stateModule, apiModule, panelsModule, liveApply],
  close,
} = await load(
  [
    '/src/lib/assistant.svelte.ts',
    '/src/lib/state.svelte.ts',
    '/src/lib/api.ts',
    '/src/lib/panels.svelte.ts',
    '/src/lib/live-apply.ts',
  ],
  { svelte: true },
)
const { assistant } = assistantModule
const { app } = stateModule
const { api } = apiModule
const { panels } = panelsModule

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
// Long enough for the mock's latency (40 ms a call) on whatever a change
// sets off, several calls deep.
const settle = () => sleep(250)

// Every call the store makes for these lists, counted, with what it asked.
const calls = { runs: [], run: [], unseen: 0, routines: 0 }
const real = {
  runs: api.runs,
  run: api.run,
  unseenRuns: api.unseenRuns,
  routines: api.routines,
}
api.runs = (...args) => (calls.runs.push(args), real.runs(...args))
api.run = (id) => (calls.run.push(id), real.run(id))
api.unseenRuns = () => (calls.unseen++, real.unseenRuns())
api.routines = () => (calls.routines++, real.routines())
function forget() {
  calls.runs = []
  calls.run = []
  calls.unseen = 0
  calls.routines = 0
}

const run = (op, id, extra = {}) => ({ kind: 'routineRun', op, id, origin: 'remote', ...extra })

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')
assert.equal(app.supportsRoutines, true)

const apply = liveApply.applierFor('assistant')
assert.ok(apply, 'the assistant store registers an apply')

// ── before anything has read the lists ───────────────────────────────

// Off screen: only the count on the bar.
app.section = 'journal'
panels.settings = null
assert.equal(apply([run('updated', 'run-1')]), true)
await settle()
assert.deepEqual(calls.run, [], 'nothing on screen draws the log, so no run is read')
assert.deepEqual(calls.runs, [])
assert.equal(calls.unseen, 1, 'only the count on the bar')
forget()

// On screen, where the Assistant app names a routine's transcript from a
// log nothing has read yet: the router reads it whole, as it always did.
app.section = 'assistant'
assert.equal(apply([run('updated', 'run-1')]), false)
assert.deepEqual(calls.run, [])
forget()

// ── the log is read without its briefs ───────────────────────────────

await assistant.start()
assert.equal(calls.runs.length, 1)
assert.equal(calls.runs[0][1], true, 'the log asks for slim runs')
assert.ok(assistant.runs.some((r) => r.id === 'run-1'))
forget()

// ── off screen ───────────────────────────────────────────────────────

app.section = 'journal'
panels.settings = null
assert.equal(apply([run('updated', 'run-1')]), true)
assert.equal(apply([run('updated', null, { ids: ['run-1', 'run-2'] })]), true, 'however wide')
await settle()
assert.deepEqual(calls.run, [], 'no tab on screen, so no run is read')
assert.deepEqual(calls.runs, [])
assert.equal(calls.routines, 0)
assert.equal(calls.unseen, 2, 'only the count, each time')
forget()

// The Overview's "Needs attention" draws what a dream noticed, so what it
// remembers is read again there -- and of the rest, still only the count.
{
  let memoriesRead = 0
  const realMemories = api.memories
  api.memories = () => (memoriesRead++, realMemories())
  app.section = 'overview'
  assert.equal(apply([run('updated', 'run-1')]), true)
  await settle()
  assert.equal(memoriesRead, 1, 'what it remembers, for "Needs attention"')
  assert.deepEqual(calls.run, [])
  assert.deepEqual(calls.runs, [])
  assert.equal(calls.routines, 0)
  assert.equal(calls.unseen, 1)
  api.memories = realMemories
  app.section = 'journal'
  forget()
}

// ── on screen: Settings → What it did ────────────────────────────────

panels.settings = 'runs'
const row = () => assistant.runs.find((r) => r.id === 'run-1')
assert.equal(row().seen, false)
await api.markRunsSeen(['run-1'])
assert.equal(apply([run('updated', 'run-1'), run('updated', 'run-1')]), true)
await settle()
assert.deepEqual(calls.run, ['run-1'], 'the one run is read again, once')
assert.deepEqual(calls.runs, [], 'and not the log')
assert.equal(calls.routines, 1, 'the routines move with a run: its next time')
assert.equal(calls.unseen, 1)
assert.equal(row().seen, true, 'patched in place')
assert.equal(row().brief ?? null, null, 'without a brief, like the rest of the log')
forget()

const count = assistant.runs.length
assert.equal(apply([run('deleted', 'run-2')]), true)
await settle()
assert.equal(
  assistant.runs.find((r) => r.id === 'run-2'),
  undefined,
  'deleted elsewhere, gone here',
)
assert.equal(assistant.runs.length, count - 1)
assert.deepEqual(calls.run, [], 'nothing to read for a run that is gone')
forget()

// A routine on its own: the routines again, no run.
assert.equal(apply([{ kind: 'routine', op: 'updated', id: 'ro-brief', origin: 'remote' }]), true)
await settle()
assert.equal(calls.routines, 1)
assert.deepEqual(calls.run, [])
forget()

// Too wide to patch: the router reloads the whole log instead.
assert.equal(apply([run('updated', null, { ids: ['run-1', 'run-2'] })]), false)
assert.equal(apply([run('updated', null)]), false, 'a run change naming none')
const many = ['a', 'b', 'c', 'd'].map((id) => run('updated', id))
assert.equal(apply(many), false, 'more than a few')

// ...and the Assistant app counts as on screen, since it names a routine's
// transcript from the log.
panels.settings = null
app.section = 'assistant'
assert.equal(apply([run('updated', 'run-1')]), true)
await settle()
assert.deepEqual(calls.run, ['run-1'])

Object.assign(api, real)
await close()
console.log('assistant-store: all checks passed')

// Unlocking arms the app's idle-vault poll; see `jobs-store.test.mjs`.
process.exit(0)
