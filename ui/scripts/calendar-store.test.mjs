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

// ── moving an account calendar's events ──────────────────────────────
//
// Against the mock's Google "Work": a lunch with one guest, a weekly sync
// with three, and somebody else's review. A move is drawn at once, asks
// only when there is a series or a guest to ask about, and is written to
// the server -- the mock -- only once it is answered.

/** Wait, a macrotask at a time, for `test` to hold. */
async function until(test, what) {
  for (let i = 0; i < 500; i++) {
    if (test()) return
    await new Promise((r) => setTimeout(r, 0))
  }
  assert.fail(`timed out waiting for ${what}`)
}

/** Every Work event called `title`, earliest first, as the server has them. */
async function onWork(title) {
  const all = await api.events({ calendarId: 'c-google-work', limit: 1000 })
  return all.filter((e) => e.title === title).sort((a, b) => a.start.localeCompare(b.start))
}

/** Put the window on `event`'s day and answer it as the grid has it. */
async function open(event) {
  calendar.goto(event.localDate)
  await calendar.refresh()
  const shown = calendar.events.find((e) => e.id === event.id)
  assert.ok(shown, `${event.title} is on the grid once its day is shown`)
  return shown
}

const localToday = (() => {
  const now = new Date()
  return plusDays(`${now.getFullYear()}-${now.getMonth() + 1}-${now.getDate()}`, 0)
})()
const minutesIn = (ts) => new Date(ts).getHours() * 60 + new Date(ts).getMinutes()
const lengthOf = (e) => (Date.parse(e.end) - Date.parse(e.start)) / 60_000

{
  const [seeded] = await onWork('Lunch with Ana')
  const lunch = await open(seeded)
  const day = lunch.localDate
  assert.ok(calendar.canMove(lunch), 'an own event on a writable calendar can be moved')
  calendar.selection = { kind: 'event', id: lunch.id }

  // Dropped at two o'clock, and asked about first: Ana is told.
  const cancelled = calendar.moveEvent(lunch, day, 14 * 60)
  const slot = calendar.slotsOn(day).find((s) => s.event?.id === lunch.id)
  assert.equal(slot?.start, 14 * 60, 'it is drawn where it was dropped at once')
  assert.ok(slot.saving, 'drawn as on its way')
  assert.equal(calendar.canMove(slot.event), false, 'and cannot be picked up again meanwhile')
  await until(() => calendar.moveQuestion, 'the question about guests')
  assert.match(calendar.moveQuestion.detail, /Ana Ferreira is sent the new time/)
  assert.deepEqual(
    calendar.moveQuestion.options.map((o) => o.label),
    ['Move and notify guests'],
    'a one-off asks only whether to tell its guest',
  )
  calendar.moveQuestion.cancel()
  await cancelled
  assert.equal(calendar.moves.size, 0, 'Cancel puts it back')
  assert.equal((await onWork('Lunch with Ana'))[0].start, lunch.start, 'and writes nothing')

  const moving = calendar.moveEvent(lunch, day, 14 * 60)
  await until(() => calendar.moveQuestion, 'the question again')
  calendar.moveQuestion.options[0].run()
  await moving
  assert.equal(calendar.moveQuestion, null, 'answering takes the question down')
  const [moved] = await onWork('Lunch with Ana')
  assert.equal(minutesIn(moved.start), 14 * 60, 'the server has it at the new time')
  assert.equal(lengthOf(moved), 60, 'for as long as it was')
  assert.equal(calendar.moves.size, 0, 'and it is drawn from the server again')
  assert.equal(calendar.selection?.id, moved.id, 'still selected, under the id it came back as')

  // Stretched from its bottom edge: the same question, a longer lunch.
  const stretching = calendar.resizeEvent(calendar.selected, 90)
  await until(() => calendar.moveQuestion, 'the question for a resize')
  calendar.moveQuestion.options[0].run()
  await stretching
  const [longer] = await onWork('Lunch with Ana')
  assert.equal(minutesIn(longer.start), 14 * 60, 'a resize holds the start')
  assert.equal(lengthOf(longer), 90, 'and changes the length')
}

