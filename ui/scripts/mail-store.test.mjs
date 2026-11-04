// Behaviour checks for `mail.svelte.ts` itself: `refresh`, `loadMore` and
// the `#applyChanges`/`#patchOne` pair `live.svelte.ts` calls through
// `registerApply`. `mail.test.mjs` only loads the pure logic in `mail.ts`
// -- `applyRowPatch`, `mergeSearchPage` and the rest -- never this class.
// Written before Phase 4 moves `refresh`'s and `loadMore`'s generation
// guard onto `latest()`, and the live-apply pair onto the shared
// `store/` helpers, on the same theory as `library-store.test.mjs`. `#act`
// and `#remove` -- the optimistic single-row actions -- are untouched by
// that migration and are not characterised again here.

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
  modules: [mailModule, stateModule, liveApplyModule, mailApi, { api }, mockMail],
  close,
} = await load(
  [
    '/src/lib/mail.svelte.ts',
    '/src/lib/state.svelte.ts',
    '/src/lib/live-apply.ts',
    '/src/lib/mail-api.ts',
    '/src/lib/api.ts',
    '/src/lib/mock-mail.ts',
  ],
  { svelte: true },
)
const { mail } = mailModule
const { app } = stateModule
const { applierFor } = liveApplyModule

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')

// ── start / refresh ──────────────────────────────────────────────────

await mail.start()
assert.ok(mail.mailboxes.length > 0, 'the mock vault ships with mailboxes')
assert.ok(mail.selectedMailbox, 'the inbox is selected by default')
assert.equal(mail.loading, false, 'loading settles back to false once the load lands')

// ── All accounts at once ─────────────────────────────────────────────
//
// The mock seeds two accounts with mail, so Mail opens on every inbox at
// once rather than on whichever account's comes first.

{
  assert.equal(
    mail.selectedMailbox,
    'unified:inbox',
    'two accounts with mail: Mail opens on the unified Inbox',
  )
  assert.equal(mail.mailbox?.pseudo, 'unified', '`mailbox` finds the unified row too')
  assert.ok(
    !mail.mailboxes.some((m) => m.pseudo === 'unified'),
    'unified rows stay out of `mailboxes`, which everything else reads as real folders',
  )
  const accountsShown = new Set(mail.threads.map((t) => t.accountId))
  assert.ok(
    accountsShown.has('acct-google') && accountsShown.has('acct-fastmail'),
    "both accounts' inbox threads are in the one list",
  )
  const dates = mail.threads.map((t) => t.lastDate)
  assert.deepEqual(dates, [...dates].sort().reverse(), 'newest first, across both accounts')
  assert.equal(
    new Set(mail.threads.map((t) => t.id)).size,
    mail.threads.length,
    'no thread listed twice',
  )
  assert.ok(
    mail.threads.every((t) => !t.snoozedUntil),
    'snoozed threads stay out of the unified Inbox, as out of each one',
  )

  // Its badge is its members' added up.
  await mail.refreshUnreadCounts()
  const google = mail.mailboxes.find((m) => m.accountId === 'acct-google' && m.role === 'inbox')
  const fastmail = mail.mailboxes.find((m) => m.accountId === 'acct-fastmail' && m.role === 'inbox')
  assert.equal(
    mail.unreadCounts.get('unified:inbox'),
    (mail.unreadCounts.get(google.id) ?? 0) + (mail.unreadCounts.get(fastmail.id) ?? 0),
    "the unified Inbox's unread count is both inboxes' together",
  )

  // A batch change from a background sync names no thread at all -- it is
  // taken, not declined to `RELOAD.mail`'s list-only refresh.
  const applier = applierFor('mail')
  assert.equal(
    applier([{ kind: 'thread', op: 'updated', origin: 'remote' }]),
    true,
    'a sync pass with no ids is handled by the store itself',
  )
  await new Promise((r) => setTimeout(r, 700))
  assert.equal(mail.selectedMailbox, 'unified:inbox', 'and leaves the open list where it was')
}

// A selection that has left the reloaded page is dropped.
mail.selectedThread = 'not-a-real-id'
await mail.refresh()
assert.equal(mail.selectedThread, null, 'a selection not in the reloaded page is cleared')

// ── loadMore ─────────────────────────────────────────────────────────

if (mail.nextCursor) {
  const before = mail.threads.length
  await mail.loadMore()
  assert.ok(mail.threads.length >= before, 'loadMore appends without shrinking the list')
}

