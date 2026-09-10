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
// Counted and answerable, not merely stubbed: the checks at the foot of this
// file are about *how often* the table asks the document whether a dialog is
// open, and about it believing the answer afterwards.
let dialogQueries = 0
let dialogIsOpen = false
globalThis.document ??= {
  querySelector: (selector) => {
    if (selector === '[aria-modal="true"]') {
      dialogQueries += 1
      return dialogIsOpen ? { tagName: 'DIV' } : null
    }
    return null
  },
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

const { ACTIONS, GROUPS, shortcuts } = await server.ssrLoadModule('/src/lib/shortcuts.svelte.ts')
const { app } = await server.ssrLoadModule('/src/lib/state.svelte.ts')

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

// ── ...and its heading is one the surfaces know how to order ──────────
//
// The regression this file was written for. `entriesFor` and the tray menu
// both look a row up by its *group*, and the app bar used to pass a separate
// lower-case `source` field holding the same word. Renaming one of the two
// spellings emptied every right-click menu on the bar, silently.
//
// `GROUPS` is now the single ordered list -- the help sheet reads it and the
// tray composes from it -- so a row filed under a heading that is not in it
// is drawn by neither.

for (const action of ACTIONS) {
  assert.ok(
    GROUPS.includes(action.group),
    `${action.label} is in group ${JSON.stringify(action.group)}, which no surface orders. ` +
      `It would never appear. Groups drawn: ${GROUPS.join(', ')}`,
  )
}

// A heading listed twice would draw twice, with the rows split between them.
assert.equal(new Set(GROUPS).size, GROUPS.length, `a heading is listed twice in GROUPS`)

// Every app's heading has at least one tray row under it, so a right-click on
// a tab in the app bar offers something. `Everywhere` and `Go to` are the two
// headings that belong to no app: they are keyboard and palette rows, and the
// tray skips them for being empty rather than drawing a stray separator.
for (const group of GROUPS) {
  if (group === 'Everywhere' || group === 'Go to') continue
  assert.ok(
    trayRows.some((r) => r.group === group),
    `the app bar offers group ${group} and no tray action is in it`,
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

// ── One key press asks the document about dialogs once ────────────────
//
// Whether a dialog is over the window is answered from the DOM, because
// `panels` only knows about the two dialogs it owns and every other modal in
// the application is raised by a component (see `dialogOpen`). That is the
// right source. Asking it once per *binding* is not: `match` runs `when` on
// every candidate row, most of those `when`s are `anywhere`, and the handler
// runs on every keystroke -- including every keystroke typed into an editor,
// where the answer is the same `false` several times in a row.
//
// Measured on a long journal entry, that repeated query was the largest piece
// of this application's own code on the typing path. It is now answered once
// per sweep of the table, which is what this pins down: bind the number to a
// press rather than to how many rows happen to be gated on `anywhere`, or it
// will drift back up the next time a shortcut is added.

const press = (key, extra = {}) => {
  let prevented = false
  shortcuts.press({
    key,
    ctrlKey: false,
    metaKey: false,
    shiftKey: false,
    altKey: false,
    defaultPrevented: false,
    target: null,
    preventDefault: () => (prevented = true),
    ...extra,
  })
  return prevented
}

app.screen = 'main'

dialogQueries = 0
press('q')
assert.ok(
  dialogQueries <= 1,
  `a single key press asked the document about dialogs ${dialogQueries} times; ` +
    `it must ask at most once per sweep of the table`,
)

// The caret being in a text field narrows the table to the `whileTyping` rows
// and must not widen the question: this is the press that happens hundreds of
// times a minute.
dialogQueries = 0
press('q', { target: { tagName: 'TEXTAREA' } })
assert.ok(
  dialogQueries <= 1,
  `a key typed into a field asked the document about dialogs ${dialogQueries} times`,
)

// ...and the answer is not kept between presses. A dialog opened by one
// keystroke has to be seen by the next, or a bare letter would go on reaching
// the window behind it -- the bug `dialogOpen` was written to fix.
dialogQueries = 0
press('q')
press('q')
assert.equal(dialogQueries, 2, 'the dialog question must be asked afresh on each press')

// ── ...and nothing outside a press is answered from it ────────────────
//
// The scope belongs to the key press that opened it, and `when` is documented
// as being re-read every time somebody looks. Those two only agree if the
// sharing is confined to the sweep: a press must not leave an answer lying
// about for the next reader that is not a press.
//
// The other readers are the tray composing its menu and the app bar building
// a right-click menu, both through `trayEntries`, which calls `when` outside
// any sweep. Neither trips this today -- every `tray` row is gated on the
// screen and a capability, and none of them asks about dialogs -- so what is
// checked here is the mechanism rather than one route through it. That is
// deliberate: the rows that do ask are thirty-two of the keyboard's, the
// pairing of tray rows with `when`s is not fixed, and the failure when it does
// happen is a menu built from what was on screen at some unrelated earlier
// moment. A dialog dismissed with the mouse leaves no keystroke behind to put
// that right.

const asksAboutDialogs = ACTIONS.find((a) => {
  if (!a.when) return false
  dialogQueries = 0
  a.when()
  return dialogQueries > 0
})
assert.ok(asksAboutDialogs, 'no row consults the document about dialogs; has the gate moved?')

dialogIsOpen = false
press('q') // opens a sweep, and must not leave its answer behind

dialogIsOpen = true
dialogQueries = 0
const applies = asksAboutDialogs.when()
assert.ok(dialogQueries > 0, 'a reader outside a press was answered from the press before it')
assert.equal(
  applies,
  false,
  `${asksAboutDialogs.label} still applied with a dialog over the window`,
)

dialogIsOpen = false
assert.equal(
  asksAboutDialogs.when(),
  true,
  `${asksAboutDialogs.label} stayed inapplicable after the dialog closed`,
)

await server.close()
console.log('actions: all checks passed')
