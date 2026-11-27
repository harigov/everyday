// `app.goTo` (`lib/state.svelte.ts`) waits for the app's view to load
// (`lib/views.svelte.ts`) before it switches, and that wait is a disk read on
// a desktop and a round trip on web access. Whatever happened during it wins
// over the older ask: another app chosen, or the vault locked. And a view
// that could not be loaded is not "on screen" for the caller to act on.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

stubBrowser({
  window: true,
  localStorage: true,
  matchMedia: true,
  document: true,
  navigator: true,
  location: true,
  createObjectURL: () => 'blob:stub',
})

const {
  modules: [stateModule, viewsModule],
  close,
} = await load(['/src/lib/state.svelte.ts', '/src/lib/views.svelte.ts'], { svelte: true })
const { app } = stateModule
const { views } = viewsModule

// The view loads are in the test's hands: each `ensure` waits until it is
// let go, and `view` answers for what has been let go as arrived.
const arrived = new Set()
let pending = []
views.ensure = (key) =>
  new Promise((resolve) => {
    pending.push((ok) => {
      if (ok) arrived.add(key)
      resolve()
    })
  })
views.view = (key) => (arrived.has(key) ? {} : undefined)
const settle = async (ok = true) => {
  const out = pending
  pending = []
  for (const go of out) go(ok)
  await new Promise((resolve) => setTimeout(resolve, 0))
}

app.status = {
  name: 'Test',
  backend: 'sqlite',
  unlocked: true,
  encrypted: true,
  autoLockSeconds: 0,
  forgetKeySeconds: 0,
  path: '/tmp/x',
  writable: true,
  capabilities: { notes: true, tasks: true },
}
app.screen = 'main'
app.setSection('journal')

{
  const going = app.goTo('notes')
  await settle()
  assert.equal(await going, true, 'a view that arrives is switched to')
  assert.equal(app.section, 'notes')
}

{
  arrived.clear()
  app.setSection('journal')
  const going = app.goTo('todo')
  // While its view loads, somebody picks another app.
  app.setSection('notes')
  await settle()
  assert.equal(await going, false, 'the older ask gives way')
  assert.equal(app.section, 'notes', 'and does not pull them off the app they chose')
}

{
  arrived.clear()
  app.setSection('journal')
  const first = app.goTo('todo')
  const second = app.goTo('notes')
  await settle()
  assert.equal(await first, false, 'of two asks, the later wins')
  assert.equal(await second, true)
  assert.equal(app.section, 'notes')
}

{
  arrived.clear()
  app.setSection('journal')
  const going = app.goTo('notes')
  // The vault locks while it loads, and is unlocked again before it lands.
  await app.lockedElsewhere()
  app.screen = 'main'
  await settle()
  assert.equal(await going, false, 'a lock in between ends the ask')
  assert.equal(app.section, 'journal')
}

{
  arrived.clear()
  app.setSection('journal')
  const going = app.goTo('notes')
  await settle(false)
  assert.equal(await going, false, 'a view that could not be loaded is nothing to act on')
  assert.equal(app.section, 'notes', 'but the app is shown, so the stage can say what went wrong')
}

await close()
console.log('app-goto: all checks passed')
