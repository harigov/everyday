// Behaviour checks for `listen.svelte.ts`, against the mock backend's
// scripted reading.
//
// What is pinned: that events from a session drive the store, that pause,
// skip, speed and stop reach the session, that starting another thing ends the
// one before, that the speed is kept, and that however a session ends -- Stop,
// an error, a lock -- nothing is left running.

import { load, makeCheck, stubBrowser } from './harness.mjs'

// A `localStorage` that keeps what it is given, to check the speed is kept.
const kept = new Map()
const localStorage = {
  getItem: (key) => kept.get(key) ?? null,
  setItem: (key, value) => kept.set(key, String(value)),
  removeItem: (key) => kept.delete(key),
}

stubBrowser({
  window: globalThis,
  localStorage,
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
  modules: [listenModule, stateModule, mockListen],
  close,
} = await load(
  ['/src/lib/listen.svelte.ts', '/src/lib/state.svelte.ts', '/src/lib/mock-listen.ts'],
  { svelte: true },
)
const { listening, LISTEN_SPEEDS } = listenModule
const { app } = stateModule
const { mockListenEmit, mockListenRunning, mockListenSpeed } = mockListen

const { check, ok, finish } = makeCheck()

/** Wait for `condition`, or give up after `ms` and let the check fail. */
async function until(condition, ms = 4000) {
  const stop = Date.now() + ms
  while (!condition() && Date.now() < stop) await new Promise((r) => setTimeout(r, 20))
  return condition()
}

await app.start()
check('the mock vault opens unlocked', app.screen, 'main')

const thread = { type: 'mailThread', threadId: 'thread-1' }
const story = { type: 'story', storyId: 'story-1' }

// ── before anything ──────────────────────────────────────────────────────

ok('the mock backend can play', listening.available)
check(
  'nothing is active',
  [listening.active, listening.state, listening.title, listening.current, listening.error],
  [false, null, '', null, null],
)
ok('the speeds include normal', LISTEN_SPEEDS.includes(1))
check('at normal speed to begin with', listening.speed, 1)
check('nothing is being read', listening.isReading(thread), false)

// ── a reading, start to finish ───────────────────────────────────────────

await listening.start(thread, 'Lunch on Thursday')
check(
  'started',
  [listening.active, listening.state, listening.title],
  [true, 'preparing', 'Lunch on Thursday'],
)
ok('it is that thread', listening.isReading(thread))
ok('and not another', !listening.isReading({ type: 'mailThread', threadId: 'thread-2' }))
ok('nor a story', !listening.isReading(story))

ok('it plays', await until(() => listening.state === 'playing'))
ok('and says the first sentence', await until(() => listening.current === 0))
ok('which is kept', listening.sentences[0] !== undefined && listening.sentences[0].length > 0)
ok('the next follows', await until(() => listening.current === 1))
ok('and the first is still there', listening.sentences[0] !== undefined)

listening.pause()
ok('pausing reaches the session', await until(() => listening.state === 'paused'))
const at = listening.current
await new Promise((r) => setTimeout(r, 400))
check('and nothing is said while paused', listening.current, at)
listening.toggle()
ok('toggling resumes', await until(() => listening.state === 'playing'))
listening.toggle()
ok('and toggling again pauses', await until(() => listening.state === 'paused'))
listening.resume()
ok('resuming plays', await until(() => listening.state === 'playing'))

// Skipping while paused is exact: the new sentence is said at once and the
// session stays paused, so nothing races the sentence timer.
listening.pause()
ok('paused to skip', await until(() => listening.state === 'paused'))
const here = listening.current
listening.forward()
ok('forward goes on a sentence', await until(() => listening.current === here + 1))
check('and stays paused', listening.state, 'paused')
listening.back()
ok('back goes back', await until(() => listening.current === here))
listening.resume()
ok('and it plays on', await until(() => listening.state === 'playing'))

listening.setSpeed(1.5)
check('the speed is chosen', listening.speed, 1.5)
ok('and reaches the session', await until(() => mockListenSpeed() === 1.5))
check('and is kept', localStorage.getItem('jatayu.listen.speed'), '1.5')

mockListenEmit({ type: 'trouble', message: 'That part could not be read.' })
check('a trouble is shown', listening.error, 'That part could not be read.')
check('and the session carries on', listening.active, true)

// ── replacing it ─────────────────────────────────────────────────────────

await listening.start(story, 'The bike plan')
check('another thing replaces it', [listening.active, listening.title], [true, 'The bike plan'])
ok('it is the story now', listening.isReading(story) && !listening.isReading(thread))
check('with nothing of the old one left', [listening.sentences.length, listening.error], [0, null])
ok('and it plays', await until(() => listening.current === 0))
ok('the story words, not the thread ones', /council/.test(listening.sentences[0] ?? ''))

// ── Stop ─────────────────────────────────────────────────────────────────

listening.stop()
ok('stopping ends it', await until(() => !listening.active, 1000))
check(
  'and puts everything back',
  [
    listening.state,
    listening.title,
    listening.source,
    listening.current,
    listening.sentences.length,
  ],
  [null, '', null, null, 0],
)
ok('the session is gone', await until(() => !mockListenRunning(), 1000))

// ── it fails ─────────────────────────────────────────────────────────────

await listening.start(thread, 'Another')
ok('playing', await until(() => listening.state === 'playing'))
mockListenEmit({ type: 'ended', error: 'The voice stopped working.' })
check(
  'an error ends it and is kept',
  [listening.active, listening.error],
  [false, 'The voice stopped working.'],
)

// ── the vault locks ──────────────────────────────────────────────────────

await listening.start(thread, 'Once more')
check('reading again', listening.active, true)
await app.lock()
check('a lock ends it', [listening.active, listening.state, listening.error], [false, null, null])
ok('and the session with it', await until(() => !mockListenRunning(), 1000))

finish('listen-store')
await close()

// `app.start()` arms the idle-vault poll; see `editor.test.mjs`'s own note.
process.exit(process.exitCode ?? 0)
