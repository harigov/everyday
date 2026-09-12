// Behaviour checks for the context menus: where one goes, and what one
// containing conditional rows looks like once the conditions are applied.
//
// The same reasoning as the other four files here. Almost all of a menu is
// checked by the compiler and by looking at it, and these two parts are
// neither: a menu raised in the bottom-right corner of the window is the
// case nobody tries by hand, and a menu built from a list half of whose rows
// are `false` is the case nobody notices -- a rule across the top of a menu
// or a heading over nothing is not an error, it is just slightly wrong,
// every time, for as long as it takes somebody to mention it.
//
// No test framework, deliberately -- one dependency-free file, run by
// `npm run check`. The TypeScript is loaded through Vite so it is compiled
// exactly as the application compiles it.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: menuLib, close } = await load('/src/lib/menu.ts')
const { MENU_MARGIN, SEP, placeMenu, placeSubmenu, tidyMenu } = menuLib

const SCREEN = { width: 1000, height: 800 }
const SIZE = { width: 200, height: 300 }

// ── placing a menu ────────────────────────────────────────────────────

// Room below and to the right: it opens from the pointer, which is where
// every platform puts it.
assert.deepEqual(placeMenu({ x: 40, y: 50 }, SIZE, SCREEN), { x: 40, y: 50 })

// Against the right edge, it opens leftwards rather than sliding back --
// sliding would put the pointer inside the menu, over an item nobody chose.
assert.deepEqual(placeMenu({ x: 960, y: 50 }, SIZE, SCREEN), { x: 760, y: 50 })

// The same at the bottom, and both at once in the corner: the case a menu
// raised on the last row of a list is always in.
assert.deepEqual(placeMenu({ x: 40, y: 700 }, SIZE, SCREEN), { x: 40, y: 400 })
assert.deepEqual(placeMenu({ x: 960, y: 700 }, SIZE, SCREEN), { x: 760, y: 400 })

// Flipping is only worth it if the flipped menu is actually on screen. Near
// the left edge of a narrow window there is no room either way, so it is
// clamped -- and never off the top or left, which would be unreachable.
{
  const at = placeMenu({ x: 100, y: 60 }, SIZE, { width: 180, height: 800 })
  assert.equal(at.x, MENU_MARGIN)
}
{
  // Taller than the window: the top of it is the part worth showing.
  const at = placeMenu({ x: 40, y: 400 }, { width: 200, height: 900 }, SCREEN)
  assert.equal(at.y, MENU_MARGIN)
}

// ── placing a submenu ─────────────────────────────────────────────────

const PARENT = { left: 300, right: 480, top: 200 }

// Out of the right-hand edge of its parent, level with the row that opened
// it, which is the only place it reads as belonging to that row.
assert.deepEqual(placeSubmenu(PARENT, SIZE, SCREEN), { x: 480, y: 200 })

// No room on the right: it opens back across its parent instead.
assert.deepEqual(placeSubmenu({ left: 700, right: 880, top: 200 }, SIZE, SCREEN), {
  x: 500,
  y: 200,
})

// Level with its row where it can be, slid up where it cannot -- but never
// flipped: the first item of a submenu has to stay near the row it came
// from.
assert.deepEqual(placeSubmenu({ ...PARENT, top: 700 }, SIZE, SCREEN), {
  x: 480,
  y: 800 - MENU_MARGIN - 300,
})

// ── tidying a built menu ──────────────────────────────────────────────

const open = { label: 'Open' }
const del = { label: 'Delete' }

// The ordinary case is left exactly as written.
assert.deepEqual(tidyMenu([open, SEP, del]), [open, SEP, del])

// A row dropped by its condition takes its rule with it, rather than leaving
// the menu opening or ending with a line across it.
assert.deepEqual(tidyMenu([false, SEP, open, SEP, null]), [open])
assert.deepEqual(tidyMenu([SEP, SEP, open, SEP, SEP, del, SEP]), [open, SEP, del])

// A heading is a label for the group under it, so it goes when the group
// does -- and is never ruled off from what it names.
assert.deepEqual(tidyMenu([{ kind: 'heading', label: 'Move to' }, false]), [])
assert.deepEqual(
  tidyMenu([{ kind: 'heading', label: 'Move to' }, SEP, open]),
  [{ kind: 'heading', label: 'Move to' }, open],
  'a separator between a heading and its group would rule off the heading',
)

// Two headings in a row means the first named nothing.
assert.deepEqual(
  tidyMenu([{ kind: 'heading', label: 'Plan' }, { kind: 'heading', label: 'Record' }, open]),
  [{ kind: 'heading', label: 'Record' }, open],
)

await close()
console.log('menu: all checks passed')
