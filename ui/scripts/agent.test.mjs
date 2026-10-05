// Behaviour checks for the assistant panel's two loops.
//
// Same reasoning as `notify.test.mjs` and `menu.test.mjs`: the type checker
// covers most of the interface, and what it cannot cover is the modules made
// of rules. `agent.ts` has three, and each of their failures is quiet.
//
// Folding a stream of events into a turn is the first. A card that opens and
// never closes is a spinner nobody can stop. A confirmed call drawn twice is
// a transcript that says two things happened when one did. A tool result
// matched to the wrong call attaches "deleted the deck" to the card that
// listed your tasks. None of those fail to compile, and none of them throw.
//
// Replaying a stored thread is the second, and is the same hazard from the
// other end: four stored roles become two drawn ones, and a result has to
// find the call it answers across that fold.
//
// The third is one line -- whether a base URL is this machine -- and decides
// whether the panel offers a composer or asks for an API key.
//
// No test framework, deliberately: one dependency-free file, run by
// `npm run check`, with the TypeScript loaded through Vite so it compiles
// exactly as the application compiles it.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const {
  modules: [agent, svelteInternal],
  close,
} = await load(['/src/lib/agent.ts', 'svelte/internal/client'])
const {
  activity,
  applyEvent,
  elapsed,
  emptyTurn,
  gerund,
  isLoopback,
  liveTurn,
  pastTense,
  planOf,
  replay,
  settle,
  threadGroups,
  toolLabel,
  workSummary,
} = agent

// ── folding a stream into a turn ──────────────────────────────────────

// Prose arrives in pieces and has to end up in one piece, in order.
{
  const turn = emptyTurn('assistant', 'pending')
  applyEvent(turn, { type: 'started', messageId: 'msg-1' })
  for (const text of ['Added ', 'it', '.']) applyEvent(turn, { type: 'delta', text })
  applyEvent(turn, { type: 'finished', messageId: 'msg-1' })

  assert.equal(turn.id, 'msg-1', 'the placeholder id is replaced by the stored one')
  assert.equal(turn.text, 'Added it.')
  assert.equal(turn.error, null)
}

// A tool that runs opens a card and closes it, and the summary is what the
// card ends up showing.
{
  const turn = emptyTurn('assistant', 'x')
  applyEvent(turn, {
    type: 'toolStarted',
    callId: 'c1',
    name: 'list_tasks',
    arguments: { open_only: true },
  })
  assert.equal(turn.cards.length, 1)
  assert.equal(turn.cards[0].state, 'running')

  applyEvent(turn, {
    type: 'toolFinished',
    callId: 'c1',
    name: 'list_tasks',
    ok: true,
    summary: '3 results',
  })
  assert.equal(turn.cards[0].state, 'done')
  assert.equal(turn.cards[0].summary, '3 results')
}

// A failed tool is drawn as failed rather than as a result. The interface
// must not have to read the summary to work out which it was.
{
  const turn = emptyTurn('assistant', 'x')
  applyEvent(turn, { type: 'toolStarted', callId: 'c1', name: 'update_task', arguments: {} })
  applyEvent(turn, {
    type: 'toolFinished',
    callId: 'c1',
    name: 'update_task',
    ok: false,
    summary: 'no task with that id',
  })
  assert.equal(turn.cards[0].state, 'failed')
}

// A confirmed destructive call is ONE card that changes, not two cards.
// Drawing it twice would be a transcript claiming the delete happened twice.
{
  const turn = emptyTurn('assistant', 'x')
  applyEvent(turn, {
    type: 'confirmationRequired',
    callId: 'c9',
    name: 'delete_task',
    subject: 'Order the timber',
    arguments: {},
  })
  assert.equal(turn.cards.length, 1)
  assert.equal(turn.cards[0].state, 'waiting')
  assert.equal(turn.cards[0].subject, 'Order the timber')

  applyEvent(turn, { type: 'toolStarted', callId: 'c9', name: 'delete_task', arguments: {} })
  assert.equal(turn.cards.length, 1, 'the approved call reuses the card that asked')
  assert.equal(turn.cards[0].state, 'running')
  assert.equal(turn.cards[0].subject, 'Order the timber', 'and keeps what it was about')
}

