// The Assistant app's own rules, without a browser.
//
// Two things worth checking without a store: which panes a vault can show,
// and the words a trigger says for itself in the mock -- which has to agree
// with what the core says, or a routine reads one way in the demo build and
// another in the real one.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

// Enough of a browser for the modules to evaluate. Nothing here is called.
stubBrowser({
  window: true,
  localStorage: { getItem: () => null, setItem: () => {} },
  matchMedia: () => ({ matches: false, addEventListener() {} }),
  document: { documentElement: { style: { setProperty() {} } } },
  location: true,
  crypto: { randomUUID: () => 'x' },
})

const {
  modules: [assistant, mock],
  close,
} = await load(['/src/lib/assistant.svelte.ts', '/src/lib/mock.ts'], {
  svelte: { compilerOptions: { hmr: false } },
})
const { PANES, PANE_LABELS } = assistant

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

const { mockInvoke } = mock
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

await close()
console.log('assistant: all checks passed')
