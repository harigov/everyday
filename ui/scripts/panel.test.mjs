// `panel/cards.ts`: the decisions the panel page makes about the cards the shell
// sends it -- which are drawn, which of their lines, what a press switches off
// and for how long, how the level meter moves, and what height the shell is
// told. The drawing itself (`PanelStack.svelte`, `PanelCard.svelte`) is looked
// at in a browser, at /panel.html in a development build, where sample cards
// stand in for the shell.

import { load, makeCheck } from './harness.mjs'

const { check, ok, finish } = makeCheck()

const { module: m, close } = await load('/src/panel/cards.ts')

const card = (id, over = {}) => ({
  id,
  owner: 'meeting',
  tone: 'neutral',
  kicker: 'Meeting',
  title: id,
  meter: false,
  busy: false,
  ...over,
})
const action = (id, label = id, over = {}) => ({ id, label, primary: false, ...over })

// ── which cards are drawn ────────────────────────────────────────────────

const five = ['a', 'b', 'c', 'd', 'e'].map((id) => card(id))
check(
  'three of five are drawn',
  m.visible(five).shown.map((c) => c.id),
  ['a', 'b', 'c'],
)
check('and two wait', m.visible(five).more, 2)
check('three of three: none wait', m.visible(five.slice(0, 3)).more, 0)
check('one card', m.visible(five.slice(0, 1)).shown.length, 1)
check('no cards', m.visible([]), { shown: [], more: 0 })
check('the limit is the shell’s', m.MAX_SHOWN, 3)
check('a different limit', m.visible(five, 2).more, 3)
check('the quiet line', m.moreLabel(2), '+2 more')

// ── which lines ──────────────────────────────────────────────────────────

const said = (text, over = {}) => ({ text, pending: false, ...over })
const fiveLines = ['one', 'two', 'three', 'four', 'five'].map((t) => said(t))
check(
  'the last three lines',
  m.lastLines(fiveLines).map((l) => l.text),
  ['three', 'four', 'five'],
)
check('fewer than three: all of them', m.lastLines(fiveLines.slice(0, 2)).length, 2)
check('no lines', m.lastLines(undefined), [])
check('an empty line that is over is not drawn', m.lastLines([said('hello'), said('  ')]).length, 1)
check(
  'an empty line still coming is: its caret says the card is waiting',
  m.lastLines([said('', { pending: true })]).length,
  1,
)
check(
  'blank lines do not use up the three',
  m.lastLines([said('a'), said('b'), said(''), said('c'), said('d')]).map((l) => l.text),
  ['b', 'c', 'd'],
)

// ── the count ────────────────────────────────────────────────────────────

check('seconds', m.splitCount('12s'), { value: '12', unit: 's' })
check('a clock', m.splitCount('1:05'), { value: '1:05', unit: '' })
check('minutes', m.splitCount('5 min'), { value: '5', unit: 'min' })
check('words are left whole', m.splitCount('Now'), { value: 'Now', unit: '' })

// ── lists, and what is folded ────────────────────────────────────────────

const list = (heading, n, folded = false) => ({
  heading,
  folded,
  items: Array.from({ length: n }, (_, i) => ({ text: `${heading} ${i}` })),
})
const brief = [list('Agenda', 2), list('With', 0), list('Context', 3, true)]
const headings = (open) => m.drawnSections(brief, open).map((s) => s.heading)
check('closed: folded lists and empty ones are not drawn', headings(false), ['Agenda'])
check('open: the folded ones too', headings(true), ['Agenda', 'Context'])
check('no lists', m.drawnSections(undefined, true), [])
ok('a folded list earns a More', m.hasFolded(brief))
ok('an empty folded list does not', !m.hasFolded([list('Context', 0, true)]))
ok('nothing folded, no More', !m.hasFolded([list('Agenda', 2)]))

// ── how tall the lists may be ────────────────────────────────────────────

check('one card on a tall screen: the most', m.listsBudget(1080, 1), 360)
check('three cards on a tall screen share it', m.listsBudget(1080, 3), 197)
check('a short screen still scrolls', m.listsBudget(720, 3), 96)
check('no cards counts as one', m.listsBudget(1080, 0), 360)
check('a screen that will not say', m.listsBudget(0, 2), 360)
check('a screen that says nonsense', m.listsBudget(Number.NaN, 2), 360)
ok('more cards, less each', m.listsBudget(900, 3) < m.listsBudget(900, 2))

