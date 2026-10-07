// Behaviour checks for `calendar.svelte.ts` itself: `refresh`, `patch` and
// `removeBlock`, none of which any existing test loads --
// `calendar.test.mjs` stops at `time.ts`'s pure grid arithmetic. Written
// before Phase 4 moves this store onto the shared `store/` helpers, on the
// same theory as `library-store.test.mjs`.
//
// And where a new event goes: a block on this computer until an account
// calendar is the default, a draft bound for that calendar afterwards --
// written only on Save -- and the repeating blocks a series is made of.
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
  modules: [calendarModule, stateModule, apiModule],
  close,
} = await load(['/src/lib/calendar.svelte.ts', '/src/lib/state.svelte.ts', '/src/lib/api.ts'], {
  svelte: true,
})
const { calendar } = calendarModule
const { app } = stateModule
const { api } = apiModule

/** `YYYY-MM-DD`, `n` local days after `iso`. */
function plusDays(iso, n) {
  const [y, m, d] = iso.split('-').map(Number)
  const at = new Date(y, m - 1, d + n)
  const p = (v) => String(v).padStart(2, '0')
  return `${at.getFullYear()}-${p(at.getMonth() + 1)}-${p(at.getDate())}`
}

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')

// ── start / refresh ──────────────────────────────────────────────────

await calendar.start()
assert.equal(calendar.loading, false, 'loading settles back to false once the load lands')

// Book something to work with -- the store's one write.
const block = await calendar.book({
  subject: { type: 'adhoc' },
  day: calendar.anchor,
  startMinutes: 9 * 60,
  minutes: 30,
  title: 'characterised',
})
assert.ok(block, 'booking a block should succeed against the mock')
assert.equal(calendar.selection?.id, block.id, 'booking selects the new block by default')

// A selection outside the window loaded by `refresh` is dropped.
calendar.selection = { kind: 'block', id: 'not-a-real-id' }
await calendar.refresh()
assert.equal(calendar.selection, null, 'a selection for a block not in range is cleared')

// ── patch ────────────────────────────────────────────────────────────

{
  const before = block.updatedAt
  await new Promise((r) => setTimeout(r, 5))
  calendar.patch(block.id, { title: 'patched' })
  const now = calendar.blocks.find((b) => b.id === block.id)
  assert.equal(now.title, 'patched', 'patch assigns the changes onto the block in place')
  assert.notEqual(now.updatedAt, before, 'patch stamps a fresh updatedAt')
  await calendar.flush()
}

// Patching an id that is not loaded does nothing.
calendar.patch('not-a-real-id', { title: 'ignored' })
await calendar.flush()

// ── removeBlock ──────────────────────────────────────────────────────

{
  calendar.selection = { kind: 'block', id: block.id }
  const before = calendar.blocks.length
  await calendar.removeBlock(block.id)
  assert.equal(calendar.blocks.length, before - 1, 'the block is gone from the grid')
  assert.ok(
    !calendar.blocks.some((b) => b.id === block.id),
    'the removed block is not still loaded',
  )
  assert.equal(calendar.selection, null, 'removing the selected block clears the selection')
}

// ── where new events go ──────────────────────────────────────────────

{
  assert.equal(calendar.defaultCalendar, null, 'a fresh vault sends new events to this computer')
  const before = calendar.blocks.length
  await calendar.newEvent({ day: calendar.anchor, startMinutes: 10 * 60, minutes: 60 })
  assert.equal(
    calendar.blocks.length,
    before + 1,
    'with no default calendar, a new event is a block',
  )
  assert.equal(calendar.selection?.kind, 'block', 'and the new block is selected')
  assert.equal(calendar.draft, null, 'and nothing is left open as a draft')
}

const work = calendar.writableCalendars.find((c) => c.id === 'c-google-work')
assert.ok(work, 'the mock seeds a Google calendar that can be written to')