{
  // Somebody else's, as its sync says: not offered at all.
  const [seeded] = await onWork('Quarterly review')
  const review = await open(seeded)
  assert.equal(review.own, false, 'the sync says whose it is')
  assert.equal(calendar.canMove(review), false, 'so it is not offered as movable')

  // One whose sync did not say: the server is asked on the drop, and says.
  const unknown = { ...review, own: undefined }
  assert.ok(calendar.canMove(unknown), 'not knowing, it is offered')
  await calendar.moveEvent(unknown, review.localDate, 9 * 60)
  assert.equal(calendar.moveQuestion, null, 'somebody else’s meeting asks nothing')
  assert.equal((await onWork('Quarterly review'))[0].start, review.start, 'and stays put')
  assert.equal(calendar.canMove(unknown), false, 'and is not offered again')
}

{
  // A drag from a stale copy -- moved elsewhere since the last sync --
  // changes what the server has, not what this vault last saw.
  const [lunch] = await onWork('Lunch with Ana')
  const stale = {
    ...lunch,
    start: new Date(Date.parse(lunch.start) - 3_600_000).toISOString(),
    end: new Date(Date.parse(lunch.end) - 3_600_000).toISOString(),
  }
  const stretching = calendar.resizeEvent(stale, 30)
  await until(() => calendar.moveQuestion, 'the question for a stale resize')
  calendar.moveQuestion.options[0].run()
  await stretching
  const [after] = await onWork('Lunch with Ana')
  assert.equal(after.start, lunch.start, 'a resize keeps the start the server has')
  assert.equal(lengthOf(after), 30, 'at the new length')

  // And Edit waits for a move on its way: it would read the old times.
  const fresh = await open(after)
  const moving = calendar.moveEvent(fresh, fresh.localDate, 13 * 60)
  await calendar.editEvent(fresh.id)
  assert.equal(calendar.editing, null, 'no editor opens while it is moving')
  await until(() => calendar.moveQuestion, 'the question for the move')
  calendar.moveQuestion.cancel()
  await moving
}

{
  // One of a series, this occurrence only.
  const upcoming = (await onWork('Team sync')).filter((e) => e.localDate > localToday)
  const [first, second] = upcoming
  const sync = await open(first)
  const before = (await onWork('Team sync')).length
  const moving = calendar.moveEvent(sync, sync.localDate, 11 * 60)
  await until(() => calendar.moveQuestion, 'the series question')
  assert.match(calendar.moveQuestion.detail, /one of a series/)
  assert.match(calendar.moveQuestion.detail, /Everyone invited is sent the new time/)
  assert.deepEqual(
    calendar.moveQuestion.options.map((o) => o.label),
    ['This event', 'All events'],
  )
  calendar.moveQuestion.options[0].run()
  await moving
  const after = await onWork('Team sync')
  assert.equal(after.length, before, 'moving one occurrence keeps the series whole')
  assert.equal(minutesIn(after.find((e) => e.id === sync.id).start), 11 * 60, 'this one moved')
  assert.equal(minutesIn(after.find((e) => e.id === second.id).start), 10 * 60, 'the next did not')

  // All of them, a day later and an hour earlier: the weekday moves too.
  const next = await open(second)
  const all = calendar.moveEvent(next, plusDays(next.localDate, 1), 9 * 60)
  await until(() => calendar.moveQuestion, 'the series question again')
  calendar.moveQuestion.options[1].run()
  await all
  const shifted = await onWork('Team sync')
  const weekday = (new Date(second.start).getDay() + 1) % 7
  assert.ok(shifted.length > 2, 'the series is still a series')
  assert.ok(
    shifted.every((e) => new Date(e.start).getDay() === weekday && minutesIn(e.start) === 9 * 60),
    'every occurrence is on the next weekday, at nine',
  )
}

