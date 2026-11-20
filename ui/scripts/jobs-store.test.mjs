// Behaviour checks for `jobs.svelte.ts` itself -- the class, not the cards
// that draw it.
//
// Driven through the real mock backend (`mock-jobs.ts`), the same way
// `feed-store.test.mjs` is: a fake `api` would be a second copy of what the
// mock already does, and what is worth checking here is the round trip --
// the count that has to agree with the list, the cancel that has to leave
// the note, the reload that must not be undone by an older one.
//
// Written against the mock's seed (a finished job asked from a conversation
// and a failed one started by hand, a project still running, a watch asleep
// between looks and one that has stopped), and against its script, which is
// sped up with `mockJobsTempo` so that a whole job takes under a second.
// Every job started here is finished or cancelled before the file ends, so
// nothing is left to run behind the next assertion.

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
  modules: [jobsModule, stateModule, apiModule, mockJobs, formatModule, timeModule],
  close,
} = await load(
  [
    '/src/lib/jobs.svelte.ts',
    '/src/lib/state.svelte.ts',
    '/src/lib/api.ts',
    '/src/lib/mock-jobs.ts',
    '/src/lib/format.ts',
    '/src/lib/time.ts',
  ],
  { svelte: true },
)
const {
  jobs,
  isLive,
  isWatch,
  isLookingNow,
  nextLookText,
  everyText,
  untilText,
  notifyText,
  troubleText,
  sentToText,
} = jobsModule
const { app } = stateModule
const { api, onChange } = apiModule
const { mockJobsTempo, mockStartResearch } = mockJobs

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

/**
 * Wait for something the script does on its own, reloading the store each
 * time round -- which is what a `job` change event makes the real one do.
 */
async function until(what, condition) {
  for (let i = 0; i < 150; i++) {
    await jobs.refresh()
    if (condition()) return
    await sleep(20)
  }
  assert.fail(`never happened: ${what}`)
}

const ids = (list) => list.map((j) => j.id)

// The seeded jobs that are still going: a project that is running and a watch
// asleep between looks. Neither belongs to a script, so what a check about the
// jobs it *started* wants is the live ones minus these.
const SEEDED_LIVE = ['job-3', 'job-4']
const live = () => ids(jobs.active).filter((id) => !SEEDED_LIVE.includes(id))

// What the router would hear, to check the script says what it is doing.
const changes = []
onChange((change) => changes.push(change))

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')
assert.equal(app.supportsJobs, true, 'and carry the jobs capability')
assert.equal(jobs.enabled, true)

// ── start ────────────────────────────────────────────────────────────

assert.deepEqual(jobs.jobs, [], 'nothing is loaded before it is asked for')
const first = jobs.start()
assert.equal(jobs.start(), first, 'asking twice while loading waits for the first')
await first
assert.equal(jobs.loading, false, 'loading settles back to false once the load lands')
assert.deepEqual(
  ids(jobs.jobs),
  ['job-3', 'job-4', 'job-1', 'job-5', 'job-2'],
  'the mock has a running project, a watch asleep, a finished job, a stopped watch and a failed one',
)
assert.equal(jobs.unseen, 1, 'only the finished one has not been looked at')
assert.equal(jobs.byId('job-1')?.status, 'done')
assert.equal(jobs.byId('job-2')?.status, 'failed')
assert.match(jobs.byId('job-2').reason, /503/, 'a failed job says why')
assert.equal(jobs.byId('nowhere'), undefined)
assert.deepEqual(
  ids(jobs.active),
  SEEDED_LIVE,
  'the project is running and the watch is asleep, which is still going',
)

// ── forConversation ──────────────────────────────────────────────────

assert.deepEqual(
  ids(jobs.forConversation('c-job-ask')),
  ['job-1'],
  'a conversation has the jobs it started',
)
assert.deepEqual(jobs.forConversation('c-elsewhere'), [])
assert.deepEqual(jobs.forConversation(''), [], 'a job started by hand belongs to no conversation')

// ── refreshCount ─────────────────────────────────────────────────────

// Started behind the store's back, so that only the count can have seen it.
const hidden = await api.startResearch('Only the count should hear about this')
await jobs.refreshCount()
assert.equal(jobs.unseen, 2, 'the count moved')
assert.equal(jobs.jobs.length, 5, 'and the list did not')
await api.cancelJob(hidden.id)

