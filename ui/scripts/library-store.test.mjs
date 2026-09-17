// Behaviour checks for `library.svelte.ts` itself -- the class, not the
// pure helpers `library.test.mjs` already covers.
//
// Nothing here exercised `refresh`, `patch`/`touch` or `remove` before this
// file: the store is a runes module, and every existing check of the
// library app either loaded `rating.ts`/`websearch.ts` alone or stopped at
// the type checker. Written *before* Phase 4 moves this store onto the
// shared `store/` helpers, so a migration that changes what any of these
// three do is caught here rather than only by a reviewer's memory of the
// original file.
//
// Driven through the real mock backend, the same way `editor.test.mjs` and
// `conflict.test.mjs` are: a fake `api` would only be a second copy of what
// `mock.ts` already does, and the point of these three methods is the round
// trip through it.

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
  modules: [libraryModule, stateModule],
  close,
} = await load(['/src/lib/library.svelte.ts', '/src/lib/state.svelte.ts'], { svelte: true })
const { library } = libraryModule
const { app } = stateModule

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')

// ── start / refresh ──────────────────────────────────────────────────

await library.start()
assert.ok(library.kinds.length > 0, 'the mock vault ships with shelves')
assert.ok(library.items.length > 0, 'and with items on them')
assert.ok(library.stats, 'refresh loads the shelf stats alongside the items')
assert.equal(library.loading, false, 'loading settles back to false once the load lands')

// A selection that has scrolled out of the current filter is dropped, not
// left pointing at a card the grid no longer shows.
const target = library.items[0]
library.selected = target.id
library.filter = 'done'
await library.refresh()
if (!library.items.some((i) => i.id === target.id)) {
  assert.equal(library.selected, null, 'a selection outside the filter is cleared by refresh')
}
library.filter = 'all'
await library.refresh()

// ── patch / touch ────────────────────────────────────────────────────

{
  const item = library.items[0]
  const before = item.updatedAt
  await new Promise((r) => setTimeout(r, 5))
  library.patch(item.id, { notes: 'characterised' })
  assert.equal(item.notes, 'characterised', 'patch assigns the changes onto the item in place')
  assert.notEqual(item.updatedAt, before, 'patch stamps a fresh updatedAt')
  assert.ok(
    library.items[0] === item,
    'the same object is still in the list -- no reconciliation pass',
  )

  // The write is queued, not immediate -- it lands once the autosave timer's
  // debounce elapses. `flush()` is what the close handshake calls to force it
  // without waiting.
  await library.flush()
}

{
  // `touch` on its own is what a field mutated directly -- a favourite, a
  // rating, a cover -- uses.
  const item = library.items[0]
  const before = item.updatedAt
  await new Promise((r) => setTimeout(r, 5))
  item.favourite = !item.favourite
  library.touch(item.id)
  assert.notEqual(item.updatedAt, before)
  await library.flush()
}

// Patching an id that is not on the shelf does nothing -- no crash, no dirty
// write.
library.patch('not-a-real-id', { notes: 'ignored' })
await library.flush()

// ── remove ───────────────────────────────────────────────────────────

{
  const doomed = library.items[0]
  const countBefore = library.items.length
  library.selected = doomed.id
  await library.remove(doomed.id)
  assert.equal(
    library.items.length,
    countBefore - 1,
    'the row is gone from the list once the delete has landed',
  )
  assert.ok(
    !library.items.some((i) => i.id === doomed.id),
    'the removed item is not still in the list',
  )
  assert.equal(library.selected, null, 'removing the open item closes the detail panel')
}

await close()
console.log('library-store: all checks passed')

// Same reason `editor.test.mjs` needs this: `app.start()` arms the idle-vault
// poll, which is meant to run for the life of the window and would otherwise
// hang this process after the last check has passed.
process.exit(0)
