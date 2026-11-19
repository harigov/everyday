// Behaviour checks for `voice.svelte.ts` and the spoken half of
// `agent.svelte.ts`, against the mock backend's scripted conversation.
//
// What is pinned: that events from a session drive the store and the turns
// drawn in the thread, that a reply talked over is cut to what was heard, that
// a card asked about aloud is an ordinary card, and that however a session
// ends -- Stop, an error, another thread, a lock -- nothing is left running.
// The first block of checks rides the script itself, which takes a few
// seconds; the rest speak for the session through `mockVoiceEmit`.

import { load, makeCheck, stubBrowser } from './harness.mjs'

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
  modules: [voiceModule, agentModule, stateModule, mockVoice],
  close,
} = await load(
  [
    '/src/lib/voice.svelte.ts',
    '/src/lib/agent.svelte.ts',
    '/src/lib/state.svelte.ts',
    '/src/lib/mock-voice.ts',
  ],
  { svelte: true },
)
const { voice } = voiceModule
const { agent } = agentModule
const { app } = stateModule
const { mockVoiceEmit, mockVoiceScreen, mockVoiceRunning } = mockVoice

const { check, ok, finish } = makeCheck()

/** Wait for `condition`, or give up after `ms` and let the check fail. */
async function until(condition, ms = 6000) {
  const stop = Date.now() + ms
  while (!condition() && Date.now() < stop) await new Promise((r) => setTimeout(r, 20))
  return condition()
}

await app.start()
check('the mock vault opens unlocked', app.screen, 'main')

// ── before anything ──────────────────────────────────────────────────────

ok('the mock backend can talk', voice.available)
check(
  'nothing is active',
  [voice.active, voice.state, voice.muted, voice.error],
  [false, null, false, null],
)
check('and the bar says nothing', voice.label, '')

agent.conversationId = null
await voice.start()
check(
  'no conversation, no session',
  [voice.active, voice.error],
  [false, 'No conversation is open. Try again in a moment.'],
)

agent.conversationId = 'conv-1'
agent.busy = true
await voice.start()
check('a typed turn running, no session', voice.active, false)
ok('and it says why', /finish answering/.test(voice.error ?? ''))
agent.busy = false

// ── the script, start to finish ──────────────────────────────────────────

await voice.start()
check('started', [voice.active, voice.state, voice.label], [true, 'starting', 'Starting…'])
await voice.start()
ok('starting twice changes nothing', mockVoiceRunning() && voice.active)

ok('it listens', await until(() => voice.state === 'listening'))
check('and says so', voice.label, 'Listening')
await voice.toggleMute()
check('muted', [voice.muted, voice.label], [true, 'Microphone off'])
await voice.toggleMute()
check('and not', [voice.muted, voice.label], [false, 'Listening'])

ok('it hears somebody', await until(() => voice.state === 'hearing'))
check('still says Listening', voice.label, 'Listening')
ok('the screen went with it', mockVoiceScreen() !== null)
check('the one in front of them', mockVoiceScreen()?.app, app.section)

ok('it works out what they said', await until(() => voice.state === 'understanding'))
check('and says so', voice.label, 'Working it out…')

ok('what they said is a turn', await until(() => agent.turns.length === 2))
check(
  'their words, then a reply being written',
  agent.turns.map((t) => [t.role, t.text]),
  [
    ['user', "What's on my calendar today?"],
    ['assistant', ''],
  ],
)
check('the thread is busy while it answers', agent.busy, true)
check('thinking', [voice.state, voice.label], ['thinking', 'Thinking…'])

ok('it speaks', await until(() => voice.state === 'speaking'))
check('and says so', voice.label, 'Speaking')
ok('the reply streams in', await until(() => agent.turns[1].text.length > 0))
ok('and finishes', await until(() => !agent.busy))
const reply = agent.turns[1]
check(
  'the whole reply',
  reply.text,
  'You have standup at ten, and lunch with Priya at half past twelve.',
)
ok('under the id the vault gave it', reply.id.startsWith('msg-voice-'))
check('settled', reply.phase, 'done')
ok('it goes back to listening', await until(() => voice.state === 'listening'))