// A card carries whether it may offer "later", from the event that raised
// it -- `false` for something with no proposal form, such as the search
// taint, which the backend never sets it true for.
{
  const turn = emptyTurn('assistant', 'x')
  applyEvent(turn, {
    type: 'confirmationRequired',
    callId: 'c1',
    name: 'delete_task',
    subject: 'Order the timber',
    arguments: {},
    kind: 'destructive',
    canPark: true,
  })
  assert.equal(turn.cards[0].canPark, true)

  applyEvent(turn, {
    type: 'confirmationRequired',
    callId: 'c2',
    name: 'web_search',
    subject: 'alice@example.com',
    arguments: {},
    kind: 'search',
    canPark: false,
  })
  assert.equal(turn.cards[1].canPark, false)
}

// docs/plans/dreaming.md's Phase 5: parking a call as a proposal is a third
// answer beside confirm and decline, and the card that says so must keep
// saying so. The backend still fires a `toolFinished` for the skipped call
// -- the model has to be told why -- and that must not flip a card that has
// already told the person "saved for later" into one that reads "failed".
// The same holds for an ordinary decline, which used to make exactly this
// mistake: a fast enough reply drew "declined" for an instant and then
// silently became "failed".
{
  const turn = emptyTurn('assistant', 'x')
  applyEvent(turn, {
    type: 'confirmationRequired',
    callId: 'c1',
    name: 'delete_task',
    subject: 'Order the timber',
    arguments: {},
    kind: 'destructive',
    canPark: true,
  })
  // What the panel does the moment the person clicks "later" -- see
  // `agent.svelte.ts`'s own `confirm` -- set here directly because this
  // file tests `applyEvent` in isolation.
  turn.cards[0].state = 'later'

  applyEvent(turn, {
    type: 'toolFinished',
    callId: 'c1',
    name: 'delete_task',
    ok: false,
    summary: 'Not done. It has been saved as a proposal for the person to decide later...',
  })
  assert.equal(turn.cards[0].state, 'later', 'the answer already given is not overwritten')

  const declined = emptyTurn('assistant', 'y')
  applyEvent(declined, {
    type: 'confirmationRequired',
    callId: 'd1',
    name: 'delete_task',
    subject: 'Order the timber',
    arguments: {},
    kind: 'destructive',
    canPark: true,
  })
  declined.cards[0].state = 'declined'
  applyEvent(declined, {
    type: 'toolFinished',
    callId: 'd1',
    name: 'delete_task',
    ok: false,
    summary: 'The person declined this.',
  })
  assert.equal(declined.cards[0].state, 'declined', 'nor is a decline')
}

// Two calls in one turn is ordinary. Their results must not be swapped:
// this is the check that stops "deleted" landing on the card that listed.
{
  const turn = emptyTurn('assistant', 'x')
  applyEvent(turn, { type: 'toolStarted', callId: 'a', name: 'list_tasks', arguments: {} })
  applyEvent(turn, { type: 'toolStarted', callId: 'b', name: 'create_task', arguments: {} })
  applyEvent(turn, {
    type: 'toolFinished',
    callId: 'b',
    name: 'create_task',
    ok: true,
    summary: 'created task Ring the vet',
  })

  assert.equal(turn.cards[0].summary, '', 'the first call has not reported yet')
  assert.equal(turn.cards[1].summary, 'created task Ring the vet')
  assert.equal(turn.cards[0].state, 'running')
  assert.equal(turn.cards[1].state, 'done')
}

// A result for a call nobody opened is dropped rather than inventing a card.
{
  const turn = emptyTurn('assistant', 'x')
  applyEvent(turn, { type: 'toolFinished', callId: 'ghost', name: 'x', ok: true, summary: 'hi' })
  assert.equal(turn.cards.length, 0)
}

// A failure is terminal and is drawn on the turn, not on a card.
{
  const turn = emptyTurn('assistant', 'x')
  applyEvent(turn, { type: 'failed', message: 'The API key was refused.' })
  assert.equal(turn.error, 'The API key was refused.')
}

// Nothing may still be open once the turn is over: the run has gone, so a
// spinner would never stop and a question would have nobody listening.
{
  const turn = emptyTurn('assistant', 'x')
  applyEvent(turn, { type: 'toolStarted', callId: 'a', name: 'list_tasks', arguments: {} })
  applyEvent(turn, {
    type: 'confirmationRequired',
    callId: 'b',
    name: 'delete_task',
    subject: 'x',
    arguments: {},
  })
  applyEvent(turn, { type: 'toolStarted', callId: 'c', name: 'create_task', arguments: {} })
  applyEvent(turn, {
    type: 'toolFinished',
    callId: 'c',
    name: 'create_task',
    ok: true,
    summary: '',
  })

  settle(turn)
  assert.deepEqual(
    turn.cards.map((c) => c.state),
    ['failed', 'failed', 'done'],
    'running and waiting cards are closed; a finished one is left alone',
  )
}

