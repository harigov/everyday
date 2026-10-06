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
  modules: [mailModule, stateModule, liveApplyModule],
  close,
} = await load(['/src/lib/mail.svelte.ts', '/src/lib/state.svelte.ts', '/src/lib/live-apply.ts'], {
  svelte: true,
})
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

await close()
console.log('mail-store: all checks passed')

// `app.start()` arms the idle-vault poll; see `editor.test.mjs`'s own note.
process.exit(0)