// ── talked over ──────────────────────────────────────────────────────────

mockVoiceEmit({ type: 'replyCut', messageId: reply.id, text: 'You have standup at ten' })
check('the reply is what was heard', agent.turns[1].text, 'You have standup at ten')
mockVoiceEmit({ type: 'replyCut', messageId: 'msg-nobody', text: 'x' })
check('a reply nobody drew is ignored', agent.turns.length, 2)

mockVoiceEmit({ type: 'trouble', message: 'The speaker went quiet.' })
check(
  'trouble is shown and the session goes on',
  [voice.error, voice.active],
  ['The speaker went quiet.', true],
)

// ── a card asked about aloud is an ordinary card ────────────────────────────

mockVoiceEmit({ type: 'heard', text: 'Delete the timber task' })
mockVoiceEmit({ type: 'agent', event: { type: 'started', messageId: 'msg-2' } })
mockVoiceEmit({
  type: 'agent',
  event: {
    type: 'confirmationRequired',
    callId: 'call-1',
    name: 'delete_task',
    subject: 'Order the timber',
    arguments: {},
    kind: 'destructive',
    canPark: true,
  },
})
mockVoiceEmit({ type: 'state', state: 'asking' })
check('a new turn begins', agent.turns.length, 4)
check(
  'the card is waiting',
  agent.turns[3].cards.map((c) => [c.callId, c.state]),
  [['call-1', 'waiting']],
)
check('the bar asks for yes or no', voice.label, 'Say yes or no')
check('and the thread is busy', agent.busy, true)

// Talked over: the open reply is settled, so its card cannot sit waiting for
// an answer nobody can give it.
mockVoiceEmit({ type: 'heard', text: 'Actually, leave it' })
check('the old card is settled', agent.turns[3].cards[0].state, 'failed')
check('the old reply is over', agent.turns[3].phase, 'done')
check('and a new reply is open', [agent.turns.length, agent.busy], [6, true])
mockVoiceEmit({ type: 'agent', event: { type: 'failed', message: 'The API key was refused.' } })
check(
  'a failed turn ends the reply',
  [agent.turns[5].error, agent.busy],
  ['The API key was refused.', false],
)
ok('and the session carries on', voice.active)

// ── Stop ─────────────────────────────────────────────────────────────────

await voice.toggleMute()
await voice.stop()
check(
  'Stop ends it and puts everything back',
  [voice.active, voice.state, voice.muted, voice.error, mockVoiceRunning()],
  [false, null, false, null, false],
)

// ── ended, with a reason ─────────────────────────────────────────────────

await voice.start()
mockVoiceEmit({ type: 'heard', text: 'Hello' })
check('a turn is open', agent.busy, true)
mockVoiceEmit({ type: 'ended', error: 'The microphone went away.' })
check(
  'a session that ended on its own says why',
  [voice.active, voice.state, voice.error],
  [false, null, 'The microphone went away.'],
)
check('and no turn is left open', agent.busy, false)
check('nor spinning', agent.turns.at(-1).phase, 'done')
await voice.start()
check('starting again clears it', [voice.active, voice.error], [true, null])

// ── another thread ───────────────────────────────────────────────────────

agent.conversationId = 'conv-2'
mockVoiceEmit({ type: 'heard', text: 'Into the wrong thread' })
ok('a session does not follow the thread it was started in', await until(() => !voice.active, 1000))
ok(
  'and nothing it said went into the other',
  !agent.turns.some((t) => t.text === 'Into the wrong thread'),
)

// ── the vault locks ──────────────────────────────────────────────────────

agent.conversationId = 'conv-3'
await voice.start()
check('talking again', voice.active, true)
await app.lock()
check('a lock ends it', [voice.active, voice.state, voice.error], [false, null, null])
ok('and the session with it', await until(() => !mockVoiceRunning(), 1000))

finish('voice-store')
await close()

// `app.start()` arms the idle-vault poll; see `editor.test.mjs`'s own note.
process.exit(process.exitCode ?? 0)