// ── folding into a turn that lives in a $state array ──────────────────
//
// The check above this one runs `applyEvent` on plain objects, and that is
// exactly why it missed the bug this replaces: `$state` arrays store a
// *proxy* of what is pushed, so a caller that keeps the reference it pushed
// mutates an object nothing is watching. The panel showed an empty reply
// for every turn -- no prose, no tool cards, and therefore no confirm
// buttons on a destructive call.
//
// `proxy` is Svelte's own, from the copy this interface builds against, so
// this checks the real behaviour rather than a model of it.
{
  const { proxy } = svelteInternal

  const turns = proxy([])
  turns.push(emptyTurn('assistant', 'local-1'))

  // Read the way the panel reads, so the proxy has cached its sources --
  // which is the step that made the old bug invisible on a fresh object.
  assert.equal(turns[0].text, '')
  assert.equal(turns[0].cards.length, 0)

  // The reference the store must use is the one read back out, not the one
  // it pushed.
  const reply = turns[turns.length - 1]
  assert.notEqual(reply, undefined)

  applyEvent(reply, { type: 'started', messageId: 'msg-7' })
  applyEvent(reply, { type: 'delta', text: 'Added it.' })
  applyEvent(reply, {
    type: 'confirmationRequired',
    callId: 'c1',
    name: 'delete_task',
    subject: 'Order the timber',
    arguments: {},
  })

  assert.equal(turns[0].text, 'Added it.', 'streamed prose must reach the array the panel reads')
  assert.equal(turns[0].cards.length, 1, 'and so must a card, or its buttons never appear')
  assert.equal(turns[0].id, 'msg-7')

  // And the failure mode itself, so this cannot regress quietly: mutating
  // the pushed reference instead updates nothing.
  const raw = emptyTurn('assistant', 'local-2')
  turns.push(raw)
  assert.equal(turns[1].text, '')
  applyEvent(raw, { type: 'delta', text: 'invisible' })
  assert.equal(turns[1].text, '', 'mutating the pushed reference is exactly the bug')
}

// Local ids are unique, because the panel keys its `{#each}` on them and two
// turns sharing a key is a runtime error that takes the render down. A turn
// keeps its local id whenever a request fails before the backend names it.
{
  const seen = new Set()
  for (const turn of [emptyTurn('user', 'local-1'), emptyTurn('assistant', 'local-2')]) {
    assert.ok(!seen.has(turn.id), 'ids must not repeat')
    seen.add(turn.id)
  }
}

// ── replaying a stored thread ─────────────────────────────────────────

// A declaration rather than an arrow returning an object literal: an arrow
// whose body is `({...})` immediately followed by a bare block confuses the
// TypeScript parser eslint runs over this file, and every check below is
// inside a bare block.
function msg(over) {
  return {
    id: 'm',
    conversationId: 'c',
    role: 'user',
    content: '',
    toolCalls: [],
    toolCallId: null,
    failed: false,
    createdAt: '2026-09-08T00:00:00Z',
    ...over,
  }
}

// Four stored roles become two drawn ones, and the tool result finds the
// call it answers across that fold.
{
  const turns = replay([
    msg({ id: 'm1', role: 'user', content: 'add a task to ring the vet' }),
    msg({
      id: 'm2',
      role: 'assistant',
      content: '',
      toolCalls: [{ id: 'call_1', name: 'create_task', arguments: { title: 'Ring the vet' } }],
    }),
    msg({ id: 'm3', role: 'tool', content: 'created task Ring the vet', toolCallId: 'call_1' }),
    msg({ id: 'm4', role: 'assistant', content: 'Added it.' }),
  ])

  assert.deepEqual(
    turns.map((t) => t.role),
    ['user', 'assistant', 'assistant'],
    'tool turns fold onto the assistant turn that asked for them',
  )
  assert.equal(turns[1].cards.length, 1)
  assert.equal(turns[1].cards[0].state, 'done')
  assert.equal(turns[1].cards[0].summary, 'created task Ring the vet')
  assert.equal(turns[2].text, 'Added it.')
}

