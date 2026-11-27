// Behaviour checks for the live-refresh router.
//
// Three rules, each of which fails quietly rather than loudly.
//
//   origin        a window must not reload for its own write, or the list
//                 moves under the cursor that caused it.
//
//   coalescing    a calendar sync writes a thousand events and each one
//                 announces itself. One reload, not a thousand.
//
//   dedupe        several kinds in one batch reload the *same* store -- a task
//                 and a project, an item and a log row -- and running it twice
//                 is a wasted round trip on a connection that may be a
//                 phone's. This was broken: the map held a separate arrow
//                 function per kind, so comparing them never matched.
//
// The module reaches into all four stores, so this checks the routing table
// rather than the stores: what matters is which target a kind asks for and how
// many distinct targets a batch produces.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

// The router imports the four stores, and they were written for a browser.
// Enough of one to let the modules evaluate; nothing below runs a reload, so
// nothing depends on the stubs being faithful. Same reasoning as
// `actions.test.mjs`.
stubBrowser({
  window: true,
  localStorage: true,
  matchMedia: true,
  document: true,
  navigator: true,
  location: true,
  createObjectURL: () => 'blob:stub',
})

const {
  modules: [
    live,
    feedModule,
    libraryModule,
    moneyModule,
    stateModule,
    mailModule,
    briefModule,
    mockJobsModule,
    appwindowModule,
  ],
  close,
} = await load(
  [
    '/src/lib/live.svelte.ts',
    '/src/lib/feed.svelte.ts',
    '/src/lib/library.svelte.ts',
    '/src/lib/money.svelte.ts',
    '/src/lib/state.svelte.ts',
    '/src/lib/mail.svelte.ts',
    '/src/lib/brief.svelte.ts',
    '/src/lib/mock-jobs.ts',
    '/src/lib/appwindow.ts',
  ],
  { svelte: true },
)
const { RELOAD, RELOADS, targetsFor } = live

// ── Every kind has an answer, and it is one that exists ───────────────

const KINDS = [
  'journal',
  'entry',
  'note',
  'template',
  'routine',
  'routineRun',
  'job',
  'proposal',
  'project',
  'task',
  'block',
  'calendar',
  'event',
  'shelf',
  'item',
  'log',
  'tracker',
  'reading',
  'area',
  'goal',
  'conversation',
  'memory',
  'skill',
  'settings',
  'account',
  'thread',
  'draft',
  'newsSource',
  'story',
  'queue',
  'consumption',
  'bankLink',
  'bankAccount',
  'txn',
  'moneyCategory',
  'moneyRule',
  'screenApp',
  'screenCategory',
  'screenSpan',
  'file',
  'album',
  'healthSource',
  'workout',
  'medication',
  'dose',
  'condition',
  'labResult',
  'healthDocument',
  'allergy',
  'food',
  'meal',
  'healthSample',
  'recording',
  'voiceprint',
  'transcript',
]

for (const kind of KINDS) {
  assert.ok(kind in RELOADS, `no routing for a change of kind ${kind}`)
  const target = RELOADS[kind]
  if (target === null) continue
  assert.ok(target in RELOAD, `${kind} asks for ${target}, which is not a reload`)
}

// ...and nothing routes to a target nobody declared.
for (const [kind, target] of Object.entries(RELOADS)) {
  assert.ok(KINDS.includes(kind), `${kind} is routed and is not a change kind`)
  assert.ok(target === null || target in RELOAD, `${kind} -> ${target}`)
}

// ── A batch collapses to one reload per store ─────────────────────────

// The regression this file exists for. A task and a project both reload the
// todo app; before the fix these were two different arrow literals and the
// dedupe compared them by identity, so both ran.
assert.deepEqual(targetsFor(['task', 'project']), ['todo'])
assert.deepEqual(targetsFor(['item', 'log']), ['library'])
assert.deepEqual(targetsFor(['block', 'calendar', 'event']), ['calendar'])
assert.deepEqual(targetsFor(['tracker', 'reading']), ['tracking'])
// The two purpose records. They are stored once and read in three places --
// the todo app's goals pane, every purpose picker, the Overview -- so one
// batch carrying both is one reload of the store that holds them.
assert.deepEqual(targetsFor(['area', 'goal']), ['purpose'])

// Distinct stores stay distinct.
assert.deepEqual(new Set(targetsFor(['task', 'item'])), new Set(['todo', 'library']))

