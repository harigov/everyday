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
  modules: [notesModule, stateModule, liveApplyModule, apiModule],
  close,
} = await load(
  [
    '/src/lib/notes.svelte.ts',
    '/src/lib/state.svelte.ts',
    '/src/lib/live-apply.ts',
    '/src/lib/api.ts',
  ],
  { svelte: true },
)
const { api } = apiModule
const { notes } = notesModule
const { app } = stateModule
const { applierFor } = liveApplyModule

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')

// ── refresh ──────────────────────────────────────────────────────────

await notes.start()
assert.ok(notes.list.length > 0, 'the mock vault ships with notes')
const boats = notes.tags.find((t) => t.tag === 'boats')
assert.ok(boats, 'the mock vault tags some of them "boats"')
assert.equal(
  boats.count,
  notes.list.filter((n) => n.tags.includes('boats')).length,
  'a tag carries how many notes have it',
)
assert.ok(
  notes.tags.every((t, i, all) => i === 0 || all[i - 1].count >= t.count),
  'tags come most used first',
)

await notes.setTag('boats')
assert.ok(
  notes.list.every((n) => n.tags.includes('boats')),
  'a tag filter narrows the list to notes carrying it',
)
await notes.setTag(null)
assert.ok(notes.list.length > 1, 'clearing the tag widens the list again')

// ── the kind filter ──────────────────────────────────────────────────

assert.ok(notes.mixed, 'the mock vault holds notes the assistant and calls wrote')
for (const origin of ['own', 'assistant', 'meeting']) {
  notes.setOrigin(origin)
  assert.ok(notes.visible.length > 0, `the mock vault has ${origin} notes`)
  assert.ok(
    notes.visible.every((n) => n.origin === origin),
    `the ${origin} filter shows only ${origin} notes`,
  )
  assert.ok(notes.list.length > notes.visible.length, 'the list underneath keeps every kind')
}
await notes.setTag('boats')
notes.setOrigin('assistant')
assert.ok(
  notes.visible.every((n) => n.origin === 'assistant' && n.tags.includes('boats')),
  'the kind filter and the tag filter narrow together',
)
assert.ok(notes.mixed, 'a tag filter does not hide the kind filter')
assert.match(notes.showing.view ?? '', /assistant wrote tagged boats/)
notes.setOrigin(null)
await notes.setTag(null)
assert.equal(notes.visible, notes.list, 'no kind filter shows the list as it is')

// A note opened from the vault says who started it.
{
  const meeting = notes.list.find((n) => n.origin === 'meeting')
  await notes.openNote(meeting.id)
  assert.equal(notes.open.origin, 'meeting')
}

// A note made or opened under a kind filter that would hide it lets the
// filter go, so it has a row in the sidebar.
{
  notes.setOrigin('meeting')
  await notes.create('made while looking at meetings')
  assert.equal(notes.origin, null, 'a new note is the person’s, so the meetings filter goes')
  assert.ok(notes.visible.some((n) => n.id === notes.selected))

  notes.setOrigin('assistant')
  await notes.openNote(notes.list.find((n) => n.origin === 'assistant').id)
  assert.equal(notes.origin, 'assistant', 'a note the filter shows leaves it alone')
  await notes.openNote(notes.list.find((n) => n.origin === 'meeting').id)
  assert.equal(notes.origin, null, 'one it hides lets it go')
}

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

// A row changed elsewhere moves to where a reload would put it -- here,
// the oldest note just edited goes to the top of the unpinned ones -- rather
// than staying where it was and splitting its month's section around it.
{
  const oldest = notes.list.at(-1)
  assert.ok(!oldest.pinned)
  const whole = await api.note(oldest.id)
  whole.updatedAt = new Date().toISOString()
  await api.saveNoteForce(whole)
  applier([{ kind: 'note', op: 'updated', id: oldest.id, origin: 'remote' }])
  await new Promise((r) => setTimeout(r, 100))
  assert.equal(notes.list.find((n) => !n.pinned).id, oldest.id)
  assert.ok(
    notes.list
      .slice(0, notes.list.indexOf(notes.list.find((n) => !n.pinned)))
      .every((n) => n.pinned),
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
