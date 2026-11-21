// The mock backend's talking: a scripted conversation, and the voice card's
// downloads.
//
// Nothing here is real audio. `mockVoiceStart` plays one short exchange --
// listening, a question heard, a reply written and spoken, listening again --
// so the bar, the thread and the Stop and mute buttons can be looked at in a
// browser with no microphone. It starts only when called and holds no timer
// otherwise, which is what keeps a test runner from hanging on it.
//
// Imports types alone: `api.ts` loads this, and anything it imported back
// would close a cycle.

import type {
  AgentEvent,
  ConversationId,
  LocalModelInfo,
  OnScreen,
  VoiceEvent,
  VoiceSample,
  VoiceState,
} from './types'
import { VaultError } from './types'

// ── A session ────────────────────────────────────────────────────────

const QUESTION = "What's on my calendar today?"
const REPLY = 'You have standup at ten, and lunch with Priya at half past twelve.'

interface Session {
  onEvent: (event: VoiceEvent) => void
  timers: Set<ReturnType<typeof setTimeout>>
  muted: boolean
  onScreen: OnScreen | null
}

let session: Session | null = null
let nextMessage = 1

/** What the last session was told is on screen, for a test to read. */
export function mockVoiceScreen(): OnScreen | null {
  return session?.onScreen ?? null
}

/** Whether the scripted session is running. */
export function mockVoiceRunning(): boolean {
  return session !== null
}

/**
 * Begin the scripted conversation. Resolves at once, as the real one does when
 * the session is up; the events follow on their own timers.
 */
export async function mockVoiceStart(
  _conversationId: ConversationId,
  onScreen: OnScreen | null,
  onEvent: (event: VoiceEvent) => void,
): Promise<void> {
  if (session) throw new VaultError('invalid', 'already talking')
  const mine: Session = { onEvent, timers: new Set(), muted: false, onScreen }
  session = mine

  const state = (s: VoiceState) => onEvent({ type: 'state', state: s })
  const agent = (event: AgentEvent) => onEvent({ type: 'agent', event })
  const at = (ms: number, run: () => void) => {
    const timer = setTimeout(() => {
      mine.timers.delete(timer)
      if (session === mine) run()
    }, ms)
    mine.timers.add(timer)
  }

  const messageId = `msg-voice-${nextMessage++}`
  let t = 0
  state('starting')
  at((t += 200), () => state('listening'))
  at((t += 600), () => state('hearing'))
  at((t += 500), () => state('understanding'))
  at((t += 300), () => {
    onEvent({ type: 'heard', text: QUESTION })
    state('thinking')
    agent({ type: 'started', messageId })
  })
  const words = REPLY.split(/(?<= )/)
  at((t += 300), () => state('speaking'))
  for (const text of words) at((t += 70), () => agent({ type: 'delta', text }))
  at((t += 200), () => agent({ type: 'finished', messageId }))
  at((t += 400), () => state('listening'))
}

/**
 * Say something as the session would, outside the script: what a test uses to
 * reach a case the script does not, such as a reply cut short or a failure.
 * An `ended` ends the session, as it does for real.
 */
export function mockVoiceEmit(event: VoiceEvent): void {
  const live = session
  if (!live) return
  if (event.type === 'ended') mockVoiceStop(event.error)
  else live.onEvent(event)
}

/** End the session as a person pressing Stop does. */
export function mockVoiceStop(error: string | null = null): void {
  const ending = session
  if (!ending) return
  session = null
  for (const timer of ending.timers) clearTimeout(timer)
  ending.timers.clear()
  ending.onEvent({ type: 'ended', error })
}

export function mockVoiceSetMuted(muted: boolean): void {
  if (session) session.muted = muted
}

export function mockVoiceSetScreen(onScreen: OnScreen | null): void {
  if (session) session.onScreen = onScreen
}

// ── The voice card ───────────────────────────────────────────────────

const voiceModels: LocalModelInfo[] = [
  {
    id: 'phonon',
    name: 'Phonon 31M',
    description:
      'Twenty-five natural voices that run on this computer, many times quicker than they speak.',
    languages: 'English',
    bytes: 71_198_353,
    installed: false,
    progress: null,
    error: null,
  },
  {
    id: 'kokoro',
    name: 'Kokoro 82M',
    description:
      'Twenty-eight voices that run on this computer, a few times quicker than they speak. A larger download than Phonon, and slower.',
    languages: 'English',
    bytes: 349_886_476,
    installed: false,
    progress: null,
    error: null,
  },
]

const downloadTimers = new Map<string, ReturnType<typeof setInterval>>()

export function mockVoiceModels(): LocalModelInfo[] {
  return voiceModels.map((m) => ({ ...m }))
}

export function mockDownloadVoiceModel(id: string): void {
  const model = voiceModels.find((m) => m.id === id)
  if (!model || model.installed || downloadTimers.has(id)) return
  model.error = null
  model.progress = { done: 0, total: model.bytes }
  const timer = setInterval(() => {
    if (!model.progress) return
    model.progress = {
      done: Math.min(model.bytes, model.progress.done + model.bytes / 8),
      total: model.bytes,
    }
    if (model.progress.done >= model.bytes) {
      clearInterval(timer)
      downloadTimers.delete(id)
      model.installed = true
      model.progress = null
    }
  }, 300)
  downloadTimers.set(id, timer)
}

export function mockCancelVoiceModelDownload(id: string): void {
  const timer = downloadTimers.get(id)
  if (timer) clearInterval(timer)
  downloadTimers.delete(id)
  const model = voiceModels.find((m) => m.id === id)
  if (model) model.progress = null
}

export function mockDeleteVoiceModel(id: string): void {
  const model = voiceModels.find((m) => m.id === id)
  if (!model) return
  mockCancelVoiceModelDownload(id)
  model.installed = false
}

/** Under half a second of a plain tone: enough for something to play. */
export function mockVoiceSample(): VoiceSample {
  const rate = 8000
  const count = rate / 2
  const bytes = new Uint8Array(44 + count * 2)
  const view = new DataView(bytes.buffer)
  const text = (at: number, s: string) => {
    for (let i = 0; i < s.length; i++) view.setUint8(at + i, s.charCodeAt(i))
  }
  text(0, 'RIFF')
  view.setUint32(4, 36 + count * 2, true)
  text(8, 'WAVEfmt ')
  view.setUint32(16, 16, true)
  view.setUint16(20, 1, true) // PCM
  view.setUint16(22, 1, true) // mono
  view.setUint32(24, rate, true)
  view.setUint32(28, rate * 2, true)
  view.setUint16(32, 2, true)
  view.setUint16(34, 16, true)
  text(36, 'data')
  view.setUint32(40, count * 2, true)
  for (let i = 0; i < count; i++) {
    // A tone that fades in and out, so it does not click.
    const fade = Math.sin((Math.PI * i) / count)
    const wave = Math.sin((2 * Math.PI * 440 * i) / rate)
    view.setInt16(44 + i * 2, Math.round(wave * 6000 * fade), true)
  }
  let binary = ''
  for (const b of bytes) binary += String.fromCharCode(b)
  return { wav: btoa(binary) }
}
