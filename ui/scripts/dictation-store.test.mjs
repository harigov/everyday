// Behaviour checks for `dictation.svelte.ts`, against the mock backend's
// scripted dictation.
//
// What is pinned: that what is heard grows in the store, that Stop hands the
// words to whoever asked and Cancel does not, that a session ends on silence
// by itself, that a second start cannot take the first one's words, that a
// failure still hands over what was heard, and that a failure or a lock
// leaves nothing running.

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
  modules: [dictationModule, stateModule, mockDictation],
  close,
} = await load(
  ['/src/lib/dictation.svelte.ts', '/src/lib/state.svelte.ts', '/src/lib/mock-dictation.ts'],
  { svelte: true },
)
const { dictation } = dictationModule
const { app } = stateModule
const { mockDictationEmit, mockDictationRunning } = mockDictation

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

ok('the mock backend can listen', dictation.available)
check(
  'nothing is active',
  [dictation.active, dictation.state, dictation.text, dictation.owner, dictation.error],
  [false, null, '', null, null],
)

// ── Stop keeps the words ─────────────────────────────────────────────────

let done = []
await dictation.start('compose', (text, reason) => done.push([text, reason]))
check(
  'started',
  [dictation.active, dictation.state, dictation.owner, dictation.text],
  [true, 'listening', 'compose', ''],
)

ok('it hears somebody', await until(() => dictation.state === 'hearing'))
ok('the words grow', await until(() => dictation.text.split(' ').length >= 3))
const partial = dictation.text

await dictation.start('somewhere-else', () => done.push(['wrong', 'wrong']))
check('a second start changes nothing', dictation.owner, 'compose')

dictation.stop()
ok('stopping ends it', await until(() => !dictation.active, 1000))
check('and tells whoever asked, once', done.length, 1)
ok('with the words heard', done[0][0].startsWith(partial) && done[0][0].length >= partial.length)
check('and why', done[0][1], 'stopped')
check(
  'and puts everything back',
  [dictation.state, dictation.text, dictation.owner],
  [null, '', null],
)
ok('the session is gone', await until(() => !mockDictationRunning(), 1000))

// ── Cancel throws them away ──────────────────────────────────────────────

done = []
await dictation.start('compose', (text, reason) => done.push([text, reason]))
ok('hearing again', await until(() => dictation.text.length > 0))
dictation.cancel()
check('cancelling ends it at once', [dictation.active, dictation.owner], [false, null])
await new Promise((r) => setTimeout(r, 100))
check('and tells nobody', done, [])
ok('the session is gone', await until(() => !mockDictationRunning(), 1000))

// ── it falls quiet ───────────────────────────────────────────────────────

done = []
await dictation.start('compose', (text, reason) => done.push([text, reason]))
ok('it ends by itself', await until(() => !dictation.active, 8000))
check('after the silence', done.length, 1)
check('and says so', done[0][1], 'silence')
ok('with everything that was said', /agenda beforehand$/.test(done[0][0]))

// ── it fails ─────────────────────────────────────────────────────────────

done = []
await dictation.start('compose', (text, reason) => done.push([text, reason]))
ok('listening', await until(() => dictation.state === 'listening'))
mockDictationEmit({ type: 'trouble', message: 'The microphone is quiet.' })
check('a trouble is shown', dictation.error, 'The microphone is quiet.')
check('and it carries on', dictation.active, true)
mockDictationEmit({ type: 'ended', reason: 'failed', text: '', error: 'No microphone.' })
check('a failure ends it', [dictation.active, dictation.error], [false, 'No microphone.'])
// The mock ends with what it had heard, which is the point: the words said
// before the microphone went are handed over, once, as a failure.
check(
  'and still hands over what was heard',
  done.map(([, reason]) => reason),
  ['failed'],
)

// ── the vault locks ──────────────────────────────────────────────────────

done = []
await dictation.start('compose', (text, reason) => done.push([text, reason]))
check('listening again', dictation.active, true)
await app.lock()
check('a lock ends it', [dictation.active, dictation.state, dictation.error], [false, null, null])
check('and tells nobody', done, [])
ok('and the session with it', await until(() => !mockDictationRunning(), 1000))

finish('dictation-store')
await close()

// `app.start()` arms the idle-vault poll; see `editor.test.mjs`'s own note.
process.exit(process.exitCode ?? 0)