// A stored tool turn that failed is replayed as failed. The flag is on the
// record precisely so the interface never has to read the prose to decide.
{
  const turns = replay([
    msg({
      id: 'm1',
      role: 'assistant',
      toolCalls: [{ id: 'call_1', name: 'delete_task', arguments: {} }],
    }),
    msg({
      id: 'm2',
      role: 'tool',
      content: 'no task with that id',
      toolCallId: 'call_1',
      failed: true,
    }),
  ])
  assert.equal(turns[0].cards[0].state, 'failed')
}

// A call with no stored result is drawn as done, not as running -- a run
// interrupted mid-flight must not leave a spinner in the history forever.
{
  const turns = replay([
    msg({
      id: 'm1',
      role: 'assistant',
      toolCalls: [{ id: 'call_1', name: 'create_task', arguments: {} }],
    }),
  ])
  assert.equal(turns[0].cards[0].state, 'done')
}

// A tool turn with no assistant turn before it is dropped rather than drawn
// floating, and the application's own notes are never drawn at all.
{
  const turns = replay([
    msg({ id: 'm1', role: 'tool', content: 'orphan', toolCallId: 'call_x' }),
    msg({ id: 'm2', role: 'system', content: 'the vault was locked' }),
    msg({ id: 'm3', role: 'user', content: 'hello' }),
  ])
  assert.deepEqual(
    turns.map((t) => t.id),
    ['m3'],
  )
}

assert.deepEqual(replay([]), [], 'an empty thread replays to nothing')

// ── whether a model is on this machine ────────────────────────────────

for (const url of [
  'http://localhost:11434/v1',
  'http://LocalHost:11434/v1',
  'http://127.0.0.1:1234/v1',
  'http://127.10.0.2:8080',
  'http://[::1]:8080/v1',
]) {
  assert.ok(isLoopback(url), `${url} runs on this machine`)
}

for (const url of [
  'https://api.openai.com/v1',
  'https://openrouter.ai/api/v1',
  // The check is on the host, not on the string: a remote host that merely
  // mentions localhost is somebody else's server.
  'https://localhost.example.com/v1',
  'not a url',
]) {
  assert.ok(!isLoopback(url), `${url} does not`)
}

assert.ok(!isLoopback(null), 'no override means the provider default, which is remote')
assert.ok(!isLoopback(''), 'and an empty one is not an override at all')

// ── saying what a running turn is doing ───────────────────────────────
//
// The status line is the one thing a long turn shows the whole time it
// runs, and every way it can be wrong is quiet: stuck on "Searching" after
// the search came back, saying nothing in the gap after a tool while the
// model reads what it returned, or still talking after the turn has ended.

{
  const turn = liveTurn('t', 1000)
  assert.equal(turn.phase, 'thinking', 'a reply that has not started is thinking')
  assert.equal(activity(turn), 'Thinking', 'and says so, before any event has arrived')

  applyEvent(turn, { type: 'thinking', text: 'They want the weekend. ' })
  assert.equal(turn.thinking, 'They want the weekend. ', 'reasoning is kept apart from the reply')
  assert.equal(turn.text, '', 'and never leaks into it')

  applyEvent(turn, { type: 'toolPreparing', callId: 'c1', name: 'web_search' })
  assert.equal(activity(turn), 'Deciding what to search for', 'a call still being written')

  applyEvent(turn, {
    type: 'toolStarted',
    callId: 'c1',
    name: 'web_search',
    arguments: { query: 'lisbon events' },
  })
  assert.equal(turn.preparing, null, 'a call that has started is no longer being prepared')
  assert.equal(activity(turn), 'Searching the web for “lisbon events”')

  applyEvent(turn, {
    type: 'toolFinished',
    callId: 'c1',
    name: 'web_search',
    ok: true,
    summary: '',
  })
  assert.equal(
    activity(turn),
    'Thinking',
    'after a result comes back the model is reading it -- the gap that used to show nothing',
  )

  applyEvent(turn, { type: 'delta', text: 'Here' })
  assert.equal(activity(turn), 'Writing')
  applyEvent(turn, { type: 'thinking', text: 'more' })
  assert.equal(activity(turn), 'Writing', 'prose on screen outranks a late scrap of reasoning')

  settle(turn, 4000)
  assert.equal(activity(turn), null, 'a settled turn says nothing')
  assert.equal(turn.endedAt, 4000, 'and is off the clock')
  assert.equal(workSummary(turn), 'Worked for 3s · 1 step')
}

