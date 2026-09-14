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
const {
  byManualOrder,
  planDrop,
  sectionPatch,
  zoneAt,
  dueBucket,
  numberOrder,
  moveCard,
  ORDER_STEP,
} = lib

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
const ALL = new Set(TASKS.map((t) => t.id))
const drop = (dragged, target, drawn = ALL) => planDrop(TASKS, drawn, dragged, target)
const top = (id, zone) => ({ id, zone, parentId: null })
const under = (parentId, id, zone) => ({ id, zone, parentId })

// ── order ─────────────────────────────────────────────────────────────

check(
  'ties go to the task added first, not the one loaded first',
  byManualOrder([
    task('x', { sortOrder: 1 }),
    task('z', { createdAt: '2026-09-02' }),
    task('y', { createdAt: '2026-09-01' }),
  ]).map((t) => t.id),
  ['y', 'z', 'x'],
)

// ── beside ────────────────────────────────────────────────────────────

check('before a row', drop('c', top('a', 'before')), {
  parentId: null,
  order: ['c', 'a', 'b', 'd'],
})
check('after a row', drop('a', top('b', 'after')), {
  parentId: null,
  order: ['b', 'a', 'c', 'd'],
})
check('beside a subtask moves it among them', drop('c', under('a', 'a1', 'after')), {
  parentId: 'a',
  order: ['a1', 'c', 'a2'],
})
check('a subtask dragged to the top level', drop('a2', top('b', 'before')), {
  parentId: null,
  order: ['a', 'a2', 'b', 'c', 'd'],
})
check('an orphan drawn at the top level is a top-level sibling', drop('b', top('d', 'after')), {
  parentId: null,
  order: ['a', 'c', 'd', 'b'],
})

// ── onto ──────────────────────────────────────────────────────────────

check('onto a row appends it as the last subtask', drop('b', top('a', 'into')), {
  parentId: 'a',
  order: ['a1', 'a2', 'b'],
})

// ── refused ───────────────────────────────────────────────────────────

check('onto itself', drop('a', top('a', 'into')), null)
check('a task with subtasks cannot become one', drop('a', top('b', 'into')), null)
check('nor be put beside one', drop('a', under('a', 'a1', 'before')), null)
check('only a top-level row takes a drop onto it', drop('b', under('a', 'a1', 'into')), null)

// A parent hidden by a filter: its subtask is drawn at the top level, and
// the drop logic has to agree with what is on screen.
const NO_A = new Set(['b', 'c', 'a1', 'd'])
// The hidden parent keeps its place in the order, so clearing the filter
// does not move it.
check('beside a subtask drawn at the top level', drop('b', top('a1', 'after'), NO_A), {
  parentId: null,
  order: ['a', 'a1', 'b', 'c', 'd'],
})
check(
  'a sibling beside it stays a step of the same hidden job',
  drop('a2', top('a1', 'before'), new Set(['a1', 'a2'])),
  { parentId: 'a', order: ['a2', 'a1'] },
)
check('but not onto one: that would be a third level', drop('b', top('a1', 'into'), NO_A), null)
check('nor onto an orphan whose parent is out of scope', drop('b', top('d', 'into')), null)

// ── numbering ─────────────────────────────────────────────────────────

const values = (pairs) => new Map(pairs)
check(
  'a gap between neighbours is one write',
  [
    ...numberOrder(
      ['p', 'm', 'q'],
      values([
        ['p', 0],
        ['q', 1024],
        ['m', 5000],
      ]),
      'm',
    ),
  ],
  [['m', 512]],
)
check(
  'at the end, a step past the last',
  [
    ...numberOrder(
      ['p', 'q', 'm'],
      values([
        ['p', 0],
        ['q', 1024],
        ['m', 0],
      ]),
      'm',
    ),
  ],
  [['m', 1024 + ORDER_STEP]],
)
check(
  'no gap renumbers, writing only what changes',
  [
    ...numberOrder(
      ['p', 'm', 'q'],
      values([
        ['p', 0],
        ['q', 1],
        ['m', 9],
      ]),
      'm',
    ),
  ],
  [
    ['m', ORDER_STEP],
    ['q', 2 * ORDER_STEP],
  ],
)
check(
  'ties among the rest renumber too',
  [
    ...numberOrder(
      ['p', 'q', 'm'],
      values([
        ['p', 0],
        ['q', 0],
        ['m', 3],
      ]),
      'm',
    ),
  ],
  [
    ['q', ORDER_STEP],
    ['m', 2 * ORDER_STEP],
  ],
)

// ── the board ─────────────────────────────────────────────────────────

const BOARD = [
  task('t1', { sortOrder: 0 }),
  task('d1', { sortOrder: 1, status: 'doing' }),
  task('t2', { sortOrder: 2 }),
  task('d2', { sortOrder: 3, status: 'doing' }),
  task('t3', { sortOrder: 4 }),
]
check('above a card in another column', moveCard(BOARD, 't3', 'doing', 1), [
  't1',
  'd1',
  't2',
  't3',
  'd2',
])
check('at the foot of a column', moveCard(BOARD, 't1', 'doing', 2), ['d1', 't2', 'd2', 't1', 't3'])
check('into an empty column', moveCard(BOARD, 'd1', 'blocked', 0), ['t1', 't2', 'd2', 't3', 'd1'])

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
