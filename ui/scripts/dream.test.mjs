// The pure pieces of dreaming: ordinal words, the memory groups, and the
// digest fold. See docs/plans/dreaming.md and `src/lib/dream.ts`.
//
// No stores, no browser -- these are checked as the rules they are.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: dream, close } = await load('/src/lib/dream.ts')
const { ordinal, monthlySchedule, groupMemories, splitDigest, DIGEST_MARKER } = dream

// ── Ordinal words ───────────────────────────────────────────────────────

assert.equal(ordinal(1), '1st')
assert.equal(ordinal(2), '2nd')
assert.equal(ordinal(3), '3rd')
assert.equal(ordinal(4), '4th')
// The teens are all "th", including the ones that end in 1, 2 or 3.
assert.equal(ordinal(11), '11th')
assert.equal(ordinal(12), '12th')
assert.equal(ordinal(13), '13th')
assert.equal(ordinal(21), '21st')
assert.equal(ordinal(28), '28th')

// ── A monthly schedule's words ────────────────────────────────────────────

assert.equal(
  monthlySchedule({ type: 'schedule', at: '04:00', days: [], dayOfMonth: 1 }),
  'Monthly on the 1st at 04:00',
)
// No day of the month: not this trigger's business, so the caller falls
// back to whatever the service says.
assert.equal(monthlySchedule({ type: 'schedule', at: '07:00', days: ['mon'] }), null)
assert.equal(monthlySchedule({ type: 'manual' }), null)
assert.equal(monthlySchedule({ type: 'taskDue', leadDays: 1 }), null)

// ── The memory list's three groups ───────────────────────────────────────

const memory = (id, origin) => ({
  id,
  text: id,
  sourceId: null,
  pinned: false,
  origin,
  createdAt: '2026-01-01T00:00:00Z',
  updatedAt: '2026-01-01T00:00:00Z',
})

const groups = groupMemories([
  memory('m-told', 'told'),
  memory('m-bare', undefined), // sealed before provenance existed
  memory('m-confirmed', 'confirmed'),
  memory('m-inferred', 'inferred'),
  memory('m-rejected', 'rejected'),
])

assert.deepEqual(
  groups.told.map((m) => m.id),
  ['m-told', 'm-bare', 'm-confirmed'],
  'told and confirmed sit together, and an absent origin reads as told',
)
assert.deepEqual(
  groups.noticed.map((m) => m.id),
  ['m-inferred'],
)
assert.deepEqual(
  groups.unassumed.map((m) => m.id),
  ['m-rejected'],
)

// ── The transcript fold ───────────────────────────────────────────────────

assert.equal(DIGEST_MARKER, '--- digest ---')

const withDigest = splitDigest(
  ['Here is yesterday.', DIGEST_MARKER, '## Tasks', '- one finished'].join('\n'),
)
assert.equal(withDigest.text, 'Here is yesterday.')
assert.equal(withDigest.digest, '## Tasks\n- one finished')

const withoutDigest = splitDigest('Just an ordinary question.')
assert.equal(withoutDigest.text, 'Just an ordinary question.')
assert.equal(withoutDigest.digest, null)

// A message that only mentions the marker in passing, not as its own line,
// is not folded -- the match is a whole line, not a substring.
const notALine = splitDigest('The report said "--- digest ---" was odd phrasing.')
assert.equal(notALine.digest, null)

await close()
console.log('dream: all checks passed')
