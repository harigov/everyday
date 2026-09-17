// The regression net for Phase 3 of docs/plans/architecture-refactor.md:
// pulling the per-app facts scattered across `state.svelte.ts`,
// `AppBar.svelte`, `App.svelte` and `shortcuts.svelte.ts` into one registry,
// `lib/apps.ts`.
//
// Everything below is a literal, by-hand transcription of what those files
// said *before* the registry existed -- the tray menu each app offers, the
// keyboard table's ids/keys/labels/groups in order, and the app bar's own
// order, labels, icons and accents. The whole point of a registry is that a
// later change to any of those is a one-line edit in `apps.ts`; this file is
// what notices if that edit changed something nobody meant to touch.
//
// `tray.entriesFor` reads live state through `when()`, so the state a vault
// with every domain switched on would have is built by hand here rather than
// through a real unlock -- the same reason `actions.test.mjs` beside this
// file stubs a document instead of mounting one.

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
  modules: [shortcutsModule, stateModule, trayModule, appsModule],
  close,
} = await load(
  [
    '/src/lib/shortcuts.svelte.ts',
    '/src/lib/state.svelte.ts',
    '/src/lib/tray.svelte.ts',
    '/src/lib/apps.ts',
  ],
  { svelte: true },
)
const { ACTIONS, GROUPS } = shortcutsModule
const { app } = stateModule
const { tray } = trayModule
const { APPS, APP_ORDER } = appsModule

// A vault with every domain on, so every app's tray row and "Go to" binding
// is offered -- the state `canShow` needs to say yes to everything.
const FULL_CAPABILITIES = {
  blobs: true,
  transactional: true,
  humanReadable: true,
  tasks: true,
  calendars: true,
  library: true,
  trackers: true,
  purpose: true,
  notes: true,
  routines: true,
  proposals: true,
  agent: true,
  accounts: true,
  mail: true,
}
const FULL_STATUS = {
  name: 'Test',
  backend: 'sqlite',
  unlocked: true,
  encrypted: true,
  autoLockSeconds: 0,
  forgetKeySeconds: 0,
  path: '/tmp/x',
  writable: true,
  capabilities: FULL_CAPABILITIES,
}

app.screen = 'main'
app.journals = [{ id: 'j1', name: 'Journal', color: null }]
app.status = FULL_STATUS

// ── the app registry itself ─────────────────────────────────────────

assert.deepEqual(
  APP_ORDER,
  ['overview', 'notes', 'todo', 'calendar', 'library', 'mail', 'assistant', 'journal'],
  'APP_ORDER must match the order app.nextSection has always cycled in',
)

const EXPECTED_APPS = {
  overview: { label: 'Overview', icon: 'compass' },
  notes: { label: 'Notes', icon: 'pencil' },
  todo: { label: 'Todo', icon: 'check' },
  calendar: { label: 'Calendar', icon: 'calendar' },
  library: { label: 'Library', icon: 'book' },
  mail: { label: 'Mail', icon: 'mail' },
  assistant: { label: 'Assistant', icon: 'sparkle' },
  journal: { label: 'Journal', icon: 'quote' },
}
for (const [section, want] of Object.entries(EXPECTED_APPS)) {
  const got = APPS[section]
  assert.equal(got.label, want.label, `${section}'s label changed`)
  assert.equal(got.icon, want.icon, `${section}'s icon changed`)
}

// The accent: `journal`, `todo` and `library` read a live colour; every
// other app is the vault's own accent regardless of what it is handed.
const accentDeps = { app: { accent: 'J' }, todo: { accent: 'T' }, library: { accent: 'L' } }
const EXPECTED_ACCENTS = {
  overview: 'var(--accent)',
  notes: 'var(--accent)',
  todo: 'T',
  calendar: 'var(--accent)',
  library: 'L',
  mail: 'var(--accent)',
  assistant: 'var(--accent)',
  journal: 'J',
}
for (const [section, want] of Object.entries(EXPECTED_ACCENTS)) {
  assert.equal(APPS[section].accent(accentDeps), want, `${section}'s accent changed`)
}

// `canShow` now reads the registry; it must still say no to everything but
// the journal until the vault says otherwise, and yes to everything once it
// has -- the same two ends `state.svelte.ts`'s own switch used to cover.
app.status = null
for (const section of APP_ORDER) {
  assert.equal(
    app.canShow(section),
    section === 'journal',
    `${section} should ${section === 'journal' ? '' : 'not '}be offered with no vault open`,
  )
}

// ── the keyboard table ──────────────────────────────────────────────

const EXPECTED_GROUPS = [
  'Everywhere',
  'Go to',
  'Journal',
  'Notes',
  'Todo',
  'Calendar',
  'Library',
  'Mail',
  'Overview',
  'Assistant',
  'Vault',
]
assert.deepEqual(GROUPS, EXPECTED_GROUPS, 'GROUPS must keep the tray/help-sheet menu order')

