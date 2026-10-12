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
// and a failed one started by hand), and against its script, which is sped
// up with `mockJobsTempo` so that a whole job takes under a second. Every
// job started here is finished or cancelled before the file ends, so
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
  modules: [jobsModule, stateModule, apiModule, mockJobs],
  close,
} = await load(
  [
    '/src/lib/jobs.svelte.ts',
    '/src/lib/state.svelte.ts',
    '/src/lib/api.ts',
    '/src/lib/mock-jobs.ts',
  ],
  { svelte: true },
)
const { jobs } = jobsModule
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
  ['job-1', 'job-2'],
  'the mock has a finished job and a failed one, newest first',
)
assert.equal(jobs.unseen, 1, 'only the finished one has not been looked at')
assert.equal(jobs.byId('job-1')?.status, 'done')
assert.equal(jobs.byId('job-2')?.status, 'failed')
assert.match(jobs.byId('job-2').reason, /503/, 'a failed job says why')
assert.equal(jobs.byId('nowhere'), undefined)
assert.deepEqual(jobs.active, [], 'nothing is queued or running')

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
assert.equal(jobs.jobs.length, 2, 'and the list did not')
await api.cancelJob(hidden.id)

// ── refresh ──────────────────────────────────────────────────────────

await jobs.refresh()
assert.deepEqual(ids(jobs.jobs), [hidden.id, 'job-1', 'job-2'], 'newest first')
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
  assert.equal(jobs.jobs.length, 3, 'the stale answer is dropped')
}

// ── startResearch ────────────────────────────────────────────────────

// Fast enough to watch a whole job in under a second.
mockJobsTempo(0.02)

assert.equal(await jobs.startResearch('   '), null, 'an empty question starts nothing')
assert.equal(jobs.jobs.length, 3)

const started = await jobs.startResearch('  What suits a small boat?  ', 'c-ask-1')
assert.ok(started, 'it answers with the job')
assert.equal(started.status, 'queued', 'queued, not run: the worker picks it up')
assert.equal(started.kind.question, 'What suits a small boat?', 'trimmed')
assert.equal(started.askedIn, 'c-ask-1')
assert.equal(jobs.jobs[0].id, started.id, 'it goes to the top of the list at once')
assert.deepEqual(ids(jobs.forConversation('c-ask-1')), [started.id])
assert.deepEqual(ids(jobs.active), [started.id])

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
assert.deepEqual(jobs.active, [], 'a finished job is not active')

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
assert.deepEqual(new Set(ids(jobs.active)), new Set([running.id, waiting.id]))

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
assert.deepEqual(jobs.active, [])

// Cancelling what is already over is the person's mistake, said once.
app.error = null
await jobs.cancel(waiting.id)
assert.match(app.error ?? '', /already cancelled/, 'refused, and reported')
assert.equal(jobs.byId(waiting.id).status, 'cancelled', 'and nothing changed')
app.error = null

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
