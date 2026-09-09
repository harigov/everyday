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
import { createServer } from 'vite'

const server = await createServer({
  configFile: false,
  root: new URL('..', import.meta.url).pathname,
  server: { middlewareMode: true },
  appType: 'custom',
  logLevel: 'error',
})

const { applyEvent, emptyTurn, isLoopback, replay, settle } =
  await server.ssrLoadModule('/src/lib/agent.ts')

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

await server.close()
console.log('agent: all checks passed')
