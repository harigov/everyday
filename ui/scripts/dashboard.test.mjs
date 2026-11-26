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

import { load, makeCheck } from './harness.mjs'

const { module: dashboard, close } = await load('/src/lib/dashboard.ts')
const {
  activityWindows,
  addWidget,
  aqiTier,
  byTimeOrLast,
  dayAgenda,
  defaultLayout,
  eventPhase,
  fillRows,
  importantThreads,
  introduce,
  INTRODUCED,
  moveWidget,
  needsOf,
  nextId,
  NOW_SHELVES,
  nowShelf,
  parseLayout,
  removeWidget,
  reorderWidget,
  setDays,
  setSize,
  soonestBack,
  SPAN,
  tasksForToday,
  trackerWindow,
  weatherIcon,
  widget,
  WIDGETS,
  WIDGET_SIZES,
  WIDGET_TYPES,
} = dashboard

const { check, ok, finish } = makeCheck()

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
// Nine since the brief went on top of the eight home cards: it is the one
// card that is a reading of the others rather than another thing to read.
ok('the default page is short enough to read', start.length <= 9)
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

  // A new card goes where it was put, by the same "in front of" rule a drop
  // follows -- and a place that has since gone means the end, not nowhere.
  check(
    'a new card lands in front of the one chosen',
    ids(addWidget(page, 'onNow', null, 'onNow:1')),
    ['dueToday:1', 'onNow:2', 'onNow:1', 'shelves:1'],
  )
  check(
    '...in front of the first, too',
    ids(addWidget(page, 'taskTally', null, 'dueToday:1'))[0],
    'taskTally:1',
  )
  check(
    'a place that has gone means the end',
    ids(addWidget(page, 'taskTally', null, 'nope:9')).at(-1),
    'taskTally:1',
  )
}

// ── filling rows so the grid has no holes ──────────────────────────────
//
// The bug this exists for: a layout where some row's widgets add up to less
// than six columns draws a gap nobody put there, and at a wide window it is
// not a sliver -- a lone medium card is half the page with nothing beside
// it. `fillRows` is asked to reproduce the browser's own sparse auto-flow
// (never backfilling an earlier row) and then widen only the last widget on
// each row by what is left over.

{
  // Three smalls exactly fill a row (2+2+2=6): nothing to grow.
  const page = [
    widget('dueToday', null, 'a'),
    widget('onNow', null, 'b'),
    widget('recordedToday', null, 'c'),
  ]
  check('a row that already sums to six is untouched', [...fillRows(page).values()], [2, 2, 2])
}

{
  // The regression this was written for: "Goals under way" (medium, 3) is
  // the only thing on its row because the default layout's rows above it
  // close at exactly six. Alone, it should take the whole width rather than
  // leaving the other half of a wide window blank.
  const page = [...defaultLayout(), widget('goalProgress')]
  const filled = fillRows(page)
  check('a lone medium widget on its own row fills it', filled.get('goalProgress:1'), 6)
  // The rows above it close exactly on six already, so nothing about them
  // should move.
  check(
    'a row that already closes on six keeps its members as they were',
    [filled.get('importantMail:1'), filled.get('mailFollowUps:1'), filled.get('snoozedMail:1')],
    [2, 2, 2],
  )
  check(
    "...and so does a row of two halves: today's events beside today's tasks",
    [filled.get('todayEvents:1'), filled.get('todayTasks:1')],
    [3, 3],
  )
}

{
  // A row with one column left over (3 + 2 = 5): only the last widget on
  // the row grows, and only the row's own hole, not the whole remainder of
  // the grid.
  const page = [widget('weather', null, 'w'), widget('dueToday', null, 'd')]
  check(
    'the last widget on a short row takes exactly the leftover columns',
    [...fillRows(page).values()],
    [3, 3],
  )
}

{
  // A hole mid-page, not just at the end: the row above a full row should
  // fill too, because the wrap rule that opens a new row is about what
  // *follows*, not about which row happens to be last.
  const page = [
    widget('goalTally', null, 'a'), // small, 2
    widget('habitsToday', null, 'b'), // large, 6 -- does not fit the 4 left on row a's line, wraps
    widget('goalTally', null, 'c'), // small, 2
  ]
  const filled = fillRows(page)
  check('a short row in the middle of the page fills too', filled.get('a'), 6)
  check('the row it wrapped to is unaffected by the row above it', filled.get('b'), 6)
  check('a lone widget on the last row still fills it', filled.get('c'), 6)
}

