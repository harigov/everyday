// The mock backend's dictating: a scripted session, so the microphone button
// in the composer can be tried in a browser with no microphone.
//
// Nothing here is real audio. `mockDictationStart` "hears" one short
// instruction, a word at a time, then falls quiet and ends on its own as the
// real one does after the silence limit. Stop keeps the words, cancel throws
// them away. It starts timers only when a session is started and holds none
// otherwise, which is what keeps a test runner from hanging on it.
//
// Imports types alone: `api.ts` loads this, and anything it imported back
// would close a cycle.

import type { DictationEnd, DictationEvent, DictationState } from './types'
import { VaultError } from './types'

const PHRASE = 'Tell Priya that Thursday works for me, and ask her to send the agenda beforehand'

/** Before anything is heard, and between one word and the next. */
const LISTEN_MS = 400
const WORD_MS = 160
/** How long it stays quiet after the last word before it gives up listening. */
const SILENCE_MS = 1500

interface Session {
  onEvent: (event: DictationEvent) => void
  words: string[]
  /** What has been heard so far. */
  heard: string[]
  timer: ReturnType<typeof setTimeout> | null
}

let session: Session | null = null

/** Whether a scripted session is running, for a test to read. */
export function mockDictationRunning(): boolean {
  return session !== null
}

function clearTimer(live: Session) {
  if (live.timer) clearTimeout(live.timer)
  live.timer = null
}

function state(live: Session, s: DictationState) {
  live.onEvent({ type: 'state', state: s })
}

/** End `live` as the real session does: `ended` last, and nothing after. */
function end(live: Session, reason: DictationEnd, error: string | null = null) {
  if (session !== live) return
  session = null
  clearTimer(live)
  const text = reason === 'cancelled' ? '' : live.heard.join(' ')
  live.onEvent({ type: 'ended', reason, text, error })
}

/** Hear the next word, or fall quiet when there are none left. */
function hear(live: Session) {
  if (session !== live) return
  const word = live.words[live.heard.length]
  if (word === undefined) {
    live.timer = setTimeout(() => end(live, 'silence'), SILENCE_MS)
    return
  }
  if (live.heard.length === 0) state(live, 'hearing')
  live.heard.push(word)
  live.onEvent({ type: 'heard', text: live.heard.join(' ') })
  live.timer = setTimeout(() => hear(live), WORD_MS)
}

/**
 * Begin the scripted dictation. Resolves at once, as the real one does when
 * the microphone is open; the events follow on their own timers.
 */
export async function mockDictationStart(onEvent: (event: DictationEvent) => void): Promise<void> {
  if (session) throw new VaultError('invalid', 'already listening')
  const live: Session = { onEvent, words: PHRASE.split(' '), heard: [], timer: null }
  session = live
  state(live, 'listening')
  live.timer = setTimeout(() => hear(live), LISTEN_MS)
}

/**
 * Say something as the session would, outside the script: what a test uses to
 * reach a case the script does not, such as a failure. An `ended` ends the
 * session, as it does for real.
 */
export function mockDictationEmit(event: DictationEvent): void {
  const live = session
  if (!live) return
  if (event.type === 'ended') end(live, event.reason, event.error)
  else live.onEvent(event)
}

/** What `api.dictation*` reach in the mock build. */
export interface MockDictationControls {
  stop(): Promise<void>
  cancel(): Promise<void>
}

export const mockDictationControls: MockDictationControls = {
  async stop() {
    const live = session
    if (live) end(live, 'stopped')
  },
  async cancel() {
    const live = session
    if (live) end(live, 'cancelled')
  },
}
