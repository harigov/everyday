// Behaviour checks for the meeting notes formatters: stage labels, the
// recording pill's timer, and the speed-check sentence in Settings.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: fmt, close } = await load('/src/lib/meetings-format.ts')
const {
  describeSpeech,
  draftStageLabel,
  formatRealtimeFactor,
  formatTimer,
  isTooSlow,
  quietSpeakers,
  recordingWritingNote,
  speakerColor,
  speakerNameStyle,
  stageIsActive,
  stageLabel,
  wordsBySpeaker,
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

// ── the speech model, in the line meeting notes show ──────────────────

assert.equal(describeSpeech(null), 'none chosen yet')
assert.equal(
  describeSpeech({ transcriber: { type: 'local', model: 'parakeetV3' }, language: null }),
  'Parakeet TDT v3, on this computer',
)
assert.equal(
  describeSpeech({ transcriber: { type: 'openAi', model: 'gpt-4o-transcribe' } }),
  'gpt-4o-transcribe at OpenAI',
)
assert.equal(
  describeSpeech({
    transcriber: { type: 'compatible', baseUrl: 'http://localhost:8000/v1', model: 'whisper-1' },
  }),
  'whisper-1 at localhost:8000',
)
assert.equal(
  describeSpeech({ transcriber: { type: 'google', model: ' ' } }),
  'a model not yet named at Google',
)

// ── a call's draft note ───────────────────────────────────────────────

assert.equal(draftStageLabel({ type: 'transcribing' }), 'Transcribing the call')
assert.equal(draftStageLabel({ type: 'identifying' }), 'Working out who spoke')
assert.equal(draftStageLabel({ type: 'summarising' }), 'Writing the summary')
assert.equal(draftStageLabel({ type: 'recording' }), null)
assert.equal(draftStageLabel({ type: 'done' }), null)

{
  const rec = (id, noteId, stage) => ({ id, noteId, stage })
  const failed = { type: 'failed', reason: 'no model', at: { type: 'summarising' } }
  const list = [
    rec('a', null, { type: 'transcribing' }),
    rec('b', 'n-1', { type: 'summarising' }),
    rec('c', 'n-2', failed),
    rec('d', 'n-3', { type: 'done' }),
    rec('e', 'n-4', { type: 'recording' }),
  ]
  assert.equal(recordingWritingNote(list, 'n-1')?.id, 'b', 'a draft is found by its note')
  assert.equal(recordingWritingNote(list, 'n-2')?.id, 'c', 'a failed one is found too')
  assert.equal(recordingWritingNote(list, 'n-3'), null, 'a finished one is no draft')
  assert.equal(recordingWritingNote(list, 'n-4'), null, 'nor one still being recorded')
  assert.equal(recordingWritingNote(list, 'nope'), null)
  assert.equal(recordingWritingNote(list, null), null)
  assert.equal(recordingWritingNote(list, undefined), null)
}

// ── quiet voices ──────────────────────────────────────────────────────

{
  const words = (n) => Array.from({ length: n }, () => 'word').join(' ')
  const speakers = [
    { key: 0, label: 'You', how: { type: 'owner' } },
    { key: 1, label: 'Unknown 1', how: { type: 'unknown' } },
    { key: 2, label: 'Unknown 2', how: { type: 'unknown' } },
    { key: 3, label: 'Priya', how: { type: 'named' } },
    { key: 4, label: 'Sam', how: { type: 'matched', score: 0.9 } },
    { key: 5, label: 'Guess', how: { type: 'inferred' } },
    { key: 6, label: 'Unknown 6', how: { type: 'unknown' } },
  ]
  const seg = (speaker, n) => ({ startMs: 0, endMs: 1, speaker, text: words(n) })
  const segments = [
    seg(0, 3),
    seg(1, 200),
    seg(2, 5),
    seg(3, 4),
    seg(4, 2),
    seg(5, 10),
    seg(0, 400),
  ]
  const counts = wordsBySpeaker(segments)
  assert.equal(counts.get(0), 403)
  assert.equal(counts.get(1), 200)
  assert.equal(counts.get(6), undefined, 'a voice with no lines said nothing')
  const quiet = quietSpeakers(speakers, segments)
  assert.deepEqual([...quiet].sort(), [2, 5, 6], 'only unnamed voices with little to say')
  assert.ok(!quiet.has(0), 'never the owner')
  assert.ok(!quiet.has(3) && !quiet.has(4), 'never a named or matched voice')
  assert.ok(!quiet.has(1), 'a voice with plenty to say stays')

  // Plenty of words in absolute terms, but a sliver of a very long call.
  const long = [seg(0, 5000), seg(1, 30)]
  assert.ok(quietSpeakers(speakers, long).has(1), 'under two percent of the call is quiet')
  assert.equal(quietSpeakers([], []).size, 0)
}

await close()
console.log('meetings-format: all checks passed')