check('an empty page has nothing to fill', [...fillRows([]).values()], [])

{
  // The 1180px breakpoint draws three columns. Two smalls (2 + 2) no longer
  // share a row there, so each must fill its own row rather than leave a
  // column empty beside it; a medium already spans the whole three.
  const page = [
    widget('dueToday', null, 'a'),
    widget('onNow', null, 'b'),
    widget('weather', null, 'c'),
  ]
  check('at three columns a lone small fills its row', [...fillRows(page, 3).values()], [3, 3, 3])
  check('at one column everything is one column', [...fillRows(page, 1).values()], [1, 1, 1])
}

check(
  'every span fillRows returns is a real grid width',
  [...fillRows(WIDGET_TYPES.map((t, i) => widget(t, null, `${t}:${i}`))).values()].every(
    (span) => span >= 2 && span <= 6,
  ),
  true,
)

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
// Twelve since the weather card, whose one need is a forecast rather than
// another read of the vault; fourteen since the mail and meeting cards, which
// share two reads between eight of them; fifteen since the Feed's, whose two
// reads are one need; sixteen since "What's going on", whose three reads --
// projects, what is under way, the shelves' verbs -- are one need too;
// seventeen since "Who to get in touch with", one read of who is due;
// twenty-two since the home cards -- today's tasks, today's events, and the
// three Inbox cards, each a read of its own so a page with one of them does
// not decrypt the other two's threads. The three shelf cards share `going`.
// Twenty-four since the brief, whose one need is the briefs store's own load;
// twenty-seven since Money, Health and Screen time each brought one small
// read of their own.
ok(
  'the union of every need is bounded',
  needsOf(WIDGET_TYPES.reduce((list, t) => addWidget(list, t), [])).size <= 27,
)
check(
  'the three shelf cards share one read with "What\'s going on"',
  [...needsOf(['readingNow', 'watchingNow', 'playingNow'].reduce((l, t) => addWidget(l, t), []))],
  ['going'],
)
check(
  'each Inbox card asks only for its own read',
  ['importantMail', 'mailFollowUps', 'snoozedMail'].map((t) => [...needsOf(addWidget([], t))]),
  [['importantMail'], ['mailFollowUps'], ['snoozedMail']],
)
check(
  'the people card asks only for who is due',
  [...needsOf(addWidget([], 'peopleDue'))],
  ['people'],
)
// The Feed's card asks for the Feed and for nothing else, so a page with it on
// does not also pay for a tracker window, and a page without it pays nothing
// for the Feed.
check('the reading card asks only for the feed', [...needsOf(addWidget([], 'reading'))], ['news'])
check(
  'a page without it asks nothing of the feed',
  [...needsOf(defaultLayout())].includes('news'),
  false,
)
ok('the reading card has a size it offers', WIDGETS.reading.sizes.includes(WIDGETS.reading.size))

// The brief card asks for the briefs and nothing else -- its counts and its
// timeline are drawn from the digest the brief was written from, not from
// another read of the calendar or the Inbox.
check('the brief card asks only for the briefs', [...needsOf(addWidget([], 'brief'))], ['brief'])
check(
  'a page without it asks nothing of the briefs',
  [...needsOf(defaultLayout().filter((w) => w.type !== 'brief'))].includes('brief'),
  false,
)
check('the brief opens the default page', defaultLayout()[0]?.type, 'brief')
check('...across the whole width', fillRows(defaultLayout()).get('brief:1'), 6)
check('the brief is never narrow', WIDGETS.brief.sizes.includes('small'), false)
check(
  'a stored page keeps the reading card',
  ids(parseLayout(JSON.stringify([{ id: 'r', type: 'reading', size: 'large' }]))),
  ['r'],
)