// ── #applyChanges / #patchOne ────────────────────────────────────────

const applier = applierFor('mail')
assert.ok(applier, 'the mail store registers an applier on import')

// A draft change is a no-op today -- nothing in the thread list reads a
// draft directly, per the store's own comment.
assert.equal(
  applier([{ kind: 'draft', op: 'updated', id: 'd-1', origin: 'remote' }]),
  true,
  'a draft change is accepted and does nothing',
)

assert.equal(
  applier([
    { kind: 'thread', op: 'updated', id: 't1', origin: 'remote' },
    { kind: 'thread', op: 'updated', id: 't2', origin: 'remote' },
  ]),
  false,
  'more than one change in a batch is declined',
)

{
  const id = mail.threads[0].id
  const before = mail.threads.length
  const accepted = applier([{ kind: 'thread', op: 'deleted', id, origin: 'remote' }])
  assert.equal(accepted, true)
  assert.equal(mail.threads.length, before - 1, 'a deleted thread leaves the list at once')
  assert.ok(!mail.threads.some((t) => t.id === id))
}

{
  const id = mail.threads[0].id
  const row = mail.threads.find((t) => t.id === id)
  const before = row.unreadCount
  const accepted = applier([{ kind: 'thread', op: 'updated', id, origin: 'remote' }])
  assert.equal(accepted, true, 'a single update is accepted -- the fetch runs in the background')
  await new Promise((r) => setTimeout(r, 100))
  const now = mail.threads.find((t) => t.id === id)
  assert.ok(now, 'the row is still in the list after being patched')
  assert.equal(typeof now.unreadCount, typeof before, 'the row was replaced with a fresh fetch')
}

// ── The open thread follows a sync pass ──────────────────────────────
//
// A pass names no thread, and the reply somebody just sent only ever reaches
// the thread they are reading this way. The pane used to go on showing the
// thread as it was until they left it and came back.