// ── refresh ──────────────────────────────────────────────────────────

await jobs.refresh()
assert.deepEqual(
  ids(jobs.jobs),
  [hidden.id, 'job-3', 'job-4', 'job-1', 'job-5', 'job-2'],
  'newest first',
)
assert.equal(jobs.byId(hidden.id).status, 'cancelled')
assert.equal(jobs.unseen, 2)

// An older reload landing after a newer one must not put the list back. A job
// writes often while it runs and every write is a reload, so two are
// routinely in the air at once.
{
  const real = api.jobs
  let calls = 0
  api.jobs = async (query) => {
    const mine = ++calls
    const rows = await real(query)
    if (mine > 1) return rows
    await sleep(120)
    return []
  }
  const older = jobs.refresh()
  const newer = jobs.refresh()
  await Promise.all([older, newer])
  api.jobs = real
  assert.equal(jobs.jobs.length, 6, 'the stale answer is dropped')
}

// ── startResearch ────────────────────────────────────────────────────

// Fast enough to watch a whole job in under a second.
mockJobsTempo(0.02)

assert.equal(await jobs.startResearch('   '), null, 'an empty question starts nothing')
assert.equal(jobs.jobs.length, 6)

const started = await jobs.startResearch('  What suits a small boat?  ', 'c-ask-1')
assert.ok(started, 'it answers with the job')
assert.equal(started.status, 'queued', 'queued, not run: the worker picks it up')
assert.equal(started.kind.question, 'What suits a small boat?', 'trimmed')
assert.equal(started.askedIn, 'c-ask-1')
assert.equal(jobs.jobs[0].id, started.id, 'it goes to the top of the list at once')
assert.deepEqual(ids(jobs.forConversation('c-ask-1')), [started.id])
assert.deepEqual(live(), [started.id])

// It runs to its end by itself, and says what it is doing on the way.
await until('the job starts', () => ['running', 'done'].includes(jobs.byId(started.id)?.status))
assert.ok(jobs.byId(started.id).conversationId, 'a running job has a transcript')
assert.equal(
  jobs.byId(started.id).outputs.length,
  1,
  'and its note, from the first moment rather than the last',
)
await until('the job finishes', () => jobs.byId(started.id)?.status === 'done')
const done = jobs.byId(started.id)
assert.equal(done.steps, 7)
assert.equal(done.progress, 'Wrote up 3 sources')
assert.equal(done.seen, false, 'finishing is news again')
assert.equal(done.attempts, 1)
assert.ok(done.startedAt && done.finishedAt)
assert.deepEqual(live(), [], 'a finished job is not active')

// The note, which is where the findings are.
const output = done.outputs[0]
assert.equal(output.type, 'note')
const note = await api.note(output.noteId)
assert.equal(note.title, 'What suits a small boat?', 'titled after the question')
assert.ok(note.tags.includes('research'))
assert.match(JSON.stringify(note.body), /What is known/, 'written up by the end')

// Coming back: a line saying so, then the answer, in the conversation that asked.
await until('the answer comes back', () => jobs.byId(started.id)?.answered === true)
const thread = await api.conversationMessages('c-ask-1')
assert.deepEqual(
  thread.map((m) => m.role),
  ['system', 'assistant'],
  'the application speaks first, and the assistant answers',
)
assert.equal(thread[0].content, 'Research finished: What suits a small boat?')
assert.match(thread[1].content, /What suits a small boat\?/, 'and names the note it made')

// Its own transcript: the prompt, seven calls with their results, what it said.
const transcript = await api.conversationMessages(done.conversationId)
assert.equal(transcript.length, 16)
assert.equal(transcript[0].role, 'user')
assert.match(transcript[0].content, /Question: What suits a small boat\?/)
assert.equal(transcript.filter((m) => m.toolCalls.length > 0).length, 7)
const history = await api.conversations(50)
assert.ok(
  !history.some((c) => c.id === done.conversationId),
  "a job's transcript stays out of the rail's history",
)
assert.ok(
  history.some((c) => c.id === 'c-job-ask'),
  'the thread that asked is in it',
)

