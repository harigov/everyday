// The mock backend's reading aloud: a scripted session, so the player in the
// top bar can be looked at in a browser with no speakers.
//
// Nothing here is real audio. `mockListenStart` says a handful of sentences,
// one after another, each for about as long as it would take to say, and
// honours pause, resume, skip, speed and stop. It starts timers only when a
// session is started and holds none otherwise, which is what keeps a test
// runner from hanging on it.
//
// Imports types alone: `api.ts` loads this, and anything it imported back
// would close a cycle.

import type { ListenEvent, ListenSource, ListenState } from './types'
import { VaultError } from './types'

/** What a briefing of a thread says. */
const BRIEFING = [
  'Priya wrote to confirm lunch on Thursday.',
  'She suggests half past twelve at the place by the station.',
  'She also asks whether you can bring the draft agenda.',
  'Nothing else in the thread needs an answer.',
]

/** What an article says, as it would be made fit to hear. */
const ARTICLE = [
  'The city council voted on Tuesday to expand the bike network.',
  'The plan adds forty miles of protected lanes over five years.',
  'Supporters say it will cut traffic deaths.',
  'Opponents worry about parking on the main streets.',
  'A second vote is expected next month.',
]

/** How long a sentence takes to say, before the speed: a pause for breath and
 *  a little per word. */
const SENTENCE_MS = 300
const WORD_MS = 90
/** How long the session spends getting ready before the first sentence. */
const PREPARE_MS = 300

interface Session {
  onEvent: (event: ListenEvent) => void
  sentences: string[]
  /** The sentence being said, or about to be. */
  index: number
  speed: number
  state: ListenState
  timer: ReturnType<typeof setTimeout> | null
}

let session: Session | null = null

/** Whether a scripted session is running, for a test to read. */
export function mockListenRunning(): boolean {
  return session !== null
}

/** The sentence a scripted session is on, for a test to read. */
export function mockListenIndex(): number | null {
  return session?.index ?? null
}

/** The speed a scripted session was last given, for a test to read. */
export function mockListenSpeed(): number | null {
  return session?.speed ?? null
}

function sentencesFor(source: ListenSource): string[] {
  return source.type === 'mailThread' ? BRIEFING : ARTICLE
}

/** Say sentence `live.index`, then go on to the next on a timer. */
function say(live: Session) {
  if (session !== live) return
  const text = live.sentences[live.index]
  if (text === undefined) {
    end(live, null)
    return
  }
  live.state = 'playing'
  live.onEvent({ type: 'state', state: 'playing' })
  live.onEvent({ type: 'saying', index: live.index, part: Math.floor(live.index / 2), text })
  const words = text.split(/\s+/).length
  live.timer = setTimeout(
    () => {
      live.timer = null
      live.index += 1
      say(live)
    },
    (SENTENCE_MS + words * WORD_MS) / live.speed,
  )
}

function clearTimer(live: Session) {
  if (live.timer) clearTimeout(live.timer)
  live.timer = null
}

/** End `live` as the real session does: `ended` last, and nothing after. */
function end(live: Session, error: string | null) {
  if (session !== live) return
  session = null
  clearTimer(live)
  live.onEvent({ type: 'ended', error })
}

/**
 * Begin the scripted reading. Resolves at once, as the real one does when the
 * session is up; the events follow on their own timers. Replaces a session
 * already running, which is told it has ended.
 */
export async function mockListenStart(
  source: ListenSource,
  speed: number,
  onEvent: (event: ListenEvent) => void,
): Promise<void> {
  if (session) end(session, null)
  const live: Session = {
    onEvent,
    sentences: sentencesFor(source),
    index: 0,
    speed: speed > 0 ? speed : 1,
    state: 'preparing',
    timer: null,
  }
  session = live
  onEvent({ type: 'state', state: 'preparing' })
  live.timer = setTimeout(() => {
    live.timer = null
    say(live)
  }, PREPARE_MS)
}

/**
 * Say something as the session would, outside the script: what a test uses to
 * reach a case the script does not, such as a failure. An `ended` ends the
 * session, as it does for real.
 */
export function mockListenEmit(event: ListenEvent): void {
  const live = session
  if (!live) return
  if (event.type === 'ended') end(live, event.error)
  else live.onEvent(event)
}

/** What `api.listen*` reach in the mock build. */
export interface MockListenControls {
  pause(): Promise<void>
  resume(): Promise<void>
  skip(by: number): Promise<void>
  speed(speed: number): Promise<void>
  stop(): Promise<void>
}

export const mockListenControls: MockListenControls = {
  async pause() {
    const live = session
    // Still preparing: nothing is playing to pause.
    if (!live || live.state !== 'playing') return
    // Stopping the timer drops the rest of the sentence, so the sentence says
    // itself again from its start on resume, as a real one does when the
    // voice is rebuilt.
    clearTimer(live)
    live.state = 'paused'
    live.onEvent({ type: 'state', state: 'paused' })
  },
  async resume() {
    const live = session
    if (!live || live.state !== 'paused') return
    say(live)
  },
  async skip(by: number) {
    const live = session
    if (!live || live.state === 'preparing') return
    const target = Math.max(0, live.index + Math.round(by))
    clearTimer(live)
    live.index = target
    if (live.state === 'paused') {
      // Moving while paused says the new sentence at once and stays paused
      // after it, which the player draws by the sentence alone.
      const text = live.sentences[target]
      if (text === undefined) end(live, null)
      else {
        live.onEvent({ type: 'saying', index: target, part: Math.floor(target / 2), text })
      }
      return
    }
    say(live)
  },
  async speed(speed: number) {
    if (!(speed > 0)) throw new VaultError('invalid', 'speed must be above zero')
    const live = session
    if (!live) return
    live.speed = speed
    // The sentence in hand is said again at the new speed, as the real one
    // builds the voice again and starts from the sentence playing.
    if (live.state === 'playing') {
      clearTimer(live)
      say(live)
    }
  },
  async stop() {
    const live = session
    if (live) end(live, null)
  },
}
