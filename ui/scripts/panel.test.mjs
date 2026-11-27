// `panel/cards.ts`: the decisions the panel page makes about the cards the shell
// sends it -- which are drawn and which of their blocks, where each block goes,
// what a press switches off and for how long, how a box or an action item shows
// a press before the shell has answered, what a box to type in sends and when
// it puts the words back, which card is held, how the level meter moves, how a
// clock and an expiry are drawn, whose face is which colour, which theme is
// drawn, and what height the shell is told. The drawing itself
// (`PanelStack.svelte`, `PanelCard.svelte`, `blocks/`) is looked at in a
// browser, at /panel.html in a development build, where sample cards stand in
// for the shell.

import { load, makeCheck } from './harness.mjs'

const { check, ok, finish } = makeCheck()

const { module: m, close } = await load('/src/panel/cards.ts')

const card = (id, over = {}) => ({
  id,
  owner: 'meeting',
  kind: 'meeting',
  tone: 'neutral',
  kicker: 'Meeting',
  title: id,
  meter: false,
  busy: false,
  ...over,
})
const action = (id, label = id, over = {}) => ({
  id,
  label,
  primary: false,
  danger: false,
  ...over,
})
const person = (name, over = {}) => ({ name, organizer: false, you: false, ...over })

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

// ── the count, and words said twice ──────────────────────────────────────

check('seconds', m.splitCount('12s'), { value: '12', unit: 's' })
check('a clock', m.splitCount('1:05'), { value: '1:05', unit: '' })
check('minutes', m.splitCount('5 min'), { value: '5', unit: 'min' })
check('words are left whole', m.splitCount('Now'), { value: 'Now', unit: '' })

ok('the same title is a repeat', m.repeats('Product sync', 'Product sync'))
ok('whatever its case and spacing', m.repeats('  product   SYNC ', 'Product sync'))
ok('a different one is not', !m.repeats('Design review', 'Product sync'))
ok('nothing is not a repeat', !m.repeats(undefined, 'Product sync'))
ok('an empty one is not either', !m.repeats('  ', ''))

// ── which turns of a conversation ────────────────────────────────────────

const said = (text, over = {}) => ({ who: 'you', text, pending: false, ...over })
const ten = Array.from({ length: 10 }, (_, i) => said(`t${i}`))
check(
  'the last eight turns',
  m.lastTurns(ten).map((t) => t.text),
  ['t2', 't3', 't4', 't5', 't6', 't7', 't8', 't9'],
)
check('fewer: all of them', m.lastTurns(ten.slice(0, 2)).length, 2)
check('no turns', m.lastTurns(undefined), [])
check('an empty turn that is over is not drawn', m.lastTurns([said('hello'), said('  ')]).length, 1)
check(
  'an empty turn still coming is: its caret or dots say an answer is on its way',
  m.lastTurns([said('', { who: 'assistant', pending: true })]).length,
  1,
)
check(
  'blank turns do not use up the room',
  m.lastTurns([said('a'), said(''), said('b')], 2).map((t) => t.text),
  ['a', 'b'],
)

const talk = [
  said('hi'),
  said('there'),
  said('hello', { who: 'assistant', name: 'Asha' }),
  said('more', { who: 'assistant', name: 'Asha' }),
  said('ok'),
]
check(
  'a speaker’s name heads their run of turns, not every turn',
  talk.map((_, i) => m.showsName(talk, i)),
  [true, false, true, false, true],
)
check('past the end, no name', m.showsName(talk, 9), false)

// ── which blocks are drawn, and where ────────────────────────────────────

const list = (heading, n, folded = false) => ({
  type: 'list',
  heading,
  folded,
  items: Array.from({ length: n }, (_, i) => ({ text: `${heading} ${i}` })),
})
const blocks = [
  list('Agenda', 2),
  list('Empty', 0),
  list('Context', 3, true),
  { type: 'text', heading: 'Summary', text: 'Long story', folded: true },
  { type: 'text', text: '   ', folded: false },
  { type: 'conversation', turns: [] },
  { type: 'quote', text: 'Exactly this' },
]
const types = (open) =>
  m.drawnBlocks(blocks, open).map((b) => (b.type === 'list' ? b.heading : b.type))