// A kind that deliberately reloads nothing contributes nothing.
assert.deepEqual(targetsFor(['conversation', 'memory']), [])
assert.deepEqual(targetsFor(['conversation', 'task']), ['todo'])
assert.deepEqual(targetsFor(['transcript']), [])
// A kind the service raises that no command declares, so the generated
// `ChangeKind` and `RELOADS` do not name it: a supervised task's status when
// the vault unlocks, a sync pass's mailbox. It does not stop the rest of the
// batch from reloading -- it used to come back as an `undefined` target,
// called as a function. A supervised task's status reloads nothing; a folder
// or label a pass met for the first time reads the mailboxes again, which no
// list reload does.
assert.deepEqual(targetsFor(['backgroundTask']), [])
assert.deepEqual(targetsFor(['backgroundTask', 'mailbox']), ['mailboxes'])
assert.deepEqual(targetsFor(['backgroundTask', 'task']), ['todo'])
assert.deepEqual(new Set(targetsFor(['mailbox', 'thread'])), new Set(['mailboxes', 'mail']))
assert.equal(RELOAD.mailboxes(), undefined, 'nothing read before Mail is opened')

// What is followed and what it brought are one store's: a first refresh of a
// new source raises both, and the sidebar's counts and the list move
// together. A background pass raises a `story` change with no id -- "reload
// the list" -- so there is no applier, and the whole store is what reloads.
assert.deepEqual(targetsFor(['newsSource', 'story']), ['feed'])
assert.deepEqual(targetsFor(['story'], true), ['feed'], 'the Overview does not redraw for news')

// A bank's sync, a category renamed, a rule added: all one store's, which
// reloads what it has on screen. A sync raises its `txn` changes for a whole
// delta, so they collapse to one reload, and the Overview's charts -- which
// count hours and habits, not money -- are not dragged along.
assert.deepEqual(targetsFor(['bankLink', 'bankAccount', 'txn', 'moneyCategory', 'moneyRule']), [
  'money',
])
assert.deepEqual(targetsFor(['txn', 'txn', 'txn'], true), ['money'], 'a sync is one reload')

// Screen time: a label and a category are one reload of what is on screen;
// heartbeats -- a `screenSpan` change every half minute from every machine
// while anything is in use -- are a target of their own, held to once a
// minute and to today being on screen, so a burst of them is one call that
// usually does nothing. Neither drags the Overview along.
assert.deepEqual(targetsFor(['screenApp', 'screenCategory']), ['screentime'])
assert.deepEqual(targetsFor(['screenSpan', 'screenSpan'], true), ['screentimeSpans'])
assert.deepEqual(
  new Set(targetsFor(['screenApp', 'screenSpan'])),
  new Set(['screentime', 'screentimeSpans']),
)
// The app has not been opened here, so neither reads anything: a vault full
// of heartbeats costs nothing while somebody writes in the journal.
assert.equal(RELOAD.screentime(), undefined, 'no reload before Screen time is opened')
assert.equal(RELOAD.screentimeSpans(), undefined, 'nor for heartbeats')

// Files: a file arriving, changing, moving or going missing, and an album, are
// one store's, which reads what it has on screen. A scan raises a `file`
// change for every file it touches, so a batch of them is one reload; and
// the app has not been opened here, so it reads nothing.
assert.deepEqual(targetsFor(['file', 'album']), ['files'])
assert.deepEqual(targetsFor(['file', 'file', 'file'], true), ['files'], 'a scan is one reload')
assert.equal(RELOAD.files(), undefined, 'no reload before Files is opened')
// Health: every record it keeps is one reload of what is on screen, so a
// meal and a dose written together are one call. Samples -- a phone's hourly
// push, an import's batches, never with an id -- are a target of their own,
// held to once a minute, and neither drags the Overview along.
assert.deepEqual(
  targetsFor([
    'healthSource',
    'workout',
    'medication',
    'dose',
    'condition',
    'labResult',
    'healthDocument',
    'allergy',
    'food',
    'meal',
  ]),
  ['health'],
)
assert.deepEqual(targetsFor(['healthSample', 'healthSample'], true), ['healthSamples'])
// ...but a step goal's meter moves with them, while the todo app is showing it.
assert.deepEqual(
  new Set(targetsFor(['healthSample'], false, true)),
  new Set(['healthSamples', 'goalTargets']),
  'steps arriving move a goal meter',
)
assert.deepEqual(
  new Set(targetsFor(['dose', 'healthSample'])),
  new Set(['health', 'healthSamples']),
)
// The app has not been opened here, so neither reads anything: a phone
// pushing every hour costs nothing while somebody writes in the journal.
assert.equal(RELOAD.health(), undefined, 'no reload before Health is opened')
assert.equal(RELOAD.healthSamples(), undefined, 'nor for samples')

