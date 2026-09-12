// Behaviour checks for which suggestion chips are still on offer.
//
// One rule, and it fails silently in both directions:
//
//   leak     `Suggestions` stays mounted across rounds -- the journal's tag
//            row is in the page header for every entry it draws -- so a set
//            of accepted keys with no round on it filters the *next* entry's
//            suggestions by the last one's answers. A `#travel` accepted on
//            Monday never appears again; a reading keyed by position hides
//            position nought for good. Nothing throws. What it looks like is
//            the model getting worse at its job.
//
//   forget   the opposite mistake: clearing on every render, so a chip
//            accepted a moment ago comes straight back beside the three that
//            were not.
//
// Both of those are one comparison apart, which is why they are worth a test
// rather than a comment.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: suggestions, close } = await load('/src/lib/suggestions.ts')
const { NOTHING_TAKEN, exhausted, remaining, roundOf, take } = suggestions

const chip = (key) => ({ key, label: `#${key}` })

// ── Taking one takes only that one ───────────────────────────────────────

{
  const items = [chip('travel'), chip('flights'), chip('mum')]
  const after = take(items, NOTHING_TAKEN, 'flights')

  assert.deepEqual(
    remaining(items, after).map((i) => i.key),
    ['travel', 'mum'],
    'accepting one leaves the others',
  )
  assert.ok(!exhausted(items, after))
}

// ── A different list is a different round ────────────────────────────────

{
  const monday = [chip('travel'), chip('cold')]
  const taken = take(monday, NOTHING_TAKEN, 'travel')
  assert.deepEqual(
    remaining(monday, taken).map((i) => i.key),
    ['cold'],
  )

  // Tuesday's entry suggests `travel` too. It must be on offer: it has never
  // been declined *here*, and the record it would go on is a different one.
  const tuesday = [chip('travel'), chip('boat')]
  assert.deepEqual(
    remaining(tuesday, taken).map((i) => i.key),
    ['travel', 'boat'],
    'answers do not leak between rounds',
  )

  // The worse version of the same bug: readings are keyed by position, so a
  // leak hides whatever happens to be first in every later list.
  const readings = [
    { key: '0', label: 'Ibuprofen 400mg' },
    { key: '1', label: 'Slept 6h' },
  ]
  const tookFirst = take(readings, NOTHING_TAKEN, '0')
  const nextDay = [
    { key: '0', label: 'Ran 5km' },
    { key: '1', label: 'Slept 8h' },
  ]
  assert.equal(remaining(nextDay, tookFirst).length, 2, 'position nought is not hidden for ever')
}

// ── The same offer about a different record is a different round ─────────

{
  // The plainest case, and the one the chips alone cannot tell apart: two
  // entries whose suggested tags are an identical list. Accepting `#travel`
  // on Monday must not make it silently missing from Tuesday -- which reads
  // as the model getting worse, not as a bug.
  const offer = [chip('travel')]
  const onMonday = take(offer, NOTHING_TAKEN, 'travel', 'entry-monday')

  assert.equal(remaining(offer, onMonday, 'entry-monday').length, 0, 'gone where it was taken')
  assert.deepEqual(
    remaining(offer, onMonday, 'entry-tuesday').map((i) => i.key),
    ['travel'],
    'still on offer about a different record',
  )

  // And the scope alone is not enough either: a fresh answer about the same
  // record is a fresh offer.
  const later = [chip('travel'), chip('cold')]
  assert.equal(remaining(later, onMonday, 'entry-monday').length, 2)
}

// ── The same list twice is the same round ────────────────────────────────

{
  // Pressing the button again on an entry that has not changed produces the
  // same offers, and one already declined should stay declined -- otherwise
  // a re-render is enough to bring back a chip somebody just took.
  const items = [chip('travel'), chip('cold')]
  const taken = take(items, NOTHING_TAKEN, 'travel', 'entry-1')
  const again = [chip('travel'), chip('cold')]
  assert.deepEqual(
    remaining(again, taken, 'entry-1').map((i) => i.key),
    ['cold'],
  )
  assert.equal(roundOf(items, 'entry-1'), roundOf(again, 'entry-1'))
  assert.notEqual(roundOf(items, 'entry-1'), roundOf(items, 'entry-2'))
}

// ── Taking the last one empties the row ──────────────────────────────────

{
  const items = [chip('travel')]
  const taken = take(items, NOTHING_TAKEN, 'travel')
  assert.ok(exhausted(items, taken), 'nothing left is what closes the row')

  // And an empty offer is exhausted rather than an open row with no chips.
  assert.ok(exhausted([], NOTHING_TAKEN))
}

// ── Nothing is mutated on the way through ────────────────────────────────

{
  const items = [chip('a'), chip('b')]
  const first = take(items, NOTHING_TAKEN, 'a')
  const second = take(items, first, 'b')

  assert.equal(NOTHING_TAKEN.keys.size, 0, 'the shared empty record stays empty')
  assert.deepEqual([...first.keys], ['a'], 'the earlier record is not written through')
  assert.deepEqual([...second.keys].sort(), ['a', 'b'])
}

await close()
console.log('suggestions: all checks passed')
