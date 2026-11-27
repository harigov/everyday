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
import { readFileSync } from 'node:fs'
import { load } from './harness.mjs'

const {
  modules: [agent, svelteInternal],
  close,
} = await load(['/src/lib/agent.ts', 'svelte/internal/client'])
const {
  activity,
  applyEvent,
  choicesOf,
  DeltaBatch,
  elapsed,
  emptyTurn,
  gerund,
  isLoopback,
  liveTurn,
  madePictures,
  pastTense,
  planOf,
  replay,
  searchSiteProblem,
  BUILT_IN_SEARCH_SITES,
  listedNames,
  settle,
  sitesCover,
  threadGroups,
  toolLabel,
  waitingForAnswer,
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

// Options the assistant offered are read off its call, defensively, and the
// last offer in a turn is the one drawn.
{
  const turn = emptyTurn('assistant', 'x')
  applyEvent(turn, {
    type: 'toolStarted',
    callId: 'c1',
    name: 'offer_choices',
    arguments: {
      question: 'Which film for Sundays?',
      options: [
        { title: 'Paddington 2', detail: 'Gentle, and you liked the first' },
        { title: '' },
        { title: 'Arrival' },
        'not an option',
      ],
    },
  })
  const choices = choicesOf(turn)
  assert.equal(choices.question, 'Which film for Sundays?')
  assert.deepEqual(
    choices.options.map((o) => o.title),
    ['Paddington 2', 'Arrival'],
    'an option with no title, or no shape, is not drawn',
  )
  assert.equal(choices.options[1].detail, '')
  assert.equal(choicesOf(emptyTurn('assistant', 'y')), null, 'a turn that offered nothing')
}

// A step that came back broken is asked for again: what it had begun to say
// goes, so the reply is not drawn with that prose twice.
{
  const turn = emptyTurn('assistant', 'x')
  applyEvent(turn, { type: 'delta', text: 'Done the inbox. ' })
  applyEvent(turn, { type: 'toolStarted', callId: 'c1', name: 'list_tasks', arguments: {} })
  applyEvent(turn, { type: 'delta', text: 'Now the plan:' })
  applyEvent(turn, { type: 'toolPreparing', callId: 'c2', name: 'update_plan' })
  applyEvent(turn, { type: 'retrying', text: 'Done the inbox. ' })
  assert.equal(turn.text, 'Done the inbox. ', 'back to the reply as the failed step found it')
  assert.equal(turn.preparing, null, 'the call that came back broken is not still being written')
  assert.equal(turn.cards.length, 1, 'the step that finished keeps its card')
  applyEvent(turn, { type: 'delta', text: 'Now the plan:' })
  assert.equal(turn.text, 'Done the inbox. Now the plan:')
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

// A picture a call made rides on its finished event, and is the turn's to
// draw; a failed call's is not, whatever the event says.
{
  const turn = emptyTurn('assistant', 'x')
  for (const callId of ['c1', 'c2']) {
    applyEvent(turn, { type: 'toolStarted', callId, name: 'draw_illustration', arguments: {} })
  }
  applyEvent(turn, {
    type: 'toolFinished',
    callId: 'c1',
    name: 'draw_illustration',
    ok: true,
    summary: 'Plan.png',
    files: ['f-1'],
  })
  applyEvent(turn, {
    type: 'toolFinished',
    callId: 'c2',
    name: 'draw_illustration',
    ok: false,
    summary: 'that is not an SVG that can be drawn',
    files: ['f-2'],
  })
  assert.deepEqual(turn.cards[0].files, ['f-1'])
  assert.deepEqual(madePictures(turn), ['f-1'])
  assert.deepEqual(toolLabel('draw_illustration', { title: 'Plan' }, 'done'), {
    text: 'Drew',
    detail: 'Plan',
  })
  assert.equal(toolLabel('generate_image', {}, 'running').text, 'Making a picture')
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

// "Always allow" is offered only when the question named a website -- see
// `AgentEvent`'s `site` -- and the card keeps it for the button and for the
// words it says afterwards. No site, or a null one, is no button.
{
  const turn = emptyTurn('assistant', 'x')
  const page = { name: 'read_web_page', arguments: {}, kind: 'fetch', canPark: false }
  const base = { ...page, type: 'confirmationRequired' }
  applyEvent(turn, {
    ...base,
    callId: 'c1',
    subject: 'https://en.wikipedia.org/wiki/Lisbon',
    site: 'en.wikipedia.org',
  })
  applyEvent(turn, { ...base, callId: 'c2', subject: 'https://a.example/x' })
  applyEvent(turn, { ...base, callId: 'c3', subject: 'https://b.example/x', site: null })
  assert.equal(turn.cards[0].site, 'en.wikipedia.org')
  assert.equal(turn.cards[1].site, null, 'an event with no site leaves none on the card')
  assert.equal(turn.cards[2].site, null)
  assert.equal(turn.cards[0].trusted ?? null, null, 'nothing is added until it is answered')
}

// A website is safe when the list has it or a name above it. The card's note
// that "Always allow" took waits on exactly this, so it has to answer as the
// service's `sites::covers` does.
{
  const sites = ['wikipedia.org', 'docs.rs']
  assert.equal(sitesCover(sites, 'wikipedia.org'), true, 'a site covers itself')
  assert.equal(sitesCover(sites, 'en.wikipedia.org'), true, 'and the names under it')
  assert.equal(sitesCover(sites, 'WWW.Wikipedia.org.'), true, 'whatever the case, www. or dot')
  assert.equal(sitesCover(sites, 'notwikipedia.org'), false, 'not a name that ends the same way')
  assert.equal(sitesCover(sites, 'wikipedia.org.evil.example'), false, 'nor one it is a prefix of')
  assert.equal(sitesCover(['en.wikipedia.org'], 'wikipedia.org'), false, 'only downwards')
  assert.equal(sitesCover([], 'wikipedia.org'), false, 'an empty list covers nothing')
  assert.equal(sitesCover(sites, ''), false, 'and nothing is covered by anything')
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

// ── words held for a frame ────────────────────────────────────────────
//
// Streamed words are folded once a frame rather than once a fragment. What
// must not change is what the turn ends up saying, or the order it was said
// in against the cards around it -- the store flushes before anything else.
{
  const { proxy } = svelteInternal
  /** A frame that comes only when the test says so. */
  let frames = []
  const schedule = (run) => {
    frames.push(run)
    return () => (frames = frames.filter((f) => f !== run))
  }
  const nextFrame = () => {
    const due = frames
    frames = []
    for (const run of due) run()
  }

  const turns = proxy([liveTurn('local-1', 1000)])
  const reply = turns[0]
  const batch = new DeltaBatch(schedule)
  const fold = (event) => {
    if (event.type === 'delta' || event.type === 'thinking') return batch.add(reply, event)
    batch.flush()
    applyEvent(reply, event)
  }

  fold({ type: 'started', messageId: 'msg-1' })
  const words = []
  for (let n = 0; n < 200; n++) words.push(`w${n} `)
  for (const text of words.slice(0, 100)) fold({ type: 'delta', text })
  assert.equal(frames.length, 1, 'a hundred fragments ask for one frame')
  assert.equal(turns[0].text, '', 'and nothing is drawn before it')
  nextFrame()
  assert.equal(turns[0].text, words.slice(0, 100).join(''), 'then all of them, in order')
  assert.equal(turns[0].phase, 'writing')

  for (const text of words.slice(100, 150)) fold({ type: 'delta', text })
  fold({ type: 'toolStarted', callId: 'c1', name: 'list_tasks', arguments: {} })
  assert.equal(
    turns[0].text,
    words.slice(0, 150).join(''),
    'a card flushes the words before it, so they are drawn first',
  )
  assert.equal(frames.length, 0, 'and the frame it was waiting for is not wanted any more')
  assert.equal(turns[0].phase, 'tool')

  // Reasoning and prose in one frame come out as they would one at a time.
  fold({ type: 'thinking', text: 'They want ' })
  fold({ type: 'delta', text: words[150] })
  fold({ type: 'thinking', text: 'the list.' })
  nextFrame()
  assert.equal(turns[0].thinking, 'They want the list.')
  assert.equal(turns[0].phase, 'writing', 'any prose in the frame leaves it writing')

  fold({ type: 'thinking', text: ' Again.' })
  nextFrame()
  assert.equal(turns[0].phase, 'writing', 'reasoning after prose does not take it back')

  // A retry replaces what the deltas drew, held ones included.
  for (const text of words.slice(151, 160)) fold({ type: 'delta', text })
  fold({ type: 'retrying', text: 'Again: ' })
  assert.equal(turns[0].text, 'Again: ')

  for (const text of words.slice(160)) fold({ type: 'delta', text })
  fold({ type: 'finished', messageId: 'msg-1' })
  assert.equal(turns[0].text, `Again: ${words.slice(160).join('')}`, 'the end flushes the rest')
  assert.equal(turns[0].id, 'msg-1')
  assert.equal(frames.length, 0)

  // Words for another turn flush the first turn's, so neither gets the other's.
  const other = liveTurn('local-2', 2000)
  batch.add(reply, { type: 'delta', text: ' tail' })
  batch.add(other, { type: 'delta', text: 'next' })
  assert.equal(turns[0].text.endsWith(' tail'), true)
  batch.flush()
  assert.equal(other.text, 'next')

  // An empty fragment still marks the turn as writing, as one alone did.
  const quiet = liveTurn('local-3', 3000)
  batch.add(quiet, { type: 'delta', text: '' })
  batch.flush()
  assert.equal(quiet.phase, 'writing')
  batch.flush()
  assert.equal(quiet.text, '', 'flushing with nothing held does nothing')
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

// A reopened thread draws what a call made from its stored result.
{
  const turns = replay([
    msg({ id: 'm1', role: 'user', content: 'draw the plan' }),
    msg({
      id: 'm2',
      role: 'assistant',
      content: '',
      toolCalls: [{ id: 'call_1', name: 'draw_illustration', arguments: { title: 'Plan' } }],
    }),
    msg({ id: 'm3', role: 'tool', content: 'Plan.png', toolCallId: 'call_1', files: ['f-1'] }),
    msg({ id: 'm4', role: 'assistant', content: 'Here is the plan.' }),
  ])
  assert.deepEqual(madePictures(turns[1]), ['f-1'])
  assert.deepEqual(madePictures(turns[2]), [])
}

// A picture the person sent comes back with their words, and a message that
// was only a picture is still their turn. Everyone else's turn has none.
{
  const picture = {
    blob: 'a'.repeat(64),
    kind: 'image',
    mime: 'image/jpeg',
    filename: 'receipt.jpg',
    byteLen: 40_000,
    width: 2048,
    height: 1536,
    caption: '',
  }
  const turns = replay([
    msg({ id: 'm1', role: 'user', content: 'how much was this?', attachments: [picture] }),
    msg({ id: 'm2', role: 'assistant', content: '£12.40.' }),
    msg({ id: 'm3', role: 'user', content: '', attachments: [picture, picture] }),
    msg({ id: 'm4', role: 'user', content: 'and this?' }),
  ])
  assert.deepEqual(
    turns.map((t) => t.pictures.length),
    [1, 0, 2, 0],
  )
  assert.equal(turns[0].pictures[0].filename, 'receipt.jpg')
  assert.equal(turns[2].role, 'user', 'a picture with nothing said is still the person speaking')
}

// Files handed over come back with the turn they were sent with. A message
// with none carries none -- the Rust side leaves the empty list off the wire.
{
  const turns = replay([
    msg({ id: 'm1', role: 'user', content: 'read this', files: ['file-a', 'file-b'] }),
    msg({ id: 'm2', role: 'assistant', content: 'Done.' }),
    msg({ id: 'm3', role: 'user', content: 'and?' }),
  ])
  assert.deepEqual(
    turns.map((t) => t.files),
    [['file-a', 'file-b'], [], []],
  )
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
// floating. The application's own lines -- a background job finishing, say --
// are drawn, as `system` turns of their own, attributed to neither party.
{
  const turns = replay([
    msg({ id: 'm1', role: 'tool', content: 'orphan', toolCallId: 'call_x' }),
    msg({ id: 'm2', role: 'system', content: 'Research finished: «e-bikes»' }),
    msg({ id: 'm3', role: 'user', content: 'hello' }),
  ])
  assert.deepEqual(
    turns.map((t) => [t.id, t.role]),
    [
      ['m2', 'system'],
      ['m3', 'user'],
    ],
  )
  assert.equal(turns[0].text, 'Research finished: «e-bikes»')
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

// A turn held on a question is still: the panel stops its motion then.
{
  const turn = liveTurn('t', 1000)
  assert.equal(waitingForAnswer(turn), false)
  applyEvent(turn, {
    type: 'confirmationRequired',
    callId: 'c9',
    name: 'delete_task',
    subject: 'Order the timber',
    arguments: {},
    kind: 'destructive',
    canPark: true,
  })
  assert.equal(waitingForAnswer(turn), true)
  assert.equal(activity(turn), 'Waiting for your answer')
  applyEvent(turn, { type: 'toolStarted', callId: 'c9', name: 'delete_task', arguments: {} })
  assert.equal(waitingForAnswer(turn), false, 'answered, it is running again')
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
check('update_time_block', 'running', 'Updating time block')
check('update_time_block', 'done', 'Updated time block')
check('log_item', 'running', 'Logging item')
check('log_item', 'done', 'Logged item')
check('delete_log', 'waiting', 'Delete log')
// `unstar_thread`'s own verb doubles like `star`'s does: `un-` is a prefix,
// not a stressed syllable, so the short verb underneath is still `star`.
check('unstar_thread', 'running', 'Unstarring thread')
check('unstar_thread', 'done', 'Unstarred thread')
// Ending a spoken conversation: the voice, not the thread.
check('summarize_thread', 'running', 'Summarising')
check('summarize_thread', 'done', 'Summarised')
check('read_aloud', 'done', 'Read aloud')
check('end_voice_mode', 'running', 'Ending voice mode')
check('end_voice_mode', 'done', 'Ended voice mode')

function check(name, state, want) {
  assert.equal(toolLabel(name, {}, state).text, want, `${name} while ${state}`)
}

assert.equal(gerund('stop'), 'stopping')
assert.equal(gerund('tie'), 'tying')
assert.equal(gerund('unstar'), 'unstarring')
assert.equal(pastTense('apply'), 'applied')
assert.equal(pastTense('plan'), 'planned')
assert.equal(pastTense('send'), 'sent')
assert.equal(pastTense('unstar'), 'unstarred', 'not "unstared"')

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
// Also not strictly a web tool by name, but offered beside them and worth
// a detail for the same reason web_search is: which title it looked up.
assert.deepEqual(toolLabel('look_up_item', { title: 'Dune' }, 'running'), {
  text: 'Looking up',
  detail: 'Dune',
})
assert.deepEqual(toolLabel('look_up_item', { title: 'Dune' }, 'done'), {
  text: 'Looked up',
  detail: 'Dune',
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
// What it does on the computer somebody is at: one of four things opened,
// named by what it was; a site searched for what was asked or selected.
assert.deepEqual(toolLabel('open', { url: 'https://www.youtube.com/watch?v=x' }, 'done'), {
  text: 'Opened',
  detail: 'youtube.com',
})
assert.deepEqual(toolLabel('open', { app: 'Files' }, 'running'), {
  text: 'Opening',
  detail: 'Files',
})
assert.deepEqual(toolLabel('open', { section: 'mail' }, 'done'), { text: 'Opened', detail: 'Mail' })
assert.equal(toolLabel('open', { section: 'screentime' }, 'done').detail, 'Screen time')
assert.equal(toolLabel('open', { file_id: 'f-1' }, 'running').detail, 'a file')
assert.deepEqual(toolLabel('search_site', { site: 'Amazon', query: 'headphones' }, 'running'), {
  text: 'Searching Amazon',
  detail: 'headphones',
})
assert.deepEqual(toolLabel('search_site', { site: 'Google', selection: true }, 'done'), {
  text: 'Searched Google',
  detail: 'what you selected',
})
assert.equal(toolLabel('search_site', { site: 'eBay', query: 'x' }, 'waiting').text, 'Search eBay')
// Music and volume.
check('lock_screen', 'running', 'Locking the screen')
check('lock_screen', 'done', 'Locked the screen')
check('list_timers', 'done', 'Checked timers')
for (const [action, running, done] of [
  ['pause', 'Pausing', 'Paused'],
  ['play', 'Playing', 'Played'],
  ['next', 'Skipping to the next track', 'Skipped to the next track'],
  ['previous', 'Going back a track', 'Went back a track'],
  ['status', "Checking what's playing", "Checked what's playing"],
]) {
  assert.equal(toolLabel('control_media', { action }, 'running').text, running)
  assert.equal(toolLabel('control_media', { action }, 'done').text, done)
}
assert.equal(toolLabel('control_media', { action: 'toggle' }, 'waiting').text, 'Play or pause')
assert.equal(toolLabel('set_volume', { action: 'up' }, 'running').text, 'Turning the volume up')
assert.equal(toolLabel('set_volume', { action: 'down' }, 'done').text, 'Turned the volume down')
assert.equal(
  toolLabel('set_volume', { action: 'set', percent: 30 }, 'running').text,
  'Setting the volume to 30%',
)
assert.equal(
  toolLabel('set_volume', { action: 'set', percent: 0 }, 'done').text,
  'Set the volume to 0%',
)
assert.equal(toolLabel('set_volume', { action: 'mute' }, 'done').text, 'Muted')
assert.equal(toolLabel('set_volume', { action: 'unmute' }, 'running').text, 'Unmuting')
assert.equal(toolLabel('set_volume', { action: 'status' }, 'done').text, 'Checked the volume')
// A switch of the desktop's own: on, off, or only looked at.
assert.equal(
  toolLabel('desktop_setting', { setting: 'do_not_disturb', on: true }, 'done').text,
  'Turned Do Not Disturb on',
)
assert.equal(
  toolLabel('desktop_setting', { setting: 'dark_mode', on: false }, 'running').text,
  'Turning dark mode off',
)
assert.equal(
  toolLabel('desktop_setting', { setting: 'night_light' }, 'running').text,
  'Checking Night Light',
  'no `on` is a question, not a change',
)
// Copying shows what was copied; a timer, how long and what for.
assert.deepEqual(toolLabel('copy_to_clipboard', { text: '12 Elm Road' }, 'done'), {
  text: 'Copied',
  detail: '12 Elm Road',
})
assert.deepEqual(toolLabel('set_timer', { minutes: 10, label: 'pasta' }, 'done'), {
  text: 'Set a timer',
  detail: '10 minutes · pasta',
})
assert.deepEqual(toolLabel('set_timer', { hours: 1, minutes: 30 }, 'running'), {
  text: 'Setting a timer',
  detail: '1 hour 30 minutes',
})
assert.equal(toolLabel('set_timer', { hours: 0, minutes: 5 }, 'done').detail, '5 minutes')
assert.deepEqual(toolLabel('set_timer', { at: '07:00', label: 'wake up' }, 'done'), {
  text: 'Set a timer',
  detail: 'at 07:00 · wake up',
})
assert.equal(toolLabel('set_timer', {}, 'waiting').detail, null)
assert.equal(toolLabel('cancel_timer', { id: 2 }, 'running').text, 'Cancelling timer 2')
assert.equal(toolLabel('cancel_timer', { all: true }, 'done').text, 'Cancelled all timers')
// Arguments are whatever the model wrote. Nothing in them may throw.
for (const junk of [null, 'a string', 7, { query: 3 }, { url: 'not a url' }]) {
  toolLabel('web_search', junk, 'running')
  toolLabel('read_web_page', junk, 'done')
  toolLabel('get_weather', junk, 'waiting')
  toolLabel('look_up_item', junk, 'waiting')
  toolLabel('read_skill', junk, 'waiting')
}
for (const junk of [
  null,
  'a string',
  7,
  {},
  { action: 'constructor', setting: 'toString', section: 7, minutes: 'ten', all: 'yes' },
  { percent: Infinity, id: -1, on: 'yes', hours: NaN },
]) {
  for (const name of [
    'open',
    'search_site',
    'control_media',
    'set_volume',
    'desktop_setting',
    'lock_screen',
    'copy_to_clipboard',
    'set_timer',
    'list_timers',
    'cancel_timer',
  ]) {
    for (const state of ['running', 'done', 'waiting']) {
      assert.equal(typeof toolLabel(name, junk, state).text, 'string', `${name} with ${junk}`)
    }
  }
}

// ── the search sites somebody adds ───────────────────────────────────

// The same rules `validate_sites` refuses a save by, said beside the row.
{
  const site = (name, url) => ({ name, url })
  const problem = (sites, at = sites.length - 1) => searchSiteProblem(sites, at)

  assert.equal(problem([site('Amazon India', 'https://www.amazon.in/s?k={query}')]), null)
  assert.equal(problem([site('Wiki', 'http://wiki.example/find/{query}')]), null, 'http is allowed')
  assert.equal(problem([site('', '')]), null, 'a row added and not used is not wrong')
  assert.equal(problem([]), null, 'no such row')

  assert.match(problem([site('', 'https://a.example/?q={query}')]), /name/)
  assert.match(problem([site('Wiki', '')]), /address/)
  assert.match(problem([site('Wiki', 'https://a.example/')]), /\{query\}/)
  assert.match(problem([site('Wiki', 'javascript:alert({query})')]), /https/)
  assert.match(problem([site('Wiki', 'file:///etc/{query}')]), /https/)
  assert.match(problem([site('Wiki', 'wiki.example/?q={query}')]), /https/, 'no scheme')
  assert.match(problem([site('x'.repeat(41), 'https://a.example/?q={query}')]), /40/)
  assert.equal(problem([site('x'.repeat(40), 'https://a.example/?q={query}')]), null)

  // Names are the same name the way the service says so: any case, "the"
  // or "my" in front -- and only the later of the two is the one flagged.
  const twice = [
    site('Wiki', 'https://a.example/?q={query}'),
    site('the wiki', 'https://b.example/?q={query}'),
  ]
  assert.equal(problem(twice, 0), null)
  assert.match(problem(twice, 1), /already a site called the wiki/)
  assert.equal(
    problem([
      site('Wiki', 'https://a.example/?q={query}'),
      site('Wikis', 'https://b.example/?q={query}'),
    ]),
    null,
  )
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

// The sites Settings says the assistant already knows are the core's own,
// read out of the Rust: a site added, dropped or renamed there and not here
// fails this rather than leaving Settings telling somebody something untrue.
{
  const rust = readFileSync(
    new URL('../../crates/jatayu-core/src/agent/desk.rs', import.meta.url),
    'utf8',
  )
  const table = rust.slice(
    rust.indexOf('const BUILT_IN:'),
    rust.indexOf('];', rust.indexOf('const BUILT_IN:')),
  )
  const names = [...table.matchAll(/\(\s*"([^"]+)",\s*"https?:/g)].map((m) => m[1])
  assert.ok(names.length > 5, `found ${names.length} built-in sites in desk.rs`)
  assert.deepEqual([...BUILT_IN_SEARCH_SITES], names)
  assert.equal(listedNames(['A', 'B', 'C']), 'A, B and C')
  assert.equal(listedNames(['A']), 'A')
  assert.equal(listedNames([]), '')
}

await close()
console.log('agent: all checks passed')
