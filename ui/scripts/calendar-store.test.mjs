// Behaviour checks for `calendar.svelte.ts` itself: `refresh`, `patch` and
// `removeBlock`, none of which any existing test loads --
// `calendar.test.mjs` stops at `time.ts`'s pure grid arithmetic. Written
// before Phase 4 moves this store onto the shared `store/` helpers, on the
// same theory as `library-store.test.mjs`.
//
// Driven through the real mock backend, the same way `editor.test.mjs` is.

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
  modules: [calendarModule, stateModule],
  close,
} = await load(['/src/lib/calendar.svelte.ts', '/src/lib/state.svelte.ts'], { svelte: true })
const { calendar } = calendarModule
const { app } = stateModule

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')

// ── start / refresh ──────────────────────────────────────────────────

await calendar.start()
assert.equal(calendar.loading, false, 'loading settles back to false once the load lands')

// Book something to work with -- the store's one write.
const block = await calendar.book({
  subject: { type: 'adhoc' },
  day: calendar.anchor,
  startMinutes: 9 * 60,
  minutes: 30,
  title: 'characterised',
})
assert.ok(block, 'booking a block should succeed against the mock')
assert.equal(calendar.selection?.id, block.id, 'booking selects the new block by default')

// A selection outside the window loaded by `refresh` is dropped.
calendar.selection = { kind: 'block', id: 'not-a-real-id' }
await calendar.refresh()
assert.equal(calendar.selection, null, 'a selection for a block not in range is cleared')

// ── patch ────────────────────────────────────────────────────────────

{
  const before = block.updatedAt
  await new Promise((r) => setTimeout(r, 5))
  calendar.patch(block.id, { title: 'patched' })
  const now = calendar.blocks.find((b) => b.id === block.id)
  assert.equal(now.title, 'patched', 'patch assigns the changes onto the block in place')
  assert.notEqual(now.updatedAt, before, 'patch stamps a fresh updatedAt')
  await calendar.flush()
}

// Patching an id that is not loaded does nothing.
calendar.patch('not-a-real-id', { title: 'ignored' })
await calendar.flush()

// ── removeBlock ──────────────────────────────────────────────────────

{
  calendar.selection = { kind: 'block', id: block.id }
  const before = calendar.blocks.length
  await calendar.removeBlock(block.id)
  assert.equal(calendar.blocks.length, before - 1, 'the block is gone from the grid')
  assert.ok(
    !calendar.blocks.some((b) => b.id === block.id),
    'the removed block is not still loaded',
  )
  assert.equal(calendar.selection, null, 'removing the selected block clears the selection')
}

await close()
console.log('calendar-store: all checks passed')

// `app.start()` arms the idle-vault poll; `calendar.start()` arms its own
// sync poll and clock on top of it -- see `editor.test.mjs`'s own note on
// why this process must be told to exit rather than left to hang.
process.exit(0)