{
  const inbox = mail.mailboxes.find((m) => m.role === 'inbox')
  await mail.selectMailbox(inbox.id)
  const id = mail.threads.find((t) => t.messageCount > 1).id
  await mail.openThreadById(id)
  const before = mail.openThread.messages.length
  const opened = new Set(mail.expanded)
  const older = mail.openThread.messages[0].id
  mail.toggleExpanded(older)

  const sent = mockMail.mockDeliverToThread(
    id,
    { name: 'Me', email: 'me@example.com' },
    '<p>The reply I just sent</p>',
    true,
  )
  assert.equal(mail.openThread.messages.length, before, 'nothing moves until the pass says so')
  const accepted = applier([{ kind: 'thread', op: 'updated', origin: 'sync' }])
  assert.equal(accepted, true, 'a change naming no thread is taken')
  await new Promise((r) => setTimeout(r, 900))

  assert.equal(mail.openThread.messages.length, before + 1, 'and now it is in the pane')
  assert.equal(mail.openThread.messages.at(-1).id, sent.id)
  assert.equal(
    mail.threads.find((t) => t.id === id).messageCount,
    before + 1,
    'and the list row says so too',
  )
  assert.ok(mail.expanded.has(sent.id), 'open, as the newest message is')
  assert.ok(mail.expanded.has(older), 'what somebody opened stays open')
  for (const message of opened) assert.ok(mail.expanded.has(message), 'and so does the rest')

  // Nothing new: nothing opens, and what was closed stays closed.
  mail.toggleExpanded(sent.id)
  applier([{ kind: 'thread', op: 'updated', origin: 'sync' }])
  await new Promise((r) => setTimeout(r, 900))
  assert.ok(!mail.expanded.has(sent.id), 'a pass with nothing new reopens nothing')

  // Moved on while it was on its way: the other thread keeps the pane.
  const other = mail.threads.find((t) => t.id !== id).id
  applier([{ kind: 'thread', op: 'updated', origin: 'sync' }])
  await mail.openThreadById(other)
  await new Promise((r) => setTimeout(r, 900))
  assert.equal(mail.openThread.thread.id, other)

  // What somebody else wrote is read once somebody can see it, and not before.
  const reload = () => new Promise((r) => setTimeout(r, 900))
  const deliver = () =>
    mockMail.mockDeliverToThread(
      other,
      { name: 'Ada', email: 'ada@example.com' },
      '<p>Hi</p>',
      false,
    )
  const pass = () => applier([{ kind: 'thread', op: 'updated', origin: 'sync' }])
  const unread = () => mail.openThread.thread.unreadCount
  const look = (visible, focused) => {
    document.visibilityState = visible ? 'visible' : 'hidden'
    document.hasFocus = () => focused
  }

  // Behind another application: still unread, though it opens ready.
  look(true, false)
  mail.setOnScreen(true)
  const behind = deliver()
  pass()
  await reload()
  assert.ok(unread() > 0, 'a window in the background has read nothing')
  assert.ok(mail.expanded.has(behind.id), 'though it is open for when they come back')
  // ...and they come back, by the banner, as the window does.
  look(true, true)
  mail.seeOpenThread()
  await reload()
  assert.equal(unread(), 0, 'coming back to it is reading it')

  // In the app, in Journal or in Settings: the window has focus and Mail is
  // not what it is showing.
  mail.setOnScreen(false)
  const elsewhere = deliver()
  pass()
  await reload()
  assert.ok(mail.expanded.has(elsewhere.id))
  assert.ok(unread() > 0, 'Mail off screen has read nothing, with the window in front')
  mail.seeOpenThread()
  assert.ok(unread() > 0, 'and coming back to the window is not coming back to Mail')
  mail.setOnScreen(true)
  await reload()
  assert.equal(unread(), 0, 'coming back to Mail is')

  // In front of them: read as it lands.
  deliver()
  pass()
  await reload()
  assert.equal(unread(), 0, 'looking at it is reading it')

  // Marked unread by hand: a sync later is no reason to undo that.
  look(true, false)
  deliver()
  pass()
  await reload()
  await mail.markUnread(other)
  look(true, true)
  mail.seeOpenThread()
  await reload()
  assert.ok(unread() > 0, 'they said unread, so it stays unread')

  // Two at once, one dated earlier than the end of the thread: both are mail
  // that arrived, and being older is no reason to stay shut.
  const late = mockMail.mockDeliverToThread(
    other,
    { name: 'Ada', email: 'ada@example.com' },
    '<p>Sent a while ago, delayed</p>',
    true,
  )
  mockMail.mockBackdate(late.id, '2000-01-01T00:00:00.000Z')
  const onTime = deliver()
  pass()
  await reload()
  assert.equal(mail.openThread.messages[0].id, late.id, 'filed by its date')
  assert.ok(mail.expanded.has(late.id), 'the delayed one opens')
  assert.ok(mail.expanded.has(onTime.id), 'and so does the other')

  // A draft's copy on the server is what the composer below is writing. It is
  // not news, and it does not join a thread already open.
  const count = mail.openThread.messages.length
  const draftCopy = mockMail.mockDeliverToThread(
    other,
    { name: 'Me', email: 'me@example.com' },
    '<p>Half a thought</p>',
    true,
  )
  mockMail.mockMarkDraft(draftCopy.id)
  pass()
  await reload()
  assert.equal(mail.openThread.messages.length, count, 'a server copy of a draft stays out of it')
  assert.ok(!mail.expanded.has(draftCopy.id))

  // An older answer arriving last is not drawn over a newer one.
  const real = api.thread
  let release = () => {}
  const holdNextRead = () => {
    api.thread = (id) => {
      // A copy, as a real answer is: what the backend said when it said it.
      const answer = real(id).then((detail) => structuredClone(detail))
      api.thread = real
      return new Promise((resolve) => {
        release = () => answer.then(resolve)
      })
    }
  }
  holdNextRead()
  pass()
  await reload()
  const newer = deliver()
  pass()
  await reload()
  assert.ok(
    mail.openThread.messages.some((m) => m.id === newer.id),
    'the newer answer is drawn',
  )
  release()
  await new Promise((r) => setTimeout(r, 50))
  assert.ok(
    mail.openThread.messages.some((m) => m.id === newer.id),
    'and the older one, landing after it, does not take the message back',
  )
  await reload()

  // A change made here while a read is out is not undone by it, and what the
  // read saw still comes: it is asked for again.
  holdNextRead()
  pass()
  await reload()
  const during = deliver()
  const wasStarred = mail.openThread.thread.starred
  await mail.toggleStar(other)
  release()
  await new Promise((r) => setTimeout(r, 50))
  assert.equal(mail.openThread.thread.starred, !wasStarred, 'the change made meanwhile stands')
  await reload()
  assert.ok(
    mail.openThread.messages.some((m) => m.id === during.id),
    'and the message arrives',
  )
  look(false, false)
  mail.setOnScreen(false)
}

