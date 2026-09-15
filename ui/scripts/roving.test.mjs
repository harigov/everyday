// Behaviour checks for `nextRovingIndex` -- the pure arithmetic behind
// `rovingFocus`'s Home/End/PageUp/PageDown/arrow-key movement, shared by the
// DOM sweep (over whatever `VirtualList` has drawn) and the virtualized path
// (over the whole data set). See `lib/roving.ts`'s own doc, "Virtualized
// lists", for why a row not currently rendered still has to be reachable by
// index alone -- finding 6.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: roving, close } = await load('/src/lib/roving.ts')
const { nextRovingIndex } = roving

// ── A single-column list -- the journal, a mailbox's thread list ────────

assert.equal(nextRovingIndex(0, 'ArrowDown', 10), 1)
assert.equal(nextRovingIndex(9, 'ArrowDown', 10), 9, 'clamped at the bottom')
assert.equal(nextRovingIndex(0, 'ArrowUp', 10), 0, 'clamped at the top')
assert.equal(nextRovingIndex(5, 'ArrowUp', 10), 4)

assert.equal(nextRovingIndex(5, 'Home', 10), 0)
assert.equal(nextRovingIndex(5, 'End', 10), 9)
assert.equal(nextRovingIndex(0, 'End', 1), 0, 'a single row: End stays put')

assert.equal(nextRovingIndex(2, 'PageDown', 100, { page: 10 }), 12)
assert.equal(nextRovingIndex(95, 'PageDown', 100, { page: 10 }), 99, 'clamped at the bottom')
assert.equal(nextRovingIndex(5, 'PageUp', 100, { page: 10 }), 0, 'clamped at the top')

// Left/Right move nothing in a single column: the library's own use of them
// (within a row of several controls) is decided by the caller, not here.
assert.equal(nextRovingIndex(3, 'ArrowLeft', 10), null)
assert.equal(nextRovingIndex(3, 'ArrowRight', 10), null)

// A key this action does not claim.
assert.equal(nextRovingIndex(3, 'Escape', 10), null)

// An empty list: nothing to move to, whatever the key.
assert.equal(nextRovingIndex(0, 'ArrowDown', 0), null)

// ── `withinRow`: Left/Right belong to the row's own controls instead ────

assert.equal(
  nextRovingIndex(3, 'ArrowRight', 10, { columns: 3, withinRow: true }),
  null,
  'a grid row with several controls of its own: Left/Right are the caller’s, not the line’s',
)

// ── A grid -- the library's cards ────────────────────────────────────────

assert.equal(nextRovingIndex(0, 'ArrowDown', 20, { columns: 5 }), 5, 'down one line')
assert.equal(nextRovingIndex(7, 'ArrowUp', 20, { columns: 5 }), 2, 'up one line')
assert.equal(nextRovingIndex(0, 'ArrowRight', 20, { columns: 5 }), 1, 'along the line')
assert.equal(nextRovingIndex(0, 'ArrowLeft', 20, { columns: 5 }), 0, 'clamped at the line’s start')
assert.equal(nextRovingIndex(2, 'PageDown', 20, { columns: 5, page: 10 }), 12)

await close()
console.log('roving: all checks passed')