{
  // Two moves waiting at once ask one after the other, and both land.
  const [lunch] = await onWork('Lunch with Ana')
  const sync = (await onWork('Team sync')).find((e) => e.localDate > localToday)
  const first = calendar.moveEvent(lunch, lunch.localDate, 13 * 60)
  const second = calendar.moveEvent(sync, sync.localDate, 15 * 60)
  const asked = []
  for (const _ of [1, 2]) {
    await until(() => calendar.moveQuestion, 'the next question')
    const shown = calendar.moveQuestion
    asked.push(shown.title)
    shown.options[0].run()
    await until(() => calendar.moveQuestion !== shown, 'the question to go')
  }
  await Promise.all([first, second])
  assert.equal(asked.length, 2, 'each move is asked about')
  assert.ok(asked.some((t) => t.includes('Lunch')) && asked.some((t) => t.includes('Team sync')))
  assert.equal(calendar.moves.size, 0, 'and neither is left waiting')
  assert.equal(minutesIn((await onWork('Lunch with Ana'))[0].start), 13 * 60, 'lunch moved')
  const synced = (await onWork('Team sync')).find((e) => e.localDate === sync.localDate)
  assert.equal(minutesIn(synced.start), 15 * 60, 'and so did the sync')
}

{
  // With the editor open on one of a series, a drag to another day leaves
  // the repeat alone -- Save still asks -- and "all of them" carries it.
  const upcoming = (await onWork('Team sync')).filter((e) => e.localDate > localToday)
  const occurrence = await open(upcoming[1])
  await calendar.editEvent(occurrence.id)
  const before = JSON.stringify(calendar.editing.event.draft.recurrence)
  await calendar.moveEvent(occurrence, plusDays(occurrence.localDate, 1), 9 * 60)
  assert.equal(
    JSON.stringify(calendar.editing.draft.recurrence),
    before,
    'the repeat is as the server had it, so Save asks which is meant',
  )
  assert.equal(await calendar.saveEdit('series'), null, 'saving all of them works')
  const weekday = (new Date(occurrence.start).getDay() + 1) % 7
  assert.ok(
    (await onWork('Team sync')).every((e) => new Date(e.start).getDay() === weekday),
    'and the repeat followed it to the next day',
  )
}

{
  // A repeat that cannot follow a move to another day: Monday and
  // Wednesday, with its first dragged to Tuesday.
  const tz = Intl.DateTimeFormat().resolvedOptions().timeZone
  const monday = plusDays(localToday, (8 - new Date().getDay()) % 7 || 7)
  const at = (iso, h) => {
    const [y, m, d] = iso.split('-').map(Number)
    return new Date(y, m - 1, d, h).toISOString()
  }
  await api.createEvent('c-google-work', {
    title: 'Mon and Wed',
    description: '',
    location: '',
    start: at(monday, 8),
    end: at(monday, 9),
    allDay: false,
    tz,
    attendees: [],
    recurrence: { frequency: 'weekly', interval: 1, weekdays: ['monday', 'wednesday'] },
  })
  const [first] = await onWork('Mon and Wed')
  const twice = await open(first)
  const moving = calendar.moveEvent(twice, plusDays(twice.localDate, 1), 8 * 60)
  await until(() => calendar.moveQuestion, 'the series question')
  assert.deepEqual(
    calendar.moveQuestion.options.map((o) => o.label),
    ['This event'],
    'only this one is offered',
  )
  assert.match(calendar.moveQuestion.detail, /cannot follow it to another day/)
  calendar.moveQuestion.cancel()
  await moving

  // The same through the editor: "all of them" says why not.
  await calendar.editEvent(twice.id)
  await calendar.moveEvent(twice, plusDays(twice.localDate, 1), 8 * 60)
  const refused = await calendar.saveEdit('series')
  assert.match(refused ?? '', /cannot follow it to another day/, 'the editor is told why')
  assert.equal((await onWork('Mon and Wed'))[0].start, first.start, 'and nothing was written')

  // An editor that has made it all-day: the timed slot is not draggable.
  calendar.patchEdit({ allDay: true })
  assert.equal(calendar.canMove(twice), false, 'a timed drag means nothing to it now')
  calendar.cancelEdit()
}

