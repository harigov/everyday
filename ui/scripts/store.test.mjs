// Behaviour checks for the small primitives every app store is built from.
//
// Each of these replaced a pattern that had been copied out by hand into
// three or four stores, with at least one of the copies missing a line the
// others had -- a calendar page that let a stale batch land last, a search
// box that refilled itself after being cleared. The bug in each of those was
// never in the *idea*, it was in a copy that had drifted, so what is worth
// testing here is the same thing the idea promises everywhere it is used.
//
// No test framework, deliberately -- one dependency-free file, run by
// `npm run check`. The TypeScript is loaded through Vite so it is compiled
// exactly as the application compiles it.

import assert from 'node:assert/strict'
import { createServer } from 'vite'

const server = await createServer({
  configFile: false,
  root: new URL('..', import.meta.url).pathname,
  // `watch: null` because a test loads a module once and exits. Vite's
  // watcher is on by default even in middleware mode, and a watcher is a
  // per-user resource: a suite that starts one server per file exhausts the
  // supply (`EMFILE`) on any machine that already has a dev server running.
  server: { middlewareMode: true, watch: null },
  appType: 'custom',
  logLevel: 'error',
})

const { latest } = await server.ssrLoadModule('/src/lib/store/latest.ts')
const { debounce } = await server.ssrLoadModule('/src/lib/store/debounce.ts')
const { FocusRequest } = await server.ssrLoadModule('/src/lib/store/focus-request.ts')
const { DocBinding } = await server.ssrLoadModule('/src/lib/store/doc-binding.ts')

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

// ── latest: only the newest token may land ──────────────────────────────

{
  const gen = latest()
  const first = gen.next()
  const second = gen.next()
  assert.notEqual(first, second, 'two mints are two different tokens')
  assert.equal(
    gen.isCurrent(first),
    false,
    'the older token is stale the moment a newer one exists',
  )
  assert.equal(gen.isCurrent(second), true, 'the newest token is current')
}

{
  // The exact shape a store's `refresh` uses: mint before the await, check
  // after it, and let the older of two overlapping loads lose.
  const gen = latest()
  const landed = []
  async function refresh(id, delay) {
    const token = gen.next()
    await sleep(delay)
    if (!gen.isCurrent(token)) return
    landed.push(id)
  }
  // The first call is slower, so without the guard it would land *after*
  // the second and overwrite it -- the exact race `calendar.refresh` and
  // `notes.refresh` were missing.
  const a = refresh('slow-first', 30)
  const b = refresh('fast-second', 5)
  await Promise.all([a, b])
  assert.deepEqual(
    landed,
    ['fast-second'],
    'the older, slower load must not land after the newer one',
  )
}

{
  // What `reset` does: retire whatever is in flight without starting a load
  // of its own.
  const gen = latest()
  const token = gen.next()
  gen.next()
  assert.equal(
    gen.isCurrent(token),
    false,
    'a bare `next()` retires an in-flight token, the way `reset` does',
  )
}

// ── debounce: one timer, replacing whatever was waiting ─────────────────

{
  let calls = 0
  let lastArg = null
  const d = debounce((x) => {
    calls++
    lastArg = x
  }, 30)
  d.call('a')
  d.call('b')
  d.call('c')
  await sleep(60)
  assert.equal(calls, 1, 'only the last call in a burst actually runs')
  assert.equal(lastArg, 'c')
}

{
  let calls = 0
  const d = debounce(() => calls++, 20)
  d.call()
  d.cancel()
  await sleep(40)
  assert.equal(calls, 0, 'a cancelled call never runs -- what clearing a search box needs')
}

{
  // Firing on its own, with nothing left to cancel: the timer is not
  // mistakenly treated as still armed after it runs.
  let calls = 0
  const d = debounce(() => calls++, 15)
  d.call()
  await sleep(30)
  d.cancel()
  await sleep(30)
  assert.equal(calls, 1, 'a call already delivered is not affected by a later cancel')
}

// ── FocusRequest: a cursor asked for before there was anywhere to put it ─

{
  const req = new FocusRequest()
  let focused = 0
  req.bind(() => focused++)
  req.request()
  assert.equal(focused, 1, 'requesting with something already bound runs it immediately')
}

{
  // The ordinary case for the tray: the request arrives before the view
  // that owns the field has mounted.
  const req = new FocusRequest()
  let focused = 0
  req.request()
  assert.equal(focused, 0, 'nothing to run yet, so nothing runs')
  req.bind(() => focused++)
  assert.equal(focused, 1, 'binding after the request serves the pending one')
}

{
  // Retiring the binding (unmounting the view) must not leave a stale
  // request waiting to fire into a component that is gone.
  const req = new FocusRequest()
  let focused = 0
  const handler = () => focused++
  req.bind(handler)
  req.bind(null)
  req.request()
  assert.equal(focused, 0, 'a retired binding does not answer a request made after it')
}

// ── DocBinding: the editor's document, asked for rather than pushed ─────

{
  const doc = new DocBinding()
  assert.equal(doc.read(), undefined, 'nothing bound reads as undefined, not a crash')
  let text = 'first'
  doc.bind(() => text)
  assert.equal(doc.read(), 'first')
  text = 'second'
  assert.equal(doc.read(), 'second', 'each read asks again rather than caching the first answer')
  doc.bind(null)
  assert.equal(doc.read(), undefined, 'retiring the binding stops the reads')
}

await server.close()
console.log('store: all checks passed')
