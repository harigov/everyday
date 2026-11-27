// Behaviour checks for `overview.svelte.ts`'s visits: what a mount of the
// Overview reads, and what it leaves alone.
//
// The page used to read every card on every visit -- three Inbox walks and
// the follow-up scan included -- however little had changed. It now reads
// what a change made stale, what it never read, and what is older than the
// window trusts. Each check below counts the commands a visit sends, through
// the real mock backend, the same way `calendar-store.test.mjs` drives its
// store.
//
// Node is not the desktop shell, so these run the way a phone or a browser
// does: a read is trusted for `REVISIT_MS` rather than `FRESH_MS`. The rules
// about which change moves which card are the same either way.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

stubBrowser({
  window: globalThis,
  localStorage: true,
  matchMedia: true,
  document: {
    querySelector: () => null,
    documentElement: {
      style: { setProperty: () => {} },
      classList: { toggle: () => {} },
      setAttribute: () => {},
      removeAttribute: () => {},
    },
    addEventListener: () => {},
  },
  navigator: { userAgent: 'node', language: 'en-GB' },
  location: new URL('http://localhost/?unlocked=1'),
  createObjectURL: () => 'blob:stub',
})

const {
  modules: [overviewModule, stateModule, apiModule, todoModule],
  close,
} = await load(
  [
    '/src/lib/overview.svelte.ts',
    '/src/lib/state.svelte.ts',
    '/src/lib/api.ts',
    '/src/lib/todo.svelte.ts',
  ],
  { svelte: true },
)
const { overview } = overviewModule
const { app } = stateModule
const { api } = apiModule
const { todo } = todoModule

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')
assert.ok(overview.enabled, 'the mock vault keeps an Overview')

/** Every command sent through `api`, by name, since the last `sent()`. */
let calls = []
for (const [name, fn] of Object.entries(api)) {
  if (typeof fn !== 'function') continue
  api[name] = (...args) => {
    calls.push(name)
    return fn.apply(api, args)
  }
}
/** What was sent since the last call, and start counting again. */
function sent() {
  const out = calls
  calls = []
  return out
}

async function until(test, what, ms = 3000) {
  const by = Date.now() + ms
  while (!test()) {
    if (Date.now() > by) throw new Error(`timed out waiting for ${what}`)
    await new Promise((r) => setTimeout(r, 10))
  }
}

// The default page: the brief, Karma, today's events and tasks, the three
// Inbox cards, what's going on and today's habits.
const MAIL = ['threads', 'threadsAcross', 'mailFollowUps', 'mailboxes']
const mailIn = (list) => list.filter((name) => MAIL.includes(name))

// ── the first visit reads the page ───────────────────────────────────

assert.equal(overview.page, 'home')
await overview.start()
const first = sent()
assert.ok(first.includes('karma'), 'the first visit reads Karma')
assert.ok(first.includes('tasks'), "and today's tasks")
assert.ok(first.includes('mailFollowUps'), 'and the follow-ups')
assert.equal(overview.loading, false, 'loading settles once the page lands')

// ── coming straight back reads nothing ───────────────────────────────

await overview.start()
assert.deepEqual(
  sent().filter((name) => name !== 'routines' && name !== 'briefs'),
  [],
  'a second visit moments later sends no reads for the cards',
)

// ── a task changing re-reads what it can move, not the mail ──────────

overview.noteChange('task')
await overview.start()
{
  const read = sent()
  assert.ok(read.includes('tasks'), "a task change re-reads today's tasks")
  assert.ok(read.includes('karma'), 'and Karma')
  assert.deepEqual(mailIn(read), [], 'and walks no Inbox')
}

// ── a thread changing re-reads the mail, and only the mail ───────────

overview.noteChange('thread')
await overview.start()
{
  const read = sent()
  assert.ok(read.includes('mailFollowUps'), 'a thread change re-reads the follow-ups')
  assert.ok(!read.includes('karma'), 'and not Karma')
  assert.ok(!read.includes('tasks'), "nor today's tasks")
}

// ── a change that touches nothing on the page reads nothing ──────────

overview.noteChange('job')
overview.noteChange('conversation')
await overview.start()
assert.deepEqual(
  sent().filter((name) => name !== 'routines' && name !== 'briefs'),
  [],
  "a job's progress is no reason to read the page again",
)