// ── tone ─────────────────────────────────────────────────────────────────

for (const tone of ['listening', 'working', 'success', 'attention']) {
  check(`${tone} is the accent`, m.toneColour(tone), 'var(--accent)')
}
check('trouble is the danger colour', m.toneColour('trouble'), 'var(--danger)')
check('neutral is quiet', m.toneColour('neutral'), 'var(--fg-muted)')
const marks = ['neutral', 'listening', 'success', 'trouble'].map((tone) => m.toneMark(tone))
check('marks', marks, ['none', 'dot', 'check', 'alert'])

// ── a press switches the card's buttons off ──────────────────────────────

const two = (over = {}) => card('talk', { actions: [action('allow'), action('deny')], ...over })
const pressed = (at) => ({ signature: m.signature(two()), at })

check('before the press, on', m.isLocked(undefined, two(), 0), false)
check('right after, off', m.isLocked(pressed(1000), two(), 1000), true)
check('just short of four seconds, off', m.isLocked(pressed(1000), two(), 4999), true)
check('at four seconds, on again', m.isLocked(pressed(1000), two(), 5000), false)
check('four seconds is the rule', m.LOCK_MS, 4000)
check(
  'a card that ticks over with the same buttons stays off',
  m.isLocked(pressed(1000), two({ count: '11s', body: 'later' }), 2000),
  true,
)
check(
  'new buttons: on at once',
  m.isLocked(pressed(1000), two({ actions: [action('end')] }), 2000),
  false,
)
check(
  'a button renamed is a different button',
  m.isLocked(pressed(1000), two({ actions: [action('allow', 'Yes'), action('deny')] }), 2000),
  false,
)
check('a card with no buttons has a signature', typeof m.signature(card('x')), 'string')

// What is kept when the shell speaks again.
const stack = [two(), card('meeting', { actions: [action('take')] })]
const kept = { talk: pressed(1000), meeting: { signature: m.signature(stack[1]), at: 1000 } }
ok('nothing ended: the same object', m.settlePresses(kept, stack, 2000) === kept)
check('a press runs out', Object.keys(m.settlePresses(kept, stack, 5000)), [])
check(
  'a card that changed its buttons forgets its press',
  Object.keys(m.settlePresses(kept, [two({ actions: [action('end')] }), stack[1]], 2000)),
  ['meeting'],
)
const gone = m.settlePresses(kept, [stack[1]], 2000)
check('a card that went forgets its press', Object.keys(gone), ['meeting'])

const soonest = m.nextExpiry({ a: { at: 3000 }, b: { at: 1000 } }, { c: { at: 2000 } })
check('the soonest of the presses runs out first', soonest, 5000)
check('nothing waiting', m.nextExpiry({}, {}), null)

// ── a box is ticked at once, and is off until the shell has answered ─────

const box = { action: 'done:t1', done: false }
check('untouched', m.checkView(box, undefined, 0), { done: false, locked: false })
const ticked = m.checkView(box, { done: false, at: 1000 }, 1500)
check('ticked: shown ticked, and off', ticked, { done: true, locked: true })
const answered = m.checkView({ ...box, done: true }, { done: false, at: 1000 }, 1500)
check('the shell answered: it is what the shell says, and on', answered, {
  done: true,
  locked: false,
})
const silent = m.checkView(box, { done: false, at: 1000 }, 5000)
check('the shell did nothing: back after four seconds', silent, { done: false, locked: false })

const withBox = card('meeting', {
  sections: [{ heading: 'To raise', folded: false, items: [{ text: 'Josh', check: box }] }],
})
const key = m.checkKey('meeting', 'done:t1')
const boxes = { [key]: { done: false, at: 1000 } }
ok('a box press in force is kept as it is', m.settleChecks(boxes, [withBox], 2000) === boxes)
check('and goes when it runs out', Object.keys(m.settleChecks(boxes, [withBox], 5000)), [])
const answeredBox = card('meeting', {
  sections: [
    {
      heading: 'To raise',
      folded: false,
      items: [{ text: 'Josh', check: { ...box, done: true } }],
    },
  ],
})
const afterAnswer = m.settleChecks(boxes, [answeredBox], 2000)
check('and goes when the shell has answered', Object.keys(afterAnswer), [])
check('and goes with its card', Object.keys(m.settleChecks(boxes, [], 2000)), [])
ok('boxes of different cards have different keys', m.checkKey('a', 'x') !== m.checkKey('b', 'x'))

