// Behaviour checks for the meeting notes formatters: stage labels, the
// recording pill's timer, and the speed-check sentence in Settings.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: fmt, close } = await load('/src/lib/meetings-format.ts')
const {
  formatRealtimeFactor,
  formatTimer,
  isTooSlow,
  speakerColor,
  speakerNameStyle,
  stageIsActive,
  stageLabel,
} = fmt

// ── stage labels ──────────────────────────────────────────────────────

assert.equal(stageLabel({ type: 'recording' }), 'Recording')
assert.equal(stageLabel({ type: 'transcribing' }), 'Transcribing')
assert.equal(stageLabel({ type: 'identifying' }), 'Working out who spoke')
assert.equal(stageLabel({ type: 'summarising' }), 'Writing the note')
assert.equal(stageLabel({ type: 'done' }), 'Done')
assert.equal(
  stageLabel({ type: 'failed', reason: 'no speech found', at: { type: 'recording' } }),
  'Failed',
)

assert.ok(stageIsActive({ type: 'recording' }))
assert.ok(stageIsActive({ type: 'summarising' }))
assert.ok(!stageIsActive({ type: 'done' }))
assert.ok(!stageIsActive({ type: 'failed', reason: 'x', at: { type: 'recording' } }))

// ── the timer ─────────────────────────────────────────────────────────

assert.equal(formatTimer(0), '0:00')
assert.equal(formatTimer(42_000), '0:42')
assert.equal(formatTimer(65_000), '1:05')
assert.equal(formatTimer(3_661_000), '1:01:01', 'an hour rolls the format over to h:mm:ss')
assert.equal(formatTimer(-5), '0:00', 'never negative')

// ── the speed check ─────────────────────────────────────────────────────

assert.equal(formatRealtimeFactor(3.2), 'about 3.2× real time on this computer')
assert.equal(formatRealtimeFactor(0.8), 'about 0.8× real time on this computer')
assert.ok(isTooSlow(0.8))
assert.ok(isTooSlow(1.4))
assert.ok(!isTooSlow(1.5), 'the threshold itself is not too slow')
assert.ok(!isTooSlow(3.2))

// ── speakers ─────────────────────────────────────────────────────────

assert.equal(
  speakerColor(0, true),
  'var(--accent)',
  'the owner is always the accent, whichever key they hold',
)
assert.equal(speakerColor(3, false), speakerColor(3, false), 'stable for the same key')
assert.notEqual(
  speakerColor(1, false),
  speakerColor(2, false),
  'two different speakers read as two colours',
)

assert.equal(speakerNameStyle({ type: 'owner' }), 'plain')
assert.equal(speakerNameStyle({ type: 'matched', score: 0.9 }), 'plain')
assert.equal(speakerNameStyle({ type: 'named' }), 'plain')
assert.equal(speakerNameStyle({ type: 'inferred' }), 'guessed')
assert.equal(speakerNameStyle({ type: 'unknown' }), 'unknown')

await close()
console.log('meetings-format: all checks passed')
