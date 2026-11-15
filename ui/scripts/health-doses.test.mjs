// `lib/health-doses.ts`: what was due, counted here rather than asked of the
// vault, so a list of medicines does not make a read each.
//
// The count has to agree with the vault's own (`schedule::due_times`): a
// medication paused for a week that went on counting would show its
// steadiness falling towards nothing beside an adherence the vault says is
// whole.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: d, close } = await load('/src/lib/health-doses.ts')
const { expectedDoses, lastDueDay, steadiness } = d

/** A twice-daily medication, changed at noon UTC on `changed`. */
function med(extra = {}) {
  return {
    id: 'm1',
    name: 'Metformin',
    schedule: { type: 'daily', times: ['08:00:00', '20:00:00'] },
    status: 'active',
    started: '2026-11-01',
    updatedAt: '2026-11-10T12:00:00Z',
    ...extra,
  }
}

// Running: every day from its start, twice.
assert.equal(lastDueDay(med()), null)
assert.equal(expectedDoses(med(), '2026-11-01', '2026-11-14'), 28)
assert.equal(expectedDoses(med(), '2026-10-28', '2026-11-02'), 4, 'nothing before it started')

// Stopped with a date: nothing after that day.
const ended = med({ status: 'stopped', stopped: '2026-11-05' })
assert.equal(lastDueDay(ended), '2026-11-05')
assert.equal(expectedDoses(ended, '2026-11-01', '2026-11-14'), 10)

// Stopped without one: the day it was stopped is the last.
const dropped = med({ status: 'stopped' })
assert.equal(lastDueDay(dropped), '2026-11-10')
assert.equal(expectedDoses(dropped, '2026-11-01', '2026-11-14'), 20)

// Paused: up to the day before the pause, so a week paused counts nothing.
const paused = med({ status: 'paused' })
assert.equal(lastDueDay(paused), '2026-11-09')
assert.equal(expectedDoses(paused, '2026-11-01', '2026-11-14'), 18)
assert.equal(expectedDoses(paused, '2026-11-10', '2026-11-14'), 0)
// ...or its own stop, if that came first.
assert.equal(lastDueDay(med({ status: 'paused', stopped: '2026-11-03' })), '2026-11-03')

// An active medication with a stop ahead runs to it.
assert.equal(lastDueDay(med({ stopped: '2026-11-20' })), '2026-11-20')

// What was taken is held against what was due, and a pause asks nothing.
const taken = [
  { medicationId: 'm1', at: '2026-11-11T08:05:00Z', status: 'taken' },
  { medicationId: 'm1', at: '2026-11-12T08:05:00Z', status: 'taken' },
]
const still = steadiness(paused, taken, '2026-11-10', '2026-11-14')
assert.equal(still.expected, 0)
assert.equal(still.share, null, 'nothing expected is not a share of nothing')

await close()
console.log('health-doses: all checks passed')
