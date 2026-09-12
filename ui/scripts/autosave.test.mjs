// Behaviour checks for the debounce-and-write helper the three stores share.
//
// The same reasoning as `notify.test.mjs`: this is a module made of
// decisions, and every one of them is about not losing what somebody typed.
// Its bugs are the quiet kind -- a keystroke that vanishes under a slow disk
// once a fortnight, or a window that closes while the write it was told to
// wait for is still on the wire.
//
// No test framework, deliberately -- one dependency-free file, run by
// `npm run check`. The TypeScript is loaded through Vite so it is compiled
// exactly as the application compiles it.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const {
  modules: [autosaveModule, types],
  close,
} = await load(['/src/lib/autosave.ts', '/src/lib/types.ts'])
const { Autosave, AUTOSAVE_MS } = autosaveModule
const { VaultError } = types

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
/** A write that can be finished by hand, so a flush can be caught mid-air. */
function gate() {
  let release
  const promise = new Promise((r) => (release = r))
  return { promise, release }
}

// ── the basics ────────────────────────────────────────────────────────

{
  const written = []
  const saves = new Autosave((ids) => {
    written.push([...ids].sort())
    return Promise.resolve()
  })
  saves.touch('a')
  saves.touch('b')
  saves.touch('a')
  assert.equal(saves.pending, true)
  await saves.flush()
  assert.deepEqual(written, [['a', 'b']], 'repeated edits collapse into one write')
  assert.equal(saves.pending, false)

  // Nothing dirty is not an error, and not a write either.
  await saves.flush()
  assert.equal(written.length, 1)
}

// ── the debounce actually fires on its own ────────────────────────────

{
  let writes = 0
  const saves = new Autosave(() => {
    writes++
    return Promise.resolve()
  })
  saves.touch('a')
  await sleep(AUTOSAVE_MS + 60)
  assert.equal(writes, 1, 'an untouched edit is written by the timer')
}

// ── a flush waits for a write already in the air ──────────────────────
//
// This is the one the save-before-close handshake depends on. It flushes all
// four stores and lets the window close when they resolve; a flush that
// returned early while a write was still on the wire reported "everything is
// on disk" about a write that had not landed.

{
  const g = gate()
  let landed = false
  const saves = new Autosave(async () => {
    await g.promise
    landed = true
  })
  saves.touch('a')
  const first = saves.flush()
  // The dirty set is empty now -- it was taken before the await -- but the
  // write it was taken for has not finished.
  const second = saves.flush()
  let secondSettled = false
  void second.then(() => (secondSettled = true))
  await sleep(10)
  assert.equal(secondSettled, false, 'a second flush must not resolve past a write in flight')
  g.release()
  await Promise.all([first, second])
  assert.equal(landed, true)
  assert.equal(secondSettled, true)
}

// ── two writes are never in the air at once ───────────────────────────
//
// Two saves of the same record racing into the backend is how a window ends
// up arguing with itself over which version is newer.

{
  const g1 = gate()
  const g2 = gate()
  const gates = [g1, g2]
  let inFlight = 0
  let overlapped = false
  const order = []
  const saves = new Autosave(async (ids) => {
    inFlight++
    if (inFlight > 1) overlapped = true
    order.push([...ids].sort().join(','))
    await gates.shift().promise
    inFlight--
  })
  saves.touch('a')
  const first = saves.flush()
  await sleep(5) // let the first write actually start
  saves.touch('b')
  const second = saves.flush()
  g1.release()
  await sleep(10)
  g2.release()
  await Promise.all([first, second])
  assert.equal(overlapped, false, 'writes are serialised')
  assert.deepEqual(order, ['a', 'b'], 'and they go in the order they were asked for')
}

// ── a failed write puts its edits back ────────────────────────────────

{
  let attempts = 0
  const saves = new Autosave(async (ids) => {
    attempts++
    if (attempts === 1) throw new Error('disk full')
    assert.deepEqual([...ids], ['a'], 'the retry carries the same edit')
  })
  saves.touch('a')
  await saves.flush()
  assert.equal(saves.pending, true, 'an edit that did not land is still dirty')
  assert.equal(saves.retrying, true)
  await saves.flush()
  assert.equal(attempts, 2)
  assert.equal(saves.pending, false)
  assert.equal(saves.retrying, false)
}

// A rejection must not poison every flush that follows it.
{
  let attempts = 0
  const saves = new Autosave(async () => {
    attempts++
    if (attempts === 1) throw new Error('disk full')
  })
  saves.touch('a')
  await saves.flush()
  saves.touch('b')
  await saves.flush()
  assert.equal(attempts, 2, 'the chain survives a failed write')
  assert.equal(saves.pending, false)
}

// ── a lock is not a failure to retry ──────────────────────────────────

{
  const saves = new Autosave(async () => {
    throw new VaultError('locked', 'the vault is locked')
  })
  saves.touch('a')
  await saves.flush()
  assert.equal(saves.pending, false, 'there is nothing to retry against a locked vault')
  assert.equal(saves.retrying, false)
}

// ── an edit made during a write is not lost with the set that held it ──

{
  const g = gate()
  const written = []
  const saves = new Autosave(async (ids) => {
    written.push([...ids].sort())
    if (written.length === 1) await g.promise
  })
  saves.touch('a')
  const first = saves.flush()
  await sleep(5) // the set has been taken and the write is on the wire
  saves.touch('b')
  g.release()
  await first
  await saves.flush()
  assert.deepEqual(written, [['a'], ['b']], 'the edit made mid-write is written after it')
}

// ── cancel drops everything without writing it ────────────────────────

{
  let writes = 0
  const saves = new Autosave(() => {
    writes++
    return Promise.resolve()
  })
  saves.touch('a')
  saves.cancel()
  await saves.flush()
  await sleep(AUTOSAVE_MS + 60)
  assert.equal(writes, 0, 'a lock must not write what it just dropped')
}

// `forget` is the same for one record: it has been deleted, so there is
// nothing to write it to.
{
  const written = []
  const saves = new Autosave((ids) => {
    written.push([...ids].sort())
    return Promise.resolve()
  })
  saves.touch('a')
  saves.touch('b')
  saves.forget('a')
  await saves.flush()
  assert.deepEqual(written, [['b']])
}

await close()
console.log('autosave: all checks passed')
