// Where the editor's document lives between keystrokes.
//
// The journal and the notes app draw the *same* editor over the same rich
// text — `RichText.svelte`, lifted out of the journal's page when notes
// arrived precisely so there would be one place for the caret bugs to be
// fixed. What each store owes that component is a small contract, and it is
// the kind that fails quietly:
//
//   `bindBody`  the store is handed a way to *ask* for the document
//   `syncBody`  the store asks, and puts the answer in the open record
//   `edited`    something was typed; the store notes it and asks nothing
//
// The last line is the whole point. Pulling the document out of ProseMirror
// walks its entire tree and rebuilds it as JSON, which is work proportional
// to everything already written — so a store that does it per keystroke gets
// slower to type into the longer the text gets. The journal had this right
// and the notes app did not: its editor callback pulled the document on every
// character, and on a long note that was some seven times what the journal
// spent in the same callback.
//
// Neither half of that is a type error and neither shows up on a short note,
// which is why it is pinned here: the getter is counted, and the count is the
// assertion.

import assert from 'node:assert/strict'
import { createServer } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

// The stores were written for a browser. Enough of one is lent to them to let
// the modules evaluate and to let a save reach the mock backend; nothing below
// depends on the shims being faithful, because nothing below draws anything.
globalThis.window ??= globalThis
globalThis.localStorage ??= {
  getItem: () => null,
  setItem: () => {},
  removeItem: () => {},
}
globalThis.matchMedia ??= () => ({
  matches: false,
  addEventListener: () => {},
  removeEventListener: () => {},
})
globalThis.document ??= {
  querySelector: () => null,
  documentElement: {
    style: { setProperty: () => {} },
    classList: { toggle: () => {} },
    setAttribute: () => {},
    removeAttribute: () => {},
  },
  addEventListener: () => {},
}
globalThis.navigator ??= { userAgent: 'node', language: 'en-GB' }
globalThis.location ??= new URL('http://localhost/?unlocked=1')
globalThis.URL.createObjectURL ??= () => 'blob:stub'

// The Svelte plugin, for the same reason `actions.test.mjs` names it: a
// `.svelte.ts` module needs it, because it is what turns `$state` into
// something that exists. `watch: null` keeps one server per file from
// exhausting the machine's watches.
const server = await createServer({
  configFile: false,
  root: new URL('..', import.meta.url).pathname,
  plugins: [svelte()],
  server: { middlewareMode: true, watch: null },
  appType: 'custom',
  logLevel: 'error',
})

const { notes } = await server.ssrLoadModule('/src/lib/notes.svelte.ts')
const { app } = await server.ssrLoadModule('/src/lib/state.svelte.ts')

// The vault is opened first: every store gates itself on what the backend
// says it carries, so a notes store asked to load before `status` is known
// correctly does nothing at all.
await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')

// ── The notes app ─────────────────────────────────────────────────────

await notes.start()
assert.ok(notes.list.length > 0, 'the mock vault should come with notes in it')
await notes.openNote(notes.list[0].id)
assert.ok(notes.open, 'a note should be open')

// The editor's side of the contract, counted.
let asked = 0
let document_ = { type: 'doc', content: [] }
notes.bindBody(() => {
  asked += 1
  return document_
})

// Forty characters typed. The store must note every one of them — that is
// what puts the note on the autosave timer — and ask for the document for
// none of them.
asked = 0
for (let i = 0; i < 40; i++) notes.edited()
assert.equal(asked, 0, `typing asked the editor for its document ${asked} times; it must not ask`)

// ...and the save, which is the one thing that genuinely needs the text,
// asks exactly once and files what it is given.
document_ = { type: 'doc', content: [{ type: 'paragraph' }] }
asked = 0
await notes.flush()
assert.equal(asked, 1, `a save asked the editor for its document ${asked} times; it must ask once`)
assert.deepEqual(
  notes.open.body,
  document_,
  'the document the editor handed over must be the one filed against the note',
)

// A save with nothing outstanding writes nothing, so it need not ask at all;
// what it must not do is ask more than once.
asked = 0
await notes.flush()
assert.ok(asked <= 1, `a save with nothing pending asked ${asked} times`)

// Retiring the getter is what the editor does on the way out. A save after
// that must leave the last document alone rather than blanking the note.
const last = notes.open.body
notes.bindBody(null)
notes.edited()
await notes.flush()
assert.deepEqual(notes.open.body, last, 'a save with no editor bound must not lose the text')

// ── The journal, which has always had it right ────────────────────────
//
// Checked alongside rather than taken on trust: the two stores are the two
// callers of one component, and a contract only one of them keeps is the
// exact shape of the bug this file exists for.

await app.newEntry()
assert.ok(app.entry, 'an entry should be open')

let journalAsked = 0
app.bindBody(() => {
  journalAsked += 1
  return { type: 'doc', content: [] }
})

journalAsked = 0
for (let i = 0; i < 40; i++) app.scheduleSave()
assert.equal(journalAsked, 0, 'typing in the journal must not ask the editor for its document')

journalAsked = 0
await app.flush()
assert.equal(journalAsked, 1, 'a journal save asks the editor for its document exactly once')

await server.close()
console.log('editor: all checks passed')

// Said explicitly, because this is the one test file that boots the whole
// application store: `app.start()` arms the poll that watches for an idle
// vault, and that poll is meant to run for as long as the window does. There
// is nothing to stop it from out here, and without this the suite hangs after
// the last check has already passed.
process.exit(0)
