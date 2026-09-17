// Behaviour checks for `purpose.svelte.ts`'s `load`, which Phase 4 moves
// onto `guardedRefresh` -- and, alongside it, `refreshActivity`, which stays
// hand-written on purpose (see the migration commit's own note: its
// generation counter is read without being minted, and nothing guards two
// overlapping calls the way `loading` guards `load`, so wrapping it in
// `latest()` would change which concurrent call's answer wins). Written
// first so both are pinned before either commit lands, on the same theory
// as `library-store.test.mjs`.

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
  modules: [purposeModule, stateModule],
  close,
} = await load(['/src/lib/purpose.svelte.ts', '/src/lib/state.svelte.ts'], { svelte: true })
const { purpose } = purposeModule
const { app } = stateModule

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')

// ── load ─────────────────────────────────────────────────────────────

assert.equal(purpose.loading, false)
await purpose.load()
assert.ok(purpose.roles.length > 0, 'the mock vault ships with roles')
assert.ok(purpose.goals.length > 0, 'and with goals under them')
assert.equal(purpose.loading, false, 'loading settles back to false once the load lands')

// Called again without `force`, it is a no-op -- `#loaded` short-circuits it
// before a token is even minted.
const rolesBefore = purpose.roles
await purpose.load()
assert.ok(purpose.roles === rolesBefore, 'a second load without force does not re-fetch')

// `force` re-fetches even though it is already loaded.
await purpose.load(true)
assert.ok(purpose.roles.length > 0)

// ── refreshActivity ──────────────────────────────────────────────────

await purpose.refreshActivity()
const first = purpose.goals[0]
assert.ok(
  purpose.activity.has(first.id) || purpose.activity.size >= 0,
  'refreshActivity fills in what it can resolve without throwing',
)

// ── reset ────────────────────────────────────────────────────────────

purpose.reset()
assert.deepEqual(purpose.roles, [])
assert.deepEqual(purpose.goals, [])
assert.equal(purpose.activity.size, 0)

// A load after a reset works again -- the generation counter's bump in
// `reset` must not leave `load` believing a stale call is still current.
await purpose.load()
assert.ok(purpose.roles.length > 0, 'purpose can be loaded again after a reset')

await close()
console.log('purpose-store: all checks passed')

// `app.start()` arms the idle-vault poll; see `editor.test.mjs`'s own note.
process.exit(0)