// What it said to the router along the way.
assert.ok(changes.some((c) => c.kind === 'job' && c.op === 'created' && c.id === started.id))
assert.ok(changes.some((c) => c.kind === 'job' && c.op === 'updated' && c.id === started.id))
assert.ok(changes.some((c) => c.kind === 'note' && c.id === output.noteId))
assert.ok(changes.some((c) => c.kind === 'conversation' && c.id === 'c-ask-1'))
assert.ok(
  changes.every((c) => c.origin && c.origin !== 'local'),
  "the router ignores `local`, which is a window's own write; a job is nobody's",
)

// ── cancel ───────────────────────────────────────────────────────────

// Slower, so the second job is still waiting when the first is stopped.
mockJobsTempo(0.05)
const running = await jobs.startResearch('Stop me part of the way through', 'c-order')
const waiting = await jobs.startResearch('I wait my turn', 'c-order')
assert.deepEqual(ids(jobs.forConversation('c-order')), [waiting.id, running.id], 'newest first')
await until('the first starts', () => (jobs.byId(running.id)?.steps ?? 0) >= 1)
assert.equal(jobs.byId(running.id).status, 'running')
assert.equal(jobs.byId(waiting.id).status, 'queued', 'one job at a time: the second waits')
assert.deepEqual(new Set(live()), new Set([running.id, waiting.id]))

await jobs.cancel(running.id)
const stopped = jobs.byId(running.id)
assert.equal(stopped.status, 'cancelled', 'cancelling updates the list at once')
assert.equal(stopped.reason, 'cancelled by you')
assert.ok(stopped.finishedAt)
assert.equal(stopped.answered, false, 'a cancelled job does not come back')
const kept = await api.note(stopped.outputs[0].noteId)
assert.match(JSON.stringify(kept.body), /What it has read so far/, 'whatever it wrote is kept')

// It really stopped, and the worker moved on to the one that was waiting.
const steps = stopped.steps
await until('the next one starts', () => jobs.byId(waiting.id)?.status === 'running')
assert.equal(jobs.byId(running.id).steps, steps, 'no further steps after a cancel')
assert.equal(jobs.byId(running.id).status, 'cancelled')

await jobs.cancel(waiting.id)
assert.equal(jobs.byId(waiting.id).status, 'cancelled')
assert.deepEqual(live(), [])

// Cancelling what is already over is the person's mistake, said once.
app.error = null
await jobs.cancel(waiting.id)
assert.match(app.error ?? '', /already cancelled/, 'refused, and reported')
assert.equal(jobs.byId(waiting.id).status, 'cancelled', 'and nothing changed')
app.error = null

// ── watches and projects ─────────────────────────────────────────────

// What still counts as going. A watch spends nearly all its life asleep, and
// anything keyed on "queued or running" would have called it over.
for (const [status, going] of [
  ['queued', true],
  ['running', true],
  ['waiting', true],
  ['done', false],
  ['failed', false],
  ['cancelled', false],
]) {
  assert.equal(isLive({ status }), going, `${status} is ${going ? '' : 'not '}live`)
}

const project = jobs.byId('job-3')
assert.equal(project.kind.type, 'project')
assert.equal(project.status, 'running')
assert.equal(project.updates.length, 1, 'a project says what it has told them so far')
assert.equal(project.updates[0].delivered, true)
assert.deepEqual(project.updates[0].sentTo, ['thread', 'chat'])
assert.equal(project.rules, undefined, 'a project has no rules, and the record leaves them off')

const watch = jobs.byId('job-4')
assert.equal(watch.kind.type, 'watch')
assert.equal(watch.kind.probe.type, 'connection')
assert.equal(watch.kind.probe.connection, 'GitHub')
assert.equal(watch.status, 'waiting')
assert.equal(isWatch(watch), true)
assert.equal(isWatch(project), false)
assert.equal(isWatch(jobs.byId('job-1')), false)
assert.equal(watch.rules.notifyWhen, 'whenever a run finishes or fails')
assert.ok(Date.parse(watch.rules.until) > Date.now(), 'it stops in the future')
assert.ok(Date.parse(watch.wakeAt) > Date.now(), 'and next looks soon')
assert.equal(isLookingNow(watch), false)
assert.equal(isLookingNow(watch, Date.parse(watch.wakeAt) + 1), true, 'due is looking')
assert.equal(watch.observed.checks, 14)
assert.equal(watch.updates.length, 2)
assert.ok(
  Date.parse(watch.updates[0].at) < Date.parse(watch.updates[1].at),
  'oldest first, so the last is the newest',
)

