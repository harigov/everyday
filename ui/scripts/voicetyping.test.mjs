// `voicetyping/voicetyping.ts`: what the pill says, what a failure offers, and
// how the level meter moves.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: d, close } = await load('/src/voicetyping/voicetyping.ts')
const { accepts, title, hint, remedy, silence, pushLevel, barHeight, isQuiet, BARS } = d

const state = (over = {}) => ({
  seq: 3,
  phase: 'listening',
  mode: 'toggle',
  handsFree: true,
  key: 'Ctrl+Alt+M',
  message: null,
  text: null,
  pill: 'overlay',
  ...over,
})

// ── newer states win ─────────────────────────────────────────────────────

assert.equal(accepts(null, state()), true)
assert.equal(accepts(state({ seq: 3 }), state({ seq: 3, phase: 'writing' })), true)
assert.equal(accepts(state({ seq: 3 }), state({ seq: 4 })), true)
assert.equal(accepts(state({ seq: 3 }), state({ seq: 2 })), false, 'a late word from the last one')

// ── what it says ─────────────────────────────────────────────────────────

assert.equal(title(state({ phase: 'starting' })), 'Getting ready…')
assert.equal(title(state({ phase: 'transcribing' })), 'Working out the words…')
assert.equal(title(state({ phase: 'writing' })), 'Writing…')
assert.equal(title(state({ phase: 'done' })), 'Typed')
assert.equal(title(state({ phase: 'failed', message: 'Nothing was heard.' })), 'Nothing was heard.')
assert.equal(title(state({ phase: 'failed' })), 'Nothing was typed.')

assert.equal(hint(state()), 'Press Ctrl+Alt+M to finish')
assert.equal(hint(state({ mode: 'hold', handsFree: false })), 'Let go to finish')
assert.equal(
  hint(state({ mode: 'hold', handsFree: true })),
  'Press Ctrl+Alt+M to finish',
  'hands-free after a tap in hold mode',
)
assert.equal(hint(state({ phase: 'writing' })), null)
assert.equal(hint(state({ phase: 'done', message: 'Copied' })), 'Copied')

// ── what a failure offers ────────────────────────────────────────────────

assert.equal(remedy(state()), null)
assert.equal(
  remedy(state({ phase: 'failed', text: 'hello there', message: 'Could not paste.' })),
  'copy',
)
assert.equal(remedy(state({ phase: 'failed', message: 'Karma Yoga is locked.' })), 'open')
assert.equal(remedy(state({ phase: 'failed', message: 'Fix it in Settings → Models.' })), 'open')
assert.equal(remedy(state({ phase: 'failed', message: 'Nothing was heard.' })), null)

// ── the meter ────────────────────────────────────────────────────────────

let history = silence()
assert.equal(history.length, BARS)
assert.equal(isQuiet(history), true)
history = pushLevel(history, 1)
assert.equal(history.length, BARS, 'one in, one out')
assert.ok(history[BARS - 1] > 0.5, 'rises quickly')
const peak = history[BARS - 1]
history = pushLevel(history, 0)
assert.ok(history[BARS - 1] < peak && history[BARS - 1] > 0.1, 'falls slowly')
assert.equal(isQuiet(history), false)
assert.equal(pushLevel(silence(), Number.NaN).at(-1), 0, 'a bad reading is silence')
assert.ok(pushLevel(silence(), 7).at(-1) <= 1, 'clamped')
assert.ok(barHeight(0) > 0 && barHeight(0) < barHeight(0.1) && barHeight(1) > 0.999)

await close()
console.log('voice typing: all checks passed')
