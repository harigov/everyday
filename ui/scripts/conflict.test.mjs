// Drives the conflict contract through the real mock backend, the same way
// the interface does: create, save from the version we hold, then save again
// from a version something else has superseded.
//
// This used to drive a copy of `save_entry` "lifted verbatim in shape" from
// the mock, which is a contradiction in a test: it could fail on neither the
// mock nor the store it stands in for, because a change to either side left
// the copy behind, agreeing with nothing else. Loading `mock.ts` itself
// through the harness is what makes a broken conflict check in the real
// backend show up here, rather than only in `EVERYDAY_MOCK=1 npm run dev`.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

// `mock.ts` imports only `types.ts` and `colors.ts`, neither of which touches
// a browser global -- but this is the same minimal lending `library.test.mjs`
// gives `websearch.ts`, on the same theory: a sibling file gaining one is not
// this test's business to have predicted.
stubBrowser({ window: globalThis, location: new URL('http://localhost/') })

const { module: mock, close } = await load('/src/lib/mock.ts')
const { mockInvoke } = mock

await mockInvoke('unlock', { password: 'everyday' })

const id = 'e1'
const entry = (over) => ({ id, ...over })
const bodyOf = async () => (await mockInvoke('get_entry', { id })).body

// Create.
await mockInvoke('save_entry', { entry: entry({ body: 'v1', updatedAt: 't1' }), expect: null })
assert.equal(await bodyOf(), 'v1')

// Creating the same id again is a conflict, not an overwrite.
await assert.rejects(
  mockInvoke('save_entry', { entry: entry({ body: 'clash', updatedAt: 't1b' }), expect: null }),
  { code: 'conflict' },
)

// This window holds t1. Something else writes t2.
await mockInvoke('save_entry', { entry: entry({ body: 'theirs', updatedAt: 't2' }), expect: 't1' })

// Our autosave still believes it holds t1.
await assert.rejects(
  mockInvoke('save_entry', { entry: entry({ body: 'mine', updatedAt: 't3' }), expect: 't1' }),
  { code: 'conflict' },
)
assert.equal(await bodyOf(), 'theirs', 'a refused save must change nothing')

// "Keep mine" is the deliberate override.
await mockInvoke('save_entry_force', { entry: entry({ body: 'mine', updatedAt: 't3' }) })
assert.equal(await bodyOf(), 'mine')

// And a save from the now-current version lands normally again.
await mockInvoke('save_entry', {
  entry: entry({ body: 'mine again', updatedAt: 't4' }),
  expect: 't3',
})
assert.equal(await bodyOf(), 'mine again')

await close()
console.log('conflict: all checks passed')