check('closed: folded blocks and empty ones are not drawn', types(false), ['Agenda', 'quote'])
check('open: the folded ones too', types(true), ['Agenda', 'Context', 'text', 'quote'])
check('no blocks', m.drawnBlocks(undefined, true), [])
ok('a folded block earns a More', m.hasFolded(blocks))
ok('an empty folded block does not', !m.hasFolded([list('Context', 0, true)]))
ok('nothing folded, no More', !m.hasFolded([list('Agenda', 2)]))
const crowd = ['A', 'B', 'C', 'D', 'E'].map((n) => person(n))
const people = (n) => ({ type: 'people', people: crowd.slice(0, n), folded: false })
ok('more people than a card shows closed earns a More', m.hasFolded([people(5)]))
ok('three people do not', !m.hasFolded([people(3)]))
check(
  'closed, the first three people',
  m.peopleShown(crowd, false).map((p) => p.name),
  ['A', 'B', 'C'],
)
check('open, everyone', m.peopleShown(crowd, true).length, 5)
ok('a folded list of people is folded', m.isFolded({ ...people(2), folded: true }))
ok('an event is never folded', !m.isFolded({ type: 'event', title: 'x', when: 'y' }))
ok('a list with no items is empty', m.isEmpty(list('x', 0)))
ok('choices with none to pick are empty', m.isEmpty({ type: 'choices', choices: [] }))
ok('a clock is never empty', !m.isEmpty({ type: 'clock', sinceMs: 0 }))

const recording = [
  { type: 'activity', mode: 'recording' },
  { type: 'clock', sinceMs: 0 },
  { type: 'input', id: 'note', placeholder: '', submit: 'Add', multiline: false, busy: false },
  list('Your notes', 1),
  { type: 'conversation', turns: [said('q')] },
  { type: 'choices', choices: [action('choice:0')] },
  list('More notes', 1),
]
const split = m.splitBlocks(recording)
check(
  'what is happening now goes at the top',
  split.top.map((k) => k.key),
  ['activity:0', 'clock:0'],
)
check(
  'the rest scrolls, in the order given',
  split.body.map((k) => k.key),
  ['list:0', 'conversation:0', 'list:1'],
)
check(
  'the box and the answers stay over the buttons',
  split.pinned.map((k) => k.key),
  ['input:note', 'choices:0'],
)
check(
  'a block’s key does not change when another kind is added before it',
  m.splitBlocks([{ type: 'activity', mode: 'idle' }, ...recording.slice(3)]).body[0].key,
  'list:0',
)
ok('ending in a conversation keeps to the foot', m.sticksToBottom(split.body.slice(0, 2)))
ok('ending in a list does not', !m.sticksToBottom(split.body))
ok('nothing to scroll does not', !m.sticksToBottom([]))

// ── how tall the blocks may be ───────────────────────────────────────────

check('the window is made no taller than the shell’s most', m.MOST_OF_AREA, 0.8)
check('one card on a tall screen: the most', m.blocksBudget(1080, 1, 150), 440)
check('three cards on a tall screen share most of it', m.blocksBudget(1080, 3, 150), 120)
check('a short screen still scrolls', m.blocksBudget(720, 3, 150), 96)
check('no cards counts as one', m.blocksBudget(1080, 0, 150), 440)
check('a screen that will not say', m.blocksBudget(0, 2, 150), 440)
check('a screen that says nonsense', m.blocksBudget(Number.NaN, 2, 150), 440)
check('nonsense for the rest of the card is nothing', m.blocksBudget(768, 1, Number.NaN), 440)
ok('more cards, less each', m.blocksBudget(900, 3, 150) < m.blocksBudget(900, 2, 150))
ok(
  'a card with a box leaves its blocks less room',
  m.blocksBudget(1080, 2, 250) < m.blocksBudget(1080, 2, 150),
)
// A laptop's 736 pixels of work area: the window is at most 588 of them. A
// card with a reply box, and its blocks as tall as they may be, fit in that
// with the stack's padding -- the buttons are not cut off at the window's foot.
const laptop = m.blocksBudget(736, 1, 246)
ok('one card and its blocks fit the window', 32 + 246 + laptop <= 736 * m.MOST_OF_AREA)
const twoUp = m.blocksBudget(1040, 2, 246)
ok('two of them fit a taller screen’s window', 2 * (246 + twoUp) + 32 + 10 <= 1040 * m.MOST_OF_AREA)