// ── Drafts nobody typed in ───────────────────────────────────────────
//
// `new_draft` keeps nothing, so a blank one has no row until it is saved. The
// bin on one used to answer "Draft <id> not found" -- and the autosave its
// failure path started is why a second click worked.

{
  const account = mail.mailboxes[0].accountId
  const draft = await mailApi.newDraft({ account })
  await mailApi.discardDraft(draft.id)
  await mailApi.restoreDraft(draft.id)
  await mailApi.saveDraft(draft)
  await mailApi.discardDraft(draft.id)
}

// ── Priority ─────────────────────────────────────────────────────────

{
  const row = mail.threads[0]
  await mail.setPriority(row.id, true)
  assert.equal(mail.threads.find((t) => t.id === row.id).category, 'priority')
  await mail.setPriority(row.id, false)
  assert.equal(
    mail.threads.find((t) => t.id === row.id).category,
    'important',
    'unflagging always lands on important, the same simplification the mock makes',
  )
}

// ── Snoozed: the pseudo-mailbox the mock seeds for the selected account ──

{
  const snoozedBox = mail.mailboxes.find((m) => m.pseudo === 'snoozed')
  assert.ok(snoozedBox, 'the mock seeds a Snoozed pseudo-mailbox for every account')
  await mail.selectMailbox(snoozedBox.id)
  assert.equal(mail.loading, false)
  assert.ok(mail.threads.length > 0, 'the seed has at least a handful of snoozed threads')
  assert.ok(
    mail.threads.every((t) => t.snoozedUntil),
    "every row is actually snoozed -- `listTarget` left the mock's own pseudo-mailbox id alone",
  )
  // Back to an ordinary mailbox for what follows.
  const inbox = mail.mailboxes.find((m) => m.role === 'inbox')
  await mail.selectMailbox(inbox.id)
}

// ── Scheduled sends ──────────────────────────────────────────────────

await mail.refreshScheduled()
assert.ok(mail.scheduled.length > 0, 'the mock seeds at least one scheduled send')
assert.ok(
  mail.scheduled.every((s) => s.draft.state.type === 'queued'),
  'only still-queued drafts are scheduled sends',
)

{
  const before = mail.viewingScheduledFor
  const accountId = mail.scheduled[0].draft.accountId
  mail.selectScheduled(accountId)
  assert.equal(mail.viewingScheduledFor, accountId)
  assert.equal(mail.selectedThread, null, 'selecting Scheduled drops the open thread')
  await mail.selectMailbox(mail.mailboxes.find((m) => m.role === 'inbox').id)
  assert.equal(
    mail.viewingScheduledFor,
    null,
    'selecting an ordinary mailbox leaves Scheduled behind',
  )
  void before
}

// ── Inline replies vs. a message filling the reading pane ───────────

{
  const id = mail.threads[0].id
  await mail.openThreadById(id)
  await mail.reply(mail.openThread.messages.at(-1).id, false)
  assert.ok(mail.composing, 'reply() opens a draft')
  assert.equal(mail.composeInline, true, 'a reply into the open thread is always inline')
  mail.closeCompose()
  assert.equal(mail.composing, null)
  assert.equal(mail.composeInline, false, 'closeCompose always drops composeInline back to false')

  await mail.compose()
  assert.equal(mail.composeInline, false, 'a brand-new message fills the reading pane')
  const fresh = mail.composing

  // Opening a thread makes way for it, putting the message aside for its
  // sheet to offer back -- never dropping it on the floor.
  await mail.openThreadById(mail.threads[1].id)
  assert.equal(mail.composing, null, 'the thread takes the pane')
  assert.equal(mail.parkedDraft, fresh.id, 'the message it replaced is the one set aside')
  mail.parkedDraft = null

  mail.openDraft(fresh)
  assert.equal(mail.composing.id, fresh.id, 'Open brings the same draft back')
  assert.equal(mail.composeInline, false)

  // A second message over the first sets the first aside the same way.
  await mail.compose()
  assert.notEqual(mail.composing.id, fresh.id)
  assert.equal(mail.parkedDraft, fresh.id)
  mail.parkedDraft = null
  mail.closeCompose()
}

// ── Picking several threads ─────────────────────────────────────────

