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
  weekdayAndTime,
  snoozedUntilLabel,
  scheduledSendLabel,
  applyRowPatch,
  revertRow,
  removeRow,
  restoreRow,
  snoozeChoices,
  customSnoozeInstant,
  earliestSnoozeDate,
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
  isSnoozedMailbox,
  needsSyntheticSnoozedMailbox,
  listTarget,
  UNIFIED_PREFIX,
  unifiedMailboxId,
  unifiedMailboxes,
  isUnifiedMailbox,
  syncInProgress,
  newestSyncedAt,
  messageDateLabel,
  sumCategoryCounts,
  gmailSystemLabel,
  mailboxDisplayName,
  promoteGmailInbox,
  dateSection,
  withDateSections,
  refreshLimit,
  visibleThreadList,
  neighbourThread,
  isBlankDraft,
  threadRange,
  categoryTabCount,
  parseTypedAddress,
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

// `weekdayAndTime` and the two labels built from it -- always a weekday and
// a time together, unlike `threadListDate`'s either/or, because neither a
// snooze nor a send-later is ever due back within today's own 24 hours.
assert.match(
  weekdayAndTime('2026-09-15T08:00:00'),
  /^\w{3} \d/,
  'a short weekday, then a time -- "Tue 8:00" or similar, locale aside',
)
assert.match(snoozedUntilLabel('2026-09-15T08:00:00'), /^Until /)
assert.match(scheduledSendLabel(new Date(Date.now() + 3 * 3_600_000).toISOString()), /^Sends .*\(/)

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
//
// 2026-09-14 is a Monday (Zeller's congruence, not the clock this test runs
// on) -- the fixed point every weekday-dependent assertion below works from.
// 2026-09-18 is the Friday of that week, 2026-09-19 its Saturday, and
// 2026-09-21 the following Monday.

const monday = new Date('2026-09-14T09:00:00')
const mondayChoices = snoozeChoices(monday)
assert.deepEqual(
  mondayChoices.map((c) => c.key),
  ['laterToday', 'thisEvening', 'tomorrow', 'thisWeekend', 'nextWeek'],
  'every choice applies on a Monday morning',
)
assert.equal(mondayChoices[0].at.getHours(), 12, '"later today" is three hours out')
assert.equal(mondayChoices[1].at.getHours(), 18, '"this evening" is a fixed 6pm')
assert.equal(mondayChoices[2].at.getDate(), monday.getDate() + 1, '"tomorrow" is the next day')
assert.equal(mondayChoices[2].at.getHours(), 8)
assert.equal(mondayChoices[3].at.getDate(), 19, '"this weekend" is Saturday the 19th')
assert.equal(mondayChoices[3].at.getHours(), 9)
assert.equal(
  mondayChoices[4].at.getDate(),
  21,
  '"next week" is Monday the 21st, not "+7 days" blindly',
)
assert.equal(mondayChoices[4].at.getHours(), 8)

// Past 6pm, "later today" (+3h) would spill into tomorrow -- gone. Past 5pm,
// "this evening" (a fixed 6pm) is too close to "later today" to be worth
// offering twice -- also gone, an hour before "later today" itself is.
assert.deepEqual(
  snoozeChoices(new Date('2026-09-14T20:00:00')).map((c) => c.key),
  ['tomorrow', 'thisWeekend', 'nextWeek'],
  'past 6pm, neither "later today" nor "this evening" still applies',
)
assert.deepEqual(
  snoozeChoices(new Date('2026-09-14T17:00:00')).map((c) => c.key),
  ['laterToday', 'tomorrow', 'thisWeekend', 'nextWeek'],
  '"this evening" drops right at 5pm; "later today" still has an hour left',
)

// Friday and Saturday: the weekend is already here, so "this weekend" stops
// being offered -- "Next week" is still the following Monday either way,
// not "whichever Monday is seven days out".
const friday = new Date('2026-09-18T10:00:00')
assert.deepEqual(
  snoozeChoices(friday).map((c) => c.key),
  ['laterToday', 'thisEvening', 'tomorrow', 'nextWeek'],
  'no "this weekend" on a Friday',
)
assert.equal(
  snoozeChoices(friday)
    .find((c) => c.key === 'nextWeek')
    .at.getDate(),
  21,
)

const saturday = new Date('2026-09-19T10:00:00')
assert.deepEqual(
  snoozeChoices(saturday).map((c) => c.key),
  ['laterToday', 'thisEvening', 'tomorrow', 'nextWeek'],
  'no "this weekend" on a Saturday either',
)
assert.equal(
  snoozeChoices(saturday)
    .find((c) => c.key === 'nextWeek')
    .at.getDate(),
  21,
)

// ── The custom snooze date (and now time), parsed as local rather than UTC ─
//
// `new Date('2026-09-20')` is UTC midnight; `.setHours(...)` on that then
// reads back in local time, which west of Greenwich lands on the 19th, not
// the 20th. `customSnoozeInstant` must land on the day the field shows
// regardless of which side of Greenwich this test runs on.

const picked = customSnoozeInstant('2026-09-20')
assert.equal(picked.getFullYear(), 2026)
assert.equal(picked.getMonth(), 8, 'September, zero-indexed')
assert.equal(picked.getDate(), 20, 'the calendar day the field showed, not one either side of it')
assert.equal(picked.getHours(), 8, 'defaults to 8am when no time is given')
assert.equal(picked.getMinutes(), 0)

const pickedWithTime = customSnoozeInstant('2026-09-20', '14:30')
assert.equal(pickedWithTime.getDate(), 20, 'the time field never moves the day')
assert.equal(pickedWithTime.getHours(), 14)
assert.equal(pickedWithTime.getMinutes(), 30)

// `earliestSnoozeDate` never offers today: today's 8am may already be
// behind `now`, and `snoozeChoices`'s own "Later today" already covers that
// case.
assert.equal(earliestSnoozeDate(new Date('2026-09-14T09:00:00')), '2026-09-15')
assert.equal(
  earliestSnoozeDate(new Date('2026-09-14T23:59:00')),
  '2026-09-15',
  'still tomorrow, not the day after, this close to midnight',
)

// ── (p) The split inbox: category tab ordering ─────────────────────────

assert.deepEqual(
  CATEGORY_TABS.map((t) => t.key),
  ['priority', 'important', 'other', 'newsletter', 'notification'],
  'Priority leads, ahead of Important -- mirrors Category::ALL in everyday_core::mail',
)

assert.equal(stepCategoryTab(null, 1), 'priority', 'All -> the first named tab')
assert.equal(
  stepCategoryTab('notification', 1),
  null,
  'stepping past the last tab wraps back to All',
)
assert.equal(stepCategoryTab(null, -1), 'notification', 'stepping back from All wraps to the last')
assert.equal(stepCategoryTab('important', -1), 'priority')
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

// ── Bug 3: telling the backend to hide (or show) snoozed threads ────────

assert.equal(isSnoozedMailbox({ remoteName: 'Snoozed', pseudo: 'snoozed' }), true)
assert.equal(isSnoozedMailbox({ remoteName: 'Inbox' }), false)
assert.equal(
  isSnoozedMailbox({ remoteName: 'Starred', pseudo: 'starred' }),
  false,
  'a different pseudo-mailbox',
)
// A real server folder that happens to share the name (Spark makes one) is
// a folder: asking for only its snoozed threads would show it empty.
assert.equal(isSnoozedMailbox({ remoteName: 'Snoozed' }), false, 'a real folder named Snoozed')
assert.equal(isSnoozedMailbox(null), false, 'no mailbox selected yet')
assert.equal(isSnoozedMailbox(undefined), false)

// ── The Snoozed view, for an account with no Snoozed mailbox of its own ──

assert.equal(
  needsSyntheticSnoozedMailbox([
    { role: 'inbox', pseudo: undefined },
    { role: 'sent', pseudo: undefined },
  ]),
  true,
  'an Inbox and no Snoozed of its own -- a real backend, today',
)
assert.equal(
  needsSyntheticSnoozedMailbox([
    { role: 'inbox', pseudo: undefined },
    { role: 'other', pseudo: 'snoozed' },
  ]),
  false,
  'already has one -- the mock, for its own two accounts',
)
assert.equal(
  needsSyntheticSnoozedMailbox([{ role: 'sent', pseudo: undefined }]),
  false,
  'no Inbox at all yet -- nothing to stand the Snoozed view in for',
)

const INBOX = { id: 'mb-inbox', accountId: 'a1', role: 'inbox' }
const SENT = { id: 'mb-sent', accountId: 'a1', role: 'sent' }
const REAL_SNOOZED = { id: 'mb-snoozed', accountId: 'a1', pseudo: 'snoozed' }
const accountMailboxes = [INBOX, SENT, REAL_SNOOZED]

assert.deepEqual(
  listTarget(INBOX, accountMailboxes),
  { mailboxIds: ['mb-inbox'], snoozed: false },
  'an ordinary mailbox lists under its own id',
)
assert.deepEqual(
  listTarget(REAL_SNOOZED, accountMailboxes),
  { mailboxIds: ['mb-snoozed'], snoozed: true },
  "a real Snoozed pseudo-mailbox -- the mock's own -- lists under its own id too",
)
assert.deepEqual(
  listTarget({ id: 'synthetic-snoozed:a1', accountId: 'a1', pseudo: 'snoozed' }, accountMailboxes),
  { mailboxIds: ['mb-inbox'], snoozed: true },
  "the synthetic stand-in redirects to the account's own Inbox, filtered",
)
assert.equal(listTarget(null, accountMailboxes), null, 'nothing selected: nothing to list')
assert.equal(listTarget(undefined, accountMailboxes), null)

// ── All accounts at once: the unified rows ──────────────────────────────

function box(id, accountId, role, pseudo) {
  return {
    id,
    accountId,
    remoteName: id,
    role,
    pseudo,
    uidvalidity: 1,
    uidnext: 1,
    highestModseq: 1,
  }
}

// Account a1 has every role; a2 has no Spam and no Archive -- and both have
// a Snoozed row of their own (the mock's, and a synthetic stand-in), which
// must never be mistaken for a mailbox to gather.
const A1 = [
  box('a1-inbox', 'a1', 'inbox'),
  box('a1-drafts', 'a1', 'drafts'),
  box('a1-sent', 'a1', 'sent'),
  box('a1-archive', 'a1', 'archive'),
  box('a1-spam', 'a1', 'spam'),
  box('a1-trash', 'a1', 'trash'),
  box('a1-receipts', 'a1', 'other'),
  box('a1-snoozed', 'a1', 'other', 'snoozed'),
]
const A2 = [
  box('a2-inbox', 'a2', 'inbox'),
  box('a2-drafts', 'a2', 'drafts'),
  box('a2-sent', 'a2', 'sent'),
  box('a2-trash', 'a2', 'trash'),
  box('synthetic-snoozed:a2', 'a2', 'other', 'snoozed'),
]
const BOTH = [...A1, ...A2]

assert.deepEqual(
  unifiedMailboxes(A1, ['a1']),
  [],
  'one account: a unified Inbox would only be its Inbox drawn twice',
)
assert.deepEqual(
  unifiedMailboxes(A1, ['a1', 'a2']),
  [],
  "two accounts, but only one with any mailboxes loaded yet -- still one account's worth",
)
assert.deepEqual(
  unifiedMailboxes([box('a1-starred', 'a1', 'other', 'starred'), ...A2], ['a1', 'a2']),
  [],
  'a pseudo-mailbox alone does not count as an account having mailboxes',
)

{
  const rows = unifiedMailboxes(BOTH, ['a1', 'a2'])
  assert.deepEqual(
    rows.map((r) => r.id),
    [
      unifiedMailboxId('inbox'),
      unifiedMailboxId('snoozed'),
      unifiedMailboxId('drafts'),
      unifiedMailboxId('sent'),
      unifiedMailboxId('archive'),
      unifiedMailboxId('spam'),
      unifiedMailboxId('trash'),
    ],
    'every kind at least one account has, in the nav order',
  )
  assert.ok(
    rows.every((r) => r.id.startsWith(UNIFIED_PREFIX)),
    'every id carries the prefix',
  )
  assert.ok(
    rows.every((r) => r.pseudo === 'unified' && r.accountId === ''),
    'pseudo rows in no account',
  )
  assert.ok(rows.every(isUnifiedMailbox))
  const roleOf = (kind) => rows.find((r) => r.id === unifiedMailboxId(kind)).role
  assert.equal(roleOf('inbox'), 'inbox', 'the unified Inbox is an inbox -- it keeps the tabs')
  assert.equal(roleOf('sent'), 'sent')
  assert.equal(roleOf('trash'), 'trash')
  assert.equal(
    roleOf('snoozed'),
    'other',
    "Snoozed is not an inbox to anything that reads role -- no tabs, as a single account's",
  )
  assert.equal(mailboxHasTabs(rows.find((r) => r.id === unifiedMailboxId('inbox'))), true)
  assert.equal(mailboxHasTabs(rows.find((r) => r.id === unifiedMailboxId('snoozed'))), false)
  assert.equal(
    mailboxDisplayName(rows.find((r) => r.id === unifiedMailboxId('inbox'))),
    'All inboxes',
  )
}

{
  // Neither account has Spam: no unified Spam row either.
  const noSpam = BOTH.filter((m) => m.role !== 'spam')
  const ids = unifiedMailboxes(noSpam, ['a1', 'a2']).map((r) => r.id)
  assert.ok(!ids.includes(unifiedMailboxId('spam')), 'a kind nobody has is left out')
  assert.ok(ids.includes(unifiedMailboxId('archive')), 'a kind only one account has is kept')
}

assert.equal(
  unifiedMailboxes(BOTH, ['a1']).length,
  0,
  'only the accounts asked about count -- an account filtered out is not a second account',
)

{
  const rows = unifiedMailboxes(BOTH, ['a1', 'a2'])
  const row = (kind) => rows.find((r) => r.id === unifiedMailboxId(kind))
  assert.deepEqual(
    listTarget(row('inbox'), BOTH),
    { mailboxIds: ['a1-inbox', 'a2-inbox'], snoozed: false },
    'the unified Inbox lists every account Inbox, snoozed threads hidden',
  )
  assert.deepEqual(
    listTarget(row('snoozed'), BOTH),
    { mailboxIds: ['a1-inbox', 'a2-inbox'], snoozed: true },
    "the unified Snoozed is every Inbox, filtered -- never the accounts' own Snoozed rows",
  )
  assert.deepEqual(listTarget(row('drafts'), BOTH), {
    mailboxIds: ['a1-drafts', 'a2-drafts'],
    snoozed: false,
  })
  assert.deepEqual(
    listTarget(row('archive'), BOTH),
    { mailboxIds: ['a1-archive'], snoozed: false },
    'a role only one account has lists that one mailbox',
  )
  assert.equal(
    listTarget(
      row('spam'),
      BOTH.filter((m) => m.role !== 'spam'),
    ),
    null,
    'a unified row whose role no account has any longer has nothing to list',
  )
  assert.equal(isSnoozedMailbox(row('snoozed')), true, 'the unified Snoozed is a Snoozed view')
  assert.equal(isSnoozedMailbox(row('inbox')), false)
}

// ── Sync status ─────────────────────────────────────────────────────────

assert.equal(syncInProgress({ phase: 'idle' }), false)
assert.equal(syncInProgress({ phase: 'idling' }), false, 'waiting on IDLE is resting')
assert.equal(syncInProgress({ phase: 'connecting' }), true)
assert.equal(syncInProgress({ phase: 'headers' }), true)
assert.equal(syncInProgress({ phase: 'bodies' }), true)

assert.equal(
  newestSyncedAt(
    ['a1', 'a2'],
    [
      { accountId: 'a1', lastSyncedAt: '2026-10-06T09:00:00Z' },
      { accountId: 'a2', lastSyncedAt: '2026-10-06T09:05:00Z' },
      { accountId: 'a3', lastSyncedAt: '2026-10-06T10:00:00Z' },
    ],
    [],
  ),
  '2026-10-06T09:05:00Z',
  'the newest among the accounts asked about -- never one outside them',
)
assert.equal(
  newestSyncedAt(
    ['a1'],
    [{ accountId: 'a1' }],
    [{ id: 'a1', lastSyncedAt: '2026-10-05T08:00:00Z' }],
  ),
  '2026-10-05T08:00:00Z',
  "no pass this session: the account record's own",
)
assert.equal(newestSyncedAt(['a1'], [], [{ id: 'a1', lastSyncedAt: null }]), null)

// ── Gmail's own system labels, still IMAP-escaped on some rows ──────────

assert.equal(gmailSystemLabel('\\Inbox'), 'inbox')
assert.equal(gmailSystemLabel('\\\\Inbox'), 'inbox', 'doubled escaping reads the same')
assert.equal(gmailSystemLabel('INBOX'), 'inbox', 'no escaping at all, case-insensitive')
assert.equal(gmailSystemLabel('\\Sent'), 'sent')
assert.equal(gmailSystemLabel('\\Draft'), 'drafts')
assert.equal(gmailSystemLabel('\\Drafts'), 'drafts')
assert.equal(gmailSystemLabel('\\Starred'), 'starred')
assert.equal(gmailSystemLabel('\\Important'), 'important')
assert.equal(gmailSystemLabel('\\Spam'), 'spam')
assert.equal(gmailSystemLabel('\\Trash'), 'trash')
assert.equal(gmailSystemLabel('\\All'), 'all')
assert.equal(gmailSystemLabel('Boat club'), null, "a person's own folder")
assert.equal(gmailSystemLabel('\\Boat club'), null, 'escaping alone does not make a system label')

assert.equal(mailboxDisplayName({ remoteName: '\\Inbox' }), 'Inbox')
assert.equal(mailboxDisplayName({ remoteName: 'INBOX' }), 'Inbox')
assert.equal(mailboxDisplayName({ remoteName: 'Sent Mail' }), 'Sent')
assert.equal(mailboxDisplayName({ remoteName: '\\Draft' }), 'Drafts')
assert.equal(mailboxDisplayName({ remoteName: 'Junk' }), 'Spam')
assert.equal(mailboxDisplayName({ remoteName: 'Bin' }), 'Trash')
assert.equal(mailboxDisplayName({ remoteName: '[Gmail]/All Mail' }), 'All Mail')
assert.equal(mailboxDisplayName({ remoteName: '[Google Mail]/Sent Mail' }), 'Sent')
assert.equal(
  mailboxDisplayName({ remoteName: 'Boat club' }),
  'Boat club',
  "a person's own folder is shown exactly as sent",
)

const oldGmailInbox = { id: 'mb-1', accountId: 'a1', role: 'other', remoteName: '\\Inbox' }
const promoted = promoteGmailInbox([oldGmailInbox, SENT])
assert.equal(promoted[0].role, 'inbox', 'the old row is promoted once nothing else claims the role')
assert.notEqual(promoted[0], oldGmailInbox, 'a fresh object -- the original is never mutated')
assert.equal(oldGmailInbox.role, 'other', 'the original is untouched')

const alreadyHasInbox = promoteGmailInbox([INBOX, oldGmailInbox])
assert.equal(
  alreadyHasInbox[1].role,
  'other',
  'a real role: "inbox" already claims it -- the label is left as a duplicate for MailNav to hide',
)

const oldGmailSent = { id: 'mb-2', accountId: 'a1', role: 'other', remoteName: '\\Sent' }
const noInboxLabelAtAll = promoteGmailInbox([oldGmailSent])
assert.equal(
  noInboxLabelAtAll[0],
  oldGmailSent,
  'no `\\Inbox` label and no real inbox either -- nothing to promote',
)

// ── Finding 2: refresh covers what is already loaded ────────────────────

assert.equal(refreshLimit(0, 50), 50, 'never less than one page')
assert.equal(refreshLimit(30, 50), 50, 'never less than one page')
assert.equal(refreshLimit(120, 50), 120, 'covers everything already scrolled past')
assert.equal(refreshLimit(50000, 50), 1000, 'capped well short of the whole mailbox')

// ── Finding 3: `j`/`k` (and prefetch) read whichever list is on screen ──

const mailboxThreads = [thread('th-1'), thread('th-2'), thread('th-3')]
const results = [thread('th-9'), thread('th-3')]

assert.deepEqual(
  visibleThreadList('', mailboxThreads, results).map((t) => t.id),
  ['th-1', 'th-2', 'th-3'],
  'no search: the mailbox list',
)
assert.deepEqual(
  visibleThreadList('  ', mailboxThreads, results).map((t) => t.id),
  ['th-1', 'th-2', 'th-3'],
  'blank search: still the mailbox list',
)
assert.deepEqual(
  visibleThreadList('priya', mailboxThreads, results).map((t) => t.id),
  ['th-9', 'th-3'],
  'a live search: its own results',
)

assert.equal(neighbourThread(mailboxThreads, null, 1).id, 'th-1', 'nothing selected: the first row')
assert.equal(neighbourThread(mailboxThreads, 'th-1', 1).id, 'th-2')
assert.equal(neighbourThread(mailboxThreads, 'th-1', -1), null, 'off the top of the list')
assert.equal(neighbourThread(mailboxThreads, 'th-3', 1), null, 'off the bottom of the list')
assert.equal(
  neighbourThread(mailboxThreads, 'not-shown', 1).id,
  'th-1',
  'a stale id from a different list falls back to the first row',
)

// ── Finding 4: a patch or a removal applies to every list a row is in ───
//
// `#act`/`#remove` in `mail.svelte.ts` call `applyRowPatch`/`removeRow`
// once for `threads` and once for `searchResults` -- the same pure
// functions, so what they do to one array they do identically to the
// other. This is that identically, made explicit.

const inMailbox = [thread('th-1'), thread('th-2')]
const inSearch = [thread('th-2')]

const mailboxPatch = applyRowPatch(inMailbox, 'th-2', { starred: true })
const searchPatch = applyRowPatch(inSearch, 'th-2', { starred: true })
assert.equal(mailboxPatch.rows[1].starred, true)
assert.equal(searchPatch.rows[0].starred, true, 'the same star reaches the search row too')

const mailboxRemoval = removeRow(inMailbox, 'th-2')
const searchRemoval = removeRow(inSearch, 'th-2')
assert.deepEqual(
  mailboxRemoval.rows.map((t) => t.id),
  ['th-1'],
)
assert.deepEqual(searchRemoval.rows, [], 'the same removal empties the search results too')

// ── Finding 5: the neighbour a removed thread leaves behind ─────────────
//
// `#advanceIfOpen` in `mail.svelte.ts` tries the row below, then the row
// above -- so a thread removed from the *end* of the list still lands on
// its new neighbour rather than nothing. Both halves are `neighbourThread`,
// already covered above; this is the fallback composition itself.

function advanceTo(list, id) {
  return neighbourThread(list, id, 1) ?? neighbourThread(list, id, -1)
}
assert.equal(advanceTo(mailboxThreads, 'th-1').id, 'th-2', 'the row below, ordinarily')
assert.equal(
  advanceTo(mailboxThreads, 'th-3').id,
  'th-2',
  'the last row falls back to the one above it',
)
assert.equal(advanceTo([thread('th-1')], 'th-1'), null, 'the only row leaves no neighbour at all')

// ── Date sections over the thread list ──────────────────────────────────
//
// 2026-09-14 is a Monday (see the snooze tests above for the same fact
// checked against Zeller's congruence) -- the fixed "now" most of these
// assertions are read against. A second "now", the Thursday three days
// later, is the one that can actually see "Earlier this week": nothing
// before today is left in the current week when "now" is itself a Monday.

const mondayNow = new Date('2026-09-14T10:00:00')
assert.deepEqual(dateSection('2026-09-14T06:00:00', mondayNow), { key: 'today', label: 'Today' })
assert.deepEqual(dateSection('2026-09-13T12:00:00', mondayNow), {
  key: 'yesterday',
  label: 'Yesterday',
})
assert.deepEqual(
  dateSection('2026-09-10T12:00:00', mondayNow),
  { key: 'lastWeek', label: 'Last week' },
  'Thursday the 10th is in the week before this one (Mon 7th - Sun 13th)',
)
assert.deepEqual(
  dateSection('2026-09-05T12:00:00', mondayNow),
  { key: 'thisMonth', label: 'Earlier this month' },
  'before last week, but still September',
)
assert.deepEqual(dateSection('2026-08-25T12:00:00', mondayNow), {
  key: 'lastMonth',
  label: 'Last month',
})
assert.deepEqual(
  dateSection('2026-07-15T12:00:00', mondayNow),
  { key: '2026-6', label: 'July' },
  'still this year: the month alone, no year printed',
)
assert.deepEqual(
  dateSection('2025-12-20T12:00:00', mondayNow),
  { key: '2025-11', label: 'December 2025' },
  'a previous year: month and year both',
)

const thursdayNow = new Date('2026-09-17T10:00:00')
assert.deepEqual(
  dateSection('2026-09-15T12:00:00', thursdayNow),
  { key: 'thisWeek', label: 'Earlier this week' },
  'Tuesday the 15th, in the week (Mon 14th -) "now" itself is in',
)

// `withDateSections` interleaves one heading per run, not one per row --
// reusing `thread()` from the keyset-paging tests above for its rows.
const grouped = withDateSections(
  [
    thread('a', '2026-09-14T08:00:00'),
    thread('b', '2026-09-14T09:00:00'),
    thread('c', '2026-09-13T08:00:00'),
  ],
  (t) => t.lastDate,
  mondayNow,
)
assert.deepEqual(
  grouped.map((r) => (r.type === 'header' ? `#${r.key}` : r.thread.id)),
  ['#today', 'a', 'b', '#yesterday', 'c'],
  'one heading ahead of each run, never one per row under it',
)

// ── Compose: a blank draft is discarded, not autosaved ──────────────────
//
// `MailCompose.svelte`'s `discard()` used to delete the draft on this branch
// and then let its `onDestroy` safety net flush the very same draft back
// into existence -- a keystroke-loss fix (flush on unmount) colliding with
// the empty-draft fix (delete on unmount). The boolean the two paths must
// agree on is `isBlankDraft`.

function draft(overrides = {}) {
  return { subject: '', bodyHtml: '', to: [], ...overrides }
}

assert.equal(isBlankDraft(draft()), true, 'nothing typed at all')
assert.equal(
  isBlankDraft(draft({ bodyHtml: '<p></p>' })),
  true,
  'empty paragraph tags from a fresh editor are not "content"',
)
assert.equal(isBlankDraft(draft({ subject: 'Hi' })), false, 'a subject alone is worth keeping')
assert.equal(
  isBlankDraft(draft({ bodyHtml: '<p>hello</p>' })),
  false,
  'typed body text is worth keeping',
)
assert.equal(
  isBlankDraft(draft({ to: [person('Priya Raman', 'priya@example.com')] })),
  false,
  'a recipient alone is worth keeping',
)

// ── Shift+click's range ─────────────────────────────────────────────

{
  const list = ['a', 'b', 'c', 'd', 'e'].map((id) => ({ id }))
  assert.deepEqual(threadRange(list, 'b', 'd'), ['b', 'c', 'd'], 'downwards, both ends in')
  assert.deepEqual(threadRange(list, 'd', 'b'), ['b', 'c', 'd'], 'upwards reads in list order')
  assert.deepEqual(threadRange(list, 'c', 'c'), ['c'], 'one row is a range of one')
  assert.deepEqual(threadRange(list, null, 'c'), ['c'], 'no anchor yet: just the row clicked')
  assert.deepEqual(threadRange(list, 'gone', 'c'), ['c'], 'an anchor off this list: the same')
  assert.deepEqual(threadRange(list, 'a', 'gone'), [], 'a target off the list: nothing')
}

// ── The category tabs' badges ───────────────────────────────────────

{
  const counts = [
    { category: 'important', threads: 12, unread: 3 },
    { category: 'newsletter', threads: 40, unread: 0 },
    { category: null, threads: 5, unread: 1 },
  ]
  assert.deepEqual(categoryTabCount(counts, 'important'), { threads: 12, unread: 3 })
  assert.deepEqual(
    categoryTabCount(counts, 'priority'),
    { threads: 0, unread: 0 },
    'a category with no row counts nothing',
  )
  assert.deepEqual(
    categoryTabCount(counts, null),
    { threads: 57, unread: 4 },
    'All counts every row, the uncategorised included',
  )
}

// ── An address typed rather than picked ─────────────────────────────

assert.deepEqual(parseTypedAddress('  ann@example.com, '), { name: '', email: 'ann@example.com' })
assert.deepEqual(parseTypedAddress('Ann Lee <ann@example.com>'), {
  name: 'Ann Lee',
  email: 'ann@example.com',
})
assert.deepEqual(
  parseTypedAddress('"Lee, Ann" <ann@example.com>;'),
  { name: 'Lee, Ann', email: 'ann@example.com' },
  'quotes round the name are dropped, and a trailing separator is not part of it',
)
assert.equal(parseTypedAddress(' , '), null, 'nothing typed is no address')

// ── sumCategoryCounts: the unified Inbox's tab badges ───────────────

assert.deepEqual(
  sumCategoryCounts([
    [
      { category: 'important', threads: 3, unread: 1 },
      { category: null, threads: 2, unread: 0 },
    ],
    [
      { category: 'important', threads: 1, unread: 1 },
      { category: 'newsletters', threads: 4, unread: 2 },
    ],
  ]),
  [
    { category: 'important', threads: 4, unread: 2 },
    { category: null, threads: 2, unread: 0 },
    { category: 'newsletters', threads: 4, unread: 2 },
  ],
  'each category is added up across every inbox, in the order first met',
)
assert.deepEqual(sumCategoryCounts([]), [], 'no inboxes, no badges')

// ── messageDateLabel: an open message's day and time together ───────

{
  const now = new Date(2026, 9, 6, 15, 0)
  assert.match(
    messageDateLabel(new Date(2026, 9, 6, 7, 52).toISOString(), now),
    /^Today at 0?7:52/,
    'today says so, with the time',
  )
  assert.match(
    messageDateLabel(new Date(2026, 9, 5, 6, 30).toISOString(), now),
    /^Yesterday at 0?6:30/,
    'yesterday says so too',
  )
  assert.match(
    messageDateLabel(new Date(2026, 8, 1, 9, 5).toISOString(), now),
    / at 0?9:05/,
    'an older day still carries its time',
  )
}

await close()
console.log('mail: all checks passed')
