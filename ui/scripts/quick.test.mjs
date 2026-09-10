// Behaviour checks for the quick model's interface half.
//
// Everything this module does fails *quietly* by design -- a suggestion that
// does not appear is the feature working slightly less well, not an error --
// which is exactly why it needs a test. The three rules:
//
//   switched off  a job the policy has not allowed must not reach the
//                 network at all. The Rust checks too, and that is what makes
//                 the guarantee; this is what stops the request being made.
//
//   staleness     open a note, open another before the first suggestion
//                 lands, and the first note's title must not arrive beside
//                 the second. That is not a flicker -- it is a wrong title
//                 offered for a record it was never about, one tap from being
//                 applied.
//
//   dismissal     closing a suggestion has to stick against a request that
//                 was already in flight, or the chip somebody just closed
//                 comes back a moment later.

import assert from 'node:assert/strict'
import { createServer } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

// Enough of a browser for the stores this module imports to evaluate. Same
// reasoning as `live.test.mjs`.
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

const server = await createServer({
  configFile: false,
  root: new URL('..', import.meta.url).pathname,
  plugins: [svelte()],
  server: { middlewareMode: true, watch: null },
  appType: 'custom',
  logLevel: 'error',
})

const { ask, quick, slot } = await server.ssrLoadModule('/src/lib/quick.svelte.ts')

/** A promise that resolves when we say so. */
function deferred() {
  let resolve
  const promise = new Promise((r) => (resolve = r))
  return { promise, resolve }
}

const flush = () => new Promise((r) => setTimeout(r, 0))

// ── A job that is off never reaches the network ──────────────────────────

{
  quick.configured = false
  quick.jobs = [{ name: 'notes.title', label: '', blurb: '', app: '', on: true, defaultOn: true }]

  let called = 0
  const answer = await ask('notes.title', async () => {
    called += 1
    return 'a title'
  })
  assert.equal(answer, null, 'no quick model configured means no answer')
  assert.equal(called, 0, 'and no request either')

  // Configured, but this particular job switched off.
  quick.configured = true
  quick.jobs = [{ name: 'notes.title', label: '', blurb: '', app: '', on: false, defaultOn: true }]
  called = 0
  assert.equal(await ask('notes.title', async () => ((called += 1), 'x')), null)
  assert.equal(called, 0, 'a job switched off must not be asked')

  // A name this build does not have. Refused rather than run: the policy
  // cannot vouch for a job it cannot look up.
  called = 0
  assert.equal(await ask('notes.invent', async () => ((called += 1), 'x')), null)
  assert.equal(called, 0)
}

// ── A failure is silence, not an error ───────────────────────────────────

{
  quick.configured = true
  quick.jobs = [{ name: 'notes.title', label: '', blurb: '', app: '', on: true, defaultOn: true }]

  // A model endpoint that is down must not become a banner over somebody's
  // writing. It answers null and the chip simply does not appear.
  const answer = await ask('notes.title', async () => {
    throw new Error('connection refused')
  })
  assert.equal(answer, null, 'a failed job answers null rather than throwing')
}

// ── The answer to a stale question is dropped ────────────────────────────

{
  quick.configured = true
  quick.jobs = [{ name: 'notes.title', label: '', blurb: '', app: '', on: true, defaultOn: true }]

  const first = deferred()
  const second = deferred()
  const s = slot()
  const seen = []

  void s.run(
    'notes.title',
    () => first.promise,
    (v) => seen.push(v),
  )
  void s.run(
    'notes.title',
    () => second.promise,
    (v) => seen.push(v),
  )

  // The *first* note's answer comes back last, which is the race this exists
  // for: without the generation counter it would win, and the title of the
  // note somebody has closed would be offered for the one they are reading.
  second.resolve('the second note')
  await flush()
  first.resolve('the first note')
  await flush()

  assert.deepEqual(seen, ['the second note'], 'only the newest question is answered')
}

// ── A dismissal sticks against a request already in flight ───────────────

{
  quick.configured = true
  quick.jobs = [{ name: 'notes.title', label: '', blurb: '', app: '', on: true, defaultOn: true }]

  const inflight = deferred()
  const s = slot()
  const seen = []

  void s.run(
    'notes.title',
    () => inflight.promise,
    (v) => seen.push(v),
  )
  s.dismiss((v) => seen.push(v))

  // Somebody closed the row. The request had already gone.
  inflight.resolve('a title nobody wants now')
  await flush()

  assert.deepEqual(seen, [null], 'the dismissal is the only thing that lands')
}

// ── Cancelling drops what is in flight without clearing what is drawn ────

{
  quick.configured = true
  quick.jobs = [{ name: 'notes.title', label: '', blurb: '', app: '', on: true, defaultOn: true }]

  const inflight = deferred()
  const s = slot()
  const seen = []

  void s.run(
    'notes.title',
    () => inflight.promise,
    (v) => seen.push(v),
  )
  s.cancel()
  inflight.resolve('too late')
  await flush()

  assert.deepEqual(seen, [], 'a cancel is silent in both directions')
}

await server.close()
console.log('quick: all checks passed')
