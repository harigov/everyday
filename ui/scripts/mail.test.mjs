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
  CATEGORY_TABS,
  stepCategoryTab,
  isCurrentInviteResponse,
  inviteIsCancelled,
  applyInviteResponse,
  mergeSearchPage,
  remoteImagesAllowed,
  originPhrase,
  recentActionLine,
  mailboxHasTabs,
  refreshLimit,
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

// ── (p) The split inbox: category tab ordering ─────────────────────────

assert.deepEqual(
  CATEGORY_TABS.map((t) => t.key),
  ['important', 'other', 'newsletter', 'notification'],
  'mirrors Category::ALL in everyday_core::mail',
)

assert.equal(stepCategoryTab(null, 1), 'important', 'All -> the first named tab')
assert.equal(
  stepCategoryTab('notification', 1),
  null,
  'stepping past the last tab wraps back to All',
)
assert.equal(stepCategoryTab(null, -1), 'notification', 'stepping back from All wraps to the last')
assert.equal(stepCategoryTab('other', -1), 'important')

// ── (i) Invitations: response state ─────────────────────────────────────

function invite(overrides = {}) {
  return {
    uid: 'invite-1',
    method: 'request',
    summary: 'Design review',
    start: '2026-09-15T14:00:00Z',
    end: '2026-09-15T14:45:00Z',
    allDay: false,
    location: null,
    organizer: person('Priya Raman', 'priya@example.com'),
    attendees: [],
    myResponse: null,
    recurrence: null,
    ...overrides,
  }
}

assert.equal(isCurrentInviteResponse(invite({ myResponse: 'accepted' }), 'accepted'), true)
assert.equal(isCurrentInviteResponse(invite({ myResponse: 'accepted' }), 'declined'), false)
assert.equal(
  isCurrentInviteResponse(invite(), 'accepted'),
  false,
  'no response yet highlights nothing',
)

assert.equal(inviteIsCancelled(invite({ method: 'cancel' })), true)
assert.equal(inviteIsCancelled(invite({ method: 'request' })), false)

const originalInvite = invite()
const patchedInvite = applyInviteResponse(originalInvite, 'tentative')
assert.equal(patchedInvite.myResponse, 'tentative')
assert.equal(originalInvite.myResponse, null, 'the source invite is never mutated')

// ── Search: keyset paging merge ─────────────────────────────────────────

function thread(id, lastDate = '2026-09-01T00:00:00Z') {
  return {
    id,
    accountId: 'acct-google',
    subject: id,
    participants: [],
    lastDate,
    messageCount: 1,
    unreadCount: 0,
  }
}

const firstPage = [thread('th-1'), thread('th-2')]
const secondPage = [thread('th-2'), thread('th-3')] // the index repeated one near the boundary
const merged = mergeSearchPage(firstPage, secondPage)
assert.deepEqual(
  merged.map((t) => t.id),
  ['th-1', 'th-2', 'th-3'],
  'a thread a page boundary repeats is not duplicated',
)
assert.equal(firstPage.length, 2, 'the first page is not mutated')

// ── Remote images: allow-list matching ──────────────────────────────────

const settings = { senders: ['newsletter@economist.example.com'], domains: ['github.com'] }
assert.equal(remoteImagesAllowed(settings, 'newsletter@economist.example.com'), true)
assert.equal(
  remoteImagesAllowed(settings, 'Newsletter@Economist.example.com'),
  true,
  'matching is case-insensitive',
)
assert.equal(remoteImagesAllowed(settings, 'notifications@github.com'), true, 'a domain match')
assert.equal(remoteImagesAllowed(settings, 'someone@figma.com'), false)
assert.equal(remoteImagesAllowed(settings, 'not-an-address'), false, 'no domain to match against')

// ── Marks from origin ────────────────────────────────────────────────

assert.equal(originPhrase({ type: 'person' }), null, 'a person needs no mark')
assert.equal(originPhrase({ type: 'assistant', conversation: 'c' }), 'the assistant')
assert.equal(originPhrase({ type: 'mcp', client: 'Claude Desktop' }), 'an MCP client')
assert.equal(originPhrase({ type: 'routine', run: 'r' }), 'a routine')

assert.equal(
  recentActionLine({
    kind: { type: 'archive' },
    origin: { type: 'assistant', conversation: 'c' },
    at: '2026-09-01T00:00:00Z',
    state: { type: 'done' },
  }),
  'Archived by the assistant',
)
assert.equal(
  recentActionLine({
    kind: { type: 'move', to: 'mb-1' },
    origin: { type: 'mcp', client: 'Claude Desktop' },
    at: '2026-09-01T00:00:00Z',
    state: { type: 'done' },
  }),
  'Moved by an MCP client',
)
assert.equal(
  recentActionLine({
    kind: { type: 'archive' },
    origin: { type: 'assistant', conversation: 'c' },
    at: '2026-09-01T00:00:00Z',
    state: { type: 'failed', permanent: true, message: 'over quota' },
  }),
  "Couldn't archive it: over quota",
)
assert.equal(
  recentActionLine({
    kind: { type: 'archive' },
    origin: { type: 'person' },
    at: '2026-09-01T00:00:00Z',
    state: { type: 'done' },
  }),
  null,
  "a person's own successful action needs no line",
)

// ── Finding 1: the category filter belongs to the inbox alone ───────────

assert.equal(mailboxHasTabs({ role: 'inbox' }), true)
assert.equal(mailboxHasTabs({ role: 'sent' }), false)
assert.equal(mailboxHasTabs({ role: 'archive' }), false)
assert.equal(mailboxHasTabs(null), false, 'no mailbox selected yet: no tabs to have set from')
assert.equal(mailboxHasTabs(undefined), false)

// ── Finding 2: refresh covers what is already loaded ────────────────────

assert.equal(refreshLimit(0, 50), 50, 'never less than one page')
assert.equal(refreshLimit(30, 50), 50, 'never less than one page')
assert.equal(refreshLimit(120, 50), 120, 'covers everything already scrolled past')
assert.equal(refreshLimit(50000, 50), 1000, 'capped well short of the whole mailbox')

await close()
console.log('mail: all checks passed')