// ── copying ──────────────────────────────────────────────────────────────

let copied = null
check('it copied', await m.tryCopy('hello', { writeText: async (t) => void (copied = t) }), true)
check('the text went to the clipboard', copied, 'hello')
check(
  'the clipboard refused',
  await m.tryCopy('hello', {
    writeText: async () => {
      throw new Error('denied')
    },
  }),
  false,
)
check('no clipboard here', await m.tryCopy('hello', undefined), false)
ok('a button with text to copy', m.copies(action('copy', 'Copy', { copy: 'text' })))
ok('an empty text is still text to copy', m.copies(action('copy', 'Copy', { copy: '' })))
ok('a button without', !m.copies(action('end')))

// ── what is kept by card ─────────────────────────────────────────────────

const byCard = { a: true, b: false }
ok('every card still up: the same object', m.keepCards(byCard, [card('a'), card('b')]) === byCard)
check('a card that went takes its entry', m.keepCards(byCard, [card('b')]), { b: false })
check('all gone', m.keepCards(byCard, []), {})
check('without', m.without(byCard, 'a'), { b: false })
check('without leaves the original', byCard, { a: true, b: false })

// ── the level meter ──────────────────────────────────────────────────────

let history = m.silence()
check('nine bars', history.length, m.BARS)
ok('all quiet', m.isQuiet(history))
history = m.pushLevel(history, 1)
check('one in, one out', history.length, m.BARS)
ok('rises quickly', history[m.BARS - 1] > 0.5)
const peak = history[m.BARS - 1]
history = m.pushLevel(history, 0)
ok('falls slowly', history[m.BARS - 1] < peak && history[m.BARS - 1] > 0.1)
ok('no longer quiet', !m.isQuiet(history))
check('a bad reading is silence', m.pushLevel(m.silence(), Number.NaN).at(-1), 0)
ok('clamped', m.pushLevel(m.silence(), 7).at(-1) <= 1)
ok('a bar is never nothing', m.barScale(0) > 0)
ok('speech lifts it', m.barScale(0) < m.barScale(0.1))
ok('full is full', m.barScale(1) > 0.999)
ok('and no more', m.barScale(9) <= 1)

// ── telling the shell how tall the cards are ─────────────────────────────

check('rounded up', m.roundHeight(100.2), 101)
check('whole is whole', m.roundHeight(100), 100)
check('layout noise is not a pixel', m.roundHeight(99.99999), 100)
check('and not the other way either', m.roundHeight(100.0000001), 100)
check('nothing', m.roundHeight(0), 0)
check('nonsense is nothing', [m.roundHeight(-4), m.roundHeight(Number.NaN)], [0, 0])

let told = m.reportHeight(null, 140.4)
check('the first height is sent, rounded up', told, { last: 141, send: 141 })
told = m.reportHeight(told.last, 141)
check('the same height again is not', told, { last: 141, send: null })
told = m.reportHeight(told.last, 140.6)
check('nor one that rounds to it', told, { last: 141, send: null })
told = m.reportHeight(told.last, 230)
check('a taller stack is', told, { last: 230, send: 230 })
told = m.reportHeight(told.last, 0)
check('no cards: nothing said, and forgotten', told, { last: null, send: null })
told = m.reportHeight(told.last, 230)
check('the next stack is sent even if it is as tall as the last', told, { last: 230, send: 230 })

// The primary button is drawn rightmost, whatever order the card lists them in.
const take = { id: 'take', label: 'Take notes', primary: true }
const notNow = { id: 'notNow', label: 'Not now', primary: false }
check(
  'the primary button is drawn last',
  m.drawnOrder([take, notNow]).map((a) => a.id),
  ['notNow', 'take'],
)
check('no buttons, none drawn', m.drawnOrder(undefined), [])

await close()
finish('panel')
