// Behaviour checks for `live.svelte.ts`'s opt-in half: whether a batch of
// changes is handed to a store's `apply` or falls back to the whole-store
// `refresh()` -- see `live-apply.ts` and `planBatch` in `live.svelte.ts`.
//
// `registerApply('notes', ...)` below replaces the real notes store's
// applier -- registered the moment `notes.svelte.ts` is imported, which
// `live.svelte.ts` does for its own reasons -- with a fake one for the length
// of this file. The real one would reach for `api.note`, which nothing here
// stubs; a fake that returns whatever the test wants is what makes this a
// check of the *routing*, not of the notes store or the vault behind it.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

// The module reaches into every store, and they were written for a browser.
// Same stubs as `live.test.mjs`.
stubBrowser({
  window: true,
  localStorage: true,
  matchMedia: true,
  document: true,
  navigator: true,
  location: true,
  createObjectURL: () => 'blob:stub',
})

const {
  modules: [live, liveApply],
  close,
} = await load(['/src/lib/live.svelte.ts', '/src/lib/live-apply.ts'], { svelte: true })
const { planBatch } = live
const { registerApply, singleId } = liveApply

function change(kind, op, id, extra = {}) {
  return { kind, op, id, origin: 'remote', ...extra }
}

// ── singleId ───────────────────────────────────────────────────────────

assert.equal(singleId(change('note', 'updated', 'n1')), 'n1')
assert.equal(singleId(change('note', 'updated', null, { ids: ['n1'] })), 'n1')
assert.equal(
  singleId(change('note', 'updated', null, { ids: ['n1', 'n2'] })),
  null,
  'more than one id is not a single id',
)
assert.equal(singleId(change('note', 'updated', null)), null, 'neither id nor ids')

// ── A target with nothing registered always falls back ───────────────────

{
  const { applied, refreshed } = planBatch([change('task', 'updated', 't1')])
  assert.deepEqual(applied, [])
  assert.deepEqual(refreshed, ['todo'])
}

// ── A target whose store declines still falls back ────────────────────────

{
  let received = null
  registerApply('notes', (changes) => {
    received = changes
    return false
  })
  const { applied, refreshed } = planBatch([change('note', 'updated', 'n1')])
  assert.deepEqual(applied, [])
  assert.deepEqual(refreshed, ['notes'])
  assert.equal(received.length, 1)
  assert.equal(received[0].id, 'n1')
}

// ── A target whose store applies is not refreshed ─────────────────────────

{
  registerApply('notes', () => true)
  const { applied, refreshed } = planBatch([change('note', 'created', 'n2')])
  assert.deepEqual(applied, ['notes'])
  assert.deepEqual(refreshed, [])
}

// ── A mixed batch: each target gets only the changes routed to it ─────────

{
  const byTarget = {}
  registerApply('notes', (changes) => {
    byTarget.notes = changes
    return true
  })
  const batch = [change('note', 'updated', 'n1'), change('task', 'updated', 't1')]
  const { applied, refreshed } = planBatch(batch)
  assert.deepEqual(applied, ['notes'])
  assert.deepEqual(refreshed, ['todo'])
  // The applier saw its own change and nothing of the task's.
  assert.equal(byTarget.notes.length, 1)
  assert.equal(byTarget.notes[0].kind, 'note')
}

await close()
console.log('live-apply: all checks passed')