/** Long enough for the store's debounced live refresh to have run. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 900))

{
  await settle()
  await mail.selectMailbox(mail.mailboxes.find((m) => m.role === 'inbox').id)
  const ids = mail.threads.map((t) => t.id)
  assert.ok(ids.length >= 4, 'the mock inbox has enough threads to pick from')

  await mail.openThreadById(ids[0])
  mail.clickThread(ids[2], { toggle: true })
  assert.deepEqual(
    [...mail.checked].sort(),
    [ids[0], ids[2]].sort(),
    'a first Ctrl+click picks the open thread too',
  )
  mail.clickThread(ids[2], { toggle: true })
  assert.deepEqual([...mail.checked], [ids[0]], 'a second Ctrl+click puts it back')

  mail.clickThread(ids[1], { toggle: true })
  mail.clickThread(ids[3], { shift: true })
  assert.deepEqual(
    mail.targets,
    [ids[1], ids[2], ids[3]],
    'Shift+click takes the run from the last click',
  )
  assert.equal(mail.showing.open.length, 0, 'no single open thread while several are picked')

  await mail.moveSelection(1)
  assert.equal(mail.checked.size, 3, 'j only moves the cursor while threads are picked')

  mail.clickThread(ids[0])
  await Promise.resolve()
  assert.equal(mail.checked.size, 0, 'a plain click opens one and lets the rest go')

  mail.checkAll()
  assert.equal(mail.checked.size, mail.threads.length, 'Mod+A picks every loaded thread')
  mail.clearChecked()

  const before = mail.threads.length
  mail.clickThread(ids[0], { toggle: true })
  mail.clickThread(ids[1], { toggle: true })
  await mail.archiveMany(mail.targets)
  // A live refresh can land mid-archive with the page as it was a moment
  // before -- the store's own follow-up refresh is what settles it.
  await settle()
  assert.equal(mail.threads.length, before - 2, 'archiving the pick removes every picked row')
  assert.ok(!mail.threads.some((t) => t.id === ids[0] || t.id === ids[1]))
  assert.equal(mail.checked.size, 0, 'and nothing archived is left picked')
}

// ── The category tabs' badges ───────────────────────────────────────

{
  await mail.refreshCategoryCounts()
  assert.ok(mail.categoryCounts.length > 0, 'the inbox has its threads counted by category')
  const counted = mail.categoryCounts.reduce((n, c) => n + c.threads, 0)
  assert.ok(counted >= mail.threads.length, 'every listed thread is counted somewhere')
}

// ── The "show only" toggles ─────────────────────────────────────────

{
  // Every inbox at once: of the mock's two, one is all read, the other all
  // unread.
  const inbox = mail.unifiedMailboxes.find((m) => m.id === 'unified:inbox')
  const sent = mail.unifiedMailboxes.find((m) => m.id === 'unified:sent')
  await mail.selectMailbox(inbox.id)
  mail.setCategory(null)
  await mail.refresh()
  const everything = mail.threads.length
  assert.ok(everything > 0)

  mail.toggleOnly('unread')
  await mail.refresh()
  assert.deepEqual([...mail.only], ['unread'])
  assert.ok(mail.threads.length > 1, 'the seed has unread mail in this inbox')
  assert.ok(mail.threads.length < everything, 'the seed has read mail in this inbox too')
  assert.ok(
    mail.threads.every((t) => t.unreadCount > 0),
    'only unread rows are asked for',
  )
  const unreadRows = mail.threads.length

  // Reading one in the list leaves it there, read, through the next sync.
  const [first, second] = mail.threads
  await mail.openThreadById(first.id)
  await mail.refresh()
  const after = mail.threads.find((t) => t.id === first.id)
  assert.equal(after?.unreadCount, 0, 'still listed, and read')
  assert.equal(mail.threads.length, unreadRows, 'nothing else moved')
  assert.equal(mail.openThread?.thread.id, first.id, 'and the reading pane is left alone')

  // A second one lingers beside it, and `k` can still get back.
  await mail.openThreadById(second.id)
  await mail.refresh()
  assert.equal(mail.threads.length, unreadRows, 'both still listed')

  // What was done to it says it is finished with: not put back.
  await mail.archive(first.id)
  await mail.refresh()
  assert.ok(!mail.threads.some((t) => t.id === first.id), 'an archived row stays gone')

  // The toggle is the inbox's own. Sent would have nothing to show for it.
  await mail.selectMailbox(sent.id)
  assert.equal(mail.only.size, 0, 'another mailbox has its own')
  assert.ok(mail.threads.length > 0)
  await mail.selectMailbox(inbox.id)
  assert.deepEqual([...mail.only], ['unread'], 'and coming back finds it still on')
  assert.ok(
    mail.threads.every((t) => t.unreadCount > 0),
    'coming back asks afresh, so what was read has gone',
  )
  assert.ok(!mail.threads.some((t) => t.id === second.id), 'including the one that lingered')

  // A filter is no reason to lose your place: turning one on keeps the open
  // thread listed, read or not, and a thread opened from elsewhere is not
  // closed just because the list would not have shown it.
  mail.setOnly(new Set())
  await mail.refresh()
  const read = mail.threads.find((t) => t.unreadCount === 0)
  await mail.openThreadById(read.id)
  mail.toggleOnly('unread')
  await mail.refresh()
  assert.ok(
    mail.threads.some((t) => t.id === read.id),
    'the thread being read stays listed',
  )
  mail.setOnly(new Set(['starred']))
  await mail.refresh()
  assert.equal(mail.openThread?.thread.id, read.id, 'and stays open')
  assert.ok(
    mail.threads.filter((t) => t.id !== read.id).every((t) => t.starred),
    'starred rows only, beside it',
  )

  // Unstarring lingers the same way reading does, and starring again lets go.
  const starred = mail.threads.find((t) => t.starred && t.id !== read.id)
  assert.ok(starred, 'the seed has a starred thread in this inbox')
  await mail.toggleStar(starred.id)
  await mail.refresh()
  assert.equal(mail.threads.find((t) => t.id === starred.id)?.starred, false, 'kept, unstarred')
  await mail.toggleStar(starred.id)

  // A later page is asked for the same thing as the first.
  mail.setOnly(new Set(['unread']))
  await mail.refresh()
  const asked = []
  const across = api.threadsAcross
  api.threadsAcross = (mailboxes, filter, cursor, limit) => {
    asked.push({ filter, limit })
    return across(mailboxes, filter, cursor, limit)
  }
  mail.nextCursor = '0'
  await mail.loadMore()
  assert.equal(asked.at(-1).filter.unread, true, 'loadMore keeps to the toggle')

  // However many rows are held on to, a refresh asks for no more than it did:
  // counting them would ask further down the mailbox every time.
  mail.setOnly(new Set())
  await mail.refresh()
  mail.toggleOnly('unread')
  await mail.refresh()
  await mail.openThreadById(mail.threads[0].id)
  await mail.refresh()
  await mail.openThreadById(mail.threads[1].id)
  await mail.refresh()
  const settledAt = asked.at(-1).limit
  await mail.refresh()
  await mail.refresh()
  assert.equal(asked.at(-1).limit, settledAt, 'the rows held on to do not count')
  api.threadsAcross = across

  // Off again, the open thread is kept and listed, even where the page asked
  // for does not reach it -- a filter is no reason to lose your place. The
  // Newsletters tab stands in for a thread lying past the end of the page.
  mail.setOnly(new Set())
  await mail.refresh()
  const notANewsletter = mail.threads.find((t) => t.category !== 'newsletter')
  await mail.openThreadById(notANewsletter.id)
  mail.setCategory('newsletter')
  await mail.refresh()
  assert.equal(mail.selectedThread, null, 'a tab change with no toggle closes it, as it always has')
  mail.setCategory(null)
  await mail.refresh()
  mail.toggleOnly('unread')
  await mail.refresh()
  await mail.openThreadById(mail.threads[0].id)
  mail.setCategory(mail.threads[0].category === 'newsletter' ? 'notification' : 'newsletter')
  await mail.refresh()
  const kept = mail.selectedThread
  assert.ok(kept, 'with a toggle on, hidden from the tab but still in the mailbox: stays open')
  mail.setOnly(new Set())
  await mail.refresh()
  assert.equal(mail.openThread?.thread.id, kept, 'and turning the toggle off does not close it')
  assert.ok(
    mail.threads.some((t) => t.id === kept),
    'it stays listed',
  )
  mail.setCategory(null)
  await mail.refresh()

  // Gone from the mailbox altogether, though, is gone: archived on a phone,
  // which reaches this window as nothing more than a refresh.
  mail.toggleOnly('unread')
  await mail.refresh()
  const goneElsewhere = mail.threads[0].id
  await mail.openThreadById(goneElsewhere)
  await mail.refresh()
  assert.ok(
    mail.threads.some((t) => t.id === goneElsewhere),
    'lingering, read',
  )
  await mailApi.archiveMany([goneElsewhere])
  await mail.refresh()
  assert.ok(!mail.threads.some((t) => t.id === goneElsewhere), 'a row that left is dropped')
  assert.equal(mail.selectedThread, null, 'and the pane does not go on showing it')
  assert.equal(mail.openThread, null)

  // The same with a reply half written under it: set aside, not stranded.
  const replyingTo = mail.threads[0].id
  await mail.openThreadById(replyingTo)
  await mail.reply(mail.openThread.messages.at(-1).id, false)
  assert.equal(mail.composeInline, true)
  await mailApi.archiveMany([replyingTo])
  await mail.refresh()
  assert.equal(mail.openThread, null)
  assert.equal(mail.composing, null, 'no draft left in `composing` with nowhere to draw it')
  assert.ok(mail.parkedDraft, 'and it is said to be saved')
  mail.parkedDraft = null

  // Undone, or reversed: what lingered lets go, so that being read elsewhere
  // afterwards takes it out as it should.
  await mail.refresh()
  const failing = mail.threads.find((t) => t.unreadCount > 0).id
  const markRead = api.markRead
  api.markRead = () => Promise.reject(new Error('no'))
  await mail.markRead(failing)
  api.markRead = markRead
  assert.ok(
    mail.threads.find((t) => t.id === failing).unreadCount > 0,
    'a failed mark goes back to unread',
  )
  await mailApi.markReadMany([failing])
  await mail.refresh()
  assert.ok(!mail.threads.some((t) => t.id === failing), 'read elsewhere: it does not linger')

  const reversed = mail.threads.find((t) => t.unreadCount > 0).id
  await mail.markRead(reversed)
  await mail.markUnread(reversed)
  await mailApi.markReadMany([reversed])
  await mail.refresh()
  assert.ok(!mail.threads.some((t) => t.id === reversed), 'nor after being marked unread again')

  // Marking another mailbox read says nothing about this list.
  const lingerer = mail.threads[0].id
  await mail.markRead(lingerer)
  await mail.markMailboxRead(mail.mailboxes.find((m) => m.role === 'archive').id)
  await mail.refresh()
  assert.ok(
    mail.threads.some((t) => t.id === lingerer),
    'another mailbox: it stays',
  )
  await mail.markMailboxRead(inbox.id)
  await mail.refresh()
  assert.equal(mail.threads.length, 0, 'this one, with nothing open: nothing left to show')

  mail.setOnly(new Set())
  await mail.refresh()
  assert.equal(mail.only.size, 0)
  assert.ok(mail.threads.length > 0)
}

// ── Quick cleanup: archiving many threads at once ───────────────────

{
  await mail.selectMailbox('unified:inbox')
  const ids = mail.threads.slice(0, 3).map((t) => t.id)
  assert.equal(ids.length, 3, 'the seed has at least three inbox threads to clear')
  const before = mail.threads.length
  await mail.openThreadById(ids[0])
  const ok = await mail.archiveThreads([...ids, ids[0]])
  assert.equal(ok, true, 'every chunk went')
  assert.equal(mail.threads.length, before - 3, 'each thread leaves the list once')
  assert.ok(
    ids.every((id) => !mail.threads.some((t) => t.id === id)),
    'none of them is still listed',
  )
  assert.ok(
    mail.selectedThread === null || !ids.includes(mail.selectedThread),
    'the open thread moved on to one that is staying',
  )
  await mail.refresh()
  assert.ok(
    ids.every((id) => !mail.threads.some((t) => t.id === id)),
    'archived threads stay gone once the list is asked for again',
  )

  const more = mail.threads.slice(0, 2).map((t) => t.id)
  assert.equal(await mail.trashThreads(more), true)
  assert.ok(
    more.every((id) => !mail.threads.some((t) => t.id === id)),
    'trashed rows leave too',
  )
  assert.equal(await mail.archiveThreads([]), true, 'nothing to do is not a failure')
}

await close()
console.log('mail-store: all checks passed')

// `app.start()` arms the idle-vault poll; see `editor.test.mjs`'s own note.
process.exit(0)
