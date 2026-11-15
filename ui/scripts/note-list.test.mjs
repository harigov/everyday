// Behaviour checks for the notes list's rules -- see `note-list.ts`: the
// sections it is cut into, the tags that earn a chip, and the date at the
// end of a row.
//
// Dates are built relative to now rather than hard-coded, for the reason
// `entry-rows.test.mjs` gives: `groupLabel` answers "Today" and "Yesterday"
// from the clock the suite runs under.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: noteList, close } = await load('/src/lib/note-list.ts')
const {
  toNoteRows,
  sortNotes,
  shortlistTags,
  sameTag,
  rowWhen,
  countOrigins,
  noteOrigin,
  originLabel,
  originLine,
  TOP_TAGS,
} = noteList

const DAY = 86_400_000

/** A note changed `changed` days ago and made `made` days ago. */
function note(id, { changed = 0, made = changed, pinned = false, origin = 'own', title } = {}) {
  return {
    id,
    title: title ?? `Note ${id}`,
    excerpt: '',
    tags: [],
    pinned,
    wordCount: 0,
    attachmentCount: 0,
    origin,
    createdAt: new Date(Date.now() - made * DAY).toISOString(),
    updatedAt: new Date(Date.now() - changed * DAY).toISOString(),
  }
}

// ── Sections ───────────────────────────────────────────────────────────

assert.deepEqual(toNoteRows([], 'updatedDesc'), [])

{
  const rows = toNoteRows(
    [note('p', { changed: 40, pinned: true }), note('a'), note('b'), note('c', { changed: 1 })],
    'updatedDesc',
  )
  assert.deepEqual(
    rows.map((r) => (r.kind === 'header' ? r.label : r.key)),
    ['Pinned', 'p', 'Today', 'a', 'b', 'Yesterday', 'c'],
    'pinned first under its own heading, then one heading per run of days',
  )
  assert.equal(new Set(rows.map((r) => r.key)).size, rows.length, 'every key is unique')
}

{
  // "Newest" files a row under when it was made, not when it was changed.
  const rows = toNoteRows([note('a', { changed: 0, made: 1 })], 'createdDesc')
  assert.equal(rows[0].label, 'Yesterday')
}

{
  // By title there are no days to head anything with.
  const plain = toNoteRows([note('a'), note('b', { changed: 30 })], 'titleAsc')
  assert.deepEqual(
    plain.map((r) => r.kind),
    ['note', 'note'],
    'one undivided run, with no heading over it',
  )
  const pinned = toNoteRows([note('p', { pinned: true }), note('a')], 'titleAsc')
  assert.deepEqual(
    pinned.map((r) => (r.kind === 'header' ? r.label : r.key)),
    ['Pinned', 'p', 'Notes', 'a'],
    'a pinned run is told apart from the rest',
  )
}

// ── Order ──────────────────────────────────────────────────────────────

{
  // A row patched in from a change event lands where a reload would put
  // it: after the pinned ones, among the days it belongs to.
  const list = [
    note('p', { changed: 40, pinned: true }),
    note('a', { changed: 2 }),
    note('b', { changed: 30 }),
    note('fresh', { changed: 1 }),
  ]
  assert.deepEqual(
    sortNotes(list, 'updatedDesc').map((n) => n.id),
    ['p', 'fresh', 'a', 'b'],
  )
  const labels = toNoteRows(sortNotes(list, 'updatedDesc'), 'updatedDesc')
    .filter((r) => r.kind === 'header')
    .map((r) => r.label)
  assert.equal(new Set(labels).size, labels.length, `each section once: ${labels}`)
  const titled = [
    note('2', { title: 'beta' }),
    note('1', { title: 'Alpha' }),
    note('0', { title: 'beta' }),
    note('p', { title: 'Zed', pinned: true }),
  ]
  assert.deepEqual(
    sortNotes(titled, 'titleAsc').map((n) => n.id),
    ['p', '1', '0', '2'],
    'pinned first, then case aside, then by id -- the backend sort_notes order',
  )
}

// ── Tags ───────────────────────────────────────────────────────────────

function tag(name, count) {
  return { tag: name, count }
}

{
  const few = [tag('a', 3), tag('b', 1), tag('c', 1)]
  assert.deepEqual(shortlistTags(few, null), { shown: few, rest: 0 }, 'a short list is drawn whole')
}

{
  // Most used first, as `note_tags` sends them: a few used often, many once.
  const many = [
    tag('meeting', 8),
    tag('work', 5),
    tag('books', 4),
    tag('ideas', 3),
    tag('travel', 2),
    tag('baking', 1),
    tag('bike', 1),
    tag('garden', 1),
    tag('kids', 1),
  ]
  const { shown, rest } = shortlistTags(many, null)
  assert.deepEqual(
    shown.map((t) => t.tag),
    ['meeting', 'work', 'books', 'ideas', 'travel'],
    'a tag on one note never takes a chip from a long list',
  )
  assert.equal(rest, many.length - shown.length, 'the rest are counted for the More menu')
  assert.ok(shown.length <= TOP_TAGS)

  const picked = shortlistTags(many, 'Garden')
  assert.deepEqual(
    picked.shown.map((t) => t.tag).at(-1),
    'garden',
    'the tag in force is drawn wherever it ranks, case aside',
  )
  assert.equal(picked.rest, many.length - picked.shown.length)

  // The tag in force, gone from every note: still drawn, so the filter can
  // be seen and let go of, and not counted among the rest.
  const gone = shortlistTags(many, 'boats')
  assert.deepEqual(gone.shown.at(-1), { tag: 'boats', count: 0 })
  assert.equal(gone.rest, many.length - (gone.shown.length - 1))
  assert.deepEqual(
    shortlistTags([], 'boats').shown,
    [{ tag: 'boats', count: 0 }],
    'even when no tag is left at all',
  )
  assert.ok(sameTag('Boats', 'boats') && !sameTag('boats', null))
}

// ── Row dates ──────────────────────────────────────────────────────────

{
  const now = new Date()
  const yesterday = note('y', { changed: 1 })
  // Under a "Yesterday" heading the row gives only the time...
  assert.doesNotMatch(rowWhen(yesterday, 'updatedDesc', now), /yesterday/i)
  // ...but pinned, or sorted by title, nothing above it names the day.
  assert.match(rowWhen({ ...yesterday, pinned: true }, 'updatedDesc', now), /yesterday/i)
  assert.match(rowWhen(yesterday, 'titleAsc', now), /yesterday/i)
}

// ── Kinds ──────────────────────────────────────────────────────────────

assert.deepEqual(
  countOrigins([
    note('a'),
    note('b', { origin: 'assistant' }),
    note('c', { origin: 'meeting' }),
    note('d', { origin: 'meeting' }),
  ]),
  { own: 1, assistant: 1, meeting: 2 },
)
assert.equal(
  noteOrigin({ origin: 'assistant', tags: ['meeting'] }),
  'assistant',
  'said beats guessed',
)
assert.equal(noteOrigin({ tags: ['Meeting'] }), 'meeting', 'an older note read from its tags')
assert.equal(noteOrigin({ origin: null, tags: ['research'] }), 'assistant')
assert.equal(noteOrigin({ tags: ['boats'] }), 'own')
assert.equal(originLabel('assistant', 'Sakha'), 'Sakha', 'the assistant goes by its own name')
assert.equal(originLabel('meeting', 'Sakha'), 'Meetings')
assert.equal(originLine('own', 'Sakha'), null, 'the person’s own notes say nothing about it')
assert.equal(originLine('assistant', 'Sakha'), 'Written by Sakha')

await close()
console.log('note-list: all checks passed')
