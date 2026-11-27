// Behaviour checks for young Jatayu, the assistant's bird: which bird a stored
// choice means, and what the bird is doing.
//
// Same reasoning as `agent.test.mjs`: the scene is checked by looking at it,
// and these are the rules beside it, whose failures are quiet. A stored word
// read as "no bird" takes the bird away from somebody who chose one; a `null`
// read as "the default" brings back a bird somebody sent away. A turn that has
// finished but is still drawn as typing is a header that says the assistant
// is busy when it is not -- the one thing the bird is for getting wrong.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const {
  modules: [companion, agent],
  close,
} = await load(['/src/lib/companion.ts', '/src/lib/agent.ts'])
const {
  CHEER_MS,
  DEFAULT_LOOK,
  DROOP_MS,
  PLUMAGES,
  actChangesIn,
  actOf,
  companionOf,
  headerAct,
  lookOf,
  plumageOf,
  sniffs,
} = companion
const { emptyTurn, liveTurn } = agent

// ── reading a stored choice ───────────────────────────────────────────

// `null` is somebody who chose no bird, and only that.
assert.equal(lookOf(null), null)

// Missing -- a backend older than the field, or nothing chosen yet -- is the
// default bird, as the vault reads a record with no key.
assert.deepEqual(lookOf(undefined), DEFAULT_LOOK)
assert.deepEqual(lookOf({}), DEFAULT_LOOK)
assert.deepEqual(DEFAULT_LOOK, { plumage: 'gold' })

// A plumage this build knows is the one drawn.
assert.deepEqual(lookOf({ plumage: 'dusk' }), { plumage: 'dusk' })

// A word this build does not know -- one a later build added -- is drawn as
// the default rather than refused.
assert.deepEqual(lookOf({ plumage: 'morning-mist' }), { plumage: 'gold' })

// The companion was a dog first, stored as a breed, a coat and its markings.
// A vault that chose one still hands those words over; none of them names a
// plumage, so it is the default bird, not an error and not no bird.
assert.deepEqual(lookOf({ breed: 'pug', coat: 'charcoal', markings: 'mask' }), DEFAULT_LOOK)

// What is written back is the word itself, default or not, so a later change
// of default does not quietly repaint somebody's bird -- and it reads back
// as the look that was written.
assert.deepEqual(companionOf({ plumage: 'ember' }), { plumage: 'ember' })
assert.deepEqual(companionOf(DEFAULT_LOOK), { plumage: 'gold' })
for (const { id } of PLUMAGES) {
  assert.deepEqual(lookOf(companionOf({ plumage: id })), { plumage: id })
}

// The default is one of the choices, and the first, so Settings can show it
// as chosen and offers it first.
assert.equal(PLUMAGES[0].id, DEFAULT_LOOK.plumage)

// Every word fits what the vault accepts: lower case, digits, hyphens, and
// short. A word the core refuses is a Save that fails for a click on a
// swatch. And no two swatches are the same word.
for (const word of PLUMAGES.map((p) => p.id)) {
  assert.match(word, /^[a-z0-9-]{1,24}$/, `${word} must be a word the vault accepts`)
}
assert.equal(new Set(PLUMAGES.map((p) => p.id)).size, PLUMAGES.length)

