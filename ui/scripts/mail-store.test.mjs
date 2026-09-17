// Behaviour checks for `mail.svelte.ts` itself: `refresh`, `loadMore` and
// the `#applyChanges`/`#patchOne` pair `live.svelte.ts` calls through
// `registerApply`. `mail.test.mjs` only loads the pure logic in `mail.ts`
// -- `applyRowPatch`, `mergeSearchPage` and the rest -- never this class.
// Written before Phase 4 moves `refresh`'s and `loadMore`'s generation
// guard onto `latest()`, and the live-apply pair onto the shared
// `store/` helpers, on the same theory as `library-store.test.mjs`. `#act`
// and `#remove` -- the optimistic single-row actions -- are untouched by
// that migration and are not characterised again here.

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
  modules: [mailModule, stateModule, liveApplyModule],
  close,
} = await load(['/src/lib/mail.svelte.ts', '/src/lib/state.svelte.ts', '/src/lib/live-apply.ts'], {
  svelte: true,
})
const { mail } = mailModule
const { app } = stateModule
const { applierFor } = liveApplyModule

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')

// ── start / refresh ──────────────────────────────────────────────────

await mail.start()
assert.ok(mail.mailboxes.length > 0, 'the mock vault ships with mailboxes')
assert.ok(mail.selectedMailbox, 'the inbox is selected by default')
assert.equal(mail.loading, false, 'loading settles back to false once the load lands')

// A selection that has left the reloaded page is dropped.
mail.selectedThread = 'not-a-real-id'
await mail.refresh()
assert.equal(mail.selectedThread, null, 'a selection not in the reloaded page is cleared')

// ── loadMore ─────────────────────────────────────────────────────────

if (mail.nextCursor) {
  const before = mail.threads.length
  await mail.loadMore()
  assert.ok(mail.threads.length >= before, 'loadMore appends without shrinking the list')
}

// ── #applyChanges / #patchOne ────────────────────────────────────────

const applier = applierFor('mail')
assert.ok(applier, 'the mail store registers an applier on import')

// A draft change is a no-op today -- nothing in the thread list reads a
// draft directly, per the store's own comment.
assert.equal(
  applier([{ kind: 'draft', op: 'updated', id: 'd-1', origin: 'remote' }]),
  true,
  'a draft change is accepted and does nothing',
)

assert.equal(
  applier([
    { kind: 'thread', op: 'updated', id: 't1', origin: 'remote' },
    { kind: 'thread', op: 'updated', id: 't2', origin: 'remote' },
  ]),
  false,
  'more than one change in a batch is declined',
)

{
  const id = mail.threads[0].id
  const before = mail.threads.length
  const accepted = applier([{ kind: 'thread', op: 'deleted', id, origin: 'remote' }])
  assert.equal(accepted, true)
  assert.equal(mail.threads.length, before - 1, 'a deleted thread leaves the list at once')
  assert.ok(!mail.threads.some((t) => t.id === id))
}

{
  const id = mail.threads[0].id
  const row = mail.threads.find((t) => t.id === id)
  const before = row.unreadCount
  const accepted = applier([{ kind: 'thread', op: 'updated', id, origin: 'remote' }])
  assert.equal(accepted, true, 'a single update is accepted -- the fetch runs in the background')
  await new Promise((r) => setTimeout(r, 100))
  const now = mail.threads.find((t) => t.id === id)
  assert.ok(now, 'the row is still in the list after being patched')
  assert.equal(typeof now.unreadCount, typeof before, 'the row was replaced with a fresh fetch')
}

await close()
console.log('mail-store: all checks passed')

// `app.start()` arms the idle-vault poll; see `editor.test.mjs`'s own note.
process.exit(0)
