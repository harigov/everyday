// Behaviour checks for the parts of `notes.svelte.ts` `editor.test.mjs`
// does not reach: `refresh`, and the `#applyChanges`/`#patchOne` pair
// `live.svelte.ts` calls through `registerApply`. `editor.test.mjs` already
// characterises the `DocBinding`/`Autosave` half -- `bindBody`, `syncBody`,
// `edited`, `flush` -- so this file does not repeat it.
//
// Written before Phase 4 moves `refresh` and the live-apply pair onto the
// shared `store/` helpers, on the same theory as `library-store.test.mjs`:
// pass on the unmigrated file first.

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
  modules: [notesModule, stateModule, liveApplyModule],
  close,
} = await load(['/src/lib/notes.svelte.ts', '/src/lib/state.svelte.ts', '/src/lib/live-apply.ts'], {
  svelte: true,
})
const { notes } = notesModule
const { app } = stateModule
const { applierFor } = liveApplyModule

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')

// ── refresh ──────────────────────────────────────────────────────────

await notes.start()
assert.ok(notes.list.length > 0, 'the mock vault ships with notes')
assert.ok(notes.tags.includes('boats'), 'the mock vault tags one of them "boats"')

await notes.setTag('boats')
assert.ok(
  notes.list.every((n) => n.tags.includes('boats')),
  'a tag filter narrows the list to notes carrying it',
)
await notes.setTag(null)
assert.ok(notes.list.length > 1, 'clearing the tag widens the list again')

// ── #applyChanges / #patchOne, through `live.svelte.ts`'s own entry point ─

const applier = applierFor('notes')
assert.ok(applier, 'the notes store registers an applier on import')

// A batch wider than one record is declined, so `live` falls back to a
// whole-store refresh.
assert.equal(
  applier([
    { kind: 'note', op: 'updated', id: 'n-1', origin: 'remote' },
    { kind: 'note', op: 'updated', id: 'n-2', origin: 'remote' },
  ]),
  false,
  'more than one change in a batch is declined',
)

// A delete removes the row synchronously.
{
  const before = notes.list.length
  const accepted = applier([{ kind: 'note', op: 'deleted', id: 'n-2', origin: 'remote' }])
  assert.equal(accepted, true)
  assert.equal(notes.list.length, before - 1, 'a deleted note leaves the list at once')
  assert.ok(!notes.list.some((n) => n.id === 'n-2'))
}

// A create/update is accepted synchronously and patched in once the fetch
// that follows lands.
{
  await notes.create('a title the tag filter will not exclude')
  const id = notes.selected
  notes.setTitle('renamed by another window')
  await notes.flush()

  // Drop the local copy's title back, as if this window had not made the
  // edit -- `#patchOne`'s fetch is what is meant to bring the rename back.
  const row = notes.list.find((n) => n.id === id)
  row.title = 'stale'

  const accepted = applier([{ kind: 'note', op: 'updated', id, origin: 'remote' }])
  assert.equal(accepted, true, 'a single update is accepted -- the fetch runs in the background')
  // `#patchOne` does not block `#applyChanges`'s own return -- see its doc --
  // so the list is asked to settle before it is checked.
  await new Promise((r) => setTimeout(r, 100))
  assert.equal(
    notes.list.find((n) => n.id === id)?.title,
    'renamed by another window',
    'the row is patched from a fresh fetch, not left with the stale local copy',
  )
}

// A create/update for a note that does not carry the active tag filter is
// never inserted into the list -- `#patchOne`'s own guard, not merely the
// backend's query.
{
  await notes.setTag('boats')
  await notes.create('not a boat note')
  const untagged = notes.selected
  // `create` already excluded it -- its own `refresh` reads through the
  // filter -- so this is testing the live-apply path once more, on top of
  // that, with a change naming exactly the note the filter should refuse.
  const before = notes.list.length
  const accepted = applier([{ kind: 'note', op: 'updated', id: untagged, origin: 'remote' }])
  assert.equal(accepted, true)
  await new Promise((r) => setTimeout(r, 100))
  assert.equal(
    notes.list.length,
    before,
    'an untagged note is not inserted into a tag-filtered list',
  )
  assert.ok(!notes.list.some((n) => n.id === untagged))
  await notes.setTag(null)
}

await close()
console.log('notes-store: all checks passed')

// `app.start()` arms the idle-vault poll; see `editor.test.mjs`'s own note.
process.exit(0)