// ── mail and meeting windows ───────────────────────────────────────────
//
// Each distinct window is a read that decrypts every message or event in it,
// so these are the rules that decide how many of those a page costs.
{
  const page = ['topSenders', 'mailVolume', 'writeTo'].reduce((l, t) => addWidget(l, t), [])
  check('mail cards arrive at a month, not the longest window', page[0].days, 30)
  check('three mail cards at one window are one read', activityWindows(page, 'mail'), [30])
  check(
    'a card set to a quarter adds a second read, shortest first',
    activityWindows(setDays(page, 'mailVolume:1', 90), 'mail'),
    [30, 90],
  )
  check('a mail card asks nothing of the calendar', activityWindows(page, 'meetings'), [])
  const both = addWidget(addWidget([], 'inTouch'), 'meetingLoad')
  check(
    'the people card reads both, at its own window',
    [activityWindows(both, 'mail'), activityWindows(both, 'meetings')],
    [[30], [30]],
  )
  check(
    '...and a window over entries is not a mail window',
    activityWindows(addWidget([], 'writing'), 'mail'),
    [],
  )
  check(
    'a stored mail card with a window nobody offers falls back to the month',
    parseLayout(JSON.stringify([{ type: 'topSenders', days: 365 }]))[0].days,
    30,
  )
  for (const type of WIDGET_TYPES) {
    const spec = WIDGETS[type]
    if (spec.window !== undefined) {
      ok(`${type} arrives at a window it offers`, spec.windows?.includes(spec.window) === true)
    }
  }
}

// The readings window is the longest any *tracker* card asks for, never
// shorter than the floor a heatmap needs.
{
  const page = setDays(addWidget([], 'trackerChart'), 'trackerChart:1', 14)
  check('a short chart does not shrink the heatmap window', trackerWindow(page, 120), 120)
  check(
    '...and a longer one widens it',
    trackerWindow(setDays(page, 'trackerChart:1', 365), 120),
    365,
  )

  // The regression: "What you have written" is a window over *entries* and
  // arrives set to a year. Counting it here widened the readings query from
  // four months to twelve, drew a heatmap captioned "four months" with a
  // year in it, and computed every streak's hit rate over that year --
  // because somebody added a card about their journal.
  check(
    'a window over something else does not widen the readings query',
    trackerWindow(addWidget(page, 'writing'), 120),
    120,
  )
  ok(
    'every widget with a window is either a tracker widget or is not counted',
    WIDGET_TYPES.filter((t) => WIDGETS[t].windows).some(
      (t) => !WIDGETS[t].needs.includes('trackerDays'),
    ),
    // If this ever stops being true the check above is testing nothing, and
    // whoever removed the last non-tracker window should delete it.
  )
}

// ── reading a stored page back ─────────────────────────────────────────

check('nothing stored means nothing to restore', parseLayout(null), null)
check('a string that is not JSON is not a page', parseLayout('{oh no'), null)
check('JSON that is not a list is not a page', parseLayout('{"a":1}'), null)
check('an empty list means nobody has arranged one', parseLayout('[]'), null)

{
  // A page arranged while areas were still roles keeps its two balance
  // cards, under their new names and with their own ids and sizes.
  const stored = JSON.stringify([
    { id: 'roleBalance-1', type: 'roleBalance', size: 'medium', subject: null, days: null },
    { id: 'roleShare-1', type: 'roleShare', size: 'large', subject: null, days: null },
  ])
  check(
    'a card from before areas were renamed is kept under its new name',
    parseLayout(stored).map((w) => [w.id, w.type, w.size]),
    [
      ['roleBalance-1', 'areaBalance', 'medium'],
      ['roleShare-1', 'areaShare', 'large'],
    ],
  )
}

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

// ── "Plan for tomorrow" ordering ─────────────────────────────────────
//
// The widget draws tomorrow's proposals in the order the day runs; a task
// due sometime with no due *time* has to sort after every timed one, not
// before it the way an empty string would under plain comparison.

{
  const items = [
    { id: 'no-time', at: null },
    { id: 'late', at: '17:00:00' },
    { id: 'early', at: '09:00:00' },
    { id: 'also-no-time', at: null },
  ]
  check(
    'timed rows sort by clock, untimed rows sort last and keep their order',
    byTimeOrLast(items, (i) => i.at).map((i) => i.id),
    ['early', 'late', 'no-time', 'also-no-time'],
  )
  check(
    'an empty list is an empty list',
    byTimeOrLast([], () => null),
    [],
  )
}

// ── The Weather widget ───────────────────────────────────────────────