// Two calls in flight: the line names the newest still running, and goes
// back to thinking only once neither is.
{
  const turn = liveTurn('t')
  applyEvent(turn, { type: 'toolStarted', callId: 'a', name: 'list_tasks', arguments: {} })
  applyEvent(turn, { type: 'toolStarted', callId: 'b', name: 'get_weather', arguments: {} })
  assert.equal(activity(turn), 'Checking the weather where you live')
  applyEvent(turn, {
    type: 'toolFinished',
    callId: 'b',
    name: 'get_weather',
    ok: true,
    summary: '',
  })
  assert.equal(turn.phase, 'tool', 'one is still running')
  assert.equal(activity(turn), 'Listing tasks')
  applyEvent(turn, { type: 'toolFinished', callId: 'a', name: 'list_tasks', ok: true, summary: '' })
  assert.equal(activity(turn), 'Thinking')
}

// A question outranks everything: the turn is waiting on the person.
{
  const turn = liveTurn('t')
  applyEvent(turn, {
    type: 'confirmationRequired',
    callId: 'f',
    name: 'read_web_page',
    subject: 'https://example.org/?q=secret',
    arguments: { url: 'https://example.org/?q=secret' },
    kind: 'fetch',
    canPark: false,
  })
  assert.equal(activity(turn), 'Waiting for your answer')
  assert.equal(turn.cards[0].confirmKind, 'fetch')
}

// A failure ends the turn, so nothing goes on saying "Thinking".
{
  const turn = liveTurn('t')
  applyEvent(turn, { type: 'failed', message: 'refused' })
  assert.equal(activity(turn), null)
}

// Stop: a card still running is drawn as stopped, not as failed -- nothing
// went wrong, somebody asked it to stop -- and a result that straggles in
// afterwards does not flip it back.
{
  const turn = liveTurn('t')
  applyEvent(turn, { type: 'toolStarted', callId: 'r', name: 'read_web_page', arguments: {} })
  turn.stopped = true
  settle(turn)
  assert.equal(turn.cards[0].state, 'stopped')
  applyEvent(turn, {
    type: 'toolFinished',
    callId: 'r',
    name: 'read_web_page',
    ok: true,
    summary: '',
  })
  assert.equal(turn.cards[0].state, 'stopped', 'a late result does not overwrite the stop')
}

// ── what a card calls itself ─────────────────────────────────────────

// Derived from the name, in the tense of the card's state.
check('list_tasks', 'running', 'Listing tasks')
check('list_tasks', 'done', 'Listed tasks')
check('create_note', 'running', 'Creating note')
check('create_note', 'done', 'Created note')
check('delete_task', 'waiting', 'Delete task')
check('log_reading', 'done', 'Logged reading')
check('get_transcript', 'done', 'Got transcript')
check('remember', 'running', 'Remembering')
check('label_thread', 'done', 'Labelled thread')
check('snooze_thread', 'running', 'Snoozing thread')

function check(name, state, want) {
  assert.equal(toolLabel(name, {}, state).text, want, `${name} while ${state}`)
}

assert.equal(gerund('stop'), 'stopping')
assert.equal(gerund('tie'), 'tying')
assert.equal(pastTense('apply'), 'applied')
assert.equal(pastTense('plan'), 'planned')
assert.equal(pastTense('send'), 'sent')

// The tools that reach outside the vault say what they reached for.
assert.deepEqual(toolLabel('web_search', { query: 'rain' }, 'done'), {
  text: 'Searched the web for',
  detail: 'rain',
})
assert.deepEqual(
  toolLabel('read_web_page', { url: 'https://www.bbc.co.uk/news/x' }, 'running'),
  { text: 'Reading', detail: 'bbc.co.uk' },
  'a page is named by its host, without the www',
)
assert.deepEqual(toolLabel('get_weather', { place: 'Porto' }, 'running'), {
  text: 'Checking the weather in',
  detail: 'Porto',
})
// Not a web tool, but it still earns a detail: the name alone -- "Reading
// skill" -- would lose which skill was actually followed.
assert.deepEqual(toolLabel('read_skill', { name: 'Plan a trip' }, 'running'), {
  text: 'Following',
  detail: 'Plan a trip',
})
assert.deepEqual(toolLabel('read_skill', { name: 'Plan a trip' }, 'done'), {
  text: 'Followed',
  detail: 'Plan a trip',
})
// Arguments are whatever the model wrote. Nothing in them may throw.
for (const junk of [null, 'a string', 7, { query: 3 }, { url: 'not a url' }]) {
  toolLabel('web_search', junk, 'running')
  toolLabel('read_web_page', junk, 'done')
  toolLabel('get_weather', junk, 'waiting')
  toolLabel('read_skill', junk, 'waiting')
}