const ended = jobs.byId('job-5')
assert.equal(ended.kind.probe.type, 'assistant')
assert.equal(ended.status, 'done')
assert.equal(ended.answered, true, 'a watch said what it had to as it went: nothing is owed')
assert.equal(isLive(ended), false)

// Only the ones still looking, soonest first.
assert.deepEqual(ids(jobs.watching), ['job-4'], 'a watch that has stopped is not watching')

// A watch asleep past its time has looked again by the time it is read.
{
  const realNow = Date.now
  const before = await api.job('job-4')
  Date.now = () => realNow() + 4 * 60_000
  const after = await api.job('job-4')
  Date.now = realNow
  assert.equal(after.observed.checks, before.observed.checks + 1, 'one look was missed')
  assert.ok(Date.parse(after.wakeAt) > Date.parse(before.wakeAt), 'and the next is later')
  assert.equal(after.status, 'waiting', 'it is still asleep')
}

// The words a watch's card is made of.
{
  const at = Date.parse('2026-11-20T12:00:00Z')
  assert.equal(nextLookText('2026-11-20T12:03:00Z', at), 'Next look in 3 min')
  assert.equal(nextLookText('2026-11-20T12:00:20Z', at), 'Next look in under a minute')
  assert.equal(nextLookText('2026-11-20T11:59:00Z', at), 'Looking now', 'past is looking')
  assert.equal(nextLookText('2026-11-20T13:30:00Z', at), 'Next look in 1h 30m')
  assert.equal(nextLookText(null, at), 'Waiting for its next look')

  assert.equal(everyText(5), '5 min')
  assert.equal(everyText(0), '5 min', 'zero is the default')
  assert.equal(everyText(undefined), '5 min')
  assert.equal(everyText(60), '1 hour')
  assert.equal(everyText(120), '2 hours')
  assert.equal(everyText(90), '1h 30m')

  // The app's own clock and date words, so the test holds in any locale:
  // what is checked is which day is said, not how this machine writes 18:40.
  const clock = (d) => formatModule.timeOfDay(d)
  const noon = new Date(2026, 10, 20, 12, 0)
  const evening = new Date(2026, 10, 20, 18, 40)
  const tomorrow = new Date(2026, 10, 21, 7, 5)
  const later = new Date(2026, 10, 25, 9, 30)
  assert.equal(untilText(evening.toISOString(), noon), `Until ${clock(evening)}`)
  assert.equal(untilText(tomorrow.toISOString(), noon), `Until tomorrow ${clock(tomorrow)}`)
  const day = formatModule.friendlyDate(timeModule.isoDate(later), noon)
  assert.equal(untilText(later.toISOString(), noon), `Until ${day}, ${clock(later)}`)
  assert.equal(untilText(null, noon), '', 'no end, nothing to say')
  assert.equal(untilText('not a time', noon), '')

  assert.equal(notifyText('when it finishes or fails'), 'when it finishes or fails')
  assert.equal(notifyText('  '), 'any change worth knowing', 'nothing said is any change')
  assert.equal(notifyText(undefined), 'any change worth knowing')
  assert.equal(troubleText(1, ''), 'The last look failed')
  assert.equal(
    troubleText(3, 'no answer from GitHub'),
    'The last 3 looks failed: no answer from GitHub',
  )

  assert.equal(sentToText({ delivered: false }), 'Not sent yet')
  assert.equal(sentToText({ delivered: true, sentTo: ['chat'] }), 'Sent to your chat')
  assert.equal(
    sentToText({ delivered: true, sentTo: ['thread', 'chat', 'voice'] }),
    'Sent to the thread, your chat and the voice session',
  )
  assert.equal(
    sentToText({ delivered: true, sentTo: ['thread', 'held'] }),
    'Sent to the thread · To be spoken when quiet hours end',
  )
  assert.equal(sentToText({ delivered: true }), '', 'sent nowhere in particular says nothing')
}

// Stopping a watch: it stops looking, and the card is over.
await jobs.cancel('job-4')
{
  const ended = jobs.byId('job-4')
  assert.equal(ended.status, 'cancelled')
  assert.equal(ended.reason, 'cancelled by you')
  assert.ok(!ended.wakeAt, 'asleep no longer')
  assert.equal(ended.updates.length, 2, 'what it told them stays')
  assert.equal(isLive(ended), false)
  assert.deepEqual(ids(jobs.watching), [], 'nothing is watching now')
}

// ── markSeen ─────────────────────────────────────────────────────────

