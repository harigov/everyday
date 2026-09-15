// Behaviour checks for the Mail app's pure logic -- see `lib/mail.ts`.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

// `threadListDate` goes through `format.ts`, which reads `navigator.language`
// to resolve a locale -- see `format.test.mjs` for the fuller story.
stubBrowser({ window: globalThis, location: new URL('http://localhost/'), navigator: true })

const { module: mailLib, close } = await load('/src/lib/mail.ts')
const {
  formatSenders,
  threadListDate,
  replyAllRecipients,
  applyRowPatch,
  revertRow,
  removeRow,
  restoreRow,
  snoozeChoices,
} = mailLib

function person(name, email) {
  return { name, email }
}

// ── Sender-list formatting ────────────────────────────────────────────

assert.equal(formatSenders([]), '')
assert.equal(formatSenders([person('Priya Raman', 'priya@example.com')]), 'Priya Raman')
assert.equal(
  formatSenders([
    person('Priya Raman', 'priya@example.com'),
    person('Tom Fenwick', 't@example.com'),
  ]),
  'Priya Raman, Tom Fenwick',
)
assert.equal(
  formatSenders([
    person('Priya Raman', 'priya@example.com'),
    person('Tom Fenwick', 't@example.com'),
    person('Ana Ferreira', 'ana@example.com'),
    person('Ben Carrow', 'ben@example.com'),
  ]),
  'Priya Raman, Tom Fenwick +2',
  'past the limit, the rest collapse to a count',
)
assert.equal(
  formatSenders([person('', 'noreply@example.com')]),
  'noreply@example.com',
  'a participant with no name falls back to the address',
)

// ── Date formatting for the list ──────────────────────────────────────

const today = new Date()
today.setHours(9, 30, 0, 0)
assert.match(
  threadListDate(today.toISOString()),
  /\d/,
  'something that arrived today prints as a time, not a date',
)

const yesterday = new Date()
yesterday.setDate(yesterday.getDate() - 1)
assert.equal(threadListDate(yesterday.toISOString()), 'Yesterday')

// ── Reply-all recipient computation ───────────────────────────────────

const message = {
  from: person('Priya Raman', 'priya@example.com'),
  to: [person('Me', 'me@example.com'), person('Tom Fenwick', 'tom@example.com')],
  cc: [person('Ana Ferreira', 'ana@example.com')],
}
const { to, cc } = replyAllRecipients(message, 'me@example.com')
assert.deepEqual(
  to.map((a) => a.email),
  ['priya@example.com', 'tom@example.com'],
  'the sender comes first, then the other original recipients, minus me',
)
assert.deepEqual(
  cc.map((a) => a.email),
  ['ana@example.com'],
  'the original Cc stays Cc',
)

// The account's own address never comes back, wherever it was.
const selfInCc = replyAllRecipients(
  {
    from: person('Priya Raman', 'priya@example.com'),
    to: [person('Me', 'me@example.com')],
    cc: [person('Me', 'me@example.com'), person('Ana Ferreira', 'ana@example.com')],
  },
  'me@example.com',
)
assert.equal(
  selfInCc.to.some((a) => a.email === 'me@example.com'),
  false,
)
assert.equal(
  selfInCc.cc.some((a) => a.email === 'me@example.com'),
  false,
)

// Somebody in both To and Cc on the original is not repeated in the reply.
const dup = replyAllRecipients(
  {
    from: person('Priya Raman', 'priya@example.com'),
    to: [person('Me', 'me@example.com'), person('Tom Fenwick', 'tom@example.com')],
    cc: [person('Tom Fenwick', 'tom@example.com')],
  },
  'me@example.com',
)
assert.equal(dup.to.filter((a) => a.email === 'tom@example.com').length, 1)
assert.equal(dup.cc.filter((a) => a.email === 'tom@example.com').length, 0)

// ── Optimistic apply and revert of row state ──────────────────────────

const rows = [
  { id: 'a', unreadCount: 1 },
  { id: 'b', unreadCount: 0 },
]
const patched = applyRowPatch(rows, 'a', { unreadCount: 0 })
assert.equal(patched.rows.find((r) => r.id === 'a').unreadCount, 0)
assert.equal(patched.before.unreadCount, 1, 'the row as it was is handed back for a revert')
assert.deepEqual(rows[0], { id: 'a', unreadCount: 1 }, 'the original array is untouched')

const reverted = revertRow(patched.rows, 'a', patched.before)
assert.deepEqual(reverted, rows, 'reverting restores exactly what was there before the patch')

const removed = removeRow(rows, 'a')
assert.equal(removed.rows.length, 1)
assert.equal(removed.rows[0].id, 'b')
const restored = restoreRow(removed.rows, removed.removed)
assert.deepEqual(
  restored.map((r) => r.id),
  ['a', 'b'],
  'restoring puts the row back at its original index',
)

// ── The snooze picker's times ─────────────────────────────────────────

const morning = new Date('2026-09-14T09:00:00')
const choices = snoozeChoices(morning)
assert.deepEqual(
  choices.map((c) => c.key),
  ['laterToday', 'tomorrow', 'nextWeek'],
)
assert.equal(choices[0].at.getHours(), 12, '"later today" is a few hours from now')
assert.equal(choices[1].at.getDate(), morning.getDate() + 1)
assert.equal(choices[1].at.getHours(), 8, '"tomorrow" lands at a sane morning hour')
assert.equal(Math.round((choices[2].at - morning) / 86_400_000), 7, '"next week" is seven days out')

// Late in the evening, "later today" would run past a sane hour and drops out.
const evening = new Date('2026-09-14T20:00:00')
const eveningChoices = snoozeChoices(evening)
assert.deepEqual(
  eveningChoices.map((c) => c.key),
  ['tomorrow', 'nextWeek'],
  'no "later today" once it would fall after 9pm',
)

await close()
console.log('mail: all checks passed')
