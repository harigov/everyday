// Behaviour checks for `screentime.svelte.ts` itself -- the class, not the
// pure helpers `screentime.test.mjs` covers.
//
// Driven through the real mock backend (`mock-screentime.ts`), the way
// `money-store.test.mjs` is: a fake `api` would be a second copy of what the
// mock already does, and what is worth checking here is the round trip --
// a navigation that loads what is on screen and only the newest answer, the
// label edit that shows at once and is put back when refused, the
// destructive act that asks first, and this machine's half answered by the
// shell rather than the vault.
//
// Written against the mock's seed: two machines (laptop, desktop), thirty
// days counted back from today, Google split by host, Hacker News renamed,
// the GNOME overview ignored, and Amazon with no category at all.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

const stored = new Map()

stubBrowser({
  window: globalThis,
  localStorage: {
    getItem: (key) => stored.get(key) ?? null,
    setItem: (key, value) => stored.set(key, String(value)),
    removeItem: (key) => stored.delete(key),
  },
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
  location: new URL('http://localhost/?unlocked=1'),
  createObjectURL: () => 'blob:stub',
})
Object.defineProperty(globalThis, 'navigator', {
  value: { userAgent: 'node', language: 'en-GB' },
  configurable: true,
})

const {
  modules: [storeModule, stateModule, apiModule, helpers, timeModule],
  close,
} = await load(
  [
    '/src/lib/screentime.svelte.ts',
    '/src/lib/state.svelte.ts',
    '/src/lib/api.ts',
    '/src/lib/screentime.ts',
    '/src/lib/time.ts',
  ],
  { svelte: true },
)
const { screentime, SCREEN_VIEWS } = storeModule
const { app } = stateModule
const { api, callCommand } = apiModule
const { parseConnectionCode, rangeOf } = helpers
const { addDays, localeWeekStart, todayIso } = timeModule

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

/** Wait for something that happens in the background, up to two seconds. */
async function until(what, condition) {
  for (let i = 0; i < 100; i++) {
    if (condition()) return
    await sleep(20)
  }
  assert.fail(`never happened: ${what}`)
}

/** Answer the question on screen: the first option is the yes. */
function answer(yes) {
  assert.ok(screentime.question, 'a question is on screen')
  if (yes) screentime.question.options[0].run()
  else screentime.question.cancel()
}

/** The report's numbers add up to its total, give or take a second each. */
function addsUp(report, what) {
  const slack = report.items.length + 24
  const near = (n) => Math.abs(n - report.totalSecs) <= slack
  assert.ok(near(report.items.reduce((n, i) => n + i.secs, 0)), `${what}: items`)
  assert.ok(near(report.categories.reduce((n, c) => n + c.secs, 0)), `${what}: categories`)
  assert.ok(near(report.hours.reduce((n, h) => n + h, 0)), `${what}: hours`)
  assert.ok(near(report.days.reduce((n, d) => n + d.secs, 0)), `${what}: days`)
  assert.ok(near(report.devices.reduce((n, d) => n + d.secs, 0)), `${what}: devices`)
}

const today = todayIso()
const week = rangeOf('week', today, localeWeekStart())

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')
assert.equal(app.supportsScreenTime, true, 'and carry screen time')
assert.equal(SCREEN_VIEWS.length, 4)

// ── start ────────────────────────────────────────────────────────────

app.section = 'screentime'
await screentime.start()
assert.equal(screentime.loaded, true)
assert.equal(screentime.view, 'summary', 'the summary is the pane it opens on')
assert.equal(screentime.range, 'day')
assert.equal(screentime.anchor, today, 'and today is the day')
assert.equal(screentime.rangeLabel, 'Today')
assert.equal(screentime.atPresent, true)
assert.equal(screentime.categories.length, 7, 'the starter categories are written on first read')
assert.ok(screentime.categories.some((c) => c.builtin === 'work' && c.name === 'Work'))
assert.equal(screentime.settings?.idleMinutes, 3)
assert.deepEqual(
  screentime.knownDevices.map((d) => d.device).sort(),
  ['desktop', 'laptop'],
  'both machines',
)
assert.ok(screentime.report, 'a report for today')
assert.equal(screentime.report.from, today)
assert.equal(screentime.report.hours.length, 24)
assert.equal(screentime.report.days.length, 1)
addsUp(screentime.report, 'today')
assert.equal(screentime.status?.backend, 'gnome', 'this machine is on GNOME')
assert.equal(screentime.status?.extension?.active, true, 'with the extension running')
assert.deepEqual(
  screentime.status?.clients.map((c) => c.collector),
  ['firefox', 'gnome'],
)
assert.equal(screentime.statusError, null)

