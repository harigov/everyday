// A window of one app other than the journal never reads the journal.
//
// It cannot switch to it (`appwindow.ts`), so the list of 500 entries and
// the newest page that the main window reads at its unlock, or soon after,
// would be read for nothing every time one of these windows opened. Its own
// file because which window this is is fixed when `appwindow.ts` loads.

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
  location: new URL('http://localhost/?unlocked=1&window=todo'),
  createObjectURL: () => 'blob:stub',
})

const {
  modules: [stateModule, apiModule],
  close,
} = await load(['/src/lib/state.svelte.ts', '/src/lib/api.ts'], { svelte: true })
const { app } = stateModule
const { api } = apiModule

const calls = { journals: 0, entries: 0, entry: 0 }
for (const name of Object.keys(calls)) {
  const real = api[name]
  api[name] = (...args) => {
    calls[name] += 1
    return real(...args)
  }
}

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')
assert.equal(app.section, 'todo', 'a window of one app shows that app')
assert.ok(calls.journals > 0, 'the journals themselves are still read: other apps name them')

// Past the moment the main window would have read them in its idle time.
await new Promise((resolve) => setTimeout(resolve, 2_300))
assert.equal(calls.entries, 0, `a todo window read the journal's list ${calls.entries} times`)
assert.equal(calls.entry, 0, 'or opened an entry')
assert.ok(!app.entriesLoaded)

await close()
console.log('journal-pinned: all checks passed')

// `app.start()` arms the idle-vault poll; see `editor.test.mjs`'s own note.
process.exit(0)