await jobs.refresh()
const unseenBefore = jobs.jobs.filter((j) => !j.seen).length
assert.ok(unseenBefore > 2, 'several jobs are waiting to be looked at')
assert.equal(jobs.unseen, unseenBefore, 'and the count agrees with the list')

const sent = []
{
  const real = api.markJobsSeen
  api.markJobsSeen = (list) => {
    sent.push(list)
    return real(list)
  }
}

await jobs.markSeen(['job-1'])
assert.deepEqual(sent, [['job-1']], 'named ids are sent as they are')
assert.equal(jobs.byId('job-1').seen, true, 'and the row is marked at once')
assert.equal(jobs.unseen, unseenBefore - 1, 'the count is read back')

const rest = jobs.jobs.filter((j) => !j.seen).map((j) => j.id)
await jobs.markSeen()
assert.equal(sent.length, 2)
assert.deepEqual(new Set(sent[1]), new Set(rest), 'no ids: every unseen job that is loaded')
assert.ok(jobs.jobs.every((j) => j.seen))
assert.equal(jobs.unseen, 0, 'looking at them clears the number')

// Nothing left to mark is no call at all. An empty list means *every* job to
// the service, including one that finished while the list was being read.
await jobs.markSeen()
await jobs.markSeen([])
assert.equal(sent.length, 2, 'never asks the service to mark nothing')

// ── answer ───────────────────────────────────────────────────────────

mockJobsTempo(0.02)
// Started past the store, to leave the follow-up undone.
const held = mockStartResearch('Left unanswered', '', 'c-ask-2', true)
await until('the held job finishes', () => jobs.byId(held.id)?.status === 'done')
assert.equal(jobs.byId(held.id).answered, false, 'finished, and not yet answered')
assert.deepEqual(
  (await api.conversationMessages('c-ask-2')).map((m) => m.role),
  ['system'],
  'the line is there and the answer is not',
)
await jobs.answer(held.id)
assert.equal(jobs.byId(held.id).answered, true, 'answered now, and the card can tell')
assert.deepEqual(
  (await api.conversationMessages('c-ask-2')).map((m) => m.role),
  ['system', 'assistant'],
)

// A job that has not finished has nothing to answer.
app.error = null
await jobs.answer('job-2')
assert.match(app.error ?? '', /has not finished/)
app.error = null

// ── remove ───────────────────────────────────────────────────────────

await jobs.remove('job-2')
assert.ok(!jobs.byId('job-2'), 'a deleted job leaves the list')
await assert.rejects(api.job('job-2'), { code: 'not_found' }, 'and the vault')
assert.ok(await api.note('n-job-2'), 'its note stays')
assert.deepEqual(await api.conversationMessages('c-job-2'), [], 'its transcript goes')

// ── load ─────────────────────────────────────────────────────────────

jobs.reset()
assert.deepEqual(jobs.jobs, [])
assert.equal(jobs.unseen, 0)
assert.equal(jobs.loading, false)

assert.equal((await jobs.load('job-1'))?.id, 'job-1', 'one job, fetched fresh')
assert.deepEqual(ids(jobs.jobs), ['job-1'])
await jobs.load(started.id)
assert.deepEqual(ids(jobs.jobs), [started.id, 'job-1'], 'put where its age says, not on top')
const again = await jobs.load('job-1')
assert.equal(jobs.jobs.length, 2, 'loading one that is there replaces it')
assert.equal(again.id, 'job-1')

assert.equal(await jobs.load('job-2'), null, 'one that is gone is null')
assert.equal(app.error, null, 'and nothing worth a banner')

// ── a vault without jobs ─────────────────────────────────────────────

jobs.reset()
const status = app.status
app.status = { ...status, capabilities: { ...status.capabilities, jobs: false } }
assert.equal(jobs.enabled, false)
await jobs.start()
await jobs.refresh()
await jobs.refreshCount()
assert.deepEqual(jobs.jobs, [], 'nothing is asked of a backend that has none')
assert.equal(await jobs.startResearch('Is anybody there?'), null)
assert.equal(await jobs.load('job-1'), null)
app.status = status

await close()
console.log('jobs-store: all checks passed')

// The same reason `feed-store.test.mjs` ends like this: unlocking arms the
// app's idle-vault poll, which would otherwise hold this process open for
// ever after every check has passed.
process.exit(0)