// Colours: a category's own, a palette one for the one without, a neutral
// for none.
const work = screentime.categories.find((c) => c.builtin === 'work')
const utilities = screentime.categories.find((c) => c.builtin === 'utilities')
assert.equal(screentime.colorOf(work.id), work.color)
assert.match(screentime.colorOf(utilities.id), /^#[0-9a-f]{6}$/i, 'a palette colour')
assert.equal(screentime.colorOf(null), 'var(--fg-faint)')
assert.equal(screentime.categoryOf(work.id)?.name, 'Work')
assert.equal(screentime.categoryOf(null), undefined)

// ── ranges ───────────────────────────────────────────────────────────

screentime.setRange('week')
assert.equal(stored.get('everyday.screentime.range'), 'week', 'the range is remembered here')
assert.equal(screentime.from, week.from)
assert.equal(screentime.to, week.to)
await until('the week loads', () => screentime.report?.from === week.from)
assert.equal(screentime.report.days.length, 7, 'every day of the week, empty ones too')
addsUp(screentime.report, 'this week')

screentime.setRange('month')
await until('the month loads', () => screentime.report?.days.length > 27)
const month = screentime.report
addsUp(month, 'this month')
assert.ok(month.totalSecs > 0)

// The rules `report.rs` keeps, as the mock keeps them. Over the last
// thirty days rather than this month, which may be a day old.
const thirty = await api.screenTimeReport({
  from: addDays(today, -29),
  to: today,
  tz: '',
  devices: [],
})
addsUp(thirty, 'thirty days')
const items = new Map(thirty.items.map((i) => [i.key, i]))
assert.ok(
  thirty.items.some((i) => i.browserRest && (i.name === 'Firefox' || i.name === 'Google Chrome')),
  'a browser keeps only what no site claimed',
)
assert.ok(items.has('site:mail.google.com'), 'Google is split by host')
assert.ok(items.has('site:docs.google.com'))
assert.ok(!items.has('site:google.com'), 'and not counted together')
assert.equal(items.get('site:ycombinator.com')?.name, 'Hacker News', 'a renamed site')
assert.ok(!items.has('app:gnome-shell'), 'an ignored app is left out of every total')
assert.equal(items.get('site:amazon.com')?.category, null, 'Amazon has no category')
assert.equal(items.get('site:github.com')?.category, work.id, 'GitHub is suggested Work')
assert.equal(items.get('site:github.com')?.suggested, true, 'as a suggestion')
assert.equal(items.get('app:everyday')?.suggested, false, 'this app was filed by hand')
assert.ok(thirty.previousTotalSecs >= 0)
assert.deepEqual(thirty.devices.map((d) => d.device).sort(), ['desktop', 'laptop'])

// ── stepping ─────────────────────────────────────────────────────────

screentime.goToday()
screentime.setRange('week')
await until('back on this week', () => screentime.report?.from === week.from)
screentime.step(1)
assert.equal(screentime.from, week.from, 'nothing to step forward to from this week')
screentime.step(-1)
assert.equal(screentime.atPresent, false)
assert.equal(screentime.from, addDays(week.from, -7))
// Two quick steps back: only the newest answer may land.
screentime.step(-1)
await until('two weeks back', () => screentime.report?.from === addDays(week.from, -14))
await sleep(50)
assert.equal(screentime.report.from, addDays(week.from, -14), 'and an older answer does not')
screentime.step(1)
screentime.step(1)
assert.equal(screentime.atPresent, true)
assert.equal(screentime.anchor, today, 'stepping into this week lands on today')
await until('this week again', () => screentime.report?.from === week.from)

// ── narrowing ────────────────────────────────────────────────────────
//
// On last week, which always has something in it: this one may be an hour
// old.

screentime.step(-1)
await until('last week', () => screentime.report?.from === addDays(week.from, -7))
screentime.setDevices(['desktop'])
assert.equal(stored.get('everyday.screentime.devices'), '["desktop"]')
await until('narrowed to the desktop', () =>
  screentime.report?.devices.every((d) => d.device === 'desktop'),
)
screentime.setDevices([])
screentime.setCategory(work.id)
await until('narrowed to Work', () => screentime.report?.items.every((i) => i.category === work.id))
assert.match(screentime.showing.view, /narrowed to one category/)
screentime.setCategory(null)
await until('everything again', () => screentime.report?.items.some((i) => i.category !== work.id))
screentime.step(1)

// ── the timeline ─────────────────────────────────────────────────────

let day = today
for (let back = 0; back < 7; back++) {
  screentime.openTimeline(addDays(today, -back))
  day = addDays(today, -back)
  await until(`the timeline of ${day}`, () => screentime.timeline?.date === day)
  if (screentime.timeline.runs.length > 0) break
}
assert.equal(screentime.view, 'timeline')
assert.equal(stored.get('everyday.screentime.view'), 'timeline')
const runs = screentime.timeline.runs
assert.ok(runs.length > 0, 'a day this week has something on it')
for (let i = 1; i < runs.length; i++) {
  assert.ok(runs[i - 1].start <= runs[i].start, 'runs are in order')
}
assert.ok(
  runs.every((r) => r.start < r.end),
  'and none is empty',
)
assert.ok(
  runs.some((r) => r.title),
  'titles are kept by default',
)
screentime.openTimeline(addDays(today, 3))
assert.equal(screentime.timelineDate, today, 'no timeline of a day still to come')
screentime.setView('summary')

// ── labels ───────────────────────────────────────────────────────────

// A month that is mostly behind us, so it has a few weeks in it.
screentime.setRange('month')
screentime.anchor = addDays(today, -29)
await screentime.reload()
await until('a month with GitHub in it', () =>
  screentime.report?.items.some((i) => i.key === 'site:github.com'),
)
await screentime.ensureLabels()
assert.ok(screentime.apps.length > 20, 'the label list')
const github = () => screentime.report.items.find((i) => i.key === 'site:github.com')
const githubRow = () => screentime.apps.find((r) => r.key === 'site:github.com')
assert.equal(githubRow().label, null, 'GitHub has no label yet')
assert.equal(githubRow().defaultName, 'github.com')
assert.ok(githubRow().hosts.includes('github.com'))

const renaming = screentime.saveLabel('site:github.com', 'site', { name: 'GitHub' })
assert.equal(github().name, 'GitHub', 'a rename shows at once')
assert.equal(githubRow().label?.name, 'GitHub', 'on the list too')
await renaming
const saved = (await api.listScreenApps()).find((r) => r.key === 'site:github.com')
assert.equal(saved.label?.name, 'GitHub', 'and the vault kept it')
assert.match(saved.label.id, /^[0-9a-f-]{36}$/, 'under a fresh id')
await until('the report reads the label back', () => github()?.name === 'GitHub')
assert.equal(github().appId, saved.label.id)

// Filed elsewhere, then refused: put back as it was.
const learning = screentime.categories.find((c) => c.builtin === 'learning')
await screentime.saveLabel('site:github.com', 'site', { category: learning.id })
await until('GitHub is Learning', () => github()?.category === learning.id)
assert.equal(github().suggested, false, 'a choice, not a suggestion')
const refusing = screentime.saveLabel('site:github.com', 'site', { category: 'no-such-category' })
assert.equal(github().category, 'no-such-category', 'shown at once')
await refusing
assert.equal(github().category, learning.id, 'and put back when the vault refuses')
assert.equal(githubRow().label?.category, learning.id)

// None on purpose: no suggestion applies.
await screentime.saveLabel('site:github.com', 'site', { uncategorized: true })
await until('GitHub has no category', () => github()?.category === null)
assert.equal(githubRow().label?.category, null)

// Dropped: the collector's name and the suggestion again.
await screentime.clearLabel('site:github.com')
assert.equal(githubRow().label, null)
await until('GitHub as it was', () => github()?.name === 'github.com')
assert.equal(github().category, work.id)
assert.equal(github().suggested, true)

// A label on something the summary has not shown -- the list is read for it.
screentime.apps = []
await screentime.saveLabel('app:spotify', 'app', { ignored: true })
await until(
  'Spotify is ignored',
  () => !screentime.report?.items.some((i) => i.key === 'app:spotify'),
)
assert.equal(screentime.apps.find((r) => r.key === 'app:spotify')?.label?.ignored, true)
assert.ok(
  screentime.apps.some((r) => r.key === 'app:spotify'),
  'an ignored app is still listed, to be un-ignored',
)
await screentime.saveLabel('app:spotify', 'app', { ignored: false })
await until('Spotify counts again', () =>
  screentime.report?.items.some((i) => i.key === 'app:spotify'),
)

// ── forgetting ───────────────────────────────────────────────────────

assert.ok(screentime.report.items.some((i) => i.key === 'site:amazon.com'))
screentime.forgetItem('site:amazon.com', 'amazon.com')
assert.match(screentime.question?.title ?? '', /amazon\.com/)
answer(false)
await sleep(30)
assert.ok(
  screentime.report.items.some((i) => i.key === 'site:amazon.com'),
  'no is no',
)
screentime.forgetItem('site:amazon.com', 'amazon.com')
answer(true)
await until(
  'Amazon is forgotten',
  () => !screentime.report?.items.some((i) => i.key === 'site:amazon.com'),
)
assert.equal(screentime.question, null)
assert.equal(await api.forgetScreenItem('site:amazon.com'), 0, 'nothing of it is left')

// ── categories ───────────────────────────────────────────────────────

const made = await screentime.createCategory('  Shopping ', '#c2410c')
assert.ok(made, 'made')
assert.equal(made.name, 'Shopping')
assert.equal(screentime.sortedCategories.at(-1)?.id, made.id, 'last in the order')
assert.equal(await screentime.createCategory('   ', ''), null, 'a name is needed')
await screentime.moveCategory(made, -1)
const order = screentime.sortedCategories.map((c) => c.id)
assert.equal(order.at(-2), made.id, 'one place up')
assert.equal(order.at(-1), utilities.id, 'swapped with its neighbour')
const stored2 = await api.listScreenCategories()
assert.deepEqual(
  stored2.map((c) => c.id),
  order,
  'and both were saved',
)
await screentime.moveCategory(screentime.sortedCategories[0], -1)
assert.deepEqual(
  screentime.sortedCategories.map((c) => c.id),
  order,
  'the first cannot go higher',
)
await screentime.saveCategory({ ...screentime.categoryOf(made.id), name: 'Shops' })
assert.equal(screentime.categoryOf(made.id)?.name, 'Shops')

screentime.setCategory(made.id)
screentime.deleteCategory(screentime.categoryOf(made.id))
assert.match(screentime.question?.title ?? '', /Shops/)
answer(true)
await until('the category is gone', () => !screentime.categories.some((c) => c.id === made.id))
assert.equal(screentime.category, null, 'and the summary is no longer narrowed to it')

// ── settings ─────────────────────────────────────────────────────────

await screentime.saveSettings({ ...screentime.settings, idleMinutes: 5 })
assert.equal(screentime.settings.idleMinutes, 5)
assert.equal((await api.screenTimeSettings()).idleMinutes, 5)
await screentime.saveSettings({ ...screentime.settings, idleMinutes: 0 })
assert.equal(screentime.settings.idleMinutes, 5, 'a refusal reads back what is stored')

// ── this machine ─────────────────────────────────────────────────────

await screentime.setTracking(false)
assert.equal(screentime.status.tracking, false)
assert.equal(screentime.status.extension.active, false)
await screentime.setTracking(true)
assert.equal(screentime.status.tracking, true)
await screentime.setDevice('  work laptop ')
assert.equal(screentime.status.device, 'work laptop')
await screentime.setDevice('   ')
assert.equal(screentime.status.device, 'work laptop', 'an empty name changes nothing')
await screentime.installGnomeExtension()
assert.equal(screentime.status.extension.installed, true)
const connection = await screentime.connectBrowser('chrome')
assert.ok(connection, 'a connection')
assert.equal(connection.device, 'work laptop')
const parsed = parseConnectionCode(connection.code)
assert.equal(parsed?.endpoint, 'http://127.0.0.1:7399')
assert.equal(parsed?.device, 'work laptop')
assert.ok(parsed?.token, 'with a token in it')
assert.equal(await screentime.connectBrowser('not a browser!'), null, 'refused, not thrown')
await screentime.refreshStatus()
assert.equal(screentime.status.device, 'work laptop')

// ── live ─────────────────────────────────────────────────────────────

screentime.goToday()
screentime.setRange('day')
await until('today again', () => screentime.report?.from === today)
app.section = 'calendar'
assert.equal(screentime.spansArrived(), undefined, 'not while another app is open')
app.section = 'screentime'
screentime.anchor = addDays(today, -7)
assert.equal(screentime.spansArrived(), undefined, 'nor for a day that is over')
screentime.goToday()
const first = screentime.spansArrived()
assert.ok(first instanceof Promise, 'today on screen reads again')
await first
assert.equal(screentime.spansArrived(), undefined, 'but at most once a minute')

// ── a tracker counting screen time ───────────────────────────────────

await callCommand('save_tracker', {
  tracker: {
    id: 'tracker-screen-work',
    name: 'Work on screen',
    kind: 'amount',
    icon: '',
    color: '#0369a1',
    unit: '',
    defaultValue: 0,
    scaleMax: 0,
    onCalendar: false,
    source: { type: 'screenTime', category: work.id },
    targets: [],
  },
})
const trackerDays = await callCommand('tracker_days', {
  query: { trackerIds: ['tracker-screen-work'], from: addDays(today, -13), to: today },
})
assert.ok(trackerDays.length > 0, 'work days have minutes of Work')
assert.ok(
  trackerDays.every((d) => d.sum > 0 && Number.isInteger(d.sum) && d.date >= addDays(today, -13)),
  'whole minutes, within the range',
)

// ── deleting a range ─────────────────────────────────────────────────

const yesterday = addDays(today, -1)
screentime.setRange('day')
screentime.anchor = yesterday
await screentime.reload()
await until('yesterday', () => screentime.report?.from === yesterday)
const from = new Date(`${yesterday}T00:00:00`).toISOString()
const to = new Date(`${today}T00:00:00`).toISOString()
screentime.deleteRange(from, to, 'yesterday')
assert.match(screentime.question?.title ?? '', /yesterday/)
answer(true)
await until('yesterday is empty', () => screentime.report?.totalSecs === 0)
assert.equal(screentime.report.items.length, 0)

// ── a lock ───────────────────────────────────────────────────────────

screentime.setView('apps')
screentime.forgetItem('app:zoom', 'Zoom')
screentime.reset()
assert.equal(screentime.question, null, 'a question goes with the lock, answered no')
assert.equal(screentime.loaded, false)
assert.equal(screentime.report, null)
assert.equal(screentime.timeline, null)
assert.equal(screentime.apps.length, 0)
assert.equal(screentime.categories.length, 0)
assert.equal(screentime.status, null)
assert.equal(screentime.anchor, today, 'it comes back on today')
assert.equal(screentime.view, 'apps', 'but on the same pane, which is chrome')
await screentime.flush()

await close()
console.log('screentime-store: all checks passed')

// Same reason `money-store.test.mjs` ends like this: `app.start()` arms the
// idle-vault poll, which would otherwise hold this process open for ever.
process.exit(0)
