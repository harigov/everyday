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
  modules: [todoModule, stateModule, apiModule],
  close,
} = await load(['/src/lib/todo.svelte.ts', '/src/lib/state.svelte.ts', '/src/lib/api.ts'], {
  svelte: true,
})
const { todo } = todoModule
const { app } = stateModule
const { api } = apiModule

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

// ── add: the row first, the save behind it ────────────────────────────
//
// A new task is drawn as soon as it has an id, before its save lands, and
// nothing else that writes about it may reach the vault ahead of that save.

{
  const realSave = api.saveTask
  const realSaveMany = api.saveTasks
  const order = []
  let land = null
  let refuse = null
  api.saveTask = (task) => {
    order.push(`create ${task.title}`)
    return new Promise((resolve, reject) => {
      land = () => realSave(task).then(resolve, reject)
      refuse = () => reject(new Error('the disk is full'))
    })
  }
  api.saveTasks = (tasks) => {
    order.push(`edit ${tasks.map((t) => t.title).join(', ')}`)
    return realSaveMany(tasks)
  }
  const until = async (what, condition) => {
    for (let i = 0; i < 100 && !condition(); i++) await new Promise((r) => setTimeout(r, 10))
    assert.ok(condition(), what)
  }

  // Saved: on screen while the save is in the air, and an edit made then
  // is written after it.
  const adding = todo.add('drawn before it is saved')
  await until('the row appears before its save lands', () =>
    todo.tasks.some((t) => t.title === 'drawn before it is saved'),
  )
  const drawn = todo.tasks.find((t) => t.title === 'drawn before it is saved')
  assert.deepEqual(order, ['create drawn before it is saved'])
  todo.patch(drawn.id, { title: 'edited while saving' })
  const flushed = todo.flush()
  await new Promise((r) => setTimeout(r, 60))
  assert.deepEqual(order, ['create drawn before it is saved'], 'the edit waits for the create')
  land()
  const added = await adding
  await flushed
  assert.equal(added?.id, drawn.id, 'add answers the task once its save has landed')
  assert.deepEqual(order, ['create drawn before it is saved', 'edit edited while saving'])
  assert.ok(
    todo.tasks.some((t) => t.id === drawn.id),
    'and it stays',
  )

  // Refused: the row comes off again and add answers nothing.
  order.length = 0
  const failing = todo.add('never saved')
  await until('the row appears', () => todo.tasks.some((t) => t.title === 'never saved'))
  const doomed = todo.tasks.find((t) => t.title === 'never saved')
  todo.patch(doomed.id, { title: 'never saved, edited' })
  const flushedToo = todo.flush()
  refuse()
  assert.equal(await failing, null, 'a refused save answers nothing')
  await flushedToo
  assert.ok(!todo.tasks.some((t) => t.id === doomed.id), 'and its row comes off')
  assert.deepEqual(order, ['create never saved'], 'nor is the edit written behind it')
  app.error = null

  // Deleted while its save is in the air: the delete goes after the create,
  // and not at all if the create was refused -- there is nothing to delete.
  const realDelete = api.deleteTask
  api.deleteTask = (id) => (order.push(`delete ${id}`), realDelete(id))
  order.length = 0
  const kept = todo.add('deleted while saving')
  await until('the row appears', () => todo.tasks.some((t) => t.title === 'deleted while saving'))
  const gone = todo.tasks.find((t) => t.title === 'deleted while saving')
  const removing = todo.remove(gone.id)
  await new Promise((r) => setTimeout(r, 60))
  assert.deepEqual(order, ['create deleted while saving'], 'the delete waits for the create')
  land()
  await kept
  await removing
  assert.deepEqual(order, ['create deleted while saving', `delete ${gone.id}`])
  assert.ok(!todo.tasks.some((t) => t.id === gone.id))

  order.length = 0
  const refusedToo = todo.add('deleted, never saved')
  await until('the row appears', () => todo.tasks.some((t) => t.title === 'deleted, never saved'))
  const never = todo.tasks.find((t) => t.title === 'deleted, never saved')
  const removingToo = todo.remove(never.id)
  refuse()
  assert.equal(await refusedToo, null)
  await removingToo
  assert.deepEqual(order, ['create deleted, never saved'], 'no delete for a task never written')
  app.error = null

  api.saveTask = realSave
  api.saveTasks = realSaveMany
  api.deleteTask = realDelete
}

await close()
console.log('todo-store: all checks passed')

// `app.start()` arms the idle-vault poll; see `editor.test.mjs`'s own note.
process.exit(0)
