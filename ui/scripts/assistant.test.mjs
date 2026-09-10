// The Assistant app's own rules, without a browser.
//
// Two things worth checking without a store: which panes a vault can show,
// and the words a trigger says for itself in the mock -- which has to agree
// with what the core says, or a routine reads one way in the demo build and
// another in the real one.

import assert from 'node:assert/strict'
import { createServer } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

const server = await createServer({
  configFile: false,
  plugins: [svelte({ compilerOptions: { hmr: false } })],
  // A watcher per test file exhausts inotify on a machine running the suite.
  server: { middlewareMode: true, watch: null },
  appType: 'custom',
  logLevel: 'error',
})

// Enough of a browser for the modules to evaluate. Nothing here is called.
globalThis.window ??= {}
globalThis.localStorage ??= { getItem: () => null, setItem: () => {} }
globalThis.matchMedia ??= () => ({ matches: false, addEventListener() {} })
globalThis.document ??= { documentElement: { style: { setProperty() {} } } }
globalThis.location ??= { search: '', href: 'http://localhost/' }
globalThis.crypto ??= { randomUUID: () => 'x' }

const { PANES, PANE_LABELS } = await server.ssrLoadModule('/src/lib/assistant.svelte.ts')

// ── Every pane has a name, and the names are what the sidebar draws ────

assert.deepEqual(PANES, ['runs', 'routines', 'memory'])
for (const pane of PANES) {
  assert.ok(PANE_LABELS[pane], `${pane} has no label`)
}

// The first pane is what the app opens on, and it is the one the count on the
// app bar points at: a number that led somewhere else would be a number
// nobody could clear by following it.
assert.equal(PANES[0], 'runs')

// ── The mock spells a trigger the way the core does ────────────────────

const { mockInvoke } = await server.ssrLoadModule('/src/lib/mock.ts')
// The demo vault starts locked, as a real one does.
await mockInvoke('unlock', { password: 'everyday' })

// The wording is not exported, so it is exercised through the command it backs.
const routines = await mockInvoke('list_routines', {})
const brief = routines.find((r) => r.name === 'Morning brief')
assert.ok(brief, 'the demo build has a morning brief on it')
assert.equal(
  brief.when,
  'Weekdays at 07:00',
  'the mock must say what `Trigger::describe` says, or a routine reads two ways',
)

const review = routines.find((r) => r.name === 'Weekly review')
assert.equal(review.when, 'fri at 17:00')
assert.equal(review.nextDue, undefined, 'a routine that is switched off has no next run')

// ── A run that nobody has looked at is what the badge counts ───────────

assert.equal(await mockInvoke('unseen_runs', {}), 1)
await mockInvoke('mark_runs_seen', { ids: [] })
assert.equal(await mockInvoke('unseen_runs', {}), 0, 'an empty list means all of them')

await server.close()
console.log('assistant: all checks passed')
