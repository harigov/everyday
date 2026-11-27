// What the journal's store reads, and writes, and when.
//
// Three costs it used to pay that nobody saw: every window read the list of
// 500 entries and opened the newest at every unlock, whichever app it opened
// on; moving from one entry to the next wrote the one being left, typed into
// or not, and waited on the list being read again before opening the next;
// and every few seconds of typing read the whole list again. Each is a count
// of backend calls, so the counts are the assertions.

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
  modules: [stateModule, apiModule],
  close,
} = await load(['/src/lib/state.svelte.ts', '/src/lib/api.ts'], { svelte: true })
const { app } = stateModule
const { api } = apiModule

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

// Counted at the `api` object, which is what the store calls through. An
// answer can be held back after the backend has given it (`late`), to land
// after something else has happened.
const calls = { entries: 0, entry: 0, saveEntry: 0 }
const late = { entries: 0, entry: 0, saveEntry: 0 }
const saved = []
for (const name of Object.keys(calls)) {
  const real = api[name]
  api[name] = async (...args) => {
    calls[name] += 1
    if (name === 'saveEntry') saved.push(args[0])
    const answer = await real(...args)
    if (late[name]) await sleep(late[name])
    return answer
  }
}
const reset = () => {
  for (const name of Object.keys(calls)) calls[name] = 0
  saved.length = 0
}

// ── Opening on the journal reads it at once ────────────────────────────

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')
assert.equal(app.section, 'journal', 'nothing stored: the journal is the app shown')
assert.ok(app.entriesLoaded, 'the journal on screen has its list read at the unlock')
assert.ok(app.entries.length > 3, 'the mock vault should come with entries')
assert.equal(app.entry?.id, app.entries[0].id, 'and the newest entry open')

// ── Moving through the list writes nothing that was not typed ──────────

const ids = app.entries.slice(0, 4).map((e) => e.id)
reset()
for (const id of ids.slice(1)) await app.openEntry(id)
assert.equal(calls.saveEntry, 0, `three entries looked at wrote ${calls.saveEntry} times`)
assert.equal(calls.entries, 0, `and read the list ${calls.entries} times`)
assert.equal(app.entry?.id, ids[3])

// A held key: the next press arrives before the last entry has opened. The
// second is the one shown, and each is asked for once.
reset()
const first = app.openEntry(ids[2])
const second = app.openEntry(ids[1])
await Promise.all([first, second])
assert.equal(app.selectedEntry, ids[1])
assert.equal(app.entry?.id, ids[1], 'the later of two quick opens is the one that lands')
assert.equal(calls.saveEntry, 0)

// ── What was typed is written, once, as the editor holds it ────────────

const doc = {
  type: 'doc',
  content: [{ type: 'paragraph', content: [{ type: 'text', text: 'Typed just now.' }] }],
}
app.bindBody(() => structuredClone(doc))
reset()
app.scheduleSave()
await app.openEntry(ids[0])
assert.equal(calls.saveEntry, 1, 'an edit left within the autosave delay is written once')
assert.equal(saved[0].id, ids[1], 'against the entry it was typed into')
assert.deepEqual(saved[0].body, doc, 'with the document the editor handed over')
assert.equal(app.entry?.id, ids[0])

// Locking writes nothing more when nothing more was typed.
reset()
await app.flushAll()
assert.equal(calls.saveEntry, 0, 'a flush with nothing typed writes nothing')

// ── Autosaves read the list once the typing pauses ─────────────────────

// Let the refresh the save above queued go by first.
await sleep(1_700)
reset()
for (let i = 0; i < 4; i++) {
  app.scheduleSave()
  await app.flush()
  await sleep(300)
}
assert.equal(calls.saveEntry, 4)
assert.equal(calls.entries, 0, 'no list read while saves keep arriving')
await sleep(1_800)
assert.equal(calls.entries, 1, 'one list read once they stop')
app.bindBody(null)

// ── Opening on another app leaves the journal unread ───────────────────

await app.lock()
app.section = 'todo'
reset()
await app.unlock('jatayu')
assert.equal(app.screen, 'main')
assert.ok(!app.entriesLoaded, 'a window opened on another app does not read the journal at once')
assert.equal(calls.entries, 0)
assert.equal(calls.entry, 0)
assert.equal(app.entry, null)

// A change made elsewhere does not read it either: the first read will.
app.queueListRefresh()
await sleep(50)
assert.equal(calls.entries, 0, 'a live change does not read a list nobody has shown')

// Shown: read once, and the newest opened, however many ask.
await Promise.all([app.ensureEntries(), app.ensureEntries()])
assert.equal(calls.entries, 1, `the journal shown read its list ${calls.entries} times`)
assert.ok(app.entriesLoaded)
assert.equal(app.entry?.id, app.entries[0].id)
await app.ensureEntries()
assert.equal(calls.entries, 1, 'and not again')

// ── A lock while the list is being read ────────────────────────────────

// What the read brings back is vault contents: it must not land behind the
// lock screen, even for a moment, nor leave the next unlock waiting on a
// read that was let go. Watched throughout, because the vault refusing what
// comes after it would clear the screen again soon enough to hide it.
await app.lock()
await app.unlock('jatayu')
late.entries = 400
const across = app.ensureEntries()
await app.lock()
let behindLock = 0
const watch = setInterval(() => (behindLock = Math.max(behindLock, app.entries.length)), 5)
await across
clearInterval(watch)
late.entries = 0
assert.equal(behindLock, 0, `a list read across a lock showed ${behindLock} rows behind it`)
assert.ok(!app.entriesLoaded)
assert.equal(app.entry, null, 'and opens nothing')
await app.unlock('jatayu')
await app.ensureEntries()
assert.ok(app.entriesLoaded, 'the next unlock reads it afresh')
assert.equal(app.entry?.id, app.entries[0].id)

// ── ...and otherwise once the window is idle ───────────────────────────

// So switching to the journal later finds it read.
await app.lock()
reset()
await app.unlock('jatayu')
assert.equal(calls.entries, 0)
await sleep(2_300)
assert.equal(calls.entries, 1, 'an idle window reads the journal it may switch to')
assert.ok(app.entriesLoaded)

await close()
console.log('journal-store: all checks passed')

// `app.start()` arms the idle-vault poll; see `editor.test.mjs`'s own note.
process.exit(0)