// A job moving -- queued, running, a new progress line, done -- reloads the
// jobs store alone, and does not drag the routines with it: the two are
// listed together in Settings but are separate records with separate loads.
assert.deepEqual(targetsFor(['job']), ['jobs'])
assert.deepEqual(targetsFor(['job', 'job']), ['jobs'], 'a burst of progress is one reload')
assert.deepEqual(
  new Set(targetsFor(['job', 'routineRun'])),
  new Set(['jobs', 'assistant']),
  'each store reloads its own list',
)

// A recording and a voiceprint share the one `meetings` target -- a call
// finishing (or failing) in the background, and a voice learned or forgotten,
// both just mean "re-read the lists `meetings.svelte.ts` owns".
assert.deepEqual(targetsFor(['recording', 'voiceprint']), ['meetings'])

// ── A goal's meters are read off blocks and the library ──────────────
//
// A time target counts the hours filed under a goal, and a books target the
// library's log, so an hour logged on another machine moves a meter on this
// one while the event says only "block". Refreshed only while the todo app,
// where the meters are drawn, is on screen.
assert.deepEqual(targetsFor(['block']), ['calendar'], 'the todo app is not showing')
assert.deepEqual(
  new Set(targetsFor(['block'], false, true)),
  new Set(['calendar', 'goalTargets']),
  'an hour logged elsewhere moves a goal meter',
)
assert.deepEqual(new Set(targetsFor(['log'], false, true)), new Set(['library', 'goalTargets']))
assert.deepEqual(targetsFor(['memory'], false, true), [], 'nothing a meter reads')

// ── A purpose changes the reports without naming them ────────────────
//
// A purpose is a pointer on a task, a block, an entry, a shelf item and a
// tracker, and it inherits -- so filing one task under a goal changes every
// chart the Overview draws, while the change event says only "task".
//
// The Overview therefore reloads for those kinds, but only while it is the app
// on screen: doing it always would redraw a year of balance on every
// autosave.
assert.deepEqual(targetsFor(['task']), ['todo'], 'the Overview is not showing')
assert.deepEqual(
  new Set(targetsFor(['task'], true)),
  new Set(['todo', 'overview']),
  'a task filed under a goal changes the charts',
)
// And a kind that cannot carry one does not drag the Overview along.
assert.deepEqual(targetsFor(['memory'], true), [])
assert.deepEqual(targetsFor(['journal'], true), ['journals'])
// A goal reloads the store that holds it whether or not the Overview is up,
// because the todo app draws the same list -- and reloads the Overview's own
// numbers as well when it is the thing on screen.
assert.deepEqual(targetsFor(['goal'], false), ['purpose'])
assert.deepEqual(
  new Set(targetsFor(['goal'], true)),
  new Set(['purpose', 'overview']),
  'a goal moving changes every chart drawn from it',
)
// One reload, not two, when a batch has both kinds in it.
assert.deepEqual(
  targetsFor(['goal', 'task'], true).filter((t) => t === 'overview'),
  ['overview'],
)

// A whole sync's worth of kinds is still a handful of reloads, not a hundred.
const everything = targetsFor([...KINDS, ...KINDS, ...KINDS], true)
assert.equal(new Set(everything).size, everything.length, 'a target ran twice')
assert.ok(everything.length <= Object.keys(RELOAD).length)

// What the shell opens in the main window from a card on the panel: each
// kind it names opens in its own app, and a kind it does not know -- or a
// name that happens to be an Object.prototype property -- opens nothing.
const { appForItem } = live
assert.equal(appForItem({ kind: 'thread', id: 't' }), 'mail')
assert.equal(appForItem({ kind: 'note', id: 'n' }), 'notes')
assert.equal(appForItem({ kind: 'task', id: 'k' }), 'todo')
assert.equal(appForItem({ kind: 'event', id: 'e' }), 'calendar')
assert.equal(appForItem({ kind: 'constructor', id: 'x' }), null)
assert.equal(appForItem({ kind: 'routine', id: 'x' }), null)
assert.equal(appForItem(null), null)

