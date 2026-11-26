// Pure formatting for the meeting notes UI: what a stage is called, how a
// running timer reads, and how a benchmark's realtime factor is said.
//
// Split out from `meetings.svelte.ts` for the same reason `format.ts` is its
// own file: these are functions of a value, not of the vault, and keeping
// them apart from the `$state` store is what lets `meetings.test.mjs` check
// them without booting Svelte's reactivity or a mock backend.

import { DEFAULT_COLORS } from './colors'
import { pad2 } from './time'
import type { Attribution, LocalModel, SpeechConfig, Stage } from './types'

/** What a recording's stage says in a list row or a card. */
export function stageLabel(stage: Stage): string {
  switch (stage.type) {
    case 'recording':
      return 'Recording'
    case 'transcribing':
      return 'Transcribing'
    case 'identifying':
      return 'Working out who spoke'
    case 'summarising':
      return 'Writing the note'
    case 'done':
      return 'Done'
    case 'failed':
      return 'Failed'
  }
}

/** Is this stage still moving -- worth a card in the notes list and a poll? */
export function stageIsActive(stage: Stage): boolean {
  return stage.type !== 'done' && stage.type !== 'failed'
}

/**
 * A running duration as a clock: `0:42`, `12:07`, `1:03:20`.
 *
 * Hours are only shown once there are any, so the ordinary case -- a call
 * under an hour -- reads as a stopwatch rather than as `0:12:07`.
 */
export function formatTimer(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000))
  const hours = Math.floor(total / 3600)
  const minutes = Math.floor((total % 3600) / 60)
  const seconds = total % 60
  if (hours > 0) return `${hours}:${pad2(minutes)}:${pad2(seconds)}`
  return `${minutes}:${pad2(seconds)}`
}

/**
 * A benchmark's realtime factor, said the way the settings pane promises:
 * "about 3.2× real time on this computer".
 */
export function formatRealtimeFactor(factor: number, gpu?: string | null): string {
  return `about ${factor.toFixed(1)}× real time on ${gpu ?? 'this computer'}`
}

/** Below this, the settings pane suggests a remote transcriber instead. */
export const SLOW_REALTIME_FACTOR = 1.5

export function isTooSlow(factor: number): boolean {
  return factor < SLOW_REALTIME_FACTOR
}

/** How long the call track has been silent before the pill warns about it. */
export const SYSTEM_SILENT_WARN_MS = 60_000

/**
 * A speaker's colour in the transcript: the house rule -- entity colour, no
 * donuts -- applied to a person instead of an area or a calendar. "You" is
 * always the vault's own accent, since the reader is looking for themself
 * first; everyone else cycles through the same eight-colour palette
 * `DEFAULT_COLORS` gives a journal or an area, picked by their speaker key so
 * it stays the same for the length of one transcript.
 */
export function speakerColor(key: number, isOwner: boolean): string {
  if (isOwner) return 'var(--accent)'
  return DEFAULT_COLORS[key % DEFAULT_COLORS.length]!
}

/** A timestamp within a call, from milliseconds: `0:00`, `12:07`. */
export function formatOffset(ms: number): string {
  return formatTimer(ms)
}

/** How a speaker's name should read: plain, or guessed and said so. */
export function speakerNameStyle(how: Attribution): 'plain' | 'guessed' | 'unknown' {
  if (how.type === 'unknown') return 'unknown'
  if (how.type === 'inferred') return 'guessed'
  return 'plain'
}

/**
 * The recognisers that run on this computer, as the speech card in Settings →
 * Models lists them and as a sentence names them -- `LocalModel` in
 * `crates/karmayoga-core/src/speech.rs`. `llama` ones run through llama.cpp,
 * which only a copy that can run a decision model has.
 */
export const LOCAL_RECOGNISERS: {
  id: LocalModel
  name: string
  blurb: string
  llama?: boolean
}[] = [
  { id: 'parakeetV3', name: 'Parakeet TDT v3', blurb: 'English & European languages, faster' },
  { id: 'whisperTurbo', name: 'Whisper large-v3 turbo', blurb: 'Any language, slower' },
  {
    id: 'qwen3Asr17b',
    name: 'Qwen3-ASR 1.7B',
    blurb: 'Most accurate, best with accents like Indian English; quick on a graphics card',
    llama: true,
  },
  {
    id: 'qwen3Asr06b',
    name: 'Qwen3-ASR 0.6B',
    blurb: 'Smaller and quicker Qwen3-ASR, less accurate',
    llama: true,
  },
]

/**
 * The speech model in a few words, for the line meeting notes show where its
 * choice used to be: "Parakeet TDT v3, on this computer", "gpt-4o-transcribe
 * at OpenAI". `null` reads as no choice at all.
 */
export function describeSpeech(speech: SpeechConfig | null | undefined): string {
  if (!speech) return 'none chosen yet'
  const t = speech.transcriber
  switch (t.type) {
    case 'local': {
      const name = LOCAL_RECOGNISERS.find((m) => m.id === t.model)?.name ?? t.model
      return `${name}, on this computer`
    }
    case 'openAi':
      return `${t.model.trim() || 'a model not yet named'} at OpenAI`
    case 'google':
      return `${t.model.trim() || 'a model not yet named'} at Google`
    case 'compatible': {
      const where = hostOf(t.baseUrl) ?? 'a compatible server'
      return `${t.model.trim() || 'a model not yet named'} at ${where}`
    }
  }
}

function hostOf(url: string): string | null {
  try {
    return new URL(url.trim()).host || null
  } catch {
    return null
  }
}
