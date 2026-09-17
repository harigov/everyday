// Behaviour checks for `accounts.svelte.ts` itself: `load` and the
// `#applyChanges`/`#patchOne` pair `live.svelte.ts` calls through
// `registerApply`. `accounts.test.mjs` only loads the pure rules in
// `accounts.ts` -- signInArgs, statusLabel and the rest -- never this class.
// Written before Phase 4 moves both onto the shared `store/` helpers, on
// the same theory as `library-store.test.mjs`.

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
  modules: [accountsModule, stateModule, liveApplyModule],
  close,
} = await load(
  ['/src/lib/accounts.svelte.ts', '/src/lib/state.svelte.ts', '/src/lib/live-apply.ts'],
  { svelte: true },
)
const { accounts } = accountsModule
const { app } = stateModule
const { applierFor } = liveApplyModule

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')

// ── load ─────────────────────────────────────────────────────────────

assert.equal(accounts.loading, false)
await accounts.load()
assert.ok(accounts.list.length > 0, 'the mock vault ships with accounts')
assert.equal(accounts.loading, false, 'loading settles back to false once the load lands')

const listBefore = accounts.list
await accounts.load()
assert.ok(listBefore === accounts.list, 'a second load without force does not re-fetch')

await accounts.load(true)
assert.ok(accounts.list.length > 0)

// ── #applyChanges / #patchOne ────────────────────────────────────────

const applier = applierFor('accounts')
assert.ok(applier, 'the accounts store registers an applier on import')

assert.equal(
  applier([
    { kind: 'account', op: 'updated', id: 'acct-google', origin: 'remote' },
    { kind: 'account', op: 'updated', id: 'acct-fastmail', origin: 'remote' },
  ]),
  false,
  'more than one change in a batch is declined',
)

{
  const id = accounts.list[0].id
  const before = accounts.list.length
  const accepted = applier([{ kind: 'account', op: 'deleted', id, origin: 'remote' }])
  assert.equal(accepted, true)
  assert.equal(accounts.list.length, before - 1, 'a deleted account leaves the list at once')
  assert.ok(!accounts.list.some((a) => a.id === id))
}

{
  const id = accounts.list[0].id
  const row = accounts.list.find((a) => a.id === id)
  row.displayName = 'stale copy'
  const accepted = applier([{ kind: 'account', op: 'updated', id, origin: 'remote' }])
  assert.equal(accepted, true, 'a single update is accepted -- the fetch runs in the background')
  await new Promise((r) => setTimeout(r, 100))
  assert.notEqual(
    accounts.list.find((a) => a.id === id)?.displayName,
    'stale copy',
    'the row is patched from a fresh fetch, not left with the stale local copy',
  )
}

await close()
console.log('accounts-store: all checks passed')

// `app.start()` arms the idle-vault poll; see `editor.test.mjs`'s own note.
process.exit(0)