// What a card is without the blocks that scroll.
const bare = m.cardChrome(card('x'))
check('a card with nothing but its head', bare, 26 + 28 + 40)
check('a body under the title', m.cardChrome(card('x', { body: 'Two lines' })), bare + 24)
const replyBox = (over = {}) => ({
  type: 'input',
  id: 'reply',
  placeholder: 'Reply…',
  submit: 'Send',
  multiline: true,
  busy: false,
  ...over,
})
const mailCard = (box) =>
  card('mail:1', {
    blocks: [{ type: 'mail', from: person('Ama'), subject: 'Thursday', snippet: 'Can we…' }, box],
    actions: [action('archive'), action('read'), action('open')],
  })
const yes = [{ label: 'Yes', text: 'Yes, Thursday works.' }]
check(
  'a message with a reply box, its ready-made replies and three buttons',
  m.cardChrome(mailCard(replyBox({ suggestions: yes }))),
  bare + 12 + (12 + 40 + 44) + (12 + 32),
)
ok(
  'ready-made replies take room',
  m.cardChrome(mailCard(replyBox({ suggestions: yes }))) > m.cardChrome(mailCard(replyBox())),
)
const twoModes = [
  { id: 'note', label: 'Note', placeholder: '', submit: 'Add' },
  { id: 'ask', label: 'Ask', placeholder: '', submit: 'Ask' },
]
check(
  'and so do the ways to use a box, and its error',
  m.cardChrome(mailCard(replyBox({ error: 'No network', modes: twoModes }))),
  m.cardChrome(mailCard(replyBox())) + 36 + 26,
)
check(
  'a recording: what is happening now, the box, an answer to pick and the button',
  m.cardChrome(card('recording:1', { blocks: recording, actions: [action('stop')] })),
  bare + (12 + 36) + 12 + (12 + 40) + (12 + 36) + (12 + 32),
)
const three = ['a', 'b', 'c'].map((id) => action(id))
check(
  'answers to pick, two to a row, under their question',
  m.cardChrome(card('ask:1', { blocks: [{ type: 'choices', prompt: 'Which?', choices: three }] })),
  bare + 12 + 26 + 2 * 36,
)
check(
  'the tool at work under the activity takes a line more',
  m.cardChrome(
    card('talk', {
      blocks: [{ type: 'activity', mode: 'working', tool: { name: 't', label: 'Searching' } }],
    }),
  ),
  bare + 12 + 56,
)
check(
  'something folded away earns a foot, with no buttons -- and counts as opened',
  m.cardChrome(card('x', { blocks: [list('Context', 2, true)] })),
  bare + 12 + (12 + 32),
)

// ── how a card looks ─────────────────────────────────────────────────────