{
  await calendar.setDefaultCalendar(work.id)
  assert.equal(calendar.defaultCalendar?.id, work.id, 'the default moves to the account calendar')
  assert.equal(
    calendar.calendars.filter((c) => c.isDefault).length,
    1,
    'and exactly one calendar holds it',
  )

  const blocksBefore = calendar.blocks.length
  await calendar.newEvent({ day: calendar.anchor, startMinutes: 14 * 60, minutes: 30 })
  assert.equal(calendar.blocks.length, blocksBefore, 'a new account event writes nothing yet')
  assert.equal(calendar.selection?.kind, 'draft', 'it opens as a draft in the rail')
  assert.equal(calendar.draft?.calendarId, work.id, 'bound for the default calendar')
  assert.equal(
    Date.parse(calendar.draft.draft.end) - Date.parse(calendar.draft.draft.start),
    30 * 60_000,
    'covering the time that was dragged',
  )

  calendar.patchDraft({
    title: 'Planning with Sam',
    attendees: [{ email: 'sam@example.com', name: 'Sam' }],
  })
  const error = await calendar.saveDraft()
  assert.equal(error, null, 'saving the draft writes it to the account calendar')
  assert.equal(calendar.draft, null, 'the draft is let go once saved')
  assert.equal(calendar.selection?.kind, 'event', 'and the event it became is selected')
  const created = calendar.events.find((e) => e.id === calendar.selection.id)
  assert.equal(created?.title, 'Planning with Sam', 'with the title it was given')
  assert.equal(created?.calendarId, work.id, 'on the calendar it was bound for')
  assert.equal(JSON.stringify(created?.attendees), JSON.stringify(['Sam']), 'and its guest')
}

{
  // A refused save says why, and leaves everything typed where it was.
  await calendar.newEvent({ day: calendar.anchor, startMinutes: 16 * 60, minutes: 60 })
  calendar.patchDraft({ title: 'This will fail' })
  const error = await calendar.saveDraft()
  assert.ok(typeof error === 'string' && error.length > 0, 'a refused save answers why')
  assert.equal(calendar.draft?.draft.title, 'This will fail', 'and keeps the draft to be fixed')

  // Escape is `selection = null`; a draft does not outlive the selection.
  calendar.selection = null
  assert.equal(calendar.draft, null, 'dropping the selection throws the draft away')
}

{
  await calendar.setDefaultCalendar(null)
  assert.equal(calendar.defaultCalendar, null, 'this computer can be the default again')
  const before = calendar.blocks.length
  await calendar.newEvent({ day: calendar.anchor, startMinutes: 11 * 60, minutes: 45 })
  assert.equal(calendar.blocks.length, before + 1, 'and a new event is a block once more')
  assert.equal(calendar.selection?.kind, 'block', 'selected, as before')
}

// ── repeating blocks ─────────────────────────────────────────────────

{
  const head = calendar.blocks.find((b) => b.id === calendar.selection.id)
  const through = plusDays(head.localDate, 120)
  const members = async () =>
    (await api.blocks({ from: head.localDate, to: through }))
      .filter((b) => b.series?.id === head.id)
      .sort((a, b) => a.start.localeCompare(b.start))

  await calendar.repeatBlock(head.id, { frequency: 'daily', interval: 1 })
  const written = await members()
  assert.ok(written.length > 2, 'a daily repeat writes a block for each day ahead')
  assert.equal(written[0].id, head.id, 'headed by the block it was made from')
  assert.equal(written[1].localDate, plusDays(head.localDate, 1), 'the next on the next day')
  assert.ok(
    calendar.blocks.find((b) => b.id === head.id)?.series,
    'and the block on the grid knows it repeats',
  )

  const third = written[2]
  await calendar.deleteBlockSeries(third.id, 'following')
  const left = await members()
  assert.equal(left.length, 2, '"this and following" from the third leaves the first two')
  assert.ok(!calendar.blocks.some((b) => b.id === third.id), 'and takes the third off the grid')
}

await close()
console.log('calendar-store: all checks passed')

// `app.start()` arms the idle-vault poll; `calendar.start()` arms its own
// sync poll and clock on top of it -- see `editor.test.mjs`'s own note on
// why this process must be told to exit rather than left to hang.
process.exit(0)