{
  // An all-day event dropped two days on, in the band or the month.
  const tz = Intl.DateTimeFormat().resolvedOptions().timeZone
  const day = calendar.anchor
  const midnight = (iso) => {
    const [y, m, d] = iso.split('-').map(Number)
    return new Date(y, m - 1, d).toISOString()
  }
  await api.createEvent('c-google-work', {
    title: 'Offsite',
    description: '',
    location: '',
    start: midnight(day),
    end: midnight(plusDays(day, 1)),
    allDay: true,
    tz,
    attendees: [],
    recurrence: null,
  })
  await calendar.refresh()
  const offsite = calendar.allDayOn(day).find((e) => e.title === 'Offsite')
  assert.ok(offsite, 'the all-day event is in the band')
  calendar.moveToDay({ kind: 'event', id: offsite.id, from: day }, plusDays(day, 2))
  assert.ok(
    calendar.allDayOn(plusDays(day, 2)).some((e) => e.id === offsite.id),
    'drawn on its new day at once',
  )
  await until(() => calendar.moves.size === 0, 'the all-day move to land')
  const [landed] = await onWork('Offsite')
  assert.equal(landed.localDate, plusDays(day, 2), 'and on the server two days on')
  assert.equal(landed.allDay, true, 'still all day')
}

{
  // A block dropped on another day keeps its time of day.
  const block = await calendar.book({
    subject: { type: 'adhoc' },
    day: calendar.anchor,
    startMinutes: 8 * 60 + 15,
    minutes: 45,
    title: 'day-moved',
  })
  calendar.moveToDay(
    { kind: 'block', id: block.id, from: calendar.anchor },
    plusDays(calendar.anchor, 3),
  )
  const moved = calendar.blocks.find((b) => b.id === block.id)
  assert.equal(moved.localDate, plusDays(calendar.anchor, 3), 'a block moves by its day')
  assert.equal(minutesIn(moved.start), 8 * 60 + 15, 'at the same time')
  await calendar.flush()
}

{
  // With its editor open, a drag moves what is being edited; Save writes it.
  const [seeded] = await onWork('Lunch with Ana')
  const lunch = await open(seeded)
  await calendar.editEvent(lunch.id)
  assert.equal(calendar.editing?.status, 'ready', 'the editor has it')
  await calendar.moveEvent(lunch, lunch.localDate, 16 * 60)
  assert.equal(calendar.moveQuestion, null, 'nothing is asked: Save will')
  assert.equal(minutesIn(calendar.editing.draft.start), 16 * 60, 'the editor has the new time')
  assert.equal(
    calendar.slotsOn(lunch.localDate).find((s) => s.event?.id === lunch.id)?.start,
    16 * 60,
    'and the grid draws it there',
  )
  assert.equal((await onWork('Lunch with Ana'))[0].start, lunch.start, 'with nothing written yet')
  calendar.cancelEdit()
  assert.equal(
    calendar.slotsOn(lunch.localDate).find((s) => s.event?.id === lunch.id)?.start,
    minutesIn(lunch.start),
    'and cancelling the edit puts it back',
  )
}

await close()
console.log('calendar-store: all checks passed')

// `app.start()` arms the idle-vault poll; `calendar.start()` arms its own
// sync poll and clock on top of it -- see `editor.test.mjs`'s own note on
// why this process must be told to exit rather than left to hang.
process.exit(0)
