// Pure formatting for the meeting notes UI: what a stage is called, how a
// running timer reads, and how a benchmark's realtime factor is said.
//
// Split out from `meetings.svelte.ts` for the same reason `format.ts` is its
// own file: these are functions of a value, not of the vault, and keeping
// them apart from the `$state` store is what lets `meetings.test.mjs` check
// them without booting Svelte's reactivity or a mock backend.

import { DEFAULT_COLORS } from './colors'
import type { Attribution, Stage } from './types'

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
  const pad = (n: number) => String(n).padStart(2, '0')
  if (hours > 0) return `${hours}:${pad(minutes)}:${pad(seconds)}`
  return `${minutes}:${pad(seconds)}`
}

/**
 * A benchmark's realtime factor, said the way the settings pane promises:
 * "about 3.2× real time on this computer".
 */
export function formatRealtimeFactor(factor: number): string {
  return `about ${factor.toFixed(1)}× real time on this computer`
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
 * donuts -- applied to a person instead of a role or a calendar. "You" is
 * always the vault's own accent, since the reader is looking for themself
 * first; everyone else cycles through the same eight-colour palette
 * `DEFAULT_COLORS` gives a journal or a role, picked by their speaker key so
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
