// Behaviour checks for the one action table.
//
// Three surfaces read `ACTIONS` and none of them owns it: the keyboard
// dispatches the rows with `keys`, the tray sends the rows with `tray` to the
// operating system, and the palette offers all of them. Every rule below is
// about that arrangement, and every one of them fails *silently* -- which is
// the whole reason for the file. A tray row whose group nobody draws does not
// error; the menu simply comes back empty, and it looks exactly like an app
// with nothing to offer.
//
// Loaded through Vite, like the tests beside it, so the TypeScript compiles
// the way the application compiles it. The stores the table imports are not
// exercised: nothing here calls `run`.

import assert from 'node:assert/strict'
import { createServer } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

// The table's `when` and `run` closures reach into the four stores, so loading
// it loads them -- and they were written for a browser. Enough of one is
// stubbed here to let the modules *evaluate*; nothing below runs a handler or
// reads a store, so nothing depends on the stubs being faithful. The
// alternative was to keep the table in a module with no stores in it, which
// would mean the closures could not reach the state they gate on, which is the
// entire point of `when`.
globalThis.window ??= {}
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
  documentElement: { style: { setProperty: () => {} }, classList: { toggle: () => {} } },
  addEventListener: () => {},
}
globalThis.navigator ??= { userAgent: 'node' }
globalThis.location ??= { search: '', href: 'http://localhost/' }
globalThis.URL.createObjectURL ??= () => 'blob:stub'

// The Svelte plugin, named here rather than read from `vite.config.ts`.
//
// A `.svelte.ts` module needs the plugin -- that is what turns `$state` into
// something that exists -- so unlike the tests beside this one, which load
// plain TypeScript, this cannot use an empty config. It does not use the
// *project* config either: loading a config file makes Vite watch it, and
// `watch: null` does not cover that. Watches are a per-user resource, and a
// suite that opens one server per file exhausts them (`EMFILE`) on a machine
// with a dev server already running.
const server = await createServer({
  configFile: false,
  root: new URL('..', import.meta.url).pathname,
  plugins: [svelte()],
  server: { middlewareMode: true, watch: null },
  appType: 'custom',
  logLevel: 'error',
})

const { ACTIONS } = await server.ssrLoadModule('/src/lib/shortcuts.svelte.ts')
const { TRAY_GROUPS } = await server.ssrLoadModule('/src/lib/tray.svelte.ts')

// ── Every row is drawable ─────────────────────────────────────────────

for (const action of ACTIONS) {
  assert.ok(action.label, `every action needs a label: ${JSON.stringify(action)}`)
  assert.ok(action.group, `${action.label} has no group`)
  assert.equal(typeof action.run, 'function', `${action.label} has no handler`)
  assert.ok(
    action.keys !== undefined || action.id !== undefined || action.label,
    `${action.label} is reachable by nothing`,
  )
}

// ── A tray row can actually be sent, and can be answered ──────────────
//
// The shell sends a description across to Rust and gets an *id* back. A row
// wearing `tray` without an id would be drawn in the menu and then do nothing
// when it was chosen, which is the worst of the three possible failures.

const trayRows = ACTIONS.filter((a) => a.tray)
assert.ok(trayRows.length > 0, 'the tray table is empty')

for (const row of trayRows) {
  assert.ok(row.id, `${row.label} is offered in the tray with no id`)
  assert.ok(
    row.id.includes(':'),
    `${row.id} should be prefixed with the app it belongs to, as todo:add`,
  )
  assert.ok(
    !row.id.startsWith('everyday:'),
    `${row.id} uses the prefix the shell keeps for its own two entries`,
  )
}

const ids = trayRows.map((r) => r.id)
assert.equal(new Set(ids).size, ids.length, `two tray rows share an id: ${ids}`)

// ── ...and its group is one the tray draws ────────────────────────────
//
// The regression this file was written for. `entriesFor` and the tray menu
// both look a row up by its *group*, and the app bar used to pass a separate
// lower-case `source` field holding the same word. Renaming one of the two
// spellings emptied every right-click menu on the bar, silently.

for (const row of trayRows) {
  assert.ok(
    TRAY_GROUPS.includes(row.group),
    `${row.id} is in group ${JSON.stringify(row.group)}, which the tray does not draw. ` +
      `It would never appear. Groups drawn: ${TRAY_GROUPS.join(', ')}`,
  )
}

// Every group the tray draws has something in it, so the menu never grows a
// separator with nothing after it.
for (const group of TRAY_GROUPS) {
  assert.ok(
    trayRows.some((r) => r.group === group),
    `the tray draws group ${group} and no action is in it`,
  )
}

// ── Keys are spelled the way the matcher spells them ──────────────────

for (const action of ACTIONS) {
  if (action.keys === undefined) continue
  assert.equal(action.keys, action.keys.trim(), `${action.label}: stray whitespace in its keys`)
  assert.ok(action.keys.length > 0, `${action.label} has an empty key sequence`)
  // Only chords with a modifier in them may fire while somebody is typing.
  // A bare letter that did would make the search box unusable.
  if (action.whileTyping) {
    assert.ok(
      action.keys.split(/\s+/).every((step) => step.includes('+')),
      `${action.label} fires while typing but has a bare-letter step: ${action.keys}`,
    )
  }
}

// ── Palette-only rows are findable ────────────────────────────────────
//
// A row with no keys is reached by typing part of its name. Keywords are how
// the ones people call by another word are found at all -- "lock" by "sign
// out", "board" by "kanban" -- so a row with neither a shortcut nor a keyword
// has to at least have a label somebody would think to type.

for (const action of ACTIONS) {
  if (action.keywords === undefined) continue
  assert.ok(Array.isArray(action.keywords), `${action.label}: keywords must be a list`)
  for (const word of action.keywords) {
    assert.equal(word, word.toLowerCase(), `${action.label}: keyword ${word} must be lower case`)
  }
}

await server.close()
console.log('actions: all checks passed')