{
  check('clear sky draws the sun', weatherIcon('clear sky'), 'sun')
  check('mainly clear draws the sun too', weatherIcon('mainly clear'), 'sun')
  check('overcast is just a cloud', weatherIcon('overcast'), 'cloud')
  check('partly cloudy is a cloud, not a sun-and-cloud', weatherIcon('partly cloudy'), 'cloud')
  check('fog is its own shape', weatherIcon('fog'), 'fog')
  check('drizzle rains', weatherIcon('light drizzle'), 'rain')
  check('every flavour of rain', weatherIcon('heavy rain showers'), 'rain')
  check('freezing rain still rains', weatherIcon('freezing rain'), 'rain')
  check('snow snows', weatherIcon('heavy snow showers'), 'snow')
  check('a thunderstorm storms', weatherIcon('thunderstorm with hail'), 'storm')
  check(
    'an unrecognised reading falls back to a cloud',
    weatherIcon('unknown (WMO code 4)'),
    'cloud',
  )
  check('matching is not case-sensitive', weatherIcon('CLEAR SKY'), 'sun')
}

{
  check('US good is the first tier', aqiTier('us', 'Good'), 1)
  check('US moderate is the second', aqiTier('us', 'Moderate'), 2)
  check(
    'US unhealthy for sensitive groups is the third',
    aqiTier('us', 'Unhealthy for sensitive groups'),
    3,
  )
  check('US hazardous is the worst, sixth tier', aqiTier('us', 'Hazardous'), 6)
  check('European good is also the first tier', aqiTier('european', 'Good'), 1)
  check(
    'the same word is a different tier on each scale',
    [aqiTier('us', 'Moderate'), aqiTier('european', 'Moderate')],
    [2, 3],
  )
  check(
    'European extremely poor is the worst, sixth tier',
    aqiTier('european', 'Extremely poor'),
    6,
  )
  check('matching is not case-sensitive', aqiTier('us', 'good'), 1)
  check('a null category has no tier', aqiTier('us', null), null)
  check('a word neither scale uses has no tier', aqiTier('us', 'Fair'), null)
}

// A card added to the catalogue after a page was arranged arrives on it once,
// at the top -- and one removed afterwards stays removed.
{
  const arranged = [widget('dueToday'), widget('onNow')]
  const first = introduce(arranged, [])
  check('new cards arrive at the top of an older page, in the order listed', ids(first.widgets), [
    ...INTRODUCED.map((t) => `${t}:1`),
    'dueToday:1',
    'onNow:1',
  ])
  const removed = first.widgets.filter((w) => w.type !== 'whatsGoingOn')
  const again = introduce(removed, first.seen)
  check(
    'and does not come back once removed',
    again.widgets.some((w) => w.type === 'whatsGoingOn'),
    false,
  )
  ok('a page that has nothing new is left as it was', again.widgets === removed)

  // The owner's case: a page arranged after "What's going on" arrived, and
  // before the home cards did. They land as one block, in the order they
  // were written down -- not stacked in reverse, which is what prepending
  // each in turn did.
  const older = [widget('whatsGoingOn'), widget('areaBalance')]
  check(
    'the home cards arrive in the order they read',
    introduce(older, ['whatsGoingOn']).widgets.map((w) => w.type),
    [
      'karma',
      'todayEvents',
      'todayTasks',
      'importantMail',
      'mailFollowUps',
      'snoozedMail',
      'readingNow',
      'watchingNow',
      'playingNow',
      'brief',
      'whatsGoingOn',
      'areaBalance',
    ],
  )
  // A page that had already been shown every home card is shown the brief
  // alone, on top -- the one card it has not seen.
  const homed = [widget('karma'), widget('todayEvents')]
  const seenHome = INTRODUCED.filter((t) => t !== 'brief')
  check(
    'the brief arrives alone on top of a page that has seen the rest',
    ids(introduce(homed, seenHome).widgets),
    ['brief:1', 'karma:1', 'todayEvents:1'],
  )
  check(
    '...and a page that already has one is left alone',
    introduce([widget('brief'), ...homed], seenHome).widgets.filter((w) => w.type === 'brief')
      .length,
    1,
  )
  check(
    'a card the page already has is not added twice',
    introduce([widget('todayTasks')], ['whatsGoingOn']).widgets.filter(
      (w) => w.type === 'todayTasks',
    ).length,
    1,
  )

  // A page nobody arranged is the default as it stands now, and is shown as
  // such -- not topped with every card the default chose to leave out.
  const fresh = introduce(defaultLayout(), [], false)
  check('a page nobody arranged is left as the default', ids(fresh.widgets), ids(defaultLayout()))
  check('...and everything counts as seen', fresh.seen, INTRODUCED)
  check(
    'every introduced card is a real one',
    INTRODUCED.filter((t) => !WIDGET_TYPES.includes(t)),
    [],
  )
}

