// Voice typing's pill: the parts that are not drawing.
//
// The pill is its own page in its own window -- see `crates/karmayoga-app/
// src/voicetyping/` -- so it carries none of the main interface. What it needs
// to decide is here, where `scripts/voicetyping.test.mjs` can check it without
// a window.

/** What the shell sends on `karmayoga://voice-typing/state`: the whole picture, on every change. */
export interface TypingState {
  /** Which dictation this is, counted from 1 for the life of the process. */
  seq: number
  phase: 'starting' | 'listening' | 'transcribing' | 'writing' | 'done' | 'failed'
  mode: 'toggle' | 'hold'
  /** Listening hands-free: toggle mode, or hold mode after a tap. */
  handsFree: boolean
  /** The key, as the desktop writes it. */
  key: string
  /** For `failed`, why; for `done`, a note when there is one. */
  message: string | null
  /** For `failed`, what could not be typed, when there was some. */
  text: string | null
  /** `overlay`: drawn inside a transparent window covering the work area.
   *  `window`: the whole of a small window that is exactly the pill. */
  pill: 'overlay' | 'window'
}

/** Whether `next` should replace `current`: anything but an older dictation's late word. */
export function accepts(current: TypingState | null, next: TypingState): boolean {
  return !current || next.seq >= current.seq
}

/** The status line. */
export function title(s: TypingState): string {
  switch (s.phase) {
    case 'starting':
      return 'Getting ready…'
    case 'listening':
      return 'Listening'
    case 'transcribing':
      return 'Working out the words…'
    case 'writing':
      return 'Writing…'
    case 'done':
      return 'Typed'
    case 'failed':
      return s.message?.trim() || 'Nothing was typed.'
  }
}

/** The small line under it, where there is one. */
export function hint(s: TypingState): string | null {
  switch (s.phase) {
    case 'listening':
      return s.mode === 'hold' && !s.handsFree ? 'Let go to finish' : `Press ${s.key} to finish`
    case 'done':
      return s.message?.trim() || null
    default:
      return null
  }
}

/** What a failed dictation offers beside the cross. */
export type Remedy = 'copy' | 'open' | null

export function remedy(s: TypingState): Remedy {
  if (s.phase !== 'failed') return null
  if (s.text?.trim()) return 'copy'
  if (/settings|locked|unlock/i.test(s.message ?? '')) return 'open'
  return null
}

/** How many bars the meter has. */
export const BARS = 9

/** The level history to start a dictation with: silence. */
export function silence(): number[] {
  return Array.from({ length: BARS }, () => 0)
}

/**
 * The meter after one more reading: the oldest bar drops off, and the new one
 * rises quickly and falls slowly, so speech reads as speech and not as flicker.
 */
export function pushLevel(history: readonly number[], level: number): number[] {
  const last = history[history.length - 1] ?? 0
  const clean = Number.isFinite(level) ? Math.max(0, Math.min(1, level)) : 0
  const next = clean > last ? last + (clean - last) * 0.7 : last + (clean - last) * 0.3
  return [...history.slice(-(BARS - 1)), next]
}

/** A bar's height as a fraction of the meter: never nothing, and speech lifts it off the floor. */
export function barHeight(level: number): number {
  const lifted = Math.sqrt(Math.max(0, Math.min(1, level)))
  return 0.16 + 0.84 * lifted
}

/** Nothing said lately: the meter breathes instead of sitting still. */
export function isQuiet(history: readonly number[]): boolean {
  return history.every((l) => l < 0.02)
}