// ── the plan ─────────────────────────────────────────────────────────

{
  const turn = liveTurn('t')
  assert.equal(planOf(turn), null, 'no plan until one is made')
  const steps = (...statuses) => statuses.map((status, i) => ({ text: `Step ${i + 1}`, status }))
  applyEvent(turn, {
    type: 'toolStarted',
    callId: 'p1',
    name: 'update_plan',
    arguments: { steps: steps('active', 'pending') },
  })
  applyEvent(turn, {
    type: 'toolStarted',
    callId: 'p2',
    name: 'update_plan',
    arguments: { steps: steps('done', 'active') },
  })
  assert.deepEqual(
    planOf(turn).map((s) => s.status),
    ['done', 'active'],
    'every call sends the whole list, so the last one is the plan',
  )
  assert.equal(workSummary({ ...turn, startedAt: null }), '0 steps', 'a plan is not a step')

  // Revising it reads as revising, not as making one.
  applyEvent(turn, { type: 'toolPreparing', callId: 'p3', name: 'update_plan' })
  assert.equal(activity(turn), 'Updating the plan')

  const stopped = { ...turn, cards: [...turn.cards], stopped: true }
  settle(stopped)
  assert.deepEqual(
    planOf(stopped).map((s) => s.status),
    ['done', 'pending'],
    'a stopped turn had not finished the step it was on',
  )

  applyEvent(turn, { type: 'delta', text: 'Here is the weekend.' })
  settle(turn)
  assert.deepEqual(
    planOf(turn).map((s) => s.status),
    ['done', 'done'],
    'a turn that answered was answering the step it was on',
  )
}
{
  const turn = liveTurn('t')
  applyEvent(turn, {
    type: 'toolStarted',
    callId: 'p',
    name: 'update_plan',
    arguments: { steps: [{ text: '  ' }, { text: 'Real', status: 'nonsense' }, 4, null] },
  })
  assert.deepEqual(planOf(turn), [{ text: 'Real', status: 'pending' }], 'junk steps are dropped')
}

// ── the clock and the folded summary ────────────────────────────────

assert.equal(elapsed(0), '0s')
assert.equal(elapsed(4400), '4s')
assert.equal(elapsed(72_000), '1m 12s')
{
  // A replayed turn has no clock, and says how many steps it took instead.
  const turn = emptyTurn('assistant', 'old')
  turn.cards = ['done', 'failed', 'done'].map((state, i) => ({ callId: `${i}`, name: 'x', state }))
  assert.equal(workSummary(turn), '3 steps · 1 did not work')
}

// ── the history list ─────────────────────────────────────────────────

{
  const now = new Date(2026, 9, 4, 15, 0)
  const at = (month, d, h = 12) => new Date(2026, month, d, h).toISOString()
  const t = (id, updatedAt) => ({ id, title: id, createdAt: updatedAt, updatedAt, messages: 2 })
  const groups = threadGroups(
    [
      t('a', at(9, 4, 9)),
      t('b', at(9, 4, 0)),
      t('c', at(9, 3, 23)),
      t('d', at(9, 1)),
      t('e', at(8, 20)),
    ],
    now,
  )
  assert.deepEqual(
    groups.map((g) => [g.label, g.threads.map((x) => x.id)]),
    [
      ['Today', ['a', 'b']],
      ['Yesterday', ['c']],
      ['Previous 7 days', ['d']],
      ['Previous 30 days', ['e']],
    ],
  )
  const older = threadGroups([t('x', new Date(2026, 6, 1).toISOString())], now)
  assert.deepEqual(
    older.map((g) => g.label),
    ['Older'],
  )
  assert.deepEqual(threadGroups([], now), [], 'nothing yet is no headings at all')
}

await close()
console.log('agent: all checks passed')