// ── The home cards ───────────────────────────────────────────────────

{
  const today = '2026-11-16'
  const tasks = [
    { id: 'later', dueDate: today, dueTime: '17:00:00' },
    { id: 'untimed', dueDate: today, dueTime: null },
    { id: 'tomorrow', dueDate: '2026-11-17', dueTime: null },
    { id: 'early', dueDate: today, dueTime: '09:00:00' },
    { id: 'slipped', dueDate: '2026-11-10', dueTime: null },
    { id: 'undated', dueDate: null },
    { id: 'yesterday', dueDate: '2026-11-15', dueTime: '08:00:00' },
  ]
  const rows = tasksForToday(tasks, today)
  check(
    "today's tasks: what slipped first, oldest first, then today by the clock",
    rows.map((r) => r.task.id),
    ['slipped', 'yesterday', 'early', 'later', 'untimed'],
  )
  check(
    '...and each says whether it is overdue',
    rows.map((r) => r.overdue),
    [true, true, false, false, false],
  )
  check('nothing due is nothing to show', tasksForToday([], today), [])
}

{
  const t = (id, lastDate, unreadCount, followUp = null) => ({
    id,
    lastDate,
    unreadCount,
    followUp,
  })
  const priority = [
    t('read-and-done', '2026-11-16T09:00:00Z', 0),
    t('unread', '2026-11-15T09:00:00Z', 2),
    t('owed', '2026-11-16T08:00:00Z', 0, { kind: 'reply' }),
  ]
  const important = [
    t('unread', '2026-11-15T09:00:00Z', 2),
    t('chasing', '2026-11-16T10:00:00Z', 0, { kind: 'nudge' }),
    t('newest', '2026-11-16T07:00:00-05:00', 1),
  ]
  check(
    'important mail: unread or owed a reply, each once, newest first',
    importantThreads([priority, important]).map((x) => x.id),
    ['newest', 'owed', 'unread'],
  )
}

{
  const rows = [
    { id: 'month', snoozedUntil: '2026-12-16T08:00:00Z' },
    { id: 'never', snoozedUntil: null },
    { id: 'afternoon', snoozedUntil: '2026-11-16T14:00:00Z' },
    { id: 'tomorrow', snoozedUntil: '2026-11-17T08:00:00Z' },
  ]
  check(
    'snoozed mail: soonest back first, and one with no time last',
    soonestBack(rows).map((r) => r.id),
    ['afternoon', 'tomorrow', 'month', 'never'],
  )
}

{
  const events = [
    { id: 'lunch', allDay: false, start: '2026-11-16T12:00:00Z', end: '2026-11-16T13:00:00Z' },
    { id: 'holiday', allDay: true, start: '2026-11-16T00:00:00Z', end: '2026-11-17T00:00:00Z' },
    // 11:00Z, written two hours east: compared as an instant it sorts before
    // lunch, where comparing the strings ("13:00" against "12:00") would not.
    {
      id: 'standup',
      allDay: false,
      start: '2026-11-16T13:00:00+02:00',
      end: '2026-11-16T13:15:00+02:00',
    },
  ]
  check(
    "today's events: all-day first, then by the instant each starts",
    dayAgenda(events).map((e) => e.id),
    ['holiday', 'standup', 'lunch'],
  )
  const noon = Date.parse('2026-11-16T12:30:00Z')
  check(
    'an event is over, on, or still to come -- and an all-day one is never "on"',
    events.map((e) => eventPhase(e, noon)),
    ['now', 'later', 'past'],
  )
  check(
    'an event that has just ended is over',
    eventPhase(events[0], Date.parse('2026-11-16T13:00:00Z')),
    'past',
  )
}

{
  check(
    'the shelf cards are about books, TV and games, by slug',
    ['readingNow', 'watchingNow', 'playingNow'].map(nowShelf),
    ['book', 'series', 'game'],
  )
  check('any other card is about no shelf', nowShelf('shelves'), null)
  check(
    'every shelf card is a real widget',
    Object.keys(NOW_SHELVES).filter((t) => !WIDGET_TYPES.includes(t)),
    [],
  )
}

await close()
finish('dashboard')
