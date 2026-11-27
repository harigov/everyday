// Pure formatting for the meeting notes UI: what a stage is called, how a
// running timer reads, and how a benchmark's realtime factor is said.
//
// Split out from `meetings.svelte.ts` for the same reason `format.ts` is its
// own file: these are functions of a value, not of the vault, and keeping
// them apart from the `$state` store is what lets `meetings.test.mjs` check
// them without booting Svelte's reactivity or a mock backend.

import { DEFAULT_COLORS } from './colors'
import { pad2 } from './time'
import type {
  Attribution,
  LocalModel,
  NoteId,
  Recording,
  Segment,
  SpeechConfig,
  Speaker,
  Stage,
} from './types'

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

/**
 * What a note says while the call behind it is still being worked on -- the
 * draft exists from the moment the call ends, so this is the line above its
 * body: "Transcribing the call", "Working out who spoke", "Writing the
 * summary". `null` for a stage with nothing left to say (recording is still
 * the call itself, done is a finished note).
 */
export function draftStageLabel(stage: Stage): string | null {
  switch (stage.type) {
    case 'transcribing':
      return 'Transcribing the call'
    case 'identifying':
      return 'Working out who spoke'
    case 'summarising':
      return 'Writing the summary'
    default:
      return null
  }
}

/**
 * The recording whose pipeline is still writing into this note, if any: the
 * one that names it as its note and has not finished. A failed one counts,
 * because the draft stays as it is and the person is owed the reason and a
 * way to try again. Takes the list the store already holds, so it is a
 * lookup and not a fetch.
 */
export function recordingWritingNote(
  recordings: readonly Recording[],
  noteId: NoteId | null | undefined,
): Recording | null {
  if (!noteId) return null
  return (
    recordings.find(
      (r) =>
        r.noteId === noteId &&
        (r.stage.type === 'transcribing' ||
          r.stage.type === 'identifying' ||
          r.stage.type === 'summarising' ||
          r.stage.type === 'failed'),
    ) ?? null
  )
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
 * `crates/jatayu-core/src/speech.rs`. `llama` ones run through llama.cpp,
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

/** Below this many words, an unnamed voice is "quieter" and tucked away. */
export const QUIET_WORDS = 25
/** Or below this share of everything said. */
export const QUIET_SHARE = 0.02

/** How many words each speaker said, by speaker key. */
export function wordsBySpeaker(segments: readonly Segment[]): Map<number, number> {
  const words = new Map<number, number>()
  for (const seg of segments) {
    const n = seg.text.split(/\s+/).filter(Boolean).length
    words.set(seg.speaker, (words.get(seg.speaker) ?? 0) + n)
  }
  return words
}

/**
 * Which speakers are too quiet to deserve a chip of their own. A call with
 * two people in it can still come back with dozens of voices, nearly all of
 * them a few words long; a wall of "Who is this?" buttons hides the two who
 * matter. Only voices nobody has put a name to qualify -- the owner, anyone
 * named or matched to a voiceprint stay where they are -- and only when they
 * said under `QUIET_WORDS` words or under `QUIET_SHARE` of the call.
 */
export function quietSpeakers(
  speakers: readonly Speaker[],
  segments: readonly Segment[],
): Set<number> {
  const words = wordsBySpeaker(segments)
  let total = 0
  for (const n of words.values()) total += n
  const quiet = new Set<number>()
  for (const s of speakers) {
    if (s.how.type !== 'unknown' && s.how.type !== 'inferred') continue
    const n = words.get(s.key) ?? 0
    if (n < QUIET_WORDS || n < total * QUIET_SHARE) quiet.add(s.key)
  }
  return quiet
}
