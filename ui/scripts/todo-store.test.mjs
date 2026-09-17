// Behaviour checks for `todo.svelte.ts` itself: `refresh`, `patch` and
// `remove`, none of which any existing test loads -- `tasklist.test.mjs`
// stops at the pure ordering and drop-planning functions the store is built
// from, never the runes class itself. Written before Phase 4 moves this
// store onto the shared `store/` helpers, on the same theory as
// `library-store.test.mjs`: pass on the unmigrated file first, so the
// migration commit is judged against it.
//
// Driven through the real mock backend, the same way `editor.test.mjs` is.

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
  modules: [todoModule, stateModule],
  close,
} = await load(['/src/lib/todo.svelte.ts', '/src/lib/state.svelte.ts'], { svelte: true })
const { todo } = todoModule
const { app } = stateModule

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')

// ── start / refresh ──────────────────────────────────────────────────

await todo.start()
assert.ok(todo.tasks.length > 0, 'the mock vault ships with tasks')
assert.equal(todo.loading, false, 'loading settles back to false once the load lands')

// A selection that has scrolled out of scope is dropped by refresh, not left
// pointing at a row the list no longer shows.
const other = await todo.add('a task nothing else will match')
assert.ok(other, 'the capture line should be able to add a task')
todo.selectedTask = other.id
await todo.setScope({ kind: 'inbox' })
if (!todo.tasks.some((t) => t.id === other.id)) {
  assert.equal(todo.selectedTask, null, 'a selection outside the new scope is cleared')
}
await todo.setScope({ kind: 'all' })

// ── patch ────────────────────────────────────────────────────────────

{
  const task = todo.tasks[0]
  const before = task.updatedAt
  await new Promise((r) => setTimeout(r, 5))
  todo.patch(task.id, { title: 'characterised' })
  assert.equal(task.title, 'characterised', 'patch assigns the changes onto the task in place')
  assert.notEqual(task.updatedAt, before, 'patch stamps a fresh updatedAt')
  assert.ok(
    todo.tasks.find((t) => t.id === task.id) === task,
    'the same object is still in the list',
  )
  await todo.flush()
}

// Patching an id that is not loaded does nothing.
todo.patch('not-a-real-id', { title: 'ignored' })
await todo.flush()

// ── remove, with a subtree ──────────────────────────────────────────────

{
  const parent = await todo.add('a parent task')
  assert.ok(parent)
  const child = await todo.add('a subtask', { parentId: parent.id })
  assert.ok(child)
  assert.ok(
    todo.tasks.some((t) => t.id === child.id),
    'the subtask is loaded',
  )

  todo.selectedTask = child.id
  const before = todo.tasks.length
  await todo.remove(parent.id)

  assert.ok(!todo.tasks.some((t) => t.id === parent.id), 'the parent is gone')
  assert.ok(!todo.tasks.some((t) => t.id === child.id), 'its subtask went with it')
  assert.equal(todo.tasks.length, before - 2, 'both rows left the list')
  assert.equal(todo.selectedTask, null, 'removing the subtree closes a selection inside it')
}

await close()
console.log('todo-store: all checks passed')

// `app.start()` arms the idle-vault poll; see `editor.test.mjs`'s own note.
process.exit(0)
