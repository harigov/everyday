// `writing/writing.ts`: where writing help's popup goes, what it shows of the
// selection, and how it says what went wrong.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: w, close } = await load('/src/writing/writing.ts')
const { place, preview, trouble, choiceForKey, CHOICES } = w

const viewport = { w: 1600, h: 900 }
const size = { w: 300, h: 220 }

// ── placing the popup on the overlay ─────────────────────────────────────

assert.deepEqual(
  place({ x: 400, y: 300 }, viewport, size),
  { left: 412, top: 312 },
  'below-right of the pointer',
)
assert.deepEqual(
  place({ x: 1500, y: 300 }, viewport, size),
  { left: 1188, top: 312 },
  'flipped left of the pointer near the right edge',
)
assert.deepEqual(
  place({ x: 400, y: 850 }, viewport, size),
  { left: 412, top: 618 },
  'flipped above the pointer near the bottom',
)
assert.deepEqual(place({ x: 5, y: 5 }, viewport, size), { left: 17, top: 17 })
const tiny = place({ x: 50, y: 50 }, { w: 200, h: 150 }, size)
assert.ok(tiny.left >= 8 && tiny.top >= 8, 'never off the top or left, even when it cannot fit')
assert.deepEqual(
  place(null, viewport, size),
  { left: 650, top: 226.66666666666666 },
  'no pointer: centred, a little high',
)

// ── what the popup shows of the selection ────────────────────────────────

assert.equal(preview('  their   going\nhome  '), 'their going home')
const long = preview('word '.repeat(60))
assert.ok(long.length <= 90 && long.endsWith('…'), 'a long selection is cut with an ellipsis')

// ── the keyboard ─────────────────────────────────────────────────────────

assert.deepEqual(
  CHOICES.map((c) => c.action),
  ['improve', 'grammar', 'concise'],
)
assert.equal(choiceForKey('2').action, 'grammar')
assert.equal(choiceForKey('x'), undefined)

// ── what went wrong ──────────────────────────────────────────────────────

assert.deepEqual(trouble('locked', 'vault is locked'), {
  message: 'Every Day is locked. Unlock it, then try again.',
  openMain: true,
})
assert.equal(trouble('no_vault', 'no vault is open').openMain, true)
const model = trouble(
  'quick',
  'the medium model cannot be used: no endpoint; fix it in Settings → Models',
)
assert.equal(model.openMain, true, 'a model to set up is fixed in the main window')
assert.deepEqual(trouble('quick', 'the model took longer than 60s'), {
  message: 'The model took longer than 60s.',
  openMain: false,
})

assert.deepEqual(
  trouble(
    'quick',
    'CompletionError: HttpError: Http client error: error sending request for url (http://127.0.0.1:1/v1/chat/completions)',
  ),
  {
    message: 'The medium model could not be reached. Check it in Settings → Models.',
    openMain: true,
  },
  'a model that cannot be reached is said plainly',
)

await close()
console.log('writing: all checks passed')
