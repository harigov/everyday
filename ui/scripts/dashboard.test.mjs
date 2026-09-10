// Behaviour checks for the Overview's layout rules.
//
// Same reasoning as `overview.test.mjs` beside it: the type checker covers
// most of the interface, and what it cannot cover is a rule whose failures are
// quiet. Nothing below would throw and nothing below would fail to compile.
//
// The three that would actually cost somebody something:
//
//   losing a page    `parseLayout` reads a string that outlives builds. A
//                    widget dropped from the catalogue, a size renamed, a
//                    field written by a newer version — every one of those
//                    arrives as an object this build has never seen, and the
//                    tempting answer (reject the lot) throws away a page
//                    somebody arranged card by card.
//   duplicate ids    two cards with one key is a render error, and the input
//                    is a file a person can edit.
//   over-fetching    `needsOf` is what decides which queries the app makes.
//                    A need declared on a widget nobody added is a round trip
//                    per refresh for a card that is not on the page, and it is
//                    invisible: the page looks right and the vault is busier
//                    than it should be.
//
// No test framework, deliberately: one dependency-free file, run by
// `npm run check`, with the TypeScript loaded through Vite so it compiles
// exactly as the application compiles it.

import { createServer } from 'vite'

const server = await createServer({
  configFile: false,
  root: new URL('..', import.meta.url).pathname,
  // `watch: null` because a test loads a module once and exits. See
  // `overview.test.mjs` for what a watcher per test file costs.
  server: { middlewareMode: true, watch: null },
  appType: 'custom',
  logLevel: 'error',
})

const {
  addWidget,
  defaultLayout,
  moveWidget,
  needsOf,
  nextId,
  parseLayout,
  removeWidget,
  reorderWidget,
  setDays,
  setSize,
  SPAN,
  trackerWindow,
  WIDGETS,
  WIDGET_SIZES,
  WIDGET_TYPES,
} = await server.ssrLoadModule('/src/lib/dashboard.ts')

let failed = 0
function check(what, got, want) {
  const a = JSON.stringify(got)
  const b = JSON.stringify(want)
  if (a === b) return
  failed++
  console.error(`✗ ${what}\n  got  ${a}\n  want ${b}`)
}

function ok(what, condition) {
  if (condition) return
  failed++
  console.error(`✗ ${what}`)
}

const ids = (list) => list.map((w) => w.id)

// ── the catalogue is internally consistent ─────────────────────────────
//
// Every one of these is a thing a new entry can get wrong in a way nothing
// else would notice until the card was on somebody's page.

for (const type of WIDGET_TYPES) {
  const spec = WIDGETS[type]
  ok(`${type} arrives at a size it is allowed to be`, spec.sizes.includes(spec.size))
  ok(
    `${type} offers only real sizes`,
    spec.sizes.every((s) => WIDGET_SIZES.includes(s)),
  )
  ok(`${type} needs something`, spec.needs.length > 0)
  ok(`${type} says what it answers`, spec.note.length > 0 && spec.note.endsWith('.'))
  // A window with no widget behaviour behind it is a menu that does nothing.
  if (spec.windows) ok(`${type}'s windows are ascending`, isAscending(spec.windows))
}

function isAscending(list) {
  return list.every((n, i) => i === 0 || n > list[i - 1])
}

// Every size tiles the six-column grid, which is the whole reason there are
// three of them rather than a free column count.
for (const size of WIDGET_SIZES) {
  ok(`${size} divides the grid`, 6 % SPAN[size] === 0)
}

// ── the page a vault starts on ─────────────────────────────────────────

const start = defaultLayout()
ok('the default page is short enough to read', start.length <= 8)
check(
  'every default widget is a real one',
  start.filter((w) => !WIDGET_TYPES.includes(w.type)),
  [],
)
check('ids on the default page are distinct', new Set(ids(start)).size, start.length)

// ── adding, removing, moving ───────────────────────────────────────────

{
  const page = addWidget(addWidget([], 'dueToday'), 'dueToday')
  check('the same widget can go on twice', ids(page), ['dueToday:1', 'dueToday:2'])
  check('and the ids do not collide', new Set(ids(page)).size, 2)

  const gap = removeWidget(page, 'dueToday:1')
  check('removing takes exactly one', ids(gap), ['dueToday:2'])
  // The freed id is reused, which is fine and is why this is checked: it must
  // not collide with the one still there.
  check('a freed id comes back round', nextId(gap, 'dueToday'), 'dueToday:1')
}

{
  const page = ['dueToday', 'onNow', 'shelves'].reduce((list, t) => addWidget(list, t), [])
  check('moving later swaps with the next', ids(moveWidget(page, 'dueToday:1', 1)), [
    'onNow:1',
    'dueToday:1',
    'shelves:1',
  ])
  check('moving earlier swaps with the previous', ids(moveWidget(page, 'shelves:1', -1)), [
    'dueToday:1',
    'shelves:1',
    'onNow:1',
  ])
  // Clamped, never wrapped. A card at the top that jumped to the bottom
  // because somebody pressed the arrow once too often is a page that has to
  // be put back by hand, and there is no undo anywhere in this application.
  check(
    'the first card cannot move above itself',
    ids(moveWidget(page, 'dueToday:1', -1)),
    ids(page),
  )
  check('the last cannot fall off the end', ids(moveWidget(page, 'shelves:1', 5)), ids(page))
  check('an unknown id changes nothing', ids(moveWidget(page, 'nope:9', 1)), ids(page))

  // Dropping in front of a card, and dropping past the last one.
  check('a drop lands in front of its target', ids(reorderWidget(page, 'shelves:1', 'onNow:1')), [
    'dueToday:1',
    'shelves:1',
    'onNow:1',
  ])
  check('a drop on nothing goes to the end', ids(reorderWidget(page, 'dueToday:1', null)), [
    'onNow:1',
    'shelves:1',
    'dueToday:1',
  ])
  check(
    'dropping a card on itself is not a move',
    ids(reorderWidget(page, 'onNow:1', 'onNow:1')),
    ids(page),
  )
}