check(
  'each kind is its app',
  ['meeting', 'invite', 'mail', 'notes', 'assistant', 'voiceTyping', 'notice'].map((k) => {
    const look = m.kindLook(k)
    return `${look.symbol} ${look.colour}`
  }),
  [
    'calendar var(--app-calendar)',
    'calendar var(--app-calendar)',
    'mail var(--app-mail)',
    'note var(--app-notes)',
    'wing var(--app-assistant)',
    'mic var(--app-assistant)',
    'compass var(--app-overview)',
  ],
)
check('trouble is the danger colour', m.toneColour('trouble'), 'var(--danger)')
check('success is the success colour', m.toneColour('success'), 'var(--success)')
check('attention is the accent', m.toneColour('attention'), 'var(--accent)')
check('neutral is quiet', m.toneColour('neutral'), 'var(--fg-muted)')
for (const tone of ['listening', 'working']) {
  check(`${tone} is the card’s own app colour`, m.toneColour(tone), 'var(--kind)')
}
const marks = ['neutral', 'listening', 'success', 'trouble'].map((tone) => m.toneMark(tone))
check('marks', marks, ['none', 'dot', 'check', 'alert'])

const icons = ['reply', 'archive', 'read', 'open', 'x', 'check', 'micOff', 'send', 'later', 'join']
check(
  'button marks',
  icons.map((icon) => m.actionGlyph(icon)),
  ['reply', 'archive', 'eye', 'open', 'close', 'tick', 'mic-off', 'send', 'clock', 'join'],
)
check('a mark the page does not know draws none', m.actionGlyph('rocket'), null)
check('no mark, none', m.actionGlyph(undefined), null)
check('not an inherited name either', m.actionGlyph('toString'), null)

check('a mail tool is mail’s', m.toolLook('mail'), {
  symbol: 'mail',
  colour: 'var(--app-mail)',
})
check('tasks are the tasks app’s', m.toolLook('tasks').colour, 'var(--app-todo)')
check('a tool with no mark of its own is the assistant’s', m.toolLook('generic').symbol, 'sparkle')

check('listening moves like a voice', m.modeLook('listening').motion, 'bars')
check('thinking waits in dots', m.modeLook('thinking').motion, 'dots')
check('speaking is a wave', m.modeLook('speaking').motion, 'wave')
check('recording is the red dot', m.modeLook('recording'), {
  word: 'Recording',
  motion: 'rec',
  glyph: null,
  colour: 'var(--danger)',
})
check('waiting for you pulses in the accent', m.modeLook('waiting').colour, 'var(--accent)')
check('muted is still, with its mark', m.modeLook('muted').glyph, 'mic-off')
check('done is ticked in success', m.modeLook('done').colour, 'var(--success)')
check('a failure says so', m.modeLook('failed').word, 'Didn’t work')

// ── people ───────────────────────────────────────────────────────────────

check('a face is two initials', m.face({ name: 'Josh Miller' }).initials, 'JM')
check(
  'from the address without a name',
  m.face({ name: '', email: 'ama.owusu@x.io' }).initials,
  'AO',
)
ok('a face is one of the apps’ colours', m.FACE_HUES.includes(m.faceHue('josh@x.io')))
check('the same address, the same colour', m.faceHue('Josh@X.io '), m.faceHue('josh@x.io'))
check(
  'the address decides, not the name',
  m.face({ name: 'J', email: 'josh@x.io' }).hue,
  m.face({ name: 'Josh Miller', email: 'josh@x.io' }).hue,
)
ok(
  'no literal colours: tokens only',
  m.FACE_HUES.every((h) => h.startsWith('var(--app-')),
)
ok('health’s rose is not a face', !m.FACE_HUES.includes('var(--app-health)'))