const actionShape = (a) => ({
  id: a.id ?? null,
  keys: a.keys ?? null,
  label: a.label,
  group: a.group,
  tray: a.tray === true,
})

// The eight "Go to" rows plus "the next app", in the order they have always
// been declared -- deliberately not `APP_ORDER`, since the app bar's own
// order starts at the journal rather than the overview. See `apps.ts`'s own
// note on why they stay hand-written.
const EXPECTED_GO_TO = [
  { id: null, keys: 'g j', label: 'Journal', group: 'Go to', tray: false },
  { id: null, keys: 'g n', label: 'Notes', group: 'Go to', tray: false },
  { id: null, keys: 'g t', label: 'Todo', group: 'Go to', tray: false },
  { id: null, keys: 'g c', label: 'Calendar', group: 'Go to', tray: false },
  { id: null, keys: 'g l', label: 'Library', group: 'Go to', tray: false },
  { id: null, keys: 'g m', label: 'Mail', group: 'Go to', tray: false },
  { id: null, keys: 'g o', label: 'Overview', group: 'Go to', tray: false },
  { id: null, keys: 'g a', label: 'Assistant', group: 'Go to', tray: false },
  { id: null, keys: 'mod+j', label: 'The next app', group: 'Go to', tray: false },
]
assert.deepEqual(ACTIONS.filter((a) => a.group === 'Go to').map(actionShape), EXPECTED_GO_TO)

// Every tray row, in table order, id/key/label/group intact.
const EXPECTED_TRAY_ROWS = [
  { id: 'journal:new-entry', keys: null, label: 'New journal entry', group: 'Journal', tray: true },
  { id: 'todo:add', keys: null, label: 'Add a task', group: 'Todo', tray: true },
  {
    id: 'calendar:book-now',
    keys: null,
    label: 'Set an hour aside',
    group: 'Calendar',
    tray: true,
  },
  { id: 'calendar:timer', keys: null, label: 'Track time', group: 'Calendar', tray: true },
  { id: 'overview:page', keys: null, label: 'How things are going', group: 'Overview', tray: true },
  { id: 'overview:log', keys: null, label: 'Record a reading', group: 'Overview', tray: true },
  { id: 'todo:goals', keys: null, label: 'Goals', group: 'Todo', tray: true },
  { id: 'library:add', keys: null, label: 'Add to library', group: 'Library', tray: true },
  { id: 'mail:compose', keys: null, label: 'Compose a message', group: 'Mail', tray: true },
  {
    id: 'assistant:runs',
    keys: null,
    label: 'What the assistant did',
    group: 'Assistant',
    tray: true,
  },
  {
    id: 'assistant:new-routine',
    keys: null,
    label: 'New routine',
    group: 'Assistant',
    tray: true,
  },
  { id: 'notes:new', keys: null, label: 'New note', group: 'Notes', tray: true },
  { id: 'vault:lock', keys: null, label: 'Lock this screen', group: 'Vault', tray: true },
  {
    id: 'vault:lock-all',
    keys: null,
    label: 'Lock the vault everywhere',
    group: 'Vault',
    tray: true,
  },
]
assert.deepEqual(ACTIONS.filter((a) => a.tray === true).map(actionShape), EXPECTED_TRAY_ROWS)

// ── the tray menu each app offers, fully booted ────────────────────

const trayShape = (e) => ({ id: e.id, label: e.label })

const EXPECTED_TRAY_MENUS = {
  Journal: [{ id: 'journal:new-entry', label: 'New journal entry' }],
  Notes: [{ id: 'notes:new', label: 'New note' }],
  Todo: [
    { id: 'todo:add', label: 'Add a task' },
    { id: 'todo:goals', label: 'Goals' },
  ],
  Calendar: [
    { id: 'calendar:book-now', label: 'Set an hour aside' },
    { id: 'calendar:timer', label: 'Track time' },
  ],
  Library: [{ id: 'library:add', label: 'Add to library' }],
  Mail: [{ id: 'mail:compose', label: 'Compose a message' }],
  Overview: [
    { id: 'overview:page', label: 'How things are going' },
    { id: 'overview:log', label: 'Record a reading' },
  ],
  Assistant: [
    { id: 'assistant:runs', label: 'What the assistant did' },
    { id: 'assistant:new-routine', label: 'New routine' },
  ],
  Vault: [
    { id: 'vault:lock', label: 'Lock this screen' },
    { id: 'vault:lock-all', label: 'Lock the vault everywhere' },
  ],
  // Neither belongs to an app; both come back empty, the way `actions.test`
  // already checks the tray skips them rather than drawing an empty group.
  Everywhere: [],
  'Go to': [],
}

app.status = FULL_STATUS

for (const group of GROUPS) {
  assert.deepEqual(
    tray.entriesFor(group).map(trayShape),
    EXPECTED_TRAY_MENUS[group],
    `${group}'s tray menu changed`,
  )
}

await close()
console.log('apps: all checks passed')
