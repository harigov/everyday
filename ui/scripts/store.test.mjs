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
const { guardedRefresh } = await server.ssrLoadModule('/src/lib/store/refresh.ts')
const { optimisticPatch } = await server.ssrLoadModule('/src/lib/store/optimistic-patch.ts')
const { optimisticRemove } = await server.ssrLoadModule('/src/lib/store/optimistic-remove.ts')
const { applySingleChange, patchOneFromChange } = await server.ssrLoadModule(
  '/src/lib/store/live-patch.ts',
)
const { VaultError } = await server.ssrLoadModule('/src/lib/types.ts')

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

{
  // What both stores' `reset` runs on a lock. Without it the request outlives
  // the screen it was made on: ask for the capture line on a vault with
  // nowhere to put it, lock, and the cursor jumps into the field whenever one
  // finally mounts -- possibly minutes later, in the middle of something else.
  const req = new FocusRequest()
  let focused = 0
  req.request()
  req.forget()
  req.bind(() => focused++)
  assert.equal(focused, 0, 'a forgotten request is not served by a later binding')
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

// ── guardedRefresh: mint, load, land only if still current ─────────────

{
  // The shape every store's `refresh` used to write by hand: mint before the
  // load, check after it, and let the older of two overlapping loads lose --
  // and the loading flag settle on `false` only once, from the call that is
  // actually still current.
  const gen = latest()
  const loadingCalls = []
  const landed = []
  async function refresh(id, delay) {
    await guardedRefresh(
      gen,
      async (isCurrent) => {
        await sleep(delay)
        if (!isCurrent()) return
        landed.push(id)
      },
      { setLoading: (v) => loadingCalls.push([id, v]) },
    )
  }
  const a = refresh('slow-first', 30)
  const b = refresh('fast-second', 5)
  await Promise.all([a, b])
  assert.deepEqual(
    landed,
    ['fast-second'],
    'the older, slower load must not land after the newer one',
  )
  assert.deepEqual(
    loadingCalls,
    [
      ['slow-first', true],
      ['fast-second', true],
      ['fast-second', false],
    ],
    'only the call that is still current lowers the loading flag',
  )
}

{
  // A failure is reported through `onError`, not thrown past this call --
  // the same as every store's own `catch (e) { await handle(e) }`.
  const gen = latest()
  let reported = null
  await guardedRefresh(
    gen,
    async () => {
      throw new Error('boom')
    },
    { onError: (e) => (reported = e.message) },
  )
  assert.equal(reported, 'boom')
}

{
  // With no `onError`, the failure is the caller's to catch -- for a store
  // that wants its own `try`/`catch` around this rather than the shared one.
  const gen = latest()
  await assert.rejects(
    guardedRefresh(gen, async () => {
      throw new Error('boom')
    }),
    /boom/,
  )
}

{
  // A store with no loading flag of its own -- the notes app, whose `start`
  // owns it instead -- is not asked to have one.
  const gen = latest()
  let ran = false
  await guardedRefresh(gen, async () => {
    ran = true
  })
  assert.ok(ran)
}

// ── optimisticPatch: assign, stamp, queue the write ─────────────────────

{
  const items = [{ id: 'a', title: 'first', updatedAt: 'old' }]
  const touched = []
  const patcher = optimisticPatch((id) => items.find((i) => i.id === id), {
    touch: (id) => touched.push(id),
  })
  patcher.patch('a', { title: 'second' })
  assert.equal(items[0].title, 'second', 'the changes are assigned onto the record')
  assert.notEqual(items[0].updatedAt, 'old', 'the write stamps a fresh updatedAt')
  assert.deepEqual(touched, ['a'], 'the write is queued through the autosave')
}

{
  // Patching an id that is not there does nothing -- no stamp, no queue.
  const items = [{ id: 'a', title: 'first', updatedAt: 'old' }]
  const touched = []
  const patcher = optimisticPatch((id) => items.find((i) => i.id === id), {
    touch: (id) => touched.push(id),
  })
  patcher.patch('missing', { title: 'ignored' })
  assert.equal(items[0].updatedAt, 'old')
  assert.deepEqual(touched, [])
}

{
  // `touch` on its own is what an edit that mutated a field directly -- a
  // favourite toggled, a cover set -- uses: it stamps and queues without an
  // `Object.assign` of its own.
  const items = [{ id: 'a', favourite: false, updatedAt: 'old' }]
  const touched = []
  const patcher = optimisticPatch((id) => items.find((i) => i.id === id), {
    touch: (id) => touched.push(id),
  })
  items[0].favourite = true
  patcher.touch('a')
  assert.equal(items[0].favourite, true)
  assert.notEqual(items[0].updatedAt, 'old')
  assert.deepEqual(touched, ['a'])
}

// ── optimisticRemove: drop it, ask the backend, put it back on failure ──

{
  let list = ['a', 'b', 'c']
  const calls = []
  await optimisticRemove({
    optimistic: () => {
      list = list.filter((x) => x !== 'b')
    },
    call: async () => {
      calls.push('call')
    },
    onSuccess: () => calls.push('success'),
    rollback: () => calls.push('rollback'),
  })
  assert.deepEqual(list, ['a', 'c'], 'the row is gone at once')
  assert.deepEqual(calls, ['call', 'success'], 'success runs the backend call then onSuccess')
}

{
  // The backend refuses: the rollback given to `handle` runs, and
  // `onSuccess` does not.
  let list = ['a', 'b', 'c']
  const calls = []
  await optimisticRemove({
    optimistic: () => {
      list = list.filter((x) => x !== 'b')
      calls.push('optimistic')
    },
    call: async () => {
      throw new Error('refused')
    },
    onSuccess: () => calls.push('success'),
    rollback: () => {
      list = ['a', 'b', 'c']
      calls.push('rollback')
    },
  })
  assert.deepEqual(list, ['a', 'b', 'c'], 'a refused delete is put back')
  assert.deepEqual(calls, ['optimistic', 'rollback'], 'onSuccess must not run for a refused delete')
}

{
  // A lock does not roll back -- the same policy `handle` gives everywhere
  // else: it is a screen to go to, not a row to restore before leaving it.
  const calls = []
  await optimisticRemove({
    optimistic: () => {},
    call: async () => {
      throw new VaultError('locked', 'vault is locked')
    },
    rollback: () => calls.push('rollback'),
  })
  assert.deepEqual(calls, [], 'a lock must not run the rollback')
}

// ── applySingleChange: is this one record, and what should happen? ──────

function change(kind, op, id, extra = {}) {
  return { kind, op, id, origin: 'remote', ...extra }
}

{
  const deleted = []
  const upserted = []
  const handlers = { onDeleted: (id) => deleted.push(id), onUpserted: (id) => upserted.push(id) }
  assert.equal(applySingleChange([], handlers), false, 'an empty batch is not a single change')
  assert.equal(
    applySingleChange([change('note', 'updated', 'n1'), change('note', 'updated', 'n2')], handlers),
    false,
    'more than one change is not a single change',
  )
  assert.equal(
    applySingleChange([change('note', 'updated', null)], handlers),
    false,
    'neither id nor ids',
  )
  assert.deepEqual(deleted, [])
  assert.deepEqual(upserted, [])
}

{
  const deleted = []
  const upserted = []
  const handlers = { onDeleted: (id) => deleted.push(id), onUpserted: (id) => upserted.push(id) }
  assert.equal(applySingleChange([change('note', 'deleted', 'n1')], handlers), true)
  assert.deepEqual(deleted, ['n1'])
  assert.equal(applySingleChange([change('note', 'created', 'n2')], handlers), true)
  assert.equal(applySingleChange([change('note', 'updated', 'n3')], handlers), true)
  assert.deepEqual(upserted, ['n2', 'n3'])
}

{
  // `skip` is mail's own draft bypass: reported as handled, without either
  // callback running.
  const deleted = []
  const upserted = []
  const result = applySingleChange([change('draft', 'updated', 'd1')], {
    skip: (c, id) => c.kind === 'draft' && id === 'd1',
    onDeleted: (id) => deleted.push(id),
    onUpserted: (id) => upserted.push(id),
  })
  assert.equal(result, true)
  assert.deepEqual(deleted, [])
  assert.deepEqual(upserted, [])
}

// ── patchOneFromChange: fetch the one row, place it, or fall back ───────

{
  const applied = []
  await patchOneFromChange({
    fetch: async () => 'fetched',
    apply: (v) => applied.push(v),
    fallback: async () => applied.push('fallback'),
  })
  assert.deepEqual(applied, ['fetched'])
}

{
  const applied = []
  await patchOneFromChange({
    fetch: async () => {
      throw new Error('network is down')
    },
    apply: (v) => applied.push(v),
    fallback: async () => applied.push('fallback'),
  })
  assert.deepEqual(applied, ['fallback'], 'anything other than a lock falls back')
}

{
  // A lock runs `onLocked` and nothing else -- the fallback is not a refresh
  // worth attempting against a vault that just shut.
  const applied = []
  let locked = 0
  await patchOneFromChange({
    fetch: async () => {
      throw new VaultError('locked', 'vault is locked')
    },
    apply: (v) => applied.push(v),
    onLocked: () => locked++,
    fallback: async () => applied.push('fallback'),
  })
  assert.equal(locked, 1)
  assert.deepEqual(applied, [])
}

await server.close()
console.log('store: all checks passed')