// ── what the page costs ────────────────────────────────────────────────

check('an empty page asks for nothing', [...needsOf([])], [])
check(
  'a page of stat tiles asks only for their counts',
  [...needsOf(addWidget(addWidget([], 'dueToday'), 'shelves'))].sort(),
  ['library', 'taskStats'],
)
check(
  'two widgets wanting the same thing ask for it once',
  [...needsOf(addWidget(addWidget([], 'habitStreaks'), 'habitsToday'))],
  ['trackerDays'],
)
// The whole catalogue at once is still a handful of queries, not one per card.
ok(
  'the union of every need is bounded',
  needsOf(WIDGET_TYPES.reduce((list, t) => addWidget(list, t), [])).size <= 10,
)

// The readings window is the longest any card asks for, never shorter than
// the floor a heatmap needs.
{
  const page = setDays(addWidget([], 'trackerChart'), 'trackerChart:1', 14)
  check('a short chart does not shrink the heatmap window', trackerWindow(page, 120), 120)
  check(
    '...and a longer one widens it',
    trackerWindow(setDays(page, 'trackerChart:1', 365), 120),
    365,
  )
}

// ── reading a stored page back ─────────────────────────────────────────

check('nothing stored means nothing to restore', parseLayout(null), null)
check('a string that is not JSON is not a page', parseLayout('{oh no'), null)
check('JSON that is not a list is not a page', parseLayout('{"a":1}'), null)
check('an empty list means nobody has arranged one', parseLayout('[]'), null)

{
  // The case this function exists for: a page written by a build that had a
  // widget this one does not. The unknown card goes; the rest of the page
  // survives, because throwing it away would lose work nobody can get back.
  const stored = JSON.stringify([
    { id: 'a', type: 'dueToday', size: 'small', subject: null, days: null },
    { id: 'b', type: 'wormhole', size: 'small', subject: null, days: null },
    { id: 'c', type: 'shelves', size: 'small', subject: null, days: null },
  ])
  check('an unknown widget is dropped, not the page', ids(parseLayout(stored)), ['a', 'c'])
}

{
  // A size the catalogue no longer offers for this type falls back to the one
  // it arrives at rather than to a span that would leave a hole in the grid.
  const stored = JSON.stringify([{ id: 'a', type: 'dueToday', size: 'enormous' }])
  check('a size nobody recognises falls back', parseLayout(stored)[0].size, WIDGETS.dueToday.size)
}

{
  const stored = JSON.stringify([{ id: 'a', type: 'habitHeatmap', size: 'small' }])
  check(
    'a size this widget is not allowed falls back too',
    parseLayout(stored)[0].size,
    WIDGETS.habitHeatmap.size,
  )
}

{
  // Two cards with one key is a render error, and this input is a file a
  // person can edit. The first keeps the id it was written with.
  const stored = JSON.stringify([
    { id: 'same', type: 'dueToday' },
    { id: 'same', type: 'onNow' },
  ])
  const page = parseLayout(stored)
  check('a repeated id is renamed rather than refused', page.length, 2)
  check('...and the two are distinct', new Set(ids(page)).size, 2)
  check('...and the first keeps its own', page[0].id, 'same')
}

{
  const stored = JSON.stringify([
    { type: 'trackerChart', days: 9999 },
    { type: 'trackerChart', days: 30 },
    { type: 'dueToday', days: 30 },
  ])
  const page = parseLayout(stored)
  check('a window nobody offers falls back to the default', page[0].days, 90)
  check('...and one that is offered is kept', page[1].days, 30)
  check('...and a widget with no windows keeps none', page[2].days, null)
  check('a missing id is minted', new Set(ids(page)).size, 3)
}

{
  const stored = JSON.stringify([{ type: 'habitHeatmap', subject: '' }, { type: 'habitHeatmap' }])
  const page = parseLayout(stored)
  check('an empty subject is no subject', page[0].subject, null)
  check('...and so is a missing one', page[1].subject, null)
}

// A page written by this build reads back as itself. The round trip is the
// thing that actually has to hold; everything above is a way it can fail.
{
  const page = setSize(
    setDays(
      addWidget(addWidget(defaultLayout(), 'trackerChart'), 'habitHeatmap'),
      'trackerChart:1',
      14,
    ),
    'habitHeatmap:1',
    'medium',
  )
  check('a page survives a round trip', parseLayout(JSON.stringify(page)), page)
}

await server.close()
if (failed > 0) {
  console.error(`\n${failed} check${failed === 1 ? '' : 's'} failed`)
  process.exit(1)
}
console.log('dashboard: ok')
