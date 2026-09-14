// Behaviour checks for the list view's order: where a dragged task lands,
// which drops are refused, and what joining another section changes.
//
// The same reasoning as `menu.test.mjs`. A drop that nests three deep, or
// files a task in a section it is then drawn straight back out of, is not an
// error anybody sees -- it is a list that is quietly wrong from then on.

import { load, makeCheck, stubBrowser } from './harness.mjs'

// `dueBucket` names a weekday in the reader's locale.
stubBrowser({ navigator: true })

const { module: lib, close } = await load('/src/lib/tasklist.ts')
const { bySortOrder, planDrop, sectionPatch, zoneAt, dueBucket } = lib

const { check, finish } = makeCheck()

const TODAY = '2026-09-14'
const task = (id, extra = {}) => ({
  id,
  title: id,
  notes: '',
  status: 'todo',
  priority: 'none',
  tags: [],
  sortOrder: 0,
  parentId: null,
  projectId: null,
  createdAt: '',
  updatedAt: '',
  ...extra,
})

// a, b, c at the top; a has two subtasks; d is a subtask whose parent is not
// loaded, so the list draws it at the top level.
const TASKS = [
  task('a', { sortOrder: 0 }),
  task('b', { sortOrder: 1 }),
  task('c', { sortOrder: 2 }),
  task('a1', { parentId: 'a', sortOrder: 0 }),
  task('a2', { parentId: 'a', sortOrder: 1 }),
  task('d', { parentId: 'gone', sortOrder: 3 }),
]
const top = (id, zone) => ({ id, zone, parentId: null })
const under = (parentId, id, zone) => ({ id, zone, parentId })

// ── order ─────────────────────────────────────────────────────────────

check(
  'ties keep the order they were loaded in',
  bySortOrder([task('x', { sortOrder: 1 }), task('y'), task('z')]).map((t) => t.id),
  ['y', 'z', 'x'],
)

// ── beside ────────────────────────────────────────────────────────────

check('before a row', planDrop(TASKS, 'c', top('a', 'before')), {
  parentId: null,
  order: ['c', 'a', 'b', 'd'],
})
check('after a row', planDrop(TASKS, 'a', top('b', 'after')), {
  parentId: null,
  order: ['b', 'a', 'c', 'd'],
})
check('beside a subtask moves it among them', planDrop(TASKS, 'c', under('a', 'a1', 'after')), {
  parentId: 'a',
  order: ['a1', 'c', 'a2'],
})
check('a subtask dragged to the top level', planDrop(TASKS, 'a2', top('b', 'before')), {
  parentId: null,
  order: ['a', 'a2', 'b', 'c', 'd'],
})
check(
  'an orphan drawn at the top level is a top-level sibling',
  planDrop(TASKS, 'b', top('d', 'after')),
  { parentId: null, order: ['a', 'c', 'd', 'b'] },
)

// ── onto ──────────────────────────────────────────────────────────────

check('onto a row appends it as the last subtask', planDrop(TASKS, 'b', top('a', 'into')), {
  parentId: 'a',
  order: ['a1', 'a2', 'b'],
})

// ── refused ───────────────────────────────────────────────────────────

check('onto itself', planDrop(TASKS, 'a', top('a', 'into')), null)
check('a task with subtasks cannot become one', planDrop(TASKS, 'a', top('b', 'into')), null)
check('nor be put beside one', planDrop(TASKS, 'a', under('a', 'a1', 'before')), null)
check(
  'only a top-level row takes a drop onto it',
  planDrop(TASKS, 'b', under('a', 'a1', 'into')),
  null,
)

// ── zones ─────────────────────────────────────────────────────────────

const BOX = { top: 100, height: 40 }
check(
  'with nesting: quarters beside, middle onto',
  [105, 120, 135].map((y) => zoneAt(y, BOX, { nest: true, openParent: false })),
  ['before', 'into', 'after'],
)
check(
  'without: halves',
  [105, 135].map((y) => zoneAt(y, BOX, { nest: false, openParent: false })),
  ['before', 'after'],
)
check(
  'an open parent has no "after"',
  [105, 135].map((y) => zoneAt(y, BOX, { nest: true, openParent: true })),
  ['before', 'into'],
)
check(
  'nor does it for a task that cannot nest',
  zoneAt(135, BOX, { nest: false, openParent: true }),
  'before',
)

// ── sections ──────────────────────────────────────────────────────────

const ctx = { today: TODAY, projectPurpose: null }
check('status', sectionPatch('status', 'doing', task('x'), ctx), { status: 'doing' })
check('already there', sectionPatch('priority', 'none', task('x'), ctx), {})
check('a weekday', sectionPatch('due', '2026-09-16', task('x'), ctx), { dueDate: '2026-09-16' })
check('today', sectionPatch('due', 'today', task('x'), ctx), { dueDate: TODAY })
check(
  'no date takes the time with it',
  sectionPatch('due', 'none', task('x', { dueDate: TODAY, dueTime: '09:00:00' }), ctx),
  { dueDate: null, dueTime: null },
)
check('overdue has no date to give', sectionPatch('due', 'overdue', task('x'), ctx), null)
check('nor has later', sectionPatch('due', 'later', task('x'), ctx), null)
check(
  'but a task already in one can move within it',
  sectionPatch('due', 'later', task('x', { dueDate: '2026-12-01' }), ctx),
  {},
)
check(
  'the bucket key and the patch agree',
  dueBucket(task('x', { dueDate: '2026-09-16' }), TODAY).key,
  '2026-09-16',
)
check('a goal', sectionPatch('purpose', 'goal:g1', task('x'), ctx), {
  purpose: { type: 'goal', id: 'g1' },
})
check(
  'inherited counts as already there',
  sectionPatch('purpose', 'role:r1', task('x'), {
    ...ctx,
    projectPurpose: { type: 'role', id: 'r1' },
  }),
  {},
)
check(
  'not filed refuses a task its project files',
  sectionPatch('purpose', 'none', task('x', { purpose: { type: 'goal', id: 'g1' } }), {
    ...ctx,
    projectPurpose: { type: 'role', id: 'r1' },
  }),
  null,
)

await close()
finish('tasklist')