const guests = [
  person('Ama', { status: 'accepted' }),
  person('Josh', { status: 'accepted', organizer: true }),
  person('Me', { status: 'needsAction', you: true }),
  person('Lena', { status: 'declined' }),
  person('Priya', { status: 'tentative' }),
  person('Kofi', { status: 'needsAction' }),
  person('Ines'),
  person('Tom', { status: 'accepted' }),
]
check(
  'the organiser’s face first',
  m.faces(guests, 3).shown.map((p) => p.name),
  ['Josh', 'Ama', 'Me'],
)
check('and how many more', m.faces(guests, 3).more, 5)
check('six faces by default', m.faces(guests).shown.length, 6)
check(
  'the answers in a line, leaving out the person’s own',
  m.rsvpSummary(guests),
  '3 going · 1 maybe · 1 not going · 1 no answer',
)
check('nobody has answered anything', m.rsvpSummary([person('A'), person('B')]), null)
check(
  'the marks',
  ['accepted', 'declined', 'tentative', 'needsAction', undefined].map((s) => m.rsvpLook(s)?.mark),
  ['yes', 'no', 'maybe', 'waiting', undefined],
)
check('your own answer, said to you', m.yourAnswer('accepted').words, 'You’re going')
check('not yet', m.yourAnswer('needsAction'), { mark: 'waiting', words: 'You haven’t answered' })
check('one attachment', m.plural(1, 'attachment'), '1 attachment')
check('three', m.plural(3, 'attachment'), '3 attachments')

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
const asking = (choices) => two({ blocks: [{ type: 'choices', choices }] })
const choicePress = { signature: m.signature(asking([action('choice:0')])), at: 1000 }
check(
  'answers to pick are part of what a card offers',
  m.isLocked(choicePress, asking([action('choice:0')]), 2000),
  true,
)
check(
  'new answers: on at once',
  m.isLocked(choicePress, asking([action('choice:0'), action('choice:1')]), 2000),
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

// The primary button is drawn rightmost, whatever order the card lists them in.
const take = action('take', 'Take notes', { primary: true })
const notNow = action('notNow', 'Not now')
check(
  'the primary button is drawn last',
  m.drawnOrder([take, notNow]).map((a) => a.id),
  ['notNow', 'take'],
)
check('no buttons, none drawn', m.drawnOrder(undefined), [])

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

const withBox = (done = false) =>
  card('meeting', {
    blocks: [
      {
        type: 'list',
        heading: 'To raise',
        folded: false,
        items: [{ text: 'Josh', check: { ...box, done } }],
      },
      { type: 'task', title: 'Send it', done, check: 'done:task' },
      { type: 'task', title: 'No box', done: false },
    ],
  })
check(
  'a card’s boxes: in its lists, and on its tasks that have one',
  m.checksOf(withBox()).map((c) => c.action),
  ['done:t1', 'done:task'],
)
const key = m.checkKey('meeting', 'done:t1')
const taskKey = m.checkKey('meeting', 'done:task')
const boxes = { [key]: { done: false, at: 1000 }, [taskKey]: { done: false, at: 1000 } }
ok('box presses in force are kept as they are', m.settleChecks(boxes, [withBox()], 2000) === boxes)
check('and go when they run out', Object.keys(m.settleChecks(boxes, [withBox()], 5000)), [])
check(
  'and go when the shell has answered',
  Object.keys(m.settleChecks(boxes, [withBox(true)], 2000)),
  [],
)
check('and go with their card', Object.keys(m.settleChecks(boxes, [], 2000)), [])
ok('boxes of different cards have different keys', m.checkKey('a', 'x') !== m.checkKey('b', 'x'))

// ── an action item is decided at once, and waits for the shell ───────────

const item = (state = 'pending') => ({ id: 'p1', title: 'Book the venue', state })
const decision = { from: 'pending', to: 'accepted', at: 1000 }
check('undecided', m.proposalView(item(), undefined, 0), { state: 'pending', locked: false })
check('pressed: shown decided, and off', m.proposalView(item(), decision, 1500), {
  state: 'accepted',
  locked: true,
})
check('the shell answered: as it says', m.proposalView(item('accepted'), decision, 1500), {
  state: 'accepted',
  locked: false,
})
check('the shell did nothing: back after four seconds', m.proposalView(item(), decision, 5000), {
  state: 'pending',
  locked: false,
})
check('what accepting sends', m.decisionAction('p1', 'accepted'), 'accept:p1')
check('what rejecting sends', m.decisionAction('p1', 'rejected'), 'reject:p1')
const notes = (state) => card('notes:1', { blocks: [{ type: 'proposals', items: [item(state)] }] })
const decisions = { [m.decisionKey('notes:1', 'p1')]: decision }
ok(
  'a decision in force is kept',
  m.settleDecisions(decisions, [notes('pending')], 2000) === decisions,
)
check('and goes once answered', m.settleDecisions(decisions, [notes('accepted')], 2000), {})
check('and goes with its card', m.settleDecisions(decisions, [], 2000), {})
ok('a decision is not a box', m.decisionKey('a', 'x') !== m.checkKey('a', 'x'))

check('no heading: how many', m.proposalsTitle(undefined, 3), '3 action items')
check('one', m.proposalsTitle(undefined, 1), '1 action item')
check('the shell’s heading wins', m.proposalsTitle('From the standup', 3), 'From the standup')
check('nothing decided yet', m.proposalsTally(['pending', 'pending']), null)
check('some', m.proposalsTally(['accepted', 'pending', 'rejected']), '2 of 3 decided')
check('all', m.proposalsTally(['accepted', 'rejected']), 'All decided')

// ── a box to type in ─────────────────────────────────────────────────────

const input = (over = {}) => ({
  id: 'reply',
  placeholder: 'Reply…',
  submit: 'Send',
  multiline: true,
  busy: false,
  ...over,
})
check('no modes: the box sends its own id', m.inputMode(input(), undefined), {
  action: 'reply',
  placeholder: 'Reply…',
  submit: 'Send',
  mode: null,
})
const twoWays = input({
  id: 'note',
  modes: [
    { id: 'note', label: 'Note', placeholder: 'Add a note', submit: 'Add' },
    { id: 'ask', label: 'Ask', placeholder: 'Ask about it', submit: 'Ask' },
  ],
})
check('before a mode is picked, the first', m.inputMode(twoWays, undefined).action, 'note')
check('the picked one', m.inputMode(twoWays, 'ask'), {
  action: 'ask',
  placeholder: 'Ask about it',
  submit: 'Ask',
  mode: 'ask',
})
check('one that is gone: the first again', m.inputMode(twoWays, 'old').mode, 'note')

const key_ = (k, shift = false, composing = false) => ({
  key: k,
  shiftKey: shift,
  isComposing: composing,
})
check('Enter sends', m.keyAction(key_('Enter'), true), 'send')
check('Shift+Enter is a new line', m.keyAction(key_('Enter', true), true), 'newline')
check('in a one-line box it sends', m.keyAction(key_('Enter', true), false), 'send')
check('Escape gives the keyboard back', m.keyAction(key_('Escape'), true), 'blur')
check('composing a character: nothing', m.keyAction(key_('Enter', false, true), true), null)
check('any other key: nothing', m.keyAction(key_('a'), true), null)
check(
  'a one-line box has no line breaks',
  m.cleanText('one\n two\r\nthree', false),
  'one two three',
)
check('a box of several keeps them', m.cleanText('one\ntwo', true), 'one\ntwo')

const sent = { text: 'Sounds good', at: 1000, seen: false }
check('no word yet: kept', m.sentAfter(sent, input(), 2000), { sent, restore: null })
check('busy: the shell has it', m.sentAfter(sent, input({ busy: true }), 2000), {
  sent: { text: 'Sounds good', at: 1000, seen: true },
  restore: null,
})
check('an error: the words go back', m.sentAfter(sent, input({ error: 'No network' }), 2000), {
  sent: null,
  restore: 'Sounds good',
})
const old = { ...sent, error: 'No network' }
check(
  'the error from before is not this one’s',
  m.sentAfter(old, input({ error: 'No network' }), 2000),
  { sent: old, restore: null },
)
const busyThen = m.sentAfter(old, input({ busy: true }), 2000).sent
check(
  'but once busy, the same error again is',
  m.sentAfter(busyThen, input({ error: 'No network' }), 3000).restore,
  'Sounds good',
)
check('busy, then not, with no error: it went', m.sentAfter(busyThen, input(), 3000), {
  sent: null,
  restore: null,
})
check('no word for four seconds: taken as gone', m.sentAfter(sent, input(), 5000).sent, null)
check('the box went: nothing to put back', m.sentAfter(sent, undefined, 2000), {
  sent: null,
  restore: null,
})
ok('sending is off while the shell is busy', m.inputLocked(undefined, input({ busy: true }), 0))
ok('and just after a send', m.inputLocked(sent, input(), 1500))
ok('but not once the shell has it and is done', !m.inputLocked(busyThen, input(), 1500))
ok('nor after four seconds', !m.inputLocked(sent, input(), 5000))
check(
  'the sends still keeping a box off',
  Object.keys(m.waitingSends({ a: sent, b: busyThen, c: { ...sent, at: -5000 } }, 2000)),
  ['a'],
)
// A note added to a recording is taken at once, and the box never goes busy.
const taken = { ...sent, acked: true }
ok('taken by the shell: the box takes the next at once', !m.inputLocked(taken, input(), 1500))
ok('but not while the shell says it is busy', m.inputLocked(taken, input({ busy: true }), 1500))
check('taken, it is still kept a while', m.sentAfter(taken, input(), 2000), {
  sent: taken,
  restore: null,
})
check(
  'so a failure said late still puts the words back',
  m.sentAfter(taken, input({ error: 'Could not add it' }), 2000),
  { sent: null, restore: 'Sounds good' },
)
check('and after four seconds it is forgotten', m.sentAfter(taken, input(), 5000).sent, null)
check('the page looks at it again then', Object.keys(m.waitingSends({ taken }, 2000)), ['taken'])
const boxed = card('mail:1', { blocks: [{ type: 'input', ...input() }] })
check(
  'every box on the cards, by key',
  [...m.inputsOf([boxed]).keys()],
  [m.inputKey('mail:1', 'reply')],
)

// ── which cards are held ─────────────────────────────────────────────────

check('the pointer goes on a card', m.holdChanges([], ['a', null], ['a', 'b']), {
  held: ['a'],
  changes: [['a', true]],
})
check('and on to the next', m.holdChanges(['a'], ['b', null], ['a', 'b']), {
  held: ['b'],
  changes: [
    ['b', true],
    ['a', false],
  ],
})
check('typing in the card under the pointer is one hold', m.holdChanges([], ['a', 'a'], ['a']), {
  held: ['a'],
  changes: [['a', true]],
})
check('nothing new: nothing said', m.holdChanges(['a'], ['a', null], ['a']).changes, [])
check('a card that went is forgotten without a word', m.holdChanges(['a'], [null, null], []), {
  held: [],
  changes: [],
})
check('a card not drawn cannot be held', m.holdChanges([], ['z', null], ['a']).held, [])

const words = {
  [m.inputKey('a', 'reply')]: 'Sounds good, I can do Thursday if',
  [m.inputKey('b', 'say')]: '   ',
  [m.inputKey('c', 'answer')]: '',
  [m.inputKey('a', 'note')]: 'and',
}
check('a card with words in a box is written in, once', m.writingIn(words), ['a'])
check('spaces are not words', m.writingIn({ [m.inputKey('b', 'say')]: ' \n ' }), [])
check('nothing typed', m.writingIn({}), [])
check(
  'held with the pointer elsewhere and the keyboard given back',
  m.holdChanges([], [null, null, ...m.writingIn(words)], ['a', 'b']),
  { held: ['a'], changes: [['a', true]] },
)
check(
  'and let go once the words are sent',
  m.holdChanges(['a'], [null, null, ...m.writingIn({ [m.inputKey('a', 'reply')]: '' })], ['a']),
  { held: [], changes: [['a', false]] },
)

// Whether the webview follows the pointer, so a card held by it can wait for it to leave.
const sign = (type, x = 10, y = 20, buttons = 0) => ({ type, buttons, clientX: x, clientY: y })
ok('a move somewhere new: followed', m.followsPointer(sign('pointermove'), { x: 9, y: 20 }))
ok('the first move seen: followed', m.followsPointer(sign('pointermove'), null))
ok(
  'a move to where it already was is the webview’s own, after the layout changed',
  !m.followsPointer(sign('pointermove'), { x: 10, y: 20 }),
)
ok(
  'a move with a button down is a press dragged',
  !m.followsPointer(sign('pointermove', 30, 40, 1), { x: 10, y: 20 }),
)
const pressSigns = ['pointerover', 'pointerdown', 'pointerup'].map((type) => sign(type, 30, 40))
ok(
  'a press, and what WebKit opens one with, are not followed: a macOS panel hears only those',
  pressSigns.every((event) => !m.followsPointer(event, { x: 10, y: 20 })),
)
ok(
  'until then, a hold by the pointer lapses after a few seconds',
  m.HOVER_LAPSE_MS >= 1000 && m.HOVER_LAPSE_MS < m.LOCK_MS * 2,
)

// ── what is kept by card ─────────────────────────────────────────────────

const byCard = { a: true, b: false }
ok('every card still up: the same object', m.keepCards(byCard, [card('a'), card('b')]) === byCard)
check('a card that went takes its entry', m.keepCards(byCard, [card('b')]), { b: false })
check('all gone', m.keepCards(byCard, []), {})
check('without', m.without(byCard, 'a'), { b: false })
check('without leaves the original', byCard, { a: true, b: false })
const drafts = { [m.inputKey('a', 'reply')]: 'hi', [m.inputKey('b', 'say')]: 'yo' }
ok(
  'drafts of cards still up: the same object',
  m.keepByCard(drafts, [card('a'), card('b')]) === drafts,
)
check('a card that went takes its draft', Object.values(m.keepByCard(drafts, [card('b')])), ['yo'])

// ── time ─────────────────────────────────────────────────────────────────

check('a clock', m.clockText(0, 65_000), '1:05')
check('just started', m.clockText(1000, 1500), '0:00')
check('past the hour', m.clockText(0, 3_723_000), '1:02:03')
check('a clock that has not started yet is nought', m.clockText(5000, 1000), '0:00')
check('nonsense is nought', m.clockText(Number.NaN, 1000), '0:00')

const expires = (over = {}) => ({
  afterMs: 45_000,
  action: 'expire',
  remainingMs: 30_000,
  held: false,
  ...over,
})
check('the bar starts where the shell says it is', m.expiryView(expires()), {
  durationMs: 45_000,
  delayMs: -15_000,
  fraction: 30_000 / 45_000,
  held: false,
})
check(
  'more left than the whole is the whole',
  m.expiryView(expires({ remainingMs: 90_000 })).delayMs,
  0,
)
check('none left is empty', m.expiryView(expires({ remainingMs: 0 })).fraction, 0)
check('held, it stops', m.expiryView(expires({ held: true })).held, true)
check('no expiry, no bar', m.expiryView(undefined), null)
check('nor for a nonsense one', m.expiryView(expires({ afterMs: 0 })), null)
ok(
  'a new time left starts the bar again',
  m.expiryKey(expires()) !== m.expiryKey(expires({ remainingMs: 29_000 })),
)
ok(
  'held, the same time left is the same bar',
  m.expiryKey(expires({ held: true })) === m.expiryKey(expires({ held: true })),
)

// ── the theme ────────────────────────────────────────────────────────────

check('forced for a review', m.pickTheme('dusk', 'tide', 'mica', false), 'dusk')
check('the shell’s, before what is stored', m.pickTheme(null, 'tide', 'mica', false), 'tide')
check('stored, when the shell has not said', m.pickTheme(null, null, 'mica', false), 'mica')
check('the desktop, when nothing is', m.pickTheme(null, null, null, true), 'dark')
check('light by day', m.pickTheme(null, null, 'system', false), 'light')
check(
  'a theme since removed is passed over',
  m.pickTheme('neon', null, 'mica-dark', false),
  'mica-dark',
)

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

await close()
finish('panel')
