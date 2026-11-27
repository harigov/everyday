// `lib/views.svelte.ts`: each app's view is a chunk of its own, loaded the
// first time it is shown -- and what happens when one cannot be.
//
// On web access a tab can outlive the build that served it: the page names
// chunks by hashes the server no longer has, and every app the tab had not
// opened yet fails to load. A failure must not stick to the next attempt,
// and the page reloads -- once for each build it came from, so a chunk that
// is broken rather than missing cannot reload it for ever.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

let reloads = 0
const stored = new Map()
const session = {
  getItem: (key) => stored.get(key) ?? null,
  setItem: (key, value) => void stored.set(key, String(value)),
}
// Set outright rather than through `stubBrowser`'s `??=`: a Node new enough
// has storage of its own, and `location` needs a `reload` to count.
Object.defineProperty(globalThis, 'sessionStorage', {
  value: session,
  configurable: true,
  writable: true,
})
Object.defineProperty(globalThis, 'location', {
  value: { search: '', href: 'http://localhost/', reload: () => (reloads += 1) },
  configurable: true,
  writable: true,
})
stubBrowser({ window: true, localStorage: true })

const { module, close } = await load('/src/lib/views.svelte.ts', { svelte: true })
const { Views, reloadForNewBuild } = module

const later = (ms = 0) => new Promise((resolve) => setTimeout(resolve, ms))

// The failures below are logged as they happen; that is not what is checked.
const logged = console.error
console.error = () => {}

// ── a load that fails, and the next one ──────────────────────────────

{
  const asked = []
  let failing = true
  const Journal = { name: 'Journal' }
  let release = () => {}
  const views = new Views(async (key) => {
    asked.push(key)
    if (failing) throw new TypeError('Importing a module script failed.')
    await new Promise((resolve) => (release = resolve))
    return [{ default: Journal }, null]
  })

  await views.ensure('journal')
  assert.equal(views.failed('journal'), true, 'a chunk that cannot be fetched is a failure')
  assert.equal(views.view('journal'), undefined)

  failing = false
  const again = views.ensure('journal')
  assert.equal(
    views.failed('journal'),
    false,
    'a new load clears the old failure as it starts, not once it has arrived',
  )
  const meanwhile = views.ensure('journal')
  release()
  await Promise.all([again, meanwhile])
  assert.deepEqual(asked, ['journal', 'journal'], 'two asks while one load is out are that load')
  assert.equal(views.view('journal'), Journal)
  assert.equal(views.failed('journal'), false)

  await views.ensure('journal')
  assert.equal(asked.length, 2, 'a view that has arrived is not asked for again')
}

{
  // `retry` is the desktop's "Try again": a fresh load, not the old answer.
  let failing = true
  const views = new Views(async () => {
    if (failing) throw new TypeError('Failed to fetch dynamically imported module')
    return [{ default: {} }, null]
  })
  await views.ensure('journal')
  failing = false
  views.retry('journal')
  assert.equal(views.failed('journal'), false)
  await views.ensure('journal')
  assert.ok(views.view('journal'), 'and it arrives')
}

// ── reloading for a newer build ──────────────────────────────────────

{
  let flushed = 0
  assert.equal(
    reloadForNewBuild(async () => {
      flushed += 1
    }),
    true,
    'the first failure reloads',
  )
  assert.equal(reloads, 0, 'not before what is outstanding has been written')
  await later()
  assert.equal(flushed, 1)
  assert.equal(reloads, 1)

  assert.equal(
    reloadForNewBuild(async () => {}),
    false,
    'a page reloaded from the same build does not reload again',
  )
  await later()
  assert.equal(reloads, 1)

  // The page came from another build than the one it last reloaded from:
  // that is an update since, and worth one reload of its own.
  stored.set('jatayu.reloadedFrom', 'http://localhost/assets/index-older.js')
  assert.equal(
    reloadForNewBuild(() => new Promise(() => {})),
    true,
  )
  await later(2200)
  assert.equal(reloads, 2, 'a write that never answers holds the reload up a moment, no longer')

  // Nowhere to note it: no way to know it has not been tried, so it is not.
  stored.clear()
  session.getItem = () => {
    throw new Error('storage refused')
  }
  assert.equal(
    reloadForNewBuild(async () => {}),
    false,
  )
  await later()
  assert.equal(reloads, 2)
}

console.error = logged
await close()
console.log('views-store: all checks passed')