// Every plumage carries the colours the scene paints with, as colours.
for (const p of PLUMAGES) {
  for (const colour of [p.base, p.pale, p.deep, ...p.wing]) {
    assert.match(colour, /^#[0-9a-f]{6}$/, `${p.id} has a colour the scene can read`)
  }
  assert.equal(plumageOf(p.id), p)
}

// ── which tools are sniffing ──────────────────────────────────────────

for (const name of ['list_tasks', 'get_entry', 'find_notes', 'search_mail', 'web_search']) {
  assert.ok(sniffs(name), `${name} looks something up`)
}
for (const name of ['read_web_page', 'read_skill', 'lookup_metadata', 'get_weather']) {
  assert.ok(sniffs(name), `${name} looks something up`)
}
for (const name of ['create_task', 'update_plan', 'delete_note', 'log_reading', 'send_mail']) {
  assert.ok(!sniffs(name), `${name} writes something`)
}

// ── what the bird is doing ────────────────────────────────────────────

const NOW = 1_000_000

function card(name, state) {
  return { callId: name, name, arguments: {}, state }
}

// Nothing said yet, or the person's own turn last: sitting.
assert.equal(actOf(undefined, NOW), 'idle')
assert.equal(actOf(emptyTurn('user', 'u1', 'hello'), NOW), 'idle')

// A live turn, through its phases.
const turn = liveTurn('a1', NOW)
assert.equal(actOf(turn, NOW), 'think')

turn.phase = 'preparing'
turn.preparing = 'web_search'
assert.equal(actOf(turn, NOW), 'think', 'choosing a tool is still thinking')

turn.phase = 'tool'
turn.cards.push(card('web_search', 'running'))
assert.equal(actOf(turn, NOW), 'sniff')

turn.cards[0].state = 'done'
turn.cards.push(card('create_task', 'running'))
assert.equal(actOf(turn, NOW), 'type', 'the running card decides, not the first one')

turn.cards[1].state = 'done'
turn.phase = 'writing'
assert.equal(actOf(turn, NOW), 'type')

// A question outranks everything else that is going on.
turn.cards.push({ ...card('delete_task', 'waiting'), confirmKind: 'destructive' })
turn.phase = 'waiting'
assert.equal(actOf(turn, NOW), 'ask')
turn.phase = 'tool'
assert.equal(actOf(turn, NOW), 'ask', 'even mid-tool, a waiting card is a question')

// Finished: a cheer for a while, then sitting again.
const done = { ...liveTurn('a2', NOW), phase: 'done', endedAt: NOW, text: 'Here you are.' }
assert.equal(actOf(done, NOW + 100), 'cheer')
assert.equal(actOf(done, NOW + CHEER_MS + 1), 'idle')
assert.equal(actChangesIn(done, NOW + 100), CHEER_MS - 100)
assert.equal(actChangesIn(done, NOW + CHEER_MS + 1), null, 'nothing left to wait for')

// Failed: moping, for longer.
const failed = { ...done, error: 'the provider said no' }
assert.equal(actOf(failed, NOW + CHEER_MS + 1), 'droop')
assert.equal(actOf(failed, NOW + DROOP_MS + 1), 'idle')
assert.equal(actChangesIn(failed, NOW), DROOP_MS)

// Stopped: neither.
const stopped = { ...done, stopped: true }
assert.equal(actOf(stopped, NOW + 100), 'idle')
assert.equal(actChangesIn(stopped, NOW + 100), null)

// A turn replayed from the vault has no end time, and is never celebrated
// -- opening last week's conversation is not something the bird did.
const replayed = { ...emptyTurn('assistant', 'a3', 'Old answer') }
assert.equal(actOf(replayed, NOW), 'idle')
assert.equal(actChangesIn(replayed, NOW), null)

// A clock read before the turn ended -- the header's, until it next looks --
// still cheers rather than missing the moment.
assert.equal(actOf(done, NOW - 5000), 'cheer')

// ── what the header's bird does ───────────────────────────────────────

// Not set up: asleep, whatever else is true.
assert.equal(headerAct({ ready: false, composing: true, turn: liveTurn('x', NOW) }, NOW), 'sleep')

// Somebody typing to an idle assistant: listening.
assert.equal(headerAct({ ready: true, composing: true, turn: undefined }, NOW), 'listen')
assert.equal(headerAct({ ready: true, composing: false, turn: undefined }, NOW), 'idle')

// But typing the next question while a reply is still being written does
// not stop it writing -- or stop it celebrating the one that just finished.
const writing = { ...liveTurn('w', NOW), phase: 'writing' }
assert.equal(headerAct({ ready: true, composing: true, turn: writing }, NOW), 'type')
assert.equal(headerAct({ ready: true, composing: true, turn: done }, NOW + 100), 'cheer')

await close()
console.log('companion: all checks passed')