// ── an old read is read again ────────────────────────────────────────

{
  const realNow = Date.now
  Date.now = () => realNow() + 11_000
  try {
    await overview.start()
  } finally {
    Date.now = realNow
  }
  const read = sent()
  assert.ok(read.includes('karma') && read.includes('mailFollowUps'), 'a stale page is read whole')
}

// ── while on screen, a change re-reads only what it moved ────────────

overview.noteChange('task')
overview.noteChange('thread')
await overview.liveRefresh()
{
  const read = sent()
  assert.ok(read.includes('tasks'), 'a live refresh re-reads the tasks')
  assert.deepEqual(mailIn(read), [], 'and leaves mail read under a minute ago for the next visit')
}
await overview.start()
assert.ok(sent().includes('mailFollowUps'), 'which reads the mail the live refresh left')

// A change landing while a read is on its way may not be in that read's
// answer, so the next refresh reads it again rather than waiting on it.
overview.noteChange('task')
{
  const first = overview.liveRefresh()
  overview.noteChange('task')
  await overview.liveRefresh()
  await first
  const read = sent()
  assert.equal(read.filter((name) => name === 'karma').length, 2, 'a change mid-read reads again')
}
// And without a change, a refresh leaves a read on its way to land.
overview.noteChange('task')
{
  const first = overview.liveRefresh()
  await overview.liveRefresh()
  await first
  const read = sent()
  assert.equal(read.filter((name) => name === 'karma').length, 1, 'a read on its way is let land')
}
assert.equal(overview.loading, false, 'and loading settles after both')

// ── the Brief page reads none of the cards ───────────────────────────

overview.page = 'brief'
overview.noteChange('task')
overview.noteChange('thread')
await overview.start()
assert.deepEqual(
  sent().filter((name) => name !== 'routines' && name !== 'briefs'),
  [],
  'a visit to the Brief page reads no card',
)
// Back to Home, in the app: the cards it skipped are read now.
app.section = 'overview'
overview.page = 'home'
await until(() => calls.includes('karma') && calls.includes('mailFollowUps'), 'Home to be read')
await until(() => !overview.loading, 'the read to land')
sent()

// ── a lock forgets what was read ─────────────────────────────────────

overview.reset()
await overview.start()
{
  const read = sent()
  assert.ok(read.includes('karma') && read.includes('mailFollowUps'), 'after a lock, all of it')
}

// ── ticking a task on "Tasks for today" ──────────────────────────────

{
  const row = {
    id: 'overview-test-task',
    title: 'Ticked from the Overview',
    status: 'todo',
    completedAt: null,
  }
  overview.todayTasks = [row]
  const realToggle = todo.toggleDoneOf
  let finish = () => {}
  let asked = 0
  todo.toggleDoneOf = (task) => {
    asked += 1
    assert.equal(task.status, 'todo', "the direction is the card's copy, as it was")
    return new Promise((resolve) => (finish = resolve))
  }
  try {
    const ticking = overview.tickTask(overview.todayTasks[0])
    assert.equal(overview.todayTasks[0].status, 'done', 'the box ticks before the write lands')
    assert.ok(overview.todayTasks[0].completedAt, 'and reads as finished')
    await overview.tickTask(overview.todayTasks[0])
    assert.equal(asked, 1, 'a second press while the first is on its way is let go')
    finish(null)
    await ticking
    assert.equal(overview.todayTasks[0].status, 'todo', 'a write that did not go is taken back')
    assert.equal(overview.todayTasks[0].completedAt, null)

    const again = overview.tickTask(overview.todayTasks[0])
    const saved = { ...row, status: 'done', completedAt: '2026-11-26T09:00:00.000Z' }
    finish(saved)
    await again
    assert.equal(overview.todayTasks[0].status, 'done', 'a write that went stays ticked')
    assert.equal(overview.todayTasks[0].completedAt, saved.completedAt, 'as it was written')
  } finally {
    todo.toggleDoneOf = realToggle
  }
}

await close()
console.log('overview-store: all checks passed')

// `app.start()` arms the idle-vault poll, and the page starts the calendar's
// clock; see `editor.test.mjs`'s own note.
process.exit(0)