// ── An app off screen is marked stale, not read ──────────────────────
//
// A Feed pass lands about once a minute, and a list of hundreds read again
// for a window showing something else is wasted: the app is marked stale and
// reads in full when it comes back. The same for a window hidden in the
// tray. Up Next's half of a story change still runs; it is guarded itself.
{
  const { feed } = feedModule
  const { library } = libraryModule
  const { money } = moneyModule
  const { app } = stateModule
  const read = []
  feed.refreshSources = async () => void read.push('feed sources')
  feed.reload = async () => void read.push('feed list')
  library.refresh = async () => void read.push('library')
  money.reload = async () => void read.push('money')

  app.section = 'mail'
  await RELOAD.feed()
  await RELOAD.library()
  await RELOAD.money()
  assert.deepEqual(read, [], 'nothing off screen is read')
  assert.equal(feed.stale, true, 'the Feed reads in full when it comes back')
  assert.equal(library.stale, true, 'so does the Library')
  assert.equal(money.stale, true, 'and Money')

  app.section = 'feed'
  await RELOAD.feed()
  assert.deepEqual(read, ['feed sources', 'feed list'], 'on screen, both halves')

  read.length = 0
  globalThis.document.visibilityState = 'hidden'
  await RELOAD.feed()
  assert.deepEqual(read, [], 'not in a window hidden in the tray')
  globalThis.document.visibilityState = 'visible'

  app.section = 'library'
  await RELOAD.library()
  assert.deepEqual(read, ['library'])

  // Changes made while the stream was down were never announced, so every
  // held app reads in full on its way back.
  feed.stale = false
  library.stale = false
  live.live.setStream(false)
  live.live.setStream(true)
  assert.equal(feed.stale, true, 'missed while the stream was down')
  assert.equal(library.stale, true)
  app.section = 'journal'
}

// A job's progress and a run finishing are their stores' own to patch: the
// router does not reload the lists for them.
{
  const { planBatch } = live
  const job = { kind: 'job', op: 'updated', id: 'job-1', origin: null }
  const run = { kind: 'routineRun', op: 'updated', id: 'run-1', origin: null }
  assert.deepEqual(planBatch([job]).refreshed, [], "a job change is its store's")
  assert.deepEqual(planBatch([run]).refreshed, [], "a run change is the assistant's")
  const mailbox = { kind: 'mailbox', op: 'updated', origin: null }
  assert.deepEqual(planBatch([mailbox]).refreshed, ['mailboxes'], 'a new folder: the mailboxes')
}

// ── What arrives on the stream ───────────────────────────────────────
//
// Through the router itself rather than its tables: a sync pass's `mailbox`
// change reaches the mail store past the kinds no command declares, and this
// window's own routine writes -- a brief run removed from "What it did", a
// brief switched on by this window's assistant -- reach the briefs, which a
// visit no longer reads on a desktop window.
{
  const { mockRaiseChange } = mockJobsModule
  const { briefs } = briefModule
  const { mail } = mailModule
  const { app } = stateModule
  const { ownOrigin } = appwindowModule
  const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms))
  let briefReads = 0
  let mailboxReads = 0
  briefs.refresh = async () => void briefReads++
  mail.mailboxesChanged = () => void mailboxReads++
  app.section = 'journal'
  app.screen = 'main'
  live.live.start()

  mockRaiseChange({ kind: 'backgroundTask', op: 'updated', origin: null })
  mockRaiseChange({ kind: 'mailbox', op: 'updated', origin: null })
  mockRaiseChange({ kind: 'mailbox', op: 'updated', origin: null })
  await wait(400)
  assert.equal(mailboxReads, 1, "a pass's new folder reads the mailboxes again, once a batch")

  const removed = { kind: 'routineRun', op: 'deleted', id: 'run-1', origin: ownOrigin }
  mockRaiseChange(removed)
  assert.equal(briefReads, 0, 'not before anything has read the briefs')
  briefs.loaded = true
  mockRaiseChange(removed)
  assert.equal(briefReads, 1, "this window's own routine write reads the briefs again")
  mockRaiseChange({ kind: 'task', op: 'updated', id: 'task-1', origin: ownOrigin })
  assert.equal(briefReads, 1, 'and nothing else of its own does')

  mockRaiseChange({ kind: 'mailbox', op: 'updated', origin: ownOrigin })
  assert.equal(mailboxReads, 2, "a label this window's assistant made is read too")
  briefs.loaded = false
  app.screen = 'loading'
}

await close()
console.log('live: all checks passed')
