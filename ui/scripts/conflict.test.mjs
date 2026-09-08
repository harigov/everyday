// Drives the conflict contract through the mock backend, the same way the
// interface does: create, save from the version we hold, then save again
// from a version something else has superseded.
import assert from 'node:assert/strict'

const entries = []
class VaultError extends Error {
  constructor(code, message) {
    super(message)
    this.code = code
  }
}

// The mock's save_entry logic, lifted verbatim in shape.
function saveEntry(e, expect) {
  const stored = entries.find((x) => x.id === e.id)
  if (stored ? stored.updatedAt !== expect : expect !== null) {
    throw new VaultError('conflict', 'entry was changed elsewhere since you loaded it')
  }
  const i = entries.findIndex((x) => x.id === e.id)
  if (i >= 0) entries[i] = structuredClone(e)
  else entries.unshift(structuredClone(e))
}
function saveEntryForce(e) {
  const i = entries.findIndex((x) => x.id === e.id)
  if (i >= 0) entries[i] = structuredClone(e)
  else entries.unshift(structuredClone(e))
}

const id = 'e1'
// Create.
saveEntry({ id, body: 'v1', updatedAt: 't1' }, null)
assert.equal(entries.length, 1)

// Creating the same id again is a conflict, not an overwrite.
assert.throws(() => saveEntry({ id, body: 'clash', updatedAt: 't1b' }, null), { code: 'conflict' })

// This window holds t1. Something else writes t2.
saveEntry({ id, body: 'theirs', updatedAt: 't2' }, 't1')

// Our autosave still believes it holds t1.
assert.throws(() => saveEntry({ id, body: 'mine', updatedAt: 't3' }, 't1'), { code: 'conflict' })
assert.equal(entries[0].body, 'theirs', 'a refused save must change nothing')

// "Keep mine" is the deliberate override.
saveEntryForce({ id, body: 'mine', updatedAt: 't3' })
assert.equal(entries[0].body, 'mine')

// And a save from the now-current version lands normally again.
saveEntry({ id, body: 'mine again', updatedAt: 't4' }, 't3')
assert.equal(entries[0].body, 'mine again')

console.log('conflict: all checks passed')
